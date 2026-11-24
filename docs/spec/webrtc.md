# WebRTC sessions and the SFU router

Version note (2026-11-24, media-webrtc-sfu-spec-2319): first text of RTC-1 to RTC-8, Wave 1.6
of the real-time media track. Not yet built; the fences are `unbuilt`. The package is
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
| SDP certificate fingerprint from `x509_build`, negotiated DTLS roles | RTC-3 DTLS (one certificate per unit, `expect` pinning); RTC-1 and RTC-2 (`setup:passive` in answers, `actpass` in server offers, roles never change) |
| Independent SRTP in and out keys, SSRC, ROC, replay window | dtls.md SRTP-2 and SRTP-3; RTC-3 authenticates before state (W19); RTC-4 keeps one SSRC per down-track |
| BUNDLE with RTP/RTCP demux | RTC-1 BUNDLE rules, RTC-3 `classify` and MID, then SSRC, then payload-type mapping (W3, W4) |
| Negotiated MID, RID, extmap, payload types; Opus, VP8 and H.264 passed through, no server decode | RTC-1 and RTC-2 (offer's payload types and IDs kept, W1); RTC-5 forwards payloads as bytes; VP8 descriptor is slice S2 |
| SR/RR, NACK/RTX, PLI/FIR coalescing, TWCC, pacing | rtp RTP-4 to RTP-10; RTC-4 (NACK answered on RTX only), RTC-5 (RTCP terminated), RTC-6 keyframe requests (W10) |
| Per-subscriber sequence and timestamp continuity | rtp RTP-11 `Rewrite` per down-track; RTC-4 (one SSRC for life, W7) |
| Keyframe-safe simulcast and SVC switching | RTC-6 mapping onto RTP-11 (W9) |
| RFC 6464 audio level forwarded, for active speakers | RTC-6 active speakers (W12) and audio forwarding with the extension rewritten (W13) |
| Generic dynacast: per-subscriber `Off` or a layer per video publication; unselected publisher layers paused | RTC-6 `Target`, the wanted mask and `Event.Layers` (W11) |
| `revoke(room_id, participant_id, epoch)`, one fence per participant | RTC-7, `revoke(r, room, participant, epoch)` |
| Synchronously stops ingress and egress, cancels queued RTX, invalidates DTLS and ICE | RTC-7 steps 1 to 4 (W14, W15) |
| Ack only once nothing more can be forwarded for the revoked epoch | RTC-7 acknowledgement: after every unit applies the fence, before any later `udp_send` (W14) |
| TURN over TLS, or ICE-TCP, for strict firewalls | RTC-8 passive ICE-TCP (slice S8, W17); TURN over TLS is follow-up F3 for `stun_ice` |
| runner.room: unicast, membership revision, targeted kick, expiry, SDK exposure | RTC-8's table: on main except the Hirsel room client (F1) and the SDK room API (F2) |
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
pub type Extmap = Extmap(id: u8, ext: Ext)
pub type RidDir = RidSend | RidRecv
pub type Rid = Rid(id: Inline[u8, 16], len: u8, dir: RidDir)
pub type Section = Section(media: Media, port: u16, mid: Inline[u8, 16], mid_len: u8, dir: Direction, formats: List[Format], extmaps: List[Extmap], rids: List[Rid], simulcast: List[u8], ssrcs: List[u32], rtx_pairs: List[(u32, u32)], msid: Bytes)
pub type Description = Description(origin_session: u64, origin_version: u64, bundle: List[u8], ice_ufrag: Bytes, ice_pwd: Bytes, ice_lite: bool, fingerprint: Inline[u8, 32], setup: Setup, sections: List[Section], candidates: List[Bytes])
pub type SdpError = TooLarge | Malformed(u64) | NoBundle | NotBundled(u8) | NoRtcpMux(u8) | NoMid(u8) | NoFingerprint | WrongFingerprint | NoIceCredentials | ExtmapConflict(u8) | PayloadConflict(u8) | TooManySections | TooManyFormats(u8) | TooManyRids(u8) | BadRid(u8) | Capacity(Capacity)

pub fn parse(text: Bytes, mut out: Description) -> Result[(), SdpError]
pub fn write(d: Description, mut out: Bytes) -> Result[u64, SdpError]
```

`parse` reads RFC 8866 SDP as JSEP ([RFC 8829](https://www.rfc-editor.org/rfc/rfc8829))
produces it, into a caller-owned `Description` whose lists it clears and refills, so a
session reuses one description's storage. It is a line parser over `CRLF` or bare `LF` lines,
with no backtracking.

**Bounds.** These are explicit refusals, never truncation:

- `TooLarge`: the text is longer than 32,000 bytes. A room message is at most 32,768 bytes
  (`MESSAGE_MAX`, core/programs/runner/runner-room.hirsel), and RTC-8's envelope takes at
  most 768 of the rest.
- `TooManySections`: more than 64 m-sections.
- `TooManyFormats(i)`: more than 32 formats in section `i`.
- `TooManyRids(i)`: more than 4 RIDs.

**Attributes read.** `v`, `o`, `s`, `t`, `c`, `m` and these `a=` attributes: `group:BUNDLE`,
`ice-ufrag`, `ice-pwd`, `ice-lite`, `ice-options`, `fingerprint`, `setup`, `mid`, `rtcp-mux`,
`rtcp-rsize`, `extmap` (with its optional direction), `sendrecv`, `sendonly`, `recvonly`,
`inactive`, `msid`, `ssrc` (only the `cname` and `msid` forms), `ssrc-group:FID`, `rtpmap`,
`fmtp`, `rtcp-fb`, `rid`, `simulcast`, `candidate` and `end-of-candidates`. Session-level
`ice-ufrag`, `ice-pwd`, `fingerprint` and `setup` apply to every section that lacks its own.
Any other attribute is skipped, as RFC 8866 §5.13 requires of an unknown attribute. An
extension URI or codec name outside `Ext` or `Codec` is skipped with its payload type or ID.
The answer never accepts it, so skipping it changes no negotiated behaviour.

**Codec names.** `opus/48000/2`, `VP8/90000`, `VP9/90000`, `H264/90000`, `AV1/90000`,
`rtx/90000`, `red/48000/2` and `ulpfec/90000`, compared ASCII case-insensitively
(RFC 8866 §6.6). The extension URIs are those of RFC 8843 (`mid`), RFC 8852 (`rtp-stream-id`,
`repaired-rtp-stream-id`), RFC 6464 (`ssrc-audio-level`), `abs-send-time`,
`transport-wide-cc-extensions-01` and the AV1 RTP specification's `dependency-descriptor`.

**Fingerprint.** Only `sha-256` is read. A description whose fingerprints are all of other
hashes is `NoFingerprint`. RFC 8122 §5 lets an endpoint offer several. Every browser offers
`sha-256`, and a weaker hash is not worth the second code path.

**The BUNDLE rules ([RFC 8843](https://www.rfc-editor.org/rfc/rfc8843)).** These are
checked at parse, because a description that breaks them cannot be answered:

- a `group:BUNDLE` line names every section with a nonzero port (`NoBundle` when there is no
  group, `NotBundled(i)` for a section outside it);
- every bundled RTP section has `rtcp-mux` (`NoRtcpMux(i)`, [RFC 8858](https://www.rfc-editor.org/rfc/rfc8858)) and a `mid`;
- one extension URI has one ID in every bundled section, and one ID one URI
  (`ExtmapConflict(id)`, RFC 8843 §9.2);
- one payload type is one codec configuration in every bundled section where it appears
  (`PayloadConflict(pt)`). RFC 8843 §9.1 allows a payload type to differ when MID demux
  separates the sections. This parser refuses the difference anyway. No browser emits it,
  and refusing it lets RTC-3 demultiplex by payload type when a packet's MID extension is
  absent.

**RID and simulcast ([RFC 8851](https://www.rfc-editor.org/rfc/rfc8851),
[RFC 8853](https://www.rfc-editor.org/rfc/rfc8853)).** A RID is 1 to 16 ASCII
alphanumeric, `-` or `_` bytes (`BadRid(i)` otherwise). `simulcast` holds the indexes into
`rids` in the order of the `a=simulcast` line's first list. Alternatives (`,`) and paused
markers (`~`) are refused as `BadRid(i)`. Browsers emit neither, and the server would have to
track a stream it cannot see. A section with RIDs and no `a=simulcast` line is refused the
same way.

**Writing.** `write` emits the attributes above in RFC 8829 §5.2 order, one per line with
`CRLF`, and returns the bytes appended. `parse(write(d))` gives back `d` for every `d` that
`write` accepts. That is a property law of the writer. It is not the oracle for what the
writer must say (RTC-2).

## RTC-2: Offer and answer

```hirsel unbuilt
pub type Policy = Policy(audio: List[Codec], video: List[Codec], exts: List[Ext], max_sections: u8, simulcast: bool)
pub type Negotiated = Negotiated(remote_ufrag: Bytes, remote_pwd: Bytes, remote_fingerprint: Inline[u8, 32], sections: List[Agreed])
pub type Agreed = Agreed(index: u8, mid: Inline[u8, 16], mid_len: u8, media: Media, recv: bool, send: bool, formats: List[Format], extmaps: List[Extmap], rids: List[Rid])
pub type Signaling = Stable | HaveLocalOffer(u64) | HaveRemoteOffer(u64)

pub fn answer(policy: Policy, offer: Description, local: LocalEnd, mut out: Description, mut agreed: Negotiated) -> Result[(), SdpError]
pub fn offer(policy: Policy, current: Negotiated, local: LocalEnd, adds: List[SendSection], mut out: Description) -> Result[(), SdpError]
pub fn accept_answer(current: Negotiated, sent: Description, answer: Description, mut agreed: Negotiated) -> Result[(), SdpError]
```

`LocalEnd` is the server's half: its ICE ufrag and password (fresh per PeerSession,
RTC-3), the SHA-256 fingerprint of its home unit's certificate, and the home unit's one
host candidate. `SendSection` is a down-track the server adds (RTC-5).

**`answer`.** The answer mirrors the offer's sections in order. For each section:

1. `Application` (SCTP data channels) is rejected with port 0. Data channels are not in Wave 1
   (prior art below), and RFC 8829 §5.3.1 rejects a section by its port.
2. The direction is the offer's mirror, restricted by what the server does: it receives on an
   offered `sendrecv` or `sendonly` section and sends on an offered `recvonly` section, or on
   one it created with `offer`. A `sendrecv` the client opens is answered `recvonly`.
3. Formats are the offer's formats whose codec the policy lists, in the offer's order, with
   their payload types, `fmtp` and `rtcp-fb` kept as offered. The answerer keeps the
   offerer's payload types (RFC 8829 §5.3.1). H.264 formats are kept only with
   `packetization-mode=1`, since FU-A and STAP-A are what `media_bits` reads. An `Rtx`
   format is kept when its `apt` format is kept. A section left with no media format is
   rejected with port 0.
4. Extmaps are the offer's for extensions the policy lists, with the offer's IDs (RFC 8285
   §6). `Mid` is required, and a section without it was refused at parse.
5. RIDs are answered with the direction flipped, in the same order, and `a=simulcast:recv`
   with the same list (RFC 8853 §5.3). With `policy.simulcast` false the RID lines are
   dropped and the section takes only its first encoding.
6. Every kept section names the server's ICE credentials, `ice-lite`, `setup:passive`, the
   fingerprint and, in the first bundled section, the candidate and `end-of-candidates`.

The BUNDLE group lists the kept sections' MIDs in the offer's order. `agreed` records the
result and is what RTC-3 and RTC-5 read.

**`offer`.** After the first exchange the server offers to add or remove down-tracks
(RTC-5). It reuses every existing section in place (RFC 8829 §5.2.2) and appends one
`recvonly`-for-the-client section per added down-track, with a new MID drawn from the
session's counter (`"s0"`, `"s1"`, ...), the first matching format from the publication's
agreed formats, and one SSRC and one RTX SSRC (RTC-4). It marks a removed down-track's section
`inactive` and reuses it for a later down-track of the same media kind, keeping its MID.
A server offer carries `a=setup:actpass` as RFC 8842 §5.5 requires of every offer. Its ICE
credentials, fingerprint and candidate are those of the current session, and none of them
changes. `accept_answer` then requires the answer's `a=setup:active`, which keeps the roles
of the association already running, and refuses anything else as `Malformed`. A server
offer never restarts ICE or DTLS. A participant that needs either rejoins (RTC-7).

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
in one round trip.

**Oracle for answers.** The expected answer for each law is written by hand from RFC 8829
§5.3 and the rules above, against an offer captured from a real browser. A round trip
through the writer is never the oracle.

## RTC-3: PeerSession

```hirsel unbuilt
pub opaque type PeerSession
pub type PeerLimits = PeerLimits(sections: u8, down_tracks: u16, rtx_bytes: u64, pacer_packets: u32)
pub type PeerError = Sdp(SdpError) | Fenced | Closed | Ice(stun_ice.IceError) | Dtls(DtlsError) | Srtp(SrtpError) | Rtp(RtpError) | Capacity(Capacity)
pub type Class = Stun | Dtls | Rtp | Rtcp | Turn | Other
pub type Input = Nothing | Replied | Media(MediaIn) | Control(ControlIn)
pub type Poll = Transmit | WakeAt(u64) | Idle
pub type Outbox = Outbox(data: Bytes, pkts: List[UdpTx])

pub fn classify(first: u8) -> Class
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

**Demultiplexing ([RFC 9443](https://www.rfc-editor.org/rfc/rfc9443) §7).**
`classify` reads the first byte:

| First byte | Class |
| --- | --- |
| 0 to 3 | `Stun` |
| 20 to 63 | `Dtls` |
| 64 to 79 | `Turn` |
| 128 to 191 | `Rtp` or `Rtcp`, split by the second byte: 192 to 223 is `Rtcp` (RFC 5761 §4) |
| any other | `Other` |

`Turn` and `Other` datagrams are dropped and counted. The server is no TURN server, and
16 to 19 (ZRTP) is not spoken. One comparison chain on one byte, then one on the second.

**Ingress authority.** A datagram reaches a PeerSession only from its **selected path**,
except STUN. A STUN Binding request goes to `stun_ice.lite_receive`, which checks
`MESSAGE-INTEGRITY` under this session's password and the `USERNAME` against its ufrag. A
request that fails is answered with nothing. One that passes may move the selected path
(`lite_selected`, NAT rebinding). The home unit keeps a table from remote 5-tuple to session,
which only that move changes. A DTLS, RTP or RTCP datagram from a 5-tuple outside the table
reaches no session and is dropped and counted. A source address is unauthenticated (NET-15).
The ICE password is what binds a path to the participant who received it in the answer.

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
and extensions in place (rtp lane), maps the packet to an agreed section by its MID
extension, or by SSRC once the MID has been seen on it (RFC 8843 §9.2), or by unique payload
type, and to an encoding by RID or the RTX pairing. It returns
`Media(MediaIn(section, encoding, header, payload_at, payload_len, audio_level, layer))`,
with views into `slot`. `layer` is the codec's layer and keyframe facts from `media_bits`. A
packet whose MID names no receiving section is dropped and counted. RTCP is unprotected and
parsed the same way, and its feedback returns as `Control`.

**Authentication comes before state.** Before SRTP's `open` succeeds, the session may run
only the rtp lane's stateless readers (`header.parse`, `ext.find`, `rtcp.next` and the value
readers), because SRTP needs them to find the header and estimate the index. Every stateful
operation (statistics, loss tracking, NACK, TWCC, `History`, `Rewrite`, BWE, the
active-speaker score of RTC-6) is called only with the extended index that `open`
authenticated, and only after it succeeded. A packet that fails `open`, or fails the replay
window, changes no state anywhere in the stack except a drop counter. The 5-tuple table is
the one exception with its own gate: only a STUN request that passed `MESSAGE-INTEGRITY`
under the session's ICE password changes it. The rtp lane states this rule (its Boundaries
section) and leaves its law here (W19).

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

- **One SSRC per down-track for the session's life.** When a down-track is created (RTC-5), the
  session draws its media SSRC and, for video, its RTX SSRC from a per-session generator. The
  generator never repeats a value in the session and never returns an SSRC that the remote
  end uses, and it issues one sealer for each. Switching layers, going `Off` and back to a
  layer, and rebinding the down-track to another publication all keep the SSRC and its sealer.
  The rtp lane's rewriting keeps the sequence number and timestamp continuous across each, so
  the index keeps rising. A participant that rejoins after a revoke gets a new session with a
  new DTLS association, so the epoch is new and every SSRC is free again.
- **Late packets go out as repair, never on the media SSRC.** The rewriter assigns outbound
  sequence numbers in source order and leaves a gap for a source packet that has not arrived.
  When that packet then arrives late, its outbound number is below the sealer's highest, and
  the rtp lane's `Rewrite.on_packet` answers `Late(index, ts)` (RTP-11). It is
  sent at once on the down-track's RTX SSRC as an RFC 4588 retransmission, with the outbound
  number as its OSN, and sealed under the RTX sealer at the RTX stream's next index. A
  down-track without negotiated RTX drops it and counts it. Audio sections never negotiate
  RTX, and Opus concealment covers the loss.
- **NACK repair is RTX only.** A NACK from a subscriber is answered from the rtp lane's RTX ring
  on the RTX SSRC. A subscriber that negotiated no RTX for the format gets no answer, and the
  NACK is counted. Its loss is covered by a keyframe request when the rtp lane's rules call for
  one. Caching sealed media bytes per subscriber to resend them would cost a copy of every
  packet for every subscriber, so it is not done.
- **The server's own RTCP.** Each session issues one `Rtcp` sealer for its single RTCP sender
  SSRC, drawn from the same generator. Every RR, SR, NACK, PLI, FIR and TWCC packet it sends
  uses it. SRTCP's 31-bit index rises once per packet.

`offer` (RTC-2) signals each down-track's two SSRCs with `a=ssrc` and `a=ssrc-group:FID`.
A browser also accepts unsignaled SSRCs by MID, but signaling them pins the RTX pairing.

## RTC-5: Router, rooms and fan-out

```hirsel unbuilt
pub opaque type Router
pub type RoomId = RoomId(u64)
pub type Participant = Participant(room: RoomId, id: u64, epoch: u64)
pub type PublicationId = PublicationId(u64)
pub type Target = Off | Layer(encoding: u8, spatial: u8, temporal: u8)
pub type RoomLimits = RoomLimits(participants: u16, publications: u16, down_tracks: u32, audio_forward: u8, pause_delay_ms: u32)
pub type RouterError = NoRoom | NoParticipant | NoPublication | Fenced | Exists | Limit(u32) | Peer(PeerError) | Capacity(Capacity)
pub type Event = Answer(Participant, Bytes) | Offer(Participant, u64, Bytes) | Glare(Participant, u64) | Published(PublicationId, Participant, Media, u8) | Unpublished(PublicationId) | Layers(PublicationId, u8) | Speakers(RoomId, Inline[u64, 8], u8) | Fenced(Participant)

pub fn router(units: u16, unit: u16, mut inbox: Inbox[RouterIn], peers: List[Sender[RouterIn]]) -> Result[Router, RouterError]
pub fn open_room(mut r: Router, room: RoomId, limits: RoomLimits) -> Result[(), RouterError] waits
pub fn join(mut r: Router, who: Participant, publish: bool, offer: Bytes) -> Result[(), RouterError] waits
pub fn signal(mut r: Router, who: Participant, kind: SignalIn) -> Result[(), RouterError] waits
pub fn subscribe(mut r: Router, who: Participant, p: PublicationId, target: Target) -> Result[(), RouterError] waits
pub fn set_target(mut r: Router, who: Participant, p: PublicationId, target: Target) -> Result[(), RouterError] waits
pub fn unsubscribe(mut r: Router, who: Participant, p: PublicationId) -> Result[(), RouterError] waits
pub fn leave(mut r: Router, who: Participant) -> Result[(), RouterError] waits
pub fn revoke(mut r: Router, room: RoomId, participant: u64, epoch: u64) -> Result[(), RouterError] waits
pub fn close_room(mut r: Router, room: RoomId) -> Result[(), RouterError] waits
pub fn turn(mut r: Router, now: u64, mut sock: UdpSocket, mut rx: Bytes, mut batch: List[UdpPacket], mut out: Outbox, mut events: List[Event]) -> Result[u64, RouterError] waits
```

**Identity.** A participant is `(room, id, epoch)`. `id` is the caller's: RTC-8 uses the
`runner.room` peer. `epoch` is a value the caller raises each time `id` is admitted again.
The router never compares `epoch` with anything but itself (RTC-7). A publication is one
track a participant sends: one agreed receiving section with its encodings. A **down-track**
is one subscriber's copy of one publication, bound to one sending section of the subscriber's
session.

**Units and the control order.** Every media unit runs one `Router` over its own socket. The
unit that runs the signaling adapter (RTC-8) is the **control unit**. Every control call
(`open_room` to `close_room`) is made on it. It applies the call to its own state, sends the
change as one `RouterIn` message to every other unit, in one order, and returns when the units
the change touches have applied it. Each unit takes its inbox's control messages at the start
of every `turn`, before it reads its socket. So every unit sees one order of control changes,
and a change is in force on a unit from its first turn after the message.

**A turn.** `turn` drains the control inbox, then the cross-unit media inbox (below), then
one `udp_recv` batch from the socket. It runs each datagram through its session (RTC-3), routes
each `Media` (below), drives `poll` for sessions whose deadline has passed, and ends with one
`udp_send` of `out`. It appends signaling and dynacast output to `events`, which the
adapter sends (RTC-8). It returns the earliest deadline. A unit's loop is
`turn` and then `udp_recv`'s wait until that deadline.

**Fan-out.** For a `Media` packet of publication `P`, encoding `e`:

1. The home unit decrypts once, in its receive slot (RTC-3).
2. For each local down-track of `P` whose **current** forwarding layer selects this packet
   (RTC-6), the rtp lane rewrites a scratch copy of the header: SSRC, sequence number,
   timestamp, extension IDs to the subscriber's negotiated IDs, and the codec descriptor's
   picture ID or frame number through `media_bits`. It then calls TLS-8 `seal_into` with the
   scratch header and the shared payload as source and the subscriber's span of `out.data` as
   destination. One copy, made by the cipher. Then one `UdpTx`, sent through the
   subscriber's pacer when the rtp lane paces that down-track.
3. For each other unit with at least one down-track of `P`, the home unit posts one
   `Media(P, epoch, Shared[Bytes])` message. The `Shared[Bytes]` holds the plaintext packet,
   and every remote unit runs step 2 from it. One plaintext copy per remote unit, whatever
   its number of subscribers.

The home unit keeps, per publication, the set of units with down-tracks (a `u64` bitmap, so
at most 64 units). The RTX ring keeps the plaintext packet as the rtp lane specifies, and
only on the home unit. A remote unit's NACK is answered by the remote unit asking the home unit
over the inbox, and the home unit replies with the shared packet. Only units with a
subscriber pay anything for a publication.

**RTCP is terminated.** The router never forwards a subscriber's RR, NACK or TWCC to a
publisher. Each session answers its own feedback (RTC-4) and estimates its own bandwidth
(rtp lane). Publisher keyframe requests are the router's (RTC-6). Sender reports to a
subscriber are generated per down-track from the publisher's SR, with the rewritten
timestamps (rtp lane).

**Bounds.** `RoomLimits` fixes every table's size when the room opens, so routing allocates
nothing:

- `participants` is at most 256, the `runner.room` peer limit (`PEERS_MAX`);
- `publications` is at most 8 per participant;
- `down_tracks` is at most 64 per subscriber;
- encodings are at most 4 per publication (RTC-1).

A call that would pass a bound is `Limit(bound)`, and nothing changes.

## RTC-6: Layers, dynacast and active speakers

**Layer identity.** A packet's layer is `(encoding, spatial, temporal)`:

- `encoding` is its RID's index in the publication's simulcast list (0 without simulcast);
- `spatial` and `temporal` come from the AV1 Dependency Descriptor, the VP9 payload
  descriptor or the VP8 `TID` (`media_bits`), and are 0 when the codec carries none.

`Target = Off | Layer(e, s, t)` asks for encoding `e` with every spatial layer up to `s` and
every temporal layer up to `t`. `t = 255` means every temporal layer. This is the
generic dynacast selector. The subscriber's application decides why it wants a layer. The
router never knows.

**Mapping onto `rtp`'s rewrite.** The switching itself is the rtp lane's `Rewrite`
(RTP-11), one per down-track. The router translates:

- `Off` is `rewrite.target(Off)`;
- `Layer(e, s, t)` is `rewrite.target(Layer(source, i))`, where `source` is the router's key
  for encoding `e` of the publication, and `i` is the decode-target index of `(s, t)`. For an
  AV1 Dependency Descriptor, `i` is the index the template structure gives the target with
  spatial id `s` and temporal id `t` (`media_bits` `decode_target`), clamped to the
  highest the structure has. For VP8, VP9 and H.264, `i = s · T + t`, where `T` is the
  number of temporal layers the stream has shown, and `t` is clamped to `T − 1`;
- for each packet the router fills rtp's `Unit` from `media_bits`: `start` and `end` from
  the descriptor or the marker bit, `keyframe` from the codec's keyframe test, and `needs`
  and `switches` by RTP-11's rules (Dependency Descriptor indications; for VP8 and VP9, the
  temporal id and the `Y` or `U` switching-up bit).

So the switch points below are RTP-11's rules restated for the router's targets, not a
second implementation. `Rewrite` answers `Wait` while it has no current layer, and the
router then asks for a keyframe.

**Selection per down-track.** Each down-track keeps `target`, the layer the subscriber asked
for, and `current`, the layer it forwards. The **effective target** is the target, lowered
when the subscriber's bandwidth estimate (rtp lane) cannot carry it, as follows. Each turn
that the estimate changes, the router sorts the subscriber's video down-tracks by
publication ID. It starts each at its lowest encoding at or below target, then raises them one
step at a time in that order, round-robin, while the sum of each encoding's measured ingress
rate stays within the estimate. Audio is never lowered. This is deterministic and computable
by hand.

`current` moves to the effective target only at a point where the subscriber can decode
the switch:

- **Simulcast** (changing `encoding`): at the first keyframe of the new encoding. On a change
  of effective target the router asks the publisher for a keyframe of the new encoding
  (below), and it keeps forwarding the old encoding until one arrives. Going to `Off` takes
  effect at once.
- **Spatial** (AV1 DD, VP9): up only at a frame whose decode target indication for the new
  target is `Switch` or `Required` with a keyframe, down at the next frame start.
- **Temporal**: up at a frame whose temporal layer is a switching point, for VP8 its
  `Y` bit and for VP9 its `U` bit. Down at the next frame start.

A frame is forwarded whole or not at all. Packets of a frame that began before a switch go to
the old layer's rule.

**Keyframe requests.** A down-track that needs a keyframe (a switch, a subscriber's PLI or
FIR, a `Wait` from its rewrite, or rtp's `Losses` answering `Reset`) calls `want` on its
publication's encoding's rtp `Keyframes` (RTP-6), which the home unit keeps, one per
encoding. The router makes each `Keyframes` with `interval_ns = max(100 ms, rtt)`, where
`rtt` is the publisher session's round-trip estimate at that time, and remakes it when the
estimate moves by more than a quarter. `Keyframes` coalesces, so N subscribers asking at once
cost one request. The kind is `Pli`, or `Fir` for a publisher that negotiated `ccm fir` and
not `pli`. The router calls `keyframe_seen` at each keyframe it reads.

**Publisher pause (dynacast).** For each publication the router computes the **wanted
mask**: bit `e` is set when some down-track's effective target or current layer uses encoding
`e`. A bit that turns on is published at once. A bit that turns off is published only after
it has stayed off for `pause_delay_ms` (default 5,000). A target that comes back inside the
delay costs no restart. Each published change is `Event.Layers(publication, mask)`, which the
adapter sends to the publisher (RTC-8), so its encoder stops the unwanted encodings. Packets of
an encoding outside the mask are never forwarded, whether or not the publisher has stopped
it yet.

**Active speakers.** For each audio publication that negotiated the RFC 6464 extension, the
home unit keeps a score in integer arithmetic. This is LiveKit's algorithm
(pkg/sfu/audio/audiolevel.go in /tmp/ref-livekit at `d210b4d`) with its float steps made exact:

- `amp(l) = round(65536 · 10^(-l / 20))` for levels `l` 0 to 127, a fixed table of 128 Q16 values;
- each packet adds its duration `d` (from the RTP timestamp step, at the clock rate) to
  `observed`, and when its level `l ≤ 35` (-35 dBov), adds `d` to `active` and lowers
  `loudest` to `l`;
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

The speakers are the speaking publications ranked by score, then by lower participant ID.
Each 400 ms in which the top 8 changed, the control unit emits
`Event.Speakers(room, top, count)`.

**Audio forwarding.** With `audio_forward = n > 0`, each subscriber's audio down-tracks
forward only the `n` highest-ranked speakers, excluding the subscriber itself. Other audio
down-tracks are held at `Off` until they rank again. Opus needs no keyframe, so the gate
switches at the next packet. The RFC 6464 extension is forwarded, rewritten to the
subscriber's ID. With `n = 0`, all audio is forwarded. A voice room of N speakers then costs
`N · n` audio packets per frame interval instead of `N · (N − 1)`.

## RTC-7: Revocation fence

`revoke(r, room, participant, epoch)` stops every forwarding that involves `participant` at
`epoch` or any lower epoch, on every unit, before it returns.

**Fence state.** Each unit keeps, per room, `fenced[participant]`: the highest revoked epoch,
initially absent. A unit that applies a revoke sets the entry to
`max(fenced[participant], epoch)`.

**What a unit does when it applies a revoke.** At the start of its turn, in control order,
it does each of these before it reads any datagram or media message:

1. **Ingress.** The participant's PeerSessions with epoch at or below the fence are closed
   (RTC-3) without the `close_notify` alert. Their 5-tuple rows go, so their datagrams reach no
   session. Their ICE credentials stop matching, so a Binding request under them gets no
   answer, and their DTLS and SRTP state is erased, so nothing they hold can be unprotected.
2. **Egress of their media.** Every down-track of their publications is removed. Every
   `UdpTx` not yet passed to `udp_send` that carries their media, in `out` or in a pacer queue,
   is cancelled. Their RTX ring entries, and every queued RTX answer drawn from them, are
   cancelled. These use the rtp lane's non-waiting close operations (RTP-12): `Rewrite.close`
   on each removed down-track, so every later `on_packet` answers `Drop`; `History.clear` on
   their publications' histories; and `Pacer.remove(stream)` on every pacer holding one of
   the streams, with the handed-back entries discarded.
3. **Egress to them.** Their down-tracks of other publications go with their sessions, in
   step 1, with every pacer and RTX entry of those sessions.
4. **In flight between units.** A `Media(P, epoch, ...)` message from the inbox whose
   publisher is fenced at that epoch is dropped when it is taken. The fence is set first, so
   a message posted before the revoke and taken after it is dropped.

**The acknowledgement.** The control unit applies the revoke, sends it to every other unit
(not only those it thinks are involved, since a subscription change can be in flight), and
returns `Ok(())` when every unit has acknowledged it. A unit acknowledges at the end of steps
1 to 4, before its turn's `udp_send`. So once `revoke` returns:

- no unit will pass to `udp_send` a datagram that carries media of the participant at a fenced
  epoch, or that is addressed to one of its fenced sessions;
- no datagram from the participant's fenced sessions will reach any routing step.

Datagrams already passed to `udp_send` have left the program. The fence is at the send, as
the kernel cannot recall a datagram. The router emits `Event.Fenced(participant)` after the
return. RTC-8's adapter turns it into the room message `Signal.Fenced(participant, epoch)`,
and that is what the controlling application observes.

**Rejoin.** `join` with `epoch ≤ fenced[participant]` is `Fenced`. A rejoin needs a higher
epoch, and it makes a new PeerSession with fresh ICE credentials, a new DTLS association and a
new SRTP schedule.

**Cost and liveness.** A revoke waits for at most one turn on each unit, plus one inbox hop.
A unit's turn is bounded by one `udp_recv` batch and its sessions' due timers, so the
fence's latency is bounded by the slowest unit's turn. No unit waits for another inside a
turn, so revokes cannot deadlock.

**Leave is not revoke.** `leave` closes the participant's sessions with `close_notify`,
removes its publications and drains nothing that is already queued. It is for ordinary
departures. Revocation is for authority withdrawn.

## RTC-8: Signaling through runner.room

The router takes signaling as calls and emits it as `Event`s. The `signal` module adapts
both to the generic `runner.room` (core/programs/runner/runner-room.hirsel): typed `State S`
per peer plus addressed `Message M`. The room's types are the application's, so the adapter is
generic over them:

```hirsel unbuilt
pub type Signal: codec = Offer(u64, Bytes) | Answer(u64, Bytes) | Candidate(Bytes) | EndOfCandidates | Glare(u64) | Subscribe(u64, Target) | Target(u64, Target) | Unsubscribe(u64) | Published(u64, u64, u8, u8) | Unpublished(u64) | Layers(u64, u8) | Speakers(Inline[u64, 8], u8) | Fenced(u64, u64) | Refused(u32)
pub type RoomIn[M] = Joined(u32) | Left(u32) | Message(u32, M) | Lost | Resynced
pub type RoomOut[M] = ToPeer(u32, M) | ToRoom(M)
pub type Embed[M] = Embed(wrap: own[send] fn(Signal) -> M, unwrap: own[send] fn(M) -> Option[Signal], publish: own[send] fn(u32) -> bool)

pub fn adapter[M: codec](room: RoomId, take embed: Embed[M]) -> Adapter[M]
pub fn room_in[M: codec](mut a: Adapter[M], mut r: Router, input: RoomIn[M], mut out: List[RoomOut[M]]) -> Result[(), RouterError] waits
pub fn events[M: codec](mut a: Adapter[M], events: List[Event], mut out: List[RoomOut[M]])
```

The service itself is one room peer, admitted by the application's room admission like any
other caller (`Grant` `Join`). The application wraps `Signal` in its own `M`
(`Embed.wrap`, `Embed.unwrap`), so the room carries one message type. `Embed.publish`
answers whether a peer may publish, and the adapter passes it to `join`. The application
decides it from its own state (for example the peer's `S`), and the router never reads `S`.

- **Identity.** A participant is the room peer that sent the message: `id` is its peer
  number, which the relay stamps and a peer cannot forge. The adapter keeps a counter per
  room and gives each `Joined(peer)` the next value as `epoch`. A slot whose peer changes
  shows as `Left` and then `Joined`, so a reused number gets a new epoch.
- **Unicast.** An offer, answer, candidate, glare or layer mask goes peer to peer
  (`ToPeer`). Only the peer the answer was sent to learns its ICE password, which is what
  binds the media path to that peer (RTC-3). `Published`, `Unpublished` and `Speakers` go to
  the room (`ToRoom`). `runner.room` permits peer-to-peer sends for `Watch` and `Join` members,
  and room sends for `Join` members (`room_permits`,
  core/programs/runner/runner-room-grants.hirsel).
- **Revocation from the roster.** `Left(peer)` calls `revoke(room, peer, epoch)`, and
  then sends `Fenced(peer, epoch)` to the room. The application's own kick withdraws the
  peer's room grant, and the room's roster then removes it. So a kick is acknowledged to the
  application's watchers by the `Fenced` message, sent only after RTC-7's fence holds.
- **Signaling loss fails closed.** On `Lost` (the service's room session ended or reported a
  gap), the router stops forwarding for the room: ingress is kept, so ICE consent and DTLS
  stay up, but nothing is forwarded. The service cannot see a kick while it cannot see the
  roster. On `Resynced` (a complete roster after rejoin), every peer absent from it, or
  present with a changed slot, is revoked, and forwarding resumes for the rest.
- **Refusals.** A `Signal` the router refuses is answered `Refused(code)` to the sender, with
  the `RouterError` tag as `code`. A malformed `M` never reaches the adapter, because the
  room's codec check refuses it first.

**What runner.room must provide.** media-webrtc-sfu-spec-2319 read main at `29c75134`.
The verdict on each of CONSUMER-CONTRACT §3's items belongs to hirsel4-rooms-2301's report.

| Need | Status on main | Gap |
| --- | --- | --- |
| Authenticated addressed unicast | Built. `SendToPeer` exists and the relay checks the sender's grant and membership epoch (core/programs/runner/runner-room-relay.hirsel, `relay_message_target`) | none for the server side |
| A monotonic membership revision | The relay keeps `member_epoch` and `adm` internally (core/programs/runner/runner-room-grants.hirsel). The adapter needs only joined and left per peer, which the roster diff carries (`RoomDiff.left`, core/programs/runner/runner-room-flush.hirsel) | none, since the adapter's own epoch suffices |
| Targeted kick | The application withdraws the peer's `Grant` in its admission view, and the grant set's revocation path removes it | none |
| Per-connection expiry | Built for WebSocket sessions by 7e01e2ee (rooms released at expiry, RUNNER-SPEC "Enforcement") | none |
| A Hirsel-side room client | **Missing.** `std.websocket` has client framing but no `hirsel.4` session or room client | follow-up F1 |
| Room calls in the generated JS SDK | **Missing.** sdk/client/client.ts has no room API | follow-up F2 |

Until F1 lands, the adapter is driven by a test harness that feeds `RoomIn` and reads
`RoomOut`. Its laws run that way. The browser end-to-end case waits on F1 and F2.

**ICE over TCP for strict firewalls.** A unit may also listen on TCP (its `Listen` rule,
NET-6) and offer a passive ICE-TCP host candidate
([RFC 6544](https://www.rfc-editor.org/rfc/rfc6544), `tcptype passive`). An accepted
connection carries RFC 4571 framing: each datagram is preceded by its 16-bit big-endian length.
The unit deframes into the same receive slot and feeds `receive` with the connection as the
path. It frames and writes the session's output for that path instead of putting it in the UDP
batch. ICE-Lite permits a passive TCP candidate (RFC 6544 §7.2). A browser behind a firewall
that passes TCP to the server's port then needs no TURN server. That is slice S8. TURN over TLS
(RFC 8656 §3.2, for networks that pass only proxied TLS) needs a TURN server and server-side
TLS. It is follow-up F3 for `stun_ice`, since TLS-4 has no server handshake today.

## WebRTC build laws

Each law is written **red first**, in its named harness, before the code it tests. Its oracle
is outside the code under test: an RFC's text or vectors, a capture of a real browser's or
peer's wire output, or a hand derivation from this chapter. Fixtures are captured once and
committed under the package's `vectors` directory, with the capturing browser's version.
Harnesses are the package's law module (`laws.hirsel` in `webrtc_sfu`, as `stun_ice` has),
unless named otherwise.

- **W1 `sdp_browser_offers`.** *Scenario:* parse a Chromium and a Firefox offer: audio, video
  with three-RID simulcast, RTX, the seven extensions, and an `application` section. *Oracle:*
  hand-written `Description` values from reading the captures. *Then* `answer` each with the
  default policy. *Oracle:* the expected answer text written by hand from RTC-2 and RFC 8829
  §5.3. It has the same MIDs in BUNDLE, the offer's payload types and extmap IDs,
  `setup:passive`, `ice-lite`, `a=simulcast:recv` in the offer's order, and the application
  section at port 0.
- **W2 `sdp_rfc_examples`.** Parse the SDP examples of RFC 8829 §7 and RFC 8853 §6. *Oracle:*
  the RFCs' text.
- **W3 `sdp_refusals`.** One offer per refusal of RTC-1: no BUNDLE, a section outside it, no
  `rtcp-mux`, no MID, only a `sha-1` fingerprint, one extension URI under two IDs, one payload
  type as two codecs, a RID with a `~`, 32,001 bytes, 65 sections. *Oracle:* the named
  `SdpError` with the section index, by hand.
- **W4 `demux_first_byte`.** `classify` for all 256 first bytes and the RTP/RTCP split on
  the second byte. *Oracle:* RFC 9443 §7's table and RFC 5761 §4, by hand. Exhaustive.
- **W5 `glare_settles_in_one_round`.** The server offers (adding a down-track) while a client
  offer is in flight. *Oracle:* by hand from RTC-2. The client's offer gets `Glare(v)`, the
  client's answer to `v` is accepted, its re-sent offer is answered, and both ends end
  `Stable` with both changes applied.
- **W6 `late_answer_goes_out_as_rtx`.** rtp's `rewrite_late` law covers the `Late` answer
  itself. This law covers what the session does with it. *Scenario:* source sequence 10, 11,
  13, 12 of one encoding to one down-track with RTX, then the same without RTX. *Oracle:* by
  hand from RTC-4, with the RTX packet's bytes laid out per RFC 4588 §4. With RTX, the media
  SSRC carries outbound 1000, 1001, 1003, and packet 12 leaves as one SRTP packet on the RTX
  SSRC with OSN 1002, at the RTX stream's next sequence number. A harness SRTP receiver opens
  it. Without RTX, it is dropped and counted. No sealer returns `Reused`, `Retired` or
  `Range`.
- **W7 `ssrc_survives_switches`.** One down-track goes Layer(0) → Layer(2) → Off → Layer(1) and is
  rebound to a second publication. *Oracle:* by hand. The media and RTX SSRCs never change,
  each sealer is issued exactly once (no `Issued` from TLS-8), and the outbound index rises by
  one per forwarded packet.
- **W8 `fanout_across_units`.** One publisher on unit 0, two subscribers on unit 0 and one on
  unit 1, over `sim.udp`. *Oracle:* hand-derived rewritten headers (SSRC, sequence,
  timestamp, extension IDs) for 20 packets. Each subscriber's harness session opens every
  packet to the publisher's plaintext payload. Unit 1 gets exactly one `Media` message per
  packet.
- **W9 `router_fills_units`.** rtp's `simulcast_switch` and `svc_masks` laws cover switching
  given a `Unit`. This law covers how the router builds the `Unit` and the target. *Scenario:*
  captured VP8 (three encodings, two temporal layers), H.264 (three encodings), VP9 SVC, and an
  AV1 L3T3 stream with Dependency Descriptors from Chromium. *Oracle:* for each packet, the
  `(start, end, keyframe, needs, switches)` and the `Layer(e, s, t)` to `(source, i)` mapping,
  written by hand from RFC 7741, the VP9 RTP payload format and AV1 RTP §A.8 against the
  captured bytes. For AV1, the decode-target indexes are cross-checked against LiveKit's
  dependency-descriptor selector (pkg/sfu/videolayerselector in /tmp/ref-livekit) on the same
  packets.
- **W10 `keyframe_request_coalesces_across_subscribers`.** Three subscribers switch from
  encoding 0 to encoding 2 in the same 20 ms, and a fourth subscriber's `Losses` answers
  `Reset`. *Oracle:* by hand from RTC-6. Exactly one PLI reaches the publisher before the
  first keyframe of encoding 2. All four down-tracks then forward from that keyframe. A
  publisher with `ccm fir` and no `pli` gets one FIR whose sequence number rises only for the
  next new request (RFC 5104 §4.3.1.1).
- **W11 `dynacast_mask`.** Three subscribers change targets over 12 s of simulated time.
  *Oracle:* by hand. A bit turns on in the same turn, and a bit turns off after exactly 5,000
  ms with no event inside the delay when a target returns. Packets outside the mask are never
  forwarded.
- **W12 `active_speaker_scores`.** Audio-level traces of four publications, 20 ms packets.
  *Oracle:* the Q16 formula of RTC-6, computed by hand for each 400 ms window; the ranking
  must equal LiveKit's `AudioLevel` run on the same trace (Go, /tmp/ref-livekit at `d210b4d`).
  The scores differ from LiveKit's floats only by rounding.
- **W13 `audio_forward_gate`.** `audio_forward = 2`, four speakers whose ranks change.
  *Oracle:* by hand. Each subscriber gets exactly the top two other speakers, at the next
  packet after a rank change, with continuous outbound sequence numbers per down-track.
- **W14 `revoke_fence_schedules`.** *Model:* a control unit and two media units. The steps
  are: unit 0 takes a datagram, routes it locally and posts `Media` to unit 1; the control
  unit sends the revoke; each unit applies it and acknowledges; unit 1 takes the media
  message; each unit calls `udp_send`. Every interleaving that keeps each unit's own order is
  run, through a test-only seam that pauses each step. *Oracle:* exhaustive enumeration
  against the invariant that no `udp_send` after `revoke` returns carries the participant's
  media at the fenced epoch or goes to its fenced session. A negative control (apply the fence
  after taking the inbox) must fail the enumeration.
- **W15 `revoke_stops_everything`.** Real units over `sim.udp`. The revoked participant
  publishes video with NACK-driven RTX queued, and subscribes to two publications. After
  `revoke` returns: *Oracle:* by hand from RTC-7. Across 200 ms of further input, no captured
  `UdpTx` carries any of its down-tracks' SSRCs or any packet of its publications. A Binding
  request under its old ufrag gets no response. A DTLS record and an SRTP packet from its old
  5-tuple are dropped and counted. `join` at the same epoch is `Fenced`, and at a higher epoch
  it succeeds with new credentials.
- **W16 `signaling_loss_fails_closed`.** The adapter gets `Lost`, then `Resynced` with one
  peer gone and one slot replaced. *Oracle:* by hand from RTC-8. Nothing is forwarded between
  the two. Both changed peers are revoked, `Fenced` is sent for each, and forwarding resumes
  for the rest.
- **W17 `ice_tcp_framing`.** RFC 4571 frames split across reads and joined in one read, plus
  a 0-length and a 65,535-byte frame. *Oracle:* hand-derived frame boundaries. The passive
  candidate line in the answer is checked against RFC 6544 §4.5's grammar, by hand.
- **W18 `forward_allocates_nothing`.** Forwarding one RTP packet to 8 down-tracks on one unit,
  after warm-up, runs under a zero-byte allocation bound, as `stun_ice`'s Binding laws do.
  *Oracle:* the bound.
- **W19 `unauthenticated_changes_nothing`.** On an established session, feed: an SRTP packet
  with one flipped tag bit, a replayed packet, a packet whose index estimate jumps 2^15
  ahead under a bad tag, an RTCP NACK and a TWCC feedback with bad tags, and a valid packet
  from a 5-tuple outside the table. *Oracle:* by hand from RTC-3, against a snapshot of every
  stateful value (statistics, loss window, `History`, `Rewrite` ranges, TWCC log, BWE, speaker
  score, 5-tuple table, ROC). Each input leaves all of them equal and raises one drop counter.
  A following valid packet at the next index is accepted. A negative control that runs
  statistics before `open` must fail.

**Nightly, not pre-push.** These run on Boat and route reds to the owner. They do not block
a land.

- **N1 `browser_room`:** headless Chromium and Firefox publish Opus and three-encoding VP8
  simulcast, and subscribe to each other through the service. This is the interop oracle for
  RTC-2 to RTC-4 against a real DTLS, SRTP and RTCP stack. It covers a layer switch, an
  `audio_forward` gate, a revoke whose subscriber stops receiving at once, and ICE-TCP with UDP
  blocked. It waits on F1 and F2.
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
| Fan-out overhead | ≤ 250 ns per forwarded packet per down-track, excluding the cipher and the kernel | `bench/media/sfu_fanout`: 1 publisher, 1 to 64 down-tracks on one unit; instructions and cycles per packet, minus a run with `seal_into` replaced by a copy of the same bytes |
| Allocations | 0 per forwarded packet | W18, and the bench's allocation counter |
| Voice capacity | ≥ 2,000 audio streams per core | 2,000 sessions of 50 packets per second, 200-byte payloads, `audio_forward` 3, one unit; under 100 % of one core, with no drops counted |
| Forwarding jitter | p99 < 500 µs from `udp_recv` return to `udp_send` issue | the same run, from `trace` timestamps |
| Browser end to end | voice plus 1080p three-encoding simulcast | N1 |
| Room scale | a 100-peer room under `sim.udp` | N2 |

## Build slices

Each slice lands on its own with its laws. A Sol lane can run S1 to S6 in order. S7 and S8
can run beside S5. Reds that may ride a land are the nightly N1 and N2, and any cost target
not yet met. A red law of a slice's own blocks that slice. A TLS-8 `Reused`, `Issued` or
`Range` anywhere, or any red in W14, W15 or W19, blocks, because those are soundness reds.

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **S1** SDP | the package skeleton (`webrtc_sfu` with `sdp`, `peer`, `router`, `signal`); RTC-1 `parse` and `write`; RTC-2 `answer`, `offer`, `accept_answer`, glare state; captured browser offers committed as vectors | nothing | W1, W2, W3, W5 |
| **S2** VP8 descriptor | `media_bits` gains VP8 (RFC 7741): payload descriptor parse (`X`, `I`, `L`, `T`, `K`, `PictureID`, `TL0PICIDX`, `TID`, `Y`, `KEYIDX`), keyframe detection from the payload header's `P` bit, and the rewrites of `PictureID` and `TL0PICIDX` | nothing | in `media_bits`'s own laws: RFC 7741 §4's examples, and captured Chromium VP8 packets, by hand |
| **S3** PeerSession | RTC-3 over `stun_ice` Lite, the dtls lane's server, the srtp lane's contexts and the rtp lane's parsing; `classify`; the 5-tuple table; RTC-4's SSRC generator, sealers, late-packet RTX and NACK answers | S1; dtls, srtp and rtp lanes' first slices | W4, W6, W7, W19 |
| **S4** One-unit router | RTC-5 on a single unit: rooms, publications, down-tracks, fan-out, bounds; RTC-6's layer selection, switch points and keyframe-request coalescing | S2, S3 | W9, W10, W18 |
| **S5** Units and fence | the control order, `Media` messages with `Shared[Bytes]`, remote NACKs; RTC-7 with its test seam | S4 | W8, W14, W15 |
| **S6** Dynacast and speakers | RTC-6's wanted mask and pause delay, the bandwidth-limited effective target, the Q16 active-speaker scores, `audio_forward` | S4 | W11, W12, W13 |
| **S7** Signaling adapter | RTC-8's `Signal`, `Embed`, `RoomIn`/`RoomOut`, epochs, fail-closed loss; driven by a harness until F1 | S5 | W16 |
| **S8** ICE-TCP | RTC-8's passive TCP candidate, RFC 4571 framing, TCP paths in the session | S3 | W17 |
| **S9** Costs and nightly | `bench/media/sfu_fanout` and the voice-capacity run; N1 and N2 on Boat | S5, S6, S7; F1, F2 for N1 | the cost targets |

**Follow-ups this spec names.**

- **F1** (runner): a Hirsel-side `hirsel.4` room client, so a Hirsel service can be a room peer
  and feed `RoomIn`.
- **F2** (SDK): room calls in the generated JS SDK, for unicast `Message`, the roster and
  state.
- **F3** (`stun_ice`): TURN over TLS (RFC 8656) for proxied networks. It needs server-side TLS.
- **Ownership:** the active-speaker detector is this package's (RTC-6), not the rtp lane's.
  The rtp lane parses the RFC 6464 extension.

## Prior art

Goose (/tmp/ref-goose at `d5ae5bb`) and LiveKit (/tmp/ref-livekit at `d210b4d`), with str0m
(/tmp/ref-str0m) consulted for the sans-IO shape.

| Source | Mechanism | Verdict |
| --- | --- | --- |
| Goose | Networking is library code over explicit costs, not a language form (goose docs/stdlib.md, as [HTTP](http.md#goose-and-goose-http) records) | **ADOPTED.** The SFU is a package with explicit bounds and charges. Goose has no media, UDP or WebRTC code to compare beyond that. |
| Goose | Limits as counts per worker | **ADOPTED** as `RoomLimits` fixed at `open_room`, refused, never evicted |
| LiveKit | Two PeerConnections per participant, one for publishing and one for subscribing (pkg/rtc) | **REPLACED** by one bundled session per participant. One ICE, DTLS and SRTP setup, one 5-tuple and one keepalive. Glare is settled by the impolite-server rule, which LiveKit avoids by separating the directions. |
| LiveKit | Dynacast: subscribed max quality per track, upgrades at once, downgrades after `DynacastPauseDelay` 5 s (pkg/rtc/dynacast/dynacastmanagervideo.go, pkg/config/config.go) | **ADOPTED**, made generic: `Target` is an encoding and layer, not a named quality, and the mask goes to the publisher as a signal. |
| LiveKit | Audio level: 400 ms window, -35 dBov threshold, 40 % activity, two-interval EMA (pkg/sfu/audio/audiolevel.go) | **ADAPTED** to exact integer arithmetic (RTC-6), so replay and every engine agree bit for bit. |
| LiveKit | Forwarder with simulcast switching at keyframes and DD-driven SVC selection (pkg/sfu/forwarder.go, pkg/sfu/videolayerselector) | **ADOPTED** for switch points. Its test vectors are W10's oracle. |
| LiveKit | NACK answered from a sequencer by resending on the media SSRC or RTX | **REPLACED** by RTX only (RTC-4). TLS-8 never seals twice and never seals below the highest index, so a media-SSRC resend would need sealed copies per subscriber. |
| LiveKit | Single UDP port with a mux across the node, sessions found by ufrag then 5-tuple | **REPLACED** by one port per unit, so the home unit is a signaling choice and the kernel needs no steering. |
| LiveKit | Kick through its room service: remove participant, close its PeerConnection | **REPLACED** by RTC-7's fence. LiveKit's removal is asynchronous and gives no point after which no packet is forwarded. |
| str0m | Sans-IO `Rtc` with `handle_input` and `poll_output`, one per peer | **ADOPTED** as `receive` and `poll` |
| str0m and LiveKit | SCTP data channels | **NOT TAKEN** for Wave 1. Application data goes over `runner.room` or WebTransport (Wave 2). |
