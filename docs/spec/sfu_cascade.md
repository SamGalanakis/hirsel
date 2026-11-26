# SFU cascade

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Media Wave 2 package 4 of 4); design
review applied the same day (media-w2-spec-fix-2918). Not built. Rules CAS-1 to CAS-8, laws K1
to K10, slices KS1 to KS5. The review's rulings are at the end.*

`sfu_cascade` lets one room span several SFU nodes. Participants join the node nearest
them; the nodes relay each published layer between them over [QUIC](quic.md). It lives in
`core/programs/webrtc_sfu/cascade` as part of the SFU, because
its state is the router's: down-tracks, layers, fences. It extends [RTC-10](webrtc.md)'s
pooled service; a node that cascades no room behaves exactly as RTC-10 says.

The room directory it depends on is specified here as a contract, `RoomDirectory` (CAS-1),
with an in-memory fake that slices KS1 to KS5 build and test against. The production
directory is an adapter of the same contract over a linearizable Hirsel Store cell with
compare-and-set on `term`.

## CAS-1: Topology and the directory contract

- **Star per room.** Each room has one **home** node and any number of **edge** nodes. Every
  edge links to the home; edges never link to each other for that room. The home forwards a
  layer from one edge to another. Two hops at most, no loops, and one place where a fence
  starts (Design review, O3).
- **One link per node pair.** A link is one QUIC connection between two nodes, shared by
  every room the pair has in common. Rooms are multiplexed on it by a link-local room
  number assigned by the home; link numbers, like RTC-10's `RoomId`, are never reused on a
  link.
- **The directory contract.** The directory is linearizable per room. For each room it holds
  `(home: NodeId, term: u64)`, where `term` rises each time the home changes, the end of the
  home's term lease, and the room's **fence table**: every `(participant, epoch)` fence a
  home of the room has committed. For each node it holds its address and its pins. A node
  acts as home only for a term the directory gave it, and every link message about a room
  carries the room's term; an edge refuses any message with a term lower than the highest it
  has seen for that room. So two homes in different terms cannot both drive an edge.

```hirsel unbuilt
pub opaque type RoomDirectory
pub type DirEntry { home: NodeId, term: u64, fences: List[(u64, u64)] }
pub type NodeEntry { addr: Addr, pins: List[quic.Pin] }
pub type DirError = Stale(term: u64) | Held(until_ms: u64) | Unknown | Unavailable

pub fn sim_directory(clock: Clock, drift_ppm: u32) -> RoomDirectory
pub fn read(d: RoomDirectory, room: Bytes) -> Result[DirEntry, DirError] waits
pub fn node(d: RoomDirectory, n: NodeId) -> Result[NodeEntry, DirError] waits
pub fn claim(mut d: RoomDirectory, room: Bytes, n: NodeId, seen: u64) -> Result[u64, DirError] waits
pub fn renew(mut d: RoomDirectory, room: Bytes, n: NodeId, term: u64, seq: u64) -> Result[u32, DirError] waits
pub fn commit_fence(mut d: RoomDirectory, room: Bytes, term: u64, participant: u64, epoch: u64) -> Result[(), DirError] waits
```

  - `claim(room, n, seen)` makes `n` home in term `seen + 1`, by compare-and-set on `term`:
    `Stale(t)` if the term is no longer `seen`, and `Held(until)` while the old term's lease
    may still be held (below). The new home reads the fence table with the term.
  - `renew(room, n, term, seq)` renews `n`'s term lease and answers the lease length in
    milliseconds, `Stale` if `term` is not current.
  - `commit_fence` appends a fence to the room's table, by compare-and-set on `term`:
    `Stale` if the home has lost the term. A committed fence is never removed while the room
    exists.
  - `sim_directory` is the in-memory fake: one linearizable map under the simulator's clock,
    with injectable unavailability and delay, which every K law uses. The Store adapter
    implements the same six operations and is checked by the same laws.

- **The home's term lease.** A home holds its term only under a lease from the directory,
  counted as CAS-5 counts an edge's lease: from the moment the home sent its `renew`, for the
  length the directory answered. The directory counts it from when it answered, by its own
  clock. A home routes media of the room, and grants edges leases, only while its term lease
  holds, and every grant it gives an edge is shrunk so that it ends before the home's own
  term lease can (CAS-5). `claim` answers `Held` until the old term's lease has provably
  ended by CAS-5's rule, `lease_ms × (1 + d) / (1 − d)` of the directory's clock after its
  last answer to `renew`, rounded up. So a home cut off from the directory stops forwarding,
  and so do its edges, before a new home can act, and a new home's `revoke` never races an
  old home that still forwards.

## CAS-2: Links

```hirsel unbuilt
pub type NodeId = NodeId(Inline[u8, 16])
pub type LinkLimits { rooms: u32, layers: u32, link_bytes: u64, lease_ms: u32, drift_ppm: u32, guard_ms: u32 }
pub type Cascade { node: NodeId, identity: quic.Identity, directory: RoomDirectory, limits: LinkLimits }
pub type CascadeError = Term(u64) | Router(RouterError)

pub fn revoke(mut c: Cascade, mut r: Router, room: RoomId, participant: u64, epoch: u64) -> Result[(), CascadeError] waits
```

A link is mutually authenticated: each side presents its node certificate and pins the
other's from the directory's `NodeEntry.pins`, through `quic`'s `client_pins` on the
accepting side and the pin list on the dialing side (QUIC-3). An unknown peer is refused at
the handshake, before any stream opens. A link uses ALPN `hirsel-cascade-1`.

**Attachment to the pool.** A `Cascade` is part of RTC-10's `Pool` (`Pool.cascade`). Link
control runs on the pool's control unit (Unit 0) beside the room clients; each link's QUIC
endpoint lives on one media unit, and media to and from other units reaches it as inter-unit
messages, as RTC-5's fan-out already moves them.

**Canonical participants.** A room's home owns its participant identities. A participant who
joins at an edge is admitted by the edge's RTC-8 adapter, but its `(participant, epoch)` is
assigned by the home: the edge asks over the room's stream and admits only with the answer.
Edges never allocate participant ids of their own, so a fence names one participant on every
node (Design review, O6).

Each link carries:

- **One link control stream**, bidirectional, opened by the dialing node, at urgency 0 so no
  room's traffic is ahead of it: link hello, room attach and detach, lease requests and
  grants (CAS-5), fences and their acknowledgements (CAS-4), and layer announcements (below).
  A malformed message on it closes the link.
- **One room stream per attached room**, bidirectional, opened at attach, at urgency 1:
  participant admissions, subscriptions to layers, wanted masks and switch requests, scores
  and active-speaker levels, sender reports, and NACKs. Each room's stream has its own QUIC
  flow-control window, so a room that floods or stalls its stream delays only itself, and a
  malformed message on it detaches that room from the link and leaves the others. Fences for
  a room never travel on its room stream, so a room stream's backlog never delays a fence.
- Every control message carries `(room number, term, seq)`; `seq` is per room, per stream
  and per direction, so a room's messages are applied in order and an acknowledgement names
  what it covers.
- **QUIC datagrams for media.** Each datagram is one RTP packet as the router holds it after
  SRTP removal: the plaintext `Shared[Bytes]` of RTC-5, behind an 8-byte header of room
  number (`u32`) and layer number (`u32`). QUIC encrypts the link; SRTP is not reapplied
  between nodes (Design review, O4). A room with SFrame (end-to-end) carries SFrame
  ciphertext as its payload, which the nodes cannot read, exactly as on one node. Each media
  datagram is sent with its room number as its `quic` tag, so a room's queued datagrams can
  be dropped at once (QUIC-7, CAS-5).
- **Layer numbers.** The sending node announces each layer on the link control stream before
  its first datagram: `Layer(room, term, layer_num, participant, epoch, kind, rid)`. A
  `layer_num` is bound to that `(participant, epoch)` and encoding for good, rises within a
  `(room number, term)`, and is never reused; a room that would pass 2^32 layer numbers in
  one term is detached and reattached under a new room number. A datagram whose layer is not
  announced is dropped and counted.
- **Canonical RTP metadata.** On a link, packets use one cascade table of header-extension
  ids and payload types, fixed in the package (`cascade/canon.hirsel` <!-- doc-ref: planned -->): extension ids are
  the order of RTC-1's `Ext` variants from 1, and payload types are RTC-2's codec formats in
  order from 96. The sending node rewrites each packet into the table as RTC-5 rewrites for a
  subscriber, and the receiving node treats a link layer as a publication whose agreed
  formats and extmaps are the table, so its own RTC-5 rewrite for each local subscriber is
  unchanged.
- **Link MTU.** A link carries media only after DPLPMTUD has validated a path MTU that fits
  the largest RTP packet the room admits plus headers (1,400 bytes of datagram payload by
  default, Design review, O5). Until then, or if the path cannot, the link is refused for
  media and reported: splitting an RTP packet over datagrams or falling back to a stream
  would add latency or head-of-line blocking that the room's clients never agreed to.

## CAS-3: Forwarding

- **One copy per layer per node.** A node subscribes upstream to a layer when its first local
  down-track needs it and ends the subscription when its last one leaves, as RTC-6's
  dynacast already decides per publication. The home sends each layer once over each link
  that subscribed it, and an edge fans it out locally through its own router.
- **Layer choice.** Each edge reports, per remote publication, its node's wanted mask: the
  union of its units' RTC-6 local wanted masks, sent on the room stream whenever it changes.
  The home takes each edge's mask as one more unit's `Wanted` in its own union, and forwards
  over a link exactly the encodings in that edge's mask. So an edge with one subscriber on
  the low layer and another on the high one receives both. A remote publisher's layers are
  switched at keyframes as local ones are, by the node where the subscriber is.
- **Repair is edge-local.** Each node keeps its own RTP-12 histories for the layers it
  receives, and answers its local subscribers' NACKs from them. A packet a node lacks is
  asked of its upstream node once per sequence number over the room stream; the upstream
  answers from its own history with a datagram, or not at all. A NACK never crosses two
  links: the home answers from its own history. PLI and FIR from an edge go to the home,
  which rate-limits them per publication as RTC-6 does for local subscribers.
- **Congestion.** The link's QUIC controller (QUIC-6) bounds the link. When a link's datagrams
  are dropped for congestion (counted by `quic`), the sending node lowers the layers it
  forwards over that link, highest first, as RTC-6's bandwidth estimate does for one
  subscriber. A congested link costs the rooms on that link, not other links.
- **Sender reports.** The home forwards each publication's RTCP SR timing over the room
  stream so edge nodes rewrite timestamps the same way the home does (RTC-4), and
  subscribers on any node see one consistent clock per publication.

## CAS-4: The cross-node fence

`revoke(room, participant, epoch)` is called on the home. It returns `Ok(())` only when no
node of the room will route that participant's media at a fenced epoch, the same guarantee
as RTC-7 but across nodes, and only once the fence is durable:

0. The home commits the fence to the directory with `commit_fence(room, term, participant,
   epoch)`, a compare-and-set on its term. If it fails with `Stale(t)` the node is no longer
   home and `revoke` returns `Err(Term(t))` at once; the caller revokes at the new home,
   which already has the fence table. While the directory is unavailable the step waits. So
   a home that fails after `Ok` cannot take the fence with it: the next home reads it with
   its term (CAS-7).
1. The home applies RTC-7 on its own units.
2. It sends `Fence(participant, epoch, seq)` on the link control stream of every link in the
   room's grant ledger (CAS-5), after its own fence holds. Every link: not only those it
   believes involved, for the reason RTC-7 gives for units.
3. An edge applies RTC-7 locally, with one addition: datagrams from the link whose layer
   belongs to the participant at a fenced epoch are dropped when taken, like an inter-unit
   message in RTC-7 step 4. It then acknowledges `seq`.
4. The home returns `Ok(())` when every edge in the ledger has acknowledged, or when CAS-5
   makes an unacknowledged edge safe.

Both ends of a link drop a fenced participant's media at ingress: the edge as step 3 says,
and the home for datagrams arriving from edges, whose layer numbers map to a fenced
`(participant, epoch)` once step 1 holds. Media of the participant already in a link's
datagrams before a node's fence point may still arrive, and that node drops it, because its
fence is set before it acknowledges.

Suspension crosses nodes the same way, as its own cause (RTC-7's suspend cause set): a lease
or term suspension on an edge is cause `Lease` or `Term`, never `Signaling`, so RTC-8's
signaling recovery cannot lift it, and a lease renewal cannot lift a signaling suspension.

## CAS-5: Partitions fail closed

A fence must not wait forever for an edge it cannot reach, and it must not return while that
edge might still be forwarding. Leases resolve both (Design review, O1). Below, `d` is
`drift_ppm / 10⁶` and `L` is `lease_ms`.

- **The lease clock.** Every lease time in this chapter is read from a clock that keeps
  counting while the host is suspended: `CLOCK_BOOTTIME` on Linux, not
  `CLOCK_MONOTONIC`, which stops during suspend. A host clock that does not advance while a
  VM is paused, and does not step forward on resume, is a drift outside `drift_ppm`: the
  guarantee does not cover a node whose VM is live-paused while it holds a lease, and
  cascade nodes must not be paused that way.
- **Leases.** An edge may forward media of a room (to its local subscribers and up the link)
  only while it holds a lease from the home. It asks for renewal every `L / 3` with
  `LeaseRequest(room, term, seq)`; `seq` rises with each request. The home answers
  `LeaseGrant(room, term, seq, remaining)`, echoing the request's `seq`. The edge applies a
  grant only if its `seq` is that of a request it sent, takes the lease's start to be the
  moment it *sent that request*, by its own lease clock, and holds the lease until that start
  plus `min(L, remaining)`. A late grant for an older request therefore never extends a lease
  from a newer request's start. The home counts the lease from the moment it *sent the
  grant*, by its own clock.
- **Transitive leases.** A home's grant must end before its own term lease can. With `R` the
  milliseconds left on the home's term lease when it grants, `remaining` is
  `floor(R × (1 − d) / (1 + d)) − guard_ms`, and no grant is sent when that is not positive.
  The edge's lease then ends, in real time, at most `remaining / (1 − d)` after the grant,
  and the home's term lease lasts at least `R / (1 + d)` of real time after it, which is no
  sooner.
- **The deadline is checked where media leaves.** Every media unit holds each room's lease
  deadline: on an edge the end of its lease, on the home the end of its term lease, both as
  values of the shared lease clock. A unit puts a room's media into a `udp_send` batch, or
  into a QUIC packet it commits, only while `now + guard_ms ≤ deadline`, checked when the
  batch is handed to the socket, not when the media was routed. `guard_ms` (default 5 ms)
  covers one NET-16 `udp_send` batch, which must complete within it; at `deadline −
  guard_ms` the unit drops the room's queued link datagrams with `quic.drop_datagrams` and
  its batched packets for local subscribers. So no media of the room leaves a node after its
  deadline, whatever the control unit is doing, and RTC-7's suspend (below) is cleanup, not
  the fence.
- **Expiry.** When an edge's lease ends without renewal, the edge also applies RTC-7 suspend
  with cause `Lease` to the room on its own units. It forwards nothing of the room, in either
  direction, until a new lease. Local ICE and DTLS stay up, as suspend keeps them, so
  recovery needs no new handshakes.
- **No renewal across a pending fence.** From the moment the home sends a fence to an edge, it
  grants that edge no lease renewal for the room until the edge acknowledges the fence.
- **The grant ledger.** The home keeps, per room, every edge it has granted a lease in the
  current term, with the time it sent the last grant. An edge leaves the ledger only when it
  has acknowledged every fence sent to it and detached, or when its last grant is older than
  the wait below. Losing the link or the edge detaching does not remove it: CAS-4 waits on
  every edge in the ledger, linked or not.
- **The fence across a partition.** The home returns `Ok(())` when the edge acknowledges, or
  when `ceil(L × (1 + d) / (1 − d))` of the home's clock has passed since the last grant it
  sent that edge before the fence, whichever comes first. In the second case it closes the
  room on that link. The edge counts its lease from its request, which in real time precedes
  the grant, so its lease ends within `L / (1 − d)` of real time from the grant. Even with
  the home's clock fast by `d`, its wait lasts at least `L / (1 − d)` of real time, which
  covers the lease of an edge whose clock is slow by `d`. The home's wait rounds up and the
  edge's remaining time rounds down, so integer milliseconds only widen the margin. By the
  time the home returns, the edge's units have stopped sending the room's media.
- **What the caller sees.** `revoke` returns `Ok(())` in both cases, because both are the
  guarantee. The router also emits `Event.Severed(room, node)` for each edge it dropped, and
  RTC-8's adapter turns that into a room message, so the application can move those
  participants.
- **Bound.** A fence's latency across nodes is at most the directory commit, plus the slowest
  edge's RTC-7 bound plus one link round trip when every edge answers. When one cannot, the
  home's wait is `ceil(L × (1 + d) / (1 − d))` of its own clock after the last grant, which
  is at most `L × (1 + d) / (1 − d)²` of real time, plus 1 ms of rounding; the last grant
  came before the fence, so that bounds the time after the fence is sent. The default `L`
  is 2,000 ms.
- **Clock assumption.** The only assumption is that every node's lease clock rate stays
  within `drift_ppm` (default 200 ppm) of real time. There is no assumption about wall-clock
  agreement. A node whose clock runs faster than that breaks the guarantee. This is the same
  assumption leases make in Chubby and in etcd's leader lease.

## CAS-6: Memory and bounds

A link is charged at attach: per room, its layer table, its sequence windows, its room
stream's buffers and its RTP-12 histories for the layers it may receive, from the room's
`RoomLimits` (RTC-10's `room_cost` gains a `cascade` term per link). The link itself is
charged QUIC-9's cost, and the home's grant ledger one entry per edge per room.
`limits.rooms` and `limits.layers` bound what one peer node can ask for, and an attach past
either is refused by name, as RTC-10 refuses an Open. Datagrams a node cannot route at once
are dropped and counted, never queued, as RTC-5's ingress does.

## CAS-7: Faults

- A link's loss (idle timeout, reset, a QUIC error) ends its rooms' attachments on both
  sides, but not the home's grant ledger (CAS-5). The edge's leases run out and it suspends
  those rooms. When the link returns the edge reattaches under new link-local room numbers,
  in this order: the home sends the room's whole fence table on the link control stream,
  the edge applies it and acknowledges, and only then does the home send the room's first
  `LeaseGrant` on the new link. Masks, SRs and subscriptions follow on the new room stream.
  An edge therefore never forwards a room's media under a lease while missing a fence the
  room has.
- A malformed message on a room stream detaches that room from the link and leaves the
  link's other rooms running. A malformed message on the link control stream closes the
  link.
- A home change (a new term in the directory) ends every lease of the old term. The new home
  reads the room's fence table with its term from `claim`, applies every fence on its own
  units before it routes, and sends the table to each edge before granting it a lease, as
  on reattach. Edges that see the new term detach from the old home and attach to the new
  one.

## CAS-8: Not in Wave 2

- Edge-to-edge links, trees and meshes (Design review, O3).
- Cascading MoQ relays; MoQ has its own relay ([MoQ](moq.md) MOQ-6, its O3).
- Recording on an edge; recording stays on the home (RTC-9).
- Moving a participant between nodes mid-session: they rejoin with a higher epoch on the
  other node, as RTC-8 rejoins.
- More than one link per node pair. One link is one QUIC connection and so one unit's work,
  which caps a node pair's cascade traffic at one core. That is accepted for KS1 to KS5; the
  scaling path is one link per pair of `Steer.Quic` members, with rooms spread across them.

## Laws

Red first in `core/programs/webrtc_sfu/cascade/laws.hirsel`,
under `sim.udp` with three simulated nodes and `sim_directory` unless named.

- **K1 `partition_fence_fails_closed`.** A home and two edges; the link to edge B is cut, and
  then `revoke` is called for a participant on edge B who is sending to subscribers on B. The
  trace must show two things: `revoke` returns within `L × (1 + d) / (1 − d)²` of real time
  (plus 1 ms) of the last grant to B, and from the moment it returns, B's units send no
  datagram carrying that participant's media to anyone, including a batch that B's media
  unit had built before its deadline and a turn of B's control unit stalled past the
  deadline. Repeat with B's clock running fast and slow at `drift_ppm`, and the home's the
  other way. Then cut the home from the directory and from both edges. A new home in the
  next term revokes a participant whose subscribers sit on the old home. From the new
  home's return onward, neither the old home nor an edge holding a transitive lease from it
  sends media of the room. A grant answering an older request, delivered late, extends no
  lease. *Oracle:* CAS-5's inequalities, worked by hand in the law with `d` at its bound,
  and CAS-1's term lease, checked over the trace's real time.
- **K2 `fence_reaches_every_node`.** With all links up, a revoke returns after both edges
  acknowledge. After it returns, no node sends the participant's media, and a rejoin at the
  same epoch on any node is `Fenced`. An edge whose link drops after it was granted a lease
  and before the fence is still waited for. The home is killed right after `revoke` returns
  `Ok`; the next home, from `claim`, refuses the participant's rejoin at that epoch and
  drops its media on ingress. A home that lost its term gets `Err(Term)`. *Oracle:* RTC-7
  applied across nodes, and CAS-4 step 0.
- **K3 `one_copy_per_layer_per_node`.** 50 subscribers on edge A to one publisher on edge B:
  each forwarded packet crosses B→home once and home→A once. A second publisher's layers
  cross only to nodes with a subscriber. Two subscribers on A, one wanting only the low
  simulcast layer and one only the high, receive both layers across the link and nothing of
  the middle one. *Oracle:* link datagram counts against the subscription table and RTC-6's
  wanted mask.
- **K4 `repair_is_edge_local`.** Losing 5 % on the home→A link only, A's subscribers'
  NACKs are answered by A for what A holds. A asks the home once per missing sequence
  number, and B's publisher sees no NACK caused by A's loss. *Oracle:* RTP-12 history
  contents and the link logs.
- **K5 `stale_term_ignored`.** An old home that has not yet learned it lost its term sends a
  fence-free subscribe and a lease grant. Edges that have seen the new term refuse both, and
  the old home's grant extends no lease. *Oracle:* CAS-1.
- **K6 `link_loss_recovers`.** The home→A link is cut for 5 s and then returns. A suspends the
  room within its lease, then reattaches. On the new link the trace shows the fence table,
  then A's acknowledgement, then the first `LeaseGrant`, in that order; a fence set during
  the cut holds on A before A forwards any media of the room. Its subscribers resume at the
  next keyframe. A room stream flooded and then sent a malformed message detaches only its
  room, while a fence for another room on the same link is acknowledged within the RTC-7
  bound. *Oracle:* CAS-2 and CAS-7.
- **K7 `congested_link_costs_its_rooms`.** Two links, one shaped to 1 Mbit/s: rooms on the
  shaped link lower layers, while rooms on the other link keep every layer and their delay.
  *Oracle:* CAS-3, from the per-link forward logs.
- **K8 `attach_cost_is_exact`.** Attaching a room charges exactly `room_cost`'s cascade term,
  and detaching returns it. An attach past `limits.rooms` is refused by name. *Oracle:* the
  MEM ledger, as W34.
- **K9 `unpinned_node_refused`.** A node presenting a certificate the directory does not list
  gets no link, whether it dials or is dialled, and opens no stream. *Oracle:* QUIC-3, with
  `Refused(Pin)`.
- **K10 `three_node_room_on_boat`** (nightly, real hosts). 3 nodes in two regions and 30
  browsers: every browser receives every other's audio and video, measured end to end. The
  mouth-to-ear p95 is at most the single-node p95 plus the inter-region RTT plus 20 ms.
  *Oracle:* audio timestamps through RTC-4, against a ping of the inter-region path.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **KS1** Link | ALPN, mutual pins, the link control stream and room streams, the media datagram header, layer announcements, the canonical table, MTU check, `sim_directory` | quic QS7, RTC-10 P3 | K9 |
| **KS2** Forwarding | attach, canonical participants, subscriptions, wanted masks, one copy per layer, SR forwarding, link congestion | KS1, quic QS7, RTC-10 P3 | K3, K7, K8 |
| **KS3** Repair | edge-local histories and upstream asks | KS2, quic QS7, RTC-10 P3 | K4 |
| **KS4** Fences and leases | CAS-4 with the directory commit, CAS-5 with the per-unit deadline, transitive grants and the ledger, terms, ordered reattach | KS2, quic QS7, RTC-10 P3 | K1, K2, K5, K6 |
| **KS5** Real hosts | K10 on Boat | KS4, quic QS8 | K10 |

No reds ride in slice KS4: laws K1, K2 and K5 are the fence's soundness. Law K10 may ride.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) has no media relay. There are no local clones of
LiveKit or mediasoup. Their designs below come from their public documentation, and are
cited as such.

| Source | Mechanism | Verdict |
| --- | --- | --- |
| LiveKit self-hosted distributed mode ([docs.livekit.io, "Distributed multi-region"](https://docs.livekit.io/home/self-hosting/distributed/)) | "a room must fit on a single node"; other nodes only proxy signaling to the room's node | **NOT TAKEN**: here a room spans nodes |
| LiveKit | a shared Redis as the room directory | **REPLACED**: the directory must be linearizable per room and carry fences (CAS-1), which a best-effort cache does not give |
| mediasoup `PipeTransport` and `pipeToRouter` ([mediasoup.org documentation](https://mediasoup.org/documentation/v3/mediasoup/api/#PipeTransport)) | plain RTP between routers, one pipe per pair, consumers piped one by one | **ADAPTED**: one QUIC link per pair, with layers multiplexed in datagrams; QUIC replaces bare RTP so links are authenticated and congestion-controlled |
| moq-rs cluster (/tmp/ref-moq at `d791d54`, rs/moq-relay/src/cluster.rs, drafts/draft-lcurley-moq-cluster.md) | relay mesh with HOP_PATH loop detection and ROUTE_COST | **NOT TAKEN** in Wave 2: a star per room has no loops to detect (O3) |
| Chubby and etcd leader leases | a lease is counted from the moment of the request by the holder and from the grant by the grantor, with a bound on clock drift | **ADOPTED** in CAS-5 |
| research-1124 PLAN row C7 | inter-SFU relay over QUIC, datagrams for media, a reliable stream for control, one copy per layer per node, edge-local RTX, a cross-node fence | **ADOPTED**; this chapter gives the fence its partition rule |

## Design review

Rulings of the media Wave 2 design review (VERDICT-MEDIA-W2, 2026-11-26), applied above:

- **O1 (leases with a drift bound, or `Err(Unreachable)`).** Keep leases, hardened: the
  deadline is checked by every media unit where media leaves, with `guard_ms` (CAS-5);
  transitive grants are shrunk; the fence is committed to the directory before `Ok` (CAS-4
  step 0); the grant ledger outlives the link (CAS-5); reattach applies the fence table
  before the first grant (CAS-7). `lease_ms` stays per link, not per room.
- **O2 (the room directory).** A linearizable Hirsel Store cell with compare-and-set on
  `term`, behind the `RoomDirectory` contract of CAS-1; `sim_directory` for KS1 to KS5, the
  Store adapter for production.
- **O3 (star or tree).** Star first.
- **O4 (media without SRTP between nodes).** Plaintext RTP over mutually authenticated QUIC,
  with the canonical extension and payload-type table (CAS-2).
- **O5 (the 1,400-byte link MTU floor).** Kept.
- **O6 (admission at an edge).** The home owns the canonical `(participant, epoch)`; the
  edge's RTC-8 adapter admits with the id the home assigned (CAS-2).

Not ruled by the review; the standing choice is kept as written:

- **O6's second half (how `Cascade` attaches to the pool).** As `Pool.cascade`, run on the
  pool's units (CAS-2), not as a second service; this is what replaces RTC-10's undefined
  `RelayLink`.
