# SFU cascade

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Media Wave 2 package 4 of 4). Not built.
Rules CAS-1 to CAS-8, laws K1 to K10, slices K1 to K5. Goes to design review (astra) before
any build; CAS-5's partition rule is the first item of that review.*

`sfu_cascade` lets one room span several SFU nodes. Participants join the node nearest
them; the nodes relay each published layer between them over [QUIC](quic.md). It lives in
`core/programs/webrtc_sfu/cascade` <!-- doc-ref: planned --> as part of the SFU, because
its state is the router's: down-tracks, layers, fences. It extends [RTC-10](webrtc.md)'s
pooled service; a node that cascades no room behaves exactly as RTC-10 says.

**Dependency: the room directory.** Which node is a room's home, and which nodes it spans, is
decided by a multi-node room directory that does not exist yet. This chapter fixes only the
contract it needs (CAS-1) and is not buildable past slice K1 until the directory is
specified. The directory is open item O2.

## CAS-1: Topology and the directory contract

- **Star per room.** Each room has one **home** node and any number of **edge** nodes. Every
  edge links to the home; edges never link to each other for that room. The home forwards a
  layer from one edge to another. Two hops at most, no loops, and one place where a fence
  starts. A tree or mesh is open item O3.
- **One link per node pair.** A link is one QUIC connection between two nodes, shared by
  every room the pair has in common. Rooms are multiplexed on it by a link-local room
  number assigned by the home; link numbers, like RTC-10's `RoomId`, are never reused on a
  link.
- **The directory contract.** For each room the directory gives `(home: NodeId, term: u64)`,
  where `term` rises each time the home changes, and for each node its address and its
  `quic.Pin`. A node acts as home only for a term the directory gave it, and every
  link message about a room carries the room's term; an edge refuses any message with a term
  lower than the highest it has seen for that room. So two homes in different terms cannot
  both drive an edge. The directory must be linearizable per room; how it gets there
  (Raft, a single coordinator, a store `Cell`) is the directory's spec.

## CAS-2: Links

```hirsel unbuilt
pub type NodeId = NodeId(Inline[u8, 16])
pub type LinkLimits { rooms: u32, layers: u32, link_bytes: u64, lease_ms: u32, drift_ppm: u32 }
pub type Cascade { node: NodeId, identity: quic.Identity, directory: Directory, limits: LinkLimits }
```

A link is mutually authenticated: each side presents its node certificate and pins the
other's SPKI digest from the directory (QUIC-3, mutual). An unknown peer is refused at the
handshake. A link uses ALPN `hirsel-cascade-1`.

Each link carries:

- **One reliable control stream** each way, ordered: link hello, room attach and detach,
  leases (CAS-5), subscriptions to layers, layer masks and switch requests, scores and
  active-speaker levels, sender reports, fences (CAS-4), acknowledgements. Each message
  carries `(room number, term, seq)`; `seq` is per room and per direction, so a room's
  messages are applied in order and an acknowledgement names what it covers.
- **QUIC datagrams for media.** Each datagram is one RTP packet as the router holds it after
  SRTP removal: the plaintext `Shared[Bytes]` of RTC-5, behind an 8-byte header of room
  number and layer number. QUIC encrypts the link; SRTP is not reapplied between nodes. A
  room with SFrame (end-to-end) carries SFrame ciphertext as its payload, which the nodes
  cannot read, exactly as on one node.
- **Link MTU.** A link carries media only after DPLPMTUD has validated a path MTU that fits
  the largest RTP packet the room admits plus headers (1,400 bytes of datagram payload by
  default). Until then, or if the path cannot, the link is refused for media and reported:
  splitting an RTP packet over datagrams or falling back to a stream would add latency or
  head-of-line blocking that the room's clients never agreed to.

## CAS-3: Forwarding

- **One copy per layer per node.** A node subscribes upstream to a layer when its first local
  down-track needs it and ends the subscription when its last one leaves, as RTC-6's
  dynacast already decides per publication. The home sends each layer once over each link
  that subscribed it, and an edge fans it out locally through its own router.
- **Layer choice.** Each edge reports the highest layer any local subscriber wants and can
  take (its own RTC-6 per-subscriber choice, folded to a maximum per publication) as a mask
  on the control stream. The home forwards the layers in the masks and no others. A remote
  publisher's layers are switched at keyframes as local ones are, by the node where the
  subscriber is.
- **Repair is edge-local.** Each node keeps its own RTP-12 histories for the layers it
  receives, and answers its local subscribers' NACKs from them. A packet a node lacks is
  asked of its upstream node once per sequence number over the control stream; the upstream
  answers from its own history with a datagram, or not at all. A NACK never crosses two
  links: the home answers from its own history. PLI and FIR from an edge go to the home,
  which rate-limits them per publication as RTC-6 does for local subscribers.
- **Congestion.** The link's QUIC controller (QUIC-6) bounds the link. When a link's datagrams
  are dropped for congestion (counted by `quic`), the sending node lowers the layers it
  forwards over that link, highest first, as RTC-6's bandwidth estimate does for one
  subscriber. A congested link costs the rooms on that link, not other links.
- **Sender reports.** The home forwards each publication's RTCP SR timing over the control
  stream so edge nodes rewrite timestamps the same way the home does (RTC-4), and
  subscribers on any node see one consistent clock per publication.

## CAS-4: The cross-node fence

`revoke(room, participant, epoch)` is called on the home. It returns only when no node of the
room will route that participant's media at a fenced epoch, the same guarantee as RTC-7 but
across nodes:

1. The home applies RTC-7 on its own units.
2. It sends `Fence(participant, epoch, seq)` on the control stream of every link the room
   uses, after its own fence holds. Every link: not only those it believes involved,
   for the reason RTC-7 gives for units.
3. An edge applies RTC-7 locally, with one addition: datagrams from the link whose layer
   belongs to the participant are dropped when taken, like an inter-unit message in RTC-7
   step 4. It then acknowledges `seq`.
4. The home returns `Ok(())` when every link has acknowledged, or when CAS-5 makes an
   unacknowledged edge safe.

Media of the participant already in a link's datagrams before an edge's fence point may still
arrive at the edge, and the edge drops it, because the edge's fence is set before its
acknowledgement. Suspend and resume cross nodes the same way.

A fence from the home is applied by an edge whatever its other control traffic: a fence is
never behind a subscription change in the queue, as RTC-10's control order already gives
fences their own slots.

## CAS-5: Partitions fail closed

A fence must not wait forever for an edge it cannot reach, and it must not return while that
edge might still be forwarding. Leases resolve both.

- **Leases.** An edge may forward media of a room (to its local subscribers and up the link)
  only while it holds a lease from the home. It asks for renewal every `lease_ms / 3`. It
  takes the lease's start to be the moment it *sent* the request, by its own monotonic clock,
  and it holds the lease until that start plus `lease_ms`. The home grants by reply, and
  counts the lease from the moment it *sent the grant*, by its own clock.
- **Expiry.** When an edge's lease ends without renewal, the edge applies RTC-7 suspend to the
  room on its own units. It forwards nothing of the room, in either direction, until a new
  lease. Local ICE and DTLS stay up, as suspend keeps them, so recovery needs no new
  handshakes.
- **The fence across a partition.** If an edge has not acknowledged a fence within `lease_ms`,
  the home stops renewing its lease for that room and closes the room on that link. The home
  then waits until `lease_ms × (1 + 2·drift_ppm/10⁶)` has passed since the last grant it
  sent that edge, and returns `Ok(())`. Each request precedes its grant in real time. So the
  edge's lease, counted from its request, ends no later than the home's count from its grant,
  even when the two clocks run at different rates within `drift_ppm`. By the time the home
  returns, the edge has suspended the room or is unable to receive any more media for it.
- **What the caller sees.** `revoke` returns `Ok(())` in both cases, because both are the
  guarantee. The router also emits `Event.Severed(room, node)` for each edge it dropped, and
  RTC-8's adapter turns that into a room message, so the application can move those
  participants.
- **Bound.** A fence's latency across nodes is at most the slowest edge's RTC-7 bound plus
  one link round trip when every edge answers, and at most `lease_ms` × (1 + 2·drift) plus
  RTC-7's local bound when one cannot. The default `lease_ms` is 2,000 ms.
- **Clock assumption.** The only assumption is that every node's monotonic clock rate stays
  within `drift_ppm` (default 200 ppm) of real time. There is no assumption about wall-clock
  agreement. A node whose clock runs faster than that breaks the guarantee. This is the same
  assumption leases make in Chubby and in etcd's leader lease. O1 asks whether it is
  acceptable.

## CAS-6: Memory and bounds

A link is charged at attach: per room, its layer table, its sequence windows and its RTP-12
histories for the layers it may receive, from the room's `RoomLimits` (RTC-10's
`room_cost` gains a `cascade` term per link). The link itself is charged QUIC-9's cost.
`limits.rooms` and `limits.layers` bound what one peer node can ask for, and an attach past
either is refused by name, as RTC-10 refuses an Open. Datagrams a node cannot route at once
are dropped and counted, never queued, as RTC-5's ingress does.

## CAS-7: Faults

- A link's loss (idle timeout, reset, a QUIC error) ends its rooms' attachments on both sides.
  The edge's leases run out (CAS-5) and it suspends those rooms. When the link returns it
  reattaches under new link-local room numbers, takes fresh leases and resubscribes, and the
  home resends masks, SRs and every fence the room has, since an edge cannot know what it
  missed.
- A malformed control message closes the room on that link and leaves the link's other
  rooms running. A malformed link-level message closes the link.
- A home change (a new term in the directory) ends every lease of the old term. Edges that
  see the new term detach from the old home and attach to the new one. RTC-7 fences live on
  the home that set them, so the new home must receive the room's fence table from the
  directory before it grants a lease. The directory contract (CAS-1) carries the fence table
  with the term; O2 asks for that.

## CAS-8: Not in Wave 2

- Edge-to-edge links, trees and meshes (O3).
- Cascading MoQ relays; MoQ has its own relay ([MoQ](moq.md) MOQ-6, its O3).
- Recording on an edge; recording stays on the home (RTC-9).
- Moving a participant between nodes mid-session: they rejoin with a higher epoch on the
  other node, as RTC-8 rejoins.

## Laws

Red first in `core/programs/webrtc_sfu/cascade/laws.hirsel` <!-- doc-ref: planned -->,
under `sim.udp` with three simulated nodes unless named.

- **K1 `partition_fence_fails_closed`.** A home and two edges; the link to edge B is cut, and
  then `revoke` is called for a participant on edge B who is sending to subscribers on B. The
  trace must show two things: `revoke` returns within CAS-5's bound, and from the moment it
  returns, B's units send no datagram carrying that participant's media to anyone. Repeat
  with B's clock running fast and slow at `drift_ppm`. *Oracle:* CAS-5's inequality, checked
  over the trace's real time.
- **K2 `fence_reaches_every_node`.** With all links up, a revoke returns after both edges
  acknowledge. After it returns, no node sends the participant's media, and a rejoin at the
  same epoch on any node is `Fenced`. *Oracle:* RTC-7 applied across nodes.
- **K3 `one_copy_per_layer_per_node`.** 50 subscribers on edge A to one publisher on edge B:
  each forwarded packet crosses B→home once and home→A once. A second publisher's layers
  cross only to nodes with a subscriber. *Oracle:* link datagram counts against the
  subscription table.
- **K4 `repair_is_edge_local`.** Losing 5 % on the home→A link only, A's subscribers'
  NACKs are answered by A for what A holds. A asks the home once per missing sequence
  number, and B's publisher sees no NACK caused by A's loss. *Oracle:* RTP-12 history
  contents and the link logs.
- **K5 `stale_term_ignored`.** An old home that has not yet learned it lost its term sends a
  fence-free subscribe and a lease grant. Edges that have seen the new term refuse both, and
  the old home's grant extends no lease. *Oracle:* CAS-1.
- **K6 `link_loss_recovers`.** The home→A link is cut for 5 s and then returns. A suspends the
  room within its lease, then reattaches. Its subscribers resume at the next keyframe, and a
  fence set during the cut holds on A after reattach. *Oracle:* CAS-7.
- **K7 `congested_link_costs_its_rooms`.** Two links, one shaped to 1 Mbit/s: rooms on the
  shaped link lower layers, while rooms on the other link keep every layer and their delay.
  *Oracle:* CAS-3, from the per-link forward logs.
- **K8 `attach_cost_is_exact`.** Attaching a room charges exactly `room_cost`'s cascade term,
  and detaching returns it. An attach past `limits.rooms` is refused by name. *Oracle:* the
  MEM ledger, as W34.
- **K9 `unpinned_node_refused`.** A node presenting a certificate whose SPKI the directory does
  not list gets no link. *Oracle:* QUIC-3, with `Refused(Pin)`.
- **K10 `three_node_room_on_boat`** (nightly, real hosts). 3 nodes in two regions and 30
  browsers: every browser receives every other's audio and video, measured end to end. The
  mouth-to-ear p95 is at most the single-node p95 plus the inter-region RTT plus 20 ms.
  *Oracle:* audio timestamps through RTC-4, against a ping of the inter-region path.

## Build slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **K1** Link | ALPN, mutual pins, control stream codec, media datagram header, MTU check | quic Q7 | K9 |
| **K2** Forwarding | attach, subscriptions, masks, one copy per layer, SR forwarding, link congestion | K1, RTC-10 P3, the room directory | K3, K7, K8 |
| **K3** Repair | edge-local histories and upstream asks | K2 | K4 |
| **K4** Fences and leases | CAS-4, CAS-5, terms, fault recovery | K2 | K1, K2, K5, K6 |
| **K5** Real hosts | K10 on Boat | K4, quic Q8 | K10 |

No reds ride in K4: K1, K2 and K5 are the fence's soundness. K10 may ride.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) has no media relay. There are no local clones of
LiveKit or mediasoup. Their designs below come from their public documentation, and are
cited as such.

| Source | Mechanism | Verdict |
| --- | --- | --- |
| LiveKit distributed rooms ([docs.livekit.io, "Distributed multi-region"](https://docs.livekit.io/home/self-hosting/distributed/)) | a room spans nodes, each participant on its nearest node, media relayed between nodes | **ADOPTED** as the shape |
| LiveKit | a shared Redis as the room directory | **REPLACED**: the directory must be linearizable per room and carry fences (CAS-1, O2), which a best-effort cache does not give |
| mediasoup `PipeTransport` ([mediasoup.org documentation](https://mediasoup.org/documentation/v3/mediasoup/api/#PipeTransport)) | plain RTP between routers, one pipe per pair, consumers piped one by one | **ADAPTED**: one QUIC link per pair, with layers multiplexed in datagrams; QUIC replaces bare RTP so links are authenticated and congestion-controlled |
| moq-rs cluster (/tmp/ref-moq at `d791d54`, rs/moq-relay/src/cluster.rs, drafts/draft-lcurley-moq-cluster.md) | relay mesh with HOP_PATH loop detection and ROUTE_COST | **NOT TAKEN** in Wave 2: a star per room has no loops to detect (O3) |
| Chubby and etcd leader leases | a lease is counted from the moment of the request by the holder and from the grant by the grantor, with a bound on clock drift | **ADOPTED** in CAS-5 |
| research-1124 PLAN row C7 | inter-SFU relay over QUIC, datagrams for media, a reliable stream for control, one copy per layer per node, edge-local RTX, a cross-node fence | **ADOPTED**; this chapter gives the fence its partition rule |

## Open for design review

- **O1** CAS-5's partition rule (first for astra). The question is whether a lease with a
  drift bound is the right fail-closed mechanism. The alternative is for `revoke` to return
  `Err(Unreachable(nodes))` and keep the fence pending; that is honest, but it leaves the
  caller holding a participant who may still be heard. Does `Ok` plus `Severed` mislead
  callers? Is a 200 ppm bound safe on the VMs we run? Should `lease_ms` be per room?
- **O2** The room directory. Who specifies and builds it, and is a store `Cell` with
  linearizable compare-and-set enough? It must also hand the fence table to a new home.
- **O3** Star or tree. A star gives each edge one hop to the home and back, two hops between
  edges. A large cross-region room may want region hubs (a tree), and that brings back loops
  and multi-hop fences.
- **O4** Media without SRTP between nodes. QUIC's encryption covers the link, but the
  plaintext RTP sits in each node's memory, as it does on one node today. Is that acceptable
  for rooms without SFrame, or should the link carry the publisher's SRTP unchanged and each
  node re-key? Re-keying costs a decrypt per node.
- **O5** The MTU floor of 1,400 bytes on links. It is fine inside clouds. Is it fine on every
  path we expect between regions?
