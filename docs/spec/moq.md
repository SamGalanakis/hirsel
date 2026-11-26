# Media over QUIC

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Media Wave 2 package 3 of 4); design
review media-w2-spec-fix-2918. MS1 to MS6 implement the codec, sessions, scheduling
and timeouts, relay, per-object SFrame and the SFU bridge. The bridge lives in
`core/programs/webrtc_sfu/moq_bridge.hirsel`. Rules MOQ-1 to MOQ-9 and laws M1 to M12 apply.*

`moq` is a sans-IO MoQ Transport endpoint and relay, written in Hirsel under
`core/programs/moq`. It follows
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
| Transport | native QUIC (ALPN `moqt-23`, URI scheme `moqt`), WebTransport; the QUIC DATAGRAM extension required on every MoQ connection (moqt-23 §6.2) | WebSocket fallback (moq-rs's `qmux`): not in the draft |
| Setup | one SETUP each way (moqt-23 §9.1) with AUTHORITY, PATH, AUTHORIZATION TOKEN, MAX_AUTH_TOKEN_CACHE_SIZE (0: no cache), MOQT IMPLEMENTATION, MAX FILTER RANGES, MAX_REQUEST_UPDATES, MAX_CONTROL_MESSAGE_SIZE | — |
| Requests | SUBSCRIBE, PUBLISH, FETCH, TRACK_STATUS, PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS, each on its own bidirectional stream; REQUEST_UPDATE; cancellation by STOP_SENDING or RESET_STREAM | — |
| Data | OBJECT_DATAGRAM (§11.2.1), SUBGROUP_HEADER streams (§11.3.1), FETCH_HEADER streams (§11.4.1) | — |
| Relay | tenant-scoped caching, fan-out, deduplication, switchover, delivery timeouts (moqt-23 §5.2, §7) | relay-to-relay meshes with routing: [sfu_cascade](sfu_cascade.md) carries the SFU's own links; MoQ cluster routing (draft-lcurley-moq-cluster) is not taken (Design review, O3) |
| Encryption | object payloads opaque to the relay (moqt-23 §7.9); end-to-end per object with [SFrame](sframe.md), keyed per track by the publisher | — |
| Catalogs and packaging | none in the package: the payload is bytes | `hang`, LOC, MSF catalogs: applications choose; the SFU bridge (MOQ-8) uses its own track naming |

## MOQ-1: Shape

```hirsel signatures moq/moq
pub opaque type Session
pub opaque type Relay
pub opaque type LinkId: key

pub type LinkStream: key = LinkStream(u64)

pub type LinkReadError = StreamReset(u64) | Refused(MoqError)

pub type AddressMode: key = InUri | InSetup

pub type LinkEvent =
    Ready(max_datagram: u64)
  | InvalidProtocol
  | Opened(stream: LinkStream, bidi: bool)
  | Readable(LinkStream)
  | Writable(LinkStream)
  | Acked(LinkStream)
  | Reset(stream: LinkStream, code: u64, final_size: u64)
  | Stopped(stream: LinkStream, code: u64)
  | Datagram(quic.PayloadSlice)
  | Closed(code: u64)

pub type AddressedEvent {
    link: LinkId,
    event: LinkEvent
}

pub type LinkOps[D] {
    address_mode: fn(D, LinkId) -> Option[AddressMode],
    max_datagram: fn(D, LinkId) -> u64,
    open: fn(mut D, LinkId, bool) -> Result[LinkStream, MoqError],
    read: fn(mut D, LinkId, LinkStream, mut Bytes) -> Result[(u64, bool), LinkReadError],
    write: fn(mut D, LinkId, LinkStream, Bytes, bool) -> Result[u64, MoqError],
    write_slice: fn(mut D, LinkId, LinkStream, take quic.PayloadSlice, bool) -> Result[bool, MoqError],
    reset: fn(mut D, LinkId, LinkStream, u64, u64) -> Result[(), MoqError],
    stop: fn(mut D, LinkId, LinkStream, u64) -> Result[(), MoqError],
    priority: fn(mut D, LinkId, LinkStream, quic.Priority) -> Result[(), MoqError],
    send_datagram: fn(mut D, LinkId, take quic.FramedSlice, quic.Priority, u64) -> Result[(), MoqError],
    drop_datagrams: fn(mut D, LinkId, u64) -> u64,
    close: fn(mut D, LinkId, u64, Bytes),
}

pub type LinkIo[D] {
    driver: D,
    ops: LinkOps[D]
}

pub opaque type LinkRegistry
pub type MixedDriver {
    q: quic.Endpoint,
    w: Option[webtransport.Endpoint],
    links: LinkRegistry
}
pub type Role: key = Publisher | Subscriber | Both
pub type RequestId: key = RequestId(u64)
pub type Scope: key = Scope(Bytes)
pub type Mode: key = Subgroup | Datagram
pub type Object {
    track: codec.TrackName,
    group: u64,
    subgroup: u64,
    id: u64,
    mode: Mode,
    publisher_priority: u8,
    status: codec.ObjectStatus,
    properties: Bytes,
    payload: quic.PayloadSlice
}
pub type Limits: key {
    requests: u32,
    tracks: u32,
    control_bytes: u32,
    object_bytes: u64,
    filter_ranges: u16,
    max_early_streams: u16,
    max_timeout_ms: u64,
    max_buffered: u64
}
pub type RelayLimits: key {
    cache_groups: u32,
    cache_bytes: u64,
    per_track_cache_bytes: u64,
    max_objects_per_group: u32,
    max_object_bytes: u64
}
pub type Event = Setup(authority: Bytes, path: Bytes, token: Option[Bytes]) | Subscribe(id: RequestId, track: codec.TrackName, filter: codec.Filter, priority: u8, token: Option[Bytes]) | Publish(id: RequestId, track: codec.TrackName, token: Option[Bytes]) | Fetch(id: RequestId, track: codec.TrackName, from: codec.Location, to: codec.Location, token: Option[Bytes]) | Received(Object) | Unknown(track: codec.TrackName, from: codec.Location, to: codec.Location) | Malformed(track: codec.TrackName) | Done(id: RequestId, code: u64) | Goaway(uri: Bytes) | Closed(code: u64)
pub type RelayEvent = Authorize(session: u32, id: RequestId, scope: Scope, track: codec.TrackName, token: Option[Bytes]) | Evicted(scope: Scope, track: codec.TrackName, groups: u32, size: u64) | Ended(session: u32, code: u64)
pub type MoqError = Closed | Role | State | Limit | Track | InvalidParam | Wire(codec.WireError) | Quic(quic.QuicError) | Web(webtransport.WtError)

pub fn mixed_driver(take q: quic.Endpoint, take w: Option[webtransport.Endpoint], max_links: u64) -> LinkIo[MixedDriver]
pub fn mixed_ops() -> LinkOps[MixedDriver]
pub fn native_link(mut d: MixedDriver, c: quic.ConnectionHandle) -> Option[LinkId]
pub fn web_link(mut d: MixedDriver, s: webtransport.SessionHandle) -> Option[LinkId]
pub fn retire_link(mut d: MixedDriver, id: LinkId)
pub fn native_events(mut d: MixedDriver, events: List[quic.Event], mut out: List[AddressedEvent])
pub fn web_events(mut d: MixedDriver, events: List[webtransport.Event], mut out: List[AddressedEvent])
pub fn link_session[D](io: LinkIo[D], link: LinkId, side: schedule.Side, role: Role, limits: Limits) -> Option[Session]
pub fn receive[D](mut s: Session, mut io: LinkIo[D], now: u64, events: List[AddressedEvent], mut out: List[Event])
pub fn poll[D](mut s: Session, mut io: LinkIo[D], now: u64) -> Option[u64]
pub fn accept_setup[D](mut s: Session, mut io: LinkIo[D], take ok: Result[Scope, u64])
pub fn accept[D](mut s: Session, mut io: LinkIo[D], id: RequestId, ok: Result[(), u64])
pub fn subscribe[D](mut s: Session, mut io: LinkIo[D], now: u64, track: codec.TrackName, f: codec.Filter, priority: u8, token: Option[Bytes]) -> Result[RequestId, MoqError]
pub fn publish[D](mut s: Session, mut io: LinkIo[D], now: u64, o: Object) -> Result[(), MoqError]
pub fn end_track[D](mut s: Session, mut io: LinkIo[D], track: codec.TrackName, code: u64)
pub fn cancel[D](mut s: Session, mut io: LinkIo[D], id: RequestId, code: u64)
pub fn close[D](mut s: Session, mut io: LinkIo[D], code: u64, reason: Bytes)
pub fn default_scope(authority: Bytes, path: Bytes) -> Scope

pub fn relay(limits: RelayLimits) -> Relay
pub fn relay_add(mut r: Relay, take s: Session, take scope: Scope) -> u32
pub fn relay_receive[D](mut r: Relay, mut io: LinkIo[D], now: u64, events: List[AddressedEvent], mut out: List[RelayEvent])
pub fn relay_authorize[D](mut r: Relay, mut io: LinkIo[D], now: u64, sid_in: u32, id: RequestId, ok: Result[(), u64])
pub fn relay_poll[D](mut r: Relay, mut io: LinkIo[D], now: u64) -> Option[u64]
```

Track names, filters and object status are declared in `moq.codec`.

```hirsel signatures moq/codec
pub type Namespace: key = Namespace(List[Bytes])
pub type TrackName: key {
    ns: Namespace,
    name: Bytes
}
pub type Location: key {
    group: u64,
    object: u64
}
pub type Filter: key = NoFilter | RelativeStart(groups: u64) | AbsoluteStart(start: Location) | GroupEnd(start: Location, end_group: u64) | AbsoluteRange(start: Location, end: Location) | NextObject
pub type ObjectStatus: key = Normal | EndOfGroup | EndOfTrack
```

The socket owner registers each native connection or WebTransport CONNECT through its
driver. A `LinkId` identifies a registration within that driver. The driver limits live
registrations to `max_links` and assigns each new registration an id it has never issued.
Translate events at transport ingress, before reusing a transport handle. The driver retires
a link on a terminal event or a local close. An owner disposing of a transport directly drains or discards
its raw events before retiring the link. An addressed event already in flight retains its
old id. `receive` and `relay_receive` deliver it only to that link's session.

The mixed driver dispatches to one adapter per link over the shared QUIC endpoint. The
socket owner feeds HTTP/3 events to WebTransport and passes the resulting events to
`web_events`. The native adapter validates the negotiated QUIC ALPN. The WebTransport
adapter validates the selected CONNECT application protocol. `Ready` reports usable
application datagram room after transport framing. A pending CONNECT cannot start MoQ
SETUP. A refused CONNECT produces `Closed`, with the HTTP status as code, or
`ERR_PROTOCOL_VIOLATION` for pin rejection. `AddressMode` controls the SETUP address fields.
`Acked` is delivered to the stream's owning link and clears its subgroup delivery timer.
WT stream priority, reliable reset and datagram purges use the owning session's operations.

`poll` runs the session's delivery timers (MOQ-5) and returns the next deadline. Request ids
are even for the client and odd for the server (moqt-23 §6.4.2.1), counted per session.
`Object.payload` is a validated owned range of the QUIC backing. MoQ datagram receive trims
the header off that range. Stream writes, duplicate checks, the cache and SFrame use its
selected bytes. Datagram sends prepend the MoQ fields into the inline prefix, leaving up
to eight bytes for the WT quarter stream id. If the header is too large for the prefix,
or the source backing would cost more than the previous copied frame, MoQ coalesces the
header and selected bytes. The property-free header fits in inline storage without a
header allocation. MTU checks use wire length.

An `Object` keeps everything moqt-23 makes part of an object's identity or forwarding: its
delivery `mode` and its `properties` (moqt-23 §2.1.1, §8.4), which a relay forwards
unchanged.

`accept_setup` answers `Event.Setup`: `Ok(scope)` accepts the session under an authorization
scope, `Err(code)` closes it. `default_scope(authority, path)` is the scope of a SETUP's
AUTHORITY and the path-abempty of its PATH, which is what an application with no tenancy of
its own passes. `end_track` sends PUBLISH_DONE for every subscription of a track the session
publishes. `close` ends the session with a code and reason.

A `Relay` owns the sessions added to it, each with the scope its `accept_setup` gave, and
runs MOQ-6 over them: `relay_receive` consumes the transport events of all its sessions,
and every request a session receives becomes `RelayEvent.Authorize`, which the application
answers with `relay_authorize`.

## MOQ-2: Control

The control channel is a pair of unidirectional streams, one each way, which carry SETUP and
then only the session-level messages the draft allows there (moqt-23 §6.3, §6.4.1, §9.1, §9.2). Every request opens its own bidirectional stream,
first message on it naming the request, and only TRACK_STATUS, SUBSCRIBE, PUBLISH, FETCH,
PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE and SUBSCRIBE_TRACKS may open one. REQUEST_OK,
REQUEST_ERROR, REQUEST_UPDATE and the responses travel on the request's stream. Closing the
request is STOP_SENDING or RESET_STREAM on that stream; there is no UNSUBSCRIBE.

- A control message longer than the peer's MAX_CONTROL_MESSAGE_SIZE is not sent; one longer
  than ours is `FIELD_LENGTH_EXCEEDED` (moqt-23 §9), checked from its length prefix before it
  is buffered.
- A second SETUP, or a request type opening a stream that may not, is `PROTOCOL_VIOLATION`.
- Concurrent requests are bounded by QUIC stream credit (moqt-23 §7.4): the package gives
  `limits.requests` bidirectional streams and raises the credit only as requests end. A relay
  that cannot open an upstream request in time answers the downstream one
  `REQUEST_ERROR(UNAVAILABLE)` or `TIMEOUT`, never queues without bound.
- Authorization: SETUP's AUTHORITY, PATH and AUTHORIZATION TOKEN are surfaced as
  `Event.Setup`, and each request's own AUTHORIZATION TOKEN parameter, if any, rides on its
  event. The application decides every request. Tokens are never cached
  (MAX_AUTH_TOKEN_CACHE_SIZE 0), so no token outlives the request that carried it.

## MOQ-3: The data model

A track is named by a namespace of 0 to 32 byte-string fields and a name (moqt-23 §2). A
track is groups, a group is subgroups, a subgroup is objects in ascending id order (§2.2). A
subgroup's objects go on one QUIC stream in Subgroup mode, or each object goes in one QUIC
datagram in Datagram mode. Object ids rise *within a subgroup stream*: an object id at or
below one already received on the same stream cannot be written, since each id is the
previous one plus a delta plus one (§11.3.1); a delta past 2^64 − 1 is `DELTA_ENCODING_OVERFLOW`. Nothing orders groups
across a track's streams, or subgroups within a group; they may arrive in any order.

**Malformed tracks** (moqt-23 §12.1). An endpoint keeps, per track, each group's final object
once known and the track's final object, and for each cached object its mode, subgroup,
priority and payload. Any of §12.1's conditions, among them an object past a group's or the
track's end and a duplicate with a different delivery mode, subgroup, publisher priority,
payload or other immutable property (§7.1), makes the track malformed: a subscriber cancels
its requests for the track from that publisher and emits `Event.Malformed`; a relay ends
every downstream subscription of the track with PUBLISH_DONE, resets its fetch streams with
`MALFORMED_TRACK` (`0x12`), and never caches the objects that triggered it. A duplicate that
agrees on all of them is dropped silently (deduplication, MOQ-6).

## MOQ-4: Data streams and datagrams

The three formats are byte-exact to moqt-23 §11: OBJECT_DATAGRAM, SUBGROUP_HEADER (then
objects, each with its id delta, properties and payload length), FETCH_HEADER (then
objects with full location). Properties the package does not know are carried through a
relay unchanged and given to the application; the package interprets only the delivery
timeout properties (MOQ-5). A data stream is reset with a reliable size of its header (QUIC
RESET_STREAM_AT), so a receiver always learns which subscription a reset stream belonged to.

A data stream or datagram may arrive before the request it belongs to is known on this side.
At most `max_early_streams` such streams are held per session, header only; past that the
newest is reset with STOP_SENDING, and early datagrams are dropped and counted.

## MOQ-5: Scheduling and timeouts

Between objects competing for one connection, the order is moqt-23 §5.1.2's: subscriber
priority first, then publisher priority, then group order (ascending or descending as the
subscription asked), then subgroup id (Subgroup mode) or object id (Datagram mode), with a
datagram ahead of a stream when the modes differ. In MoQ a lower number is a higher
priority, as in `quic.Priority`. This maps onto `quic.Priority` exactly, with no second
queue:

| Field | Value |
| --- | --- |
| `urgency` | 0 for the control streams, 1 for request streams, 2 for every object (§5.1.2: control first, then requests, then objects) |
| `order` | subscriber priority (8 bits), publisher priority (8 bits), then the group's rank (48 bits), high bits first |
| `then` | 0 for a datagram and 1 for a stream in the top bit, then the object id (datagram) or subgroup id (stream), which are varints below 2^64 |

A group's rank is fixed for the life of its stream. Each subscription takes as its base the
first group id it delivers. In ascending order the rank is `group − base`; in descending
order it is `(2^48 − 1) − (group − base)`. Both are exact while `group − base < 2^48`; a
group past that (over 8,900 years of groups at 1,000 a second) ends the subscription with
PUBLISH_DONE and a named code, never a wrapped key. A group below the base is outside the
subscription's filter and is not delivered on it. Ranks are compared only within one
subscription, which is all §5.1.2 orders: between subscriptions at equal priorities the
draft leaves the order to the implementation, and QUIC's turn-taking supplies it.
Fill-delivered objects are outside Wave 2 (FETCH serves from cache only).

**Delivery timeouts** (moqt-23 §5.2). For each of SUBGROUP_DELIVERY_TIMEOUT and
OBJECT_DELIVERY_TIMEOUT, the publisher's value is the first object's property when present,
else the track's; when the publisher's and the subscriber's values are both nonzero the
smaller is used. The two are separate timers:

- **Object timeout.** Starts when the last byte of the object's header was received upstream
  (or the object was given by the publishing application), and is checked when the object
  would be handed to `quic`: past it, a subgroup stream is reset with `DELIVERY_TIMEOUT` and
  no new stream is opened for that subgroup, and a datagram is dropped.
- **Subgroup timeout.** Starts when the subgroup's FIN was received upstream (or the
  application ended it), and runs until `LinkEvent.Acked` for the stream; if it fires
  first, the stream is reset. For a datagram it acts as the object timeout does, and the
  smaller of the two applies.

A relay serving a subscriber caps each effective timeout at `limits.max_timeout_ms`, also when
both sides sent 0 (no timeout): without the cap, a subscriber that never acknowledges would
pin the relay's send buffers for as long as its connection lives. Timeouts are deadlines on
the session's clock, returned by `poll` and folded into the next `quic` wake.

## MOQ-6: The relay

The relay half follows moqt-23 §7 and keeps every byte it holds counted.

- **Authorization first.** Every request a relay receives is put to the application as
  `RelayEvent.Authorize` with the session's scope and the request's token, and no cache
  lookup, upstream request or fan-out happens before `relay_authorize` accepts it.
- **Tenant scope.** The cache and every upstream subscription are keyed by
  `(scope, track)`, never the track name alone: two tenants that name the same track share
  nothing. A session's scope is fixed at `accept_setup`.
- **Cache.** Keyed by `(scope, track)`, group and object (moqt-23 §7.1). Each object is
  charged at MEM-24 logical size, its payload's whole allocation plus its properties and its
  record, when it is inserted. The bounds: at most `cache_groups` groups and
  `per_track_cache_bytes` bytes per track, at most `max_objects_per_group` objects per group,
  no object over `max_object_bytes` (larger ones are forwarded and not cached), and
  `cache_bytes` across the relay. When a bound is reached the relay evicts, oldest group
  first, from the track furthest over its fair share of `cache_bytes` (the total divided by
  the tracks cached), so one busy track cannot empty everyone else's cache; eviction is
  counted and reported as `RelayEvent.Evicted`, never silent. Payloads are the received
  `quic.PayloadSlice` ranges, so caching costs a reference, not a copy. Unknown and timed-out ranges are
  never cached (§7.1).
- **Fan-out.** One upstream subscription per `(scope, track)` *per matching publisher*, no
  matter how many downstream subscribers: subscription aggregation (moqt-23 §7.5), and
  §7.3's rule that a relay forwards a track's objects from every publisher it has. Each
  received object is offered to each downstream session under its own scheduling (MOQ-5)
  and timeouts; a slow subscriber loses its own objects to timeouts and never slows another.
- **Order on a stream.** A relay never reorders or drops objects within one multi-object
  stream (moqt-23 §7.5): an object it cannot send in time ends that stream with a reset,
  and the next group starts a fresh one.
- **Several publishers.** Objects from several publishers of one track are deduplicated by
  location (moqt-23 §7.3), and a duplicate that disagrees is a malformed track (MOQ-3).
  Switching between publishers follows §7.5.1 and §7.7.1: at group boundaries only.
- **FETCH from cache.** A FETCH is served from the cache. A range the cache no longer holds,
  because it was evicted or never seen, is answered as End of Unknown Range (§11.4.2), never
  as objects that do not exist, so a subscriber never learns a false permanent gap; the
  relay goes on serving the objects it does hold (§2.1.2). A subscriber receives the range
  as `Event.Unknown`.
- **Opaque payloads.** The relay reads headers, never payloads (moqt-23 §7.9). With SFrame
  the payload is ciphertext and the relay holds no key; this is the law M10 checks.

## MOQ-7: Memory and bounds

Every table has a bound from `Limits` or `RelayLimits`, charged when the session or track is
created: request streams, tracks per session, filter ranges per subscription (MAX FILTER
RANGES), request updates outstanding (MAX_REQUEST_UPDATES), the control message buffer,
per-object payload size (`object_bytes`), early streams, and the relay cache (MOQ-6). An
object header whose declared payload length exceeds `limits.object_bytes` is refused at decode
with `FIELD_LENGTH_EXCEEDED` before buffering, and data-stream receive buffers grow on demand
as bytes arrive (capped at `limits.object_bytes`), never pre-reserving a peer-declared length.
Each session uses `max_buffered` as an independent byte limit for its pending writes and
for the payloads and properties kept by its duplicate guard. A pending write pays for each
chunk's retained allocation and queue record, and one queue record per stream. A complete
handoff releases the MoQ chunk charge. QUIC reserves its own charge for the backing until
it releases the piece. Partial raw writes retain their charge until the chunk leaves MoQ.
If a pending write cannot fit, the session resets that stream with `DELIVERY_TIMEOUT` and
drops its queue. A FETCH writer reports `Limit` when it cannot queue a write. The existing
object and subgroup deadlines still apply.

A duplicate guard retains up to 1024 objects per track. It compares the selected bytes and
pays for the full backing, properties, and its record until the entry retires, even if the
relay cache has evicted the object. If its byte limit cannot accommodate a new entry, the
session ends with `INTERNAL_ERROR`. The limit cannot hide a conflicting duplicate by
evicting it early. A malformed track releases its object byte holders, and session close
releases both budgets.

The cache charges the retained backing, the stored property and track-name capacity, and
the object record with an allowance for list growth. Record charges are fixed on
admission and refunded when the group is evicted. Each charged holder accounts for its
backing independently.

## MOQ-8: The SFU bridge

The SFU publishes live room media as MoQ. The bridge is a virtual subscriber
unit in the RTC-10 pool. It offers one MoQ track per bridged layer of an
accepted down-track. The namespace is `(room, participant)` and the track
name is `<kind>/<section>/<rid>`. The section distinguishes publications
of the same media kind. A new epoch of the same participant can reuse the
name, with a new subscription and fresh group IDs so older duplicate guards
cannot mistake new frames for retransmissions.

The bridge follows the accepted control order through `router.Observer`.
It routes on the down-track's target and matches the publication's owner
generation when it takes RTP. It posts wanted masks and key requests to the
publisher's home. `Pool.bridge` configures a live publisher on native QUIC
and can enable WebTransport on the same socket. A native SETUP that supplies
an address must match the listener. A WebTransport CONNECT must match the
listener's path and authority, pass the configured origin policy, and
negotiate the MoQ protocol. SUBSCRIBE is admitted only while its track is
installed and allowed to route. This publisher returns REQUEST_ERROR
`NOT_SUPPORTED` for FETCH, during and after publication. Its session state
has no local relay cache or FETCH writer. Downstream relays retain their own
MOQ-6 cache policy.

The bridge bounds each layer's jitter window and drops a frame past its
configured byte limit. If the next packet would extend the buffered window
past its bound, the bridge polls for eligible frames and drops and counts the
backed-up window if it still cannot fit. Video waits for a fresh keyframe after either
drop. A keyframe begins a group. Each completed jitter `Frame` becomes an
object. Opus needs no RTP marker. Video waiting for a preceding packet keeps
its marker in the reorder window. The bridge moves the completed frame's
owned bytes into a shared payload range. RTP extraction and depacketization
still form the contiguous frame. Publisher SFrame ciphertext passes through
depacketization and MoQ as payload bytes. The bridge holds no SFrame keys.

**The socket fence** (RTC-7, law M12). The bridge applies the accepted
`Revoke`, stops offering the covered down-tracks and drops their buffered
RTP and pending subscription grants. The socket owner commits only the prefix
accepted by UDP. It abandons any held sealed suffix, invalidates the wire and
descriptors, and removes requeued track work before it can poll again.
For each covered track and MoQ session it:

1. Resets open subgroup streams at the end of the subgroup header and drops
   their application queues. If the reliable offset is unavailable it also
   tries offset zero. A WT stream blocked on its transport header has
   accepted no application payload. The bridge drops its pending application
   bytes in this case too.
2. Drops datagrams with the track's tag. WebTransport scopes that tag to its
   CONNECT.
3. Refuses pending subscriptions with `DOES_NOT_EXIST` and ends active ones
   with PUBLISH_DONE.
4. Acknowledges after all covered tracks and queues are settled.

No object bytes from the revoked generation leave the bridge socket after
the acknowledgment, including retransmissions. Bytes already accepted by
UDP and copies at downstream relays remain outside this boundary. The fence
preserves other publications. Detach and room close apply the same send
fence to every covered track. A suspension purges the room's pending payloads
and holds delivery until every cause clears. Established subscriptions stay
open across suspension.

## MOQ-9: What a session refuses

| Case | Result |
| --- | --- |
| ALPN or `WT-Protocol` not `moqt-23` | connection refused (`no_application_protocol` / `WT_ALPN_ERROR`) |
| A connection without the QUIC DATAGRAM extension | refused at setup (moqt-23 §6.2) |
| Namespace of more than 32 fields, or an empty field | `INVALID_TRACK_NAMESPACE` |
| Request past `limits.requests` | QUIC `STREAM_LIMIT_ERROR` (the credit was never given) |
| Control message past MAX_CONTROL_MESSAGE_SIZE | `FIELD_LENGTH_EXCEEDED` |
| Object payload length past `limits.object_bytes` | `FIELD_LENGTH_EXCEEDED` |
| Object id delta past 2^64 − 1 on its subgroup or fetch stream | `DELTA_ENCODING_OVERFLOW` |
| A track meeting a §12.1 condition | malformed track (MOQ-3) |
| Request the application refuses, at a session or a relay | REQUEST_ERROR with its code |
| Early streams past `max_early_streams` | the newest reset with STOP_SENDING |

## Laws

Red first in `core/programs/moq/laws.hirsel` unless named.

- **M1 `wire_vectors_from_the_draft`.** SETUP, each request type, REQUEST_OK and
  REQUEST_ERROR, OBJECT_DATAGRAM, SUBGROUP_HEADER and FETCH_HEADER, written by hand from
  moqt-23 §9 and §11's field lists, round-trip byte for byte; a truncated message at every
  length is an error and never a partial read. Subgroup object ids, fetch entries in both
  Group Orders (End of Group, End of Track and End of Range included), padding, the §8.6
  range example, and the draft's refusals (each with its §12 code) ride in the same law.
  *Oracle:* the draft text.

**Codec choices where moqt-23 is silent or self-contradictory** (MS1):

- MAX_CONTROL_MESSAGE_SIZE (Setup Option 0x09) is odd, so by §8.3 its value is Length and
  bytes, while §9.1.8 calls it a varint. The codec frames it by parity and requires the
  bytes to hold exactly one varint (else `MALFORMED_KEY_VALUE`), at least 65535
  (else `PROTOCOL_VIOLATION`).
- Object Properties Length (§11.1.2) has no bound; the codec refuses one past 65535, the
  Key-Value-Pair value bound of §8.3, with `FIELD_LENGTH_EXCEEDED` before it is buffered.
- The REDIRECT detail's Connect URI takes GOAWAY's 8,192-byte bound (§9.2).
- A fetch stream whose first entry is End of Group or End of Track names no prior object;
  it is `MALFORMED_OBJECT`.
- NAMESPACE (0x8) takes no Message Parameters: none names it in §9.19. REQUEST_UPDATE and
  REQUEST_OK accept the union of what any request kind allows; MS2 narrows them to the
  request on the stream.
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
  upstream SUBSCRIBE per publisher of the track (one, then two when a second publisher
  appears) and one copy of each payload in memory (a seam count of `Shared` references
  against allocations). Two scopes subscribing to the same track name cause two upstream
  subscriptions and share no cached object, and a request the application has not yet
  authorized causes no upstream request and no cache read. *Oracle:* moqt-23 §7.3 and §7.5,
  and MOQ-6.
- **M5 `slow_subscriber_costs_only_itself`.** One subscriber capped at 10 % of the track's
  rate and nine uncapped: the nine receive every object within the delivery timeout; the slow
  one's lost objects are counted as timed out. *Oracle:* MOQ-6, checked by each
  subscriber's object log.
- **M6 `priority_order`.** A scripted queue of objects across tracks with chosen subscriber
  and publisher priorities and group orders, with group ids near 2^32 and 2^47 and subgroup
  ids past 2^16, and a datagram and a stream in one group, is sent on one congested
  connection; the sending order equals moqt-23 §5.1.2's order worked by hand. *Oracle:* the
  draft.
- **M7 `delivery_timeouts_start_and_stop_apart`.** Each timer on its own trigger: with OBJECT
  50 ms, an object whose header arrived 60 ms before it could be handed to `quic` resets its
  stream with `DELIVERY_TIMEOUT`, and one handed over at 40 ms is sent however long it then
  takes to be acknowledged. With SUBGROUP 200 ms, a subgroup whose FIN arrived and whose
  stream is not `Acked` 200 ms later is reset, and one acknowledged at 190 ms is not; the
  subgroup timer never starts before the FIN. With publisher OBJECT 80 ms and subscriber 50
  ms the 50 applies; with either 0 the other applies; with both 0 and `max_timeout_ms` 500,
  the relay's cap applies. A datagram with OBJECT 100 ms and SUBGROUP 30 ms is dropped after
  30 ms. *Oracle:* moqt-23 §5.2 and its table, by hand.
- **M8 `dedup_and_switchover`.** Two publishers of one track with overlapping groups: each
  location is delivered once, and switching publishers happens at a group boundary. A
  duplicate with a different payload, subgroup, priority or mode makes the track malformed:
  downstream subscriptions end with PUBLISH_DONE and the object is not cached. Objects of
  one group arriving on two subgroup streams in interleaved id order are all accepted.
  *Oracle:* moqt-23 §2.2, §7.1, §7.3, §7.5.1, §7.7.1 and §12.1.
- **M9 `cache_bounded_and_fetch_served`.** A FETCH of the last 3 groups is served from the
  cache with no upstream request; at `cache_bytes` the oldest group of the track furthest
  over its share is evicted and counted, and the charge, at MEM-24 logical size, never
  passes `cache_bytes`, `per_track_cache_bytes` or `max_objects_per_group`. A publisher of
  zero-byte objects with large properties is bounded the same. A FETCH spanning an evicted
  group gets End of Unknown Range for it and the held groups after it. *Oracle:* MOQ-6 and
  moqt-23 §11.4.2.
- **M10 `relay_never_sees_plaintext`.** SFrame-encrypted objects through the relay: the relay
  process's memory, scanned at the end, holds no plaintext frame (a marker payload), and
  subscribers with the key decrypt every object. *Oracle:* the marker search and SFrame's
  own vectors.
- **M11 `cancel_frees_state`.** A subscriber that sends STOP_SENDING on its request stream
  frees the relay's per-subscription state; when the last subscriber of a track leaves,
  the upstream request ends. *Oracle:* moqt-23 §6.4.2 and the seam's table counts.
- **M12 `bridge_ends_on_revoke`** (in
  `core/programs/webrtc_sfu/pool_laws.hirsel`). Scoped to the bridge's send boundary (MOQ-8). Under
  `sim.udp` with 20 % loss, so retransmissions are pending, and with a polled but
  uncommitted batch at the moment of revocation: a revoked participant's MoQ tracks end
  with PUBLISH_DONE before the RTC-7 fence acknowledges, and after it no datagram leaves the
  bridge's socket carrying a byte of their objects past a stream header, neither a first
  send nor a retransmission, nor a queued datagram of theirs. Their reassembly buffers are
  freed. *Oracle:* RTC-7, checked on the socket trace.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **MS1** Codec | control messages, data formats, namespace and track names | — | M1 |
| **MS2** Session | `LinkOps` over native QUIC and WebTransport, SETUP and `accept_setup`, request streams, tokens, publisher and subscriber roles, cancellation, `close`, malformed tracks | MS1, quic QS7, webtransport TS3 | M2 (peer rows), M11 |
| **MS3** Scheduling | MOQ-5 over `quic.Priority`; the two delivery timers and `max_timeout_ms` | MS2 | M6, M7 |
| **MS4** Relay | the `Relay` API, authorization, tenant-scoped cache and its bounds, per-publisher fan-out, order, dedup and switchover, unknown ranges | MS3 | M3, M4, M5, M8, M9 |
| **MS5** SFrame | per-object SFrame for publishers and subscribers; relay opacity | MS4 | M10 |
| **MS6** SFU bridge | MOQ-8: the virtual subscriber unit, budgeted reassembly, the egress fence | MS5, RTC-10 P3, quic QS7 | M12 |

Reds that may ride: law M2 (external peer). Laws M4, M5, M9 and M12 bound memory or
revocation and block.

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

## Design review

Rulings of the media Wave 2 design review (VERDICT-MEDIA-W2, 2026-11-26), applied above:

- **O1 (moqt-23 against an incomplete moq-rs port).** `moqt-23` only, with no `-22` path;
  law M2 rides red until a `-23` peer is available.

Not ruled by the review; the standing choice is kept as written:

- **O2 (native QUIC and WebTransport).** Both from day one, now behind one `Io` and
  `Transport` (MOQ-1): the review's API fix makes the second transport a variant, not a
  second session type.
- **O3 (relay meshes).** MoQ relays do not route among themselves in Wave 2;
  draft-lcurley-moq-cluster is an individual draft and is not taken. The SFU's own links are
  [sfu_cascade](sfu_cascade.md).
- **O4 (bridge track naming and packaging).** The bridge's own naming (MOQ-8), with the
  canonical participant id; no MSF or LOC packaging in Wave 2.
