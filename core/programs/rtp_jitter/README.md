# rtp_jitter

The receive side of one RTP media stream, without I/O. The package reorders packets, assembles
codec frames from them, reports what is lost, maps RTP time to the sender's wall clock, and
gives an adaptive audio playout delay. It is the input stage for recording (C10b
`media_record`) and any other consumer of whole frames. It depends on `rtp` and `media_bits`
and is not part of `core/std`.

Modules:

- `jitter`: the reorder buffer and frame events (§§2-4).
- `depack`: per-codec assembly (§3).
- `sync`: the RTP-to-wall-clock mapper (§5).
- `playout`: the audio playout target (§6).

Nothing in the package reads a clock or a socket. The caller passes arrival times in
microseconds and collects events.

## 1. Use

```hirsel
let mut j = rtp_jitter.jitter.new(rtp_jitter.jitter.Config(codec: Vp8, window: 128, max_wait_us: 50000))
let mut events: List[rtp_jitter.jitter.Event] = list.new()
// for each RTP packet of this SSRC, after SRTP and header parsing:
rtp_jitter.jitter.push(mut j, h.seq, h.ts, h.marker, take payload, now_us, mut events)
// on a timer, so holes are given up while the stream is quiet:
rtp_jitter.jitter.poll(mut j, now_us, mut events)
// at end of stream:
rtp_jitter.jitter.finish(mut j, mut events)
```

Use one `Jitter` per SSRC. `payload` is the RTP payload: the bytes after the header and its
extension, without padding (`rtp.header.payload`). RTX is repaired into the original
sequence number before it is pushed. The `Codec` values are `Vp8`, `Vp9`, `H264`, `Av1` and
`Opus`.

`Event` has three variants:

- `Frame(ts, key, data)`: one whole frame. `ts` is the extended RTP timestamp, an `i64`
  that keeps counting past 2^32. The first frame's `ts` is its 32-bit timestamp plus
  2^32. Each later `ts` is the 32-bit timestamp extended to the value nearest the highest
  `ts` so far (`rtp.seq.estimate32`).
- `Gap(lo, hi)`: the half-open range of extended sequence indices that no `Frame`
  delivered.
- `NeedKeyframe`: send PLI or FIR upstream (§4).

## 2. Reorder buffer

### 2.1 Indices

The first packet pushed gets the extended index 65536 + its sequence number. Every later
packet gets `rtp.seq.estimate(highest, seq)`, where `highest` is the highest index so far.
A packet whose index is `Before` or `Beyond` is dropped. So is a duplicate of a buffered
packet. A packet below the floor (the lowest index not yet decided) is late and is dropped.

The buffer decides indices in order from the floor. Each index ends up either delivered in
a frame or reported in a `Gap`.

### 2.2 Holes

An index past the floor that has no packet is a hole. Its clock starts at the arrival of the
first packet past it. The hole is given up, and counts as lost, when either:

- `highest - index >= window`, or
- `now_us - (that arrival) >= max_wait_us`.

`window` is 1 to 32768 packets. `window = 1` means no reordering.

This gives the contract the reorder/loss law (§7.2) checks. A packet that arrives before the
packet `window` places after it is never given up. Any permutation in which each packet comes
after at most `window - 1` later packets loses nothing.

### 2.3 Stream start

The first packet need not be the lowest one sent. Until the start is fixed, a packet below
the floor moves the floor down, provided `highest - index < window`. The start is fixed by
the same rule as a hole one index below the floor: `highest + 1 - floor >= window`, or
`max_wait_us` after the first arrival. Nothing is emitted before the start is fixed. After it
is fixed, a packet below the floor is late.

### 2.4 Polling and end of stream

`push` and `poll` give up holes that are due and emit what follows them. `poll(now_us)` with
no packets is how a quiet stream releases a hole by time. `finish` gives up every hole and
treats every buffered run without its end as incomplete. The extended index space is
48-bit, as in `rtp.seq`.

## 3. Frames

A **run** is the packets from the floor that share the floor packet's timestamp, up to the
first packet with the marker bit set. It is **complete** when either:

- its last packet has the marker bit, or
- the index after its last packet holds a packet with another timestamp.

It is **incomplete** when the index after it is given up, or the stream finishes, first.

A complete run is a frame when its start is proven (§3.1) and `depack.assemble` accepts it.
Anything else becomes a gap. For `Opus`, every packet is its own frame.

### 3.1 Start

A run's first packet begins a frame when the packet at the index before it was received and
either had the marker bit or had another timestamp. When that index was lost, or nothing
came before it, the payload itself must prove the start:

- VP8: S=1 with partition index 0 (RFC 7741 §4.2).
- VP9: B=1, and either no layer indices or spatial layer 0 (RFC 9628 §4.2). All spatial
  layers of one timestamp form one frame, so only the base layer's first packet starts it.
- H.264: the first NAL is one of:
  - an access unit delimiter (kind 9);
  - an SPS (kind 7);
  - a non-IDR slice with `first_mb_in_slice` 0 (kind 1; H.264 §7.4.1.2.3, §7.3.3).

  The first NAL is a single NAL unit, the first unit of a STAP-A, or the start fragment of
  an FU-A. An IDR slice does not prove a start, because its SPS and PPS travel ahead of it in
  the same access unit. For libwebrtc senders this rule is exact. A sender that puts an SEI
  or a PPS ahead of a non-IDR picture's first slice can lose only that SEI or PPS.
- AV1: N=1 (a new coded video sequence, AV1 RTP §4.4). Nothing else in the aggregation header
  marks the first packet of a temporal unit. Z=0 only says the first element starts an OBU.
  So an AV1 frame whose previous packet was lost is delivered only when it starts a coded
  video sequence. Every other such frame is a gap. This is the one place where "all its
  packets arrived" is not enough.
- Opus: every packet.

### 3.2 Assembly

`depack.assemble(codec, parts)` takes the payloads of one run, in index order. It returns
`Assembled(key, data)`, or an error that turns the run into a gap.

- **VP8.** `data` is the payload after each descriptor, concatenated. The first packet must
  have S=1 and partition 0. `key` is true when the frame tag's P bit is 0 (RFC 6386 §9.1).
- **VP9.** `data` is the payload after each descriptor, concatenated across every spatial
  layer. No superframe index is added. The first packet must have B=1 in the base layer.
  `key` is true when P=0 on the first packet.
- **H.264.** `data` is each NAL unit with a 4-byte big-endian length in front (the AVCC form
  of ISO/IEC 14496-15). SPS and PPS stay in band.
  - A single NAL unit is copied whole.
  - A STAP-A gives each of its units.
  - An FU-A is rebuilt from the header that `media_bits.h264.parse_rtp` restores, followed by
    every fragment after the two-byte FU header. An FU-A must start, continue and end inside
    the run, and no other packet may come between its fragments.

  `key` is true when any NAL is an IDR slice (kind 5).
- **AV1.** OBU elements are joined across packets by Z and Y. A packet's first element must
  continue the open element exactly when one is open, and nothing may be open at the end.
  Each OBU is written with `obu_has_size_field` set and a minimal leb128 size, keeping its
  extension byte (AV1 §5.3.1). Temporal delimiters (kind 2) and tile lists (kind 8) are
  dropped (AV1 RTP §5). `key` is N on the first packet.
- **Opus.** `data` is the payload. `key` is always true.

These forms are byte-for-byte what str0m 0.26.0 (2ada76a) pops from its `DepacketizingBuffer`
for the same packets, with H.264 in its `is_avc` form (§7.1).

## 4. Gaps and keyframe requests

Consecutive given-up indices and gapped runs make one gap. A gap is emitted when the next
frame is emitted, or at the end of the `push`, `poll` or `finish` call that found it.
Consumers treat adjacent `Gap` events as one range. Every complete frame is emitted, also
after a gap. Whether the decoder can use a non-key frame after a gap is the consumer's call.

For video, `NeedKeyframe` follows these rules:

- It comes right after a `Gap`, unless one was already sent since the last keyframe.
- It comes right before a non-key `Frame` when no keyframe has been emitted since the start
  or since the last gap, again unless one was already sent.
- A key `Frame` clears both conditions.

Opus never asks for a keyframe.

## 5. Sender clock (`sync`)

`sync.new(rate)` takes the stream's RTP clock rate. `sync.on_sr(mut s, ntp, rtp)` records
the latest RTCP sender report's NTP timestamp and RTP timestamp (`rtp.rtcp.SenderInfo`).

`sync.wall_ns(s, ts)` maps an extended RTP timestamp to nanoseconds since the NTP epoch
(1900), rounded toward negative infinity. It returns None before the first report. The
computation is:

- `ntp_ns(ntp) = (ntp >> 32) * 10^9 + ((ntp & 0xffffffff) * 10^9 >> 32)`.
- The report's RTP timestamp is extended to the value nearest `ts`
  (`estimate32(ts, sr.rtp)`), so a frame before the report gives a negative delta, and so
  does one across a 32-bit wrap.
- The result is `ntp_ns + floor(delta * 10^9 / rate)`, computed as whole seconds plus a
  remainder so that nothing overflows.

Audio and video of one sender share the NTP clock. Equal `wall_ns` values mean the same
capture instant, which is lip sync (RFC 3550 §6.4.1). A consumer that writes a container
picks one stream's mapping as its timeline and places the other by `wall_ns`.

## 6. Audio playout target (`playout`)

`playout.new(rate, history, min_us, max_us)` creates the delay manager.
`playout.observe(mut p, ts, arrival_us)` records one packet with its extended RTP timestamp.
`playout.target_us(p)` gives the playout delay to aim for. It works like NetEQ's delay
manager, simplified to a fixed window:

- A packet's transit is `arrival_us - ts * 10^6 / rate`. The last `history` transits are
  kept.
- A packet's delay is its transit minus the smallest transit kept. So clock offset cancels,
  and delay is measured from the fastest recent packet.
- Delays are counted in 1 ms buckets. Delays at or past `max_us` share the last bucket.
- The target is the end of the bucket that holds the 95th percentile, which is the
  ⌈0.95·n⌉-th smallest delay: `(bucket + 1) * 1000` µs, clamped to `[min_us, max_us]`. With no
  packets it is `min_us`.

## 7. Laws

Every law goes red on the seeded bug named with it. The lane report lists each red run.

1. **Reassembly differential** (`chromium_reassembly_matches_str0m`).
   - Input: 53 Chromium 156 packets, copied from `webrtc_sfu/vectors/chromium_rtp.hirsel`. They
     cover VP8 rid h, H.264 rid h and rid q (STAP-A, FU-A and single NAL units), VP9 L3T3, and
     AV1 L3T3_KEY (N, Z and Y).
   - Each packet is parsed with `rtp.header` and pushed, both in arrival order and with each
     pair swapped.
   - The result must give exactly str0m 2ada76a's frames: timestamp, key flag, size and the
     SHA-256 of the bytes. There must be no gap and no keyframe request.
   - Seeded bug: an AV1 OBU written without its size flag.
2. **Reorder and loss** (`reorder_and_loss_deliver_exactly_the_whole_frames`, property).
   - Input: a sender that packetizes VP8, VP9, H.264, AV1 or Opus frames with each codec's
     RTP payload format. The streams start near sequence 65535 and near timestamp 2^32.
   - The network drops and duplicates packets. It delays each by fewer than `window` slots,
     breaking ties in reverse send order.
   - The events must hold exactly:
     - the frames whose packets all arrived and whose start §3.1 proves, with their bytes
       and timestamp steps;
     - the gaps over every other index from the lowest to the highest received;
     - keyframe requests by §4.
   - Seeded bugs:
     - releasing a hole one index early (`highest - index + 1 >= window`);
     - fixing the start one packet early;
     - letting an IDR slice prove an H.264 start;
     - the AV1 size flag.
3. **Time release** (`holes_release_on_time`).
   - Hand case: the start and a hole are released exactly at `max_wait_us`, never earlier.
   - The Gap and NeedKeyframe come in order, and a late packet is dropped.
   - Seeded bug: `>` for `>=`.
4. **Mapper** (`sync_maps_sender_report_to_wall_clock`).
   - Hand-derived values:
     - an NTP fraction with a remainder;
     - a frame before the report, floored to -11112 ns;
     - a frame across the RTP wrap;
     - audio at 48 kHz against video at 90 kHz with reports 0.25 s apart.
   - Seeded bug: truncating instead of flooring.
5. **Playout** (`playout_target_by_hand`, `playout_converges_to_p95`).
   - The hand case: delays 0-99 ms give 95 ms, plus the bounds and history eviction.
   - Convergence: within 10 ms of the distribution's 95th percentile for two distributions,
     uniform 0-100 ms (95 ms), and 0-10 ms with a tenth of packets at 200-210 ms (205 ms).
   - Seeded bug: the 50th percentile.
