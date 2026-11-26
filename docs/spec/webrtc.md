# WebRTC sessions and the SFU router

Version note (2026-11-24, media-webrtc-sfu-spec-2319): first text of RTC-1 to RTC-8, Wave 1.6
of the real-time media track. Built at `core/programs/webrtc_sfu/` and `core/programs/media_record/` (RTC-9), both shipped in the toolchain bundle (`webrtc_sfu@1`, `media_record@1`).
Amended 2026-11-24 (webrtc-spec-amend-2337) for the 15 findings of design review 2333: session
generations on every queue and an all-writer fence acknowledgement (RTC-7), a proven roster
across signaling loss (RTC-8, runner slice R1), per-down-track sender reports and wire
identity (RTC-4, RTC-5), decodable switching with stable target maps (RTC-6), JSEP
`bundle-only` and `tls-id` (RTC-1, RTC-2), prefix demultiplexing (RTC-3), per-direction RID
and simulcast (RTC-1, RTC-2), negotiated keyframe requests, bandwidth lowering, RFC 6464 `vad`
per leg, SSRC collision recovery and bounded fence state. JSEP is
[RFC 9429](https://www.rfc-editor.org/rfc/rfc9429), which obsoletes RFC 8829. BUNDLE is
[RFC 9143](https://www.rfc-editor.org/rfc/rfc9143), which obsoletes RFC 8843. Section
numbers cited as RFC 8843 are that text's, as the review read it.
Amended 2026-11-25 (webrtc-leg-stall-2651): RTC-6's allocation always takes a down-track's
first step, so the budget lowers a down-track to its lowest encoding but never pauses it
(LiveKit's default, `allow_pause` off). The bandwidth pause turned a leg whose base layer
measured above the 300 kbps start estimate `Off` about 1 s into a call, and with no probing
and nothing sent on the leg no estimate could rise to resume it.
Amended 2026-11-25 (webrtc-nack-rtx-repair-2689): a session that negotiated `rtcp-rsize`
now takes an RR-led datagram without an SDES (rtp.md RTP-4, RFC 5506 §4.1). libwebrtc sends
every NACK on such a session as an RR with its report blocks followed by the NACK and no
SDES, and the check refused each one as `NoCname`, so no NACK reached RTC-4's history and no
loss was ever repaired. W27 `rsize_nack_is_answered_with_rtx` is the law.
Amended 2026-11-25 (webrtc-bwe-probe-2669): RTC-6 probes. A subscriber whose estimate
leaves a video down-track below its target's encoding asks rtp's `bwe.probe` (RTP-9) for
the rate that carries every target plus a quarter, and the session's pacer sends the
cluster as padding-only packets on the leg's RTX SSRC, or on its media SSRC at a frame end
(RTP-11 `pad`) when it negotiated no RTX. A probe result raises the estimate and the
allocator adds the next encoding, so a leg that starts at 300 kbps reaches its top layer in
two probe rounds. A room may declare `pause` (LiveKit's `allow_pause`): a down-track whose
lowest encoding does not fit then goes `Off`, and a probe resumes it. The default stays as
2651 set it. W24 `probe_ramps_to_top_layer` and its companions are the laws. The package is
`webrtc_sfu`, pure Hirsel under core/programs, with no `Foreign` code in its reach, so
FFI-8 ([foreign](foreign.md)) replay and `hirsel test --sim` cover it end to end.
Amended 2026-11-25 (webrtc-opus-red-c1-2652) for Opus RED: RTC-2 answers `red/48000/2` with
its primary mapping, RTC-4 says how a RED leg would be repaired, and RTC-5 rebuilds RED per
down-track from a per-publication ring under a loss-driven policy ([RTP-13](rtp.md#rtp-13-redundant-audio-red)).
Amended 2026-11-25 (webrtc-e2ee-routing-law-2684) for end-to-end encrypted media: RTC-6 names
the clear range of each codec, the bytes an SFrame or DAVE sender leaves clear, and the router
routes on those alone (W25). VP9's keyframe test now reads the descriptor only (`B`, `P`,
SID), as libwebrtc's depacketizer does; it read the uncompressed header's frame marker, which
an encrypted VP9 frame hides, so a scrambled keyframe was counted `unparsed`.
Amended 2026-11-25 (media-record-c10b-2658): RTC-9, the recorder `media_record`, a
virtual down-track on a unit of its own, fenced by RTC-7 like every unit.
Amended 2026-11-25 (webrtc-hdrext-pack-2682) for plan item C4, the header-extension pack:
RTC-1 answers `video-layers-allocation00`, `playout-delay`, `urn:3gpp:video-orientation` and
`abs-capture-time`; RTC-5 forwards CVO on every frame end with the publication's current
value, writes a static per-subscription playout delay (`set_playout`) and passes
abs-capture-time through; RTC-6 reads the publisher's layers allocation, so planned rates
replace measured ones and a stream the publisher stops moves its down-tracks at the next
switch point. Laws W11b and W26.
Amended 2026-11-25 (webrtc-rtp-fec-2771) for FlexFEC-03 per leg ([RTP-14](rtp.md#rtp-14-flexfec-03)):
RTC-1 reads `flexfec-03/90000` and `ssrc-group:FEC-FR`; RTC-2 keeps `Flexfec` only on a
video section the browser offered `recvonly`, so Firefox, which offers none, stays NACK-only;
RTC-4 draws a FlexFEC SSRC per video down-track and re-offers its `FEC-FR` pair; RTC-5
protects keyframes and base-layer frames per leg, paced behind the group. Law W30, N1
`flexfec_per_leg`.
Amended 2026-11-25 (webrtc-dynacast-rtx-polish-2717): RTC-3's `PeerLimits` gains
`start_bps`, the estimate a session starts from. RTC-6's audio gate gives the
slots no speaker takes to quiet publications by a second score over every audible level.
In RTC-6, an encoding the publisher
stops without a layers allocation, by sending nothing for two rate windows, is dropped like
one its allocation stops. Allocation steps over dropped encodings and never prices a step
below 0, so a silent or cheaper stream can neither freeze a leg nor pay for another
publication's step. A dropped encoding up to a target stays in the local mask so that the
publisher can resume it. Within 5 s of a session's first cluster, a cluster that raised
nothing is retried after 1 s rather than 5 s (the window and spacing of libwebrtc's
repeated initial probing). A down-track whose RTX pair the agreement in force lacks re-offers
the session when it is added, so every subscribe on an RTX section signals its
`a=ssrc-group:FID`, not only FlexFEC sections and collisions.

Amended 2026-11-26 (sfu-pool-media-w2-spec-2888): RTC-10, pooled rooms. One service serves
many rooms from one pool of units and ports, with room ids that are never reused, keys that
carry their room, exact per-room sizing, a per-room control order and per-room isolation.
Laws W34 to W39, slices P1 to P4. Inter-SFU relaying is [sfu_cascade](sfu_cascade.md).

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

**Using the shipped package.** The installed toolchain ships `webrtc_sfu@1` in
`core-packages/` with every package it binds: `turn@1`, `dtls@1`, `srtp@1`, `rtp@1`,
`stun_ice@1`, `media_bits@1`, `runner@1` and `store@1`
([building](../building.md#the-installation)). A service outside the repository binds it as
`webrtc_sfu = { major = 1 }`, with `turn = { major = 1 }` if it names TURN types itself, and
builds with the installed `hirsel` alone; it calls `webrtc_sfu.signal.serve_pool` (RTC-8, RTC-10). Inside
the repository the workspace binds it by path, `core/programs/webrtc_sfu`.

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
| media-rtp-spec-2318 (RTP-1 to RTP-13, the `rtp` package) | RTP header and extension parsing and writing, RTCP (SR, RR, SDES, BYE, NACK, PLI, FIR, TWCC), the retransmission `History` with RTX, delay-based BWE, the pacer, per-down-track sequence and timestamp rewriting (`Rewrite`, answering `Forward`, `Late`, `Drop` or `Wait`), the synchronous `close` operations of RTP-12, and RTP-13's RED reader, ring, writer and policy | every byte-level RTP and RTCP operation |
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

```hirsel signatures webrtc_sfu/sdp
pub type Media: key = Audio | Video | Application
pub type Direction: key = SendRecv | SendOnly | RecvOnly | Inactive
pub type Setup: key = Active | Passive | ActPass
pub type Codec: key = Opus | Vp8 | Vp9 | H264 | Av1 | Rtx | Red | Ulpfec | Flexfec
pub type Ext: key = Mid | RidExtension | RepairedRid | AudioLevel | AbsSendTime | TransportCc | DependencyDescriptor | VideoLayersAllocation | PlayoutDelay | VideoOrientation | AbsCaptureTime
pub type Format { pt: u8, codec: Codec, clock: u32, channels: u8, fmtp: Bytes, apt: Option[u8], nack: bool, pli: bool, fir: bool, twcc: bool }
pub type Vad: key = VadOn | VadOff
pub type Extmap { id: u8, ext: Ext, dir: Option[Direction], vad: Option[Vad] }
pub type RidDir: key = RidSend | RidRecv
pub type Rid { id: Inline[u8, 16], len: u8, dir: RidDir, pts: List[u8], depend: List[u8], unresolved: List[Id], restrictions: Bytes }
pub type Stream { alternatives: List[u8], paused: List[bool] }
pub type Simulcast { send: List[Stream], recv: List[Stream] }
pub type Port: key = Rejected | BundleOnly | Port(u16)
pub type Section {
    media: Media,
    protocol: Bytes,
    fallback: Bytes,
    connection: Bytes,
    port: Port,
    mid: Inline[u8, 16],
    mid_len: u8,
    dir: Direction,
    formats: List[Format],
    extmaps: List[Extmap],
    mixed_extmaps: bool,
    rids: List[Rid],
    simulcast: Simulcast,
    ssrcs: List[u32],
    rtx_pairs: List[(u32, u32)],
    fec_pairs: List[(u32, u32)],
    msid: Bytes,
    rtcp_mux: bool,
    rtcp_rsize: bool
}
pub type Description {
    origin_session: u64,
    origin_version: u64,
    bundle: List[u8],
    ice_ufrag: Bytes,
    ice_pwd: Bytes,
    ice_lite: bool,
    ice_options: Bytes,
    mixed_extmaps: bool,
    fingerprint: Inline[u8, 32],
    setup: Setup,
    tls_id: Bytes,
    sections: List[Section],
    candidates: List[Bytes],
    end_of_candidates: bool
}
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
  most 768 of the rest. RTC-8 **Offer size** says how a browser's offer fits.
- `TooManySections`: more than 64 m-sections.
- `TooManyFormats(i)`: more than 32 formats in section `i`.
- `TooManyRids(i)`: more than 4 RIDs in section `i`, counted over both directions.

**Attributes read.** `v`, `o`, `s`, `t`, `c`, `m` and these `a=` attributes: `group:BUNDLE`,
`bundle-only`, `ice-ufrag`, `ice-pwd`, `ice-lite`, `ice-options`, `fingerprint`, `setup`,
`tls-id`, `mid`, `rtcp-mux`, `rtcp-rsize`, `extmap-allow-mixed`, `extmap` (with its optional
direction and its extension attributes), `sendrecv`, `sendonly`, `recvonly`, `inactive`,
`msid`, `ssrc` (only the `cname` and `msid` forms), `ssrc-group:FID` (into `rtx_pairs`),
`ssrc-group:FEC-FR` (media then FlexFEC SSRC, into `fec_pairs`), `rtpmap`, `fmtp`,
`rtcp-fb`, `rid`, `simulcast`, `candidate` and `end-of-candidates`. Session-level `ice-ufrag`, `ice-pwd`,
`fingerprint`, `setup` and `tls-id` apply to every section that lacks its own. `ice-lite` is
read and written only at session level. Any other attribute is skipped, as RFC 8866 §5.13
requires of an unknown attribute. An extension URI or codec name outside `Ext` or `Codec` is
skipped with its payload type or ID. The answer never accepts it, so skipping it changes no
negotiated behaviour.

**Codec names.** `opus/48000/2`, `VP8/90000`, `VP9/90000`, `H264/90000`, `AV1/90000`,
`rtx/90000`, `red/48000/2`, `ulpfec/90000` and `flexfec-03/90000`, compared ASCII case-insensitively
(RFC 8866 §6.6). The extension URIs are those of RFC 8843 (`mid`), RFC 8852 (`rtp-stream-id`,
`repaired-rtp-stream-id`), RFC 6464 (`ssrc-audio-level`), `abs-send-time`,
`transport-wide-cc-extensions-01`, the AV1 RTP specification's `dependency-descriptor`, and
the four of RTP-3's C4 kinds: libwebrtc's `video-layers-allocation00`, `playout-delay` and
`abs-capture-time` (`http://www.webrtc.org/experiments/rtp-hdrext/…`) and 3GPP TS 26.114's
`urn:3gpp:video-orientation`.
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
- a payload type reused across bundled sections must have the same media kind, codec,
  clock rate and channel count (`PayloadConflict(pt)`). RFC 9143 §9.1.1 and RFC 8859
  §4.7 specify additional restrictions on senders that reuse payload types. For browser
  interoperability the SFU accepts section-local `fmtp`, RTX `apt` and `rtcp-fb` values
  on a repeated payload type. For example, bundled mono microphone and stereo screen
  audio can share Opus PT 111. RTC-3 resolves a repeated payload type by MID or SSRC
  binding. Payload type alone can select a section only when it is unique among the
  receiving sections.
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

```hirsel signatures webrtc_sfu/sdp
pub type Policy: cross { audio: List[Codec], video: List[Codec], exts: List[Ext], max_sections: u8, simulcast: bool }
pub type Negotiated {
    remote_ufrag: Bytes,
    remote_pwd: Bytes,
    remote_fingerprint: Inline[u8, 32],
    remote_setup: Setup,
    tls_id: Bytes,
    local: LocalEnd,
    mixed_extmaps: bool,
    sections: List[Agreed]
}
pub type Agreed {
    index: u8,
    mid: Inline[u8, 16],
    mid_len: u8,
    media: Media,
    recv: bool,
    send: bool,
    formats: List[Format],
    extmaps: List[Extmap],
    ext_form: ExtForm,
    rids: List[Rid],
    streams: List[Stream],
    rid_drops: RidDrops,
    local: Section,
    down_track: bool,
    remote_ssrcs: List[u32],
    remote_rtx: List[(u32, u32)]
}
pub type Signaling: key = Stable | HaveLocalOffer(u64) | HaveRemoteOffer(u64)

pub fn answer(policy: Policy, current: Option[Negotiated], remote: Description, local: LocalEnd, mut out: Description, mut agreed: Negotiated) -> Result[(), SdpError]
pub fn offer(policy: Policy, current: Negotiated, local: LocalEnd, adds: List[SendSection], mut out: Description) -> Result[(), SdpError]
pub fn accept_answer(current: Negotiated, sent: Description, remote: Description, mut agreed: Negotiated) -> Result[(), SdpError]
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
   format is kept when its `apt` format is kept. A `Red` format is kept on an audio section when
   the policy lists `Red` and its `fmtp` names one payload type for every block
   (`111/111`), that of a format kept here; the answer keeps the offerer's mapping as
   offered. `default_policy` lists `Opus` and `Red` for audio. A `Flexfec` format is kept
   only on a video section offered `recvonly` (the server sends there and never receives
   FEC) when the policy lists `Flexfec` and the section keeps a media format;
   `default_policy` lists it for video. Chromium offers `flexfec-03` on every video
   section; Firefox offers none, so its legs carry no FEC and are repaired by NACK and RTX
   alone. `Flexfec` is never a section's media format. A section left with no media format is
   answered `Rejected`.
4. Extmaps are the offer's for extensions the policy lists, with the offer's IDs (RFC 8285
   §7). An extmap with a direction is answered with the complementary direction, and one
   whose direction does not overlap the section's answered direction is not kept (RFC 8285
   §7). For `AudioLevel` the answer repeats the offer's `vad` value, so each leg's `V` bit
   has the meaning that leg negotiated (RTC-6). `Mid` is required, and a section without it
   was refused at parse. `DependencyDescriptor` is kept only on a section that describes
   AV1 and allows mixed extmaps (session or section `extmap-allow-mixed`), since a
   descriptor carrying its structure passes the one-byte form's 16 bytes (RFC 8285 §4.2).
   On a section the server receives on, the first agreed media format must be AV1, since
   the browser sends that codec: Chromium, answered the descriptor on VP8, sends VP8
   without `TID`. On a section the server only sends on, AV1 must be among the agreed
   formats. `offer` applies the same rule to the sections it creates.
   `VideoLayersAllocation`, `PlayoutDelay`, `VideoOrientation` and `AbsCaptureTime` follow
   the general rule: kept with the offer's ID and the complementary direction. Chromium
   offers `playout-delay` without a direction and Firefox as `recvonly`, so a
   recvonly section's answer carries it `sendonly` (W1's vectors).
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
   `ice-lite` is a session-level line. A section offered with `rtcp-rsize` is answered with
   it ([RFC 5506 §5](https://www.rfc-editor.org/rfc/rfc5506#section-5); libwebrtc's answer
   copies the offer's `rtcp_reduced_size`), and RTC-3 then reads RTCP as `Reduced`
   (rtp.md). Chromium and Firefox both offer it and send transport-cc feedback alone in its
   datagram, which `Compound` refuses as `NotCompound`.

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
agreed formats (with the publication's agreed `Red` format whose blocks name it, when the
policy lists `Red`), `rtcp-rsize` as libwebrtc offers it, and one SSRC and one RTX SSRC (RTC-4).
A re-offer writes, per down-track that has a FlexFEC SSRC on a section that agreed `Flexfec`,
an `a=ssrc-group:FEC-FR <media> <fec>` line and the FEC SSRC's `a=ssrc` lines; libwebrtc
creates its FlexFEC receive stream only from that line. One server offer is in flight at a
time: an offer asked for while one is outstanding is made when its answer is taken.
When a subscription or rebind changes the Opus `fmtp` on a subscriber's sending section,
the router re-offers that section with the publication's parameters and the subscriber's
negotiated payload type. Each publication takes its Opus parameters from its own receiving
section, including when the two sessions live on different units.
A down-track that takes a section whose offer in flight, or else whose agreement in force,
announces another track's SSRCs is re-offered too, since a browser does not hand an
unsignaled SSRC to a MID that has signaled ones.
`accept_answer` keeps `rtcp-rsize` on a section only when the answer repeats it. It marks a removed down-track's section
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
  Opus offer and answer parameters describe their respective endpoints (RFC 7587 §7.1).
  The server keeps its offered Opus `fmtp` for future offers. If the section also receives,
  its incoming Opus parameters come from the answer.
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

```hirsel signatures webrtc_sfu/peer
pub opaque type PeerSession
pub type RoomId: key = RoomId(u64)
pub type Participant {
    gen: Gen,
    room: RoomId
}
pub type PeerLimits { sections: u8, down_tracks: u16, rtx_bytes: u64, pacer_packets: u32, start_bps: u64 }
pub type PeerError = Sdp(sdp.SdpError) | Fenced | Closed | Ice(stun_ice.IceError) | Dtls(dtls.DtlsError) | Srtp(srtp.SrtpError) | Rtp(rtp.header.WriteError) | Capacity(Capacity)
pub type Class: key = Stun | Dtls | Rtp | Rtcp | Drop
pub type Input = Nothing | Replied | Media(MediaIn) | Repair(MediaIn) | Control(ControlIn)
pub type Poll = Transmit | WakeAt(u64) | Idle
pub type Gen: key = Gen(id: u64, epoch: u64)
pub type Tag { src: Option[Gen], dst: Gen, room: RoomId }
pub type Outbox { data: Bytes, pkts: List[UdpTx], tags: List[Tag] }

pub fn classify(prefix: Bytes) -> Class
pub fn open(take local: sdp.LocalEnd, take cert: dtls.Identity, r: Random, base: Addr, limits: PeerLimits, agreed: sdp.Negotiated, participant: Participant) -> Result[PeerSession, PeerError]
pub fn receive(mut p: PeerSession, now: u64, from: Addr, mut slot: Bytes, mut out: Outbox) -> Result[Input, PeerError]
pub fn poll(mut p: PeerSession, now: u64, mut out: Outbox) -> Poll
pub fn renegotiated(mut p: PeerSession, agreed: sdp.Negotiated) -> Result[(), PeerError]
pub fn close(take p: PeerSession, notify: bool, mut out: Outbox)
```

A PeerSession is a sans-IO state machine over `(now, datagram)`. It never calls the network
or reads a clock. Its estimate (RTP-9) starts at `PeerLimits.start_bps` before any feedback
moves it, between a floor of 30 kbps and a ceiling of 20 Mbps; the service starts at
300 kbps, libwebrtc's and LiveKit's default. A deployment whose subscribers' links are known
to be fast raises it, so RTC-6's first allocation already reaches a higher encoding. The home unit (RTC-5) owns it, receives into `slot`, and passes `out` to
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
  the loss. On a leg that agreed `Red`, the redundant blocks of later packets cover it
  as well (RTC-5).
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
  format gets no answer, and the NACK is counted. Audio negotiates no RTX today, so no audio
  down-track keeps an egress history and a NACK on an audio leg is answered by nothing; it
  feeds that leg's RED policy (RTC-5). When audio RTX is taken, a RED leg's history records
  the RED payload type and payload as written, and a repair resends that payload, never a
  RED payload rebuilt from the ring's later state. Its loss is covered by a keyframe request
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
A browser also accepts unsignaled SSRCs by MID, but signaling them pins the RTX pairing
([RFC 4588](https://www.rfc-editor.org/rfc/rfc4588) §8.1), as libwebrtc, LiveKit and
mediasoup do. The answer to the participant's one offer, at join, names no SSRC, since no
down-track exists yet. A down-track whose RTX or FlexFEC SSRC the agreement in force does
not pair with its media SSRC (`a=ssrc-group:FID` or `FEC-FR`) therefore re-offers the
session when it is added, as a subscribe and a re-subscribe do. A down-track with neither,
audio or video without RTX, raises no re-offer: its media goes by MID. An unsubscribe
raises none either; the next re-offer names no SSRC for the ended track.

- **FlexFEC SSRC ([RTP-14](rtp.md#rtp-14-flexfec-03)).** A video down-track on a section
  that agreed `Flexfec` draws a third SSRC from the generator, with its own sealer and its
  own sequence numbers from a random start. The browser learns it only from a server offer,
  so adding such a down-track re-offers the session with its `FEC-FR` pair, and the leg
  sends FEC only once an answer in force carries that pair. A collision on the FEC SSRC
  retires and redraws it like the RTX SSRC, clears its group and re-offers. Repair packets
  are never kept in the history: a NACK or report block naming the FEC SSRC answers nothing.

## RTC-5: Router, rooms and fan-out

```hirsel signatures webrtc_sfu/router
pub opaque type Router
pub type PublicationId: key = PublicationId(u64)
pub type Target: key + codec = Off | Layer(encoding: u8, spatial: u8, temporal: u8)
pub type RoomLimits { participants: u16, ids: u16, publications: u16, down_tracks: u32, audio_forward: u8, pause_delay_ms: u32, pause: bool }
pub type Cause = Signaling | Lease | Term
pub type RouterError = NoRoom | NoParticipant | NoPublication | Fenced | Suspended | Exists | Limit(u32) | Unrepresentable | Peer(peer.PeerError) | Capacity(Capacity)

pub fn router(r: Random) -> Router
pub fn open_room(mut r: Router, room: peer.RoomId, limits: RoomLimits) -> Result[(), RouterError]
pub fn close_room(mut r: Router, room: peer.RoomId, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn join(mut r: Router, take session: peer.PeerSession) -> Result[(), RouterError]
pub fn subscribe(mut r: Router, room: peer.RoomId, who: peer.Gen, p: PublicationId, section: u8, target: Target, now: u64, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn set_target(mut r: Router, room: peer.RoomId, who: peer.Gen, p: PublicationId, target: Target, now: u64, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn set_playout(mut r: Router, room: peer.RoomId, who: peer.Gen, p: PublicationId, delay: Option[peer.Delay]) -> Result[(), RouterError]
pub fn rebind(mut r: Router, room: peer.RoomId, who: peer.Gen, from: PublicationId, to: PublicationId, target: Target, now: u64, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn unsubscribe(mut r: Router, room: peer.RoomId, who: peer.Gen, p: PublicationId) -> Result[(), RouterError]
pub fn leave(mut r: Router, room: peer.RoomId, who: peer.Gen, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn revoke(mut r: Router, room: peer.RoomId, id: u64, epoch: u64, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn suspend(mut r: Router, room: peer.RoomId, cause: Cause, mut out: peer.Outbox) -> Result[(), RouterError]
pub fn resume(mut r: Router, room: peer.RoomId, cause: Cause, mut out: peer.Outbox) -> Result[(), RouterError]
```

```hirsel signatures webrtc_sfu/units
pub opaque type Unit
pub type Event = Fenced(peer.Participant) | Layers(room: peer.RoomId, publication: router.PublicationId, mask: u8) | Speakers(room: peer.RoomId, top: Inline[u64, 8], count: u8) | Offer(who: peer.Participant, version: u64, text: Bytes) | Shut(who: peer.Participant)
pub type Plane: key { inflight: u16, rooms: u32, media_take: u32 }
pub fn inboxes(addr: Addr, index: u16, units: u16, plane: Plane) -> Result[(Doors, Inbox[ControlMessage], Inbox[router.Carried]), router.RouterError]
pub fn unit(r: Random, index: u16, doors: List[Doors], take control: Inbox[ControlMessage], take media: Inbox[router.Carried], out_bytes: u64, out_packets: u64, plane: Plane) -> Result[Unit, router.RouterError]
pub fn begin(mut u: Unit, now: u64, take c: router.Change) -> Result[(), router.RouterError]
pub fn finished(mut u: Unit, room: peer.RoomId) -> Option[Result[router.Answer, router.RouterError]]
pub fn call(mut u: Unit, clock: Clock, mut sock: UdpSocket, take c: router.Change) -> Result[router.Answer, router.RouterError] waits
pub fn turn(mut u: Unit, clock: Clock, mut sock: UdpSocket, until: u64) -> Result[Option[u64], router.RouterError] waits
pub fn take_events(mut u: Unit) -> List[Event]
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
unit that runs the signaling adapter (RTC-8) is the **control unit**. It submits a
`router.Change` through `units.call` or the `units.begin` and `units.finished` pair. The unit
that owns the affected session applies the change first. After it acknowledges success,
the control unit sends the change to the other units. The call completes when those units
acknowledge it.

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
queues signaling and dynacast events, which the adapter takes with `units.take_events`
(RTC-8). It returns the earliest deadline, if any. The unit passes the earlier of that
deadline and 50 ms from the current time as the next turn's `until`.

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

**Redundant audio (RED, [RTP-13](rtp.md#rtp-13-redundant-audio-red)).** RED is decided per
down-track, never forwarded as the publisher sent it.

- *Ingress.* A packet on a publisher's agreed `Red` payload type is read with `red.primary`;
  the primary's bytes and payload type are the `Media` that is routed, and a payload
  `red.primary` refuses is dropped and counted malformed. The publisher's redundant blocks
  are not forwarded and do not repair the uplink: recovering a lost uplink packet from a
  later packet's blocks is not taken.
- *Ring.* Each Opus publication keeps a `red.Ring` of depth `MAX_DISTANCE` (2) on every
  unit that routes it; other codecs keep a ring of depth 0. In step 2, after every local
  down-track has been written, the unit keeps the packet's primary under its ingress index
  and timestamp, so the ring holds only earlier packets while legs read it. **Deviation**
  from the `Shared[Bytes]` handle the rest of fan-out uses: the ring copies each primary (at
  most `MAX_BLOCK` bytes) into slots reserved when the publication is made, because W18
  allows no allocation per forwarded packet and a held handle per primary would be one.
- *Policy.* Each audio down-track keeps a `red.Policy`. Each RR block for the down-track's
  SSRC is one loss sample: the larger of its `fraction` and the leg's NACK rate since the
  previous block, `min(255, 256 · nacked / forwarded)`, where `nacked` counts the packets
  the leg's NACKs name (each FCI's PID and the set bits of its BLP) and `forwarded` the
  packets staged for it.
- *Egress.* A leg whose section agreed `Red` and whose policy is at distance `d > 0` is sent
  `red.write` of the primary with up to `d` past primaries, every block tagged the leg's own
  Opus payload type, in a packet on the leg's `Red` payload type. `since` is the leg's first
  packet from its current source (a change of source, or the first packet after the leg was
  held, starts it again), so no block crosses a switch whose timestamps the receiver cannot
  place. A write refused for room sends the primary alone. A leg that did not agree `Red`,
  or whose policy is `Off`, gets the primary alone on its Opus payload type, so a publisher's
  RED never reaches a leg that did not agree it.

**FlexFEC per leg (RTP-14).** Each video down-track whose FEC pair is in force keeps one
`fec.Group` and one `fec.Policy`, set by each RR block for its media SSRC and the session's
round trip. Each media packet, as rewritten and before it is sealed, is put in the group with
the size its frame's class gives: every packet of a keyframe (the class is taken at the
frame's first packet, whose descriptor says so) is `Key`, a frame in the lowest target's mask
is `Base`, and the rest are `Upper` and go unprotected. When the group is full its repair is
staged in the pacer as video right behind the packet that filled it, on the leg's `Flexfec`
payload type and FEC SSRC, with its own next sequence number, the last protected packet's
timestamp, the section's MID and a transport-wide number, so the repair is charged to the
leg's estimate and reaches the browser before it NACKs the gap. A browser that rebuilds a
packet from FEC does not NACK it; only a group with two losses leaves NACKs for RTX.

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

**Video header extensions on egress (C4).** Each leg writes, with its own negotiated IDs
(RTP-3), three values the publisher's header carried or the subscription set:

- **CVO.** The home unit keeps the publication's current orientation: the last CVO byte any
  fresh media packet of any encoding carried. Every packet a leg forwards with the marker
  bit set (the frame's last packet, or a sub-layer leg's frame end above) carries that byte,
  whether or not its own source packet had it. libwebrtc's receiver
  (`rtp_video_stream_receiver2.cc`) takes a frame's rotation from its last packet and reads
  a packet without CVO as 0°, while a sender is only required to send CVO on keyframes and
  on a change (3GPP TS 26.114 §7.4.5; libwebrtc's sender adds it on every frame end while
  the rotation is nonzero). Forwarding only the source's byte would render every delta
  frame after a switch, and after a minimal sender's change, unrotated. Repairs carry the
  byte their original packet was sent with (RTC-4's per-leg send record).
- **Playout delay.** `set_playout(who, p, Some(Delay(min, max)))`, both in 10 ms units,
  sets a static delay on that down-track: every media packet of the leg carries it, so a
  viewer-class subscriber can buffer more and an interactive one can pin a low delay.
  `None` stops writing it. `max` above 4095 or `min` above `max` is `Limit(4095)`. The
  router does not adapt it; LiveKit's jitter-driven controller is a follow-up.
- **Abs-capture-time.** Passed through unchanged: the leg writes the value the source
  packet carried, 8 or 16 bytes, and estimates nothing. A cascade keeps lip-sync across
  hops only when the original capture time survives each one.

A leg whose section did not negotiate a kind skips it (RTP-3), and an extension that does
not fit costs the packet only that extension.

**Marker bit and descriptor on egress.** A forwarded packet carries the marker bit when its
source packet does, or when it is the last packet of a layer frame whose spatial id is the
highest of the down-track's current target: VP9's `E = 1` at `SID = s` for target
`4 · s + t`, AV1's last packet in frame at `S_d`. So a sub-layer leg's decoder sees each
picture end. The current target is the rewrite's after this packet's decision. Every unit
computes this from its own copy of the encoding's state, so a leg on a unit other than the
publication's home gets the same markers. An AV1 down-track writes each packet's
Dependency Descriptor on the ID the subscriber negotiated, with the source's bytes except
`frame_number`, which goes through an RTP-11 `Translate` counter per down-track: 16 bits,
restarted at 1 when the down-track's source changes, so the leg's first frame is 1 and a
frame keeps its number across its packets. A paced packet keeps its descriptor in its slab
slot and writes it when the pacer lets it out. A packet whose descriptor cannot be written
(no ID negotiated, longer than 255 bytes, or a frame number the counter refuses) is sent
without one rather than dropped. The active-decode-targets bitmask is passed through as
the source wrote it, not narrowed to the leg's target.

**Presenting packets.** Each packet of the down-track's **current** source and of its
**pending** source (the source of a target that has not yet taken effect) goes to
`Rewrite.on_packet` (RTC-5 step 2). A filter that passed only the current layer would never
show `Rewrite` the pending layer's keyframe. The router fills rtp's `Unit` per packet from
`media_bits`: `start` and `end` from the descriptor or the marker bit (a VP8 packet with
`S = 1` and partition index 0 starts a frame; a nonzero partition with `S = 1` does not),
and `keyframe` from the codec's keyframe test, which reads only the bytes an end-to-end
encrypted sender leaves clear (W25): VP8's `P` bit in the first frame byte (RFC 7741 §4.3),
VP9's descriptor alone (`B = 1`, `P = 0`, SID 0 or no `L`; RFC 9628 §4.2, as libwebrtc's
depacketizer, never the uncompressed header), an H.264 IDR slice or SPS by NAL type, with
the slice's `pic_parameter_set_id` and the SPS and PPS it names, and AV1's `N` bit with the dependency descriptor. The
RTP S9 unit adapter fills the 32-bit `needs` and `switches` masks from the map above.

**End-to-end encrypted media.** A publisher using an encoded transform (SFrame, DAVE)
encrypts every frame byte past a codec's clear range, and the router routes on that range
alone. The ranges, the bytes libdave's `codec_utils.cpp` and the sframe package both keep
clear: the RTP header, extensions and padding; VP8's payload descriptor, then 10 frame bytes
of a keyframe and 1 of an interframe on the packet that starts the frame; VP9's payload
descriptor with its scalability structure; H.264's STAP-A and FU-A headers and sizes, SPS and
PPS whole, a slice's NAL header through `pic_parameter_set_id` (three `ue(v)`, read through
emulation prevention), and any other NAL's header; AV1's aggregation header, element
lengths, and each OBU's header, extension and size; RFC 2198's block headers. Opus frames
and RED block bodies are opaque. LiveKit's client turns RED off under E2EE
(`LocalParticipant.ts`, `disableRed: isE2EEEnabled`); the router does not depend on it.
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
   the step fits in the remaining budget. A step goes to the next encoding at or below its
   target that is not dropped (below), lowest first, starting from `Off`; from `Off` with
   none, to the lowest encoding. It costs that encoding's ingress rate (its planned rate
   from the publisher's layers allocation where known, else its measured rate) minus the
   rate of the level it leaves, and never less than 0, so no step pays for another. A
   down-track with no next step stays where it is. The first step is always taken unless
   the room declares `pause`; when it does not fit, the
   remaining budget becomes 0. With `pause`, a first step that does not fit is not taken,
   and the down-track stays `Off`. A
   down-track whose later step does not fit stays where it is for the rest of the
   computation. The others go on.
5. The computation ends when a full round raises nothing.

Without `pause` (the default) the budget therefore never pauses a video down-track: one
the estimate cannot carry forwards its lowest encoding, at its target's own spatial and
temporal layers, as LiveKit's allocator does with pause disallowed (its default). A leg
that goes `Off` on a budget sends no media, so only a probe can raise its estimate (2651).
The result is deterministic and computable by hand.

**Probing.** When the computation leaves any video down-track of the subscriber below its
target's reach (`Off` included), the subscriber's **want** is `needed + needed / 4`,
where `needed` is the rate of every audio down-track plus, for each video down-track with a
`Layer` target, the rate of its target's **reach**, the highest encoding at or below the
target that is not dropped (else the lowest); a down-track below its reach is short.
Otherwise the want is 0. The
quarter is headroom for the measurement (TWCC's 250 µs arrival quantum). Each poll, a
subscriber whose want exceeds its estimate asks its session to probe. The session asks
rtp's `bwe.probe(want, now)` (RTP-9) and hands any cluster to its pacer (RTP-10), and
starts none when:

- it has no **pad track**, a live video down-track whose section negotiated TWCC and either
  agreed RTX or, without RTX, can take RTP-11's `pad` now;
- `bwe.probe` answers none (a cluster outstanding, the loss estimate decreasing, or the
  want at or below the target);
- the target has not risen since the last cluster started, and that was less than 5 s
  ago (libwebrtc's `kAlrPeriodicProbingInterval`, so a link that cannot carry more is not
  probed at every poll), or less than 1 s ago while within 5 s of the session's first
  cluster, so a first cluster that got no result, with feedback not yet flowing, does
  not hold the leg on its lowest layer for 5 s. The window and the spacing are those of
  libwebrtc's repeated initial probing (`initial_probing` 5 s,
  `kMaxWaitingTimeForProbingResult` 1 s). There it is opt-in, runs only while no
  allocation is known and repeats at 3 times the estimate; here it always runs, a want
  is always known, and each retry asks what any cluster asks. libwebrtc's paired start
  clusters at 3 and 6 times the start rate are not adopted: `bwe` keeps one cluster
  outstanding, and each round already doubles. A cluster that raised the target lets the
  next start at once.

Each `Probe(id, pad)` the pacer answers goes out on the first pad track: on an RTX track an
RFC 4588 padding-only packet (`rtx.write_probe`) at the track's next RTX sequence number and
its last media timestamp; on a media track, at the number and timestamp RTP-11's `pad`
answers, with the payload type of its last media packet, counted in the SR as a packet of
no payload octets ([RFC 3550](https://www.rfc-editor.org/rfc/rfc3550) §6.4.1) and kept out
of the egress history. Each carries the section's MID and the session's next transport
number, and the session reports it with `bwe.probe_sent(id, index)`. A media packet the
pacer lets out while the cluster runs is reported as a member too, as libwebrtc's
`PacingController` tags it, since it goes at the cluster's rate. A result that raises the
estimate moves it, and the next poll's allocation takes the next step.

**The publisher's layers allocation.** When the publication negotiated
`video-layers-allocation00`, its home unit reads the allocation on each fresh media packet
(RTP-3 `Allocation`). Chromium sends the same allocation on every simulcast stream, each
with that stream's index; one whose index is not the encoding it arrived on does not follow
the section's encoding order and is ignored. The empty allocation is always read.

- **Planned rates.** An active stream's cumulative target at its top layer (RTP-3
  `stream_kbps`) becomes the encoding's **planned rate**, and allocation (below) costs a
  step at the planned rate where one is known, else at the measured rate (step 4 of the
selection below). A stream the
  publisher stops keeps its last planned rate, so un-pausing it is priced before any packet
  is measured. A changed planned rate reallocates the publication's down-tracks.
- **Dropped streams.** An encoding is **dropped** when the publication's published mask
  has it on and the publisher stopped it on its own: the newest allocation leaves it
  inactive, or it sent nothing in the last two closed rate windows (one, for an encoding
  never measured) while the publication sent, as LiveKit's stream tracker marks a layer
  stopped. Its next packet, or an allocation that makes it active, ends the drop. An
  encoding the mask turned off is paused by the router, not dropped. Turning an encoding on
  again clears its drop until its next allocation or rate windows say otherwise.
- **Moving away.** A down-track whose effective encoding is dropped forwards the highest
  live encoding below it, else the lowest live one above it, at its own spatial and
  temporal layers; with none live it stays. When the dropped set changes, the home unit
  reallocates its local subscribers of the publication and points each affected local
  down-track again at once, which asks for the new encoding's keyframe, so it switches at
  the next switch point with no freeze or pause-delay wait. Each `Media` message carries
  the dropped set and the planned-or-measured rate to the remote units, which do the same
  with their own down-tracks when the set changes.
- **Demand stays on the target.** Allocation never gives a dropped encoding while a step to
  a live one exists, and never prices one, so a stale or unmeasured rate can neither pay
  for another down-track's step nor leave a leg waiting on a stream that is not coming. A
  local mask still holds every dropped encoding at or below a down-track's target, so a
  stopped encoding stays wanted and the publisher can resume it; on its resume the
  allocation takes the step again at its known rate.

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
same `Keyframes`, keeping its outstanding request. It never makes a new one. Every FIR
sent, a repeat too, takes the next command sequence number (RTP-6): Chromium answers a
number once, so a repeat at the old number after a lost keyframe would hold a layer switch
forever (N1, 2779). `Keyframes` coalesces, so N subscribers asking
at once cost one request. The router calls `keyframe_seen` at each keyframe it reads.

**Publisher pause (dynacast).** For each publication, each unit computes its **local
wanted mask**: bit `e` is set when one of its down-tracks of the publication has an
effective target, a current layer or a pending layer on encoding `e`, or a target at or
above a dropped encoding `e` (below), so the publisher can resume it. An old encoding still
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
- a publication is **speaking** when `score ≥ amp(35)`;
- beside the score, the home unit keeps a **murmur** by the same steps over every audible
  level, `l ≤ 102` (the faintest level whose `amp` is not 0), in place of `l ≤ 35`: its
  own counted duration and loudest level, the same 160 ms floor and the same smoothing.
  It reads no `V` bit either, and an 800 ms expiry sets it to 0 with the score. A muted
  leg (level 127) or one without the extension has murmur 0. The murmur ranks only the
  audio gate's free slots (below); speakers, `Event.Speakers` and `V` read the score
  alone.

The speakers are the speaking publications ranked by these integer scores, then by lower
participant ID. Q16 quantization can tie two scores that LiveKit's floats would order, and
the tie-break then decides. That is the specified rank. Each 400 ms in which the top 8
changed, the control unit emits `Event.Speakers(room, top, count)`.

**Audio forwarding.** With `audio_forward = n > 0`, each subscriber has `n` slots. The
highest-ranked speakers, excluding the subscriber itself, take them first. When fewer than
`n` speak, the slots left free go to the subscriber's other audio publications that are not
speaking and have a murmur above 0, by murmur, then lower participant ID, then lower
publication ID, as Jitsi's loudest-N routing ranks every endpoint by its smoothed energy
with no speaking threshold. So a quiet talker under −35 dBov is heard while a slot is free,
and a muted one never is. No subscriber hears more than `n`. Other audio down-tracks are
held at `Off` until they take a slot again. Opus needs no keyframe, so the gate
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

**Suspend.** `suspend(r, room, cause)` uses the same machinery for a whole room without revoking
anyone. `router.Change.Suspend` carries the cause to each unit. A room keeps a set of suspension causes: `Signaling` (RTC-8's signaling loss),
`Lease` (a cascade edge whose lease ran out, [SFU cascade](sfu_cascade.md) CAS-5) and `Term`
(a cascade home change, CAS-7). The room is suspended while the set is not empty. Every unit, at its fence point, stops routing media and repair for every session of
the room, and removes every queued entry whose tag names the room and has a `src`, in step 2's
queues and messages. Entries with no `src` (STUN, DTLS, RTCP) stay. Ingress stays up, so ICE consent and DTLS keep running and RTCP receiver reports
continue. The control unit returns when every unit has acknowledged it, under the same rule
as a revoke. `resume(r, room, cause)` removes only
`cause` from the set, the same way, and the room routes again only when the set is empty. So
RTC-8's signaling recovery cannot lift a lease or term suspension, and a lease renewal cannot
lift a suspension that waits for a proven roster. A `join`, `subscribe` or `set_target` on a
suspended room is `Suspended`. RTC-8 suspends and resumes with cause `Signaling`.

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

```hirsel signatures webrtc_sfu/signal
pub type Signal: codec = Offer(u64, Bytes) | Answer(u64, Bytes) | Candidate(Bytes) | EndOfCandidates | Glare(u64) | Subscribe(u64, router.Target) | Target(u64, router.Target) | Unsubscribe(u64) | Published(u64, u64, u8, u8) | Unpublished(u64) | Layers(u64, u8) | Speakers(List[u64], u8) | Fenced(u64, u64) | Refused(u32) | Servers(Bytes, Bytes)
pub type Embed[M] { wrap: own[send] fn(take Signal) -> M, unwrap: own[send] fn(take M) -> Option[Signal], publish: own[send] fn(u32) -> bool }
pub type Build: cross { who: peer.Participant, home: u16, peer: u32, version: u64, offer: Bytes, publish: bool, ice_ufrag: Bytes, ice_pwd: Bytes }
pub type Built: cross { answer: Bytes, recvs: List[u8], sends: List[(u8, sdp.Media)] }

pub fn adapter[M: codec](room: peer.RoomId, limits: router.RoomLimits, take embed: Embed[M], units: u16) -> Adapter[M]
pub fn you[M](mut a: Adapter[M], peer: Option[u32])
pub fn room_in[M: codec](mut a: Adapter[M], take input: runner.room_client.RoomIn[M], mut out: List[runner.room_client.RoomOut[M]])
pub fn step[M: codec](mut a: Adapter[M], mut u: units.Unit, now: u64, mut out: List[runner.room_client.RoomOut[M]]) -> Result[(), router.RouterError]
pub fn take_builds[M](mut a: Adapter[M]) -> List[Build]
pub fn built[M](mut a: Adapter[M], who: peer.Participant, take result: Result[Built, router.RouterError], mut out: List[runner.room_client.RoomOut[M]])
```

```hirsel signatures runner/room_client
pub type Incarnation: key + send = Incarnation(Inline[u8, 16])
pub type RoomIn[M] = Part(Incarnation, u64, bool, bool) | PartEnd | Joined(u32) | Left(u32) | Message(u32, M) | Lost
pub type RoomOut[M] = ToPeer(u32, M) | ToRoom(M)
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
  reported a gap), the adapter calls `suspend(room, Signaling)` (RTC-7) before it takes any other
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
  - new peers in the snapshot are admitted as new identities. Then `resume(room, Signaling)` is called.

  Revoking every pre-gap session on any loss would also be sound. It is not the end state:
  the relay change costs one 16-byte incarnation, one revision counter and one flag per Part,
  the Hirsel room client (F1) and the SDK (F2) already read the head, so no consumer migrates, while a blanket revoke would force every
  participant in the room through a new ICE and DTLS handshake on every reconnect of the
  service's room session. The proof keeps an unchanged member's media flowing across a gap
  and still fails closed whenever continuity is not proved.
- **Refusals.** A `Signal` the router refuses is answered `Refused(code)` to the sender, with
  the `RouterError` tag as `code`. A malformed `M` never reaches the adapter, because the
  room's codec check refuses it first.
- **Departures.** A publication lives as long as its owner's admission. When an admission
  ends (its `Left`, its absence from the snapshot that ends a loss, or a refused re-offer
  or session `Shut` that sends the seat to rejoin), the adapter drops each of
  its publications. It sends `Unpublished(id)` to the room once per publication, and frees
  the section each subscriber held for it. The revoke ends the subscribers' down-tracks on
  every unit, and it runs before any later `Subscribe` in the control order, so a freed
  section is never shared by two tracks. `PeerLimits.down_tracks` bounds a session's live
  down-tracks, not the tracks it has ever held: an ended track's slot is taken by the next
  one, and a re-offer names no SSRC for a section whose track ended. Ending a track ends
  its media, RTX and FlexFEC SRTP send streams (`end_send`, SRTP-3) and its sender
  reports, so the session's SRTP bound of three streams per down-track plus two counts
  live tracks too. Law W33. An `Unsubscribe` of a
  publication the peer does not hold, because it was never subscribed or its owner left,
  is already done and changes nothing; it is not refused, since a client that hears
  `Unpublished` and unsubscribes is racing nothing. mediasoup closes each consumer when its
  producer closes (`producerclose`), and LiveKit removes every subscriber's down-track when
  a published track closes (`MediaTrackReceiver.Close`); both free the subscriber's
  transceiver for reuse.
- **Joining peers learn the room's publications.** At admission, after `Servers`, the
  adapter sends the new peer alone one `Published(peer, id, kind, encodings)` for each live
  publication (`ToPeer`), as LiveKit's join response lists the other participants and their
  tracks. A publication made later reaches it in the room's `Published`. A client keys
  publications by id, so an id it hears twice is one publication.
- **Offer size.** An offer travels in one room message, and the adapter builds no session
  from an offer longer than `signal.OFFER_MAX` (32,000 bytes, RTC-1's `TooLarge` bound). It
  answers such an offer `Refused(11)` before any build. `MESSAGE_MAX` (32,768) is not raised
  for SDP. A browser's offer grows with its codecs: Chrome with one audio and three
  video send sections and 8 audio and 8 video receive sections offers about 71 KB, most
  of it the H264, VP9, AV1 and HEVC profiles, FEC and audio codecs the SFU's policy never
  accepts (`sdp.default_policy`). With `RTCRtpTransceiver.setCodecPreferences` on every
  transceiver, Opus for audio and VP8 with its rtx for video, the same offer is 22.4 KB.
  So a client sets codec preferences on every transceiver, and keeps its receive sections
  to what it will use. The N1 page does this (`preferVp8`), and checks its offer against
  `OFFER_MAX` before it sends, throwing an error that names the size and this fix.

  Chunking or compressing the offer is not the end state. Chunking would raise RTC-1's
  bound with the message size it is derived from, and add per-peer reassembly state that
  must survive relay drops (`Lost`) and be bounded per peer. Compression would add a
  bounded inflater in Hirsel and `CompressionStream` in every client, to carry bytes the
  SFU discards. Filtering makes the offer smaller than either, with no new state.

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

## RTC-9: Recording

`media_record` (core/programs/media_record, its README is the package text)
records a room's publications to files. It is a virtual down-track: a media
unit with no PeerSession. Its seats are participants admitted with the
recorder's unit as home, and it records what they subscribe to.

- **A unit like the others.** The control unit sends it every `Order`, as
  it does every unit (RTC-5). The recorder applies each order to an
  observer Router and answers with its `Ack`. Home units post it the
  plaintext `Media` of each publication it subscribes to (fan-out step 3),
  and the publishers' `Sender` reports. It posts each down-track's
  `Wanted` mask and its `Key` requests to the home unit (RTC-6). It holds
  no ICE, DTLS or SRTP state and sends no RTP.
- **The fence.** RTC-7 applies unchanged. Step 4 drops a `Media` or
  `Sender` message of a fenced generation, or of a suspended room, when it
  is taken. Step 3 removes the down-tracks. Before it acknowledges, the
  recorder drops what those tracks buffer and closes their files. So once
  `revoke` returns, no frame of the fenced participant lands in a file.
  A suspension drops the room's buffered frames the same way.
- **Frames and time.** Each recorded encoding has one `rtp_jitter` buffer.
  Frames are placed on one timeline from the sender reports (rtp_jitter
  `sync`), whose zero is the recording's first frame. Each file's times
  rise.
- **Loss.** On a gap, a video track asks the home unit for a key frame
  (`Key(why: Loss)`, coalesced into one PLI or FIR upstream) and drops
  non-key frames until the next key frame.
- **Files.** One per down-track, never transcoded: Ogg Opus, WebM for VP8,
  VP9 and AV1, and fragmented MP4 (`avc3`) for H.264.

The laws are the package's: real FFmpeg frames through each codec's RTP
packetization, against the frames sent and the sender reports. ffprobe and
ffmpeg decode the files in `test-vectors.sh`.

## RTC-10: Pooled rooms

*Specified 2026-11-26 (sfu-pool-media-w2-spec-2888, Horizon H3 part 1); design review
applied the same day (media-w2-spec-fix-2918). Slice P1 built 2026-11-26
(rtc10-p1-room-keys-2930, law W35); slice P2 built 2026-11-26 (rtc10-p2-room-control-2956,
law W37, `units.Plane`). Slice P3 in part 2026-11-26 (rtc10-p3-pool-service-2976):
`serve_pool` with per-room adapters and room-client tasks, `serve` removed, `room_pps` and
`open_pps` (W38), per-room signaling loss (W39), the three fault tiers (W36), the
default `inflight`, and the ledgers, primary units and pool-start row reservation (W34).
P4 built 2026-11-26 (rtc10-p4-nightly-3039), with the N3 runner and down-track
reclamation. Slices and laws W34 to W39 below.*

Before RTC-10, `signal.serve` (S7) served **one** room: its `Service` named one `RoomId`, one `Dial` and one
`RoomLimits`, binds `u` ports, and every unit of that service routes only that room. A host
that serves many small rooms runs one service per room, each with its own ports, unit tasks,
certificates and receive pools, about 64 MiB per active room in the 2026-11-25 dogfood. RTC-10
serves many rooms from one shared pool of units and one fixed set of ports. The router was
built multi-room from S4 on (`open_room` takes a `RoomId`, every `Tag` carries its room, and
RTC-7's fence tables are per room), so most of RTC-10 is the service around it: room
admission and sizing, a control plane that does not serialize rooms behind each other, and
isolation, so one room's load or failure leaves the others' service unchanged.

### Shape

```hirsel unbuilt
pub type PoolLimits {
    units: u16,                // 1 to 64, one port and one unit task each
    unit_bytes: u64,           // each unit task's memory (CONC-15), as Service.unit_bytes
    rooms: u32,                // live rooms at once, across the pool
    sessions: u32,             // live PeerSessions per unit
    inflight: u16,             // control calls in flight at once, pool-wide; 8 by default
    media_take: u32,           // media-inbox messages a unit takes per turn
    room_pps: u32,             // authenticated datagrams per second per publisher session
    open_pps: Option[u32]      // SRTP open attempts per second per unit, before authentication
}
pub type PoolOrder[M] = Open(name: RoomName, dial: runner.room_client.Dial, limits: RoomLimits, embed: Embed[M]) | Close(name: RoomName)
pub type PoolReport = Opened(name: RoomName, room: RoomId) | Refused(name: RoomName, why: PoolRefusal) | Closed(name: RoomName, room: RoomId, why: CloseWhy)
pub type PoolRefusal = Rooms | Bytes(unit: u16, need: u64, free: u64) | Exists | Limits(RouterError)
pub type CloseWhy = Asked | Fault(RouterError)
pub type RoomName = RoomName(Inline[u8, 32])
pub type Pool[M] { tasks: Tasks, net: Net, clock: Clock, random: Random, binds: List[Addr], policy: sdp.Policy, session: peer.PeerLimits, limits: PoolLimits, orders: Receiver[PoolOrder[M]], reports: Sender[PoolReport], until: u64, relay: Option[RelayLink] }

pub fn serve_pool[M: codec + send](take p: Pool[M]) -> Result[(), ServeError] waits
pub fn room_cost(limits: RoomLimits, session: peer.PeerLimits) -> RoomCost
pub type RoomCost { replicated: u64, per_session: u64, per_remote: u64, control: u64, bridge: u64, cascade: u64 }
```

`serve` becomes `serve_pool` with one `Open` already queued: the single-room service is a
pool of one room, and there is no second entry point to keep (`/wholehog`). The application
sends `Open` and `Close` orders on `orders` and reads `PoolReport`s. `RoomName` is the
application's name for the room, 32 bytes, opaque to the pool. The pool keeps one
`Adapter[M]` and one `room_client` membership per open room on the control unit (unit 0). `relay` is the TURN relay link every room mints its credentials from (TURN-11).
`Pool.cascade` is added with [SFU cascade](sfu_cascade.md) CAS-6, which is not yet built.
Until then `RoomCost.bridge` and `RoomCost.cascade` are 0.

**Internal room ids are never reused.** `Open` draws the room's `RoomId` from a pool-wide
counter that only rises, and `Opened` reports it. Closing a room and opening one with the
same name gives a new `RoomId`. Every `Tag`, every cross-unit message, every fence table and
every 5-tuple row names the `RoomId`, so a message, a queued datagram or a late `RepairReply`
of a closed room can never be taken as the new room's: it names a room no unit has open, and
the router drops and counts it (`stale`). Without this rule, a reopened room's adapter would
hand out ids and epochs from 1 again, and a `Media` message posted before `close_room` and
taken after the reopen would match a new admission's `(id, epoch)`, which RTC-7 promises
never happens (law W35).

**Every key carries its room.** Ids and epochs are dense per room (RTC-8, Identity), so
`Gen(id, epoch)` alone names a participant only inside one room. Each key that is a `Gen`
alone today gains the room: `units.Event.Offer` and `units.Event.Shut` carry a
`Participant` (room and `Gen`), `units.Event.Layers` carries the publication's room, and the
control unit routes each `Event` to its room's adapter by that room. `PublicationId` stays
pool-wide (`router.next_publication` is one counter on the control unit), so publication ids
never collide across rooms either. The build greps every `Gen`-keyed table and message in
`webrtc_sfu` and either adds the room or states in a comment why one room is implied.

### Sizing and admission

A room's memory is fixed when it opens (RTC-5 Bounds): `room_cost(limits, session)` answers
two exact numbers from the same sizes the router allocates with:

- `replicated`: what **every** unit allocates for the room at `open_room`: the room record,
  the fence table (`ids` entries), the roster rows, the publication table
  (`publications` × `participants` rows with their wanted masks);
- `per_session`: what one admitted participant's home unit allocates: one PeerSession at
  `session` (`PeerLimits`: sections, down-tracks, RTX history, pacer), its publications'
  ingress RTX rings (`floor(rtx_bytes / 1500)` slots of 1,500 bytes, home unit only) and RED
  rings, and its down-tracks' rewrites, histories and FEC groups. A down-track on a unit
  other than its publisher's home also holds that publication's RED ring there, which the
  join that subscribes charges to that unit.
- `per_remote`: what one down-track on a unit other than its publisher's home adds there: the
  publication's RED ring on that unit, and the pacer headroom for the `Shared[Bytes]` the
  home unit's fan-out hands it, up to the pacer's bound (RTC-5 step 3). A `Shared` buffer is
  charged to the unit that allocated it until its last reference drops, so a remote
  down-track's headroom is the bound on references it may hold, not a share of the buffer;
- `control`: what the room holds on unit 0: its `Adapter[M]`, its `room_client` membership
  and that task's channels, and its pending `Build`s, each up to the room's bounds;
- `bridge`: the MoQ bridge's reassembly buffers when the room is bridged ([MoQ](moq.md)
  MOQ-8), one per bridged layer at its maximum frame size, else 0;
- `cascade`: per cascade link the room is attached to, the link's room state
  ([SFU cascade](sfu_cascade.md) CAS-6), else 0.

`room_cost` is computed from the same constants the allocations use, never from a table of
estimates, and law W34 checks it against the charges the units actually record. As built
in P1, `signal.room_cost` prices each term from the router's, the session's and the
adapter's allocation bounds (each `rtp` constructor has a `*_heap` price beside it).
The session's DTLS transport sets its MTU, timers, message width and ahead window.
The room ledger reserves its receive-side handshake budget as part of `per_session`.
The service checks the configured MTU and message width against the media unit's grant
before binding. `bridge` and `cascade` are 0: no bridge or cascade attaches to a
pooled room in this build (CAS-6 and MOQ-8 add them), so W34 has nothing of theirs to
measure yet. Each unit keeps a **ledger**: `unit_bytes` minus the fixed per-unit cost
(`units.fixed_bytes` plus `signal.serve_bytes`), minus every open room's `replicated`,
minus every homed session's `per_session`, minus every remote down-track's `per_remote`.
Unit 0's ledger also carries each room's `control`, `bridge` and `cascade`.

**The fixed cost (as built in P3).** `units.fixed_bytes` is the unit's control and media
inboxes at their byte budgets, its outbox, its receive, batch and slot buffers, the socket's
slots, and `router.pool_bytes`: the router's pool-wide row lists, which `router.reserve_pool`
allocates at exactly their bounds (`list.with_capacity`) before the unit serves any room.
The bounds (`router.pool_bounds`) are `rooms` rooms, up to `units × sessions` seats
subject to the `u16` participant field per room, and `sessions` members, parked sessions
and 5-tuple rows on each unit. Unit 0 admits a participant once its session is reserved on
a ledger. A seat has at most `session.sections` publications, one per negotiated section,
and at most `session.down_tracks` down-tracks. The router enforces these together with
each room's own bounds. Without the reservation each list grew as rooms filled,
by its growth's slack and by the capacity it keeps once rows go: W34 measured a join's
charge at 31,440 bytes against 5,473 priced. So `replicated` prices only what is
per room beyond those rows (the fence table, top speakers, and each publication's
encodings and RED ring) and `member_bytes` only the ingress RTX rings. `signal.serve_bytes`
is a worker's roster and build inboxes, and unit 0's hello and finished inboxes and ready
channel. `serve_pool` refuses to start (`Router(Capacity(..))`) when a unit's fixed cost
exceeds `unit_bytes`. `control` includes the room client task's grant
(`ROOM_CLIENT_BYTES`).

**Left out of `room_cost` (as built in P3), each sized by input or by the application:**
a session's agreed SDP and section mids, ICE credentials, outgoing DTLS flights, SRTP state, and the
offer text a re-offer keeps until it is answered; the room client's Dial, Embed and
membership, and its entry and message channels (sized by the message type `M`); the
adapter's per-seat `sends` and `subs`, and its `jobs`, `answers`, `former` and `revokes`
queues; unit 0's `Ledger.held` and `Ledger.rooms` and `Rooms.served`; and a subscribe's
transient scratch. One list still grows under churn without a bound, an open red against
Isolation item 1: `PeerSession.used` keeps every SSRC the session draws.

**Reservations are linearized on unit 0.** Unit 0 keeps every unit's ledger, and reserves
from it before it posts any change that allocates: an `Open`, a `join`'s `Build` (reserved
when the `Build` is queued, refunded if the build fails or is discarded), a subscription that
adds a remote down-track. Workers only allocate what unit 0 has already reserved. So two
pending `Build`s can never both pass a check that only one fits, and the reservation, not the
allocation, decides admission.

- `Open` is admitted when the pool has fewer than `rooms` live rooms and every unit's ledger
  holds `replicated` plus `per_session` for at least one participant. Otherwise it is
  `Refused(Rooms)` or `Refused(Bytes(unit, need, free))`, and nothing is allocated. A room
  is never opened on some units and not others.
- A `join` is admitted on a home unit whose ledger holds `per_session` and whose live session
  count is below `sessions`. When no unit has room, the join is `Limit(sessions)` and the
  peer is answered `Refused` as any refused join is (RTC-8). Admission never evicts, and no
  cap is hidden: every refusal names its bound.

**Home units.** At `Open` the pool picks the room's **primary unit**: the unit with the most
free ledger bytes, lowest index on a tie. A participant's home is the room's primary unit
while it has room for the session, and otherwise the unit with the most free bytes. Packing a
room on one unit means one decryption and zero cross-unit copies for most of its fan-out
(RTC-5 step 3 runs only for spilled subscribers), and it keeps a hot room's work mostly on
one unit. Spreading every room across every unit, as one-room `serve` does, would double
the copies and spread one room's overload over every room's units.

**Primary-unit affinity is kept** (design review, PO2): the cost of a hot room on one unit
is bounded by `room_pps` and RTC-5 Bounds, and spreading would make every room's fan-out
cross units.

**Ports.** The pool binds `units` ports once. Every room's sessions advertise their home
unit's port, so the port count is fixed by the pool and not by the room count. One-room
`serve` used the same rule: the home unit is a signaling choice, and no steering is needed.

### The control plane

Unit 0 runs every room's adapter. Before P2, one control call was in flight at a time
across the pool (`units.begin` refused a second with `Exists`). With many rooms, one room
whose units are slow to acknowledge, or one room churning, would delay every other room's
joins and revokes. RTC-10 orders control **per room**:

- **Per-room order, pool-wide window.** Each room's changes are applied in that room's
  order on every unit, as RTC-5 requires. Changes of different rooms may be in flight
  together, up to `inflight` across the pool and one per room. Every unit takes control
  messages from one FIFO inbox, and the control unit posts each room's changes in that room's
  order, so each unit applies each room's changes in order. Ordering across rooms is not
  required, because no change names two rooms. Each order and acknowledgement carries
  `(room, seq)`, and `seq` counts per room.
- **Room clients off the turn loop.** Each room's `room_client` runs in its own task, which
  makes the blocking calls (`join`, `rejoin`, `send`, `close`) and posts what it receives to
  unit 0 on the room's channel, marking the room in unit 0's **ready set**. Unit 0 never
  blocks on a room client and never polls an idle one: each turn it takes only the rooms in
  the ready set. One room's slow relay or rejoin backoff therefore never stalls another
  room's control.
- **Fair turns.** Each control-unit turn admits pending changes from ready rooms in
  round-robin order, starting after the room served first in the previous turn, until
  `inflight` is reached. A room with a backlog never takes a second slot while another room
  waits for one.
- **Inboxes are sized, and a full one is handled.** Each worker unit's control inbox holds at
  least `inflight + rooms` messages, which is every change that can be in flight to it at
  once (one per room plus a fence slot per room), and unit 0's inbox holds at least
  `(units − 1) × (inflight + 2 × rooms)` messages: those acknowledgements and one room fault
  per room from each worker (Fault containment). Both are sized at pool start from
  `PoolLimits` and charged to the fixed per-unit cost. A post that still finds an inbox
  `Full` is never ignored: the change stays pending in its room's order and is posted again
  next turn, and the room's fence waits for it. An inbox overflow is not a pool fault
  (design review, PO1).
- **Revokes first.** A pending `Revoke`, `Suspend` or `Close` of a room is admitted before that
  room's other pending changes, and a slot is held for it when every slot is taken: `inflight`
  is the window for other changes, and fence changes may exceed it by one per room. A fence
  therefore waits at most for its own room's change in flight, never for another room's
  backlog.

### Isolation

The pool isolates rooms against **outside input and load**: another room's peers, its
signaling, its publishers' rates and its subscribers' estimates. It does not add mechanisms
that guard against the router itself being wrong. A bug in the router is fixed in the router
(host rules, "trust our own implementation").

1. **Memory.** A room only ever allocates within its `room_cost`, reserved at `Open` and at
   each join. One room's growth is refused at its own bounds (`Limit`) and never takes memory
   another room was admitted with (W34).
2. **Fault containment, in three tiers.** `turn`, `take_media` and `take_datagram` no longer
   return a room's error to `serve`, which ends the whole service today. Each `RouterError`
   site has one scope, and the build lists each site with it (law W36):
   - **Peer.** A `PeerError` of one session, or a re-offer the room's bounds refuse, ends only
     that peer, as RTC-8 already answers a refused join or re-offer. The room runs on.
   - **Room.** An error in the room's shared state is that room's fault. It is reported in
     the room's `Event`s, and the control unit closes the room with the same fence as
     `Close`, reporting `Closed(name, room, Fault(e))`.
   - **Pool.** Only errors with no room end the pool: the socket's and a failed spawn. A full
     control inbox is not one (the control plane above).

   The sites, as built in P3. `router.deliver` (a unit's datagram): a `Peer` error of the
   session, or `NoParticipant` for a row whose session has gone, drops only that datagram;
   any other error of `receive` or of keeping the row (routing, allocation, a restarted
   stream, the row table's `Capacity`) is the room's fault. `router.accept` (a media-inbox
   message, taken or held): a `Peer` error drops only that message; any other is the
   room's fault. A re-offer the room's bounds refuse raises `Shut`, and the adapter
   revokes that peer, which rejoins (RTC-8). A unit's own receive slot that cannot grow
   (`Capacity`) ends its `turn`, and so the pool. A room's fault holds the room on the
   unit that raised it as a suspension does, under its own cause, from that turn, and is
   raised once per unit: a worker sends it to unit 0 with its acknowledgements
   (`Control.Fault`), which are retried, never dropped, when unit 0's inbox is full.

   **Closing is a fence.** `router.Change` gains `Close(room)`, applied at each unit's fence
   point like a revoke. A unit applying it removes the room's sessions, down-tracks and
   rows, drops the room's sessions parked for a `Build` (`r.parked`), and treats every queue
   entry and message of the room as dead. RTC-7's tag check `dead(r, t)` answers `true` for
   any tag whose room the unit does not have open, so the uncommitted suffix of a partial
   `udp_send` and every message posted before the close are purged, not sent.
   A `Build` still queued
   for the room is discarded when taken and its reservation refunded. A `revoke` or
   `suspend` in flight when its room faults returns `Ok(())` once the `Close` fence has
   completed, because a closed room routes nothing.
3. **Signaling.** Each room has its own room client membership. A `Lost` suspends only its own
   room (RTC-8), and its rejoin backoff is its own. One room's relay session failing leaves
   the others' media and control running.
4. **Ingress rate.** Each publisher session may deliver at most `room_pps` media datagrams
   per second to routing, measured by a token bucket of one second's depth. A datagram is
   classified first (RTC-3): STUN, DTLS and RTCP are not counted. An SRTP datagram is
   debited from its session's bucket only **after** SRTP authentication succeeds, so a
   datagram spoofed from a publisher's 5-tuple, which fails authentication, cannot spend the
   publisher's budget, and W19 holds unchanged. An authenticated datagram over the rate is
   dropped before routing and counted (`over_rate`) per room. RTC-5 Bounds already cap a
   room's down-tracks, so a room's fan-out work per turn is at most `room_pps` × its largest
   publication's down-tracks per second. Authentication itself is bounded per unit, not per
   room: with `open_pps` set, a unit attempts at most that many SRTP opens per second across
   its sessions, and drops and counts (`over_open`) the rest before decryption; that cap is
   part of the shared pre-authentication pool (item 6), not of a room's isolation.
5. **Media inbox, drained fairly.** A unit takes at most `media_take` messages from its media
   inbox per turn. It sorts what it takes into per-room queues and routes them round-robin by
   room, at most `media_take / r` messages of each of the `r` rooms with messages waiting
   before any room's next one, carrying the rest to the next turn ahead of new messages. A
   message that finds the inbox full, or a room queue past its share of the inbox, is dropped
   and counted against the room it names (`full`, today a unit counter). So one hot room
   costs other rooms at most their share of a turn, never a turn of its own backlog.
6. **What is not isolated: before authentication** (design review, PO3). The receive pool of a unit's port (NET-16 `slots`) is shared by
   every room homed there, because a datagram's room is known only after its 5-tuple
   lookup. An unauthenticated flood at a port slows every room on that unit, as it slows a
   one-room service. Pre-authentication cost stays bounded per datagram: STUN without a
   matching ufrag gets no answer, and DTLS without a known 5-tuple gets no state (RTC-3).
   Per-source limits belong to the TURN relay and the host's firewall, not to the router.
   The line is authentication: before it, a unit's receive pool and `open_pps` are shared by
   its rooms; after it, items 1 to 5 isolate each room. NET-16 gains no per-room receive
   quota.

**Fence latency.** RTC-7 bounds a fence by the slowest unit's turn plus 50 ms. In a pool that
bound needs a bounded turn, and RTC-10 gives one: a turn takes at most one `udp_recv` batch
(`slots` datagrams), at most `media_take` media messages, and the control messages of at
most `inflight` plus `rooms` calls, and each of those does bounded work by RTC-5 Bounds.
So a revoke in room A waits for at most one turn of room B's bounded work on each unit, and
never for room B's backlog.

### Pooling laws

Harness: `pool_laws.hirsel` in `webrtc_sfu`, over the S5 test seam (units stepped by hand
over `sim.udp`), unless named otherwise. Each is red on main before its slice.

- **W34 as built (P3)** is two laws. `pool_room_cost_is_exact` (pool_cost_laws.hirsel)
  measures, per shape, what `peer.open` charges (by a bound's headroom, less its
  input-sized copies), which must be at most `session_heap`, then opens and fills the
  room on one router whose rows `reserve_pool` holds (itself within `pool_bytes`), within
  `replicated + participants × (member_bytes + session_heap − opened)`, and checks the
  room is full by the router's own refusals. `pool_ledger_reserves_what_fits`
  (pool_ledger_laws.hirsel) runs the ledger in the hall: three rooms admitted, the fourth
  refused `Bytes(0, need, free)` from `room_cost`'s terms with nothing reserved; two
  pending builds where one fits; a remote down-track's `per_remote` and its unsubscribe;
  a leave's refund; and a close that returns the room's share on each unit. The per-unit
  live charge at every step and the remote `Shared` reference drop after `Close` are not
  measured: the hall's units share one limit. `pool_unsubscribe_frees_down_tracks`
  churns two subscriptions and checks that unsubscribe refunds the tracks' actual
  allocations and preserves the surviving track's SSRC.
- **W34 `pool_room_cost_is_exact`.** Open rooms with four `RoomLimits` shapes (the
  smallest; 256 participants and 8 publications each; 2 participants with 64 down-tracks
  each; RED and FlexFEC on), then fill each with joins up to its bounds. *Oracle:* the units'
  recorded charges (MEM-20 limit records), which the router's allocations make, not
  `room_cost`. On each unit, at every step, a room's live charge is at most what unit 0 has
  reserved for it, and the reservation is the hand-computed sum of `room_cost`'s terms:
  `replicated`, `per_session` per homed session, `per_remote` per remote down-track, and on
  unit 0 `control`, `bridge` and `cascade`. Two joins whose `Build`s are both pending when
  only one fits get one admission and one `Limit(sessions)`. A pool sized for exactly three
  such rooms admits three and answers the fourth `Refused(Bytes(..))` with the
  hand-computed `need` and `free`, and no unit's charge moved. After `Close`, once the last
  remote `Shared` reference drops, every unit's charge is back to its value before the
  `Open`, to the byte.
- **W35 `pool_reopened_room_takes_no_stale_message`.** Room `a` has publisher `p` on unit 0
  and subscriber `s` on unit 1. A `Media` message of `p` is posted to unit 1 and held in its
  inbox. `a` is closed and reopened under the same name; the new room admits two peers that
  get the same dense ids and epochs as `p` and `s`. Unit 1 then takes the held message.
  *Oracle:* RTC-7 and this section, by hand. The tap shows no datagram to the new `s`, and
  the unit counts the message `stale`. On a pool that reuses `RoomId`s the new `s` receives
  `p`'s old packet, so the law is red.
- **W36 `pool_room_fault_closes_only_that_room`.** Rooms `a` and `b` share units 0 and 1, each
  with a publisher and two subscribers across both units. Room `a` is driven into each
  room-scoped `RouterError` the build lists, with a partial `udp_send` of `a`'s media
  uncommitted on one unit and a `Build` of `a` queued, and with a revoke of `a` in flight.
  *Oracle:* by hand. `a` reports `Closed(.., Fault(e))`, every unit has dropped `a`'s state
  and parked sessions, the uncommitted suffix is not sent, the queued `Build` is discarded,
  the in-flight revoke returns `Ok(())`, `a`'s sessions get no datagram after the close
  returns, and `b`'s subscribers receive every packet `b`'s publisher sent in the same
  turns, with the same sequence numbers as a run without `a`. Each peer-scoped error the
  build lists (a re-offer that `down_tracks` refuses) ends only that peer, and its room runs
  on. On main the error ends `serve`, so `b` stops.
- **W37 `pool_revoke_waits_for_no_other_room`.** `inflight` distinct rooms other than `a`
  each hold one change in flight, and one of them, `b`, has a backlog of 100 joins; unit 1 is slowed by the seam so each acknowledgement takes 5
  turns. Room `a` revokes a participant. *Oracle:* by hand from the control-plane rules.
  `a`'s revoke is admitted in the turn it is asked, and returns after unit 1's next
  acknowledgement, not after `b`'s backlog. Unit 1's control inbox is then filled to its
  sized bound: no post is lost, and the pool does not fault. With one call per room, legal traffic
  puts at most one order per live room in a worker's inbox, below `inflight + rooms`; the
  P2 law therefore sizes the inboxes for `rooms` 1 and opens six rooms, so two fence posts
  find unit 1's inbox full (room admission against `rooms` is P3). With one call in flight pool-wide (main), it
  waits for the backlog, so the law is red.
- **W38 `pool_over_rate_publisher_costs_its_room`.** Room `a`'s publisher sends at 10 ×
  `room_pps` and room `b`'s at half `room_pps`, homed on one unit. *Oracle:* the token bucket,
  by hand. `a`'s routed count over 3 s is `room_pps` × 3 within one bucket's depth, `over_rate`
  counts the rest, and `b` routes every packet it sent. A sender spoofing `b`'s publisher's
  5-tuple at 10 × `room_pps` with packets that fail SRTP authentication leaves `b` routing
  every packet its real publisher sent (W19).
- **W39 `pool_signaling_loss_is_per_room`.** Two rooms on one pool. Room `a`'s relay session
  drops (`Lost`), its rejoin fails for 3 s, and its room client's `send` blocks throughout. *Oracle:* RTC-8 and this section. Only `a` is
  suspended. `b` admits a new peer and forwards media throughout. `a` resumes from its own
  proven roster.

N3 (nightly, Boat): 200 rooms of 4 peers with 720p video and audio on one 8-unit pool under
`sim.udp`, with one room revoked every second. The pool's p99 revoke latency stays under the
slowest turn plus 50 ms, and no room's received frame count drops while another is revoked.
The runner is `core/programs/webrtc_sfu/nightly/run.py`. Each room sends one 30 fps VP8
stream and 50 Opus packets per second to three subscribers. After warm-up and a reference
second, it revokes a publisher each second for 20 seconds. It checks each receiver's RTP
packet and frame counts and prints wall-clock revoke latency and turn duration.
The user timer in `deploy/rtc10` builds from main and submits N3 to Boat at 03:30 UTC.

### Pooling slices

| Slice | Content | Depends on | Laws |
| --- | --- | --- | --- |
| **P1** Room keys and close | `RoomId` from a pool-wide counter; `Participant` in `Event.Offer` and `Shut`, the room in `Event.Layers`; per-room `stale` drops; `room_cost` with all its terms; `router.Change.Close(room)` draining `r.parked` and discarding stale `Build`s; `dead(r, t)` true for a room not open (the `router_fence.hirsel` fix); RTC-7's suspend cause set (`Change.Suspend` carries its cause) | S7 | W35 |
| **P2** Per-room control | `(room, seq)` orders and acknowledgements; the `inflight` window, one per room, round-robin, fence slots; sized control inboxes and re-posting on `Full`; fair per-room media draining | P1 | W37 |
| **P3** Pool service | `serve_pool`, `PoolOrder` and `PoolReport`, one adapter per room on unit 0 and one room-client task per room with the ready set; ledgers with linearized reservations on unit 0; primary units; `serve` removed; the three fault tiers; `room_pps` after authentication and `open_pps`; `Pool.cascade` once CAS-6 is built | P2 | W34, W36, W38, W39 |
| **P4** Nightly | N3 on Boat | P3 | N3 |

Reds that may ride: N3 and any cost target. W34 to W39 are soundness or isolation laws of
their own slices and block them.

### Pooling: design review

Rulings of the media Wave 2 design review (VERDICT-MEDIA-W2, 2026-11-26), applied above:

- **PO1 (the `inflight` window and inboxes).** Control inboxes are sized from `inflight +
  rooms` at pool start, a `Full` post is retried, and overflow is not a pool fault (The
  control plane). The default `inflight` is calibrated against real runs in P3: it is 8
  (`signal.INFLIGHT`). In the P3 run (3 units, 32 rooms of 16 queued joins each, 512 calls,
  stepped on one core), a backlog took 512 / `inflight` control turns: 4.0 ms in all at 1,
  2.0 ms at 8 and 1.8 ms at 32, while a control turn's worst wall time grew with the window:
  22 µs at 1, 38 µs at 8 and 123 µs at 32, about 4 µs per call in flight. Past 8 the backlog
  finishes less than an eighth sooner, and every turn's control work, which every fence
  waits behind (Fence latency), keeps growing. `bound steps` budgets on a turn are not set:
  the build has no step count to read from one run, so they wait for a measured one.
- **PO2 (primary-unit affinity).** Kept (Sizing and admission).
- **PO3 (the shared receive pool).** Stays shared and unisolated before authentication;
  isolation starts after it (Isolation, items 4 and 6).
- **PO4 (where cascade attaches).** Not a separate item in the review; settled with
  [SFU cascade](sfu_cascade.md)'s O6: `Pool.cascade`, a `cascade` term in `RoomCost`, and
  lease and term expiry as their own suspend causes (RTC-7).

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
  a fresh SSRC with a new sealer, and the retired SSRC's sealer is never issued again. The
  subscribe's re-offer is answered at version 2; the collision's is at version 3.
  Its companion `subscribe_announces_rtx_pair` takes the pair from the wire (the SSRC the
  leg's frames arrive on and the one a NACK's repair arrives on): a subscribe on an RTX
  section raises one offer at version 2 whose one sendonly section has exactly those two
  `a=ssrc` lines and `a=ssrc-group:FID` with the media SSRC first. Once that offer is
  answered, an unsubscribe raises no offer, and a re-subscribe's offer at version 3 names
  only the fresh pair. A subscribe on a video section without RTX, and one on an audio
  section, raise no offer. It is red on main, where a subscribe without FlexFEC raised
  no offer.
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
  Chromium, committed with their offers, answers and capture recipe in
  `vectors/chromium_rtp.hirsel`. libwebrtc's VP8 packetizer always writes partition index 0,
  so the nonzero-index packet below is built by hand from RFC 7741 §4.2. Cases: a target change followed by a keyframe fragmented over several packets,
  a VP8 packet with `S = 1` and a nonzero partition index, a VP9 `U` picture whose spatial
  dependency is not yet met, and an AV1 structure whose decode-target table is permuted.
  *Oracle:* for each packet, the `(start, end, keyframe, needs, switches)` and the
  `Layer(e, s, t)` to `(source, i)` mapping, written by hand from RFC 7741, RFC 9628 §4.2 and
  AV1 RTP §A.8 against the captured bytes, and the egress packets a subscriber receives. The
  switch commits at the first decodable frame start, the whole frame follows, the old source
  forwards until then, and the permuted AV1 table maps by layer, not by index. For AV1, the
  decode-target indexes, each AV1 leg's markers and its descriptors' frame numbers (from
  a source numbered to wrap) are cross-checked against LiveKit's dependency-descriptor
  selector (pkg/sfu/videolayerselector in /tmp/ref-livekit) on the same packets. The VP9
  legs' markers are written by hand from RFC 9628 §4.2's `E` bit.
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
  stays on while the other still wants it. The below-base down-track and the
  unaffordable second base still forward their base layers (2651), and the held encoding
  keeps its bit until the switch. Packets outside the mask are never forwarded.
- **W11b `allocation_drops_a_stream`.** *Harness:* `dynacast_laws.hirsel`, four units. A
  simulcast publisher on unit 1 that negotiated `video-layers-allocation00` sends hand-built
  allocations. B on unit 2 targets `h`; C on unit 3 subscribes to `h` after the drop and
  retargets `f`; D on unit 1 targets `q`, then `f`. At step 20 the publisher's allocation
  drops `h` (streams `q` and `f` only), and at 140 restores it; the publisher follows the
  `f` bit of `Event.Layers`, so the `q`-only allocation while `f` is unwanted is a pause,
  not a stop. *Oracle:* the allocation bytes are written by hand from libwebrtc's README;
  the switches, `Layers` events and their steps by hand from RTC-6. B and C forward `q`
  from the first `q` keyframe after they learn of the drop, with no wait; the retargets to
  `f` turn its bit on; B returns to `h` at the first `h` keyframe after the restore. Red on
  main (VLA ignored), on a dropped set that ignores the published mask, on a remote unit that
  never applies the set, and on one that never resumes.
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
  carries no extension. `quiet_speakers_fill_free_slots` (`speaker_laws.hirsel`,
  `audio_forward = 2`): a loud talker, a muted publication, background noise at −80 dBov
  and a quiet talker at −45 dBov, the quiet one with the highest ID. *Oracle:* by hand from
  the murmur's windows and Jitsi's ranking. From the first window the loud talker and the
  quiet one are heard, and once the loud talker stops, the quiet one and the noise; the
  muted publication never is, and the listener never hears more than two. It was red on
  main, which held the quiet talker.
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

- **W21 `red_steps_down_on_a_clean_leg`.** *Harness:* `core/programs/webrtc_sfu/red_laws.hirsel`. A
  publisher sends Opus as RED (`red/48000/2`, `111/111`, distance 1) to a leg that agreed
  RED on its own payload types (Opus 109, RED 120) and a leg that did not (Opus 111), 500
  packets at 20 ms. Receiver reports at 1.01 s, 2.01 s, ... up to 9.01 s carry fraction
  26/256 up to 3.01 s, 5/256 at 4.01 s and 0 after. *Oracle:* RTP-13's policy by hand:
  distance 2 from the first report, held by the 5/256 report (under distance 2's rise, at its
  hold), until 6.01 s, 1 until 8.01 s, then `Off`; each RED packet's blocks are the exact
  earlier frames at offsets of 960 ticks, tagged 109, and the plain leg gets only primaries
  on 111. From 8.01 s, 0 redundancy bytes. Routing one distance-2 packet allocates 0 bytes,
  as W18.
- **W22 `red_recovers_bursty_loss_10` and W23 `red_recovers_bursty_loss_20`.** *Harness:*
  `std.sim.udp` between the router's egress and two receivers, a Gilbert-Elliott chain per
  destination with mean burst 2 (bad state loses all, leaves it half the time) and 10% or
  20% stationary loss; the receivers' RRs, RFC 3550's fraction each second, cross the same
  links back. 500 Opus packets. *Oracle:* the seeded arrival trace read by hand with RFC
  2198: a frame is recoverable when its own packet or one of the next two arrived. From 3 s
  on, every RED packet carries two blocks, the RED leg holds every recoverable frame, the
  plain leg holds exactly its arrived primaries, and the observed loss lies in 5–15% or
  13–27%. W22's publisher sends RED; W23's sends plain Opus, so the RED leg's
  blocks are rebuilt from primaries the publisher never sent as RED.
- **W24 `probe_ramps_to_top_layer`.** One publisher sends q (120 kbps), h (500 kbps) and f
  (1 Mbps) as keyframes; one subscriber targets f behind a 2 Mbps token-bucket policer and
  answers TWCC every 50 ms, from the 300 kbps start estimate. *Oracle:* by hand from RTC-6
  and RTP-9: the first cluster at 600 kbps puts h within reach, the second at about
  1.18 Mbps puts f within reach, so f arrives within 160 ms of subscribing and stays; on a
  router that does not probe the leg stays on q. Its companions: `probe_padding_stays_in_budget`
  (one cluster of 5 to 7 padding-only RTX packets with no payload, on consecutive RTX
  numbers, and none more while it is outstanding; red on a pacer that pads at the target
  rate), `initial_probes_retry_each_second` (with no feedback, clusters start at subscribe
  and 1, 2, 3 and 4 s after, then none inside the 5 s gate; red on main, which waited 5 s
  from the first), `probe_resumes_paused_leg` (without `pause` the base flows at once; with it the leg
  waits for one probe round) and `probe_pads_media_without_rtx` (padding on the media SSRC
  only after a frame end, its numbers gapless with the media's, no media frame lost).
  `start_estimate_is_the_sessions` covers the start estimate in W24's room with no
  feedback: from a 1.2 Mbps `start_bps` the down-track hears f alone from its first
  keyframe, and from 600 kbps h alone. It was red on main, which started every session at
  300 kbps. `stopped_stream_is_not_priced` has two publishers, A whose f is never sent and
  B, from a 1.2 Mbps start: both forward h, as RTC-6's rounds give by hand; on main A's
  silent f cost 0 − 500 kbps, A waited on it forever and the freed budget gave B f.
  `silent_stream_moves_its_viewers` stops f with no layers allocation at 1.5 s: the
  down-track hears nothing until the rate window that closes at 4 s marks f stopped, h
  from that window's h keyframe, and f again from f's first packet back at 5 s; on main it
  froze on f until f returned.
- **W25 `e2ee_routes_on_clear_bytes`.** *Harness:* `core/programs/webrtc_sfu/e2ee_laws.hirsel`,
  W9's VP8 three-encoding simulcast with temporal layers, H.264 simulcast, VP9 L3T3 and AV1
  L3T3 scenes with their target changes (AV1's at the 20th packet), and an Opus publication
  sent as RED to a RED leg and a plain leg, its RED raised by a 26/256 report. Each scene runs
  in the clear, then with every byte outside RTC-6's clear ranges XORed with seeded nonzero
  bytes (seeds 2684 and 2685) and with every bit flipped. *Oracle:* the clear ranges written
  by hand from RFC 7741 §4.2, RFC 9628 §4.2, RFC 6184 §5.6–5.8 with H.264 §7.3.3 and §7.4.1,
  the AV1 RTP payload §4.4 with AV1 §5.3, and RFC 2198 §3, independent of `media_bits` and
  `sframe`; and the clear run of the same scene. Every datagram (to whom, RTCP bytes, each
  forwarded packet with its encrypted bytes zeroed and its source capture), every published
  layer mask and the router's counts equal the clear run's. Red on main before this law,
  where VP9's keyframe test read the uncompressed header's frame marker, so every scrambled
  VP9 keyframe was `unparsed` and the router asked the publisher for one instead of
  forwarding it; red on an AV1 keyframe test
  that also reads `seq_profile` from the first OBU's payload.

- **W26 `rotation_survives_a_layer_switch`.** *Harness:* `router_hdrext_laws.hirsel`, one
  unit. A VP8 simulcast publisher sends CVO only on keyframes and on the frame where the
  rotation changes (TS 26.114's minimum) and abs-capture-time on every packet; one
  subscriber negotiated CVO, playout delay and abs-capture-time on IDs other than the
  publisher's. It switches from `q` to `h`, the rotation changes from 90° to 0°, and
  `set_playout(10, 100)` is set and later cleared. *Oracle:* the CVO byte from TS 26.114
  (`R1 R0 = 01` is 90°), the playout bytes `00 a0 64` from libwebrtc's README (12-bit MIN
  `0x00a`, 12-bit MAX `0x064`), and the capture bytes as sent. Every forwarded frame end
  carries the current rotation, the delay only while set, and the capture time unchanged.
  Red on a router that forwards CVO only when the source packet has it, on a leg that drops
  the playout delay, and on one that drops the capture time.
- **W27 `rsize_nack_is_answered_with_rtx`.** *Harness:* `core/programs/webrtc_sfu/router_laws.hirsel`.
  A subscriber on an `rtcp-rsize` session takes source packets 300 to 307, loses three and
  sends one NACK as libwebrtc does: an RR with one report block, then the NACK with
  (n0 + 2, BLP 0) and (n0 + 5, BLP 1), and no SDES. *Oracle:* RFC 5506 §4.1, RFC 4585
  §6.2.1 and RFC 4588 §4 by hand: each of n0 + 2, n0 + 5 and n0 + 6 comes back exactly once
  on the leg's RTX SSRC and type, with its OSN, its timestamp t0 + 3000 k and the
  publisher's payload, within one RTT (0 in the sim) plus the pacing time of three answers
  at the 750 kbps start rate (at most 2.14 ms, 3 ms polled). Red on main: the check refused
  the datagram as `NoCname` and nothing was answered.

- **W28 `departures_free_subscriber_sections`.** *Harness:*
  `core/programs/webrtc_sfu/signal_churn_laws.hirsel` over W16's room. A subscriber with 2
  receiving video sections and 4 down-tracks stays while six publishers join and publish in
  turn, and leave by `Left`, by absence from the snapshot after a `Lost`, and by a refused
  re-offer, each twice. *Oracle:* RTC-8 by hand. Each subscription is granted and carries
  the publisher's mark on the down-track's SSRC; the subscriber's session holds exactly one
  live down-track while subscribed and none after the departure; the room hears
  `Unpublished(id)` once; and the subscriber's later `Unsubscribe(id)` is not refused. Red
  on main at the first `Unpublished` check; with that check off, red at round 3 with a
  `Limit` refusal (the leaked sections); with sections freed and the session counting every
  track it has held, red at round 5 (`Peer`, `Capacity`).
- **W29 `late_joiner_learns_publications`.** *Harness:* `signal_churn_laws.hirsel`. Peer 1
  publishes, then peer 2 joins. *Oracle:* RTC-8 by hand: peer 2 alone is sent exactly one
  `Published(1, id, ..)`, and its subscription to `id` is granted and carries peer 1's mark.
  Red on main, where peer 2 is told nothing.
- **W30 `flexfec_recovers_drops_without_nack`.** *Harness:* `fec_laws.hirsel`. A subscriber
  offers a Chromium video section with `flexfec-03`; its subscription raises one re-offer
  whose `FEC-FR` pair names the media SSRC and a fresh FEC SSRC. After the answer and an RR
  at loss 20/256 (level 3), the publisher sends frames of 4 (key), 4 and 6 packets. The
  receiver, in wire order, drops offsets 0, 3, 6, 9, 10 and 13, rebuilds packets with its own
  FlexFEC-03 decoder and NACKs a gap only when a later frame starts or the stream ends.
  *Oracle:* draft-03 §4.2 and libwebrtc's reader, `ZeroMutableExtensions` and
  `FinishPacketRecovery`, re-derived in the law; RTP-14 F3 and F4 by hand: groups {0,1}
  {2,3} {4..7} {8..11} {12,13}, five repairs on PT 49 with MID `d`; 0, 3, 6 and 13 are
  rebuilt bit-exactly with the transport number zeroed; only 9 and 10 are NACKed, and each
  comes back as exactly one RTX. Red on main, where no FEC SSRC is drawn and no re-offer is
  raised.
- **W31 `sdp_opus_fmtp_per_section`.** An offer bundles mono microphone and stereo screen
  audio on Opus PT 111. Both answer sections keep the corresponding `fmtp` on PT 111.
  A subscriber receives the two publications on PT 109. Its re-offers carry each source's
  parameters on the assigned MID, even when the second subscription arrives while the
  first re-offer is in flight and the subscriber answers with its own Opus preferences.
  The parent refuses the publisher offer with `PayloadConflict(111)`.
- **W32 `sdp_opus_reused_section_announces_ssrc`.** Publisher A sends a mono microphone and
  a stereo screen on Opus PT 111, publisher C a mono microphone. Subscriber B's re-offers
  announce A's microphone and the stereo screen. B then drops A's microphone and takes C's
  on the freed section. A new re-offer names C's SSRC on that MID and no longer A's, with
  the mono and stereo parameters each on their own section. *Oracle:* RTC-2 and RTC-4 by
  hand. Red on 4c187144, where the unchanged `fmtp` raised no re-offer and the browser kept
  A's `a=ssrc`, so Chromium stopped playing C after 4,050 bytes.
- **W33 `flexfec_churn_keeps_legs_sending`.** *Harness:* `fec_laws.hirsel`. A FlexFEC
  subscriber with `down_tracks: 4` subscribes to and unsubscribes from one VP8 publication
  ten times. Each round's re-offer names a fresh media and FEC SSRC, and the round's
  two-packet keyframe must reach the subscriber on that media SSRC with one repair on
  PT 49 on the paired FEC SSRC. *Oracle:* RTC-4 and RTC-7 by hand: one live track needs 3
  of the 14 send streams, and RTP-14 F4 closes a keyframe group at its marker. Red on
  5a334a9c, where an ended track kept its send streams and the sixth round's leg could
  not seal.

**Nightly, not pre-push.** These run on Boat and route reds to the owner. They do not block
a land.

- **N1 `browser_room`:** headless Chromium and Firefox publish Opus and three-encoding VP8
  simulcast, and subscribe to each other through the service. This is the interop oracle for
  RTC-2 to RTC-4 against a real DTLS, SRTP and RTCP stack. It covers a layer switch, an
  `audio_forward` gate, a revoke whose subscriber stops receiving at once, and a relayed
  session over TURN with UDP blocked (turn.md U8). The service joins the room through `room_client` (F1).
  It runs from `core/programs/webrtc_sfu/service` (`run.py` builds the deployment, `n1.mjs`
  drives the browsers); the N1 decisions below say how. Its check `flexfec_per_leg`: the
  page keeps `flexfec-03` in `setCodecPreferences`; a Chromium subscriber's remote
  description carries `flexfec-03/90000` and an `a=ssrc-group:FEC-FR` line on the video
  section and its inbound stats count `fecPacketsReceived` above 0; a Firefox subscriber's
  carries neither.
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
| **S4** One-unit router | RTC-5 on a single unit: rooms, publications, down-tracks, fan-out in the subscriber's wire identity, transport sequence numbers at send, bounds and fence capacity; RTC-6's stable target maps, current and pending presentation, switch timeline, and negotiated keyframe requests with in-place intervals | S2, S3; the rtp amend's in-place `Keyframes` interval | W6, W7, W9, W10, W18, W27 (W6 and W7 need the router's down-tracks and switches) |
| **S5** Units and fence | the control order, doorbells, `Media`, `RepairAsk` and `RepairReply` messages with tags, the uncommitted suffix; RTC-7 with its test seam, `suspend` and `resume` | S4 | W8, W14, W15 |
| **S6** Dynacast and speakers | RTC-6's cross-unit wanted mask and pause delay, the bandwidth-limited effective target with probing and the room's opt-in `pause`, the Q16 active-speaker scores, `audio_forward` with per-leg `V` | S5 (remote units' masks); rtp's RTP-9 probe, RTP-10 `cluster` and RTP-11 `pad` | W11, W12, W13, W24 |
| **S7** Signaling adapter | RTC-8's `Signal`, `Embed`, `RoomIn`/`RoomOut`, dense ids and epochs, suspend on loss, resume from a proven roster; driven by a harness in its laws and by `room_client` (F1) in service | S5; R1 for the end-to-end path (the adapter fails closed without it) | W16 |
| **S8** withdrawn | ICE-TCP is replaced by TURN over a stream (turn.md TURN-2, slice U8) | — | — |
| **S9** Costs and nightly | `bench/media/sfu_fanout` and the voice-capacity run; N1 and N2 on Boat | S5, S6, S7 | the cost targets |
| **C1** Opus RED | RTP-13 and its use here: RTC-2's `Red` answer, RTC-5's ring, policy and RED legs | S6 | W21, W22, W23 |
| **C4** Header-extension pack | RTP-3's VLA, playout-delay, CVO and abs-capture-time; RTC-1's answers, RTC-5's egress extensions and `set_playout`, RTC-6's planned rates and dropped streams | S6 | W11b, W26 |
| **C14** FlexFEC-03 | RTP-14 and its use here: RTC-1's `FEC-FR`, RTC-2's `Flexfec` answer and re-offer lines, RTC-4's FEC SSRC, RTC-5's groups and paced repairs | S6, C1 | W30, W33; N1 `flexfec_per_leg` |
| **C9b** E2EE routing | RTC-6's clear ranges; VP9's keyframe from the descriptor alone | S6, C1 | W25 |
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
  negotiate for), `ring_misses`, `unparsed` (a VP8, VP9, H.264 or AV1 packet whose descriptor
  does not parse, or an AV1 packet with no dependency descriptor, which is dropped),
  `unmapped`, and `refused` (an AV1 target the structure cannot represent: `set_target`
  answers `Unrepresentable`, and a remap at a new structure keeps the leg's target). An ask counts as `unrequested` only once the
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
  when it is let out. The pacer's `Probe` and `Pad` answers go out as RTC-6's probing
  padding on the session's first pad track, and nothing pads without one.
- **H.264 state per source.** RTP-11's `after_end` is a fact about a source, so the
  router keeps it per encoding: the newest index seen, its timestamp and its marker,
  moved only by a newer index and cleared when the stream restarts. Before any packet of
  the source is seen, a packet may start a unit. A down-track keeps one set of parameter
  sets and the source it sent them from; a packet of any other source is judged with
  only the sets it carries, since nothing of that source reached the subscriber since
  the last switch away from it. The first packet sent from another source clears the
  sets. W9 found the earlier per-leg state switching to a pending H.264 encoding one
  access unit late (2595).
- **Not closed in S4.** W9 `router_fills_units` runs over the Chromium captures in
  `vectors/chromium_rtp.hirsel` (2595), with the three cases Chromium cannot emit built by
  hand: VP8 `S = 1` with a nonzero partition index, VP8 with TID (the capture had none: its answer bound the descriptor on VP8),
  and a permuted AV1 decode-target table. The AV1 decode-target map is built (2595): `rtp.ext` binds `DependencyDescriptor`,
  every unit parses it with its encoding's structure, `Layer(e, s, t)` maps through RTC-6's
  `(S_d, T_d)` and is rebuilt at a structure-carrying keyframe, and the decision unit is the
  descriptor's frame number. `router_maps_av1_targets` checks it over LiveKit's captured
  Chromium L3T3 descriptors. 2635 closed three of 2595's reds: the descriptor is answered on
  AV1 sections only (RTC-2 step 4, `sdp_descriptor_on_av1_only` against str0m's answers),
  egress writes it per down-track, and sub-layer VP9 and AV1 legs get the marker bit (RTC-6,
  W9 against LiveKit's selector, and `av1_across_units` on a unit other than the home and
  through the pacer, at the capture's own 20 ms pacing, above the start estimate). An RTX
  repair carries the media packet's descriptor (W9). Our egress for both unit-1 legs decodes under LiveKit's own descriptor
  parser, and a LiveKit selector downstream at the same target takes every packet with the
  same markers. Still open: a section the browser sends VP8 on and receives AV1 on drops the
  descriptor for both directions; a track `offer` appends on a transport that negotiated no
  descriptor ID gets none; the active-decode-targets bitmask is not narrowed per leg. A
  structure-carrying descriptor older than the structure in use (a repair or a reordered
  packet of an earlier keyframe) is dropped and counted `unparsed`. Each is an open red on S4's ticket. The re-offer
  after an SSRC collision (RTC-4) is built in S7 (S7 decisions, Re-offer).

**S5 decisions.** Gaps the spec left open, decided while S5 was built
(`core/programs/webrtc_sfu/units.hirsel`, `router_fence.hirsel`):

- **Control unit.** Unit 0 is the control unit, and it is also a media unit. It makes
  every control call: `units.begin` starts one, `units.finished` answers it once every unit
  has acknowledged, and `units.call` blocks on both over the unit's socket. Since RTC-10 P2
  one call is in flight per room, within the pool's `inflight` window (RTC-10, The control
  plane).
- **Apply order.** A change is applied home unit first, then to the others. A change the
  home unit refuses goes no further. `Event.Fenced` is raised on the control unit once a
  revoke has returned.
- **Sessions.** An opened `PeerSession` is parked on its home unit (`units.park`), and
  `Admit` moves it into the room. An admission that is refused, for example `Fenced`,
  unparks the session, so a refused join holds nothing. S6 and S7 build sessions inside
  their unit from cross-unit inputs. S7 owns join-from-offer (S7 decisions, Sessions).
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
  down-track of the publication, shared between those units. From S6 it also carries the
  publication's speaking state and the encoding's measured rate. `RepairAsk` goes to the publication's home unit, and `RepairReply` comes back
  with the ring's payload. A unit counts each kind it takes (`UnitCounts`).
- **Doorbells.** After a send step, a unit sends one empty datagram to each unit it posted
  to. A doorbell from an address that is not a sibling's is counted `forged` and ignored.
- **Test seam.** `units.seam(u, limit, tapping)` caps each `udp_send` at `limit` datagrams,
  which makes a partial commit. With `tapping`, the unit records every committed datagram,
  with its tag and bytes. In the laws, all units run as values in one task.
- **Real sockets.** `units_on_loopback` runs three units and two browsers over
  127.0.0.1 sockets bound at port 0. The units' doors name the addresses the binds return.
- **Not built in S5.** `close_room` across units is not built.

**S6 decisions.** Gaps the spec left open, decided while S6 was built
(`core/programs/webrtc_sfu/router_dynacast.hirsel`; laws W11 in `dynacast_laws.hirsel`,
W12 and W13 in `speaker_laws.hirsel`):

- **Masks start full.** Every encoding of a new publication starts published: the
  publisher sends all of them until the mask says otherwise, so a remote subscriber whose
  `Wanted` is still in flight sees no gap. The pause delay counts from the first check that
  sees the union empty: a `Wanted` taken, a packet of the publication, or a poll.
- **Per-unit union.** The home unit keeps one bit per unit for each encoding. A unit
  sends `Carry.Wanted(unit, mask)` only when its local mask changed, and the home unit
  replaces that unit's bits. `Carry.Layers` takes a published change from a home unit
  that is not unit 0 to the control unit, which raises `Event.Layers`.
- **Rates.** The home unit measures each encoding over 1 s windows on a grid from the
  publication's first packet: the plaintext bytes up to the payload end of each fresh media
  packet (not repairs), times 8. A window that closes sets the rate of each encoding that had
  packets in it, so a paused encoding keeps its last rate. Each `Media` message carries its
  encoding's rate (planned where known, else measured) and the publication's dropped set
  to the remote units; there is no separate rate or allocation message. A window that
  closes also counts, for each published encoding that had no packets in it, a window
  toward its stop (Dropped streams, above); its next packet resets the count. Before the
  first window closes no encoding is measured and each costs 0, so a down-track is lowered
  only once its rates are known.
- **Effective target.** A lowered video target is `Layer(e', s, t)` with the target's own
  `s` and `t`. Audio's effective target is its target. A held audio down-track keeps its mask
  bit, so the gate never pauses a publisher's audio. Allocation runs on every poll for every
  local member, which covers a single unit's unsubscribe, leave and revoke and a moved
  estimate.
- **Scores.** Durations are counted in RTP ticks at the publication's clock rate, so 400 ms
  is 19,200 ticks at 48 kHz. The first packet, and the first after an 800 ms expiry, adds no
  duration. A packet without the level extension counts as level 127. An expiry resets the
  whole window, not only the score.
- **Speakers.** Ticks fall on multiples of 400 ms of the router clock and are armed only
  after a score changed. `count` is the number of entries in `top`. Each entry is one
  publication, named by its owner's participant ID. Equal score and participant fall back to
  the lower publication ID.
- **Opus.** An Opus down-track that the gate releases asks for no keyframe.
- **Fence.** `Wanted`, `Layers` and `Score` carry the publisher's generation. `Score`
  carries the murmur beside the score, so a remote unit's gate fills the same free slots. A message
  whose publisher is fenced or no longer owns the publication is dropped and counted
  `stale`. The state lives in `Publication` and `Down`, so leaving a seat or ending a
  down-track removes it.
- **Laws on one router.** W12 and W13 run on one router. The `Score` message and the gate
  on a remote unit are covered by no law yet.

**S7 decisions.** Gaps the spec left open, decided while S7 was built
(`core/programs/webrtc_sfu/signal.hirsel`, W16 in `signal_laws.hirsel`):

- **Sans-IO adapter on the control unit.** The adapter holds no router. `room_in` takes
  the room client's entries in order and answers with `RoomOut`. `step` drives the control
  order on unit 0, one call in flight (S5): it answers the call that finished, turns each
  `Event.Fenced` into the `Fenced` signal, and begins the next call. The service calls
  `step` after every `room_in` and every unit turn. `you` names the service's own peer
  (`room_client.member(room).you`), which is never admitted.
- **Sessions.** An `Offer` from an admitted peer with no session asks for a `Build` on its
  home unit, the unit with the fewest sessions, ties to the lowest. The home unit opens
  the session from the offer (with `Build.publish` from `Embed.publish`), parks it, and
  reports `built`. The adapter then queues `Admit`. Once it returns, `Answer` goes to the
  peer and one `Publish` is queued for each section the peer sends on. Each publication
  goes to the room as `Published(peer, id, kind, encodings)`. A refused build or
  admission is sent to the peer as `Refused`, and the peer may offer again.
- **Ending an admission.** An admission with no session is fenced at once. One whose
  session is being built is admitted and revoked when the build reports. Otherwise
  `Revoke` is queued, and `Fenced(peer, epoch)` goes to the room when `Event.Fenced`
  says the fence holds, or at once if the router had nothing to fence.
- **Signals.** A `Candidate` or `EndOfCandidates` changes nothing, because the server is
  ICE-Lite (RTC-3). `Subscribe` takes the peer's first free send section of the
  publication's kind, and is refused `Limit` when there is none. A section is free again
  once its publication's owner departs (RTC-8 Departures). `Unsubscribe` of a publication
  the peer does not hold changes nothing and is not answered. A server-only signal from
  a peer is answered `Refused(0)`. Otherwise a `Refused` code is the `RouterError` tag,
  counted from 1 (`signal.code`).
- **Loss.** `Lost` puts `Suspend` ahead of every waiting call and drops a waiting resume.
  Every `Message` from then until resume is answered `Refused(Suspended)`. The comparison
  runs at the `PartEnd` of the first Part with `snapshot` set. A stale snapshot leaves the
  room suspended. Revokes are queued before the resume, so each fence holds on every unit
  before the room routes again.
- **Layers and Speakers.** `step` sends `Event.Layers(publication, mask)` to the publication's
  owner as `Layers(publication, mask)`, and `Event.Speakers` to the room as
  `Speakers(peers, count)`, each router id replaced by its seat's peer number. `Speakers`
  carries a `List[u64]`, because `Inline` is not `codec`.
- **Re-offer.** After the collisions one `peer.receive` found, the home unit makes one
  re-offer (`peer.reoffer`): every section a down-track sends on names the track's
  current SSRC and RTX SSRC (`a=ssrc`, `a=ssrc-group:FID`), at the next origin version,
  which is also the `Offer` signal's version. It reaches the control unit as `Carry.Offer`
  (SDP in the packet) and `Event.Offer`, and the adapter sends it to the peer. The peer's
  `Answer` while a re-offer is in flight goes back through `router.answer_reoffer` to the
  home unit as `Carry.Answer`. Each carry is dropped if its participant is fenced. The
  answer is refused when no re-offer is in flight, when it names another version, or when
  it does not parse or match the offer (`sdp.accept_answer`). A peer's `Refused` while a
  re-offer is in flight also refuses it.
- **Refused re-offer.** The home unit changes nothing and raises `Shut` (`Carry.Shut`,
  then `Event.Shut`). The adapter revokes the admission at its epoch, which closes the
  session on every unit, and sends `Fenced` once the fence holds. Until then the session
  runs on, so no unit holds a closed session. The seat then waits at the next epoch for the peer's fresh offer: that is the
  rejoin. A re-offer raised while the room is lost cannot be sent, so it is treated as
  refused. A media inbox that is full drops the carry, as it drops media. A dropped
  `Offer` leaves the re-offer unanswered, and a dropped `Answer` leaves it in flight.
- **Refused codes.** Code 0 is a signal the peer may not send. Codes 1 to 10 are the
  `RouterError` tags in declaration order: `NoRoom`, `NoParticipant`, `NoPublication`,
  `Fenced`, `Suspended`, `Exists`, `Limit`, `Unrepresentable`, `Peer` and `Capacity`.
  Code 11 is an offer longer than `signal.OFFER_MAX` (RTC-8 Offer size).
- **Embed callables.** In the native build, a named function converted to an `Embed`
  field crashes (SIGSEGV). Closures do not, so W16 uses closures.

**N1 decisions.** Gaps the spec left open, decided while N1 was built (2626)

- **Deployment.** `service/main.hirsel` runs the room place, `signal.serve_pool` (one room) and the TURN
  relay in one process on 127.0.0.1: the place listens on its listen address (7800) and
  serves the call page (`index.html`, the bundled `page.ts`) from its site directory, the
  media units bind the media address and the next port (7810, 7811), and the relay listens
  on the TURN address (3478) over UDP and TCP. The relay's `allow` is the media units' addresses (TURN-7). `run.py
  --build-only` builds it with `sdk ts` and esbuild; `run.py` alone also starts it.
- **TURN credentials at admission.** `signal.relay(adapter, Relaying)` gives the adapter the
  master, the relay plan and randomness; each admission then gets `mint_session`'s ICE and
  TURN credentials together, `Servers(username, password)` goes to the peer before any
  offer, the home unit answers with the minted ICE credentials (`Build.ice_ufrag`,
  `ice_pwd`), and each ended admission is queued as a `RelayRevoke` that serve forwards to
  the relay (TURN-9). With no relay the unit draws ICE credentials, as before.
- **Unit certificates differ in serial.** Each unit's self-signed certificate takes a
  random nonzero 63-bit serial (RFC 5280 section 4.1.2.2). Firefox's NSS refuses a second
  certificate with the issuer and serial of one it has seen (`SEC_ERROR_REUSED_ISSUER_AND_SERIAL`),
  so with serial 1 everywhere its second session, on another unit, failed DTLS.
  `unit_certificates_differ_in_serial` is red on serial 1.
- **The service's room link pings.** `room_client`'s reader pings after `idle_ms` with
  nothing read and ends the session only when a second window passes unanswered, as the
  server's reader does. It used to end the session at the first quiet window, so a room
  quiet for longer than `idle_ms` lost the service, which rejoined as a new peer.
  runner's `room_client_keeps_quiet_link` is red on that. The service dials with 10 s.
- **Harness.** Playwright (`playwright-core` 1.63.0) and its Chromium and Firefox install
  into `~/.cache/hirsel/n1`, never the repo. Pages run with fake devices; Firefox needs
  `media.navigator.permission.fake` and an active capture to expose loopback host
  candidates, so a page that sends nothing still holds a microphone capture. Every case
  judges by `getStats` (inbound bytes, decoded frames, frame width, the selected pair's
  candidate types and relay protocol) and by what the service prints (`allocations`,
  `revoked`), never by timing alone. Relay-only pages use `iceTransportPolicy: "relay"`
  with the minted `Servers` credentials, over `turn:` with `?transport=udp` or `tcp`.
- **Cases.** `publish_subscribe_both_directions` (Chromium and Firefox each publish Opus
  and three-encoding VP8 and receive the other), `layer_switch`, `revoke_stops_media` and
  `revoke_stops_media_chromium_leaves` (after `Fenced` the subscriber's inbound bytes stop),
  `audio_forward_gate_chromium` and `_firefox` (a listener receives a speaker's audio only
  while `audio_forward` allows it), and `relay_only_udp` and `relay_only_tcp`.
- **Layer switch.** The subscriber is retargeted to q and must decode a narrower width
  than it decoded before; retargeted to f, it must decode wider again. The width is matched
  to a rid by the publisher's `outbound-rtp` widths.
- **Reduced-size RTCP (2639).** `layer_switch` was red for a Firefox subscriber:
  receiving Chromium's simulcast, Firefox dropped from h to q within seconds and never
  climbed back. The answer left out `rtcp-rsize`, so the session read RTCP as `Compound`
  and dropped as malformed every datagram that did not start with SR or RR. Both browsers
  send transport-cc feedback alone in its datagram (libwebrtc
  `RTCPSender::SendCombinedRtcpPacket`), so no feedback reached RTP-9 and every leg's
  estimate stayed at the 300 kbit/s start. After Opus, that budget fits only Chromium's
  q encoding once its rates are measured. The answer now carries `rtcp-rsize` (step 6
  above), and `browser_feedback_reaches_bwe` feeds Firefox's own captured feedback.
- **Open red: `layer_switch` for a Firefox subscriber is not yet shown green.** On
  2026-11-25 the host was saturated (load about 60 on 32 CPUs, CPUs 8 to 11 at 92 to 95%
  busy with other builds), and N1 scored 5 of 10 both with and without the rsize answer,
  with the same five cases red. The publishers were CPU-limited: a Chromium f encoding
  measured about 17 kbit/s, so the allocation (RTC-6, on measured rates) put the Firefox
  subscriber on f, which then delivered a few frames and stalled. Two questions remain for
  an unloaded run. Does a Firefox subscriber now reach h and f? And can A climb at all?
  RTP-9 raises A only while A is below 1.5 times the acked rate plus 10 kbit/s, and S4
  sends no probes or padding. A leg that carries Opus and q at their nominal rates, about
  180 kbit/s, would hold A at the 300 kbit/s start, and h would never fit. libwebrtc
  leaves that state by probing, and since 2669 so does the SFU: a leg short of its target
  asks for a cluster and sends the pacer's `Probe` and `Pad` answers (RTC-6 **Probing.**,
  W24), so the second question is now one for N1 on an unloaded host.
- **NACKs never reached the history (2689).** With drops and no FEC in N1, a Chromium
  subscriber sent about 1,400 NACKs in 30 s and the SFU answered none: its decoder stalled
  at each loss until a PLI. A trace of `rtcp_in` showed every NACK datagram refused before
  parsing. On its `rtcp-rsize` session libwebrtc sends an RR (with report blocks) and the
  NACK in one datagram with no SDES, and rtp.md's check required a CNAME whenever an SR or
  RR came first, in either mode. RFC 5506 §4.1 lets a reduced-size datagram skip the
  compound rules, so in `Reduced` mode the check no longer asks for one. With that change,
  N1 traces show each dropped packet NACKed, found in the history (locally or through a
  unit's `RepairAsk`) and sent as RTX with its rewritten number as OSN. Firefox's lone RRs
  pass for the same reason on an `rtcp-rsize` session. W27 is the law.
- **SDES-less and empty-CNAME RTCP (2755).** On a compound session Firefox's lone RRs were
  still refused as `NoCname`, and Chromium's sender reports for its simulcast SSRCs, whose
  SDES carries a zero-length CNAME, as `Body(SDES)`, so those legs gave no RTT and no
  lip-sync. RFC 3550 §A.2 asks a receiver for no CNAME and §6.5 lets an item be empty, as
  libwebrtc and str0m read them, so rtp.md's check now takes both; `rtcp_compound_rules`
  carries the captured datagrams.
- **Open.** After a real `Lost` the service rejoins with a new peer number, but a page
  learns the service's number once, from `Servers`, so its later signals go to the old
  number.

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
- RTP-6's `Keyframes`: an in-place interval update that keeps the outstanding request
  (RTC-6).

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
| LiveKit | Bandwidth pause: with `allow_pause` set, a track the estimate cannot carry at its lowest layer is paused, and padding probes resume it (pkg/sfu/forwarder.go `ProvisionalAllocate`, streamallocator `maybeProbeWithPadding`); with it off, the default, the lowest layer is taken | **ADOPTED** in RTC-6's allocation (2651, 2669): pause is off by default, so the first step is always taken; a room that declares `pause` gets LiveKit's opt-in behaviour, and RTC-6's probing resumes a paused leg. Padding goes on RTX, or on the media SSRC only after a frame's last packet, as libwebrtc's `RTPSender::GeneratePadding` does without RTX. |
| Janus | Waits for a keyframe of the candidate substream before switching, and rebases sequence and timestamp from elapsed time when it has no SR (src/rtp.c) | **ADOPTED** as RTC-6's pending-source presentation and its no-SR fallback. |
| LiveKit | NACK answered from a sequencer by resending on the media SSRC or RTX | **REPLACED** by RTX only (RTC-4). TLS-8 never seals twice and never seals below the highest index, so a media-SSRC resend would need sealed copies per subscriber. |
| LiveKit | Single UDP port with a mux across the node, sessions found by ufrag then 5-tuple | **REPLACED** by one port per unit, so the home unit is a signaling choice and the kernel needs no steering. |
| LiveKit | Kick through its room service: remove participant, close its PeerConnection | **REPLACED** by RTC-7's fence. LiveKit's removal is asynchronous and gives no point after which no packet is forwarded. |
| libwebrtc (`/tmp/ref-libwebrtc-hdrext`, `b2946b39`) | The receiver takes a frame's rotation from its last packet and resets it to 0° on a packet without CVO (`rtp_video_stream_receiver2.cc`, `frame_object.cc`); the sender writes CVO on a keyframe, a change, or while nonzero (`rtp_sender_video.cc:398-405`); the sender's layers allocation carries each stream's index (`rtp_video_layers_allocation_extension.cc`) | **ADOPTED.** RTC-5 writes the publication's current CVO on every forwarded frame end, so any conforming sender's rotation survives a switch; RTC-6 reads only the allocation whose index matches the stream it came on. |
| LiveKit | `PlayoutDelayController` (pkg/sfu/playoutdelay.go:86): per down-track min and max, raised and lowered from jitter and NACK statistics, written into the playout-delay extension until acked | **ADAPTED** to a static per-subscription value (`set_playout`). The adaptive controller is a follow-up; a static value is what C4 asked for and is deterministic for the laws. |
| Janus | Parses `video-layers-allocation00` for the publisher's spatial and temporal layer counts only, ignoring rates (src/rtp.c:464-527, used from src/ice.c) | **ADAPTED.** RTC-6 uses the per-stream rates and the active set as well: planned rates for allocation and a dropped stream as a switch trigger. |
| str0m | Sans-IO `Rtc` with `handle_input` and `poll_output`, one per peer | **ADOPTED** as `receive` and `poll` |
| str0m and LiveKit | SCTP data channels | **NOT TAKEN** for Wave 1. Application data goes over `runner.room` or WebTransport (Wave 2). |
