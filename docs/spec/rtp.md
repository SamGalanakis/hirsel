# RTP and RTCP

Version note (2026-11-24, media-rtp-spec-2318): first text of RTP-1 to RTP-12, Wave 1.4 of the
real-time media track. Amended by rtp-spec-amend-2340 for the 16 findings of review 2336 and the
rtp side of WebRTC review 2333 (findings 4 and 5). Amended by rtp-s8-rewrite-2476 for the five
RTP-11 gaps slice S9 (2362) found: `Unit.frame`, `Unit`'s home, the H.264 caller state, the
target-number sentence, and AV1 without chains; and for the switch projection's rounding. Not yet built; the fences are `unbuilt`. The
section ends with the [build laws](#build-laws) and the [build slices](#build-slices).

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
`ext.find`, `rtcp.check`, `rtcp.next`, `twcc.parse` and the value readers) may run on bytes that have not been
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
| each subscriber's own wire identity (PT, MID, extension IDs, transport numbers) and repair | [RTP-7](#rtp-7-retransmission-history-and-rtx) per-leg send contract |
| Opus, VP8 and H.264 passed through with no server decode | payloads are opaque here; frame facts come from the `media_bits` unit adapter ([RTP-11](#rtp-11-stream-rewriting-and-layer-switching), slice S9), which gains a VP8 payload descriptor module |
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
pub type Error = Short | Version | Csrc | Extension | Padding | Buffer | Index
pub type Block = NoBlock | OneByte(at: u64, len: u64) | TwoByte(at: u64, len: u64) | Opaque(profile: u16, at: u64, len: u64)
pub type Header = Header(marker: bool, pt: u8, seq: u16, ts: u32, ssrc: u32, csrcs: u8, padded: bool, block: Block, end: u64, len: u64)
pub type Fields = Fields(marker: bool, pt: u8, seq: u16, ts: u32, ssrc: u32)
pub type WriteError = Capacity(Capacity) | Form

pub fn parse(pkt: Bytes) -> Result[Header, Error]
pub fn payload(h: Header, pkt: Bytes) -> Result[(u64, u64), Error]
pub fn csrc(h: Header, pkt: Bytes, i: u8) -> Result[u32, Error]
pub fn write(f: Fields, w: View[ExtWriter], mut out: Bytes) -> Result[u64, WriteError]
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
length. When `P` is set, the last byte is the padding count, which includes itself. The
count is read only when `len − end >= 1`; with no byte after the header, `P` is `Padding`.
A count of 0, or one larger than `len − end`, is `Padding`. A count equal to `len − end`
gives an empty payload: a padding-only packet, which is valid.

**Views are bound to their buffer.** A `Header` records `len`, the length of the buffer it
was parsed from. Every accessor that takes a `Header` with a buffer (`payload`, `csrc`, and
RTP-3's readers) first checks that the buffer's length is `h.len` and answers `Buffer`
otherwise. It reads no byte before that check. `csrc(h, pkt, i)` answers `Index` when
`i >= h.csrcs`. No accessor reads a byte before checking that it is present, and a failed
accessor changes nothing. (Two different buffers of one length are the caller's fault, which
the check cannot see. The rule makes a stale view harmless, never out of bounds.)

Each offset check is written as `at <= len && size <= len − at`, so no sum can wrap.

`header.write(f, w, mut out)` appends a fixed header with `V = 2`, `P = 0`, `CC = 0` and
the extension block of `w`, a view of one `ExtWriter` that holds up to 16 elements
([RTP-3](#rtp-3-header-extensions)), to the spare of `out`,
and answers the bytes appended. A forwarder never forwards CSRCs: it is not a mixer, so
the header it writes has none. Its errors are `Capacity` (the spare is short, with the
whole header's length as `requested`) and the form error of RTP-3. A refused write appends
nothing.

Law: `parse` accepts exactly the headers RFC 3550 §5.1 and RFC 8285 §4 describe, answers
the same `end` that SRTP authenticates, and reads no byte past it. `payload` refuses exactly
the padding counts that cannot be. Every accessor is bounded by its own view and buffer, and
a refused access changes nothing. `write` followed by `parse` gives back the fields and
extension values written.

## RTP-2: Extended sequence numbers

```hirsel unbuilt
// module seq
pub type Index = Index(u64) | Before | Beyond
pub fn nearest(highest: i64, wire: u64, bits: u8) -> i64
pub fn estimate(highest: u64, s: u16) -> Index
pub fn estimate32(highest: i64, t: u32) -> i64
pub fn estimate24(highest: i64, r: u32) -> i64
```

Wire counters wrap. Every stateful part of `rtp` works in 64-bit extended values instead,
so no comparison anywhere is modular.

**One nearest-cycle calculation.** `seq.nearest(highest, wire, bits)` is the index estimate
of [RFC 3711 §3.3.1](https://www.rfc-editor.org/rfc/rfc3711#section-3.3.1) and Appendix A,
for a counter of `bits` bits (16, 24 or 32), computed in signed arithmetic wide enough that
nothing wraps. With `w = bits`, `half = 2^(w−1)`, `s_l = highest mod 2^w` (taken as a
non-negative residue) and `roc = floor(highest / 2^w)`:

- if `s_l < half`: `v = roc − 1` when `wire − s_l > half`, else `v = roc`;
- otherwise: `v = roc + 1` when `s_l − half > wire`, else `v = roc`;
- the candidate is `v · 2^w + wire`.

The comparisons are strict and on integers, not modular values, so a tie at exactly half
the range keeps `roc`, as Appendix A does. The candidate is the nearest cycle and nothing
more: a packet whose true index is more than half the range from `highest` gets the wrong
candidate ([RFC 3711 §3.3.1](https://www.rfc-editor.org/rfc/rfc3711#section-3.3.1) warns of
it). Under SRTP that packet then fails authentication and is discarded, which is the only
safe answer. A gap that large needs trusted resynchronization (SRTP-4's `receive_from` in
[the DTLS spec](dtls.md)) or a new SRTP context. `rtp` has neither and guesses nothing.

**Typed bounds before narrowing.** `seq.estimate(highest, s)` is `nearest(highest, s, 16)`
checked against the SRTP index domain `[0, 2^48 − 1]` before it is narrowed to `u64`:

- below 0 is `Before`: a packet from before the stream began, which no sender sent. SRTP
  refuses it as `TooOld` (SRTP-4), and every `rtp` caller skips it;
- above `2^48 − 1` is `Beyond`: the index space is exhausted. A sender stops and rekeys
  (DTLS law `sender_index_monotone`); a receiver discards the packet;
- otherwise `Index(i)`.

A first packet has `highest` equal to its own `seq` (ROC 0), as RFC 3711 §3.3.1 starts it.
The 48-bit bound is Appendix A's ROC modulo `2^32`, stated as a range instead of a wrap.

SRTP calls `estimate` to pick the index it authenticates. **It never commits an
unauthenticated estimate**: it commits the new `highest` only after `open` succeeds, and it
hands the authenticated index to the caller. Every stateful `rtp` operation takes that
index. `rtp` keeps no second unwrapper, so the index that SRTP authenticated is the index that
statistics, loss tracking and rewriting use.

**Timestamps and TWCC reference times are signed.** `estimate32` applies the same
`nearest` rule to 32-bit RTP timestamps, and `estimate24` to the 24-bit TWCC reference time.
They have no lower bound: an extended timestamp is per source, starts at the first
authenticated packet's raw timestamp, and may go below it, because an authenticated packet
reordered before the first one can carry an earlier timestamp. So they answer `i64`, and
`i64` holds every value a stream can reach.

**A source restart is not a crypto event.** When an authenticated sender restarts its
sequence (RFC 3550 A.1's dropout and restart rule, [RTP-5](#rtp-5-reception-statistics-reports-and-round-trip-time)
`Restarted`), the statistics start again from the restart index. The SRTP index keeps
rising; nothing resets it.

Law: `nearest` is RFC 3711 Appendix A's estimate, including its ties. `estimate` answers
`Before` and `Beyond` exactly outside `[0, 2^48 − 1]`, and the extended values it gives are
the only ordering any stateful operation uses.

## RTP-3: Header extensions

```hirsel unbuilt
// module ext
pub type Kind = Mid | Rid | RepairedRid | AudioLevel | AbsSendTime | TransportSeq
pub type Form = OneOnly | TwoOnly | Mixed
pub type Error = Overrun | Value(Kind) | Buffer
pub type Element = Element(id: u8, at: u64, len: u64)
pub type MapError = BadId | IdTaken | KindTaken | Full | Value(Kind)
pub opaque type ExtMap
pub opaque type ExtWriter

pub fn map(form: Form) -> ExtMap
pub fn bind(mut m: ExtMap, id: u8, k: Kind) -> Result[(), MapError]
pub fn vad(mut m: ExtMap, on: bool)
pub fn next(pkt: Bytes, h: Header, mut at: u64) -> Result[Option[Element], Error]
pub fn find(m: ExtMap, pkt: Bytes, h: Header, k: Kind) -> Result[Option[Element], Error]
pub fn audio_level(m: ExtMap, pkt: Bytes, h: Header, e: Element) -> Result[(Option[bool], u8), Error]
pub fn abs_send_time(pkt: Bytes, h: Header, e: Element) -> Result[u32, Error]
pub fn transport_seq(pkt: Bytes, h: Header, e: Element) -> Result[u16, Error]
pub fn token(pkt: Bytes, h: Header, k: Kind, e: Element) -> Result[Element, Error]
pub fn writer() -> ExtWriter
pub fn put(mut w: ExtWriter, m: ExtMap, k: Kind, value: Bytes, from: u64, len: u64) -> Result[(), MapError]
pub fn put_level(mut w: ExtWriter, m: ExtMap, voice: bool, level: u8) -> Result[(), MapError]
pub fn put_other(mut w: ExtWriter, m: ExtMap, id: u8, value: Bytes, from: u64, len: u64) -> Result[(), MapError]
```

An **extension map** binds local ids to kinds, as an SDP `a=extmap` line negotiates them
([RFC 8285 §5](https://www.rfc-editor.org/rfc/rfc8285#section-5)). Each peer and direction
has its own map, because a publisher's ids and a subscriber's ids are negotiated apart. Its
`Form` is what the session negotiated ([RFC 8285 §§4.1.2, 6](https://www.rfc-editor.org/rfc/rfc8285#section-6)):

- `OneOnly`: one-byte headers only. Ids are 1 to 14.
- `TwoOnly`: two-byte headers only, as for a peer that negotiated ids above 14 without
  `a=extmap-allow-mixed`. Ids are 1 to 255.
- `Mixed`: the peer sent `a=extmap-allow-mixed`; each packet may use either form. Ids are 1
  to 255, and an id above 14 can only travel in a two-byte block.

`bind` refuses an id outside its form's range, so id 15 or 16 in a `OneOnly` map is `BadId`,
and an id or kind already bound (`IdTaken`, `KindTaken`). `vad(m, on)` records the
negotiated RFC 6464 `vad` attribute ([RFC 6464 §4](https://www.rfc-editor.org/rfc/rfc6464#section-4));
the session resolves the attribute's default before calling it. A kind `rtp` does not name
stays unbound: the caller reads it with `next` and the raw element, and writes it with
`put_other`. The dependency descriptor goes that way to `media_bits.dependency`.

**Reading.** `ext.next(pkt, h, mut at)` walks the block's elements in order, from `at = 0`,
whatever the map's form, because the block's profile says which form it is. It first checks
the buffer against `h` (`Buffer`, RTP-1).

- One-byte form: a zero byte is padding and is skipped. Each element is one byte `id ‖ L`
  with `len = L + 1`. Id 15 ends the walk with `None`, and so does id 0 with a nonzero `L`:
  both are reserved, and the rest of the block is ignored, as
  [RFC 8285 §4.2](https://www.rfc-editor.org/rfc/rfc8285#section-4.2) says.
- Two-byte form: a zero byte is padding. Each element is `id`, `len` (0 to 255), then the
  value. A nonzero `id` that is the block's last byte, with no length byte after it, is
  `Overrun`.
- An element whose value runs past the block is `Overrun`. An `Opaque` or absent block has
  no elements.

Every `Some` moves `at` past at least one byte, and the end of the walk is terminal: once
`next` has answered `None` it moves `at` to the block's end, and every later call answers
`None`. So a walk takes at most the block's length in steps.

`find` walks the **whole** block before it answers, and answers the first element whose id
the map binds to `k` only when the walk ends without `Overrun`. An `Overrun` anywhere in the
block is `Overrun`, even after a match.

An `Overrun` comes from an authenticated sender that broke the format. The forwarder drops
the packet. `str0m` stops reading at the overrun and keeps the packet
(`src/rtp/ext.rs:472-531` at `2ada76a`); `rtp` refuses it instead, so a forwarder never
re-sends a header it could not read.

**Values.** Each reader checks the buffer against `h`, checks that `e` lies inside `h`'s
block, and checks its value's length. It answers `Buffer` or `Value(k)` otherwise:

| kind | URI | value | reader answer |
|---|---|---|---|
| `AudioLevel` | `urn:ietf:params:rtp-hdrext:ssrc-audio-level` | exactly 1 byte `V ‖ level` ([RFC 6464 §3](https://www.rfc-editor.org/rfc/rfc6464#section-3); libwebrtc `AudioLevelExtension::Parse` reads one octet) | `(voice, level)`, level 0 to 127 in −dBov; `voice` is `None` when the map's `vad` is off, because `V` then means nothing |
| `AbsSendTime` | `http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time` | exactly 3 bytes, 6.18 fixed-point seconds | the 24-bit value |
| `TransportSeq` | `http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01` | exactly 2 bytes | the 16-bit transport sequence number |
| `Mid` | RFC 9143 §15 | 1 to 255 bytes, each an SDP `token-char` ([RFC 8866 §9](https://www.rfc-editor.org/rfc/rfc8866#section-9)), the grammar of the negotiated `a=mid` identification-tag; so `audio-1` is legal | the element's range, unchanged |
| `Rid`, `RepairedRid` | RFC 8852 §3 | 1 to 255 bytes, each ASCII `A-Z a-z 0-9`; the wire `RtpStreamId` grammar is narrower than SDP's `rid-id`, so `a_b` is refused | the element's range, unchanged |

A token reader answers a range in the packet, not a `String`. The caller compares it with
the negotiated value in place, so reading a MID allocates nothing.

**Writing.** An egress header carries the subscriber's ids, not the publisher's. The
forwarder fills an `ExtWriter` (a fixed buffer of 256 bytes inside the writer, with up to 16
elements) and `header.write` lays the block out. `put` looks up the subscriber map's id for
`k`. An unbound kind is skipped and answers `Ok`, because the subscriber did not negotiate
it. `put` checks the value against the kind's wire grammar above and answers `Value(k)`
otherwise, so a forwarder never writes a RID the receiver would refuse. `put_level` writes
exactly one audio-level octet, with `V` clear when the map's `vad` is off. `put_other`
refuses an id outside the map's form range. Past 16 elements or 256 bytes, a put is `Full`.
The form is chosen once per header from the map's `Form`:

- `OneOnly`: one-byte. A value of 0 or more than 16 bytes is refused with `Form` at
  `header.write`, and nothing is appended.
- `TwoOnly`: two-byte, even when every element would fit the one-byte form.
- `Mixed`: one-byte when every id is 1 to 14 and every value is 1 to 16 bytes, else
  two-byte.

Elements are written in the order put, with no padding between them. The block is
zero-padded to a 4-byte boundary. A writer with no elements writes no block and leaves `X`
clear.

Law: `next` walks exactly RFC 8285's element grammar in both forms, makes progress at every
step, and stops for good at its end. Each reader accepts exactly its documented lengths and
grammar. `write` uses exactly the form the map's negotiation allows, and never writes an
element a receiver of that negotiation would misread.

## RTP-4: RTCP compound packets

```hirsel unbuilt
// module rtcp
pub type Mode = Compound | Reduced
pub type Error = Version | Length | Padding | NotCompound | NoCname | Body(u8) | Buffer | Index
pub type Item = Sr(SenderInfo, u8) | Rr(u32, u8) | Sdes(u8) | Bye(u8, Option[(u64, u64)])
    | Nack(u32, u32) | Twcc(u32, u32) | Pli(u32, u32) | Fir(u32) | Other(u8, u8)
pub type Packet = Packet(item: Item, at: u64, body: u64, end: u64, len: u64)
pub type SenderInfo = SenderInfo(ssrc: u32, ntp: u64, rtp: u32, packets: u32, octets: u32)
pub type Block = Block(ssrc: u32, fraction: u8, lost: i32, highest: u32, jitter: u32, lsr: u32, dlsr: u32)
pub type WriteError = Capacity(Capacity) | Unrepresentable

pub fn is_rtcp(pkt: Bytes) -> bool
pub fn check(pkt: Bytes, mode: Mode) -> Result[u64, Error]
pub fn next(pkt: Bytes, mut at: u64) -> Result[Option[Packet], Error]
pub fn block(pkt: Bytes, p: Packet, i: u8) -> Result[Block, Error]
pub fn cname(pkt: Bytes, p: Packet, ssrc: u32) -> Result[Option[(u64, u64)], Error]
pub fn nack_next(pkt: Bytes, p: Packet, mut at: u64) -> Result[Option[(u16, u16)], Error]
pub fn fir_next(pkt: Bytes, p: Packet, mut at: u64) -> Result[Option[(u32, u8)], Error]

pub fn write_sr(info: SenderInfo, blocks: View[List[Block]], mut out: Bytes) -> Result[u64, WriteError]
pub fn write_rr(ssrc: u32, blocks: View[List[Block]], mut out: Bytes) -> Result[u64, WriteError]
pub fn write_cname(ssrc: u32, cname: Bytes, mut out: Bytes) -> Result[u64, WriteError]
pub fn write_bye(ssrc: u32, mut out: Bytes) -> Result[u64, WriteError]
pub fn write_nack(sender: u32, media: u32, pairs: View[List[(u16, u16)]], mut out: Bytes) -> Result[u64, WriteError]
pub fn write_pli(sender: u32, media: u32, mut out: Bytes) -> Result[u64, WriteError]
pub fn write_fir(sender: u32, target: u32, seq: u8, mut out: Bytes) -> Result[u64, WriteError]
```

**Demultiplexing.** On a BUNDLE transport RTP and RTCP share one port.
`rtcp.is_rtcp(pkt)` is false for fewer than 4 bytes, the RTCP common header. Otherwise it is
true when the second byte is 192 to 223, the payload-type range RFC 5761 §4 reserves for
RTCP. A forwarder never negotiates an RTP payload type of 64 to 95, which that rule would
misroute.

**Checking the whole datagram first.** `rtcp.check(pkt, mode)` validates every packet of a
datagram before any of it reaches a stateful operation, and answers the packet count. A
datagram that fails anywhere is dropped whole. Each packet is checked, in order, with every
length computed in 64-bit arithmetic:

- `Length`: fewer than 4 bytes remain, or `4·(length + 1)` bytes do not fit. The last packet
  ends exactly at the datagram's end.
- `Version`: the version is not 2.
- `Padding`: `P` is set on a packet that is not the last of the datagram; or the count byte
  is 0, or larger than the packet's body after its 4-byte header. For a packet whose body is
  made of 32-bit words (SR, RR, SDES, BYE, NACK, PLI, FIR) the count is also a multiple of 4
  ([RFC 3550 §6.4.1](https://www.rfc-editor.org/rfc/rfc3550#section-6.4.1)). For TWCC (FMT 15),
  whose FCI ends at any byte, the count is 1 to 255, and the FCI must end within the 3 bytes
  before the padding ([RTP-8](#rtp-8-transport-wide-sequence-numbers-and-feedback)). Every
  body check below is on the **unpadded** body.
- `Body(pt)`: the unpadded body breaks its table row below.
- `NotCompound`: in `Compound` mode, the first packet is not SR or RR. In either mode, a
  datagram whose first packet is SR or RR is an ordinary compound packet, and must hold an
  SDES whose chunks include the first packet's sender SSRC with a CNAME
  ([RFC 3550 §6.1](https://www.rfc-editor.org/rfc/rfc3550#section-6.1)); otherwise `NoCname`.
  A lone RR is `NoCname`.

`Reduced` is passed when the session negotiated `a=rtcp-rsize`. Then a datagram that does
not begin with SR or RR is reduced-size ([RFC 5506 §3](https://www.rfc-editor.org/rfc/rfc5506#section-3))
and may hold any packets in any order.

| type, format | item | unpadded body |
|---|---|---|
| 200 SR | `Sr(info, count)` | at least `24 + 24·RC`; later bytes are a profile extension and are skipped |
| 201 RR | `Rr(ssrc, count)` | at least `4 + 24·RC` |
| 202 SDES | `Sdes(count)` | exactly `SC` chunks. Each chunk: an SSRC, then items of `type ‖ length ‖ value`, each inside the body; a CNAME (type 1) has length 1 to 255; then an END byte (0) and zero bytes to the next 32-bit boundary, inside the body |
| 203 BYE | `Bye(count, reason)` | `4·SC` bytes of SSRCs, then either nothing or a reason `length ‖ text` inside the body followed by zero bytes to the boundary |
| 205, FMT 1 | `Nack(sender, media)` | both SSRCs, then an FCI of `4n` bytes, `n >= 1` ([RFC 4585 §6.2.1](https://www.rfc-editor.org/rfc/rfc4585#section-6.2.1)) |
| 205, FMT 15 | `Twcc(sender, media)` | both SSRCs, then at least the 8-byte FCI prefix; the rest is checked by [RTP-8](#rtp-8-transport-wide-sequence-numbers-and-feedback)'s parser, which `check` runs |
| 206, FMT 1 | `Pli(sender, media)` | exactly the two SSRCs: the FCI is empty and the packet unpadded ([RFC 4585 §6.3.1](https://www.rfc-editor.org/rfc/rfc4585#section-6.3.1)) |
| 206, FMT 4 | `Fir(sender)` | both SSRCs, the media SSRC field ignored, then an FCI of `8n` bytes, `n >= 1` ([RFC 5104 §4.3.1.1](https://www.rfc-editor.org/rfc/rfc5104#section-4.3.1.1)) |
| anything else | `Other(pt, fmt)` | none |

**Reading.** After `check`, `rtcp.next(pkt, mut at)` answers one packet as a `Packet` view:
its item, its start `at`, the start of its body `body`, the end of its unpadded body `end`,
and the datagram length `len`. Every accessor that takes a `Packet` checks the buffer's
length against `len` (`Buffer`) and stays inside `[body, end)`. `block(pkt, p, i)` answers
`Index` when `i` is not below the item's count. `cname(pkt, p, ssrc)` finds that SSRC's
CNAME in an SDES. `nack_next` reads `(PID, BLP)` pairs and `fir_next` reads `(SSRC, seq)`
entries, each answering `None` at `end`. The caller expands each NACK pair into PID and every
`PID + i + 1` whose bit `i` is set, and extends each with `seq.estimate` against the stream's
highest sent index, skipping anything but `Index`.

**Writing.** Each writer appends one packet (or, for reports, a run of packets) to the spare
of `out`, with `P = 0`, and answers its length. It first checks that the packet can be
represented at all: a CNAME of 0 or more than 255 bytes, or a packet longer than `2^16`
words, is `Unrepresentable`. Then a short spare is `Capacity` with the whole length as
`requested`. A refused write appends nothing.

- `write_sr` and `write_rr` take any number of report blocks. RC has 5 bits, so the first
  packet carries up to 31 blocks and each further 31 go in another RR under the same SSRC
  ([RFC 3550 §6.4](https://www.rfc-editor.org/rfc/rfc3550#section-6.4)). 32 blocks give one
  packet of 31 and an RR of 1.
- `write_pli` writes both SSRCs and an empty FCI. `write_nack` writes one FCI entry per pair.
- `write_fir` writes the sender SSRC, a media SSRC field of 0, and one FCI entry: the target
  SSRC, the command sequence number, and three reserved bytes of 0
  ([RFC 5104 §4.3.1.1](https://www.rfc-editor.org/rfc/rfc5104#section-4.3.1.1)).

**Composing.** The caller sends regular reports as ordinary compound packets: SR or RR
first, then an SDES with its CNAME, then the rest. The first RTCP datagram of a session is
always compound ([RFC 5506 §4](https://www.rfc-editor.org/rfc/rfc5506#section-4)). After it,
with `a=rtcp-rsize`, feedback that should go at once (NACK, PLI, FIR, TWCC) may go
reduced-size.

Law: `check` accepts exactly the compound datagrams RFC 3550 §6.1 and A.2 allow and, with
`Reduced`, the reduced-size ones RFC 5506 §3 allows, and nothing reaches a stateful operation
before it does. Every view is bounded by its packet. Each writer's output parses back to its
inputs, and a refusal appends nothing.

## RTP-5: Reception statistics, reports and round-trip time

```hirsel unbuilt
// module stats
pub type Seen = Counted | Held | Restarted
pub opaque type Received
pub opaque type Sent

pub fn received(clock: u32, first: u64, first_ts: i64, at_ns: u64) -> Received
pub fn on_packet(mut r: Received, index: u64, ts: i64, at_ns: u64) -> Seen
pub fn on_sr(mut r: Received, info: SenderInfo, at_ns: u64)
pub fn report(mut r: Received, ssrc: u32, now_ns: u64) -> Block
pub fn rtt(b: Block, ntp_now: u64) -> Option[u64]

pub fn sent(clock: u32) -> Sent
pub fn on_sent(mut s: Sent, payload: u64)
pub fn sender_info(s: Sent, ssrc: u32, ntp_now: u64, rtp_now: u32) -> SenderInfo
```

**Receiving.** A `Received` follows one incoming SSRC's packets on the wire, from its first
authenticated packet ([RFC 3550 A.3](https://www.rfc-editor.org/rfc/rfc3550#appendix-A.3)).
It holds the base index, the highest index, the count received, the `expected_prior` and
`received_prior` of the last report, the last transit and the jitter. It skips RFC 3550's
probation: the session negotiated the SSRC, and SRTP authenticated the packet. Statistics are
per SSRC on the wire: a repair that arrives on an RTX SSRC is counted in that SSRC's
`Received`, never in the media stream's. Whether a media packet was delivered, as original
or repair, is [RTP-6](#rtp-6-loss-tracking-nack-and-keyframe-requests)'s.

- **Restart.** RFC 3550 A.1's rule, on authenticated indices: a packet 3,000 or more
  above the highest, or 100 or more below it (A.1's `MAX_DROPOUT` and `MAX_MISORDER`
  bounds), is `Held` and counted nowhere. When the next
  packet's index is the held index plus 1, the statistics restart there, as if that held
  packet were the first, and `on_packet` answers `Restarted`. Any other packet clears the hold
  and is counted normally. A restart touches only statistics; SRTP's index is unchanged
  ([RTP-2](#rtp-2-extended-sequence-numbers)).
- `expected = highest − base + 1`. `lost = expected − received`, a signed value clamped to
  the 24-bit range `[−2^23, 2^23 − 1]`. Duplicates count as received, so `lost` can be
  negative, as RFC 3550 §6.4.1 allows.
- Per report: `expected_interval = expected − expected_prior`,
  `lost_interval = expected_interval − (received − received_prior)`, both signed. The fraction
  is `(lost_interval << 8) / expected_interval` when both are positive, else 0.
- The block's `highest` is the low 32 bits of the highest extended index:
  `highest mod 2^32`, whose top half is the cycle count
  ([RFC 3550 §6.4.1](https://www.rfc-editor.org/rfc/rfc3550#section-6.4.1)).
- **Jitter** is RFC 3550 A.8's integer form, on signed values. The arrival time in timestamp
  units is `a = (at_ns · clock) / 10^9`, computed exactly in 128 bits and floored, and
  `transit = a − ts` is signed, since `ts` is a signed extended timestamp
  ([RTP-2](#rtp-2-extended-sequence-numbers)). The first packet only sets the last transit.
  For each later packet, with `d = |transit − last_transit|`, the scaled jitter `J` (16 times
  the jitter) is updated as `J += d − ((J + 8) >> 4)`. The report carries `J >> 4`. A packet
  whose index is not above the highest still counts as received and still updates the
  jitter, as RFC 3550 does; equal arrival or sample times are allowed.
- `on_sr` keeps the SR's middle 32 NTP bits and the arrival time. A report's `lsr` is those
  bits, and its `dlsr` is the delay since then in units of `1/65536` s, floored. Both are 0
  before any SR.

**Round-trip time.** `stats.rtt(b, ntp_now)` reads a block that a peer sent about one of our
streams. With `A` the middle 32 bits of `ntp_now`, `r = (A − lsr − dlsr) mod 2^32` in units
of `1/65536` s ([RFC 3550 §6.4.1](https://www.rfc-editor.org/rfc/rfc3550#section-6.4.1)). The
compact NTP value wraps every 65,536 s, so the subtraction is modular. The answer is `r` in
nanoseconds, floored, when `r < 2^31` (under 32,768 s), and `None` when `lsr` is 0 (no SR
yet, which RFC 3550 marks with 0) or when `r >= 2^31`, which is a negative round trip or one
too long to be real.

**Sending.** A `Sent` counts one outgoing SSRC's packets and payload octets (not headers
or padding), as an SR reports them; the SR's two counters are those counts modulo `2^32`.
Each outgoing SSRC, media or RTX, has its own `Sent`, and its SR goes out under that SSRC.
`sender_info` takes the NTP time and the matching RTP timestamp from the caller. For a
rewritten stream the caller gets the RTP timestamp from `Rewrite.sr_timestamp`
([RTP-11](#rtp-11-stream-rewriting-and-layer-switching)), so the SR speaks the subscriber's
timeline and counts only what was sent to that subscriber. An SSRC that sent nothing since
its last report sends an RR, not an SR ([RFC 3550 §6.4](https://www.rfc-editor.org/rfc/rfc3550#section-6.4)).
A forwarder never relays a publisher's SR to a subscriber.

Law: the counts, fraction, cumulative loss, highest and jitter are RFC 3550 A.3 and A.8 to
the bit, across reordering, duplicates, restarts and wraps. The round-trip time is RFC 3550
§6.4.1's, across the compact NTP wrap.

## RTP-6: Loss tracking, NACK and keyframe requests

```hirsel unbuilt
// module loss
pub type NackConfig = NackConfig(window: u32, retry_ns: u64, tries: u8, max_age_ns: u64)
pub type Gap = Tracked | Reset
pub type Due = Done(Option[u64]) | More
pub opaque type Losses
pub opaque type Keyframes
pub type Why = Loss | Switch
pub type Request = Pli | Fir(u8)

pub fn losses(c: NackConfig, first: u64, at_ns: u64) -> Result[Losses, Capacity]
pub fn on_packet(mut l: Losses, index: u64, at_ns: u64) -> Gap
pub fn due(mut l: Losses, now_ns: u64, rtt_ns: Option[u64], max: u32, mut pairs: List[(u16, u16)]) -> Due

pub fn keyframes(fir: bool, interval_ns: u64) -> Keyframes
pub fn want(mut k: Keyframes, why: Why, now_ns: u64) -> Option[Request]
pub fn set_interval(mut k: Keyframes, interval_ns: u64)
pub fn keyframe_seen(mut k: Keyframes)
```

**Losses.** A `Losses` follows one incoming media stream for which the session negotiated
`nack`. It holds a bitmap of `window` indices below the highest, with the time each missing
index was found missing, the time it was last asked for and its tries. An original and a
repair (an RTX packet whose OSN extended to that index, [RTP-7](#rtp-7-retransmission-history-and-rtx))
both clear the index, and a second arrival of either is a duplicate that changes nothing.

- `losses` refuses a `window` of 0 with `Capacity` (requested 0 is never admitted), and
  admits the window's complete charge ([RTP-12](#rtp-12-state-charges-and-close)).
- `on_packet` with an index above the highest marks every index between them missing. A gap
  wider than `window` clears the bitmap and answers `Reset`: the caller then asks for a
  keyframe, because asking for that many packets would not repair the stream sooner. An
  index at or below the highest clears its bit, if it is in the window.
- `due(now, rtt, max, mut pairs)` appends at most `max` NACK pairs to send now, in ascending
  index order, to `pairs`. An index is due when it has never been asked for, or when
  `retry_ns` (or the round-trip time, when the caller has one and it is larger) has passed
  since it was. After `tries` requests, or `max_age_ns` after it went missing, it is dropped
  from the bitmap without another request. Only the indices it actually wrote are marked as
  asked; the rest stay due. It answers `More` when due indices remain that did not fit,
  and otherwise `Done` with the next time something is due. `pairs` must have room for `max`
  more entries: `due` checks that before it changes anything, and otherwise appends nothing,
  changes nothing and answers `More`.
- Pairs are packed greedily: the lowest due index is a PID, and each due index from
  `PID + 1` to `PID + 16` sets bit `index − PID − 1` of its BLP.

The window is the caller's explicit bound. No cap is hidden: an index outside the window
answers `Reset`.

**Keyframe requests.** Many subscribers may ask for a keyframe of one publication within a
few milliseconds. A `Keyframes` coalesces them into one request per requester SSRC and
target media SSRC, which is the scope of a FIR command sequence number
([RFC 5104 §4.3.1.1](https://www.rfc-editor.org/rfc/rfc5104#section-4.3.1.1)). `fir` records
whether the upstream peer negotiated `ccm fir`.

- `why` says what the request is for. `Loss` is picture loss: it is always a PLI, because
  RFC 5104 §4.3.1.2 forbids FIR for it. `Switch` is a decoder that must start fresh (a layer
  switch or a new subscriber): it is a FIR when `fir` is set, and a PLI otherwise.
- `want(why, now)` answers a request when one should go upstream now, and `None` when one is
  already outstanding and `interval_ns` has not passed since it was sent. An outstanding
  request of either kind covers a new `want` of either kind, because one keyframe answers
  both.
- A FIR request is **new** when no FIR is outstanding: it takes the next command sequence
  number, mod 256. A **repeat**, sent when `interval_ns` passed with the request still
  outstanding, carries the same number, as RFC 5104 §4.3.1.1 requires.
- `set_interval` changes the interval in place, as the caller's round-trip estimate moves.
  The outstanding request and its FIR command sequence number are kept, and the next repeat
  is due at the last send plus the new interval.
- `keyframe_seen` ends the outstanding request. The next `want` is new and goes at once.

The caller detects keyframes with `media_bits` and tells `Keyframes`. Which subscribers
asked, and whether they may ask, is the caller's.

Law: NACK pairs cover exactly the missing indices that are due, each in the fewest pairs a
left-to-right packing gives, and an output that is too short loses no retry state. A burst
of `want` calls inside one interval yields one upstream request. Picture loss never sends a
FIR, and the FIR sequence number changes only for a new request.

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
pub fn unwrap(h: Header, pkt: Bytes) -> Result[Rtx, header.Error]
pub fn write(f: Fields, w: View[ExtWriter], osn: u16, pre: Bytes, payload: Bytes, from: u64, len: u64, mut out: Bytes) -> Result[u64, WriteError]
pub fn write_probe(f: Fields, w: View[ExtWriter], pad: u8, mut out: Bytes) -> Result[u64, WriteError]
```

**History.** A `History[P]` remembers what a stream sent, so a NACK can be answered. `P` is
the caller's: a forwarder's ingress history holds a handle to the received plaintext, and
each egress history holds the record the per-leg contract below requires. `rtp` never copies
a payload into a history.

- The history is a ring of `slots` entries, indexed by `index mod slots`. `slots` is 1 to
  `2^15`; `history` refuses anything else with `Capacity`. Each entry keeps its full index, its
  time and the time it was last resent. `put` stores into the slot and answers the value it
  replaced, if any, so the caller drops it where it chooses. All storage is charged at
  `history`.
- **Wire lookups.** A NACK names a 16-bit number, which the caller extends with
  `seq.estimate` against the highest index sent. The estimate names the one index within
  half the range of the highest. Because a ring holds at most `2^15` consecutive indices, it
  never holds two entries with the same low 16 bits, and an entry the estimate cannot name is
  never answered.
- `get(index, now, min_resend)` answers `Found(index)` when the slot holds that exact index,
  sent no more than `max_age_ns` ago, and not resent within `min_resend`. It then records the
  resend. It answers `Throttled` for a resend inside `min_resend`, `BeforeSwitch` for an
  index below the gate, and `Missing` otherwise.
- `gate(index)` refuses every later lookup below `index`. A gate only rises: a lower
  `index` than the current gate changes nothing. The rewriter sets it at a source
  switch ([RTP-11](#rtp-11-stream-rewriting-and-layer-switching)): a packet from before a
  switch belongs to the old source, which the receiver's decoder discards. LiveKit filters
  NACKs the same way (`pkg/sfu/rtpmunger.go:261-274` at `d210b4d`).
- `clear` drops every entry and answers how many it dropped.

**Per-leg send contract.** One publication reaches many subscribers, each a separate leg
with its own negotiation, so nothing on a leg's wire may come from the publisher's. For each
output (RFC 4588 §§4–5, RFC 8852 §3; mediasoup restores each consumer's fields before RTX in
`RtpStreamSend.cpp:575-599` at `90ca2ae`):

- Use that leg's negotiated payload type, MID value, extension ids ([RTP-3](#rtp-3-header-extensions))
  and stream identifiers (SSRC, RID, repaired RID).
- Assign a fresh transport sequence number on that leg's transport at **every actual send**,
  media, RTX repair and probe alike ([RTP-8](#rtp-8-transport-wide-sequence-numbers-and-feedback)).
- Key the leg's egress history by the extended **output** index, and keep in `P` the source
  generation, a bounded handle and range of the shared plaintext, and the original output
  timestamp, marker and codec rewrite: enough to reproduce the exact media payload that leg
  was sent, including its rewritten codec descriptor. A plaintext handle alone is not enough.
- Gate the history at every source switch.
- On a NACK, extend each number against the leg's highest output index, look it up, and send
  the repair with the mapped RTX payload type and RTX SSRC, the RTX stream's next sequence
  number, the original **output** index as OSN, the original timestamp and marker, and the
  saved payload.
- On an authenticated inbound RTX packet, resolve `apt` to the media payload type and the
  stream association (the `a=ssrc-group:FID` pairing or the repaired RID, as negotiated),
  extend the OSN with `seq.estimate` against **that media stream's** highest index (never
  the RTX stream's), drop anything but `Index`, and deduplicate repair against original there
  ([RTP-6](#rtp-6-loss-tracking-nack-and-keyframe-requests)).

**RTX.** A retransmission goes on the RTX stream with its own SSRC, payload type and
sequence numbers ([RFC 4588 §4](https://www.rfc-editor.org/rfc/rfc4588#section-4)). Its
payload is the original sequence number (OSN) in 2 bytes, big-endian, followed by the
original egress payload without its padding. The timestamp and marker are the original's.

The OSN is **payload**, so SRTP must encrypt it. `rtx.write(f, w, osn, pre, payload, from,
len, mut out)` builds the RTX header, then appends the OSN, `pre` (the leg's rewritten codec
descriptor, or nothing) and `payload[from..from + len]` (the shared remainder) behind it into
a packet pre-sized in `out`, and answers the header's length. Like `header.write`, it
appends the whole packet or nothing: a short spare is `Capacity` with the whole packet's
length as `requested`. The caller then passes the plaintext packet to `srtp.protect_rtp`
([SRTP-3](dtls.md#srtp-3-streams-roc-and-sending)). It parses the RTP header and uses its end
as the offset `at` for in-place protection. It authenticates the header and encrypts the OSN
and the rest of the payload, including any rewritten codec descriptor. A repair costs one
copy of its payload. See `rtx_protected` in [Build laws](#build-laws).

`rtx.unwrap(h, pkt)` reads a received, decrypted RTX packet. It takes the payload range
from `header.payload`, so it is bound to `pkt` like every RTP-1 accessor (`Buffer`), and
reads nothing outside that range:

- an empty payload is a `Probe`, the padding-only packet that senders use to probe
  bandwidth. It carries no OSN and repairs nothing;
- a 1-byte payload is malformed (`header.Error.Padding`);
- otherwise it is `Repair(osn, at)`, with the original payload from `at`.

**Probes.** `rtx.write_probe(f, w, pad, mut out)` writes a padding-only RTX packet: the
header with `P = 1` and `pad` bytes of padding, of which the last is the count, and answers
the header's length, as `rtx.write` does. `pad` is 1 to 255, the most one padding count
octet can say ([RFC 3550 §5.1](https://www.rfc-editor.org/rfc/rfc3550#section-5.1)); a `pad`
of 0 is `Form`, and nothing is appended.
A probe packet's **size**, as the pacer and the estimator count it, is its RTP bytes before
protection, header and padding included. A larger probe budget is split into several
packets, each with its own transport number.

Law: a NACK is answered only for an index the history holds exactly, within its age, not
before the gate and not inside the resend interval, on that leg's own RTX identity. An RTX
packet's plaintext payload is exactly the OSN and the original egress payload, and after
protection neither is readable on the wire. A padding-only RTX packet is a probe and never a
repair.

## RTP-8: Transport-wide sequence numbers and feedback

```hirsel unbuilt
// module twcc
pub type Acked = Acked(index: u64, sent_ns: u64, size: u32, arrived_us: Option[i64])
pub type Error = Prefix | Chunk | Symbol | Deltas | Tail | Capacity(Capacity) | Buffer
pub opaque type SendLog
pub opaque type RecvLog

pub fn send_log(slots: u32) -> Result[SendLog, Capacity]
pub fn assign(mut s: SendLog, sent_ns: u64, size: u32) -> u16
pub fn parse(pkt: Bytes, p: Packet) -> Result[u32, Error]
pub fn on_feedback(mut s: SendLog, pkt: Bytes, p: Packet, max: u32, mut out: List[Acked]) -> Result[u64, Error]

pub fn recv_log(slots: u32) -> Result[RecvLog, Capacity]
pub fn on_packet(mut r: RecvLog, seq: u16, at_ns: u64)
pub fn write(mut r: RecvLog, sender: u32, media: u32, max: u64, mut out: Bytes) -> Result[u64, Capacity]
```

Transport-wide congestion control ([draft-holmer-rmcat-transport-wide-cc-extensions-01](https://datatracker.ietf.org/doc/html/draft-holmer-rmcat-transport-wide-cc-extensions-01))
numbers every packet a transport sends, across all its streams, and the receiver reports
when each arrived.

**The feedback format.** After the RTPFB header (FMT 15) and the two SSRCs:

- an 8-byte prefix: base sequence number (16 bits), packet status count (16), reference
  time (24, signed, in units of 64 ms), feedback packet count (8);
- packet status chunks, 16 bits each:
  - **run length**: `0 ‖ S(2) ‖ length(13)`;
  - **status vector**: `1 ‖ 0 ‖` fourteen 1-bit symbols (0 not received, 1 received with
    a small delta), or `1 ‖ 1 ‖` seven 2-bit symbols (`00` not received, `01` small delta,
    `10` large or negative delta, `11` reserved);
- one receive delta per received packet, in units of 250 µs: 1 byte unsigned for a small
  delta, 2 bytes signed for a large one. The first is relative to the reference time, and
  each later one to the previous received packet.

**Parsing.** `twcc.parse(pkt, p)` validates one FCI completely and answers its status count.
It emits nothing; `rtcp.check` runs it, and `on_feedback` runs it again before changing any
state. The packet status count is independent of the FCI's byte length, and the parser walks
both with one cursor that only moves forward:

- `Prefix`: the 8-byte prefix does not fit.
- Chunks are read until they represent exactly the status count. A run-length chunk with
  length 0 represents no status and is consumed (its 2 bytes are the progress). A run of
  `S = 11` is `Symbol`. A run's or a vector's statuses past the status count are clipped and
  ignored. A `11` symbol inside the count is `Symbol`. A chunk that does not fit is `Chunk`.
- Then one delta per received status, each inside the FCI, else `Deltas`.
- After the last delta at most 3 bytes may remain, each 0, else `Tail` (RTCP padding, when
  `P` is set, was already removed by `check`).

The cursor never passes the unpadded FCI end, so the work is bounded by the FCI's length.

**Sending.** `assign` gives the next transport sequence number (the caller writes it as the
`TransportSeq` extension) and records the send time and size in a ring of `slots`, at every
actual send ([RTP-7](#rtp-7-retransmission-history-and-rtx)). `on_feedback(s, pkt, p, max,
mut out)` first parses; when the status count exceeds `max`, or `out` lacks room for it, it
answers `Capacity` and changes nothing, so the caller can retry with more room. Then it
appends one `Acked` per reported packet whose state changed, in sequence order. `arrived_us`
is the remote arrival time in microseconds on the remote clock, or `None` for a packet
reported lost. Only differences between arrival times mean anything.

- The base sequence number is extended with `seq.estimate` against the highest assigned,
  and the reference time with `estimate24` against the last report's (0 before the first).
  A base that extends to anything but `Index` is skipped whole.
- A reported number whose record has left the ring is skipped.
- **Overlap.** Reports may overlap (draft §3.1.1). Each send record keeps its receipt state:
  unreported, lost, or received. A packet reported received is emitted once, the first
  time; a later report that marks it not received changes nothing, because it arrived. A
  packet reported lost and later reported received is emitted again as received. A loss is
  emitted once.

**Receiving.** A `RecvLog` records each arrival by its extended transport number in a ring
of `slots`; a number already reported, a repeat, or one `slots` or more below the highest is
ignored. `write` builds one feedback packet from the first unreported number (before the
first write, the lowest seen; never one that has left the ring) to the highest received, and
advances past what it reported. With nothing to report it writes nothing and answers 0. Its
choices are fixed, so its output can be derived by hand:

- **Times are quantized once.** Each arrival is `q = floor((at_ns − origin) / 250_000)`, with
  `origin` the first arrival the log saw. The full reference is `R = floor(q_first / 256)`,
  kept as an unbounded integer. The first delta is `q_first − 256·R`, so it is 0 to 255 after
  any number of revolutions of the 24-bit field, and each later delta is the difference of
  consecutive `q`. Only the serialized reference time is truncated, to `R mod 2^24`.
  Rounding never accumulates.
- **A delta is small** when it is 0 to 255, and large when it fits a signed 16-bit value.
  A delta that fits neither ends the packet before that packet's number. The next `write`
  starts there, with a new reference time.
- **Chunks are greedy.** At each position, with `run` the number of equal symbols that
  start there: a run-length chunk of `min(run, 8191)` when `run >= 14`; else a 1-bit vector
  of the next `min(14, left)` symbols when none of them is large; else a 2-bit vector of the
  next `min(7, left)`. Unused symbol bits are 0.
- The packet ends early when the status count would pass 65,535, or when the next chunk and
  its deltas would make the packet exceed `max` bytes: then it ends at the last of that
  chunk's statuses that fits, and its last chunk is chosen by the same rule over those
  statuses. When `max` cannot hold the header, prefix and one status with its delta, or
  `out`'s spare cannot hold the packet, `write` answers `Capacity` and advances nothing.
- The FCI is zero-padded to a 4-byte boundary, and `P` stays clear, so the packet may sit
  anywhere in a compound. `str0m` pads with RTCP padding and sets `P`
  (`src/rtp/rtcp/twcc.rs:217-226`). The parser accepts both.
- The feedback packet count increases by one per packet written, mod 256.

Law: `write` then `on_feedback` gives back every recorded arrival, quantized to 250 µs, with
no drift across a long run or a full revolution of the reference time. `parse` accepts
exactly the draft's FCI grammar with bounded work, overlapping reports never count a
receipt twice or turn it into a loss, and a refusal for room changes nothing.

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
before the group's first is skipped. Send times are first rounded to the nearest whole
millisecond (half up) from the first send time the estimator saw, and a packet sent before
that origin is skipped, as str0m's `accumulate_packet` does; arrival times stay in
microseconds. A group's send and arrival times are the latest of its packets'. When a group
closes, the estimator takes the send delta and arrival delta between it and the previous
group, and the later group's arrival time is the point's arrival and the detector's clock.

**Trendline.** Each delta pair adds a point (`trendline.rs`): `accumulated += arrival_delta
− send_delta`, `smoothed = 0.9·smoothed + 0.1·accumulated`, and the point is
`(arrival − first arrival, smoothed)`. The window holds the last 20 points, kept in
order of `x`: a point goes after the last one at or before it, and the first is dropped
when a 21st arrives. The detector runs only on a full window. There the trend is the least-squares slope `Σ(x − x̄)(y − ȳ) / Σ(x − x̄)²`, or the previous
trend when the denominator is 0.

**Detector.** With `n` the number of points seen, capped at 1,000, and
`m = min(n, 60) · trend · 4`, against the threshold `γ` (initially 12.5):

- `m > γ`: overuse time grows by the send delta (half of it at the first such point), and
  the count by one. The answer becomes `Overuse` when the time passes 10 ms, the count passes
  1, and the trend rose since the last point.
- `m < −γ`: `Underuse`. Otherwise `Normal`. Both clear the overuse time and count.
- Then `γ` adapts, unless `|m| > γ + 15`: `γ += k · (|m| − γ) · min(Δt, 100)`, with
  `k = 0.039` when `|m| < γ` and `0.0087` otherwise, `Δt` the whole milliseconds
  (truncated) on the detector's clock since its last update (0 at the first update, and
  when the clock went back), and `γ` clamped to `[6, 600]`. A point skipped by the
  `γ + 15` rule still counts as an update for `Δt`.

**Acked rate.** The acked rate is the bytes of packets that arrived in the last 500 ms of
remote time (strictly after the latest arrival minus 500 ms), times 8, divided by the span
from the window's first arrival to its last, with the span at least 150 ms. It exists once
any packet has arrived. Rate control runs once per `on_acked`, after its packets, with the
detector's current answer and the acked rate, and not before an acked rate exists. It replaces libwebrtc's Bayesian estimator, whose extra state buys
smoothing that the 500 ms window already gives.

**Rate control** (`rate_control.rs`). `A` starts at `start_bps` in Increase. The state
moves on each detector answer: `Overuse`
to Decrease from any state; `Underuse` to Hold from any state; `Normal` from Hold to Increase,
from Decrease to Hold, and Increase stays. With `R` the acked rate and `A` the estimate:

- **Increase.** When `A >= 1.5·R + 10_000`, only the time of the last change moves to now,
  as in str0m and libwebrtc. Otherwise, with `t` the seconds since the last change (0 before
  any), capped at 1: near convergence, `A += max(1000, 0.5 · min(t / r, 1)
  · p)`, where `r` is the round-trip time plus 100 ms in seconds and `p` the expected packet
  size in bits: `f = A / 30`, `p = f / ceil(f / 9600)`; the round-trip time is the last one
  `on_acked` was given, 0 before any. Otherwise
  `A += max(1000, (exp(t · ln 1.08) − 1) · A)`, where `ln 1.08` is the `f64`
  `0.0769610411361284`. Then `A = min(A, 1.5·R)`. (str0m writes `min(A, 1.5·R, 1.5·R + 10 kbps)`, which is the same
value.) The cap applies after the increase, so it may lower `A`, as in str0m.
- **Decrease**, at most once per round-trip time clamped to `[10, 200]` ms (100 ms before
  any round-trip time is known; no wait before the first change), unless `R` has fallen
  below `A / 2`: `A = min(A, 0.85 · R)`, and the state becomes Hold. A decrease held back by
  the wait leaves the state at Decrease.
- **Convergence** follows libwebrtc's throughput average, in kbit/s with `α = 0.05`. On a
  decrease (every answer in the Decrease state, also one the wait holds back), an average
  that `R` falls more than 3 deviations below is cleared. Then `avg = (1 − α)·avg + α·R`
  (`avg = R` when none exists), `var = (1 − α)·var + α·(avg − R)² / max(avg, 1)`, with `var`
  clamped to `[0.4, 2.5]` and starting at 0.4. On an increase, an average that `R` exceeds
  by more than 3 deviations is cleared. The deviation is `sqrt(var · avg)`. The estimate is
  near convergence while an average exists.
- `A` stays within `[min_bps, max_bps]`, and every change of `A` moves the time of the last
  change to now.

**Loss.** A second estimate `L`, starting at `start_bps`, follows the classic loss rule of
the GCC draft (§6). With `p` the fraction of packets reported lost since the last update, and
at least 20 reported (counts carry across `on_acked` calls until they reach 20, then
restart): `p > 0.10` gives `L = L · (1 − 0.5·p)`, at most once per 300 ms plus
the round-trip time; `p < 0.02` gives `L = 1.05 · L`, at most once per second; otherwise `L`
holds. The round-trip time is the last given, 0 before any, the waits run on `now_ns`, and
`L` stays within `[min_bps, max_bps]`. A start phase lets the target leave `start_bps`
before loss has spoken: until 2 s after the first `on_acked`, while no packet has yet been
reported lost, the end of each `on_acked` sets `L = max(L, A)` (after the probe measurement
above, so a probe's raise carries into `L`). Without it, `L` rises 5% a second and a
300 kbit/s start needs 25 s to reach 1 Mbit/s whatever the delay estimate sees; libwebrtc's
send-side estimator trusts the delay-based estimate the same way during its first 2 s
(`IsInStartPhase`), there while the last loss fraction is zero. `target()` is `min(A, L)`,
truncated to an integer.

**Probing.** A sender that sends less than it could cannot see more capacity: the increase
is capped at 1.5 times the acked rate. A forwarder that wants a higher layer must raise the
estimate first. `probe(want, now)` answers a cluster to send when `want > target()` and no
cluster is outstanding: `Cluster(id, rate, 5, 15 ms)` with `rate = min(2·target(), want)`
and `id` counting up from 0. The pacer sends at least 5 probes and keeps going until one is
sent at least 15 ms after the first ([RTP-10](#rtp-10-pacer)), so the cluster spans 15 ms
whatever the probe size. The caller hands it to the pacer
([RTP-10](#rtp-10-pacer)), which answers `Probe(id, pad)` per packet. The caller sends each
as a padding-only RTX packet ([RTP-7](#rtp-7-retransmission-history-and-rtx)), assigns that
datagram its own transport number, and tells the estimator with `probe_sent(id, index)`. The
caller asks only for a transport that has a stream with RTX, because a probe on any other
stream would reach a decoder. A cluster is outstanding until feedback covers its packets, or
1 s passes.

When feedback (`on_acked`) has reported at least 80% of the cluster's packets as received,
the rates are measured on the **distinct received members** only, in send order, with
`first` and `last` the earliest and latest of them:

- send rate `(bytes − last size) · 8 / (last send − first send)`;
- receive rate `(bytes − first size) · 8 / (last arrival − first arrival)`;
- `bytes` is the sum of the received members' sizes. A member reported twice counts once.

The receive span runs from the earliest arrival to the latest, and the size taken off is
the earliest-arriving member's, as libwebrtc's `ProbeBitrateEstimator` and str0m's
`src/bwe/probe/estimator.rs` take them; in order the two edges coincide.
A span of zero (fewer than two received members, or all at one time) gives no result. A
result `min(send, receive)` above `A` sets `A` to it, within `[min_bps, max_bps]`, and moves
the time of the last change to now. The result is taken once per `on_acked`, after rate
control.

The cluster's members are the indices `probe_sent` gave for its `id`, in send order. Transport
numbers rise in send order, so an index at or below the last member's, or an `id` other than
the outstanding cluster's, is ignored. The estimator ends the cluster by the pacer's rule, on
the send times feedback reports: its last member is the first that is at least its
`packets`-th and was sent at least `min_ns` after the first, and later members are ignored.
Feedback covers the cluster when every member up to its last has been reported, received or
lost; the cluster is measured then, once, and is no longer outstanding. The 1 s runs from the
`probe` that answered it, checked by `probe` and at the end of `on_acked`. The cluster rate is
`min(2·target(), want)`, with `2·target()` saturating. `Bwe` keeps one member list, sized by
`probe` for the most probes a cluster can hold (`rate · 15 ms / 1,600 + 2`, since a probe is
at least 200 bytes); a `probe_sent` past it is ignored. `bwe` admits no charge
(it answers a `Bwe`, not a `Result`), so this per-cluster reserve is the probe's only growth,
and no per-packet operation allocates. The estimate a probe raises is the delay estimate `A`; the public
`target()` is still `min(A, L)`, so a binding loss limit keeps the target at `L`.

Law: on a link whose queue grows, the detector answers `Overuse` and the target falls; on an
idle link the threshold decays to its floor; on a link with spare capacity a probe raises
`A` to what the link delivered, and the target to `min(A, L)`. The arithmetic is the text
above, to the bit.

## RTP-10: Pacer

```hirsel unbuilt
// module pacer
pub type Class = Audio | Retransmit | Video | Padding
pub type Next[Q] = Send(Q) | Probe(u32, u8) | Pad(u8) | Wait(u64) | Idle
pub opaque type Pacer[Q]

pub fn pacer[Q](slots: u32, factor: f64, queue_limit_ns: u64, header: u32) -> Result[Pacer[Q], Capacity]
pub fn set_rate[Q](mut p: Pacer[Q], target_bps: u64, padding_bps: u64)
pub fn probe[Q](mut p: Pacer[Q], c: Cluster, now_ns: u64)
pub fn push[Q](mut p: Pacer[Q], stream: u32, class: Class, size: u32, now_ns: u64, take q: Q) -> Result[(), Q]
pub fn poll[Q](mut p: Pacer[Q], now_ns: u64) -> Next[Q]
pub fn remove[Q](mut p: Pacer[Q], stream: u32, mut out: List[Q]) -> u64
```

The pacer spreads one transport's packets over time at a rate above the target, so a
keyframe does not leave as one burst that overflows a bottleneck queue (`str0m`
`src/pacer/leaky.rs`). Sizes are RTP bytes before protection, header included.

- The pacing rate is `factor · target` (2.5 by default, libwebrtc's). The pacer keeps a
  **debt** in bytes. Time drains it at the pacing rate, and every send the pacer answers
  adds its size **once**, at the `poll` that answers it, audio included. The debt never
  exceeds 500 ms at the pacing rate.
- `poll(now)` drains the debt for the time since the last poll. Then it answers, in order:
  - `Send(q)` for the oldest audio packet, whatever the debt: audio is small, regular and
    late audio is worse than a burst;
  - `Send(q)` for the oldest queued packet of the highest class (Retransmit, then Video),
    when the debt is 0;
  - `Probe(id, pad)` while a cluster is active, when the debt is 0 and no media packet is
    queued. The cluster's rate stands in for the pacing rate while it runs. The probe size
    is `max(200, rate · 1 ms / 8)` bytes, capped at `header + 255`, where `header` is the
    RTX probe header length the caller gave; `pad = size − header`, 1 to 255. The cluster
    ends with the first probe that is at least its `packets`-th and is answered at least
    `min_ns` after its first;
  - `Pad(pad)` when the queues are empty, a padding rate is set, and the padding debt
    allows it, for the caller to send as one padding-only RTX packet of `header + pad`
    bytes, `pad` 1 to 255;
  - `Wait(at)`, the time the debt reaches 0, when packets are queued **or a cluster is
    active**: a live cluster with empty queues never answers `Idle`;
  - `Idle` otherwise.
- **Queue limit.** When the queued bytes would take longer than `queue_limit_ns` at the
  pacing rate, the rate rises to send them within the limit. A limit of 0 sets no limit.
  As str0m (`src/pacer/leaky.rs:355-388`) and libwebrtc compute it, the rate is
  `max(base, queued · 8 / max(1 ms, queue_limit_ns − q̄))`, with `base` the pacing rate or a
  running cluster's rate, `queued` every queued byte, audio included, and `q̄` the queued
  entries' average time since their `push`.
- **Rate in force.** `poll` takes the rate from the state at its start and uses it both for
  the drain since the last poll and for the answer's `Wait`. A poll earlier than the last
  drains nothing. Debts are `f64` bytes; `Wait` is rounded up to the nanosecond, and a debt
  whose `Wait` time has come is 0, so a `poll` at the time a `Wait` named always finds it
  paid. A rate of 0 holds no debt (its 500 ms cap is 0), so nothing waits.
- **Sizes.** The probe size `rate / 8000` is truncated to whole bytes and is at least
  `header + 1`, so `pad` is 1 to 255 for any header. A `Pad` takes the same size from the
  padding rate.
- **Padding.** A queued `Padding` packet is the lowest class, after Video; any queued packet
  goes before a probe or a `Pad`. The padding debt drains at the padding rate, every answered
  send adds to it as to the debt, and it holds at most 500 ms at the padding rate. `Pad` needs
  both debts at 0. With a padding rate set and nothing else to answer, `poll` answers `Wait`
  at the later time both debts reach 0, not `Idle`, so the caller knows when to poll.
- **Clusters.** `probe` replaces a running cluster. The cluster's first time is the `poll`
  that answers its first probe.
- `pacer` refuses 0 slots, and `2^32 − 1` slots, with `Capacity`.
- The caller logs each probe's send time ([RTP-8](#rtp-8-transport-wide-sequence-numbers-and-feedback))
  as the `now` of the `poll` that answered it, so the estimator ends the cluster where the
  pacer did.
- `push` with no free slot gives back `q` (`Error(q)`), which the caller drops. Nothing is
  dropped silently.
- `remove(stream)` moves every queued entry of the stream to `out` and answers the count.

Law: a packet of size `s` leaves no sooner than the debt allows, so over any interval the
bytes sent (excluding audio) are at most the pacing rate times the interval plus one
packet. Audio is never held behind video, and is charged once like any send. Every probe and
padding packet is representable. A full pacer refuses explicitly.

## RTP-11: Stream rewriting and layer switching

```hirsel unbuilt
// module rewrite
pub type Unit = Unit(start: bool, end: bool, keyframe: bool, needs: u32, switches: u32, frame: u16)
pub type Target = Off | Layer(source: u32, target: u8)
pub type TargetError = Unrepresentable()
pub type Out = Forward(u64, u32) | Late(u64, u32) | Drop | Wait
pub opaque type Rewrite

pub fn rewrite(clock: u32, ranges: u32, frames: u32, first_seq: u64, first_ts: u32) -> Result[Rewrite, Capacity]
pub fn target(mut r: Rewrite, t: Target) -> Result[(), TargetError]
pub fn on_packet(mut r: Rewrite, source: u32, index: u64, ts: i64, at_ns: u64, padding_only: bool, u: Unit) -> Out
pub fn on_sr(mut r: Rewrite, source: u32, info: SenderInfo, at_ns: u64)
pub fn sr_timestamp(mut r: Rewrite, now_ns: u64) -> Option[u32]
pub fn switch_index(r: Rewrite) -> Option[u64]
pub fn pending(r: Rewrite) -> Option[u32]
pub fn close(mut r: Rewrite)

// module counter
pub type Mode = Compact | Translate
pub type CounterError = Absent | Width | Full
pub opaque type Counter

pub fn counter(width: u8, mode: Mode, slots: u32) -> Result[Counter, Capacity]
pub fn restart(mut c: Counter, first: u64)
pub fn forward(mut c: Counter, key: i64, value: Option[u64], width: u8) -> Result[u64, CounterError]
pub fn omit(mut c: Counter, key: i64)
pub fn lookup(c: Counter, key: i64) -> Option[u64]
```

A forwarder sends one publication to many subscribers, and each subscriber may receive a
different layer. A `Rewrite` turns the source packets one subscriber receives into one
continuous outgoing stream. The receiver sees sequence numbers with no gaps except real
losses, timestamps that advance with the media, and a layer change only where its decoder
can follow it. A rewrite answers numbers; the caller writes the header
([RTP-1](#rtp-1-the-fixed-header)) and protects the packet.

**What is encrypted.** On every egress packet, SRTP authenticates the RTP header and
encrypts everything after it. A rewritten codec descriptor is payload, so it must be
encrypted. For RTX packets, use the in-place protection path in
[RTP-7](#rtp-7-retransmission-history-and-rtx). When an original media packet's payload is the
shared plaintext unchanged (audio, H.264, a frame whose descriptor needs no rewrite), the
caller writes only the header into its destination and calls TLS-8 `seal_into` with the
shared payload as source. When the descriptor is rewritten, the caller writes it and the
rest of the payload after the header and calls `seal` in place from the header's end. This
copies the payload once. The AV1 dependency descriptor is a header extension, so its rewrite
stays in the clear header.

**Sources and units.** A source is an incoming stream, named by the caller's 32-bit key
(for simulcast, one per layer SSRC). Every packet arrives with its authenticated index,
its signed extended timestamp ([RTP-2](#rtp-2-extended-sequence-numbers)) and a `Unit`:

- `start` and `end` mark the first and last packet of a **decodable frame**;
- `keyframe` marks a frame that needs no earlier frame;
- `needs` has bit `i` set when target `i` needs this frame. Targets are 0 to 31, so the
  mask has 32 bits, as many as an AV1 dependency descriptor's decode targets;
- `switches` has bit `i` set when a decoder may begin to follow target `i` at this frame,
  because every frame it references for that target has been forwarded or is in the frame;
- `frame` tells apart the frames that share one RTP timestamp. VP9 and AV1 send every
  spatial layer of a picture as its own frame with the picture's timestamp, and each layer
  frame has its own `needs`, so one decision per timestamp would forward or drop all of
  them together. `frame` is the VP9 spatial id (`SID`), the AV1 dependency descriptor's
  frame number, and 0 for VP8, H.264 and audio, which send one frame per timestamp.

**Where `Unit` lives.** `Unit` is `rtp.rewrite.Unit`, the type `on_packet` takes. `rtp`
depends on `std` alone, and `media_bits` has consumers that are not RTP (`image_core`,
`image_pipeline`, `media_container`), so neither package depends on the other. The adapter
answers its own `media_bits.unit.Unit` with the same five facts, and the caller, which
depends on both, builds `rtp.rewrite.Unit` from it and adds `frame`. Moving the type to a
package both import would change no decision.

`rtp` knows no codec. The **unit adapter** in `media_bits` (slice S9) fills `Unit` from the
payload and owns every codec fact, as the RFCs define them:

- VP8 ([RFC 7741 §4.2](https://www.rfc-editor.org/rfc/rfc7741#section-4.2)): `start` is `S = 1`
  with `PID = 0`, checked against the first partition's header and body as libwebrtc does
  (`video_rtp_depacketizer_vp8.cc:175-191` at `3710345`) before the frame type is read;
  `keyframe` is the payload header's `P = 0`; `switches` for an upper temporal layer comes
  from `Y`.
- VP9 ([RFC 9628 §4.2](https://www.rfc-editor.org/rfc/rfc9628#section-4.2)): `start` is `B`;
  `switches` comes from `U` and `D` and is set only when every spatial and temporal layer
  the frame references was forwarded.
- H.264 ([RFC 6184 §§5.1, 5.7–5.8](https://www.rfc-editor.org/rfc/rfc6184#section-5.7)):
  `start` is the first packet of an access unit, never a FU start that only starts a NAL
  unit; `keyframe` is an IDR access unit with usable SPS and PPS, sent with it or before it
  on this leg. A packet's payload cannot show whether it begins an access unit, so the
  caller passes the adapter two facts it keeps per source and leg:
  - `after_end`: the packet at `index − 1` of the same source was seen and ended an access
    unit (its marker was set, or it had another timestamp). When that packet was lost or has
    not arrived, `after_end` is false, and the packet is not a `start`. A frame's decision
    never reads `start`, so this only delays a switch to the next access unit whose
    predecessor arrived; it never changes which packets of a frame go out together.
  - the parameter sets: the SPS and PPS **of that source** that were sent on this leg, and
    those the packet carries. A PPS or SPS id names different sets in different sources, so
    a pending source's IDR is judged against that source's own sets, never the current
    source's. The caller keeps one set per source it presents and clears the leg's sets of
    the old source at a switch, so an IDR from B is a keyframe only when B's SPS and PPS
    reach the receiver with it or before it.
- AV1 ([AV1 RTP Appendix A](https://aomediacodec.github.io/av1-rtp-spec/#appendix-a-dependency-descriptor-rtp-header-extension)):
  `needs` from each decode target's indication (a target needs a frame unless its indication
  is Not Present, §A.8.3); `switches` from a Switch indication, and only when the chains say
  the frame's dependencies were forwarded. A decode target with no protecting chain
  (a structure without chains, §A.5) has no way to show that, so its `switches` bit is set
  only at a keyframe.

**Target numbers.** The target numbering, and so which bits of `needs` and `switches` a
frame sets, is the forwarding spec's stable map ([RTC-6](webrtc.md#rtc-6-layers-dynacast-and-active-speakers)):
for VP8 and H.264, target `i` is temporal layer `i` and needs every frame of temporal layer
at most `i`; for VP9, `i = 4 · S + T`; for AV1, the decode target index. `rtp` reads bits
and never orders targets, because VP9's and AV1's numbers are not ordered by quality.
**A down-switch is a switch point like any other**: it lives in the masks. At any frame
start, a decoder that follows target `i` already has every frame a lower target `j`
references, so the adapter sets bit `j` of `switches` for every `j` that the down-track's
current target contains (for VP8 and H.264, every `j ≤ i` at any frame start, as VP9 does
for its down-switches). An adapter that sets fewer bits keeps forwarding correct and only
delays the down-switch to the next frame that sets them.

**Targets.** `target(Layer(source, i))` asks to forward target `i` of `source`, and answers
`Unrepresentable` for `i > 31`. `target(Off)` stops forwarding. The current layer changes
only at a switch point:

- within one source, from target `i` to `j` at a packet with `start` and bit `j` of
  `switches`, whose frame has no decision yet and that is newer than every packet already
  decided (a frame some of whose packets were already decided under `i` keeps that
  decision, so it is forwarded whole or not at all);
- to another source, or from `Off`, only at a packet of that source with `start`,
  `keyframe` and bit `j` of `switches`.

Until then the old layer keeps forwarding. While a target on another source is pending, the
caller asks for that source's keyframe when it sets the target, and again at each `Keyframes`
interval until the switch happens ([RTP-6](#rtp-6-loss-tracking-nack-and-keyframe-requests),
`Switch`). `pending(r)` answers the pending source, if any.

**Generations.** Each time a source becomes current it starts a new **generation**, which
owns its own ring of `ranges` entries `(from index, offset)` and its own timestamp offset.
Only the current generation's packets ever change an offset. A pending source has no
generation, so its dropped packets close up nothing.

**Deciding a packet.** `on_packet` takes these steps in order:

1. After `close`, `Drop`.
2. If the packet is a switch point for the pending target (above), the switch happens now:
   a new generation starts at the next outgoing index, and the packet is forwarded. This is
   checked before anything answers `Wait` or `Drop`, so the first eligible frame is never
   lost, including from `Off`.
3. With no current layer, `Wait`, and the caller asks for a keyframe.
4. A packet of any source but the current one, pending or old, answers `Drop` and changes
   nothing.
5. A packet of the current source is decided **per frame**. The frame's decision is made at
   its first packet seen (forward when the current target's bit is set in `needs`), kept in a
   ring of `frames` entries keyed by generation, timestamp and `Unit.frame`, and applied to
   every packet of the frame, in any order. A frame whose decision has left the ring answers
   `Drop`: a packet with no entry whose index is at or below the highest index an evicted
   entry saw is treated as such a frame, since a frame's packets are consecutive indices.

**Sequence numbers.** A forwarded packet's outgoing index is `index − offset`.

- A packet of the current generation that is not forwarded, or that is padding only, is
  **dropped and closed up** once: every later packet's offset grows by one, so the receiver
  never asks for it. Each change of offset takes one entry of the generation's ring.
- A packet below the highest forwarded index takes the offset of its range. A packet older
  than the oldest range answers `Drop`. The ring is the caller's explicit bound.
- A real upstream loss leaves a gap in the outgoing indices. The receiver asks for it, and
  the egress history maps it back.

**Late packets.** An SRTP sealer refuses any index not above every index it has sealed
(TLS-8, Sender ownership). A source packet that arrives after a later one was forwarded has
an outgoing index below the highest sealed, so it cannot go on the media stream. The
rewrite answers `Late(index, ts)`. When the subscriber negotiated RTX, the caller sends the
packet as an RTX repair with that index as its OSN ([RTP-7](#rtp-7-retransmission-history-and-rtx)),
which the receiver accepts as a retransmission. Without RTX, the packet is dropped. This is
the cost of a sealer that never reuses a nonce: a stream without RTX, commonly audio, loses
every packet that the network reordered past its successor. A sealer with a window of
unsealed indices would remove that cost, and would belong to TLS-8. The decision is logged
with its reversibility in `followups.md`.

**Timestamps.** Each generation keeps its source-to-output offset, so within a generation
the outgoing timestamp is `ts − offset` on the extended timeline, and the wire value is that
mod `2^32`. Every timestamp difference of the source is kept, including equal timestamps
within a frame. At a switch from source A to source B, at the accepted frame start:

- The first new outgoing index is the last outgoing index plus 1.
- Each SR's wire timestamp is extended on its own source's timeline (`estimate32`, against
  that source's latest timestamp presented).
- `on_sr` keeps the latest SR of the current source and of the pending source, with its
  arrival time. An SR of any other source is ignored, and a source's SR is forgotten when it
  stops being current or pending. **Usable** means kept, and both SRs come from one sender
  clock (the publisher's), which the caller vouches for by presenting them on one rewrite,
  and the projection below fits the signed 64-bit timeline (an NTP difference of more than
  2^31 seconds does not).
- **With usable SRs** from both sources, project B's capture time into A's timeline. Both
  sources run at the one negotiated `clock`, so the RTP part carries over exactly and only the
  NTP difference is scaled: `t_A = rtp_A + (ts − rtp_B) + floor(((ntp_B − ntp_A) · clock) / 2^32)`,
  with `ntp_B − ntp_A` the signed 64-bit difference of the 32.32 NTP values and the product
  floored exactly (128 bits). This is the capture time `ntp_B + (ts − rtp_B) / clock` of the
  switch packet, read on A's clock; scaling `ts − rtp_B` through NTP units and back would
  lose a tick to rounding (900 ticks at 90 kHz would come back as 899). The anchor is
  `t_A − offset_A`: the projected value **minus A's current source-to-output offset**, so it
  is on the output timeline.
- **Otherwise**, advance the last outgoing timestamp by the elapsed arrival time at the
  negotiated clock rate: `last_out + ((at_ns − last_at_ns) · clock) / 10^9`.
- Clamp the anchor to at least `last_out + 1`, so the output never goes back, set B's
  offset to `ts − anchor`, and keep B's differences from then on.

So at 90 kHz, an old source value 90,000 mapped to 9,000, followed 10 ms later by B's first
frame, gives 9,900 in either case (mediasoup adds the saved offset the same way,
`SimulcastProducerStreamManager.cpp:533` at `90ca2ae`).

**Switch record.** At every source switch the rewrite records the first outgoing index of
the new generation. `switch_index` answers it, and the caller passes it to `History.gate`
([RTP-7](#rtp-7-retransmission-history-and-rtx)).

**Sender reports.** `sr_timestamp(now_ns)` answers the outgoing RTP timestamp for an SR the
caller sends at `now_ns` under this stream's outgoing SSRC, with its own counts
([RTP-5](#rtp-5-reception-statistics-reports-and-round-trip-time)). A call that answers a
timestamp is the report, so it takes the rewrite `mut`:

- `None` when nothing was forwarded since the last report: the caller sends an RR
  ([RFC 3550 §6.4](https://www.rfc-editor.org/rfc/rfc3550#section-6.4)).
- When the current source has a usable SR, its capture mapping moved to `now_ns` by the
  SR's arrival: `rtp + ((now_ns − sr_at_ns) · clock) / 10^9 − offset`, mod `2^32`.
- Otherwise, for an active sender without an upstream SR, extrapolate from the local anchor
  of the last forwarded packet, its outgoing timestamp and send time:
  `last_out + ((now_ns − last_sent_ns) · clock) / 10^9`, mod `2^32`. The send time is the
  `at_ns` the packet was presented with, since the rewrite sees no later time.
- Both formulas floor signed values exactly, so a report time before the anchor's goes back
  by the elapsed ticks.

**Codec counters.** Payloads carry their own counters: the VP8 picture ID (absent, 7 or 15
bits) and TL0PICIDX (8 bits), the VP9 picture ID (absent, 7 or 15 bits) and the dependency
descriptor's frame number (16 bits). A `Counter` is a stateful, bounded mapping per output
stream from pictures to output values of `width` bits, which `media_bits` writes into the
caller's per-subscriber copy of the payload descriptor with its `rewrite_*` operations. The
output width is fixed when the counter is made; `media_bits` writes the field at that width
whatever the source's width was.

- A picture is named by the caller's `key` (for a picture ID, the frame's extended
  timestamp; for TL0PICIDX, the extended input TL0 value). A ring of `slots` entries keeps each
  key's decision, so every fragment of a picture, in any order, gets the same answer, and an
  entry that has left the ring is `Full`.
- `forward(key, value, width)` emits a picture. `Compact` mode (the VP8 picture ID, and
  TL0PICIDX fed only base pictures) answers the last output plus 1 mod `2^width`: omitted
  pictures leave no gap, and an absent input field is synthesized. `Translate` mode (VP9
  picture IDs, AV1 frame numbers) answers `value − offset` mod `2^width`, with `value`
  extended at its input `width`: a constant translation that keeps every reference
  difference exact. In `Translate`, an absent field is `Absent`, and an input width other
  than the output width is `Width`, because a reference difference cannot be carried across
  widths. A refused picture is dropped, and the caller asks for a keyframe.
- `omit(key)` records an intentional omission, which `Compact` closes up and `Translate`
  leaves as a gap.
- `lookup(key)` answers an emitted picture's output. A dependent enhancement layer frame
  carries its base picture's mapped TL0PICIDX, from `lookup` of the base's input TL0, so TL0
  advances once per base picture.
- `restart(first)` at a source switch clears the ring, because the old source's keys mean
  nothing for the new one, sets the next output to the last output plus 1 (or `first` when
  nothing was emitted yet), and for `Translate` sets the offset from the new source's first
  value. The egress history keeps each sent packet's own codec rewrite (RTP-7), so clearing
  the ring loses nothing a repair needs.

Law: the outgoing indices of one stream increase by exactly one per forwarded packet,
except across real upstream losses. A dropped or padding-only packet of the current
generation leaves no gap, and no other source's packet changes an offset. A layer change
happens only at a switch point, and the first eligible frame is never refused. The outgoing
timestamp never goes back and follows the clock rules above to the tick. A late packet is
never sealed on the media stream. Every codec counter stays continuous, and every reference
points at the picture the receiver was sent.

## RTP-12: State, charges and close

Every piece of state is sized when it is made: a history's slots, a log's ring, a pacer's
slots, a loss window, a rewrite's ranges and frames, a counter's slots. Each size must be
positive, and a size of 0 is refused with `Capacity`, because a ring of no slots has no
`mod`. Each constructor admits its complete charge from the current limit
([MEM-24](budgets.md#mem-24-logical-charges)) or answers `Capacity`, and the drop refunds it.
A charge is computed with checked arithmetic and rounded up to whole bytes: a loss window
charges `ceil(window / 8)` bytes of bitmap plus 17 bytes per tracked index (two times and a
tries count), so a nine-index window charges 2 bytes of bitmap, not 1. No per-packet
operation allocates, grows a list or admits a charge.

**Output lists.** The caller's output lists (`pairs`, `out`) are the caller's, sized by the
caller, and their growth is the only charge on that path. Every operation that appends to
one takes an explicit bound (`max`) and checks the room before changing any state. When the
output is too short it either refuses with nothing changed (`on_feedback`, `write`) or
writes what fits and keeps the rest pending (`due`'s `More`), so no retry state is lost.
A bounded input cannot make unbounded output: a TWCC report with 65,535 statuses is refused
for room before one `Acked` is appended.

**Close.** A forwarder that stops a subscriber, or revokes a participant, needs nothing
already decided to leave afterwards. The forwarding spec runs the fence. `rtp` gives it these
operations, which never wait:

- `Rewrite.close()`: every later `on_packet` answers `Drop`.
- `History.clear()`: no NACK can be answered from it.
- `Pacer.remove(stream)`: every queued entry for the stream is handed back.
- dropping a `Losses`, `Keyframes`, `SendLog`, `RecvLog`, `Counter` or `Bwe` refunds its
  charge.

After these return, no packet of that stream can come out of `rtp` again, and every other
stream is untouched.

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
| LiveKit `pkg/sfu/rtpmunger.go:135-274` | range map of sequence offsets, drop close-up, NACK gate after a keyframe switch | ADAPTED | the same model, with a bounded ring of ranges per source generation and a `Late` answer, because a TLS-8 sealer cannot seal a reordered index |
| LiveKit `pkg/sfu/codecmunger/vp8.go:130-145,238-290` | VP8 picture ID and TL0PICIDX munging that closes up dropped pictures | ADAPTED | `Counter`'s `Compact` mode, keyed per picture so every fragment maps alike; `Translate` for codecs whose references are picture differences |
| mediasoup `RtpStreamSend.cpp:575-599`, `SimulcastProducerStreamManager.cpp:533` at `90ca2ae` | restore each consumer's fields before RTX; add the saved offset at a simulcast switch | ADOPTED | the per-leg send contract (RTP-7) and the switch anchor minus A's offset (RTP-11) |
| LiveKit `pkg/sfu/forwarder.go` | codec-aware layer selection | REPLACED | `rtp` takes `needs` and `switches` masks and knows no codec; `media_bits` computes them |
| str0m `src/rtp/rtcp/remb.rs`, `xr.rs` | REMB and XR | NOT TAKEN | TWCC replaces REMB, and no Wave 1 consumer reads XR |

## Build laws

The build lane writes each law **red first**: the case is added to its harness before the
code it tests, and it fails there. Each law names its oracle, which comes from outside the
code under test. The harness is the package's law suite, `core/programs/rtp/laws.hirsel` <!-- doc-ref: planned -->
(`[tests.sources]` in `core/programs/rtp/hirsel.toml` <!-- doc-ref: planned -->), unless a law names another.
Where a law gives an expected value that is not quoted from a source, its arithmetic is
written here and repeated in the law's comment.

- **`header_chrome_vector` (RTP-1, RTP-3).** *Input:* the 48 bytes of `hb1` in str0m's
  `src/rtp/header.rs:545-549`, with a `Mixed` map `1 AudioLevel, 2 AbsSendTime,
  3 TransportSeq, 4 Mid` (`ext.rs:373-386`) and `vad` on. *Oracle:* str0m's expectation
  (`header.rs:567-588`), checked by hand. `seq` 47000, `ts` 10000, `ssrc` 777459193, `P` and
  `X` set, `end` 32, MID `xYj`, audio level `(Some(true), 42)`, abs-send-time `0xFE206A`
  (63.531654 s), transport sequence 0. After `header.payload` the payload is `[1, 2, 3, 4]`,
  from offset 32 with 12 bytes of padding.
- **`padding_counts` (RTP-1).** A 13-byte packet with `P` set and last byte 0 is `Padding`.
  Last byte 2 is `Padding` (only 1 byte follows the header). Last byte 1 gives an empty
  payload. A 12-byte packet with `P` set is `Padding` without reading a count. *Oracle:* RFC
  3550 §5.1.
- **`bounded_views` (RTP-1, RTP-3, RTP-4).** Every accessor on hostile or mismatched input
  answers a bounded error and changes nothing. *Inputs, by hand:* every prefix of `hb1` from
  0 to 47 bytes through `parse`, never a read past the prefix: `Short` below 12 bytes;
  `Extension` from 12 to 31 bytes, because `hb1` has `X` set and no CSRCs, so its block runs
  to byte 32; and from 32 to 47 bytes `Ok` with `end` 32 and `len` the prefix's length,
  because the whole header is present and `parse` never reads padding (`payload` checks it,
  as `padding_counts` shows); a header with `CC = 2` read at `csrc` 1 (a value) and 2 (`Index`); the
  `Header` of `hb1` used with a 47-byte buffer (`Buffer`); a two-byte block whose last byte
  is a lone nonzero id (`Overrun`); a one-byte block where the `Mid` element is followed by an
  element that overruns (`find` answers `Overrun`, not the match); an RR with `RC = 1` read at
  `block` 1 (`Index`); `is_rtcp` on 0 to 3 bytes (false); a 3-byte RTCP datagram (`Length`
  before `Version`); `rtx.unwrap` of `hb1`'s `Header` with a 47-byte buffer (`Buffer`). The
  RTP-1 and RTP-3 cases land in S1, the RTP-4 cases in S2, and the RTX case in S4. After each refusal the cursor and the state passed in are unchanged.
  *Oracle:* RFC 3550 §§5.1, 6.4 and RFC 8285 §§4.2–4.3, by hand.
- **`extension_forms` (RTP-3).** A hand-built two-byte block (`0x1000`): a padding byte, an
  element of id 7 with length 0, an element of id 20 with a 17-byte RID, then an element whose
  length runs one byte past the block. `next` answers the first two, the RID's range, and then
  `Overrun`. A one-byte block with id 15 before a valid element answers `None` at id 15, and
  so does one whose byte is `0x03` (id 0, `L = 3`). After `None`, `next` answers `None` again.
  A `OneOnly` map refuses `bind` of id 16 (`BadId`); a `Mixed` map accepts it. `token` of MID
  `audio-1` is accepted; of RID `a_b` is `Value(Rid)`. `audio_level` of `0xAA` is
  `(Some(true), 42)` with `vad` on and `(None, 42)` with `vad` off; a 2-byte audio-level
  element is `Value(AudioLevel)`. *Oracle:* RFC 8285 §§4.2, 4.3 and 6, RFC 8852 §3, RFC 9143
  §15, RFC 6464 §§3–4, by hand.
- **`egress_form` (RTP-3).** A `Mixed` subscriber map with audio level on id 1, transport
  sequence on id 2 and RID on 3: writing audio level `(true, 42)` and transport sequence
  0x1234 gives the one-byte block `BE DE 00 02 | 10 AA 21 12 34 00 00 00`. Adding a 17-byte
  RID makes the whole header two-byte: `10 00 00 07 | 01 01 AA 02 02 12 34 03 11` then the
  17 RID bytes and 2 zero bytes (3 + 4 + 19 = 26 bytes of elements, padded to 28, length 7). The same two
  short values under `TwoOnly` give `10 00 00 02 | 01 01 AA 02 02 12 34 00`. Under `OneOnly`,
  the 17-byte RID is refused with `Form` and nothing is appended. With `vad` off, the level
  octet is `0x2A`. *Oracle:* RFC 8285 §§4.2, 4.3 and RFC 6464 §3, by hand.
- **`index_estimate` (RTP-2).** *Oracle:* RFC 3711 Appendix A, by hand.
  `estimate(65535, 0) = Index(65536)`; `estimate(65536, 65535) = Index(65535)`;
  `estimate(0, 65535) = Before`. Ties at half the range keep ROC: with highest
  `65636` (`s_l = 100`), wire `32868` (difference exactly 32768) gives `Index(98404)` and its
  neighbour `32869` gives `Index(32869)`; with highest `105536` (`s_l = 40000`), wire `7232`
  (`s_l − 2^15 = 7232`, not greater) gives `Index(72768)` and its neighbour `7231` gives
  `Index(138303)`. Highest `65536` with wire `40000` gives `Index(40000)`, even if the sender's
  true index was 105536: the estimate is the nearest cycle, and the packet would fail
  authentication. Highest `2^48 − 1` with wire 0 gives `Beyond`. The exhaustion of a real
  sender is the DTLS law `sender_index_monotone`.
- **`nack_pairs` (RTP-4, RTP-6).** The pair `(65436, 0b1000_0000_0000_1001)` read against a
  highest index of 196,618 expands to 196,508, 196,509, 196,512 and 196,524. *Oracle:* str0m
  `src/rtp/rtcp/nack.rs:134-155`. A `Losses` with those four due packs them back into that
  one pair. *Oracle:* RFC 4585 §6.2.1. A `Losses` with `window` 9 is charged 2 bytes of
  bitmap (`ceil(9 / 8)`). With `window` 512 and 18 isolated indices due (each 17 apart, so
  each needs its own pair) and `max` 16, `due` appends 16 pairs and answers `More`; the next `due` at the
  same time appends the other 2 at their first try. `window` 0 is `Capacity`.
- **`rtcp_compound_rules` (RTP-4).** Hand-built datagrams through `check`, each pinned as
  full bytes in the law. *Accepted:* SR + SDES(CNAME of the SR's SSRC) with `P` set on the
  SDES and a count of 4; with `Reduced`, the lone PLI `81 CE 00 02 00 00 00 01 00 00 00 02`
  (sender 1, media 2), the NACK `81 CD 00 03 00 00 00 01 00 00 00 02 FF 9C 80 09` and the FIR
  `84 CE 00 04 00 00 00 01 00 00 00 00 00 00 00 02 07 00 00 00` (target 2, sequence 7,
  reserved 0). *Refused:* `P` on the SR (`Padding`); a lone RR, and an RR with an SDES whose
  CNAME belongs to another SSRC (`NoCname`); the lone PLI without `Reduced` (`NotCompound`); a
  length word one past the datagram (`Length`); an SDES with `SC = 2` whose second chunk is
  cut (`Body(202)`); a BYE whose reason length runs past its body (`Body(203)`); a PLI with 4
  bytes of FCI, a NACK with an empty FCI and a FIR with 12 bytes of FCI (`Body(205)` for the NACK);
  a NACK whose 6 bytes of FCI are padded to a word with a count of 2 (`Padding`, which `check`
  answers before any body check, because a word-bodied count is a multiple of 4); a valid SR
  + SDES followed by a final RR whose padding count is 3 (`Padding`). `is_rtcp` holds for
  second bytes 192 and 223 and fails for 191 and 224. `write_rr` of 32 blocks writes an RR
  of 31 and an RR of 1, each parsing back. `write_cname` of 256 bytes is `Unrepresentable`
  with any spare, and a spare one byte short of a valid CNAME packet is `Capacity`; neither
  appends.
  `write_fir`, `write_pli` and `write_nack` give exactly the bytes above. *Oracle:* RFC 3550
  §6.1, §6.4 and A.2, RFC 4585 §§6.2.1, 6.3.1, RFC 5104 §4.3.1.1, RFC 5506 §3, RFC 5761 §4.
- **`rtt_example` (RTP-5).** A block with `lsr = 0xB7052000` and `dlsr = 0x00054000`, read at
  an NTP time whose middle bits are `0xB7108000`, gives 6.125 s. *Oracle:* the worked example
  of RFC 3550 §6.4.1. Across the compact wrap, `A = 0x00010000`, `lsr = 0xFFFF0000`,
  `dlsr = 0x00008000` give `(0x00010000 − 0xFFFF0000 − 0x00008000) mod 2^32 = 0x00018000`,
  98,304 / 65,536 = 1.5 s. `lsr = 0` gives `None`; `A = lsr`, `dlsr = 1` gives `None`
  (`r = 2^32 − 1`).
- **`reception_report` (RTP-5).** Clock 90,000. Indices 65,530 + `i` for `i` 0 to 9, with
  `i` = 3 and 7 missing (the 16-bit sequence wraps at `i = 6`), each with timestamp `3000·i`,
  arriving at `i · 33.4 ms`. The first report has `lost = 2`, fraction `(2 << 8) / 10 = 51`,
  and jitter 2: A.8's recurrence over the eight packets leaves `J = 45`, and `45 >> 4 = 2`,
  worked in the law's comment. *Oracle:* RFC 3550 A.3 and A.8. A second interval: `i` 10 to 19
  all arrive, and the missing `i = 3` arrives late among them. Then `expected_interval = 10`,
  received in the interval 11, `lost_interval = −1`, so the fraction is 0; cumulative
  `lost = 20 − 19 = 1`; `highest = 65,549 = 0x0001000D` (cycle 1, sequence 13). Then index
  70,549 (5,000 above the highest) is `Held`, and 70,550 is `Restarted`: the next report has
  `expected = 2`, `lost = 0` and `highest = 70,550`. An RTX
  repair of `i = 7` counts in the RTX SSRC's `Received`, not here. *Oracle:* RFC 3550 A.3 and
  §6.4.1, by hand.
- **`keyframe_coalesce` (RTP-6).** Interval 200 ms, `fir` set. `want(Switch)` at 0, 10 and
  50 ms answers `Fir(0)`, `None`, `None`. At 210 ms it answers `Fir(0)` again (repeat, same
  number). `set_interval` to 100 ms at 250 ms: `want(Switch)` at 300 ms answers `None`, and at
  310 ms answers `Fir(0)` (a repeat, 100 ms after 210). After `keyframe_seen`,
  `want(Switch)` at 320 ms answers `Fir(1)`. After
  `keyframe_seen` again, `want(Loss)` at 500 ms answers `Pli`, and `want(Switch)` at 510 ms answers
  `None` (covered). With `fir` clear, `want(Switch)` answers `Pli`. *Oracle:* RFC 5104
  §§4.3.1.1–4.3.1.2, by hand.
- **`history_lookup` (RTP-7).** Four slots. Put indices 1 to 6. `get(2)` is `Missing`
  (overwritten), `get(5)` is `Found(5)`, and a second `get(5)` inside `min_resend` is
  `Throttled`. Past `max_age` it is `Missing`. After `gate(6)`, `get(5)` is `BeforeSwitch`.
  `clear` answers 4, and `get(6)` is then `Missing`. Put 65,537 into a fresh four-slot
  history after 1 (same slot, same low 16 bits): `get(1)` is `Missing`. `history(0, ..)` and
  `history(2^15 + 1, ..)` are `Capacity`. *Oracle:* RTP-7, by hand.
- **`rtx_layout` (RTP-7).** The plaintext layout, before protection. `rtx.write` with RTX
  payload type 97, RTX SSRC 0x0000BEEF, RTX sequence 5, timestamp 3000, marker set, OSN
  0x1234 and payload `AA BB CC` gives `80 E1 00 05 00 00 0B B8 00 00 BE EF 12 34 AA BB CC` and
  answers header length 12, the `at` from which protection starts. `unwrap` of an RTX packet
  whose payload is all padding is `Probe`, and of a 1-byte payload is an error. **Two legs:**
  one source packet forwarded to leg 1 (PT 96, MID `0`, transport-seq id 3, RTX PT 97) and leg
  2 (PT 100, MID `v-1`, transport-seq id 5, RTX PT 101), each with its own rewritten VP8
  picture ID. A NACK on each leg yields that leg's RTX PT and SSRC, its next RTX sequence, its
  own output index as OSN, and its own rewritten descriptor in the payload; the media send and
  the repair on a leg carry two distinct transport numbers. After a source switch, a NACK for
  an index before the switch yields nothing (`BeforeSwitch`). *Oracle:* RFC 4588 §4 and RFC
  8852 §3, by hand.
- **`rtx_protected` (RTP-7, with SRTP).** *Harness:* `core/programs/srtp/laws.hirsel`.
  For each SRTP profile TLS-8 names (`Aes128CmHmacSha1_80`, `AeadAes128Gcm`,
  `AeadAes256Gcm`), the law uses a fixed master key and salt and calls `srtp.protect_rtp` on
  the `rtx_layout` packet at index 5. Protection starts at `at = 12`. *Oracle:* an independent
  opener in `core/programs/srtp/oracle.py` uses Python `cryptography` (RFC 3711 §4 AES-CM
  with HMAC-SHA1-80 and RFC 7714 AES-GCM with the 12-byte header as AAD). The law compares
  the protected packet with the expected wire bytes in `core/programs/srtp/vectors.hirsel`.
  The independent opener recovers OSN 0x1234 and `AA BB CC`, and the wire bytes at
  offsets 12 to 16 differ from `12 34 AA BB CC`. Negative control, the same packet sealed
  with the OSN left in the authenticated header (protection from offset 14): under each GCM
  profile the independent opener's tag check fails; under AES-CM the tag passes, because the
  HMAC covers the same bytes, but the opener's decryption from offset 12 yields an OSN other
  than 0x1234.
- **`twcc_report_bytes` (RTP-8).** Arrivals of transport numbers 10, 11, 12 and 13 at 0, 12,
  23 and 43 ms (the arrivals of str0m's `twcc.rs:1529-1549`). `write` with sender 1 and media
  2 gives the FCI `00 0A 00 04 00 00 00 00 | BC 00 | 00 30 2C 50 | 00 00` and an RTCP length
  of 6. *Oracle:* the draft's layout, by hand: one 1-bit vector chunk of four received
  symbols, and deltas of 0, 48, 44 and 80 quarter-milliseconds.
- **`twcc_chunks` (RTP-8).** *Oracle:* the draft's §3.1, by hand.
  - 221 packets received with small deltas encode as the one run-length chunk `0x20DD`.
  - Two packets, the second arriving 1 ms *before* the first (delta −4), give status count 2,
    the 2-bit vector chunk `0xD800` (symbols `01`, `10`, then five clipped `00`) and the deltas
    `04 FF FC` (the log's origin is the earlier arrival, the second packet, so the first
    packet's `q` is 4 and the reference 0).
  - A delta of 8.2 s ends the packet before that number.
  - A reference time of `0xFFFFFF` followed by `0x000000` extends to the next 64 ms.
  - A full revolution: an arrival with `q_first = 256 · 2^24 + 1` writes reference time 0 and
    first delta 1, and parses back to the same arrival.
  - Parsing: a zero-length run chunk followed by a valid vector parses to the vector's
    statuses; an FCI cut inside a chunk is `Chunk`; a `11` symbol is `Symbol`; a missing last
    delta is `Deltas`; 4 nonzero trailing bytes are `Tail`. None emits an `Acked`.
  - Overlap: a report of 10 to 13 all received, then a report of 12 to 15 with 12 marked not
    received, emits 12 once, as received, and never as lost.
  - Room: a report of 4 statuses with `max` 2 answers `Capacity` and leaves the log as it
    was; the retry with `max` 4 emits all four. A report of 65,535 statuses with `max` 1,000
    is refused before any `Acked`.
- **`trendline_flat` (RTP-9).** Packets fed through `on_acked`, packet `k` sent and arriving
  at `10k` ms, so each is a new group under the strict `> 5 ms` rule and every delay change
  is 0: every answer is `Normal`. The window fills at packet 21. The threshold stays 12.5 at the first update (`Δt = 0`), then
  `γ = 12.5 − 0.039 · 12.5 · 10 = 7.625`, then `7.625 · 0.61 = 4.65125`, clamped to 6.0 on
  the third update. *Oracle:* RTP-9's recurrence, by hand; the first update's `Δt = 0` is
  str0m's (`src/bwe/delay/trendline.rs:234-260`).
- **`trendline_overuse` (RTP-9).** The input sequence of str0m's `test_overuse`
  (`src/bwe/delay/trendline.rs:323-388`), fed as explicit group deltas with that test's
  threshold clock (`100g` ms in the warm-up, 600 ms after it), gives the hypotheses that
  test expects after warm-up: `Normal`, `Normal`, `Overuse`. On the arrival clock instead
  the third is still `Normal`. *Oracle:* the peer's test,
  which shares RTP-9's constants.
- **`aimd_steps` (RTP-9).** From `A = 1,000,000` in Increase, not near convergence, after a
  first `Normal` at 0 with `R = 600,000` that only stamps the time, then `Normal` with
  `R = 1,000,000` at `t = 1`: `A = 1,080,000` (within the 1.08 rounding of `exp`, which the
  law states to the bit). An `Overuse` with `R = 800,000` gives `A = 680,000`. *Oracle:* RTP-9,
  by hand.
- **`probe_result` (RTP-9, RTP-10).** Target 1 Mbit/s (`A = 1,000,000`, `L = 3,000,000`), `want`
  3 Mbit/s: the cluster rate is 2 Mbit/s, the probe size `max(200, 2,000,000 · 0.001 / 8) =
  250` bytes (a 20-byte header and `pad` 230, representable), one every 1 ms, so the cluster
  ends with its 16th probe, at 15 ms. The pacer, with empty queues,
  answers `Probe` every 1 ms and `Wait` between them, never `Idle`. Feedback reports 13 of the
  16 received (members 5, 9 and 12 missing; 13/16 ≥ 80%), arriving 1.25 ms apart per send
  slot. On the 13 received members, `bytes = 3,250`, the send span 15 ms and the arrival span
  18.75 ms: send rate `(3,250 − 250) · 8 / 0.015 = 1,600,000`, receive rate
  `(3,250 − 250) · 8 / 0.01875 = 1,280,000`, so `A = 1,280,000` and `target() = 1,280,000`. A
  variant with `L = 1,100,000`, a binding loss limit, runs the same cluster and ends with
  `A = 1,280,000` and `target() = 1,100,000`. With only 12 received (75%), there is no
  result.
  *Oracle:* RTP-9's rate arithmetic, by hand.
- **`pacer_spacing` (RTP-10).** Target 400 kbit/s, factor 2.5 (pacing 1 Mbit/s, 125 bytes per
  ms), queue limit 100 ms. *Scenario 1:* three 1,250-byte video packets queued at 0 leave at
  0, 10 and 20 ms. *Scenario 2:* the same three, and a 250-byte audio packet queued at 5 ms.
  The audio leaves at 5 ms (debt 625 + 250 = 875 bytes), so the debt reaches 0 at 12 ms: the
  second video packet leaves at 12 ms and the third at 22 ms. A pacer with two slots refuses
  a third push and gives it back. `remove` of the video stream answers its queued count.
  *Oracle:* RTP-10's debt arithmetic, by hand.
- **`rewrite_continuity` (RTP-11).** One source, target 0. Indices 100 to 105, with 102 a
  frame that target 0 does not need and 104 padding only. Outgoing indices are 1000, 1001,
  1002 and 1003 for 100, 101, 103 and 105, and the timestamps keep their source spacing. A
  late 102 answers `Drop`. *Oracle:* RTP-11, by hand.
- **`rewrite_late` (RTP-11).** Index 201 arrives after 202 was forwarded as 2002: it answers
  `Late(2001, ts)`. A real gap (203 lost, 204 forwarded as 2004) leaves 2003 unsent. *Oracle:*
  TLS-8's sealer rule and RTP-11, by hand.
- **`simulcast_switch` (RTP-11).** Clock 90,000. *Oracle:* RTP-11's clock rules, by hand.
  - Source A (sequence base 7,000) is current with offset 81,000. Its frame of timestamp
    90,000 is three packets; the first went out as outgoing index 500 with timestamp 9,000.
  - The target moves to B (sequence base 31,000, unrelated). B's packets answer `Drop` until
    one with `start`, `keyframe` and the target's `switches` bit. A's other two fragments,
    interleaved with B's dropped packets, go out as 501 and 502 at 9,000, the last at arrival
    `t0`; B's drops change none of A's offsets.
  - *SR case:* A's SR maps NTP 1000 s to 90,000; B's maps NTP 1000 s to 5,000,000. B's
    keyframe has timestamp 5,000,900, so its capture time is 1000.010 s, which is 90,900 on
    A's timeline, minus A's offset 81,000: 9,900.
  - *No-SR case:* the same keyframe arrives at `t0 + 10 ms`: `9,000 + 900 = 9,900`.
  - The keyframe is two packets with equal timestamps: both go out as 9,900, as outgoing
    indices 503 and 504. `switch_index` is 503.
  - B's next frame, at timestamp 5,003,900, goes out as `5,003,900 − 4,991,000 = 12,900` (B's
    offset is `5,000,900 − 9,900`).
  - Then C (timestamp base near the wrap, no SR): its keyframe at timestamp 4,294,967,000
    arrives 20 ms after B's last output 12,900, so it goes out as `12,900 + 1,800 = 14,700`;
    C's next frame, at wire timestamp 296 (592 ticks later across the 32-bit wrap), goes out
    as 15,292.
  - An SR sent 5 ms after 15,292 went out, with no SR from C, carries
    `15,292 + 450 = 15,742` under the outgoing SSRC with that stream's own counts.
- **`svc_masks` (RTP-11).** A stream with targets 0 to 31 and hand-written `needs` and
  `switches` masks per frame. Target 31 is accepted and target 32 is `Unrepresentable`.
  Target 2 is asked for at frame 1. It takes effect at the first frame whose `switches` has
  bit 2, and frames no target needs are closed up. From `Off`, the first packet with `start`,
  `keyframe` and the bit is forwarded (not `Wait`). A frame of three packets keeps one
  decision even when its later packets carry other masks. Two layer frames of one picture
  share a timestamp with `frame` 0 and 1, and only the first is needed by the target: the
  first goes out and the second is closed up. *Oracle:* RTP-11, by hand.
- **`codec_counter_continuity` (RTP-11).** *Oracle:* RTP-11's counter rules, by hand.
  A VP8 picture-ID counter (`Compact`, width 7) and a TL0PICIDX counter (`Compact`, width 8),
  target 0, then target 1 after the switch:
  - A: picture 125 (TL0 254, base, in two fragments) → 125 for both fragments, TL0 254;
    picture 126 (an enhancement, filtered) → omitted; 127 (TL0 255) → 126, TL0 255; 0
    (TL0 0) → 127, TL0 0 (TL0 wraps 255 → 0).
  - A switch to B, whose pictures are 15 bits and whose TL0 happens to be 254, a key still
    in A's ring before `restart`: keyframe 4000 (TL0 254) → 0 (picture wraps 127 → 0), TL0 1,
    not A's stale 254; its dependent enhancement 4001 (TL0 254) → 1, with TL0
    `lookup(254) = 1`, so the base it references is the picture sent as 0.
  - A frame-number counter (`Translate`, width 16) restarted so 65,534 → 10: 65,535 → 11,
    0 omitted (a gap at 12), 1 → 13, and frame 1's reference difference 2 points at 11, the
    output of 65,535. An absent field is `Absent`; a 15-bit input into this counter is `Width`.
- **`unit_adapter` (`media_bits`, RTP-11).** *Harness:* `core/programs/media_bits/vp8-laws.hirsel` <!-- doc-ref: planned -->
  and the existing `media_bits` laws for VP9, H.264 and AV1. Captured or hand-built packets
  give `Unit`: a VP8 packet with `S = 1`, `PID = 0` and a truncated first partition is not a
  `start`; a VP9 frame with `U` set whose referenced spatial layer was not forwarded has no
  `switches` bit; an H.264 FU-A start of the second NAL of an IDR access unit is not a `start`;
  an AV1 frame whose descriptor marks target 31 as Switch with satisfied chains has bit 31 of
  `switches`. *Oracle:* RFC 7741 §4.2, RFC 9628 §4.2, RFC 6184 §§5.7–5.8, AV1 RTP Appendix A,
  by hand.
- **`close_leaves_nothing` (RTP-12).** Two streams, each with known queued pacer entries and
  history entries. First, each delivers: a `poll` answers an entry of each, and a `get`
  answers `Found` on each. Then `Rewrite.close`, `History.clear` and `Pacer.remove` for
  stream 1: its `on_packet` answers `Drop`, every lookup is `Missing`, `remove` handed back
  exactly its queued entries, and `poll` never answers one of them. Stream 2 still forwards,
  still answers `Found`, and its queued entries still leave. *Oracle:* RTP-12, by hand.
- **`nack_recovery_under_loss` (RTP-6, RTP-7).** *Harness:* `std.sim.udp` (W0.3), with a
  sender (history, RTX, pacer) and a receiver (`Losses`) as two sans-IO ends. Fifty packets
  at 50 per second, then two sentinel packets, over a link of 40 ms delay with a **fixed
  delivery schedule** written in the law: media 10 lost once (its first NACK repairs it);
  media 20 lost with its first two RTX repairs (the third repairs it); media 30 lost and every
  repair of it lost (permanently lost after `tries` 3); media 49, the last, lost and revealed
  only by the first sentinel. `tries` 3, `retry_ns` 100 ms, `max_age_ns` and the history's
  age 2 s, `min_resend` 50 ms. *Oracle:* the schedule, by hand: the receiver ends with every
  media index but 30 delivered once, sends exactly the NACKs the schedule implies, and every
  RTX packet's OSN and payload equal the original's. Eight random schedules at 10% loss each
  way also run, as simulation coverage with no fixed expectation beyond RTP-6's laws.
- **`congestion_backoff` (RTP-8 to RTP-10).** *Harness:* `std.sim.udp`, a 1 Mbit/s
  bottleneck with a 100-packet queue. The sender starts at 300 kbit/s and offers 3 Mbit/s of
  1,200-byte packets through the pacer. Within 10 s the target is between 0.7 and 1.1 Mbit/s
  and stays there; the receiver's goodput over the last 5 s is at least 0.7 Mbit/s; at least
  1,000 queue-delay samples are taken in the last 5 s, and their 95th percentile is below
  200 ms. *Oracle:* the scenario's capacity, with 10 s and 200 ms as acceptance thresholds.
- **`parse_rewrite_cost` (RTP-12).** The cost path of RTP-12: at most 80 ns, 0 allocations.
  Each run first checks one rewritten packet's outgoing index, timestamp, SSRC and
  extension bytes against values worked by hand, and folds every output header into a
  checksum read after the timed region, so a skipped rewrite cannot pass. *Oracle:* the
  measurement, on a shielded CPU.

## Build slices

Each slice is one lane's commit sequence. It lands when its laws pass. The order is a
dependency order. S1 to S9 need neither `sim.udp` nor any crypto build; S12 needs the SRTP
package. Each law is carried by exactly one slice.

| slice | scope | needs | laws | size | lane |
|---|---|---|---|---|---|
| **S1** header and extensions | RTP-1, RTP-2, RTP-3: `core/programs/rtp/` <!-- doc-ref: planned --> with `hirsel.toml`, modules `header`, `seq`, `ext` | nothing | `header_chrome_vector`, `padding_counts`, `extension_forms`, `egress_form`, `index_estimate`, the RTP-1 and RTP-3 cases of `bounded_views` | M | Sol |
| **S2** RTCP | RTP-4: module `rtcp`, `check`, the `Packet` views and the writers | S1 | `rtcp_compound_rules`, the RTP-4 cases of `bounded_views`, the parse half of `nack_pairs` | M | Sol |
| **S3** statistics and requests | RTP-5, RTP-6: modules `stats` and `loss` | S2 | `rtt_example`, `reception_report`, `keyframe_coalesce`, the packing half of `nack_pairs` | M | Sol |
| **S4** history and RTX | RTP-7: modules `history` and `rtx`, plaintext layout and the per-leg contract | S1 | `history_lookup`, `rtx_layout`, the RTX case of `bounded_views` | S | Sol |
| **S5** TWCC | RTP-8: module `twcc` | S2 | `twcc_report_bytes`, `twcc_chunks` | M | Sol |
| **S6** bandwidth estimation | RTP-9: module `bwe` | S5 | `trendline_flat`, `trendline_overuse`, `aimd_steps` | M | Sol |
| **S7** pacer and probing | RTP-10 and RTP-9's probe | S4, S6 | `pacer_spacing`, `probe_result` | S | Sol |
| **S8** rewriting | RTP-11, RTP-12: modules `rewrite` and `counter`, and the close operations | S1, S4, S7 | `rewrite_continuity`, `rewrite_late`, `simulcast_switch`, `svc_masks`, `codec_counter_continuity`, `close_leaves_nothing` | L | Opus |
| **S9** unit adapter in `media_bits` | a `vp8` module (RFC 7741 descriptor: picture ID, TL0PICIDX, TID, `Y`, `S`, `PID`, keyframe from `P`, in-place rewrite of picture ID and TL0PICIDX at the output width), and the `Unit` adapter for VP8, VP9, H.264 and AV1. The one owner of the VP8 parser and descriptor laws; WebRTC slice S2 consumes them at egress | nothing | `unit_adapter` | M | Opus |
| **S10** network laws | the two `sim.udp` laws | S4 to S7, `std.sim.udp` (media-w0-sim-udp-2324) | `nack_recovery_under_loss`, `congestion_backoff` | M | Sol |
| **S11** cost | the RTP-12 cost path; the only slice with a cost target and the only one that runs a benchmark | S1, S3, S8 | `parse_rewrite_cost` | S | Sol |
| **S12** protected RTX | the SRTP wire case of RTP-7, in `core/programs/srtp/` | S4, DTLS slice S5 | `rtx_protected` in `core/programs/srtp/laws.hirsel` | S | Devin |

Every slice that changes a `.hirsel` source under `core/programs/rtp` <!-- doc-ref: planned --> or
`core/programs/media_bits` refreshes that package's `hirsel.laws` in the same commit.
