# WebRTC sessions and the SFU router

Version note (2026-11-24, media-webrtc-sfu-spec-2319): first text of RTC-1 to RTC-8, Wave 1.6
of the real-time media track. Not yet built; the fences are `unbuilt`.
Amended 2026-11-24 (webrtc-spec-amend-2337) for the 15 findings of design review 2333: session
generations on every queue and an all-writer fence acknowledgement (RTC-7), a proven roster
across signaling loss (RTC-8, runner slice R1), per-down-track sender reports and wire
identity (RTC-4, RTC-5), decodable switching with stable target maps (RTC-6), JSEP
`bundle-only` and `tls-id` (RTC-1, RTC-2), prefix demultiplexing (RTC-3), per-direction RID
and simulcast (RTC-1, RTC-2), negotiated keyframe requests, bandwidth pause, RFC 6464 `vad`
per leg, SSRC collision recovery and bounded fence state. JSEP is
[RFC 9429](https://www.rfc-editor.org/rfc/rfc9429), which obsoletes RFC 8829. BUNDLE is
[RFC 9143](https://www.rfc-editor.org/rfc/rfc9143), which obsoletes RFC 8843. Section
numbers cited as RFC 8843 are that text's, as the review read it. The package is
`webrtc_sfu`, pure Hirsel under core/programs, with no `Foreign` code in its reach, so
FFI-8 ([foreign](foreign.md)) replay and `hirsel test --sim` cover it end to end.

This chapter specifies three layers, each sans-IO:

- **SDP** (RTC-1, RTC-2): a minimal JSEP subset. One BUNDLE group, `rtcp-mux`, ICE-Lite on the
  server, DTLS-SRTP, MID and RID, simulcast and SVC.
- **PeerSession** (RTC-3, RTC-4): one remote endpoint's media transport. It joins the Wave 1
  libraries: `stun_ice` (ICE-Lite), `dtls` (DTLS 1.2 and the `use_srtp` export), `srtp`,
  `rtp` (headers, extensions, RTCP, the RTX ring, congestion control and the pacer) and
  `media_bits` (codec payload descriptors).
- **Router** (RTC-5, RTC-6, RTC-7): rooms of participants. It covers fan-out, generic
  dynacast, active-speaker forwarding and the synchronous revocation fence
  `revoke(room, participant, epoch)`.

Signaling runs over the generic `runner.room` (RTC-8) through a sans-IO adapter. The section
ends with the [build laws](#webrtc-build-laws), the [cost targets](#cost-targets), the
[build slices](#build-slices) and [prior art](#prior-art).

Every name here is a general media-routing primitive. A room is a set of participants that
publish and subscribe to tracks. Nothing in this package knows what an application calls its
rooms, who its users are, or why a subscriber picks a layer.

**What the sibling specifications own.** This chapter uses their operations and does not
specify them again:

| Owner | Owns | Used here as |
| --- | --- | --- |
| [TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement) | key shares, the DTLS 1.2 and SRTP schedules, sealers, openers, `p256_key`, `sign`, `std.x509_build` | the only key material; sender ownership fixes RTC-4's SSRC rules |
| [NET-15 to NET-17](net.md#udp) | `UdpSocket`, `udp_recv`, `udp_send`, `UdpTx`, the `UdpBind` and `UdpSend` rules | the PeerSession's datagrams |
| `stun_ice` (media-stun-ice-2203, built) | STUN Binding, ICE-Lite `lite`, `lite_receive`, `lite_selected` | the server's ICE agent |
| media-dtls-spec-2317 (DTLS-1 to DTLS-9, SRTP-1 onward; drafted) | the DTLS 1.2 server handshake, flights and retransmission, the record layer, the `use_srtp` export; the SRTP and SRTCP contexts, ROC estimation and the replay window | the DTLS endpoint and the SRTP contexts |
| media-rtp-spec-2318 (RTP-1 to RTP-12, the `rtp` package) | RTP header and extension parsing and writing, RTCP (SR, RR, SDES, BYE, NACK, PLI, FIR, TWCC), the retransmission `History` with RTX, delay-based BWE, the pacer, per-down-track sequence and timestamp rewriting (`Rewrite`, answering `Forward`, `Late`, `Drop` or `Wait`), and the synchronous `close` operations of RTP-12 | every byte-level RTP and RTCP operation |
| `media_bits` (built; VP8 added by slice S2 here) | Opus TOC, H.264 NAL and FU-A, VP9 descriptor, AV1 Dependency Descriptor, keyframe detection | layer identity and switch points |

Where a sibling's final names differ from the names used here, the sibling's names win, and
the build lane renames on its side without changing behaviour.

### Contract traceability

Each requirement of the Wave 1 consumer contract
(research-1124/media-wave1/CONSUMER-CONTRACT.md in the briefs repository) that falls to this
package, and where it is met. Items owned by the dtls and rtp specifications are listed only
where this package must use them in a particular way.

| Requirement | Met by |
| --- | --- |
| A standalone, ephemeral media service with no store mount | RTC-5: all state is in units' memory, fixed per room at `open_room`, and gone at `close_room`; nothing is durable |
| SDP certificate fingerprint from `x509_build`, negotiated DTLS roles | RTC-3 DTLS (one certificate per unit, `expect` pinning); RTC-1 and RTC-2 (`setup:passive` in answers, `actpass` in server offers, `tls-id` names the association, roles never change) |
| Independent SRTP in and out keys, SSRC, ROC, replay window | dtls.md SRTP-2 and SRTP-3; RTC-3 authenticates before state (W19); RTC-4 keeps one SSRC per down-track and recovers from a collision |
| BUNDLE with RTP/RTCP demux | RTC-1 BUNDLE rules, RTC-3 prefix `classify`, then authenticated MID, SSRC and unique payload-type binding (W3, W4) |
| Negotiated MID, RID, extmap, payload types; Opus, VP8 and H.264 passed through, no server decode | RTC-1 and RTC-2 (offer's payload types and IDs kept, RIDs per direction, W1, W2); RTC-5 writes each subscriber's own PT, MID and extension IDs and forwards payloads as bytes (W8); VP8 descriptor is slice S2 |
| SR/RR, NACK/RTX, PLI/FIR coalescing, TWCC, pacing | rtp RTP-4 to RTP-10; RTC-4 (per-down-track SRs, NACK answered on RTX only from the egress history), RTC-5 (RTCP terminated, TWCC per outbound transport), RTC-6 negotiated keyframe requests (W6, W8, W10) |
| Per-subscriber sequence and timestamp continuity | rtp RTP-11 `Rewrite` per down-track; RTC-4 (one SSRC while collision-free, W7); RTC-6's switch timeline |
| Keyframe-safe simulcast and SVC switching | RTC-6 mapping onto RTP-11 (W9) |
| RFC 6464 audio level forwarded, for active speakers | RTC-6 active speakers (W12) and audio forwarding with the level bits kept and `V` per leg (W13) |
| Generic dynacast: per-subscriber `Off` or a layer per video publication; unselected publisher layers paused | RTC-6 `Target`, the cross-unit wanted mask and `Event.Layers` (W11) |
| `revoke(room_id, participant_id, epoch)`, one fence per participant | RTC-7, `revoke(r, room, participant, epoch)`, with fence capacity reserved at admission |
| Synchronously stops ingress and egress, cancels queued RTX, invalidates DTLS and ICE | RTC-7 steps 1 to 5, by session-generation tags on every queue (W14, W15) |
| Ack only once nothing more can be forwarded for the revoked epoch | RTC-7 acknowledgement: after every unit has applied the fence and every writer has handed off or cancelled its work, with both endpoints rechecked before any later send or retry (W14) |
| TURN over TLS, or ICE-TCP, for strict firewalls | TURN over a stream with UDP relays, [turn.md](turn.md) TURN-2; ICE-TCP is withdrawn (S8, W17) |
| runner.room: unicast, membership revision, targeted kick, expiry, SDK exposure | RTC-8's table: on main. The roster incarnation, revision and snapshot (runner slice R1, 2ec0de5b), the incarnation and revision in the SDK's room API (`LiveRoom.incarnation` and `revision`, F2) and the Hirsel room client (`room_client`, F1) are on main |
| Idle lease (contract §4) | not this package's: hirsel4-rooms-2301 reports it. The service holds a room membership for each room it routes, so it is affected only if that membership keeps a Place alive |

## Session shape

**One transport per participant.** A participant holds one PeerSession: one ICE-Lite agent,
one DTLS association and one SRTP schedule, with every m-section bundled on it. The session
carries both what the participant publishes and what it receives. Subscriptions are added by
renegotiation on the same session, so a participant pays one handshake and one 5-tuple.

**The server is ICE-Lite and DTLS passive.** The server has a public address and never sends
the first packet. It answers every DTLS setup offer with `a=setup:passive`, so the remote end
is always the DTLS client. One UDP port per media unit (RTC-5) is the only candidate.

**Units.** A media service runs `u` media units ([CONC-15](tasks.md#conc-15-units)), each with one
`udp_bind` socket on its own port. A participant's **home unit** is chosen when its first
offer is accepted, and its session never moves. Port-per-unit makes the home unit a
signaling decision, so no kernel steering is needed and no packet arrives at a unit that does
not own its session.

## RTC-1: SDP descriptions

```hirsel unbuilt
pub type Media = Audio | Video | Application
pub type Direction = SendRecv | SendOnly | RecvOnly | Inactive
pub type Setup = Active | Passive | ActPass
pub type Codec = Opus | Vp8 | Vp9 | H264 | Av1 | Rtx | Red | Ulpfec
pub type Ext = Mid | Rid | RepairedRid | AudioLevel | AbsSendTime | TransportCc | DependencyDescriptor
pub type Format = Format(pt: u8, codec: Codec, clock: u32, channels: u8, fmtp: Bytes, apt: Option[u8], nack: bool, pli: bool, fir: bool, twcc: bool)
pub type Vad = VadOn | VadOff
pub type Extmap = Extmap(id: u8, ext: Ext, dir: Option[Direction], vad: Option[Vad])
pub type RidDir = RidSend | RidRecv
pub type Rid = Rid(id: Inline[u8, 16], len: u8, dir: RidDir, pts: List[u8], depend: List[u8], restrictions: Bytes)
pub type Stream = Stream(alternatives: List[u8], paused: List[bool])
pub type Simulcast = Simulcast(send: List[Stream], recv: List[Stream])
pub type Port = Rejected | BundleOnly | Port(u16)
pub type Section = Section(media: Media, port: Port, mid: Inline[u8, 16], mid_len: u8, dir: Direction, formats: List[Format], extmaps: List[Extmap], rids: List[Rid], simulcast: Simulcast, ssrcs: List[u32], rtx_pairs: List[(u32, u32)], msid: Bytes, rtcp_mux: bool)
pub type Description = Description(origin_session: u64, origin_version: u64, bundle: List[u8], ice_ufrag: Bytes, ice_pwd: Bytes, ice_lite: bool, fingerprint: Inline[u8, 32], setup: Setup, tls_id: Bytes, sections: List[Section], candidates: List[Bytes])
pub type SdpError = TooLarge | Malformed(u64) | NoBundle | NotBundled(u8) | NoRtcpMux(u8) | NoMid(u8) | NoFingerprint | WrongFingerprint | NoIceCredentials | ExtmapConflict(u8) | PayloadConflict(u8) | TooManySections | TooManyFormats(u8) | TooManyRids(u8) | BadRid(u8) | AnswerMismatch(u8) | AssociationChanged | Capacity(Capacity)

pub fn parse(text: Bytes, mut out: Description) -> Result[(), SdpError]
pub fn write(d: Description, mut out: Bytes) -> Result[u64, SdpError]
```

`parse` reads RFC 8866 SDP as JSEP ([RFC 9429](https://www.rfc-editor.org/rfc/rfc9429))
produces it, into a caller-owned `Description` whose lists it clears and refills, so a
session reuses one description's storage. It is a line parser over `CRLF` or bare `LF` lines,
with no backtracking.

**Bounds.** These are explicit refusals, never truncation. Each limit is accepted exactly
and refused one past it:

- `TooLarge`: the text is longer than 32,000 bytes. A room message is at most 32,768 bytes
  (`MESSAGE_MAX`, core/programs/runner/runner-room.hirsel), and RTC-8's envelope takes at
  most 768 of the rest.
- `TooManySections`: more than 64 m-sections.
- `TooManyFormats(i)`: more than 32 formats in section `i`.
- `TooManyRids(i)`: more than 4 RIDs in section `i`, counted over both directions.

**Attributes read.** `v`, `o`, `s`, `t`, `c`, `m` and these `a=` attributes: `group:BUNDLE`,
`bundle-only`, `ice-ufrag`, `ice-pwd`, `ice-lite`, `ice-options`, `fingerprint`, `setup`,
`tls-id`, `mid`, `rtcp-mux`, `rtcp-rsize`, `extmap-allow-mixed`, `extmap` (with its optional
direction and its extension attributes), `sendrecv`, `sendonly`, `recvonly`, `inactive`,
`msid`, `ssrc` (only the `cname` and `msid` forms), `ssrc-group:FID`, `rtpmap`, `fmtp`,
`rtcp-fb`, `rid`, `simulcast`, `candidate` and `end-of-candidates`. Session-level `ice-ufrag`, `ice-pwd`,
`fingerprint`, `setup` and `tls-id` apply to every section that lacks its own. `ice-lite` is
read and written only at session level. Any other attribute is skipped, as RFC 8866 §5.13
requires of an unknown attribute. An extension URI or codec name outside `Ext` or `Codec` is
skipped with its payload type or ID. The answer never accepts it, so skipping it changes no
negotiated behaviour.

**Codec names.** `opus/48000/2`, `VP8/90000`, `VP9/90000`, `H264/90000`, `AV1/90000`,
`rtx/90000`, `red/48000/2` and `ulpfec/90000`, compared ASCII case-insensitively
(RFC 8866 §6.6). The extension URIs are those of RFC 8843 (`mid`), RFC 8852 (`rtp-stream-id`,
`repaired-rtp-stream-id`), RFC 6464 (`ssrc-audio-level`), `abs-send-time`,
`transport-wide-cc-extensions-01` and the AV1 RTP specification's `dependency-descriptor`.
For `ssrc-audio-level`, the extension attribute `vad=on` or `vad=off` is kept in
`Extmap.vad`, `None` when absent ([RFC 6464](https://www.rfc-editor.org/rfc/rfc6464) §4).
An absent `vad` means `vad=on`, the RFC's default, everywhere this spec reads it.
Other extension attributes are skipped.

The negotiated extmap mode maps to `ext.Form` (RTP-3). `extmap-allow-mixed` gives `Mixed`.
Without it, any negotiated ID above 14 gives `TwoOnly`, and otherwise the form is `OneOnly`.

**Fingerprint.** Only `sha-256` is read. A description whose fingerprints are all of other
hashes is `NoFingerprint`. RFC 8122 §5 lets an endpoint offer several. Every browser offers
`sha-256`, and a weaker hash is not worth the second code path.

**Ports.** A section's port is one of three things, and they are never confused:
`Rejected` is port 0 without `a=bundle-only`, a section the other side declined or removed.
`BundleOnly` is port 0 with `a=bundle-only`, an offered section that is live on the BUNDLE
transport (RFC 9429 §§5.2.1, 5.3.1; RFC 9143 §7.3). `Port(n)` is any other port.

**The BUNDLE rules ([RFC 9143](https://www.rfc-editor.org/rfc/rfc9143)).** These are
checked at parse, because a description that breaks them cannot be answered:

- a `group:BUNDLE` line names every section that is not `Rejected` (`NoBundle` when there is
  no group, `NotBundled(i)` for a section outside it);
- the **tagged** section is the first one the group names (RFC 9143 §9.3.1). Transport
  attributes (`ice-ufrag`, `ice-pwd`, `fingerprint`, `setup`, `tls-id`, `rtcp-mux`,
  candidates) are resolved on it, and the description's top-level fields hold its values.
  `rtcp-mux` is required on the tagged section (`NoRtcpMux(i)`,
  [RFC 8858](https://www.rfc-editor.org/rfc/rfc8858)). A `BundleOnly` section carries no
  transport attributes of its own and is never refused for lacking them;
- every bundled section has a `mid` (`NoMid(i)`);
- one extension URI has one ID in every bundled section, and one ID one URI
  (`ExtmapConflict(id)`, RFC 8843 §9.2);
- one payload type is one codec configuration in every bundled section where it appears
  (`PayloadConflict(pt)`). RFC 8843 §9.1.1 requires this of an offerer that reuses a
  payload type across bundled sections, so the rule refuses only what the RFC already
  forbids. A payload type may legally appear in two sections with that one configuration.
  RTC-3 then cannot route by payload type alone, and routes by MID and SSRC binding.
- with `rtcp-mux`, no RTP section uses payload types 64 to 95
  ([RFC 5761](https://www.rfc-editor.org/rfc/rfc5761) §4), because their marker-bit form
  collides with RTCP packet types 192 to 223 in the second octet (`PayloadConflict(pt)`).

**RID and simulcast ([RFC 8851](https://www.rfc-editor.org/rfc/rfc8851),
[RFC 8853](https://www.rfc-editor.org/rfc/rfc8853)).** A RID identifier is 1 to 16 ASCII
alphanumeric, `-` or `_` bytes (`BadRid(i)` otherwise). The characters are RFC 8851 §10's
grammar, which sets no length. The 16-byte cap is this spec's bound, the length browsers
accept for `sendEncodings[].rid`. Each `a=rid` line
keeps its direction, its `pt=` list, its `depend=` list (as indexes into `rids`) and the rest
of its restrictions as bytes, so nothing the offerer said about a RID is lost (RFC 8851
§§6.2.2 to 6.3). `a=simulcast` keeps its send list and its receive list separately. Each
list is a sequence of streams. A stream is one or more alternative RIDs separated by `,`,
each with its own `~` paused marker (RFC 8853 §5.1). Both forms are legal syntax and are
parsed. What the answer does with each is RTC-2's rule. A list that names a RID absent
from `rids`, or a RID in the wrong direction, is `BadRid(i)`. So is a section with RIDs and
no `a=simulcast` line.
A simulcast RID is negotiated only when it is ASCII alphanumeric (RFC 8852 §3 wire grammar).

**`tls-id`.** The `tls-id` attribute ([RFC 8842](https://www.rfc-editor.org/rfc/rfc8842)
§5.5) is kept as bytes. It names the DTLS association. RTC-2 compares it to decide whether a
re-offer continues the running association.

**Writing.** `write` emits the attributes above in RFC 9429 §5.2 order, one per line with
`CRLF`, and returns the bytes appended. `ice-lite` goes at session level only.
`parse(write(d))` gives back `d` for every `d` that `write` accepts. That is a property law of
the writer. It is not the oracle for what the writer must say (RTC-2).

## RTC-2: Offer and answer

```hirsel unbuilt
pub type Policy = Policy(audio: List[Codec], video: List[Codec], exts: List[Ext], max_sections: u8, simulcast: bool)
pub type Negotiated = Negotiated(remote_ufrag: Bytes, remote_pwd: Bytes, remote_fingerprint: Inline[u8, 32], remote_setup: Setup, tls_id: Bytes, sections: List[Agreed])
pub type Agreed = Agreed(index: u8, mid: Inline[u8, 16], mid_len: u8, media: Media, recv: bool, send: bool, formats: List[Format], extmaps: List[Extmap], rids: List[Rid], streams: List[Stream])
pub type Signaling = Stable | HaveLocalOffer(u64) | HaveRemoteOffer(u64)

pub fn answer(policy: Policy, current: Option[Negotiated], offer: Description, local: LocalEnd, mut out: Description, mut agreed: Negotiated) -> Result[(), SdpError]
pub fn offer(policy: Policy, current: Negotiated, local: LocalEnd, adds: List[SendSection], mut out: Description) -> Result[(), SdpError]
pub fn accept_answer(current: Negotiated, sent: Description, answer: Description, mut agreed: Negotiated) -> Result[(), SdpError]
```

`LocalEnd` is the server's half: its ICE ufrag and password (fresh per PeerSession,
RTC-3), the SHA-256 fingerprint of its home unit's certificate, its `tls-id` (fresh per
PeerSession), and the home unit's one host candidate. `SendSection` is a down-track the
server adds (RTC-5).

**`answer`.** The answer mirrors the offer's sections in order. For each section:

1. A `Rejected` offered section is answered `Rejected`. `Application` (SCTP data channels)
   is answered `Rejected`. Data channels are not in Wave 1 (prior art below), and
   RFC 9429 §5.3.1 rejects a section by port 0 without `bundle-only`. A `BundleOnly`
   offered section is live and is answered like any other section. Its answer carries the
   BUNDLE transport's port, since the answerer does not use `bundle-only` (RFC 9143 §7.3).
2. The direction is the offer's mirror, restricted by what the server does: it receives on an
   offered `sendrecv` or `sendonly` section and sends on an offered `recvonly` section, or on
   one it created with `offer`. A `sendrecv` the client opens is answered `recvonly`.
3. Formats are the offer's formats whose codec the policy lists, in the offer's order, with
   their payload types, `fmtp` and `rtcp-fb` kept as offered. The answerer keeps the
   offerer's payload types (RFC 9429 §5.3.1). H.264 formats are kept only with
   `packetization-mode=1`, since FU-A and STAP-A are what `media_bits` reads. An `Rtx`
   format is kept when its `apt` format is kept. A section left with no media format is
   answered `Rejected`.
4. Extmaps are the offer's for extensions the policy lists, with the offer's IDs (RFC 8285
   §7). An extmap with a direction is answered with the complementary direction, and one
   whose direction does not overlap the section's answered direction is not kept (RFC 8285
   §7). For `AudioLevel` the answer repeats the offer's `vad` value, so each leg's `V` bit
   has the meaning that leg negotiated (RTC-6). `Mid` is required, and a section without it
   was refused at parse.
5. **RIDs and simulcast, one direction at a time** (RFC 8853 §5.3.2). The server receives
   simulcast and never sends it. So the offer's `send` list is processed and its `recv`
   list is declined: the answer carries no `send` list, and every `RidRecv` RID is left out.
   For the `send` list, each RID is kept only when the intersection of its `pt=` list with
   the formats kept in step 3 is not empty (an absent `pt=` means every kept format), and
   when it meets RTC-1's wire grammar and every RID in its `depend=` list is also kept.
   A kept RID is answered `recv` with its restrictions narrowed to that intersection and
   otherwise as offered. A RID is never broadened. Each stream of the list keeps the
   alternatives whose RID was kept, and a
   stream left with none is removed. A paused alternative (`~`) is removed from the
   answer: the server does not track a stream it was told not to expect. The answer's
   `a=simulcast:recv` lists the remaining streams in the offer's order, which is a subset
   the RFC allows. `Agreed.streams` records them, and the encoding index of RTC-6 is the
   position in that list, whichever alternative's RID a packet carries. With
   `policy.simulcast` false, or nothing left, the RID and simulcast lines are dropped and
   the section takes only its unlabelled encoding. Every removal is counted per reason, so
   none is silent.
6. Every kept section names the server's ICE credentials, `setup:passive`, the fingerprint
   and `tls-id`, and, in the tagged section, the candidate and `end-of-candidates`.
   `ice-lite` is a session-level line.

The BUNDLE group lists the MIDs of the sections that are not `Rejected`, in the offer's
order. A `Rejected` section's MID is left out of the group (RFC 9143 §7.3). `agreed` records
the result and is what RTC-3 and RTC-5 read.

**A re-offer on a running session.** When `current` is `Some`, the offer continues the
association only if its ICE ufrag and password, its fingerprint and its `tls-id` are
unchanged and its `setup` keeps the client's role (`active` or `actpass`, answered
`passive`). Anything else is `AssociationChanged`, the running session is left exactly as
it was, and the participant rejoins (RTC-7) to restart ICE or DTLS. The offer's sections
must keep the running sections' count prefix, order, MIDs and media kinds (RFC 9429
§5.8.3), or the offer is `AnswerMismatch(i)` for the first section that differs.

**`offer`.** After the first exchange the server offers to add or remove down-tracks
(RTC-5). It reuses every existing section in place (RFC 9429 §5.2.2) and appends one
`recvonly`-for-the-client section per added down-track, with a new MID drawn from the
session's counter (`"s0"`, `"s1"`, ...), the first matching format from the publication's
agreed formats, and one SSRC and one RTX SSRC (RTC-4). It marks a removed down-track's section
`inactive` and reuses it for a later down-track of the same media kind, keeping its MID.
A server offer carries `a=setup:actpass` as RFC 8842 §5.5 requires of every offer. Its ICE
credentials, fingerprint, `tls-id` and candidate are those of the current session, and none
of them changes. A server offer never restarts ICE or DTLS. A participant that needs either
rejoins (RTC-7).

**`accept_answer`.** Before it installs anything, it validates the answer against the
offer it answers (`sent`), and refuses with nothing changed:

- the same number of sections in the same order, each with the offered MID and media kind
  (`AnswerMismatch(i)`);
- the same protocol line as offered (`AnswerMismatch(i)`);
- a direction that is a valid answer to the offered one (`AnswerMismatch(i)`);
- only formats the offer listed, with the offered payload types (`AnswerMismatch(i)`);
- the remote ICE credentials, fingerprint and `tls-id` of the running association, and
  `a=setup:active` (`AssociationChanged`).

Only after all of these pass is `agreed` replaced. A refused answer leaves the running
session, its agreed sections and its `Signaling` state unchanged.

**Receive-only participants.** A first offer may contain only `recvonly` sections, or none
that send, for example from a client that only watches. It is answered like any other.
Each such section is answered `sendonly` and gets a down-track when the participant
subscribes. A section the client sends on is a **publication** only when the caller of
`join` allows it to publish (RTC-5, its `publish` argument). Otherwise it is answered `inactive`, and
packets on it are dropped and counted.

**Glare.** Both ends may offer. `Signaling` tracks the server's side, and the server never
rolls back. When a client offer arrives while the server is `HaveLocalOffer(v)`, the server
refuses it with `Signal.Glare(v)` (RTC-8). The client rolls back and answers the server's
offer first. This is the "perfect negotiation" split of
[W3C WebRTC §10.7](https://www.w3.org/TR/webrtc/#perfect-negotiation-example) with the
server as the impolite peer. One side never rolls back, so two offers in flight always settle
in one round trip. An answer whose version is not the outstanding `v` is stale and is
refused as `AnswerMismatch(0)` with nothing changed.

**Oracle for answers.** The expected answer for each law is written by hand from RFC 9429
§5.3 and the rules above, against an offer captured from a real browser. A round trip
through the writer is never the oracle.

## RTC-3: PeerSession

```hirsel unbuilt
pub opaque type PeerSession
pub type PeerLimits = PeerLimits(sections: u8, down_tracks: u16, rtx_bytes: u64, pacer_packets: u32)
pub type PeerError = Sdp(SdpError) | Fenced | Closed | Ice(stun_ice.IceError) | Dtls(DtlsError) | Srtp(SrtpError) | Rtp(RtpError) | Capacity(Capacity)
pub type Class = Stun | Dtls | Rtp | Rtcp | Drop
pub type Input = Nothing | Replied | Media(MediaIn) | Repair(MediaIn) | Control(ControlIn)
pub type Poll = Transmit | WakeAt(u64) | Idle
pub type Gen = Gen(id: u64, epoch: u64)
pub type Tag = Tag(src: Option[Gen], dst: Gen, room: RoomId)
pub type Outbox = Outbox(data: Bytes, pkts: List[UdpTx], tags: List[Tag])

pub fn classify(prefix: Bytes) -> Class
pub fn open(take local: LocalEnd, take cert: CertKey, limits: PeerLimits, agreed: Negotiated, participant: Participant) -> Result[PeerSession, PeerError]
pub fn receive(mut p: PeerSession, now: u64, from: Addr, mut slot: Bytes, mut out: Outbox) -> Result[Input, PeerError]
pub fn poll(mut p: PeerSession, now: u64, mut out: Outbox) -> Poll
pub fn renegotiated(mut p: PeerSession, agreed: Negotiated) -> Result[(), PeerError]
pub fn close(take p: PeerSession, mut out: Outbox)
```

A PeerSession is a sans-IO state machine over `(now, datagram)`. It never calls the network
or reads a clock. The home unit (RTC-5) owns it, receives into `slot`, and passes `out` to
`udp_send` as one batch. `out.data` is a TX staging buffer and each `UdpTx` names a span of
it, so one `udp_send` sends every packet a turn produced.

**Session generations on every output.** `UdpTx` (NET-16) carries no owner, so the outbox
keeps one `Tag` beside each `UdpTx`, at the same position in `out.tags`. A tag names the
destination session's generation `dst` (the subscriber's `(id, epoch)`, RTC-5) and, for
media and repair, the source publisher's generation `src`. Every other queue of this
package carries the same pair: pacer entries, the cross-unit `Media` and `RepairReply`
messages (RTC-5), and the uncommitted suffix of a partial `udp_send`. RTC-7 fences by these tags, so it never has to infer an owner from
an SSRC or an address.

**Demultiplexing ([RFC 9443](https://www.rfc-editor.org/rfc/rfc9443) §3, which updates
[RFC 7983](https://www.rfc-editor.org/rfc/rfc7983) §7).** `classify` reads a bounded prefix
of the datagram, at most its first two bytes:

| First byte | Class |
| --- | --- |
| 0 to 3 | `Stun` |
| 16 to 19 (ZRTP) | `Drop` |
| 20 to 63 | `Dtls` |
| 64 to 79 (TURN channel or QUIC) | `Drop` |
| 80 to 127 (QUIC) | `Drop` |
| 128 to 191 | `Rtp` or `Rtcp`, split by the second octet: 192 to 223 is `Rtcp` (RFC 5761 §4), any other value is `Rtp` |
| 192 to 255 (QUIC) | `Drop` |
| any other | `Drop` |

An empty datagram is `Drop`. A datagram in the 128 to 191 range with fewer than two bytes
is `Drop`, because its second octet cannot be read. The class only routes. Each reader
then checks its own minimum (12 bytes for RTP, 8 for RTCP, 20 for STUN, 13 for a DTLS
record) and drops and counts a shorter datagram. The server speaks no TURN, ZRTP or QUIC,
so those ranges are the local drop policy, counted. RTC-1 forbids RTP payload types 64 to
95 under `rtcp-mux`, which is what makes the second octet unambiguous.

**Ingress authority.** A datagram reaches a PeerSession only from its **selected path**,
except STUN. A STUN Binding request goes to `stun_ice.lite_receive`, which checks
`MESSAGE-INTEGRITY` under this session's password and the `USERNAME` against its ufrag. A
request that fails is answered with nothing and changes nothing, the selected path
included. One that passes may move the selected path (`lite_selected`, NAT rebinding). The
home unit keeps a table from remote 5-tuple to session, which only that move changes. A
DTLS, RTP or RTCP datagram from a 5-tuple outside the table reaches no session and is
dropped and counted. A source address is unauthenticated (NET-15). The ICE password is what
binds a path to the participant who received it in the answer.

**DTLS.** The session runs the dtls lane's endpoint (docs/spec/dtls.md, DTLS-2 to DTLS-9)
as `server`, the passive side `negotiate_setup` gives for the offer's `actpass` or `active`.
It passes the offer's fingerprint to `expect`, so the dtls endpoint does the pinning (RFC
8122, DTLS-7) and a mismatch fails the handshake as `Fingerprint`. The server's
`Config.srtp` lists `AeadAes128Gcm`, `AeadAes256Gcm`, `Aes128CmHmacSha1_80` in that order,
and DTLS-5's server choice takes the first the client offers. Once `Connected`, the session
calls `take_srtp` and builds one SRTP `session` (SRTP-2), which issues each SSRC's sealers at
its first packet. Media before completion is dropped and counted. The certificate is the
home unit's, made once at start with `std.x509_build.self_signed_p256` and paired by
`identity`. Each session gets its own `Endpoint`, so the per-unit certificate is shared and
no key is.

**Media in.** An `Rtp` datagram is unprotected in place in `slot` by the srtp lane's context
for its SSRC, under the replay window that context keeps. The session then parses the header
and extensions in place (rtp lane) and binds the packet to an agreed section, only after
authentication:

1. If the packet carries the MID extension and the MID names a receiving section, the
   packet's SSRC is bound to that section, or rebound when the MID differs from the SSRC's
   current binding and this packet is newer than the one that made it. Newer means a
   higher extended index on that SSRC (RFC 8843 §9.2). An older packet carrying an earlier
   MID does not rebind it. A MID that names no receiving section drops and counts the
   packet.
2. Otherwise, if the SSRC is bound, the packet goes to its bound section.
3. Otherwise, if the payload type names exactly one receiving section, the packet goes there
   and binds its SSRC. A payload type that names two or more sections, or none, drops and
   counts the packet.

The payload type is then checked against the bound section's agreed formats, and a type
outside them drops and counts the packet. The encoding is found by RID (or repaired RID)
through `Agreed.streams`, or by the RTX pairing. The session returns
`Media(MediaIn(section, encoding, header, payload_at, payload_len, audio_level, layer))`,
with views into `slot`. `layer` is the codec's layer and keyframe facts from `media_bits`.

**Repair in.** A publisher's RTX packet is authenticated the same way, under the RTX SSRC's
own SRTP context. Its payload type must be an agreed `Rtx` format, whose `apt` names the
media format. The media stream it repairs is found by the `ssrc-group:FID` pairing, or by
the `repaired-rtp-stream-id` extension's RID ([RFC 8852](https://www.rfc-editor.org/rfc/rfc8852)
§3) mapped to its encoding. `rtx.unwrap` (RTP-7) reads the OSN, and the OSN is extended
against the **media** stream's highest index. A repair whose OSN extends to `None`, or to an
index the media stream has already received, is dropped and counted, so a duplicate never
reaches routing. Otherwise it returns `Repair(MediaIn(...))` with the media stream's
section, encoding, extended index and the original payload's view, and routing treats it
as that media packet arriving late.

**RTCP** is unprotected the same way. The session walks it with `rtcp.next(pkt, mut at)`
after `rtcp.check(pkt, mode)` accepts the datagram, and its feedback returns as `Control`.

**Authentication comes before state.** Before SRTP's `open` succeeds, the session may run
only the rtp lane's stateless readers (`header.parse`, `ext.find`, `rtcp.check`, `rtcp.next`
and the value readers), because SRTP needs them to find the header and estimate the index.
Every stateful operation (statistics, loss tracking, NACK, TWCC, `History`, `Rewrite`, BWE, the
active-speaker score of RTC-6, and the SSRC-to-MID and SSRC-to-RID bindings above) is
called only with the extended index that `open` authenticated, and only after it succeeded.
A packet that fails `open`, or fails the replay window, changes no state anywhere in the
stack except a drop counter. The 5-tuple table is the one exception with its own gate: only a
STUN request that passed `MESSAGE-INTEGRITY` under the session's ICE password changes it.
The rtp lane states this rule (its Boundaries section) and leaves its law here (W19).

**Time.** `poll` drives every timer of the stack: DTLS retransmission, ICE consent (as
`stun_ice` keeps it), RTCP report intervals, TWCC feedback, NACK generation, the pacer and
keyframe-request throttles. It returns the earliest deadline as `WakeAt`. The home unit
sleeps to the minimum over its sessions with one timer.

**Lifetimes and fences.** `close` erases the session: the ICE credentials stop matching, the
DTLS state, every sealer and opener drop (TLS-8 erases them), pacer entries and the RTX ring's
down-track entries are freed, and the 5-tuple rows go. A closed session produces no datagram,
except that `close` may append a DTLS `close_notify` alert to `out`. RTC-7 sends that alert
only for an ordinary leave, never for a revoke.

## RTC-4: Outbound SSRCs, sequence numbers and repair

TLS-8 fixes three rules for every sealer. These rules decide the egress design:

1. A domain (here, the SSRC) is issued at most once per send epoch, so a sealer cannot be
   re-made for an SSRC already used in the session.
2. A sealer refuses any counter not above every counter it has sealed. For SRTP the counter
   is the 48-bit packet index, `ROC · 2^16 + SEQ`.
3. A packet is never sealed twice. A resend repeats the protected bytes.

So:

- **One SSRC per down-track while the session has no collision.** When a down-track is
  created (RTC-5), the session draws its media SSRC and, for video, its RTX SSRC from a
  per-session generator. The generator never repeats a value in the session and never
  returns an SSRC that the remote end has used so far, and it issues one sealer for each.
  Switching layers, going `Off` and back to a layer, and rebinding the down-track to another
  publication all keep the SSRC and its sealer. The rtp lane's rewriting keeps the sequence
  number and timestamp continuous across each, so the index keeps rising. A participant
  that rejoins after a revoke gets a new session with a new DTLS association, so the epoch
  is new and every SSRC is free again.
- **SSRC collision ([RFC 3550](https://www.rfc-editor.org/rfc/rfc3550) §8.2).** The
  generator cannot predict an SSRC the remote end first uses later. A collision is
  confirmed when an authenticated packet or RTCP report from the remote end uses an SSRC
  that this session sends on. The session then retires the affected local SSRC: it sends
  an RTCP BYE for it, calls `end_send` on it (SRTP-3, so its sealers drop and the SSRC
  stays marked used), draws a fresh SSRC (and RTX SSRC, if the colliding one was either)
  from the generator, issues new sealers for them, makes a fresh `Rewrite` for the
  down-track at a new random first index, asks the publisher for a keyframe (RTC-6), and
  re-offers the section with the new `a=ssrc` and `a=ssrc-group:FID` lines (RTC-2). If the
  re-offer is refused, the session is closed and the participant renegotiates by rejoining.
  A retired SSRC is never drawn again in the session, so its sealer is never reissued. The
  RTX SSRC follows its media SSRC in this, as RFC 4588 §4 pairs them.
- **Late packets go out as repair, never on the media SSRC.** The rewriter assigns outbound
  sequence numbers in source order and leaves a gap for a source packet that has not arrived.
  When that packet then arrives late, its outbound number is below the sealer's highest, and
  the rtp lane's `Rewrite.on_packet` answers `Late(index, ts)` (RTP-11). If RTX was
  negotiated for the down-track's format, it is sent at once on the down-track's RTX SSRC as
  an RFC 4588 retransmission: the low 16 bits of `index` as its OSN, the mapped timestamp
  and marker, the RTX payload type, and the RTX stream's next sequence number, sealed under
  the RTX sealer. Otherwise it is dropped and counted. A late packet is never given a new
  media sequence number. Audio sections never negotiate RTX, and Opus concealment covers
  the loss.
- **NACK repair is RTX only, from a per-down-track history.** Each video down-track keeps an
  egress `History` (RTP-7) keyed by its extended outbound index. Each entry holds the source
  generation (RTC-3 `Gen`), a handle to the plaintext in the publisher's ingress ring, and
  the **original output rewrite** of that packet: its outbound sequence number, timestamp,
  marker, payload type, MID value, extension IDs and the rewritten codec descriptor
  counters. A NACK from a subscriber is looked up there. A hit whose source generation is
  still current and whose plaintext is still in the ingress ring is rebuilt from the
  recorded rewrite, never re-derived from the current state, and sent on the RTX SSRC with
  the recorded sequence number as OSN and a fresh transport sequence number. The history is
  gated at every source switch (`History.gate` with `Rewrite.switch_index`), so a NACK for a
  packet from before a switch answers nothing. A subscriber that negotiated no RTX for the
  format gets no answer, and the NACK is counted. Its loss is covered by a keyframe request
  when the rtp lane's rules call for one. Caching sealed media bytes per subscriber to resend
  them would cost a copy of every packet for every subscriber, so it is not done.
- **Sender reports are per down-track.** An SR describes the RTP packets that its SSRC sent
  ([RFC 3550](https://www.rfc-editor.org/rfc/rfc3550) §6.4.1), and `protect_rtcp` picks the
  sealer by the clear sender SSRC (SRTP-3). So each down-track's SR is sent with that
  down-track's media SSRC as its sender SSRC, sealed under that SSRC's `Rtcp` sealer and
  SRTCP index. It carries the down-track's own outgoing NTP-to-RTP mapping (RTC-5) and its
  own sent packet and octet counts (`stats.Sent`, RTP-5). Its report blocks are left empty,
  since the server's reception reports go under the control SSRC.
- **The server's own feedback.** Each session issues one `Rtcp` sealer for its control SSRC,
  drawn from the same generator. Standalone receiver feedback (RR, NACK, PLI, FIR and TWCC)
  uses it. SRTCP's 31-bit index rises once per packet on each sender SSRC.

`offer` (RTC-2) signals each down-track's two SSRCs with `a=ssrc` and `a=ssrc-group:FID`.
A browser also accepts unsignaled SSRCs by MID, but signaling them pins the RTX pairing.

## RTC-5: Router, rooms and fan-out

```hirsel unbuilt
pub opaque type Router
pub type RoomId = RoomId(u64)
pub type Participant = Participant(room: RoomId, id: u64, epoch: u64)
pub type PublicationId = PublicationId(u64)
pub type Target = Off | Layer(encoding: u8, spatial: u8, temporal: u8)
pub type RoomLimits = RoomLimits(participants: u16, ids: u16, publications: u16, down_tracks: u32, audio_forward: u8, pause_delay_ms: u32)
pub type RouterError = NoRoom | NoParticipant | NoPublication | Fenced | Suspended | Exists | Limit(u32) | Peer(PeerError) | Capacity(Capacity)
pub type Event = Answer(Participant, Bytes) | Offer(Participant, u64, Bytes) | Glare(Participant, u64) | Published(PublicationId, Participant, Media, u8) | Unpublished(PublicationId) | Layers(PublicationId, u8) | Speakers(RoomId, Inline[u64, 8], u8) | Fenced(Participant)

pub fn router(units: u16, unit: u16, mut inbox: Inbox[RouterIn], peers: List[Sender[RouterIn]]) -> Result[Router, RouterError]
pub fn open_room(mut r: Router, room: RoomId, limits: RoomLimits) -> Result[(), RouterError] waits
pub fn join(mut r: Router, who: Participant, publish: bool, offer: Bytes) -> Result[(), RouterError] waits
pub fn signal(mut r: Router, who: Participant, kind: SignalIn) -> Result[(), RouterError] waits
pub fn subscribe(mut r: Router, who: Participant, p: PublicationId, target: Target) -> Result[(), RouterError] waits
pub fn set_target(mut r: Router, who: Participant, p: PublicationId, target: Target) -> Result[(), RouterError] waits
pub fn rebind(mut r: Router, who: Participant, from: PublicationId, to: PublicationId, target: Target) -> Result[(), RouterError] waits
pub fn unsubscribe(mut r: Router, who: Participant, p: PublicationId) -> Result[(), RouterError] waits
pub fn leave(mut r: Router, who: Participant) -> Result[(), RouterError] waits
pub fn revoke(mut r: Router, room: RoomId, participant: u64, epoch: u64) -> Result[(), RouterError] waits
pub fn suspend(mut r: Router, room: RoomId) -> Result[(), RouterError] waits
pub fn resume(mut r: Router, room: RoomId) -> Result[(), RouterError] waits
pub fn close_room(mut r: Router, room: RoomId) -> Result[(), RouterError] waits
pub fn turn(mut r: Router, now: u64, mut sock: UdpSocket, mut rx: Bytes, mut batch: List[UdpPacket], mut out: Outbox, mut events: List[Event]) -> Result[u64, RouterError] waits
```

**Identity.** A participant is `(room, id, epoch)`. `id` is the caller's, a number below
`RoomLimits.ids`. RTC-8 gives it a dense local id that it reuses (RTC-8, Identity).
`epoch` is a value the caller raises each time `id` is admitted again. The router never
compares `epoch` with anything but itself (RTC-7). A publication is one track a participant
sends: one agreed receiving section with its encodings. A **down-track** is one subscriber's
copy of one publication, bound to one sending section of the subscriber's session. A
session's **generation** is its participant's `Gen(id, epoch)` (RTC-3), the tag every queue
entry carries.

**Units and the control order.** Every media unit runs one `Router` over its own socket. The
unit that runs the signaling adapter (RTC-8) is the **control unit**. Every control call
(`open_room` to `close_room`) is made on it. It applies the call to its own state, sends the
change as one `RouterIn` message to every other unit, in one order, and returns when the units
the change touches have applied it. So every unit sees one order of control changes.

**Waking a unit.** A unit waits in `udp_recv` (NET-16), which no inbox message interrupts.
So whenever a unit posts one or more messages to another unit's inbox in a turn, it ends
the turn by sending that unit one zero-length **doorbell** datagram from its own socket to
the other unit's port, at most one per destination per turn. Each unit's service rules
allow `UdpSend` to the sibling units' ports. A zero-length datagram from a sibling unit's
address only wakes the unit, and from any other address it is dropped and counted. A
forged doorbell costs one turn and nothing else, because it carries no data. A doorbell is
only a wake. No fence or ordering depends on it arriving: a unit never waits longer than
its next timer deadline, nor longer than 50 ms when it has no deadline sooner, so a lost
doorbell delays a fence by at most 50 ms.

**A turn.** `turn` drains the control inbox, then the cross-unit media inbox (below). It then
takes one `udp_recv` batch, and **drains the control inbox again before it routes any
datagram of that batch**, so a fence that arrived while the unit waited is in force for
every datagram the wait returned. It runs each datagram through its session (RTC-3), routes
each `Media` and `Repair` (below), drives `poll` for sessions whose deadline has passed, and
ends with the send step: it drops every entry of `out` and of the uncommitted suffix whose
tag names a fenced generation, or a suspended room with a `src`, then calls `udp_send`. A partial send (NET-16:
`Ok(k)` with `k` below the count) keeps entries `k` onward as the **uncommitted suffix**,
which the next turn's send step rechecks against the fences before it retries it. It
appends signaling and dynacast output to `events`, which the adapter sends (RTC-8). It
returns the earliest deadline. A unit's loop is `turn` and then `udp_recv`'s wait until that
deadline or 50 ms, whichever comes first.

**Fan-out.** For a `Media` packet of publication `P`, encoding `e`:

1. The home unit decrypts once, in its receive slot (RTC-3).
2. For each local down-track of `P` whose current or pending layer (RTC-6) is on encoding
   `e`, the router calls the down-track's `Rewrite.on_packet` (RTP-11), which takes a
   signed `ts: i64` extended by RTP-2 and decides whether the packet is forwarded, and under
   which layer. `Forward` and `Late` carry the `u32` wire timestamp. For a `Forward`, the
   rtp lane writes a scratch copy of the header in the subscriber's wire identity: the
   down-track's SSRC, the rewritten sequence number and timestamp, the subscriber's negotiated payload
   type for that codec configuration, the subscriber section's MID value in the MID
   extension, the subscriber's negotiated extension IDs for every extension it keeps, no
   RID (a down-track is one unlabelled stream), and the codec descriptor's picture ID or
   frame number through `media_bits`. The publisher's MID, RID, payload type and
   transport sequence number never reach a subscriber. The packet is then queued, with
   its `Tag`, for the subscriber's pacer, or straight to `out` when the rtp lane does not
   pace that down-track. A pacer entry (RTP-10's `Q`) is unsealed: the scratch header, the
   tag and a `Shared[Bytes]` of the plaintext. When any down-track queues the packet, the
   home unit first copies the payload out of its receive slot into that one `Shared[Bytes]`
   (the copy step 3 makes for a remote unit), so the slot can be reused at the next
   `udp_recv` while the entry waits. At send, it takes the next transport-wide sequence number of the
   subscriber's own transport (`twcc.assign`, RTP-8). When the payload is unchanged, the
   router calls TLS-8 `seal_into` with the scratch header in the subscriber's span of
   `out.data` and the shared payload as source: one copy, made by the cipher. When the
   codec descriptor was rewritten, it writes header, rewritten descriptor and the rest
   of the payload into the span and calls `seal` in place from the header's end, so the
   descriptor is encrypted (rtp RTP-11). The egress history records the original output
   rewrite (RTC-4).
3. For each other unit with at least one down-track of `P`, the home unit posts one
   `Media(P, src, speaking, Shared[Bytes])` message, where `src` is the publisher's
   generation and `speaking` is the publication's speaking state (RTC-6) after this packet. The
   `Shared[Bytes]` holds the plaintext packet, and every remote unit runs step 2 from it.
   One plaintext copy per remote unit, whatever its number of subscribers.

A `Repair` from the publisher (RTC-3) is routed the same way as the media packet whose
index it carries. Each down-track's `Rewrite` answers it as it answers any packet arriving
late. For an outbound RTX repair, `rtx.write` takes `pre` (the rewritten descriptor)
before the shared remainder.

The home unit keeps, per publication, the set of units with down-tracks (a `u64` bitmap, so
at most 64 units). The ingress RTX ring keeps the plaintext packet as the rtp lane specifies,
and only on the home unit. A remote unit's NACK is looked up in that down-track's egress
history on the remote unit. A hit sends `RepairAsk(P, src, dst, ingress index)` to the home
unit. The home unit replies `RepairReply(P, src, dst, Shared[Bytes])` if `src` is not fenced
there, and the remote unit rechecks both `src` and `dst` against its own fences when it
takes the reply. Only units with a subscriber pay anything for a publication.

**RTCP is terminated.** The session walks feedback with `rtcp.next(pkt, mut at)` after
`rtcp.check(pkt, mode)` accepts the datagram (RTC-3). The router never forwards a subscriber's
RR, NACK or TWCC to a publisher. Each session answers its own feedback (RTC-4) and estimates
its own bandwidth (rtp lane). Publisher keyframe requests are the router's (RTC-6). A subscriber's sender
reports are generated per down-track (RTC-4) from the down-track's own outgoing timeline.
The router relies on RTP-11's `sr_timestamp` to answer that timeline's RTP timestamp for an
NTP time, which is correct across a source switch only under RTC-6's switch rule.

**Bounds.** `RoomLimits` fixes every table's size when the room opens, so routing allocates
nothing:

- `participants` is at most 256, the `runner.room` peer limit (`PEERS_MAX`);
- `ids` is the fence capacity (RTC-7), at least `participants`. One entry is reserved when a
  participant id is first admitted and kept until `close_room`;
- `publications` is at most 8 per participant;
- `down_tracks` is at most 64 per subscriber;
- encodings are at most 4 per publication (RTC-1).

A call that would pass a bound is `Limit(bound)`, and nothing changes. A `join` with an id
the room has never admitted, when all `ids` entries are reserved, is `Limit(ids)`: the
identity is refused explicitly rather than admitted without a fence entry.

## RTC-6: Layers, dynacast and active speakers

**Layer identity.** A packet's layer is `(encoding, spatial, temporal)`:

- `encoding` is the position of its stream in the publication's agreed simulcast list
  (`Agreed.streams`, RTC-2), found from its RID or any of the stream's alternatives (0
  without simulcast);
- `spatial` and `temporal` come from the AV1 Dependency Descriptor, the VP9 payload
  descriptor or the VP8 `TID` (`media_bits`), and are 0 when the codec carries none.

`Target = Off | Layer(e, s, t)` asks for encoding `e` with every spatial layer up to `s` and
every temporal layer up to `t`. `t = 255` means every temporal layer. This is the
generic dynacast selector. The subscriber's application decides why it wants a layer. The
router never knows.

**Mapping onto `rtp`'s rewrite.** The switching itself is the rtp lane's `Rewrite`
(RTP-11), one per down-track, whose masks hold 32 decode targets. The router translates each
`Layer(e, s, t)` to `rewrite.target(Layer(source, i))`, which refuses `i > 31`
(`Unrepresentable`). `source` is the router's key for encoding `e` of the publication and
`i` comes from a **stable** per-codec map. The map never depends on how many layers a stream
has shown so far, so a newly observed layer never
renumbers a target that is in use. `Off` is `rewrite.target(Off)`.

- **VP8 and H.264.** `i = t`, clamped to 3 (VP8's `TID` has two bits). H.264 without SVC
  carries no layer, so `i = 0`. `needs` bit `j` is set for a frame of temporal id `t'` when
  `t' ≤ j`. `switches` has every bit at a keyframe, and, for VP8, bit `t'` at a frame with
  `Y = 1` (a layer sync frame, which depends only on the base layer,
  [RFC 7741](https://www.rfc-editor.org/rfc/rfc7741) §4.2).
- **VP9** ([RFC 9628](https://www.rfc-editor.org/rfc/rfc9628) §4.2). `i = 4 · s + t`, with
  `s` and `t` each at most 3, so 16 targets cover every structure the map accepts. A layer
  frame whose `SID` or `TID` passes 3 is in no target, so no switch can reach it. Every
  down-track keeps its current target, and the frame goes to each `Rewrite` with empty
  `needs` and `switches`, so each leg closes it up and its numbering has no hole. The frame
  is counted once (`unmapped`). Forwarding it is unsound: it may reference earlier frames
  above 3, which the map never tracked, so no switch point for it can be computed. A lower
  layer never references it, so the legs stay decodable. `needs` bit `4 · S + T` is set for a
  layer frame `(s', t')` when `s' ≤ S` and `t' ≤ T`. `switches` follows VP9's own switch
  facts. Every bit is set at a keyframe. A **temporal** up-switch from `T` to `T'` within
  spatial `S` is allowed at the first layer frame with `TID` above `T` that follows a
  picture with `U = 1` and `TID ≤ T`. `U` is the switching-up point, after which no frame
  of a higher temporal layer references an earlier picture of a layer above it. A
  **spatial** up-switch to `S'` is allowed at a picture whose layer frame at `S'` has
  `P = 0` (not inter-picture predicted). Its only references are lower spatial layers of
  the same picture (`D = 1`), and the target forwards all of those. A down-switch, temporal
  or spatial, is allowed at any picture start, since a lower layer never references a
  higher one. The temporal rule depends on the down-track's current `T`, so for VP9 the router
  fills `switches` per down-track, once for each `on_packet` call, not once per packet.
- **AV1** (Dependency Descriptor, [AV1 RTP §A.8](https://aomediacodec.github.io/av1-rtp-spec/#a8-dependency-descriptor-rtp-header-extension)).
  `media_bits` `decode_target` answers the indication for a frame and a decode-target
  index, and the router needs the inverse. `Structure` is opaque, so S2 adds
  `media_bits.target_layers(View[Structure], d) -> Option[(u8, u8)]`, which reads the
  templates' spatial and temporal ids and indications. At each new template structure it derives,
  for every decode target `d`, the pair `(S_d, T_d)`: the highest spatial id and the highest
  temporal id over the templates whose indication for `d` is not Not Present. `Layer(e, s,
  t)` then maps to the `d` with `(S_d, T_d) = (s, t)`. With no such `d`, it maps to the `d`
  whose pair is the greatest at or below `(s, t)` in both coordinates. A structure is
  **unrepresentable**, and refused explicitly with the encoding kept at its current target
  and the refusal counted, when it has more than 32 decode targets, or when two decode
  targets share one pair, or when no single greatest pair exists at or below some request.
  `needs` bit `d` is set unless the indication is Not Present, and `switches` bit `d` is set
  for `Switch` and at every keyframe (RTP-11). The map is rebuilt only when the structure
  changes, which happens only at a keyframe.

**Presenting packets.** Each packet of the down-track's **current** source and of its
**pending** source (the source of a target that has not yet taken effect) goes to
`Rewrite.on_packet` (RTC-5 step 2). A filter that passed only the current layer would never
show `Rewrite` the pending layer's keyframe. The router fills rtp's `Unit` per packet from
`media_bits`: `start` and `end` from the descriptor or the marker bit (a VP8 packet with
`S = 1` and partition index 0 starts a frame; a nonzero partition with `S = 1` does not),
and `keyframe` from the codec's keyframe test. The RTP S9 unit adapter fills the 32-bit
`needs` and `switches` masks from the map above.
`Rewrite` commits a switch only at a decodable frame start. It forwards the rest of that
frame under the same decision, and it keeps forwarding the old source until then.
`Rewrite` answers `Wait` while it has no current layer, and the router then asks for a
keyframe.

**The timeline at a switch.** The router relies on this RTP-11 operation, which the rtp
amend after review 2336 specifies. At the accepted frame start of source B, replacing A,
the down-track keeps its SSRC and sealer, and the first new outbound index is the last
outbound index plus 1. The new timestamp offset is found by projecting the B packet's
capture time (from B's SR) into A's RTP clock, subtracting A's current source-to-output
offset, and clamping the result to at least the last outbound timestamp plus 1. That fixes
B's offset. Every packet of that frame, including a second packet with the same source
timestamp, takes the same output timestamp. When either source has no usable SR, the new
output timestamp is the last outbound timestamp advanced by the elapsed arrival time at the
negotiated clock rate.

**Selection per down-track.** Each down-track keeps `target`, the layer the subscriber asked
for, `current`, the layer it forwards, and `pending`, a layer awaiting its switch point. The
**effective target** is the target lowered to what the subscriber's bandwidth estimate (rtp
lane) can carry. Each turn that the estimate or a target changes, the router recomputes it
for that subscriber:

1. Explicit `Off` targets stay `Off`.
2. The budget is the estimate minus the measured rate of every audio down-track, which is
   never lowered.
3. Every other video down-track starts at `Off`.
4. In ascending publication ID, round after round, each down-track is raised one step if
   the step fits in the remaining budget. A step is the next encoding at or below its
   target, lowest first, starting from `Off`, and costs that encoding's measured ingress
   rate minus the rate of the level it leaves. A down-track whose step does not fit stays
   where it is for the rest of the computation. The others go on.
5. The computation ends when a full round raises nothing.

A down-track whose first step does not fit is left `Off`, which is the **bandwidth pause**.
It resumes when a later recomputation admits it. The result is deterministic and
computable by hand.

`current` moves to the effective target only at a switch point, by the rules of the map
above, and only through `Rewrite`. A simulcast change (another encoding) happens at the
first keyframe of the new encoding. On a change of effective target, the router asks the
publisher for a keyframe of the new encoding (below) and keeps forwarding the old encoding
until one arrives. Going to `Off` takes effect at once. A frame is forwarded whole or not at
all.

**Keyframe requests.** A down-track that needs a keyframe calls `Keyframes.want(Loss|Switch, now)`
(RTP-6). The home unit keeps one `Keyframes` per publication encoding. Picture loss, such
as a subscriber's PLI or rtp's `Losses` answering `Reset`, is `Loss` and always requests
PLI. A layer switch, a subscriber's FIR for a fresh decoder, or a `Wait` from the rewrite is
`Switch`, which requests FIR when the publisher negotiated `ccm fir` for the encoding's
payload type ([RFC 5104](https://www.rfc-editor.org/rfc/rfc5104) §4.3.1), and PLI otherwise.
A PLI is sent only when the publisher negotiated `nack pli` for that payload type
([RFC 4585](https://www.rfc-editor.org/rfc/rfc4585) §§4.2, 6.3.1). If the selected feedback
was not negotiated, the down-track waits for the next independent frame and counts the wait.
The throttle interval is `max(100 ms, rtt)`, where `rtt` is the publisher session's round-trip
estimate. When the estimate moves by more than a quarter, the router updates the interval **in place** on the
same `Keyframes`, keeping its outstanding request and its FIR command sequence number
(RFC 5104 §4.3.1.1). It never makes a new one. `Keyframes` coalesces, so N subscribers asking
at once cost one request. The router calls `keyframe_seen` at each keyframe it reads.

**Publisher pause (dynacast).** For each publication, each unit computes its **local
wanted mask**: bit `e` is set when one of its down-tracks of the publication has an
effective target, a current layer or a pending layer on encoding `e`. An old encoding still
held during a pending switch keeps its bit, and an explicit `Off` sets none. A unit sends its
local mask to the publication's home unit whenever the mask changes (`Wanted(P, unit,
mask)`). The home unit keeps the latest mask from every unit, and the **wanted mask** is
their union. A bit that turns on is published at once. A bit that turns off is published
only after it has stayed off for `pause_delay_ms` (default 5,000). A target that comes back
inside the delay costs no restart. Each published change is `Event.Layers(publication,
mask)`, which the control unit's adapter sends to the publisher (RTC-8), so its encoder stops
the unwanted encodings. Packets of an encoding outside the mask are never forwarded, whether
or not the publisher has stopped it yet.

**Active speakers.** For each audio publication that negotiated the RFC 6464 extension, the
home unit keeps a score in integer arithmetic. This is LiveKit's algorithm
(pkg/sfu/audio/audiolevel.go in /tmp/ref-livekit at `d210b4d`) with its float steps made exact.
It reads only the seven level bits. The `V` bit is ignored when the publisher's leg
negotiated `vad=off`, and the score never reads it in any case.

- `amp(l) = round(65536 · 10^(-l / 20))` for levels `l` 0 to 127, a fixed table of 128 Q16 values;
- each packet whose extended RTP timestamp is above the highest seen adds its duration `d`
  (the timestamp step, at the clock rate) to `observed`. When its level `l ≤ 35` (-35 dBov),
  it adds `d` to `active` and lowers `loudest` to `l`. A reordered packet, whose timestamp is
  not above the highest, adds no duration and changes nothing;
- when `observed ≥ 400` ms, the window closes. If `active ≥ 160` ms (40 %), then
  `lin = floor(amp(loudest) · active / 400)` and
  `score = score + floor_div((lin − score) · 2, 3)`, which is the smoothing of two intervals.
  All of this is signed 64-bit arithmetic, and `floor_div` rounds toward negative infinity,
  so a falling score drops by at least one each window until it reaches `lin`. Otherwise
  `score = 0`, as LiveKit's `observeLocked` does. Then `observed`, `active` and `loudest`
  reset (`loudest` to 127). LiveKit computes
  `10^(-(loudest - 20·log10(active / interval)) / 20)` with `interval` 400 ms, which equals
  `amp(loudest) · active / interval`;
- a publication with no packet for 800 ms has score 0;
- a publication is **speaking** when `score ≥ amp(35)`.

The speakers are the speaking publications ranked by these integer scores, then by lower
participant ID. Q16 quantization can tie two scores that LiveKit's floats would order, and
the tie-break then decides. That is the specified rank. Each 400 ms in which the top 8
changed, the control unit emits `Event.Speakers(room, top, count)`.

**Audio forwarding.** With `audio_forward = n > 0`, each subscriber's audio down-tracks
forward only the `n` highest-ranked speakers, excluding the subscriber itself. Other audio
down-tracks are held at `Off` until they rank again. Opus needs no keyframe, so the gate
switches at the next packet. The RFC 6464 extension is forwarded with the subscriber's ID and
the seven level bits unchanged ([RFC 6464](https://www.rfc-editor.org/rfc/rfc6464) §3).
The `V` bit follows each leg's negotiated `vad` (RTC-2 step 4). On a subscriber leg with
`vad=off`, `V` is written 0 and means nothing. On a leg with `vad` on, `V` is the
publisher's `V` when the publisher's leg also negotiated it on, and otherwise whether the
publication is speaking by the score above. A remote unit takes that state from the
`Media` message that carried the packet (RTC-5), so it never needs the home unit's score. So a bit that had no meaning on the way in is
never given one on the way out. A leg on which the extension was not negotiated, or not in
a direction toward the subscriber, carries no extension. With `n = 0`, all audio is
forwarded. A voice room of N speakers then costs `N · n` audio packets per frame interval
instead of `N · (N − 1)`.

## RTC-7: Revocation fence

`revoke(r, room, participant, epoch)` stops every forwarding that involves `participant` at
`epoch` or any lower epoch, on every unit, in both directions, before it returns.

**Fence state.** Each unit keeps, per room, `fenced[participant]`: the highest revoked epoch,
initially absent. The table has `RoomLimits.ids` entries, fixed at `open_room`. An entry is
reserved when `join` first admits an id (RTC-5 Bounds), so an admitted identity can always
be revoked. A unit that applies a revoke sets the entry to `max(fenced[participant],
epoch)`, so a repeated or older revoke changes nothing. An entry keeps its high-water mark
until `close_room`. Nothing evicts it, because an evicted entry would let a stale epoch
rejoin. A caller that churns through ids reuses them with rising epochs, as RTC-8 does, or
gets `Limit(ids)` for an id beyond the table.

**What a unit does when it applies a revoke.** A unit applies a revoke at its next fence
point: the start of its turn, or right after `udp_recv` returns (RTC-5, A turn). The control
unit's message rings the unit's doorbell, so a waiting unit wakes for it. In control order,
the unit does each of these before it routes any datagram or media message:

1. **Ingress.** The participant's PeerSessions with epoch at or below the fence are closed
   (RTC-3) without the `close_notify` alert. Their 5-tuple rows go, so their datagrams reach no
   session. Their ICE credentials stop matching, so a Binding request under them gets no
   answer, and their DTLS and SRTP state is erased, so nothing they hold can be unprotected.
2. **Every queue, by tag.** Every entry whose `Tag` (RTC-3) has a fenced `src` or a fenced
   `dst` is removed: in `out`, in the uncommitted suffix of a partial `udp_send`, in every
   pacer (`Pacer.remove(stream)` for each removed down-track; for a down-track that stays, each entry its pacer hands out is checked against the fences before it is sealed, and a fenced one is dropped), and among the `RepairAsk` and `RepairReply` messages it holds. This covers
   media, RTX repair (late-packet RTX and NACK answers), and RTCP addressed to a fenced
   session.
3. **Routing state.** Every down-track of their publications, and every down-track they
   subscribe to, is removed. `Rewrite.close` is called on each, so every later `on_packet`
   answers `Drop`. `History.clear` is called on their publications' ingress histories and on
   the egress histories of every removed down-track (RTP-12).
4. **In flight between units.** A `Media`, `RepairAsk` or `RepairReply` message whose `src`
   or `dst` is fenced at its epoch is dropped when it is taken. The fence is set first, so a
   message posted before the revoke and taken after it is dropped.
5. **Writers.** Media units write only UDP (RTC-8 Strict firewalls). `udp_send` commits whole
   datagrams (NET-16), and the suffix it did not commit was purged in step 2, so no writer
   holds a partly sent packet.

**The acknowledgement.** The control unit applies the revoke, sends it to every other unit
(not only those it thinks are involved, since a subscription change can be in flight), rings
their doorbells, and returns `Ok(())` when every unit has acknowledged it. A unit
acknowledges only after steps 1 to 5 hold together. Every older admission of the
participant has stopped routing, and every writer has either finished handing its bytes to
the kernel before the fence point or had its uncommitted work cancelled. No `udp_send`
is in progress on a unit at its fence point, because the unit runs them on
its own task between fence points. So once `revoke` returns:

- no unit will pass to `udp_send` a datagram that carries media or
  repair of the participant at a fenced epoch, or that is addressed to one of its fenced
  sessions. Before every send and every retry of an uncommitted suffix, the unit rechecks
  both endpoints' tags against the fences;
- no datagram from the participant's fenced sessions will reach any routing step.

Bytes already handed to the kernel before the fence point have left the program and may
still arrive. The fence is at the send, because the kernel cannot recall a datagram. The
router emits `Event.Fenced(participant)` after the return. RTC-8's adapter turns it into the
room message `Signal.Fenced(peer, epoch)`, and that is what the controlling application
observes.

**Rejoin.** `join` with `epoch ≤ fenced[participant]` is `Fenced`. A rejoin needs a higher
epoch, and it makes a new PeerSession with fresh ICE credentials, a new DTLS association and a
new SRTP schedule.

**Suspend.** `suspend(r, room)` uses the same machinery for a whole room without revoking
anyone. Every unit, at its fence point, stops routing media and repair for every session of
the room, and removes every queued entry whose tag names the room and has a `src`, in step 2's
queues and messages. Entries with no `src` (STUN, DTLS, RTCP) stay. Ingress stays up, so ICE consent and DTLS keep running and RTCP receiver reports
continue. The control unit returns when every unit has acknowledged it, under the same rule
as a revoke. `resume(r, room)` lifts it the same way. A `join`, `subscribe` or `set_target`
on a suspended room is `Suspended`. RTC-8 uses this for signaling loss.

**Cost and liveness.** A revoke or a suspend waits for each unit's next fence point plus one
inbox hop. The doorbell wakes a waiting unit at once, and a lost doorbell costs at most the
50 ms wait cap. A unit inside a turn reaches its next fence point when its current
`udp_send` returns, and that call waits at most until the turn's deadline (NET-16). So the
fence's latency is bounded by the slowest unit's turn plus 50 ms. No unit
waits for another inside a turn, so revokes cannot deadlock.

**Leave is not revoke.** `leave` closes the participant's sessions with `close_notify`,
removes its publications and drains nothing that is already queued. It is for ordinary
departures. Revocation is for authority withdrawn.

## RTC-8: Signaling through runner.room

The router takes signaling as calls and emits it as `Event`s. The `signal` module adapts
both to the generic `runner.room` (core/programs/runner/runner-room.hirsel): typed `State S`
per peer plus addressed `Message M`. The room's types are the application's, so the adapter is
generic over them:

```hirsel unbuilt
pub type Signal: codec = Offer(u64, Bytes) | Answer(u64, Bytes) | Candidate(Bytes) | EndOfCandidates | Glare(u64) | Subscribe(u64, Target) | Target(u64, Target) | Unsubscribe(u64) | Published(u64, u64, u8, u8) | Unpublished(u64) | Layers(u64, u8) | Speakers(Inline[u64, 8], u8) | Fenced(u64, u64) | Refused(u32) | Servers(Bytes, Bytes)
pub type Incarnation = Incarnation(Inline[u8, 16])
pub type RoomIn[M] = Part(Incarnation, u64, bool, bool) | PartEnd | Joined(u32) | Left(u32) | Message(u32, M) | Lost
pub type RoomOut[M] = ToPeer(u32, M) | ToRoom(M)
pub type Embed[M] = Embed(wrap: own[send] fn(Signal) -> M, unwrap: own[send] fn(M) -> Option[Signal], publish: own[send] fn(u32) -> bool)

pub fn adapter[M: codec](room: RoomId, limits: RoomLimits, take embed: Embed[M]) -> Adapter[M]
pub fn room_in[M: codec](mut a: Adapter[M], mut r: Router, input: RoomIn[M], mut out: List[RoomOut[M]]) -> Result[(), RouterError] waits
pub fn events[M: codec](mut a: Adapter[M], events: List[Event], mut out: List[RoomOut[M]])
```

The service itself is one room peer, admitted by the application's room admission like any
other caller (`Grant` `Join`). The application wraps `Signal` in its own `M`
(`Embed.wrap`, `Embed.unwrap`), so the room carries one message type. `Embed.publish`
answers whether a peer may publish, and the adapter passes it to `join`. The application
decides it from its own state (for example the peer's `S`), and the router never reads `S`.

`Incarnation`, `RoomIn` and `RoomOut` are built in `room_client`
(core/programs/runner/room_client.hirsel, F1), with `Incarnation` a `key` so the adapter can
compare a snapshot's incarnation with the pre-gap one. `room_client.next` appends the
`RoomIn` of each frame the service's session reads, and `room_client.send` sends one
`RoomOut`. The rest of the block is S7's.

**The roster the adapter reads.** Each room Part the relay sends begins with a head,
`Part(incarnation, revision, complete, snapshot)`, followed by that Part's `Joined`, then
`Message`, then `Left` entries, the order the relay writes them. `complete` is the relay's
existing flag on a membership's first Part. The room client appends `Lost` before a Part's
head when that Part reports a gap (messages the relay dropped for this member), and appends
`Lost` when its session or membership ends.
`PartEnd` follows a Part's last entry. These are runner slice R1's fields (below):

- the **incarnation** names one life of the room's roster. It is 16 random bytes, drawn by
  the relay each time it creates the roster, and again before the first admission after
  the roster has emptied. So it never repeats across rooms, empty periods or relay
  restarts;
- within one incarnation, peer numbers are never reused. The relay resets its peer counter
  only when it draws a new incarnation. So `(incarnation, peer)` is the **admission
  identity**: it names exactly one admission;
- the **revision** is the roster's membership revision. It starts at 1 for each
  incarnation and rises by one at every admission and every removal;
- `snapshot` is set on the Part after which the peer's view of the roster is complete at
  `revision`: every member at that revision has been told, and no change is pending.

- **Identity.** A participant is the room peer that sent the message, whose peer number the
  relay stamps and a peer cannot forge. The adapter maps each admission identity
  `(incarnation, peer)` to a router participant `(room, id, epoch)`. `id` is a dense local id
  below `RoomLimits.ids`: the adapter gives each new admission the lowest id that has no live
  admission, so ids are reused and never run out while the room has at most `participants`
  members. `epoch` is the next value of one counter per room that only rises, so a reused id
  always comes back above its fence. `Fenced`, `Published` and `Speakers` signals name the
  peer number, never the local id.
- **Unicast.** An offer, answer, candidate, glare or layer mask goes peer to peer
  (`ToPeer`). Only the peer the answer was sent to learns its ICE password, which is what
  binds the media path to that peer (RTC-3). `Published`, `Unpublished` and `Speakers` go to
  the room (`ToRoom`). `runner.room` permits peer-to-peer sends for `Watch` and `Join` members,
  and room sends for `Join` members (`room_permits`,
  core/programs/runner/runner-room-grants.hirsel).
- **Revocation from the roster.** `Left(peer)` calls `revoke(room, id, epoch)` for that
  admission, and then sends `Fenced(peer, epoch)` to the room. The application's own kick
  withdraws the peer's room grant, and the room's roster then removes it. So a kick is
  acknowledged to the application's watchers by the `Fenced` message, sent only after RTC-7's
  fence holds.
- **Signaling loss suspends at once.** On `Lost` (the service's room session ended or
  reported a gap), the adapter calls `suspend(room)` (RTC-7) before it takes any other
  input. Every unit stops routing the room's media and repair, and cancels what it has queued
  for the room. The call returns when every unit has acknowledged it. The service cannot see
  a kick while it cannot see the roster. Ingress stays up, so ICE consent and DTLS survive a
  short gap.
- **Resuming from a proven roster.** After `Lost`, the adapter builds a scratch roster. A Part
  with `complete` set starts a new roster image: the scratch roster is emptied there, because
  that image carries no `Left` for a peer that went during the gap, so absence from it is the
  only sign. With no `complete` Part the service's membership continued and the relay's diffs
  are exact, so the scratch roster starts as the pre-gap roster. A Part with `complete` set,
  or with an incarnation other than the adapter's, that arrives with no `Lost` before it is
  treated as `Lost` first. A `Message` taken while the room is suspended is answered
  `Refused` to its stamped sender and applied to nothing. The adapter resumes only at the
  `PartEnd` of a Part whose head has `snapshot` set, after every entry of that Part is
  applied. It then compares the snapshot with what it knew before the gap:
  - a snapshot whose incarnation equals the pre-gap incarnation and whose revision is below
    the last revision the adapter applied is **stale**. It is discarded, and the room stays
    suspended until a current snapshot arrives;
  - if the incarnation equals the pre-gap incarnation, each pre-gap admission whose
    `(incarnation, peer)` is in the snapshot continues with its id and epoch. Each one absent
    from it is revoked and `Fenced` is sent;
  - if the incarnation differs, the relay restarted or the room emptied during the gap, and
    no pre-gap admission's continuity can be proved. Every pre-gap admission is revoked, and
    each present peer must join afresh with a new offer. The same holds when an entry
    arrives with no `Part` head since `Lost`, so the adapter fails closed against a relay that predates R1;
  - new peers in the snapshot are admitted as new identities. Then `resume(room)` is called.

  Revoking every pre-gap session on any loss would also be sound. It is not the end state:
  the relay change costs one 16-byte incarnation, one revision counter and one flag per Part,
  the Hirsel room client (F1) and the SDK (F2) already read the head, so no consumer migrates, while a blanket revoke would force every
  participant in the room through a new ICE and DTLS handshake on every reconnect of the
  service's room session. The proof keeps an unchanged member's media flowing across a gap
  and still fails closed whenever continuity is not proved.
- **Refusals.** A `Signal` the router refuses is answered `Refused(code)` to the sender, with
  the `RouterError` tag as `code`. A malformed `M` never reaches the adapter, because the
  room's codec check refuses it first.

**What runner.room must provide.** media-webrtc-sfu-spec-2319 read main at `29c75134`, and
this amend reread the relay at `0ea91569`. The verdict on each of CONSUMER-CONTRACT §3's
items belongs to hirsel4-rooms-2301's report.

| Need | Status on main | Gap |
| --- | --- | --- |
| Authenticated addressed unicast | Built. `SendToPeer` exists and the relay checks the sender's grant and membership epoch (core/programs/runner/runner-room-relay.hirsel, `relay_message_target`) | none for the server side |
| A room incarnation, a membership revision and admission identities | **Built** by runner slice R1 (2ec0de5b). Each Part's head carries the roster's 16-byte incarnation and its revision, and `next_peer` resets to 1 only when the next admission into an empty roster draws a new incarnation (core/programs/runner/runner-room-relay.hirsel, `room_reconcile_seat`), so `(incarnation, peer)` names one admission. Law `room_roster_identity` | none |
| A marked complete roster | **Built** by R1. The flush sets `snapshot` on the Part after which the member has been told the whole roster at its revision, with no change pending (core/programs/runner/runner-room-live.hirsel, `room_part_start`) | none |
| Targeted kick | The application withdraws the peer's `Grant` in its admission view, and the grant set's revocation path removes it | none |
| Per-connection expiry | Built for WebSocket sessions by 7e01e2ee (rooms released at expiry, RUNNER-SPEC "Enforcement") | none |
| A Hirsel-side room client | **Built** by F1. `room_client` (core/programs/runner/room_client.hirsel) holds one room membership on a `hirsel.4` session (`wire_client`), decodes each R1 Part head into `RoomIn.Part`, sends `RoomOut`, and keeps the roster as the SDK does: a new incarnation replaces it, a revision below one already told is refused, and the Part with `snapshot` completes an image. Laws `room_client_follows_relay_roster` (the relay's own roster is the oracle at every snapshot) and `room_client_over_socket` | none |
| Room calls in the generated JS SDK | **Built.** `client.room` (sdk/client/room.ts) gives unicast and room `send`, the roster and state, and `LiveRoom.incarnation` and `revision` (F2): a new incarnation replaces the roster, and a same-incarnation image continues its peers ([sdk.md](sdk.md#rooms)) | none |

**Runner slice R1 (the relay's roster identity).** In core/programs/runner:

- `RoomRoster` gains `incarnation: Inline[u8, 16]` and `revision: u64`. `make_room_roster`
  draws the incarnation from the place's `Random` and sets the revision to 1. `next_peer`
  is no longer reset when a room's count reaches 0. Instead, the next admission into a
  roster whose count is 0 draws a fresh incarnation, resets `next_peer` to 1 and the revision
  to 1. Each admission and each removal raises the revision by one.
- `room_part_start` writes the head after `you`: the 16 incarnation bytes, the revision as
  a `u64`, and a `snapshot` byte. `ROOM_PART_FIXED` grows by 25 bytes. The flush sets
  `snapshot` on a Part after which the member's `told` equals the roster for every slot, with
  no `changed` entry pending.
- Law `room_roster_identity`, in runner_owner_laws.hirsel's room harness (`room_law_*`): a
  watcher disconnects, every other member leaves so the room empties, new members join
  (taking peer 1 again), and the watcher reconnects. A second case churns 300 sequential
  admissions through a room with 256 slots on one incarnation. *Oracle:* by hand. The
  reconnecting watcher sees a different incarnation. No peer number repeats within one
  incarnation. The revision equals the hand-counted admissions plus removals. `snapshot` is
  set on exactly the Part that completes the roster. On main before R1 the first case
  shows peer 1 twice with nothing to tell the two apart, so the law is red.

The shipped JS SDK reads Live Parts (sdk/client/wire.ts), so R1's Part head changes a wire
field it reads: the SDK decoder, the frame schema and the frame vectors change with the relay
in the same commit. The Hirsel room client (F1) reads it too.
The adapter's laws drive it with a harness that feeds `RoomIn` and reads `RoomOut`. In
service, `room_client` feeds it and carries its `RoomOut` (S7), and the browser end-to-end
case runs that way.

**Strict firewalls.** Media units listen on UDP only. A browser that cannot reach them over
UDP uses TURN over TCP or TLS on 443 with a UDP relay ([turn.md](turn.md) TURN-2). The relay is
a sibling place on the same host, so a relayed browser reaches a unit as an ordinary UDP
source, authenticated by ICE and DTLS like any other (TURN-8). Signaling mints the TURN
username and password beside the ICE credentials (TURN-5), at admission and before any
offer, since a relay-only browser gathers no candidate without them. The adapter sends them
to the admitted peer alone as `Servers(username, password)` (`ToPeer`); the TURN URLs are the
deployment's and fixed (TURN-11). The ICE ufrag and password minted with them go into the
answer. A revoke also reaches the relay (TURN-9), outside RTC-7's acknowledgement: the fence
already drops the epoch's packets on every path. The wiring (one `RelayPlan` for both places,
the subject derived from the admission identity, the forwarded `RelayRevoke`) is
turn.md's U8 decisions under TURN-7. Passive ICE-TCP (RFC 6544), once slice S8, is withdrawn: TURN over TLS reaches
every network it reached, and networks that pass only TLS, and media units keep one transport
and one writer kind.

## WebRTC build laws

Each law is written **red first**, in its named harness, before the code it tests. Its oracle
is outside the code under test: an RFC's text or vectors, a capture of a real browser's or
peer's wire output, or a hand derivation from this chapter. Fixtures are captured once and
committed under the package's `vectors` directory, with the capturing browser's version.
Harnesses are the package's law module (`laws.hirsel` in `webrtc_sfu`, as `stun_ice` has),
unless named otherwise.

- **W1 `sdp_browser_offers`.** *Scenario:* parse a Chromium and a Firefox offer: audio, video
  with three-RID simulcast, RTX, the seven extensions, and an `application` section, plus a
  variant whose second video section is `bundle-only` with port 0. *Oracle:* hand-written
  `Description` values from reading the captures, `Port.BundleOnly` and `tls-id` included.
  *Then* `answer` each with the default policy. *Oracle:* the expected answer text written by
  hand from RTC-2 and RFC 9429 §5.3. It has the kept MIDs in BUNDLE with the rejected
  application MID left out, the offer's payload types and extmap IDs, `setup:passive`,
  session-level `ice-lite`, `tls-id`, `a=simulcast:recv` in the offer's order, the
  `bundle-only` section accepted on the BUNDLE port, and the application section rejected at
  port 0. The local credentials and the BUNDLE MID list are pinned in the expected text.
- **W2 `sdp_rfc_examples`.** Parse the SDP examples of RFC 9429 §7 and RFC 8853 §5.6, and
  answer those that are offers. *Oracle:* for each fixture, the expected fields written by
  hand from the RFC's text: each section's port kind, MID, direction, formats, RIDs with
  direction and restrictions, and both simulcast lists. For RFC 8853 §5.6.1 (send and
  receive simulcast) the answer accepts the send list and declines the receive list. For
  §5.6.2 (alternatives and paused streams) the answer keeps the alternatives and removes the
  paused entries. A fixture whose expected result is a refusal names the `SdpError`.
  Parsing without an error is never the pass condition.
- **W3 `sdp_refusals`.** One offer per refusal of RTC-1 and RTC-2: no BUNDLE, a section
  outside it, no `rtcp-mux` on the tagged section, no MID, only a `sha-1` fingerprint, one
  extension URI under two IDs, one payload type as two codecs, payload type 80 with
  `rtcp-mux`, an invalid RID identifier, a simulcast list naming an absent RID, 32,001 bytes,
  65 sections, 33 formats, and five RIDs. Each limit also has an exact-limit fixture
  (32,000 bytes, 64 sections, 32 formats, four RIDs) that is accepted. Acceptance cases kept
  distinct from refusals: a paused list entry (accepted, removed from the answer), a RID
  with a dangling `depend=` (removed), and a RID whose `pt=` list has an empty intersection
  with the kept formats (removed). *Oracle:* the named `SdpError` with the section index, or
  the expected answer lines, by hand.
- **W4 `demux_prefix`.** `classify` over every first byte with a second byte at each
  boundary: 191, 192, 223 and 224 (RTP, RTCP, RTCP, RTP), plus the empty datagram and a
  1-byte datagram in the 128 to 191 range (`Drop`). A short RTP, RTCP, STUN and DTLS datagram
  is each dropped and counted by its reader. A payload type repeated in two sections without a
  MID extension is dropped. An SSRC rebound to a newer MID is not taken back by an older
  packet carrying the earlier MID. *Oracle:* RFC 9443 §3's table and RFC 5761 §4, by hand.
  Exhaustive over the first byte.
- **W5 `glare_settles_in_one_round`.** The server offers (adding a down-track) while a client
  offer is in flight. *Oracle:* by hand from RTC-2. The client's offer gets `Glare(v)`, the
  client's answer to `v` is accepted, its re-sent offer is answered, and both ends end
  `Stable` with both changes applied. Then: an answer to an older version is refused as
  stale, an answer with a changed fingerprint or `tls-id` is `AssociationChanged`, and an
  answer with a reordered section is `AnswerMismatch`. Each refusal leaves the running
  session's agreed sections and its forwarding unchanged, shown by the next packet.
- **W6 `late_answer_goes_out_as_rtx`.** rtp's `rewrite_late` law covers the `Late` answer
  itself. This law covers what the session does with it, and with a NACK. *Scenario:*
  source sequence 10, 11, 13, 12 of one encoding to one down-track with RTX, the same across
  the 16-bit rollover (65534, 65535, 1, 0), then the next on-time packet, then the same
  without RTX. Then two subscribers whose legs use different MIDs, payload types and
  extension IDs each NACK a packet. One subscriber NACKs a packet from before a source
  switch. *Oracle:* by hand from RTC-4, with RTX bytes laid out per RFC 4588 §4. With RTX,
  the media SSRC carries the outbound numbers in source order with no number reused. The late
  packet leaves as one SRTP packet on the RTX SSRC, with the low 16 bits of its outbound index
  as OSN, its mapped timestamp and marker, and the RTX stream's next sequence number. The next
  on-time packet takes the media number after the highest. A harness SRTP receiver opens it.
  Without RTX, the late packet is dropped and counted. Each NACK answer carries that
  subscriber's original translated header bytes (OSN, payload type, MID, extension IDs,
  payload) and a fresh transport sequence number. The pre-switch NACK answers nothing. No
  sealer returns `Reused`, `Retired` or `Range`.
- **W7 `ssrc_survives_switches`.** One down-track goes Layer(0) → Layer(2) → Off → Layer(1)
  and is rebound to a second publication, then to a third: A → B → C. A's output offset is
  nonzero, the three sources have unrelated sequence and timestamp bases, one switch crosses
  a sequence wrap, B's first frame is two packets with equal timestamps, and SRs are taken
  from the publishers' wire. A second run puts A, B and C on three publishers, whose SRs are
  never presented across publishers, so every switch uses the arrival rule. Then an
  authenticated remote packet arrives on the down-track's media SSRC. *Oracle:* by hand from
  RTC-4 and RTC-6. Until the collision, the media and RTX SSRCs never change, each sealer is
  issued exactly once (no `Issued` from TLS-8), the outbound index rises by one per forwarded
  packet, and at every intended switch the output timestamp equals the hand-projected
  value, which is strictly above the last. The two packets of B's first frame share one output
  timestamp. After the collision, a BYE for the old SSRC goes out, the down-track continues on
  a fresh SSRC with a new sealer, and the retired SSRC's sealer is never issued again.
- **W8 `fanout_across_units`.** One publisher on unit 0, two subscribers on unit 0 and two on
  unit 1, over `sim.udp`. The subscribers' legs use different MIDs, payload types and
  extension IDs from the publisher's and from each other. *Oracle:* hand-derived rewritten
  headers (SSRC, sequence, timestamp, payload type, MID value, extension IDs, transport
  sequence number from each subscriber's own transport) for 20 packets. Each subscriber's
  harness session opens every packet to the publisher's plaintext payload. Unit 1 gets
  exactly one `Media` message per packet for its two subscribers. A valid NACK from a unit 1
  subscriber is answered through `RepairAsk` and `RepairReply`. Then two SRs are opened, one
  per down-track: each is sealed under its own down-track's SSRC, and its sender SSRC,
  packet and octet counts and RTP timestamp match that subscriber's media.
- **W9 `router_fills_units`.** rtp's `simulcast_switch` and `svc_masks` laws cover switching
  given a `Unit`. This law covers how the router builds the `Unit` and the target, through
  actual egress. *Scenario:* captured VP8 (three encodings, two temporal layers), H.264
  (three encodings), VP9 SVC, and an AV1 L3T3 stream with Dependency Descriptors from
  Chromium. Cases: a target change followed by a keyframe fragmented over several packets,
  a VP8 packet with `S = 1` and a nonzero partition index, a VP9 `U` picture whose spatial
  dependency is not yet met, and an AV1 structure whose decode-target table is permuted.
  *Oracle:* for each packet, the `(start, end, keyframe, needs, switches)` and the
  `Layer(e, s, t)` to `(source, i)` mapping, written by hand from RFC 7741, RFC 9628 §4.2 and
  AV1 RTP §A.8 against the captured bytes, and the egress packets a subscriber receives. The
  switch commits at the first decodable frame start, the whole frame follows, the old source
  forwards until then, and the permuted AV1 table maps by layer, not by index. For AV1, the
  decode-target indexes are cross-checked against LiveKit's dependency-descriptor selector
  (pkg/sfu/videolayerselector in /tmp/ref-livekit) on the same packets.
- **W10 `keyframe_request_coalesces_across_subscribers`.** Three subscribers switch from
  encoding 0 to encoding 2 in the same 20 ms, and a fourth subscriber's `Losses` answers
  `Reset`. *Oracle:* by hand from RTC-6. Exactly one PLI reaches the publisher before the
  first keyframe of encoding 2. All four down-tracks then forward from that keyframe. A
  publisher with `ccm fir` and no `pli` gets one FIR. Its round-trip estimate then moves by
  more than a quarter, and the retry after the new interval carries the same FIR sequence
  number. A new request after the keyframe carries the next number mod 256 (RFC 5104
  §4.3.1.1). A publisher that negotiated neither gets no feedback packet at all.
- **W11 `dynacast_mask`.** Four subscribers change targets over 12 s of simulated time:
  one on the home unit and one on each of three units, two of which want different bits of
  the same publication at once, and one of which later drops its bit while the other keeps
  it. A home unit that keeps one remote mask instead of the per-unit union fails. One subscriber's budget is below one base layer, and a
  second subscriber can afford one base but not a second. One down-track holds its old
  encoding during a pending switch. *Oracle:* by hand from RTC-6. A bit turns on in the same
  turn, and a bit turns off after exactly 5,000 ms with no event inside the delay when a
  target returns. Each remote unit's demand sets bits, and the bit dropped by one remote unit
  stays on while the other still wants it. The below-base down-track stays `Off`,
  the unaffordable second base stays `Off` in publication order, and the held encoding
  keeps its bit until the switch. Packets outside the mask are never forwarded.
- **W12 `active_speaker_scores`.** Audio-level traces of four publications, 20 ms packets,
  with a reordered packet, an 800 ms expiry, and two scores that tie after Q16 quantization.
  *Oracle:* the Q16 formula of RTC-6, computed by hand for each 400 ms window, and the
  tie-break for the tie. A second oracle is LiveKit's `AudioLevel`, run on the same traces
  (Go, /tmp/ref-livekit at `d210b4d`), compared only on traces whose rounding cannot change
  the order.
- **W13 `audio_forward_gate`.** `audio_forward = 2`, four speakers whose ranks come from a
  fixed schedule supplied independently of W12. Legs: one with `vad=on`, one with
  `vad=off`, one with the extension absent, and one with the extension remapped to another
  ID. *Oracle:* by hand. Each subscriber gets exactly the top two other speakers, at the next
  packet after a rank change, with continuous outbound sequence numbers per down-track. The
  seven level bits arrive unchanged, `V` follows RTC-6's rule per leg, and the absent leg
  carries no extension.
- **W14 `revoke_fence_schedules`.** *Model:* a control unit and two media units. Steps: unit 0
  takes a datagram, routes it locally and posts `Media` to unit 1; unit 1's subscriber NACKs
  a packet and unit 0 posts a `RepairReply`; unit 0's `udp_send` commits only a prefix of its
  batch; the control unit sends the revoke; each
  unit applies it and acknowledges; each unit takes its messages, retries its suffix and
  writes. Every interleaving that keeps each unit's own order is run, through a test-only
  seam that pauses each step, for the revoked participant as publisher and as subscriber.
  *Oracle:* exhaustive enumeration against the invariant that no `udp_send`
  after `revoke` returns carries the participant's media or repair at the fenced epoch, or
  goes to its fenced session. The same enumeration runs `suspend`. A negative control that
  acknowledges while a writer still holds a fenced packet (the uncommitted suffix kept for
  retry) must fail the enumeration. A further step holds, across the ACK, a paced entry from a
  publisher the down-track was rebound away from; the invariant must still hold.
- **W15 `revoke_stops_everything`.** Real units over `sim.udp`. The
  revoked participant publishes video with NACK-driven RTX queued, and subscribes to two
  publications. At the revoke, an RTX reply and a UDP suffix are each held across
  the acknowledgement, in both directions. After `revoke` returns: *Oracle:* by hand from
  RTC-7, from captures at each unit's writer. Across 200 ms of further input, no
  captured datagram carries any of its down-tracks' SSRCs or any packet of its
  publications. A Binding request under its old ufrag gets no response. A DTLS record and an
  SRTP packet from its old 5-tuple are dropped and counted. An unrelated participant's media
  keeps flowing throughout. `join` at the same epoch is `Fenced`, and at a higher epoch it
  succeeds with new credentials. A repeated revoke changes nothing. Then ids are churned in
  sequence up to `RoomLimits.ids`. One more unseen id is `Limit(ids)`, and the oldest
  fenced epoch is still refused `Fenced`.
- **W16 `signaling_loss_fails_closed`.** The adapter, driven by a harness, gets `Lost` while
  units hold queued media and an RTX repair for the room, then Parts with `snapshot`. Cases:
  (a) the same incarnation on a continued membership, with one peer `Left` and one
  unchanged; (b) a new membership (`complete`) on the same incarnation whose image omits a
  pre-gap peer and carries no `Left` for it; (c) a snapshot Part that itself carries a
  `Left`; (d) a stale snapshot whose revision is below the last applied; (e) a changed
  incarnation in which a different admission holds the same peer number; (f) entries with
  no `Part` head since `Lost`; (g) a `complete` Part with no `Lost` before it. *Oracle:* by
  hand from RTC-8, with captures at every unit. Nothing is forwarded from `Lost` until
  resume, and the queued repair never leaves. In (a) the gone peer is revoked with `Fenced`
  sent and the unchanged peer resumes with its id and epoch. In (b) and (c) the omitted or
  left peer is revoked before `resume`. In (d) the snapshot is discarded and the room stays
  suspended. In (e) and (f) every pre-gap admission is revoked, the same-number peer
  included. In (g) the room suspends before any entry of that Part is applied.

  Case (b) is red on an adapter that seeds the scratch roster from the pre-gap roster, and
  (c) on one that compares at the head.
- **W17** withdrawn with ICE-TCP; stream framing is turn.md's `stream_framing`.
- **W20 `relay_only_session`.** A browser end (ICE nomination, DTLS, SRTP) reaches a
  PeerSession only through `turn.Client` and the relay, over UDP and over a stream, with
  credentials from `mint_session` and the relay from `relay_config` (turn.md U8). *Oracle:*
  by hand from turn.md TURN-7 to TURN-9: the relayed address is selected, DTLS connects and
  media is taken; the SFU's IP on a port outside `allow` and an outside address are
  refused; after the forwarded `RelayRevoke` the allocation is gone, nothing relays, and a
  new Allocate for the subject ends `Closed(Credentials)`.
- **W18 `forward_allocates_nothing`.** Forwarding RTP packets to 8 down-tracks on one unit,
  after warm-up, runs under a zero-byte allocation bound, as `stun_ice`'s Binding laws do.
  The measured turn must deliver eight packets, so dropping all traffic cannot pass.
  *Oracle:* the bound and the eight captured packets.
- **W19 `unauthenticated_changes_nothing`.** On an established session, feed: an SRTP packet
  with one flipped tag bit, a replayed packet, a packet whose index estimate jumps 2^15
  ahead under a bad tag, a bad-tag packet carrying a new MID or RID for a known SSRC, an
  RTCP NACK and a TWCC feedback with bad tags, a valid packet from a 5-tuple outside the
  table, and a STUN Binding request from a new address with invalid `MESSAGE-INTEGRITY`.
  *Oracle:* by hand from RTC-3, against a snapshot of every stateful value (statistics, loss
  window, `History`, `Rewrite` ranges, TWCC log, BWE, speaker score, SSRC-to-MID and
  SSRC-to-RID bindings, selected path, 5-tuple table, ROC). Each input leaves all of them
  equal and raises one drop counter. A following valid packet at the next index is accepted.
  A negative control that runs statistics before `open` must fail.

**Nightly, not pre-push.** These run on Boat and route reds to the owner. They do not block
a land.

- **N1 `browser_room`:** headless Chromium and Firefox publish Opus and three-encoding VP8
  simulcast, and subscribe to each other through the service. This is the interop oracle for
  RTC-2 to RTC-4 against a real DTLS, SRTP and RTCP stack. It covers a layer switch, an
  `audio_forward` gate, a revoke whose subscriber stops receiving at once, and a relayed
  session over TURN with UDP blocked (turn.md U8). The service joins the room through `room_client` (F1).
- **N2 `room_vopr`:** 100 participants over `sim.udp`, with Gilbert-Elliott loss, reordering,
  NAT rebinding, joins, leaves, revokes and target changes from a seed. The invariants are
  RTC-7's, the TLS-8 nonce rules (no `Reused`, `Issued` or `Range` from any sealer), RTC-4's
  SSRC rules, and bounded memory per room.

## Cost targets

From PLAN row 1.6 (research-1124/realtime-media) and the MOONSHOT's SFU row. The build measures
them under the host rules' matched-comparison terms, against mediasoup's
`SimpleConsumer`/`SimulcastConsumer` path on the same CPU, with what each side does stated.

| Target | Bar | Measured as |
| --- | --- | --- |
| Fan-out overhead | ≤ 250 ns per forwarded packet per down-track, excluding the cipher and the kernel | `bench/media/sfu_fanout`: 1 publisher, 1 to 64 down-tracks on one unit; instructions and cycles per packet, subtracting a copy of the same bytes for both paths (`seal_into` and in-place `seal`) |
| Allocations | 0 per forwarded packet | W18, and the bench's allocation counter |
| Voice capacity | ≥ 2,000 audio streams per core | 2,000 sessions of 50 packets per second, 200-byte payloads, `audio_forward` 3, one unit; under 100 % of one core, with no drops counted |
| Forwarding jitter | p99 < 500 µs from `udp_recv` return to `udp_send` issue | the same run, from `trace` timestamps |
| Browser end to end | voice plus 1080p three-encoding simulcast | N1 |
| Room scale | a 100-peer room under `sim.udp` | N2 |

## Build slices

Each slice lands on its own with its laws. A Sol lane can run S1 to S6 in order; S2 can run
beside S1. S7 runs after S5, beside S6. R1 is a runner slice and can run at any time. Reds that may ride a land
are the nightly N1 and N2, and any cost target not yet met. A red law of a slice's own blocks
that slice. A TLS-8 `Reused`, `Issued` or `Range` anywhere, or any red in W14, W15, W16 or
W19, blocks, because those are soundness reds.

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **S1** SDP | the package skeleton (`webrtc_sfu` with `sdp`, `peer`, `router`, `signal`); RTC-1 `parse` and `write` with port kinds, `tls-id`, per-direction RIDs and simulcast, extmap direction and `vad`; RTC-2 `answer`, `offer`, `accept_answer` with association reuse and answer validation, glare state; captured browser offers committed as vectors | nothing | W1, W2, W3, W5 |
| **S2** VP8 descriptor | `media_bits` gains `target_layers` for AV1 (RTC-6) and VP8 (RFC 7741): payload descriptor parse (`X`, `I`, `L`, `T`, `K`, `PictureID`, `TL0PICIDX`, `TID`, `Y`, `KEYIDX`, `S` and partition index), keyframe detection from the payload header's `P` bit, and the rewrites of `PictureID` and `TL0PICIDX` | nothing | in `media_bits`'s own laws: RFC 7741 §4's examples, and captured Chromium VP8 packets, by hand |
| **S3** PeerSession | RTC-3 over `stun_ice` Lite, the dtls lane's server, the srtp lane's contexts and the rtp lane's parsing; prefix `classify`; authenticated MID, SSRC and payload-type binding; inbound RTX; the 5-tuple table; the `Tag`ged outbox; RTC-4's SSRC generator, sealers, collision recovery, late-packet RTX, per-down-track egress history and NACK answers, per-down-track SRs | S1; dtls, srtp and rtp lanes' first slices; the rtp amend's RTP-7 history record and RTP-11 switch timeline | W4, W19 |
| **S4** One-unit router | RTC-5 on a single unit: rooms, publications, down-tracks, fan-out in the subscriber's wire identity, transport sequence numbers at send, bounds and fence capacity; RTC-6's stable target maps, current and pending presentation, switch timeline, and negotiated keyframe requests with in-place intervals | S2, S3; the rtp amend's in-place `Keyframes` interval | W6, W7, W9, W10, W18 (W6 and W7 need the router's down-tracks and switches) |
| **S5** Units and fence | the control order, doorbells, `Media`, `RepairAsk` and `RepairReply` messages with tags, the uncommitted suffix; RTC-7 with its test seam, `suspend` and `resume` | S4 | W8, W14, W15 |
| **S6** Dynacast and speakers | RTC-6's cross-unit wanted mask and pause delay, the bandwidth-limited effective target with bandwidth pause, the Q16 active-speaker scores, `audio_forward` with per-leg `V` | S5 (remote units' masks) | W11, W12, W13 |
| **S7** Signaling adapter | RTC-8's `Signal`, `Embed`, `RoomIn`/`RoomOut`, dense ids and epochs, suspend on loss, resume from a proven roster; driven by a harness in its laws and by `room_client` (F1) in service | S5; R1 for the end-to-end path (the adapter fails closed without it) | W16 |
| **S8** withdrawn | ICE-TCP is replaced by TURN over a stream (turn.md TURN-2, slice U8) | — | — |
| **S9** Costs and nightly | `bench/media/sfu_fanout` and the voice-capacity run; N1 and N2 on Boat | S5, S6, S7 | the cost targets |
| **R1** Room roster identity (runner) | RTC-8's relay change: incarnation and revision on `RoomRoster`, no `next_peer` reset within an incarnation, the Part head and `snapshot` flag | nothing | `room_roster_identity` in runner_owner_laws.hirsel |

**S4 decisions.** Gaps the spec left open, decided while S4 was built:

- **Shape.** The S4 router is sans-IO on one unit. `join` takes an opened `PeerSession`, and
  `receive(r, now, room, who, from, slot, out)` names the participant whose session the
  datagram is for: the home unit's 5-tuple table resolves `who` before the router sees it.
  `poll` drives every session and answers the earliest deadline. `revoke` runs the fence on
  this one unit; S5 adds the cross-unit order.
- **Repair ring.** A publication's ring has `floor(rtx_bytes / 1500)` slots of 1500 bytes,
  reserved at `publish`. A packet longer than a slot is not kept, so a NACK for it misses
  and counts `ring_misses`.
- **Down-track start.** `peer.add_track(p, section, first_seq, first_ts)` takes the first
  outgoing sequence number and timestamp, which the router draws from its `Random`.
- **Path move.** An authenticated nomination from a new source moves the selected path
  (`stun_ice.lite_receive`: the latest authenticated nomination wins, as mediasoup's IceServer
  does; an ordinary check moves nothing). The DTLS association, SRTP contexts and bindings
  carry over unchanged. DTLS needs no new peer address, because it uses that address only for
  cookie admission, which a `Verified` server skips. Media from the old source is then
  off-path (W19's path-move case).
- **Codec frames.** VP8 per-leg counters are keyed by the extended timestamp. A VP9 picture's
  layer frames share one timestamp, so the `Rewrite` frame key is the spatial id. The VP9
  target map is `i = 4s + t` with `s` and `t` clamped to 3 each. A layer frame whose `SID`
  or `TID` passes 3 is closed up on every leg and counted `unmapped` once per packet (RTC-6).
- **Counters.** `Counts` holds `unrequested` (a keyframe ask the publisher did not
  negotiate for), `ring_misses`, `unparsed` (a VP8, VP9 or H.264 payload whose descriptor does
  not parse, which is dropped) and `unmapped`. An ask counts as `unrequested` only once the
  encoding's SSRC is known: before its first packet there is nothing to ask about.
- **Rebind.** `rebind(who, from, to, target)` moves `who`'s down-track for `from` onto `to`.
  The track keeps its section, SSRCs, sealers, history and `Rewrite`, so `to` is one more
  source switch (RTC-6): `to`'s layer is pending, its keyframe is asked for at once, and `from`
  forwards until that keyframe arrives. It refuses `NoPublication` when `who` has no down-track
  for `from` or `to` is gone, `Exists` when `who` already has one for `to`, and
  `Peer(Sdp(AnswerMismatch(section)))` when the section agreed no format for `to`'s codec.
  Until the switch, the down-track keeps both publications' H.264 parameter-set state, and a
  repair looks in both rings. Unpublishing either one settles the down-track on the other.
- **SRs across a rebind.** RTP-11 projects a switch through two SRs only when they come from
  one sender clock, and the caller vouches for that. Only one publisher's own SRs share a
  clock, so the router presents a pending publication's SRs to the down-track only when the
  same participant publishes the current one. Across publishers, the switch uses the arrival
  rule.
- **Collision restart.** After a collision (RTC-4), the session queues the down-track
  (`peer.collided`). The router drains that queue after each `peer.receive`. It clears the
  down-track's parameter sets and any rebind in flight, then applies its target again, which
  asks for a keyframe. The fresh `Rewrite` starts at a random sequence number and timestamp,
  so the new SSRC's stream begins at that keyframe.
- **Keyframe request buffer.** `peer.request_keyframe` builds a compound RR, SDES CNAME and
  PLI or FIR with the SRTCP trailer in a 320-byte buffer, allocated per request (off the
  forward path).
- **Pacer.** Each session has one RTP-10 pacer of `pacer_packets` slots. Every packet to
  the subscriber on a section that agreed TWCC goes through it. A section without TWCC
  feeds no estimate, so the 300 kbps start rate would hold for good: its packets go
  straight to `out`, as RTC-5 step 2 allows. Paced packets go as media on the track's class (Audio, which leaves
  whatever the debt, or Video) and RTX repairs as Retransmit. Each slot owns a 1500-byte
  payload buffer, reserved at `open`, so pacing allocates nothing. A queued entry holds the
  payload as the leg sends it (descriptor rewritten, an RTX repair's OSN first) and the
  header fields. The header, with the next transport number of the session's TWCC
  (RTP-8), is written when the pacer lets the packet out, and then the packet is sealed.
  The rate is 2.5 times the BWE target (libwebrtc's pace multiplier), raised so a queue
  drains within 2 s. A packet the full pacer refuses is dropped and counted `paced`. Its
  outbound number stays used, so the subscriber's NACK repairs it from the history. The
  session drains its pacer after each push and at each `poll`, and `poll`'s wake includes
  the pacer's next send. A let-out packet that cannot be queued (a full outbox) is dropped
  and counted `capacity`, and the drain goes on. `unpace` takes a down-track's entries out when the router ends
  the track and at a collision. An entry whose SSRC a collision has retired is dropped
  when it is let out. S4 does not probe and sends no padding.
- **Not closed in S4.** AV1 routing needs a dependency-descriptor kind in `rtp.ext` (an rtp
  amend). W9 waits for captured or reference simulcast and SVC packets. The re-offer after an
  SSRC collision (RTC-4) waits for the renegotiation path. Each is an open red on S4's ticket.

**S5 decisions.** Gaps the spec left open, decided while S5 was built
(`core/programs/webrtc_sfu/units.hirsel`, `router_fence.hirsel`):

- **Control unit.** Unit 0 is the control unit, and it is also a media unit. It makes
  every control call: `units.begin` starts one, `units.finished` answers it once every unit
  has acknowledged, and `units.call` blocks on both over the unit's socket. One call is in
  flight at a time.
- **Apply order.** A change is applied home unit first, then to the others. A change the
  home unit refuses goes no further. `Event.Fenced` is raised on the control unit once a
  revoke has returned.
- **Sessions.** An opened `PeerSession` is parked on its home unit (`units.park`), and
  `Admit` moves it into the room. An admission that is refused, for example `Fenced`,
  unparks the session, so a refused join holds nothing. S6 and S7 build sessions inside
  their unit from cross-unit inputs. Join-from-offer has no owner yet: S7 decides it.
- **One outbox, no separate suffix.** A unit keeps one persistent outbox. `udp_send`
  commits a prefix, and the rest stays queued as the uncommitted suffix. A purge runs at
  `apply`, at the control drain and at `ready` (the send step). Every queued datagram's tag is
  rechecked there against the fence, so nothing can survive in a copy outside the queue.
- **Layers behind the send-step purge.** The recheck of a `Media` or `RepairReply` against
  `src` and `dst`, the `RepairAsk` refusal for a fenced publisher, and the pacer's bar on a
  rebound-away publisher's entries each repeat a removal the send-step purge already makes.
  W14 and W15 stay green with any one of them removed (the S5 report lists the seeded runs).
  They stay as second layers, and no law claims them.
- **Demux.** A unit resolves a datagram by its 5-tuple row, and a STUN check by its named
  ufrag among admitted members (`named`). Parked sessions are never consulted.
- **Messages.** `Media` carries the publisher's plaintext once per remote unit with a
  down-track of the publication, shared between those units. It carries no speaking flag
  until S6. `RepairAsk` goes to the publication's home unit, and `RepairReply` comes back
  with the ring's payload. A unit counts each kind it takes (`UnitCounts`).
- **Doorbells.** After a send step, a unit sends one empty datagram to each unit it posted
  to. A doorbell from an address that is not a sibling's is counted `forged` and ignored.
- **Test seam.** `units.seam(u, limit, tapping)` caps each `udp_send` at `limit` datagrams,
  which makes a partial commit. With `tapping`, the unit records every committed datagram,
  with its tag and bytes. In the laws, all units run as values in one task.
- **Real sockets.** `units_on_loopback` runs three units and two browsers over
  127.0.0.1 sockets bound at port 0. The units' doors name the addresses the binds return.
- **Not built in S5.** `close_room` across units is not built.

**Follow-ups this spec names.**

- **F1** (runner): on main. `room_client` is a Hirsel-side `hirsel.4` room client, so a Hirsel
  service is a room peer and feeds `RoomIn`, including R1's Part head.
- **F2** (SDK): on main. `LiveRoom` exposes the roster's incarnation and revision
  (sdk/client/room.ts, [sdk.md](sdk.md#rooms)), with room calls, unicast `Message`, the
  roster and state.
- **F3** is [turn.md](turn.md): TURN client and relay, long-term credentials, the stream fallback.
- **Ownership:** the active-speaker detector is this package's (RTC-6), not the rtp lane's.
  The rtp lane parses the RFC 6464 extension.

**Handoff after review 2336.** All three items are done in [rtp.md](rtp.md):

- RTP-11's switch timeline projects capture time into the old source's clock, subtracts
  that source's current offset, clamps, and fixes the new offset at the accepted frame
  start. The no-SR fallback advances by elapsed arrival time at the clock rate (RTC-6).
- RTP-7's egress history record: an entry maps the extended output index to the source
  generation, the ingress plaintext handle and the original output rewrite (sequence number,
  timestamp, marker, payload type, MID, extension IDs, codec counters). The history is gated
  at each source switch (RTC-4).
- RTP-6's `Keyframes`: an in-place interval update that keeps the outstanding request and
  the FIR command sequence number (RTC-6).

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) and LiveKit (/tmp/ref-livekit at `d210b4d`), with str0m
(/tmp/ref-str0m) consulted for the sans-IO shape. Review 2333 added mediasoup
(/tmp/ref-mediasoup at `90ca2ae`) and Janus (/tmp/ref-janus at `2f1e1ca`) for repair,
switching and timelines.

| Source | Mechanism | Verdict |
| --- | --- | --- |
| Goose | Networking is library code over explicit costs, not a language form (goose docs/stdlib.md, as [HTTP](http.md#goose-and-goose-http) records) | **ADOPTED.** The SFU is a package with explicit bounds and charges. Goose has no media, UDP or WebRTC code to compare beyond that. |
| Goose | Limits as counts per worker | **ADOPTED** as `RoomLimits` fixed at `open_room`, refused, never evicted |
| LiveKit | Two PeerConnections per participant, one for publishing and one for subscribing (pkg/rtc) | **REPLACED** by one bundled session per participant. One ICE, DTLS and SRTP setup, one 5-tuple and one keepalive. Glare is settled by the impolite-server rule, which LiveKit avoids by separating the directions. |
| LiveKit | Dynacast: subscribed max quality per track, upgrades at once, downgrades after `DynacastPauseDelay` 5 s (pkg/rtc/dynacast/dynacastmanagervideo.go, pkg/config/config.go) | **ADOPTED**, made generic: `Target` is an encoding and layer, not a named quality, and the mask goes to the publisher as a signal. |
| LiveKit | Audio level: 400 ms window, -35 dBov threshold, 40 % activity, two-interval EMA (pkg/sfu/audio/audiolevel.go) | **ADAPTED** to exact integer arithmetic (RTC-6), so replay and every engine agree bit for bit. |
| LiveKit | Forwarder with simulcast switching at keyframes and DD-driven SVC selection (pkg/sfu/forwarder.go, pkg/sfu/videolayerselector) | **ADOPTED** for switch points. Its dependency-descriptor selector is W9's cross-check for AV1 decode-target indexes. |
| LiveKit and mediasoup | Late and repaired packets keep their source position: LiveKit's munger returns the earlier mapped sequence and refuses an unavailable mapping (pkg/sfu/rtpmunger.go); mediasoup's `SeqManager` maps input plus base and advances its high-water mark only for newer output, and restores each consumer's header before RTX (worker/src/RTC/SeqManager.cpp, RTP/RtpStreamSend.cpp) | **ADAPTED.** RTC-4 keeps source order and sends a late packet as RFC 4588 RTX, since TLS-8's sealer cannot seal below its highest index. The egress history records each subscriber's own rewrite, as mediasoup restores it. |
| LiveKit | Bandwidth pause: a track the estimate cannot carry at its lowest layer is paused (pkg/sfu/forwarder.go) | **ADOPTED** in RTC-6's allocation, which starts every video down-track at `Off`. |
| Janus | Waits for a keyframe of the candidate substream before switching, and rebases sequence and timestamp from elapsed time when it has no SR (src/rtp.c) | **ADOPTED** as RTC-6's pending-source presentation and its no-SR fallback. |
| LiveKit | NACK answered from a sequencer by resending on the media SSRC or RTX | **REPLACED** by RTX only (RTC-4). TLS-8 never seals twice and never seals below the highest index, so a media-SSRC resend would need sealed copies per subscriber. |
| LiveKit | Single UDP port with a mux across the node, sessions found by ufrag then 5-tuple | **REPLACED** by one port per unit, so the home unit is a signaling choice and the kernel needs no steering. |
| LiveKit | Kick through its room service: remove participant, close its PeerConnection | **REPLACED** by RTC-7's fence. LiveKit's removal is asynchronous and gives no point after which no packet is forwarded. |
| str0m | Sans-IO `Rtc` with `handle_input` and `poll_output`, one per peer | **ADOPTED** as `receive` and `poll` |
| str0m and LiveKit | SCTP data channels | **NOT TAKEN** for Wave 1. Application data goes over `runner.room` or WebTransport (Wave 2). |
