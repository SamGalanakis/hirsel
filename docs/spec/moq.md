# Media over QUIC

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Media Wave 2 package 3 of 4). Not built.
Rules MOQ-1 to MOQ-9, laws M1 to M12, slices M1 to M6. Goes to design review (astra) before
any build.*

`moq` is a sans-IO MoQ Transport endpoint and relay, written in Hirsel under
`core/programs/moq` <!-- doc-ref: planned -->. It follows
[draft-ietf-moq-transport-23](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/23/)
(2026-11-02; -22 was 2026-10-01), cited by section below as "moqt-23 §n". It runs over
native [QUIC](quic.md) or over [WebTransport](webtransport.md) for browsers, and it is the
fan-out path for broadcast-shaped rooms where WebRTC's per-subscriber state costs too much.

**One draft at a time.** The package speaks exactly one moq-transport draft: ALPN `moqt-23`
on native QUIC, and `moqt-23` in `WT-Available-Protocols` on WebTransport. A peer that offers
only another draft is refused at ALPN (`no_application_protocol`) or with
`WT_ALPN_ERROR`. Moving to the next draft is a version bump of the whole package with its
laws, never a second codec beside the first. Drafts still change wire formats between
revisions (-23 has no MAX_REQUEST_ID and no UNSUBSCRIBE; request streams and stream credit
replace them), so a multi-draft codec would be a compatibility layer with no end state.

## Scope

| Area | In Wave 2 | Not in Wave 2 (and why) |
| --- | --- | --- |
| Roles | publisher, subscriber, and relay (moqt-23 §7) in one endpoint | — |
| Transport | native QUIC (ALPN `moqt-23`, URI scheme `moqt`), WebTransport; QUIC DATAGRAM required for Datagram delivery (moqt-23 §3) | WebSocket fallback (moq-rs's `qmux`): not in the draft |
| Setup | one SETUP each way (moqt-23 §9.1) with AUTHORITY, PATH, AUTHORIZATION TOKEN, MAX_AUTH_TOKEN_CACHE_SIZE (0: no cache), MOQT IMPLEMENTATION, MAX FILTER RANGES, MAX_REQUEST_UPDATES, MAX_CONTROL_MESSAGE_SIZE | — |
| Requests | SUBSCRIBE, PUBLISH, FETCH, TRACK_STATUS, PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS, each on its own bidirectional stream; REQUEST_UPDATE; cancellation by STOP_SENDING or RESET_STREAM | — |
| Data | OBJECT_DATAGRAM (§11.2.1), SUBGROUP_HEADER streams (§11.3.1), FETCH_HEADER streams (§11.4.1) | — |
| Relay | caching, fan-out, deduplication, switchover, delivery timeouts (moqt-23 §5.2, §7) | relay-to-relay meshes with routing: [sfu_cascade](sfu_cascade.md) carries the SFU's own links; MoQ cluster routing (draft-lcurley-moq-cluster) is open item O3 |
| Encryption | object payloads opaque to the relay (moqt-23 §7.9); end-to-end per object with [SFrame](sframe.md), keyed per track by the publisher | — |
| Catalogs and packaging | none in the package: the payload is bytes | `hang`, LOC, MSF catalogs: applications choose; the SFU bridge (MOQ-8) uses its own track naming |

## MOQ-1: Shape

```hirsel unbuilt
pub opaque type Session
pub type Role = Publisher | Subscriber | Both
pub type RequestId = RequestId(u64)
pub type Namespace = Namespace(List[Bytes])
pub type TrackName = TrackName(ns: Namespace, name: Bytes)
pub type Location = Location(group: u64, object: u64)
pub type Mode = Subgroup | Datagram
pub type Object = Object(track: TrackName, group: u64, subgroup: u64, id: u64, publisher_priority: u8, status: ObjectStatus, payload: Shared[Bytes])
pub type ObjectStatus = Normal | EndOfGroup | EndOfTrack | DoesNotExist
pub type Limits { requests: u32, tracks: u32, cache_groups: u32, cache_bytes: u64, control_bytes: u32, filter_ranges: u16 }
pub type Event = Setup(path: Bytes, token: Option[Bytes]) | Subscribe(RequestId, TrackName, Filter, priority: u8) | Publish(RequestId, TrackName) | Fetch(RequestId, TrackName, from: Location, to: Location) | Received(Object) | Done(RequestId, code: u64) | Goaway(uri: Bytes)

pub fn session(side: quic.Side, role: Role, limits: Limits) -> Session
pub fn receive(mut s: Session, mut e: quic.Endpoint, c: quic.ConnectionHandle, events: List[quic.Event], mut out: List[Event])
pub fn accept(mut s: Session, mut e: quic.Endpoint, id: RequestId, ok: Result[(), u64])
pub fn subscribe(mut s: Session, mut e: quic.Endpoint, track: TrackName, f: Filter, priority: u8) -> Result[RequestId, MoqError]
pub fn publish(mut s: Session, mut e: quic.Endpoint, o: Object, mode: Mode) -> Result[(), MoqError]
pub fn cancel(mut s: Session, mut e: quic.Endpoint, id: RequestId, code: u64)
```

A `Session` is one MoQ session on one QUIC connection or WebTransport session. Request ids
are even for the client and odd for the server (moqt-23 §9), counted per session. Object
payloads are `Shared[Bytes]`: a relay forwards the same buffer to every subscriber with
`quic.write_shared` or `quic.send_datagram`, never copying a payload (MOQ-6).

## MOQ-2: Control

The control channel is a pair of unidirectional streams, one each way, which carry SETUP and
then GOAWAY only (moqt-23 §3, §9.1, §9.2). Every request opens its own bidirectional stream,
first message on it naming the request, and only TRACK_STATUS, SUBSCRIBE, PUBLISH, FETCH,
PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE and SUBSCRIBE_TRACKS may open one. REQUEST_OK,
REQUEST_ERROR, REQUEST_UPDATE and the responses travel on the request's stream. Closing the
request is STOP_SENDING or RESET_STREAM on that stream; there is no UNSUBSCRIBE.

- A control message longer than the peer's MAX_CONTROL_MESSAGE_SIZE is not sent; one longer
  than ours is `PROTOCOL_VIOLATION`, checked from its length prefix before it is buffered.
- A second SETUP, or a request type opening a stream that may not, is `PROTOCOL_VIOLATION`.
- Concurrent requests are bounded by QUIC stream credit (moqt-23 §7.4): the package gives
  `limits.requests` bidirectional streams and raises the credit only as requests end. A relay
  that cannot open an upstream request in time answers the downstream one
  `REQUEST_ERROR(UNAVAILABLE)` or `TIMEOUT`, never queues without bound.
- Authorization: SETUP's AUTHORIZATION TOKEN and PATH are surfaced as `Event.Setup`; the
  application accepts or closes. Tokens are never cached (MAX_AUTH_TOKEN_CACHE_SIZE 0), so
  no token outlives the request that carried it.

## MOQ-3: The data model

A track is named by a namespace of 0 to 32 byte-string fields and a name (moqt-23 §2). A
track is groups, a group is subgroups, a subgroup is objects in id order. A subgroup's
objects go on one QUIC stream in Subgroup mode, or each object goes in one QUIC datagram in
Datagram mode. Group and object ids only rise within a track; an object id at or below one
already received on the same subgroup stream is `PROTOCOL_VIOLATION`.

## MOQ-4: Data streams and datagrams

The three formats are byte-exact to moqt-23 §11: OBJECT_DATAGRAM, SUBGROUP_HEADER (then
objects, each with its id delta, extension headers and payload length), FETCH_HEADER (then
objects with full location). Extension headers the package does not know are carried
through a relay unchanged and given to the application; the package interprets none in
Wave 2. A data stream is reset with a reliable size of its header (QUIC RESET_STREAM_AT), so
a receiver always learns which subscription a reset stream belonged to.

## MOQ-5: Scheduling and timeouts

Between objects competing for one connection, the order is moqt-23 §5.1.2's: subscriber
priority first, then publisher priority, then group order (ascending or descending as the
subscription asked), then subgroup. This maps onto `quic.priority`: urgency from the two
priorities folded into 0 to 7 by their high bits, and order from the group, so a newer group
of a live track preempts an older one in QUIC's own scheduler with no second queue.

Delivery timeouts (moqt-23 §5.2): an object or subgroup past the smaller non-zero of
SUBGROUP_DELIVERY_TIMEOUT and OBJECT_DELIVERY_TIMEOUT is abandoned: its stream reset, its
datagram not sent. Timeouts are deadlines on the session's clock, folded into the next
`quic.poll` wake.

## MOQ-6: The relay

The relay half follows moqt-23 §7 and keeps every byte it holds counted.

- **Cache.** Keyed by full track name, group and object (moqt-23 §7.1). Per track, at most
  `cache_groups` groups and at most `cache_bytes` across the relay, charged at insert. When
  full, a track's oldest group is evicted first, and the track with the oldest newest-group
  goes first across tracks; eviction is counted, never silent. Payloads are the received
  `Shared[Bytes]`, so caching costs a reference, not a copy.
- **Fan-out.** One upstream subscription per track no matter how many downstream
  subscribers (moqt-23 §7.2). Each received object is offered to each downstream session
  under its own scheduling (MOQ-5) and timeouts; a slow subscriber loses its own objects to
  timeouts and never slows another.
- **Order on a stream.** A relay never reorders or drops objects within one multi-object
  stream (moqt-23 §7.5): an object it cannot send in time ends that stream with a reset,
  and the next group starts a fresh one.
- **Several publishers.** When two publishers serve one track, objects are deduplicated by
  location (moqt-23 §7.3), and switching between them follows §7.5.1 and §7.7.1: at group
  boundaries only.
- **Opaque payloads.** The relay reads headers, never payloads (moqt-23 §7.9). With SFrame
  the payload is ciphertext and the relay holds no key; this is the law M10 checks.

## MOQ-7: Memory and bounds

Every table has a bound from `Limits`, charged when the session or track is created: request
streams, tracks per session, filter ranges per subscription (MAX FILTER RANGES), request
updates outstanding (MAX_REQUEST_UPDATES), the control message buffer, and the relay cache.
A peer can make the relay hold at most its QUIC credit plus the cache it is charged against.

## MOQ-8: The SFU bridge

The SFU publishes a room's media as MoQ so large audiences subscribe through MoQ relays
instead of WebRTC. One track per published layer: namespace `(room, participant)`, name
`<kind>/<rid>`; group per keyframe interval (a group starts at each keyframe the SFU
forwards), object per RTP frame's payload as reassembled by the existing depacketizers. The
bridge runs in the room's unit (RTC-10) and consumes the plaintext `Shared[Bytes]` the
router already shares, so publishing to MoQ adds no copy. RTC-7 applies unchanged: a revoked
participant's tracks end with PUBLISH_DONE before the fence is acknowledged, and an
end-to-end-encrypted room publishes SFrame ciphertext the bridge cannot read. The bridge is
slice M6; its track naming is open item O4.

## MOQ-9: What a session refuses

| Case | Result |
| --- | --- |
| ALPN or `WT-Protocol` not `moqt-23` | connection refused (`no_application_protocol` / `WT_ALPN_ERROR`) |
| Datagram delivery without the QUIC DATAGRAM extension | `PROTOCOL_VIOLATION` at the first such SUBSCRIBE |
| Namespace of more than 32 fields | `PROTOCOL_VIOLATION` |
| Request past `limits.requests` | QUIC `STREAM_LIMIT_ERROR` (the credit was never given) |
| Control message past MAX_CONTROL_MESSAGE_SIZE | `PROTOCOL_VIOLATION` |
| Subscription the application refuses | REQUEST_ERROR with its code |

## Laws

Red first in `core/programs/moq/laws.hirsel` <!-- doc-ref: planned --> unless named.

- **M1 `wire_vectors_from_the_draft`.** SETUP, each request type, REQUEST_OK and
  REQUEST_ERROR, OBJECT_DATAGRAM, SUBGROUP_HEADER and FETCH_HEADER, written by hand from
  moqt-23 §9 and §11's field lists, round-trip byte for byte; a truncated message at every
  length is an error and never a partial read. *Oracle:* the draft text.
- **M2 `interop_with_moq_rs_draft_23`.** Against moq-rs at a pinned commit with `moqt-23`
  named explicitly (its draft-23 port is not advertised by default, version.rs:282): a moq-rs
  publisher through the Hirsel relay to a moq-rs subscriber, and a Hirsel publisher through
  moq-relay to a Hirsel subscriber, 60 s of a 30 fps track of 3 subgroups each. *Oracle:*
  each received object's SHA-256 against the publisher's log. Rides red until moq-rs's port
  is complete (O1).
- **M3 `relay_preserves_stream_order`.** Under `sim.udp` with 10 % loss and reordering, every
  subscriber of a relay receives each subgroup's objects in id order, with no gap except
  where the stream ends in a reset. *Oracle:* moqt-23 §7.5, checked over the trace.
- **M4 `one_upstream_per_track`.** 1,000 subscribers to one track through a relay cause one
  upstream SUBSCRIBE and one copy of each payload in memory (a seam count of `Shared`
  references against allocations). *Oracle:* moqt-23 §7.2 and MOQ-6.
- **M5 `slow_subscriber_costs_only_itself`.** One subscriber capped at 10 % of the track's
  rate and nine uncapped: the nine receive every object within the delivery timeout; the slow
  one's lost objects are counted as timed out. *Oracle:* MOQ-6, checked by each
  subscriber's object log.
- **M6 `priority_order`.** A scripted queue of objects across tracks with chosen subscriber
  and publisher priorities and group orders is sent on one congested connection; the
  sending order equals moqt-23 §5.1.2's order worked by hand. *Oracle:* the draft.
- **M7 `delivery_timeout_takes_the_smaller`.** With SUBGROUP 200 ms and OBJECT 50 ms, an
  object delayed 60 ms is abandoned; with OBJECT 0 (unset) it is sent. *Oracle:* moqt-23
  §5.2.
- **M8 `dedup_and_switchover`.** Two publishers of one track with overlapping groups: each
  location is delivered once, and switching publishers happens at a group boundary.
  *Oracle:* moqt-23 §7.3, §7.5.1, §7.7.1.
- **M9 `cache_bounded_and_fetch_served`.** A FETCH of the last 3 groups is served from the
  cache with no upstream request; at `cache_bytes` the oldest group is evicted and counted,
  and the charge never passes the bound. *Oracle:* MOQ-6.
- **M10 `relay_never_sees_plaintext`.** SFrame-encrypted objects through the relay: the relay
  process's memory, scanned at the end, holds no plaintext frame (a marker payload), and
  subscribers with the key decrypt every object. *Oracle:* the marker search and SFrame's
  own vectors.
- **M11 `cancel_frees_state`.** A subscriber that sends STOP_SENDING on its request stream
  frees the relay's per-subscription state; when the last subscriber of a track leaves,
  the upstream request ends. *Oracle:* moqt-23 §9 and the seam's table counts.
- **M12 `bridge_ends_on_revoke`** (in
  `core/programs/webrtc_sfu/pool_laws.hirsel` <!-- doc-ref: planned -->). A revoked participant's MoQ tracks end before the RTC-7 fence
  acknowledges, and no object of theirs is sent after it. *Oracle:* RTC-7.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **M1** Codec | control messages, data formats, namespace and track names | — | M1 |
| **M2** Session | SETUP, request streams, publisher and subscriber roles, cancellation | M1, quic Q7, webtransport T3 | M2 (peer rows), M11 |
| **M3** Scheduling | MOQ-5 over `quic.priority`; delivery timeouts | M2 | M6, M7 |
| **M4** Relay | cache, fan-out, order, dedup and switchover | M3 | M3, M4, M5, M8, M9 |
| **M5** SFrame | per-object SFrame for publishers and subscribers; relay opacity | M4 | M10 |
| **M6** SFU bridge | MOQ-8 | M4, RTC-10 P3 | M12 |

Reds that may ride: M2 (external peer). M4, M5, M9 and M12 bound memory or revocation and
block.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) has no MoQ. moq-rs (/tmp/ref-moq at `d791d54`) is the
reference: rs/moq-net (the IETF codec under src/ietf, versions in src/version.rs, the
moq-lite variant under src/lite), rs/moq-relay (src/cache.rs, src/cluster.rs), rs/moq-quic,
rs/moq-congestion, drafts/draft-lcurley-moq-cluster.md.

| Source | Mechanism | Verdict |
| --- | --- | --- |
| moq-rs moq-net | several drafts and moq-lite negotiated in one codec (src/version.rs, ALPN `moqt-20`, `moqt-22`, `moqt-23`) | **REPLACED** by one draft per package version |
| moq-rs moq-lite | a simpler non-IETF wire (src/lite) | **NOT TAKEN**: the IETF draft is the interoperable target |
| moq-rs moq-relay | cache of recent groups per track, one upstream per track (src/cache.rs) | **ADOPTED**, with exact byte charges and counted eviction |
| moq-rs cluster | HOP_PATH and ROUTE_COST relay mesh (drafts/draft-lcurley-moq-cluster.md, src/cluster.rs) | **NOT TAKEN** in Wave 2 (O3); sfu_cascade has its own directory |
| moq-rs moq-e2ee | end-to-end encryption above the transport (rs/moq-e2ee) | **ADAPTED** as SFrame per object, the same SFRAME chapter the SFU uses |
| moq-rs moq-congestion | delay-based controllers for media (gcc, scream) beside cubic and bbr | **NOT TAKEN** in Wave 2; [quic](quic.md) O2 |
| Goose | one path per capability | **ADOPTED** as one draft, one relay |

## Open for design review

- **O1** moqt-23 against an incomplete moq-rs port. moq-rs's stable advertisement is
  `moqt-22`; its draft-23 port is a spike offered only when named. Target 23 and ride M2 red,
  or target 22 for working interop today and bump later?
- **O2** Native QUIC and WebTransport both from day one, or WebTransport first since
  browsers are the audience?
- **O3** Relay meshes. Use draft-lcurley-moq-cluster (an individual draft, not adopted)
  for MoQ relay-to-relay routing, or route MoQ only over sfu_cascade's directory?
- **O4** The SFU bridge's track naming and group boundaries; whether MSF or LOC packaging is
  worth following so third-party MoQ players can play SFU rooms.
