# RTP and RTCP

Version note (2026-11-24, media-rtp-spec-2318): first text of RTP-1 to RTP-12, Wave 1.4 of the
real-time media track. Not yet built; the fences are `unbuilt`. The section ends with the
[build laws](#build-laws) and the [build slices](#build-slices).

`rtp` is a pure Hirsel package at `core/programs/rtp/` <!-- doc-ref: planned -->. It holds the parts of a real-time
media endpoint or forwarder that sit between the packet cipher and the codec:

- the RTP fixed header and its header extensions, read in place from the packet bytes;
- RTCP compound packets: sender and receiver reports, SDES, BYE, NACK, PLI, FIR and
  transport-wide feedback;
- reception statistics, round-trip time and loss tracking;
- a retransmission history and RTX ([RFC 4588](https://www.rfc-editor.org/rfc/rfc4588));
- transport-wide sequence numbers and their feedback;
- a delay-based bandwidth estimator and a pacer;
- per-stream sequence and timestamp rewriting, with layer switching that waits for a
  switch point.

Every operation is **sans-IO**. It takes bytes, a time in nanoseconds and its own state, and
it answers with bytes to send, a time to be called again or a decision. Nothing in `rtp`
waits, reads a clock, draws randomness or touches a socket. So every part of it runs the
same under `hirsel test --sim`, under replay, and in a served run.

`rtp` names no application concept. A stream is identified by a 32-bit source key the
caller picks. A layer is a pair of small integers. Which stream feeds which, who may receive
what, and why a stream is closed are the caller's to decide.

## Boundaries

Three neighbours own what `rtp` does not.

- **SRTP and SRTCP** ([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement) and the SRTP
  section of the DTLS spec, media-dtls-spec-2317) authenticate and decrypt every packet.
  `rtp` never sees a key. SRTP uses two `rtp` operations: `header.parse` to find where the
  header ends, and `seq.estimate` ([RTP-2](#rtp-2-extended-sequence-numbers)) to estimate the
  48-bit packet index. There is one unwrapper in the stack, and it is `seq.estimate`.
- **Codec bitstreams** belong to `media_bits`. `rtp` treats every payload as opaque bytes.
  A caller that switches layers tells `rtp` what each frame is (a `Unit`,
  [RTP-11](#rtp-11-stream-rewriting-and-layer-switching)), using `media_bits` to find out.
- **Session and forwarding policy** belongs to the forwarding service (media-webrtc-sfu-spec-2319):
  SDP, the demultiplexing of STUN, DTLS and SRTP on one socket, which subscriber gets which
  publication, layer selection, active-speaker ranking, and the revocation fence. `rtp`
  gives that service synchronous `close` operations that leave nothing queued
  ([RTP-12](#rtp-12-state-charges-and-close)).

**Authentication comes before state.** The stateless operations (`header.parse`,
`ext.find`, `rtcp.next` and the value readers) may run on bytes that have not been
authenticated. SRTP needs the first two to authenticate at all. Every operation that changes
state takes the packet's extended index, which the caller has only after SRTP's `open`
succeeded. The caller must not feed an unauthenticated packet to a stateful operation. That
rule belongs to the session glue and its law is the forwarding spec's. `rtp` itself cannot
tell, because a plaintext packet is only bytes.

### Contract traceability

Every requirement of the Wave 1 consumer contract
(`research-1124/media-wave1/CONSUMER-CONTRACT.md` §1, "RTP / RTCP SFU forwarding") maps to a
section here or to a named owner.

| contract requirement | where |
|---|---|
| BUNDLE with RTP/RTCP demux | [RTP-4](#rtp-4-rtcp-compound-packets) `rtcp.is_rtcp` ([RFC 5761 §4](https://www.rfc-editor.org/rfc/rfc5761#section-4)); STUN/DTLS/SRTP first-byte demux ([RFC 7983](https://www.rfc-editor.org/rfc/rfc7983)) is the forwarding spec's |
| negotiated MID, RID and extmap | [RTP-3](#rtp-3-header-extensions): one `ExtMap` per direction and peer, `mid`, `rid` and `repaired-rid` readers and writers |
| negotiated payload types | the forwarding spec maps them; `header.write` ([RTP-1](#rtp-1-the-fixed-header)) writes the payload type the caller gives |
| Opus, VP8 and H.264 passed through with no server decode | payloads are opaque here; frame facts come from `media_bits`, which gains a VP8 payload descriptor module (slice S9) |
| SR/RR | [RTP-5](#rtp-5-reception-statistics-reports-and-round-trip-time) |
| NACK/RTX | [RTP-6](#rtp-6-loss-tracking-nack-and-keyframe-requests), [RTP-7](#rtp-7-retransmission-history-and-rtx) |
| PLI/FIR coalescing | [RTP-6](#rtp-6-loss-tracking-nack-and-keyframe-requests) `Keyframes` |
| TWCC | [RTP-8](#rtp-8-transport-wide-sequence-numbers-and-feedback) |
| pacing | [RTP-10](#rtp-10-pacer), driven by [RTP-9](#rtp-9-bandwidth-estimation) |
| per-subscriber sequence and timestamp continuity | [RTP-11](#rtp-11-stream-rewriting-and-layer-switching) |
| keyframe-safe simulcast and SVC layer switching | [RTP-11](#rtp-11-stream-rewriting-and-layer-switching) |
| RFC 6464 audio-level forwarding | [RTP-3](#rtp-3-header-extensions) reads it on ingress and writes it on egress; ranking speakers is the forwarding spec's |
| dynacast (`Off | Layer(rid)` per subscriber, unselected layers paused) | the forwarding spec; `Rewrite.target` ([RTP-11](#rtp-11-stream-rewriting-and-layer-switching)) takes `Off` or a layer |
| revocation: stop forwarding, flush or cancel queued RTX | the fence is the forwarding spec's; [RTP-12](#rtp-12-state-charges-and-close) gives the synchronous `close` of rewrite, history and pacer state |
| DTLS, SRTP and SRTCP | the DTLS spec |

**Not taken in Wave 1**, each a later decision with no hook left for it here: REMB, RTCP XR,
RTCP APP (parsed as `Other` and skipped), FEC of any kind (RED, ULPFEC, FlexFEC), the video
layers allocation extension, transport-cc v2's feedback request, RFC 8888 congestion control
feedback, abs-capture-time, playout-delay, video-orientation, RFC 6465 mixer-to-client audio
levels, SDES items other than CNAME, and libwebrtc's second-generation loss-based estimator.

## RTP-1: The fixed header

```hirsel unbuilt
// module header
pub type Error = Short | Version | Csrc | Extension | Padding
pub type Block = NoBlock | OneByte(at: u64, len: u64) | TwoByte(at: u64, len: u64) | Opaque(profile: u16, at: u64, len: u64)
pub type Header = Header(marker: bool, pt: u8, seq: u16, ts: u32, ssrc: u32, csrcs: u8, padded: bool, block: Block, end: u64)
pub type Fields = Fields(marker: bool, pt: u8, seq: u16, ts: u32, ssrc: u32)
pub type WriteError = Capacity(Capacity) | Form

pub fn parse(pkt: Bytes) -> Result[Header, Error]
pub fn payload(h: Header, pkt: Bytes) -> Result[(u64, u64), Error]
pub fn csrc(h: Header, pkt: Bytes, i: u8) -> u32
pub fn write(f: Fields, exts: View[ExtWriter], mut out: Bytes) -> Result[u64, WriteError]
```

`header.parse(pkt)` reads the fixed header of [RFC 3550 §5.1](https://www.rfc-editor.org/rfc/rfc3550#section-5.1)
and the extension block's position. It never copies and never allocates. `end` is the
offset of the first payload byte: `12 + 4·CC`, plus `4 + 4·length` when `X` is set. This is
exactly the RTP header that SRTP authenticates and leaves clear. Checks, in order:

- `Short`: fewer than 12 bytes.
- `Version`: the top two bits are not `2`.
- `Csrc`: `12 + 4·CC` exceeds the length.
- `Extension`: `X` is set and the 4-byte extension header, or `4·length` bytes after it,
  do not fit.

The block's form comes from its 16-bit profile ([RFC 8285 §4](https://www.rfc-editor.org/rfc/rfc8285#section-4)):
`0xBEDE` is one-byte, `0x100` followed by any four appbits (`profile & 0xFFF0 == 0x1000`) is
two-byte, and any other profile is `Opaque`. An opaque block is not an error: the packet
forwards, and it has no readable extensions. `at` and `len` locate the block's elements,
after the 4-byte block header.

`parse` does not look at padding, because under SRTP the padding is encrypted.
`header.payload(h, pkt)` is called on the plaintext and answers the payload's offset and
length. When `P` is set, the last byte is the padding count, which includes itself. A count
of 0, or one larger than `len − end`, is `Padding`. A count equal to `len − end` gives an
empty payload: a padding-only packet, which is valid.

Each offset check is written as `at <= len && size <= len − at`, so no sum can wrap.

`header.write(f, exts, mut out)` appends a fixed header with `V = 2`, `P = 0`, `CC = 0` and
the extensions that `exts` holds ([RTP-3](#rtp-3-header-extensions)) to the spare of `out`,
and answers the bytes appended. A forwarder never forwards CSRCs: it is not a mixer, so
the header it writes has none. Its errors are `Capacity` (the spare is short, with the
whole header's length as `requested`) and the form error of RTP-3. A refused write appends
nothing.

Law: `parse` accepts exactly the headers RFC 3550 §5.1 and RFC 8285 §4 describe, answers
the same `end` that SRTP authenticates, and reads no byte past it. `payload` refuses exactly
the padding counts that cannot be. `write` followed by `parse` gives back the fields and
extension values written.

## RTP-2: Extended sequence numbers

```hirsel unbuilt
// module seq
pub fn estimate(highest: u64, s: u16) -> Option[u64]
pub fn estimate32(highest: u64, t: u32) -> Option[u64]
pub fn estimate24(highest: u64, r: u32) -> Option[u64]
```

Wire counters wrap. Every stateful part of `rtp` works in 64-bit extended values instead,
so no comparison anywhere is modular.

`seq.estimate(highest, s)` is the index estimate of [RFC 3711 §3.3.1](https://www.rfc-editor.org/rfc/rfc3711#section-3.3.1).
With `s_l = highest mod 2^16` and `roc = highest >> 16`:

- if `s_l < 2^15`: `v = roc − 1` when `s − s_l > 2^15`, else `v = roc`;
- otherwise: `v = roc + 1` when `s_l − 2^15 > s`, else `v = roc`;
- the answer is `Some(v · 2^16 + s)`, or `None` when `v` would be `roc − 1` with `roc = 0`.

The comparisons are on integers, not modular values. `None` names a packet from before the
stream began, which no sender sent: SRTP refuses it as `TooOld` (SRTP-4 in
[the DTLS spec](dtls.md)), and every `rtp` caller skips it. A first packet has `highest` equal to
its own `seq` (ROC 0), as RFC 3711 §3.3.1 starts it.

SRTP calls `estimate` to pick the index it authenticates. It commits the new `highest`
only after `open` succeeds, and it hands the authenticated index to the caller. Every
stateful `rtp` operation takes that index. `rtp` keeps no second unwrapper, so the index
that SRTP authenticated is the index that statistics, loss tracking and rewriting use.

`estimate32` applies the same rule to 32-bit RTP timestamps (half range `2^31`), and
`estimate24` to the 24-bit TWCC reference time (half range `2^23`). An extended timestamp is
per source and starts at the first authenticated packet's timestamp. A timestamp that extends to
`None` is from before the source's first packet: the caller drops the packet before any
stateful operation.

Law: `estimate` is RFC 3711's estimate, and the extended values it gives are the only
ordering any stateful operation uses.

## RTP-3: Header extensions

```hirsel unbuilt
// module ext
pub type Kind = Mid | Rid | RepairedRid | AudioLevel | AbsSendTime | TransportSeq
pub type Error = Overrun | Value(Kind)
pub type Element = Element(id: u8, at: u64, len: u64)
pub type MapError = BadId | IdTaken | KindTaken | Full
pub opaque type ExtMap
pub opaque type ExtWriter

pub fn map(two_byte: bool) -> ExtMap
pub fn bind(mut m: ExtMap, id: u8, k: Kind) -> Result[(), MapError]
pub fn next(pkt: Bytes, h: Header, mut at: u64) -> Result[Option[Element], Error]
pub fn find(m: ExtMap, pkt: Bytes, h: Header, k: Kind) -> Result[Option[Element], Error]
pub fn audio_level(pkt: Bytes, e: Element) -> Result[(bool, u8), Error]
pub fn abs_send_time(pkt: Bytes, e: Element) -> Result[u32, Error]
pub fn transport_seq(pkt: Bytes, e: Element) -> Result[u16, Error]
pub fn token(pkt: Bytes, e: Element) -> Result[Element, Error]
pub fn writer() -> ExtWriter
pub fn put(mut w: ExtWriter, m: ExtMap, k: Kind, value: Bytes, from: u64, len: u64) -> Result[(), MapError]
pub fn put_other(mut w: ExtWriter, id: u8, value: Bytes, from: u64, len: u64) -> Result[(), MapError]
```

An **extension map** binds local ids to kinds, as an SDP `a=extmap` line negotiates them
([RFC 8285 §5](https://www.rfc-editor.org/rfc/rfc8285#section-5)). Each peer and direction
has its own map, because a publisher's ids and a subscriber's ids are negotiated apart.
`bind` refuses id 0, id 15 in a one-byte map, an id above 255, and an id or kind already
bound (`BadId`, `IdTaken`, `KindTaken`); `put` past the writer's 16 elements or 256 bytes is `Full`. `two_byte` records whether the peer negotiated `a=extmap-allow-mixed`.
A kind `rtp` does not name stays unbound: the caller reads it with `next` and the raw
element, and writes it with `put_other`. The dependency descriptor goes that way to
`media_bits.dependency`.

**Reading.** `ext.next(pkt, h, mut at)` walks the block's elements in order, from `at = 0`.
In the one-byte form a zero byte is padding and is skipped. Each element is one byte
`id ‖ L` with `len = L + 1`. Id 15 ends the walk with `None`, and the rest of the block is
ignored, as RFC 8285 §4.2 says. In the two-byte form a zero byte is padding. Each element is
`id`, `len` (0 to 255), then the value. An element whose value runs past the block is
`Overrun`. An `Opaque` or absent block has no elements. `find` walks to the first element
whose id the map binds to `k`, and it reports a later `Overrun` only if it reaches it.

An `Overrun` comes from an authenticated sender that broke the format. The forwarder drops
the packet. `str0m` stops reading at the overrun and keeps the packet
(`src/rtp/ext.rs:472-531` at `2ada76a`); `rtp` refuses it instead, so a forwarder never
re-sends a header it could not read.

**Values.** Each reader checks its value's length and answers `Value(k)` otherwise:

| kind | URI | value | reader answer |
|---|---|---|---|
| `AudioLevel` | `urn:ietf:params:rtp-hdrext:ssrc-audio-level` | first byte `V ‖ level`; length at least 1, later bytes ignored ([RFC 6464 §3](https://www.rfc-editor.org/rfc/rfc6464#section-3)) | `(voice, level)`, level 0 to 127, in −dBov |
| `AbsSendTime` | `http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time` | exactly 3 bytes, 6.18 fixed-point seconds | the 24-bit value |
| `TransportSeq` | `http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01` | exactly 2 bytes | the 16-bit transport sequence number |
| `Mid`, `Rid`, `RepairedRid` | RFC 8843 §15, RFC 8852 §3 | 1 to 255 bytes, each `A-Z a-z 0-9`, and for `Rid` and `RepairedRid` also `-` and `_` ([RFC 8851 §10](https://www.rfc-editor.org/rfc/rfc8851#section-10)) | the element's range, unchanged |

A token reader answers a range in the packet, not a `String`. The caller compares it with
the negotiated value in place, so reading a MID allocates nothing.

**Writing.** An egress header carries the subscriber's ids, not the publisher's. The
forwarder fills an `ExtWriter` (a fixed buffer of 256 bytes inside the writer, with up to 16
elements) and `header.write` lays the block out. `put` looks up the subscriber map's id for
`k`. An unbound kind is skipped and answers `Ok`, because the subscriber did not negotiate
it. The form is chosen once per header:

- **one-byte** when every id is 1 to 14 and every value is 1 to 16 bytes;
- otherwise **two-byte**, when the subscriber map has `two_byte`;
- otherwise the write is refused with `Form`, and nothing is appended.

Elements are written in the order put, with no padding between them. The block is
zero-padded to a 4-byte boundary. A writer with no elements writes no block and leaves `X`
clear.

Law: `next` walks exactly RFC 8285's element grammar in both forms. Each reader accepts
exactly its documented lengths. `write` picks the one-byte form whenever it can, and never
writes an element a one-byte receiver would misread.

## RTP-4: RTCP compound packets

```hirsel unbuilt
// module rtcp
pub type Error = Version | Length | Padding | NotCompound | Body(u8)
pub type Item = Sr(SenderInfo, u64, u8) | Rr(u32, u64, u8) | Sdes(u64, u64, u8) | Bye(u64, u8, Option[(u64, u64)])
    | Nack(u32, u32, u64, u64) | Twcc(u32, u32, u64, u64) | Pli(u32, u32) | Fir(u32, u64, u64) | Other(u8, u8, u64, u64)
pub type SenderInfo = SenderInfo(ssrc: u32, ntp: u64, rtp: u32, packets: u32, octets: u32)
pub type Block = Block(ssrc: u32, fraction: u8, lost: i32, highest: u32, jitter: u32, lsr: u32, dlsr: u32)

pub fn is_rtcp(pkt: Bytes) -> bool
pub fn next(pkt: Bytes, reduced: bool, mut at: u64) -> Result[Option[Item], Error]
pub fn block(pkt: Bytes, at: u64, i: u8) -> Block
pub fn nack_next(pkt: Bytes, mut at: u64, end: u64) -> Option[(u16, u16)]
pub fn fir_next(pkt: Bytes, mut at: u64, end: u64) -> Option[(u32, u8)]

pub fn write_sr(info: SenderInfo, blocks: View[List[Block]], mut out: Bytes) -> Result[u64, Capacity]
pub fn write_rr(ssrc: u32, blocks: View[List[Block]], mut out: Bytes) -> Result[u64, Capacity]
pub fn write_cname(ssrc: u32, cname: Bytes, mut out: Bytes) -> Result[u64, Capacity]
pub fn write_bye(ssrc: u32, mut out: Bytes) -> Result[u64, Capacity]
pub fn write_nack(sender: u32, media: u32, pairs: View[List[(u16, u16)]], mut out: Bytes) -> Result[u64, Capacity]
pub fn write_pli(sender: u32, media: u32, mut out: Bytes) -> Result[u64, Capacity]
pub fn write_fir(sender: u32, media: u32, seq: u8, mut out: Bytes) -> Result[u64, Capacity]
```

**Demultiplexing.** On a BUNDLE transport RTP and RTCP share one port.
`rtcp.is_rtcp(pkt)` is true when the second byte is 192 to 223, the payload-type range RFC
5761 §4 reserves for RTCP. A forwarder never negotiates an RTP payload type of 64 to 95,
which that rule would misroute.

**Parsing.** `rtcp.next(pkt, reduced, mut at)` reads one packet of a compound datagram and
moves `at` past it. It never copies. Each packet is checked, in order:

- `Version`: the version is not 2.
- `Length`: the 4-byte header does not fit, or `4·(length + 1)` bytes do not fit.
- `Padding`: `P` is set on a packet that is not the last of the datagram, or its count is 0
  or larger than the body ([RFC 3550 A.2](https://www.rfc-editor.org/rfc/rfc3550#appendix-A.2)).
- `NotCompound`: `at` is 0, `reduced` is false, and the type is not SR or RR. A reduced-size
  datagram ([RFC 5506](https://www.rfc-editor.org/rfc/rfc5506)) may begin with any type, and
  the caller passes `reduced` when the session negotiated `a=rtcp-rsize`.
- `Body(pt)`: the body is too short for what its count promises.

| type, format | item | body checks |
|---|---|---|
| 200 SR | `Sr(info, blocks_at, count)` | `24 + 24·RC` bytes fit; later bytes are a profile extension and are skipped |
| 201 RR | `Rr(ssrc, blocks_at, count)` | `4 + 24·RC` bytes fit |
| 202 SDES | `Sdes(at, len, count)` | none; chunks are read by the caller, and only CNAME is used |
| 203 BYE | `Bye(at, count, reason)` | `4·SC` bytes fit, then an optional length-prefixed reason |
| 205, FMT 1 | `Nack(sender, media, fci_at, fci_end)` | at least one 4-byte FCI entry |
| 205, FMT 15 | `Twcc(sender, media, fci_at, fci_end)` | read by [RTP-8](#rtp-8-transport-wide-sequence-numbers-and-feedback) |
| 206, FMT 1 | `Pli(sender, media)` | no FCI is required; trailing bytes ignored |
| 206, FMT 4 | `Fir(sender, fci_at, fci_end)` | at least one 8-byte entry |
| anything else | `Other(pt, fmt, at, len)` | none |

`nack_next` reads `(PID, BLP)` pairs ([RFC 4585 §6.2.1](https://www.rfc-editor.org/rfc/rfc4585#section-6.2.1)).
The caller expands each pair into PID and every `PID + i + 1` whose bit `i` is set, and
extends each with `seq.estimate` against the stream's highest sent index, skipping a `None`. `fir_next` reads
`(SSRC, seq)` entries ([RFC 5104 §4.3.1](https://www.rfc-editor.org/rfc/rfc5104#section-4.3.1)).

**Writing.** Each writer appends one packet to the spare of `out`, with `P = 0`, and
answers its length. A short spare is `Capacity` with the packet's length as `requested`,
and nothing is appended. The caller assembles a compound datagram SR or RR first, then
SDES, then the rest, or a reduced-size one in any order.

Law: `next` accepts exactly the compound and reduced-size datagrams RFC 3550 A.2 and RFC
5506 §3 allow, and each writer's output parses back to its inputs.

## RTP-5: Reception statistics, reports and round-trip time

```hirsel unbuilt
// module stats
pub opaque type Received
pub opaque type Sent

pub fn received(clock: u32, first: u64, first_ts: u64, at_ns: u64) -> Received
pub fn on_packet(mut r: Received, index: u64, ts: u64, at_ns: u64)
pub fn on_sr(mut r: Received, info: SenderInfo, at_ns: u64)
pub fn report(mut r: Received, ssrc: u32, now_ns: u64) -> Block
pub fn rtt(b: Block, ntp_now: u64) -> Option[u64]

pub fn sent(clock: u32) -> Sent
pub fn on_sent(mut s: Sent, payload: u64)
pub fn sender_info(s: Sent, ssrc: u32, ntp_now: u64, rtp_now: u32) -> SenderInfo
```

**Receiving.** A `Received` follows one incoming SSRC from its first authenticated packet
([RFC 3550 A.3](https://www.rfc-editor.org/rfc/rfc3550#appendix-A.3)). It holds the base
index, the highest index, the count received, the `expected_prior` and `received_prior` of
the last report, and the jitter. It skips RFC 3550's probation: the session negotiated the
SSRC, and SRTP authenticated the packet.

- `expected = highest − base + 1`. `lost = expected − received`, clamped to the signed
  24-bit range `[−2^23, 2^23 − 1]`.
- Per report: `expected_interval = expected − expected_prior`,
  `lost_interval = expected_interval − (received − received_prior)`. The fraction is
  `(lost_interval << 8) / expected_interval` when both are positive, else 0.
- **Jitter** is RFC 3550 A.8's integer form. The arrival time in timestamp units is
  `a = (at_ns · clock) / 10^9`, computed exactly in 128 bits and floored. With
  `transit = a − ts` and `d = |transit − last_transit|`, the scaled jitter `J` (16 times the
  jitter) is updated as `J += d − ((J + 8) >> 4)`. The report carries `J >> 4`. A packet
  whose index is not above the highest still counts as received, and still updates the
  jitter. That is RFC 3550's text.
- `on_sr` keeps the SR's middle 32 NTP bits and the arrival time. A report's `lsr` is those
  bits, and its `dlsr` is the delay since then in units of `1/65536` s, floored. Both are 0
  before any SR.

**Round-trip time.** `stats.rtt(b, ntp_now)` reads a block that a peer sent about one of our
streams. With `A` the middle 32 bits of `ntp_now`, the round-trip time is `A − lsr − dlsr` in
units of `1/65536` s ([RFC 3550 §6.4.1](https://www.rfc-editor.org/rfc/rfc3550#section-6.4.1)),
answered in nanoseconds and floored. It is `None` when `lsr` is 0, or when the difference
would be negative.

**Sending.** A `Sent` counts one outgoing stream's packets and payload octets (not headers
or padding), as an SR reports them. `sender_info` takes the NTP time and the matching RTP
timestamp from the caller. For a rewritten stream the caller gets the RTP timestamp from
`Rewrite.sr_timestamp` ([RTP-11](#rtp-11-stream-rewriting-and-layer-switching)), so the SR
speaks the subscriber's timeline and counts only what was sent to that subscriber. A
forwarder never relays a publisher's SR to a subscriber.

Law: the counts, fraction, cumulative loss and jitter are RFC 3550 A.3 and A.8 to the bit.
The round-trip time is RFC 3550 §6.4.1's.

## RTP-6: Loss tracking, NACK and keyframe requests

```hirsel unbuilt
// module loss
pub type NackConfig = NackConfig(window: u32, retry_ns: u64, tries: u8, max_age_ns: u64)
pub type Gap = Tracked | Reset
pub opaque type Losses
pub opaque type Keyframes
pub type Kind = Pli | Fir

pub fn losses(c: NackConfig, first: u64, at_ns: u64) -> Result[Losses, Capacity]
pub fn on_packet(mut l: Losses, index: u64, at_ns: u64) -> Gap
pub fn due(mut l: Losses, now_ns: u64, rtt_ns: Option[u64], mut pairs: List[(u16, u16)]) -> Option[u64]

pub fn keyframes(kind: Kind, interval_ns: u64) -> Keyframes
pub fn want(mut k: Keyframes, now_ns: u64) -> Option[u8]
pub fn keyframe_seen(mut k: Keyframes)
```

**Losses.** A `Losses` follows one incoming media stream for which the session negotiated
`nack`. It holds a bitmap of `window` indices below the highest, with the time each missing
index was found missing, the time it was last asked for and its tries.

- `on_packet` with an index above the highest marks every index between them missing. A gap
  wider than `window` clears the bitmap and answers `Reset`: the caller then asks for a
  keyframe, because asking for that many packets would not repair the stream sooner. An
  index at or below the highest clears its bit, if it is in the window.
- `due` writes the NACK pairs to send now, in ascending index order, into `pairs`, and
  answers the time it next has something due. An index is due when it has never been
  asked for, or when `retry_ns` (or the round-trip time, when the caller has one and it is
  larger) has passed since it was. After `tries` requests, or `max_age_ns` after it went
  missing, it is dropped from the bitmap without another request.
- Pairs are packed greedily: the lowest due index is a PID, and each due index from
  `PID + 1` to `PID + 16` sets bit `index − PID − 1` of its BLP.

The window is the caller's explicit bound, charged at `losses` (`window / 8` bytes for the
bitmap and 16 bytes per tracked index for times and tries). No cap is hidden: an index
outside the window answers `Reset`.

**Keyframe requests.** Many subscribers may ask for a keyframe of one publication within a
few milliseconds. A `Keyframes` coalesces them into one request upstream per media SSRC.

- `want(now)` answers `Some(seq)` when a request should go upstream now, and `None` when one
  is already outstanding and `interval_ns` has not passed since it was sent.
- A request is **new** when no request is outstanding. A new request takes the next FIR
  command sequence number, mod 256. A **repeat**, sent when `interval_ns` passed with the
  request still outstanding, carries the same number, as [RFC 5104 §4.3.1.1](https://www.rfc-editor.org/rfc/rfc5104#section-4.3.1.1)
  requires. For PLI the number is unused.
- `keyframe_seen` ends the outstanding request. The next `want` is new and goes at once.

The caller detects keyframes with `media_bits` and tells `Keyframes`. Which subscribers
asked, and whether they may ask, is the caller's.

Law: NACK pairs cover exactly the missing indices that are due, each in the fewest pairs a
left-to-right packing gives. A burst of `want` calls inside one interval yields one upstream
request, and the FIR sequence number changes only for a new request.

## RTP-7: Retransmission history and RTX

```hirsel unbuilt
// module history
pub type Lookup = Found(u64) | Missing | Throttled | BeforeSwitch
pub opaque type History[P]

pub fn history[P](slots: u32, max_age_ns: u64) -> Result[History[P], Capacity]
pub fn put[P](mut h: History[P], index: u64, at_ns: u64, take p: P) -> Option[P]
pub fn get[P](mut h: History[P], index: u64, now_ns: u64, min_resend_ns: u64) -> Lookup
pub fn entry[P](h: History[P], index: u64) -> Option[View[P]]
pub fn gate[P](mut h: History[P], index: u64)
pub fn clear[P](mut h: History[P]) -> u64

// module rtx
pub type Rtx = Repair(u16, u64) | Probe
pub fn unwrap(h: Header, pkt: Bytes, payload_at: u64, payload_len: u64) -> Result[Rtx, header.Error]
pub fn write(f: Fields, exts: View[ExtWriter], osn: u16, mut out: Bytes) -> Result[u64, WriteError]
pub fn write_probe(f: Fields, pad: u8, mut out: Bytes) -> Result[u64, WriteError]
```

**History.** A `History[P]` remembers what a stream sent, so a NACK can be answered. `P` is
the caller's: a forwarder's ingress history holds a handle to the received plaintext, and
each egress history holds a small record that leads back to it. `rtp` never copies a
payload into a history.

- The history is a ring of `slots` entries, indexed by `index mod slots`. Each entry keeps
  its full index, its time and the time it was last resent. `put` stores into the slot and
  answers the value it replaced, if any, so the caller drops it where it chooses. All
  storage is charged at `history`.
- `get(index, now, min_resend)` answers `Found(index)` when the slot holds that exact index,
  sent no more than `max_age_ns` ago, and not resent within `min_resend`. It then records the
  resend. It answers `Throttled` for a resend inside `min_resend`, `BeforeSwitch` for an
  index below the gate, and `Missing` otherwise.
- `gate(index)` refuses every later lookup below `index`. The rewriter sets it at a layer
  switch ([RTP-11](#rtp-11-stream-rewriting-and-layer-switching)): a packet from before a
  switch belongs to the old layer, which the receiver's decoder discards. LiveKit filters
  NACKs the same way (`pkg/sfu/rtpmunger.go:261-274` at `d210b4d`).
- `clear` drops every entry and answers how many it dropped.

**RTX.** A retransmission goes on the RTX stream with its own SSRC, payload type and
sequence numbers ([RFC 4588 §4](https://www.rfc-editor.org/rfc/rfc4588#section-4)). Its
payload is the original sequence number (OSN) in 2 bytes, followed by the original payload
without its padding. The timestamp and marker are the original's. `rtx.write` writes the
header and the OSN into `out`, and the caller seals the original payload behind them with
`seal_into` ([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement)), so it is never copied.

`rtx.unwrap` reads a received RTX packet after `header.payload` removed its padding:

- an empty payload is a `Probe`, the padding-only packet that senders use to probe
  bandwidth. It carries no OSN and repairs nothing;
- a 1-byte payload is malformed (`header.Error.Padding`);
- otherwise it is `Repair(osn, at)`, with the original payload from `at`.

The caller extends the OSN with `seq.estimate` against the **media** stream's highest index,
never the RTX stream's, and drops a packet whose OSN extends to `None`.

`rtx.write_probe` writes a padding-only RTX packet: the header with `P = 1` and `pad` bytes
of padding, of which the last is the count. `pad` is 1 to 255.

Law: a NACK is answered only for an index the history holds exactly, within its age, not
before the gate and not inside the resend interval. An RTX packet's payload is exactly the
OSN and the original payload. A padding-only RTX packet is a probe and never a repair.

## RTP-8: Transport-wide sequence numbers and feedback

```hirsel unbuilt
// module twcc
pub type Acked = Acked(index: u64, sent_ns: u64, size: u32, arrived_us: Option[i64])
pub opaque type SendLog
pub opaque type RecvLog

pub fn send_log(slots: u32) -> Result[SendLog, Capacity]
pub fn assign(mut s: SendLog, sent_ns: u64, size: u32) -> u16
pub fn on_feedback(mut s: SendLog, pkt: Bytes, fci_at: u64, fci_end: u64, mut out: List[Acked]) -> Result[u64, Error]

pub fn recv_log(slots: u32) -> Result[RecvLog, Capacity]
pub fn on_packet(mut r: RecvLog, seq: u16, at_ns: u64)
pub fn write(mut r: RecvLog, sender: u32, media: u32, max: u64, mut out: Bytes) -> Result[u64, Capacity]
```

Transport-wide congestion control ([draft-holmer-rmcat-transport-wide-cc-extensions-01](https://datatracker.ietf.org/doc/html/draft-holmer-rmcat-transport-wide-cc-extensions-01))
numbers every packet a transport sends, across all its streams, and the receiver reports
when each arrived.

**Sending.** `assign` gives the next transport sequence number (the caller writes it as the
`TransportSeq` extension) and records the send time and size in a ring of `slots`.
`on_feedback` reads a feedback FCI and appends one `Acked` per reported packet, in sequence
order, to `out`. `arrived_us` is the remote arrival time in microseconds on the remote
clock, or `None` for a packet reported lost. Only differences between arrival times mean
anything. A reported number whose record has left the ring, or that extends to `None`, is skipped. The base sequence
number is extended with `seq.estimate` against the highest assigned, and the reference time
with `estimate24` against the last report's. A feedback packet whose base number or reference
time extends to `None` is older than any report already read, and is skipped whole.

**The feedback format.** After the RTPFB header (FMT 15) and the two SSRCs:

- base sequence number (16 bits), packet status count (16), reference time (24, signed,
  in units of 64 ms), feedback packet count (8);
- packet status chunks, 16 bits each:
  - **run length**: `0 ‖ S(2) ‖ length(13)`;
  - **status vector**: `1 ‖ 0 ‖` fourteen 1-bit symbols (0 not received, 1 received with
    a small delta), or `1 ‖ 1 ‖` seven 2-bit symbols (`00` not received, `01` small delta,
    `10` large or negative delta, `11` reserved and refused);
- one receive delta per received packet, in units of 250 µs: 1 byte unsigned for a small
  delta, 2 bytes signed for a large one. The first is relative to the reference time, and
  each later one to the previous received packet.

**Receiving.** A `RecvLog` records each arrival by its extended transport number in a ring
of `slots`. `write` builds one feedback packet from the first unreported number to the
highest received, and advances past what it reported. Its choices are fixed, so its
output can be derived by hand:

- **Times are quantized once.** Each arrival is `q = floor((at_ns − origin) / 250_000)`, with
  `origin` the first arrival the log saw. The reference time is `floor(q_first / 256)` mod
  `2^24`. The first delta is `q_first − 256·ref`, and each later delta the difference of
  consecutive `q`. Rounding never accumulates.
- **A delta is small** when it is 0 to 255, and large when it fits a signed 16-bit value.
  A delta that fits neither ends the packet before that packet's number. The next `write`
  starts there, with a new reference time.
- **Chunks are greedy.** At each position, with `run` the number of equal symbols that
  start there: a run-length chunk of `min(run, 8191)` when `run >= 14`; else a 1-bit vector
  of the next `min(14, left)` symbols when none of them is large; else a 2-bit vector of the
  next `min(7, left)`. Unused symbol bits are 0.
- The packet ends early when the next chunk and its deltas would exceed `max` bytes or the
  status count would pass 65,535.
- The FCI is zero-padded to a 4-byte boundary, and `P` stays clear, so the packet may sit
  anywhere in a compound. `str0m` pads with RTCP padding and sets `P`
  (`src/rtp/rtcp/twcc.rs:217-226`). The parser accepts both.
- The feedback packet count increases by one per packet written, mod 256.

Law: `write` then `on_feedback` gives back every recorded arrival, quantized to 250 µs, with
no drift across a long run. Chunk choice is the greedy rule above, and every status count,
delta width and reference-time wrap is handled as the draft defines it.

## RTP-9: Bandwidth estimation

```hirsel unbuilt
// module bwe
pub type Usage = Normal | Overuse | Underuse
pub type Config = Config(start_bps: u64, min_bps: u64, max_bps: u64)
pub opaque type Bwe

pub fn bwe(c: Config) -> Bwe
pub fn on_acked(mut b: Bwe, acked: View[List[Acked]], now_ns: u64, rtt_ns: Option[u64])
pub fn target(b: Bwe) -> u64
pub type Cluster = Cluster(id: u32, rate_bps: u64, packets: u32, min_ns: u64)
pub fn probe(mut b: Bwe, want_bps: u64, now_ns: u64) -> Option[Cluster]
pub fn probe_sent(mut b: Bwe, id: u32, index: u64)
```

The estimator is Google congestion control's delay-based controller
([draft-ietf-rmcat-gcc-02](https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02)),
with the trendline detector and constants of libwebrtc as `str0m` carries them
(`src/bwe/delay/` at `2ada76a`), a classic loss rule, and probing. Floats are SEM-2 floats:
every expression below is evaluated as written, so two runs give the same bits. `exp` is
`std.float.exp`. Times inside the estimator are `f64` milliseconds.

**Arrival groups.** Acked packets with an arrival time, in sequence order, form groups
(`arrival_group.rs`). A packet joins the current group when its send time equals the
group's last, or when its propagation delta (arrival delta minus send delta, both against
the group's last packet) is negative, its arrival delta is at most 5 ms, and it arrived
within 100 ms of the group's first packet. Otherwise it starts a new group when its send
time is more than 5 ms after the group's first packet, and joins otherwise. A packet sent
before the group's first is skipped. When a group closes, the estimator takes the send
delta and arrival delta between its last packet and the previous group's last packet.

**Trendline.** Each delta pair adds a point (`trendline.rs`): `accumulated += arrival_delta
− send_delta`, `smoothed = 0.9·smoothed + 0.1·accumulated`, and the point is
`(arrival − first arrival, smoothed)`. The window holds the last 20 points. With a full
window the trend is the least-squares slope `Σ(x − x̄)(y − ȳ) / Σ(x − x̄)²`, or the previous
trend when the denominator is 0.

**Detector.** With `n` the number of points seen, capped at 1,000, and
`m = min(n, 60) · trend · 4`, against the threshold `γ` (initially 12.5):

- `m > γ`: overuse time grows by the send delta (half of it at the first such point), and
  the count by one. The answer becomes `Overuse` when the time passes 10 ms, the count passes
  1, and the trend rose since the last point.
- `m < −γ`: `Underuse`. Otherwise `Normal`. Both clear the overuse time and count.
- Then `γ` adapts, unless `|m| > γ + 15`: `γ += k · (|m| − γ) · min(Δt, 100)`, with
  `k = 0.039` when `|m| < γ` and `0.0087` otherwise, `Δt` the milliseconds since its last
  update, and `γ` clamped to `[6, 600]`.

**Acked rate.** The acked rate is the bytes of packets that arrived in the last 500 ms of
remote time, times 8, divided by the span from the window's first arrival to its last, with
the span at least 150 ms. It replaces libwebrtc's Bayesian estimator, whose extra state buys
smoothing that the 500 ms window already gives.

**Rate control** (`rate_control.rs`). The state moves on each detector answer: `Overuse`
to Decrease from any state; `Underuse` to Hold from any state; `Normal` from Hold to Increase,
from Decrease to Hold, and Increase stays. With `R` the acked rate and `A` the estimate:

- **Increase.** Nothing changes when `A >= 1.5·R + 10_000`. Otherwise, with `t` the seconds
  since the last change, capped at 1: near convergence, `A += max(1000, 0.5 · min(t / r, 1)
  · p)`, where `r` is the round-trip time plus 100 ms in seconds and `p` the expected packet
  size in bits: `f = A / 30`, `p = f / ceil(f / 9600)`. Otherwise
  `A += max(1000, (exp(t · ln 1.08) − 1) · A)`. Then `A = min(A, 1.5·R)`. (str0m writes `min(A, 1.5·R, 1.5·R + 10 kbps)`, which is the same
value.) The cap applies after the increase, so it may lower `A`, as in str0m.
- **Decrease**, at most once per round-trip time clamped to `[10, 200]` ms, unless `R` has
  fallen below `A / 2`: `A = min(A, 0.85 · R)`.
- **Convergence** follows libwebrtc's throughput average, in kbit/s with `α = 0.05`. On a
  decrease, an average that `R` falls more than 3 deviations below is cleared. Then
  `avg = (1 − α)·avg + α·R`, `var = (1 − α)·var + α·(avg − R)² / max(avg, 1)`, with `var`
  clamped to `[0.4, 2.5]` and starting at 0.4. On an increase, an average that `R` exceeds
  by more than 3 deviations is cleared. The deviation is `sqrt(var · avg)`. The estimate is
  near convergence while an average exists.
- `A` stays within `[min_bps, max_bps]`.

**Loss.** A second estimate `L`, starting at `start_bps`, follows the classic loss rule of
the GCC draft (§6). With `p` the fraction of packets reported lost since the last update, and
at least 20 reported: `p > 0.10` gives `L = L · (1 − 0.5·p)`, at most once per 300 ms plus
the round-trip time; `p < 0.02` gives `L = 1.05 · L`, at most once per second; otherwise `L`
holds. `target()` is `min(A, L)`.

**Probing.** A sender that sends less than it could cannot see more capacity: the increase
is capped at 1.5 times the acked rate. A forwarder that wants a higher layer must raise the
estimate first. `probe(want, now)` answers a cluster to send when `want > target()` and no
cluster is outstanding: `Cluster(id, rate, 5, 15 ms)` with `rate = min(2·target(), want)` and
`id` counting up from 0. The caller hands it to the pacer ([RTP-10](#rtp-10-pacer)), which
answers `Probe(id, bytes)`. The caller sends each one as a padding-only RTX packet, assigns it
a transport number, and tells the estimator with `probe_sent(id, index)`. The caller asks only
for a transport that has a stream with RTX, because a probe on any other stream would
reach a decoder. A cluster is outstanding until feedback covers its packets, or 1 s passes.
When feedback (`on_acked`) covers at least 80% of the packets recorded for the cluster, the send rate is `(bytes − last size) · 8 / (last send − first send)` and
the receive rate is `(bytes − first size) · 8 / (last arrival − first arrival)`. A result
`min(send, receive)` above `A` sets `A` to it.

Law: on a link whose queue grows, the detector answers `Overuse` and the target falls; on an
idle link the threshold decays to its floor; on a link with spare capacity a probe raises
the estimate to what the link delivered. The arithmetic is the text above, to the bit.

## RTP-10: Pacer

```hirsel unbuilt
// module pacer
pub type Class = Audio | Retransmit | Video | Padding
pub type Next[Q] = Send(Q) | Probe(u32, u64) | Pad(u64) | Wait(u64) | Idle
pub opaque type Pacer[Q]

pub fn pacer[Q](slots: u32, factor: f64, queue_limit_ns: u64) -> Result[Pacer[Q], Capacity]
pub fn set_rate[Q](mut p: Pacer[Q], target_bps: u64, padding_bps: u64)
pub fn probe[Q](mut p: Pacer[Q], c: Cluster, now_ns: u64)
pub fn push[Q](mut p: Pacer[Q], stream: u32, class: Class, size: u32, now_ns: u64, take q: Q) -> Result[(), Q]
pub fn poll[Q](mut p: Pacer[Q], now_ns: u64) -> Next[Q]
pub fn sent[Q](mut p: Pacer[Q], size: u32, now_ns: u64)
pub fn remove[Q](mut p: Pacer[Q], stream: u32, mut out: List[Q]) -> u64
```

The pacer spreads one transport's packets over time at a rate above the target, so a
keyframe does not leave as one burst that overflows a bottleneck queue (`str0m`
`src/pacer/leaky.rs`).

- The pacing rate is `factor · target` (2.5 by default, libwebrtc's). The pacer keeps a
  **debt** in bytes. Every send adds its size, and time drains it at the pacing rate.
  The debt never exceeds 500 ms at the pacing rate.
- `poll(now)` drains the debt for the time since the last poll. Then it answers, in order:
  - `Send(q)` for the oldest audio packet, whatever the debt: audio is small, regular and
    late audio is worse than a burst;
  - `Send(q)` for the oldest queued packet of the highest class (Retransmit, then Video),
    when the debt is 0;
  - `Probe(id, bytes)` while a cluster is active, when the debt is 0 and no media packet is
    queued. The cluster's rate stands in for the pacing rate while it runs, and `bytes` is
    `max(200, rate · 1 ms / 8)`, at most 1,200. The cluster ends when it has answered
    `packets` probes and `min_ns` has passed since its first;
  - `Pad(bytes)` when the queues are empty, a padding rate is set, and the padding debt
    allows it, for the caller to send as padding-only RTX packets;
  - `Wait(at)`, the time the debt reaches 0, when packets are queued;
  - `Idle` otherwise.
- **Queue limit.** When the queued bytes would take longer than `queue_limit_ns` at the
  pacing rate, the rate rises to send them within the limit.
- `push` with no free slot gives back `q` (`Error(q)`), which the caller drops. Nothing is
  dropped silently.
- `remove(stream)` moves every queued entry of the stream to `out` and answers the count.

Law: a packet of size `s` leaves no sooner than the debt allows, so over any interval the
bytes sent (excluding audio) are at most the pacing rate times the interval plus one
packet. Audio is never held behind video. A full pacer refuses explicitly.

## RTP-11: Stream rewriting and layer switching

```hirsel unbuilt
// module rewrite
pub type Unit = Unit(start: bool, end: bool, keyframe: bool, needs: u16, switches: u16)
pub type Target = Off | Layer(source: u32, target: u8)
pub type Out = Forward(u64, u64) | Late(u64, u64) | Drop | Wait
pub opaque type Rewrite
pub opaque type Counter

pub fn rewrite(clock: u32, ranges: u32, first_seq: u64, first_ts: u64) -> Result[Rewrite, Capacity]
pub fn target(mut r: Rewrite, t: Target)
pub fn on_packet(mut r: Rewrite, source: u32, index: u64, ts: u64, at_ns: u64, padding_only: bool, u: Unit) -> Out
pub fn on_sr(mut r: Rewrite, source: u32, info: SenderInfo)
pub fn sr_timestamp(r: Rewrite, ntp_now: u64) -> Option[u32]
pub fn switch_index(r: Rewrite) -> Option[u64]
pub fn pending(r: Rewrite) -> Option[u32]
pub fn close(mut r: Rewrite)

pub fn counter(width: u8) -> Counter
pub fn restart(mut c: Counter, first: u64)
pub fn map(c: Counter, value: u64) -> u64
```

A forwarder sends one publication to many subscribers, and each subscriber may receive a
different layer. A `Rewrite` turns the source packets one subscriber receives into one
continuous outgoing stream. The receiver sees sequence numbers with no gaps except real
losses, timestamps that advance with the media, and a layer change only where its decoder
can follow it. A rewrite answers numbers; the caller writes the header
([RTP-1](#rtp-1-the-fixed-header)) and seals.

**Sources and units.** A source is an incoming stream, named by the caller's 32-bit key
(for simulcast, one per layer SSRC). Every packet arrives with its authenticated index,
its extended timestamp and a `Unit` the caller fills from `media_bits`:

- `start` and `end` mark the first and last packet of a frame;
- `keyframe` marks a frame that needs no earlier frame;
- `needs` has bit `i` set when target `i` needs this frame. Targets are up to 16 decode
  targets. For a dependency descriptor, a target needs a frame unless its indication is
  Not Present ([AV1 RTP §A.8.3](https://aomediacodec.github.io/av1-rtp-spec/#a83-decode-target-indication)).
  For VP8, VP9 or H.264 temporal layers, target `i` needs every frame whose temporal layer is
  at most `i`;
- `switches` has bit `i` set when a decoder may begin to follow target `i` at this frame:
  a keyframe allows every target, a Switch indication allows its target, and a frame that
  starts a lower temporal layer allows that layer.

**Targets.** `target(Layer(source, i))` asks to forward target `i` of `source`. `target(Off)`
stops forwarding. The current layer changes only at a switch point:

- within one source, from target `i` to `j` at a packet with `start` and bit `j` of
  `switches`;
- to another source, only at a packet of that source with `start` and `keyframe`;
- from `Off`, as to another source.

Until then the old layer keeps forwarding. With no current layer, every packet answers
`Wait`, and the caller asks for a keyframe ([RTP-6](#rtp-6-loss-tracking-nack-and-keyframe-requests)).
While a target on another source is pending, that source's packets answer `Drop`: the caller
asks for that source's keyframe when it sets the target, and again at each `Keyframes`
interval until the switch happens. `pending(r)` answers the pending source, if any.

**Sequence numbers.** A packet is forwarded when it belongs to the current source and the
current target's bit is set in `needs`. Its outgoing index is `index − offset`.

- A packet that is not forwarded, or that is padding only, is **dropped and closed up**:
  every later packet's offset grows by one, so the receiver never asks for it. The rewrite
  keeps a ring of `ranges` entries `(from index, offset)`, one per change of offset.
- A packet below the highest forwarded index takes the offset of its range. A packet older
  than the oldest range answers `Drop`. The ring is the caller's explicit bound.
- A real upstream loss leaves a gap in the outgoing indices. The receiver asks for it, and
  the egress history maps it back.

**Late packets.** An SRTP sealer refuses any index not above every index it has sealed
(TLS-8, Sender ownership). A source packet that arrives after a later one was forwarded has
an outgoing index below the highest sealed, so it cannot go on the media stream. The
rewrite answers `Late(index, ts)`. When the subscriber negotiated RTX, the caller sends the
packet as an RTX repair with that index as its OSN, which the receiver accepts as a
retransmission. Without RTX, the packet is dropped. This is the cost of a sealer that never
reuses a nonce: a stream without RTX, commonly audio, loses every packet that the network
reordered past its successor. A sealer with a window of unsealed indices would remove that
cost, and would belong to TLS-8. The decision is logged with its reversibility in
`followups.md`.

**Timestamps.** Within a source the outgoing timestamp is `ts − ts_offset`, so media timing
is kept exactly. At a switch to another source the offset is chosen so the stream stays
continuous:

- when both sources have an SR, by capture time. With `n` the switch packet's NTP time,
  `n = ntp_B + ((ts − rtp_B) · 2^32) / clock`, the timestamp on the old source's timeline
  is `rtp_A + ((n − ntp_A) · clock) >> 32`, with signed floor division in 128 bits;
- otherwise by arrival: the last outgoing timestamp plus
  `max(1, ((at_ns − last_at_ns) · clock) / 10^9)`.

The result is raised to at least the last outgoing timestamp plus 1, so a timestamp never
goes back.

**Switch record.** At every source switch the rewrite records the first outgoing index of
the new layer. `switch_index` answers it, and the caller passes it to `History.gate`
([RTP-7](#rtp-7-retransmission-history-and-rtx)).

**Codec counters.** Payloads carry their own counters: the VP8 picture ID (15 bits) and
TL0PICIDX (8 bits), the VP9 picture ID (15 bits) and the dependency descriptor's frame
number (16 bits). A `Counter` of `width` bits keeps them continuous the same way. At a
switch the caller calls `restart(first)` with the new source's first value, which sets the
offset so the output continues at the last output plus 1. `map(value)` answers
`(value − offset) mod 2^width`. `media_bits` writes the result into the payload with its
`rewrite_*` operations, on the caller's per-subscriber copy of the payload descriptor.

**Sender reports.** `sr_timestamp(ntp_now)` answers the outgoing timestamp for `ntp_now`,
extrapolated from the current source's last SR (`rtp + ((ntp_now − ntp) · clock) >> 32 −
ts_offset`, mod `2^32`). It is `None` before the current source has sent one. The caller
then sends a receiver report in place of a sender report.

Law: the outgoing indices of one stream increase by exactly one per forwarded packet,
except across real upstream losses. A dropped or padding-only packet never leaves a gap. A
layer change happens only at a switch point. The outgoing timestamp never goes back, and a
late packet is never sealed on the media stream.

## RTP-12: State, charges and close

Every piece of state is sized when it is made: a history's slots, a log's ring, a pacer's
slots, a loss window, a rewrite's ranges. Each constructor admits its full size from the
current limit ([MEM-24](budgets.md#mem-24-logical-charges)) or answers `Capacity`, and the
drop refunds it. No per-packet operation allocates, grows a list or admits a charge. The
caller's output lists (`pairs`, `out`) are the caller's, sized by the caller, and their
growth is the only charge on that path.

**Close.** A forwarder that stops a subscriber, or revokes a participant, needs nothing
already decided to leave afterwards. The forwarding spec runs the fence. `rtp` gives it these
operations, which never wait:

- `Rewrite.close()`: every later `on_packet` answers `Drop`.
- `History.clear()`: no NACK can be answered from it.
- `Pacer.remove(stream)`: every queued entry for the stream is handed back.
- dropping a `Losses`, `Keyframes`, `SendLog`, `RecvLog` or `Bwe` refunds its charge.

After these return, no packet of that stream can come out of `rtp` again.

**Cost.** PLAN Wave 1.4 sets the target: **at most 80 ns to parse and rewrite one packet,
with 0 allocations**. The measured path is `header.parse`, `ext.find` for MID, audio level
and transport sequence number, `Rewrite.on_packet`, and `header.write` of a subscriber
header with audio level and transport sequence number, on a 1,200-byte packet shaped like
the vector of law `header_chrome_vector`. Instructions come from one `perf stat` run, and
cycles from 60 interleaved pairs on a shielded CPU, as the host rules say. The forwarding
spec owns the per-subscriber fan-out target (250 ns), of which this path is part. No peer
ratio is quoted.

## Prior art

Goose (`/tmp/ref-goose` at `d5ae5bb`) is the language peer. `str0m` (`/tmp/ref-str0m` at
`2ada76a`) is the protocol peer. The rewriter is checked against LiveKit's
(`/tmp/ref-livekit` at `d210b4d`).

| source | mechanism | verdict | why |
|---|---|---|---|
| Goose `stdlib/binary.goose:9-18` | checked reads that test `at <= len && size <= len − at`, so a bad offset cannot wrap | ADOPTED | every offset check in RTP-1, RTP-3 and RTP-4 |
| Goose `stdlib/slot_pool.goose` | a freelist pool whose freed slot is reused while old references still see it | REPLACED | `History` is a ring indexed by `index mod slots` that checks the stored index on every lookup, so a stale entry cannot answer for a new one |
| str0m `src/rtp/header.rs:129-226` | one-pass fixed header and extension parse | ADAPTED | the parse stops at the header and leaves padding until after decryption, because SRTP encrypts it |
| str0m `src/rtp/ext.rs:472-531` | one- and two-byte element walk that stops quietly at an overrun | ADAPTED | `rtp` refuses an overrun, so a forwarder never re-sends a header it could not read |
| str0m `src/rtp/header.rs:256-292` | ROC-style extension of 16- and 32-bit counters | ADOPTED | as `seq.estimate`, shared with SRTP so the stack has one unwrapper |
| str0m `src/streams/rtx_cache.rs` | an evicting buffer of sent packets by sequence and age, plus a size index for padding | ADAPTED | the history stores the caller's handle, never a copy; the size index is not taken, because probes are padding only |
| str0m `src/rtp/rtcp/twcc.rs` | TWCC register and wire format, with deltas from quantized absolute times | ADOPTED | the format and the no-drift quantization; padding is zero bytes in the FCI with `P` clear |
| str0m `src/bwe/delay/` | GCC arrival groups, trendline, detector, AIMD | ADOPTED | the constants and the arithmetic, written out in RTP-9 |
| str0m `src/bwe/acked_bitrate_estimator.rs` | Bayesian acked-rate estimate | REPLACED | a 500 ms window rate, which is enough for the decrease and the increase cap |
| str0m `src/bwe/loss_controller.rs` | libwebrtc's loss-based estimator v2 | NOT TAKEN | the GCC draft's classic loss rule is enough for Wave 1 |
| str0m `src/pacer/leaky.rs` | leaky-bucket pacer with debt and padding | ADAPTED | strict classes with audio never held, an explicit full answer, and removal by stream |
| LiveKit `pkg/sfu/rtpmunger.go:135-274` | range map of sequence offsets, drop close-up, NACK gate after a keyframe switch | ADAPTED | the same model, with a bounded ring of ranges and a `Late` answer, because a TLS-8 sealer cannot seal a reordered index |
| LiveKit `pkg/sfu/forwarder.go` | codec-aware layer selection | REPLACED | `rtp` takes `needs` and `switches` masks and knows no codec; `media_bits` computes them |
| str0m `src/rtp/rtcp/remb.rs`, `xr.rs` | REMB and XR | NOT TAKEN | TWCC replaces REMB, and no Wave 1 consumer reads XR |

## Build laws

The build lane writes each law **red first**: the case is added to its harness before the
code it tests, and it fails there. Each law names its oracle, which comes from outside the
code under test. The harness is the package's law suite, `core/programs/rtp/laws.hirsel` <!-- doc-ref: planned -->
(`[tests.sources]` in `core/programs/rtp/hirsel.toml` <!-- doc-ref: planned -->), unless a law names another.

- **`header_chrome_vector` (RTP-1, RTP-3).** *Input:* the 48 bytes of `hb1` in str0m's
  `src/rtp/header.rs:545-549`, with the map `1 AudioLevel, 2 AbsSendTime, 3 TransportSeq,
  4 Mid` (`ext.rs:373-386`). *Oracle:* str0m's expectation (`header.rs:567-588`), checked by
  hand. `seq` 47000, `ts` 10000, `ssrc` 777459193, `P` and `X` set, `end` 32, MID `xYj`,
  audio level `(true, 42)`, abs-send-time `0xFE206A` (63.531654 s), transport sequence 0. After
  `header.payload` the payload is `[1, 2, 3, 4]`, from offset 32 with 12 bytes of padding.
- **`padding_counts` (RTP-1).** A 13-byte packet with `P` set and last byte 0 is `Padding`.
  Last byte 2 is `Padding` (only 1 byte follows the header). Last byte 1 gives an empty
  payload. *Oracle:* RFC 3550 §5.1.
- **`extension_forms` (RTP-3).** A hand-built two-byte block (`0x1000`): a padding byte, an
  element of id 7 with length 0, an element of id 20 with a 17-byte RID, then an element whose
  length runs one byte past the block. `next` answers the first two, the RID's range, and then
  `Overrun`. A one-byte block with id 15 before a valid element answers `None` at id 15.
  *Oracle:* RFC 8285 §4.2 and §4.3, by hand.
- **`egress_form` (RTP-3).** A subscriber map without `two_byte` and with RID bound to 3:
  writing audio level `(true, 42)` and transport sequence 0x1234 gives the one-byte block
  `BE DE 00 02 | 10 AA 21 12 34 00 00 00` for ids 1 and 2. Adding a 17-byte RID is refused
  with `Form`. With `two_byte`, the same three give a `0x1000` block, laid out by hand.
  *Oracle:* RFC 8285 §4.2 and §4.3 and RFC 6464 §3.
- **`index_estimate` (RTP-2).** `estimate(65535, 0) = Some(65536)`. `estimate(65536, 65535) = Some(65535)`.
  `estimate(0, 65535) = None` (no ROC −1 below zero). `estimate(196618, 65436) = Some(196508)`.
  *Oracle:* RFC 3711 §3.3.1, by hand.
- **`nack_pairs` (RTP-4, RTP-6).** The pair `(65436, 0b1000_0000_0000_1001)` read against a
  highest index of 196,618 expands to 196,508, 196,509, 196,512 and 196,524. *Oracle:* str0m
  `src/rtp/rtcp/nack.rs:134-155`. A `Losses` with those four due packs them back into that
  one pair. *Oracle:* RFC 4585 §6.2.1.
- **`rtcp_compound_rules` (RTP-4).** Hand-built datagrams: an SR, then an SDES with `P` set
  is accepted; `P` on the SR is `Padding`; a lone PLI is `NotCompound` without `reduced`
  and accepted with it; a length word one past the datagram is `Length`. `is_rtcp` holds for
  second bytes 192 and 223 and fails for 191 and 224. *Oracle:* RFC 3550 A.2, RFC 5506 §3,
  RFC 5761 §4.
- **`rtt_example` (RTP-5).** A block with `lsr = 0xB7052000` and `dlsr = 0x00054000`, read at
  an NTP time whose middle bits are `0xB7108000`, gives 6.125 s. *Oracle:* the worked example
  of RFC 3550 §6.4.1.
- **`reception_report` (RTP-5).** Clock 90,000. Indices 0 to 9 with 3 and 7 missing, each
  with timestamp `3000·i`, arriving at `i · 33.4 ms`. The report has `lost = 2`,
  fraction `(2 << 8) / 10 = 51`, and the jitter that A.8's integer recurrence gives over the
  eight transits, computed by hand in the law's comment. *Oracle:* RFC 3550 A.3 and A.8.
- **`keyframe_coalesce` (RTP-6).** Interval 200 ms, FIR. `want` at 0, 10 and 50 ms answers
  `Some(0)`, `None`, `None`. At 210 ms it answers `Some(0)` again (repeat, same number). After
  `keyframe_seen`, `want` at 220 ms answers `Some(1)`. *Oracle:* RFC 5104 §4.3.1.1, by hand.
- **`history_lookup` (RTP-7).** Four slots. Put indices 1 to 6. `get(2)` is `Missing`
  (overwritten), `get(5)` is `Found(5)`, and a second `get(5)` inside `min_resend` is
  `Throttled`. Past `max_age` it is `Missing`. After `gate(6)`, `get(5)` is `BeforeSwitch`.
  `clear` answers 4, and `get(6)` is then `Missing`. *Oracle:* RTP-7, by hand.
- **`rtx_layout` (RTP-7).** `rtx.write` for OSN 0x1234 with a 3-byte original payload gives
  the header, `12 34`, then the payload after `seal_into`. `unwrap` of an RTX packet whose
  payload is all padding is `Probe`, and of a 1-byte payload is an error. *Oracle:* RFC 4588
  §4, by hand.
- **`twcc_report_bytes` (RTP-8).** Arrivals of transport numbers 10, 11, 12 and 13 at 0, 12,
  23 and 43 ms (the arrivals of str0m's `twcc.rs:1529-1549`). `write` with sender 1 and media
  2 gives the FCI `00 0A 00 04 00 00 00 00 | BC 00 | 00 30 2C 50 | 00 00` and an RTCP length
  of 6. *Oracle:* the draft's layout, by hand: one 1-bit vector chunk of four received
  symbols, and deltas of 0, 48, 44 and 80 quarter-milliseconds.
- **`twcc_chunks` (RTP-8).** 221 packets received with small deltas encode as the one
  run-length chunk `0x20DD`. A negative delta forces a 2-bit vector and a 2-byte delta. A
  delta of 8.2 s ends the packet before that number. A reference time of `0xFFFFFF` followed
  by `0x000000` extends to the next 64 ms. *Oracle:* the draft's §3.1, by hand.
- **`trendline_flat` (RTP-9).** Groups 5 ms apart with no delay change: every answer is
  `Normal`, and the threshold falls `12.5 · 0.805^n` until it clamps at 6.0 on the fourth
  update. *Oracle:* RTP-9's recurrence, by hand.
- **`trendline_overuse` (RTP-9).** The input sequence of str0m's `test_overuse`
  (`src/bwe/delay/trendline.rs:323-388`) gives the hypotheses that test expects. *Oracle:*
  the peer's test, which shares RTP-9's constants.
- **`aimd_steps` (RTP-9).** From `A = 1,000,000` in Increase, not near convergence, with
  `R = 1,000,000` and `t = 1`: `A = 1,080,000` (within the 1.08 rounding of `exp`, which the
  law states to the bit). An `Overuse` with `R = 800,000` gives `A = 680,000`. *Oracle:* RTP-9,
  by hand.
- **`probe_result` (RTP-9).** Five 1,200-byte probe packets sent 1 ms apart and received 2 ms
  apart give a send rate of 9.6 Mbit/s, a receive rate of 4.8 Mbit/s, and a new estimate of
  4.8 Mbit/s from 1 Mbit/s. *Oracle:* RTP-9, by hand.
- **`pacer_spacing` (RTP-10).** Target 400 kbit/s, factor 2.5 (pacing 1 Mbit/s). Three
  1,250-byte video packets queued at 0 leave at 0, 10 and 20 ms. An audio packet queued at
  5 ms leaves at 5 ms. A pacer with two slots refuses a third push and gives it back.
  `remove` of the video stream answers its queued count. *Oracle:* RTP-10, by hand.
- **`rewrite_continuity` (RTP-11).** One source, target 0. Indices 100 to 105, with 102 a
  frame that target 0 does not need and 104 padding only. Outgoing indices are 1000, 1001,
  1002 and 1003 for 100, 101, 103 and 105, and the timestamps keep their source spacing. A
  late 102 answers `Drop`. *Oracle:* RTP-11, by hand.
- **`rewrite_late` (RTP-11).** Index 201 arrives after 202 was forwarded as 2002: it answers
  `Late(2001, ts)`. A real gap (203 lost, 204 forwarded as 2004) leaves 2003 unsent. *Oracle:*
  TLS-8's sealer rule and RTP-11, by hand.
- **`simulcast_switch` (RTP-11).** Target moves from source A to B. B's packets answer `Drop`
  until one with `start` and `keyframe`. That packet's outgoing index is the last plus 1, and
  its timestamp is the SR-mapped value worked by hand from two given SRs. `switch_index` is
  that index. A second case with no SRs uses the arrival rule. *Oracle:* RTP-11, by hand.
- **`svc_masks` (RTP-11).** A three-target stream with hand-written `needs` and `switches`
  masks per frame. Target 2 is asked for at frame 1. It takes effect at the first frame whose
  `switches` has bit 2, and frames no target needs are closed up. *Oracle:* RTP-11, by hand.
- **`close_leaves_nothing` (RTP-12).** After `Rewrite.close`, `History.clear` and
  `Pacer.remove` for a stream, `on_packet` answers `Drop`, every lookup is `Missing`, and
  `poll` never answers an entry of that stream. *Oracle:* RTP-12.
- **`nack_recovery_under_loss` (RTP-6, RTP-7).** *Harness:* `std.sim.udp` (W0.3), with a
  sender (history, RTX, pacer) and a receiver (`Losses`) as two sans-IO ends. Two thousand
  packets at 50 per second cross a link with 10% independent loss each way and 40 ms delay.
  With `tries` 10, `retry_ns` 100 ms, `max_age_ns` and the history's age 2 s, and `min_resend`
  50 ms, every packet arrives. A repair fails when its NACK or its RTX is lost, with chance
  `0.19`, so one packet is lost for good with chance `0.1 · 0.19^10 ≈ 6.1·10^−9`, and any of the
  2,000 with chance below `1.3·10^−5`. The law runs 8 fixed seeds. *Oracle:* that probability, by hand.
- **`congestion_backoff` (RTP-8 to RTP-10).** *Harness:* `std.sim.udp`, a 1 Mbit/s bottleneck
  with a 100-packet queue, and a sender offering 3 Mbit/s. Within 10 s the target is between
  0.7 and 1.1 Mbit/s and stays there, and the 95th percentile queueing delay over the last
  5 s is below 200 ms. *Oracle:* the scenario's capacity.
- **`parse_rewrite_cost` (RTP-12).** The cost path of RTP-12: at most 80 ns, 0 allocations.
  *Oracle:* the measurement, on a shielded CPU.

## Build slices

Each slice is one Sol lane's commit sequence. It lands when its laws pass. The order is a
dependency order, and S1 to S8 need neither `sim.udp` nor any crypto build.

1. **S1, header and extensions** (RTP-1, RTP-2, RTP-3): `core/programs/rtp/` <!-- doc-ref: planned --> with
   `hirsel.toml`, modules `header`, `seq`, `ext`. Laws `header_chrome_vector`,
   `padding_counts`, `extension_forms`, `egress_form`, `index_estimate`.
2. **S2, RTCP** (RTP-4): module `rtcp`, parse and the writers. Laws `rtcp_compound_rules` and
   the parse half of `nack_pairs`.
3. **S3, statistics and requests** (RTP-5, RTP-6): modules `stats` and `loss`. Laws
   `rtt_example`, `reception_report`, `keyframe_coalesce`, the packing half of `nack_pairs`.
4. **S4, history and RTX** (RTP-7): modules `history` and `rtx`. Laws `history_lookup`,
   `rtx_layout`.
5. **S5, TWCC** (RTP-8): module `twcc`. Laws `twcc_report_bytes`, `twcc_chunks`.
6. **S6, bandwidth estimation** (RTP-9): module `bwe`. Laws `trendline_flat`,
   `trendline_overuse`, `aimd_steps`, `probe_result`.
7. **S7, pacer** (RTP-10): module `pacer`. Law `pacer_spacing`.
8. **S8, rewriting** (RTP-11, RTP-12): module `rewrite` with `Counter`, and the close
   operations. Laws `rewrite_continuity`, `rewrite_late`, `simulcast_switch`, `svc_masks`,
   `close_leaves_nothing`.
9. **S9, VP8 in `media_bits`**: a `vp8` module that parses the RFC 7741 payload descriptor
   (picture ID, TL0PICIDX, TID, the `S` and `PID` fields), detects a keyframe from the first
   payload byte, and rewrites the picture ID and TL0PICIDX in place. Its laws are the RFC
   7741 §4.2 descriptor layouts by hand, in `core/programs/media_bits/vp8-laws.hirsel` <!-- doc-ref: planned -->.
   Independent of S1 to S8.
10. **S10, network laws**: `nack_recovery_under_loss` and `congestion_backoff`. Needs S4 to S7
    and `std.sim.udp` (media-w0-sim-udp-2324). It waits for W0.3 to land.
11. **S11, cost**: `parse_rewrite_cost`. Needs S1, S3 and S8. It is the only slice with a cost
    target, and the only one that runs a benchmark.

Every slice that changes a `.hirsel` source under `core/programs/rtp` <!-- doc-ref: planned --> or
`core/programs/media_bits` refreshes that package's `hirsel.laws` in the same commit.
