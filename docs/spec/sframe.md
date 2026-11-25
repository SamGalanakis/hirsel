# SFrame: end-to-end media encryption

Version note (2026-11-25, sframe-import-build-2702, E1 built): slice E1 is built on TLS-8
slice T16. SFRAME-2 to SFRAME-13, the keying cost, the cost targets and the build laws now
describe the schedule build. The byte-keyed API, `salt_of` and the library's key and salt
derivation are gone; `ratchet_key`, a distribution helper, keeps its one HKDF over `std.hmac`.

Version note (2026-11-25, sframe-activate-2688): `sframe@1` ships in the toolchain bundle. A
program outside the repository binds it as `sframe = { major = 1 }`; the toolchain supplies it
with `media_bits@1` ([building](../building.md#the-installation)). `remove_generation` answers
`false` for a generation of `2^(64 − bits)` or more, as `install_ratcheted` refuses one, instead
of removing the installed generation it aliases after the shift (SFRAME-8, law K2).

Version note (2026-11-25, sframe-import-build-2702 v6): v6 applies the import attack
(sframe-import-attack-2699) to SFRAME-14 before E1 builds it. `sender` and `Follow` take `at`,
the step of the imported bytes, so a granted key resumes at a stored step in one core call
(M3). `install` takes the schedule and issues its own opener, `install_from` replaces
`opener` (S2), and a generation keeps its last forward candidate (S3). Intermediate openers
are all or nothing (S1), and a `Capacity` from a packet operation is the caller's
`Capacity` with nothing advanced (M4). The one-sender claim is narrowed to what TLS-8's new
send-key registry gives (M1), the erasure claims to what survives a ratchet (M2, S5), and a
granted send key is a resumed key (S6). A core key-seal operation to replace `ratchet_key`
is an open item (S7).

Version note (2026-11-25, sframe-import-spec-2696 v5): v5 specifies the keying end state, which
slice E1 builds. Base keys enter only as TLS-8 SFrame schedules
([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement), SFrame schedules), and core allows one
sealer per ratchet step and KID. A policy fixes each KID's direction, and the byte-keyed API
goes ([SFRAME-14](#sframe-14-keys-from-a-tls-8-schedule)). SFRAME-2 to SFRAME-13 still describe
the v4 build until E1 lands, and SFRAME-14 says what each becomes. `ssrc_key` (SFRAME-11) is
withdrawn before it is built. SFRAME-7's claim that `srtp`
opens through TLS-5's sink is corrected: `srtp` uses TLS-8 openers.

v4 (sframe-fixes-2680) applies the crypto recheck
(sframe-recheck-2678).
- **B1:** the ranged AAD now binds the frame's NAL start codes as `enc(starts)` (SFRAME-9). Before
  v4 an on-path party could merge two NALs, by rewriting a start code as an escaped body, or split
  one, by deleting an emulation-prevention byte, and the frame still opened with output that
  differed from the sender's. Every ranged tag changes; the wire length does not.
- **F1:** `jump` now bounds an empty window too, anchored at its floor, so one member frame no
  longer blacks out a key before its first frame or a ratchet step (SFRAME-7), and a caller with
  `jump` set installs with a floor near the sender's CTR (SFRAME-13).
- **F2:** a sender resumed from a `Mark` at or past the volume limit is `Exhausted`, from `seal` as
  from `reserve`, instead of trapping (SFRAME-5).
- The recheck's notes correct three claims: two forward commits can evict the real step (SFRAME-8),
  the framing is not in the AAD (SFRAME-9), and G1's rewrite cases rest on `oracle.py`'s reading of
  libwebrtc (build laws).

v3 (sframe-c9a-spec-2642) is the text as built. It names the
built API (`current_kid`, `wire_bound`, the `ranges` helpers), and its law G2 covers H.264 only,
because headless Chromium 153 has no H.265 encoder. v2 was amended after the crypto attack
(sframe-attack-2649). The Annex B escape is now a bijection (SFRAME-9), H.264 parameter sets
are made stable against Chrome's receive-side SPS rewrite (SFRAME-10), a ratcheted `base` is
the key at its step and a receiver can be advanced (SFRAME-8), ranged capacity and length rules
are bounded before any CTR is spent (SFRAME-9), and sender reservations, receive floors, a
jump bound and the per-track rule close S1 to S5. v1 (same day) was the first text of SFRAME-1
to SFRAME-13, the C9a item of the SOTA video-calls plan. The sections end with
[decisions](#decisions), [build laws](#build-laws), [cost targets](#cost-targets) and
[build slices](#build-slices).

This text defines the package `core/programs/sframe/`. It is a pure library implementing
[RFC 9605](https://www.rfc-editor.org/rfc/rfc9605) (SFrame) with the cipher suite
`AES_128_GCM_SHA256_128` (0x0004). It covers:

- the SFrame header;
- the key schedule;
- sealing and opening through TLS-8 SFrame sealers and openers;
- a replay window;
- the sender-key ratchet of RFC 9605 §5.1;
- a **ranged framing**, so a frame stays parseable by its codec's RTP packetizer.

The ranged framing keeps codec-chosen byte ranges in the clear and authenticates them as AAD.
The package takes keys and frames only. It reaches no `Foreign` code and does no I/O, so
`hirsel test --sim`, recording and replay hold for every program built on it
([FFI-8](foreign.md)).

**Keying, read first.** Keys reach the library only as TLS-8 SFrame schedules
([SFRAME-14](#sframe-14-keys-from-a-tls-8-schedule);
[TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement), SFrame schedules). Core holds each base
key and derives `sframe_secret`, the per-KID key and salt, and the ratchet from it. The nonce
is built inside the sealer or opener core issues, so no key, salt or ratchet output is ever
program bytes. What this gives and what it leaves:

- **A granted base key can be used**, as any key sealed with `secret.seal` can. Derived keys
  are confidential, not only use-marked.
- **Core marks direction.** A policy fixes each KID's direction (`sframe_import(receive)` or
  `sframe_import(send,<kid>,<bits>)`), so a receive key never seals, as RFC 9605 §4.4.1 asks.
- **One sender per key within a run** is core's: a second `send` import of equal bytes, by any
  route, is `Spent` (TLS-8's send-key registry). Equal bytes in another run, and bytes one
  ratchet step apart, stay the issuer's (SFRAME-13).
- **Erasure.** A base key, its PRKs and the imported bytes are zeroed at each ratchet step, at
  import and at drop. A retained step's per-KID key and salt live as long as its opener, and
  ring's AEAD key schedules are not erased (TLS-8, erasure).
- **Recordings.** A base key that reaches a member as program bytes, over `runner.room` or
  TLS, is on that member's tape in the `aead_open` that delivered it, so such a tape is
  key-bearing. Only a granted key leaves no copy.

Who uses it:

1. server-side group members holding keys, such as AI voice agents and recorders;
2. MoQ end-to-end encryption (Wave 2.3), which uses the RFC framing (SFRAME-12);
3. a Hirsel-native client SDK, and the JavaScript transform Hirsel ships for browsers.

The JavaScript transform implements this text over WebRTC Encoded Transform, Baseline since
Chrome 141. No browser ships native SFrame.

`sframe` is not `core/std`: SFrame is a media-framing protocol, not a general primitive.

## SFrame

### SFRAME-1: Scope

`sframe` implements RFC 9605 §4.2 to §4.4 and §5.1 for suite 0x0004, plus a ranged
framing for encoded-transform tracks; everything else in RFC 9605 and its neighbours is out of
scope or a named later slice.

In scope:

- **The header** (§4.3): KID and CTR, encoded and decoded, with minimal encoding enforced
  (SFRAME-3).
- **The key schedule** (§4.4.2) for `AES_128_GCM_SHA256_128` (SFRAME-4).
- **Seal and open** (§4.4.3, §4.4.4), with the application's metadata as AAD (SFRAME-5,
  SFRAME-6).
- **A replay window** per receiving key (SFRAME-7). RFC 9605 §9.3 leaves anti-replay to the
  application; this library owns it, as `srtp` owns SRTP's.
- **Sender keys** (§5.1): the KID split into generation and ratchet step, and the ratchet
  `base_key[i+1]`. The application decides when a sender ratchets or rekeys, and the library
  follows on receive (SFRAME-8).
- **Two framings** (SFRAME-9):
  - the **RFC framing**, `header ‖ ciphertext ‖ tag`, byte-exact to RFC 9605;
  - the **ranged framing**, which keeps codec-chosen ranges of a frame in place and in the
    clear, authenticates them, and moves the header and tag to a trailer.
- **Codec range rules** for Opus, VP8, H.264 and H.265 (SFRAME-10), over `media_bits`.

Not taken, with the reason:

| Item | Reason |
| --- | --- |
| `AES_128_CTR_HMAC_SHA256_{80,64,32}` (0x0001–0x0003) | They need AES-CTR and a truncated HMAC tag. TLS-5's AEAD sink has neither and offers no truncated tag by design. Short tags also weaken every frame (RFC 9605 §7.5). Every browser-side SFrame implementation known to the plan ships GCM. |
| `AES_256_GCM_SHA512_128` (0x0005) | Its HKDF is over SHA-512, which is not a derivation sink. It is added when a consumer asks for it, together with the sink. |
| DAVE's 8-byte truncated tag, ULEB128 nonce and `0xFAFA` footer | Hirsel follows RFC 9605, not DAVE's wire format. The ranged framing borrows DAVE's idea of authenticated clear ranges only. |
| MLS (RFC 9420) and the MLS KID scheme (§5.2) | Deferred until a consumer needs membership that the server cannot be trusted with. The KID is an opaque `u64` to the core of this library, so an MLS scheme can sit on top later. |
| RTP per-packet mode, the SFrame payload descriptor and `a=sframe` (draft-ietf-avtcore-rtp-sframe-02) | A named later slice, P1 (SFRAME-11). It touches `rtp` and webrtc's SDP, and no browser ships it. |
| Buffering a frame whose KID has no key | The RFC says MAY. The library reports `NoKey(kid)`, and the caller may buffer and retry (SFRAME-6). |

### SFRAME-2: Types

The package's public types and operations are the ones below. Modules are `sframe` (header,
keys, seal and open, window, ratchet) and `ranges` (layouts, escaping, codec rules).

```hirsel unbuilt
// module sframe
pub type Suite = Aes128GcmSha256_128()
pub fn suite_id(s: Suite) -> u16                     // 0x0004

pub type Header { kid: u64, ctr: u64 }
pub type HeaderError = Short | Malformed
pub fn header_len(h: Header) -> u64                   // 1 to 17
pub fn encode_header(h: Header, mut out: Bytes) -> Result[u64, Capacity]
pub fn decode_header(b: Bytes, at: u64) -> Result[(Header, u64), HeaderError]

pub type Scheme = Fixed(u64) | Ratcheted(Ratchet)
pub type Ratchet { generation: u64, bits: u8, step: u64 }      // the step the sender seals at
pub type Follow { generation: u64, bits: u8, step: u64, at: u64, forward: u8, keep: u8, floor: u64 }

pub type Limits { frame: u64, window: u64, keys: u64, ranges: u64, jump: u64 }
pub fn limits() -> Limits                               // 4 MiB, 128, 16, 256, 0 (off)

pub type SframeError = Invalid | Framing | Unreserved | Exhausted | Keys | Schedule(schedule.ScheduleError) | Capacity(Capacity)
pub type Discard: key = Malformed | Replayed | TooOld | TooFar | Tampered
pub type OpenError = NoKey(u64) | Discarded(Discard) | Capacity(Capacity)
pub type Sealed { kid: u64, ctr: u64, len: u64 }
pub type Opened { kid: u64, ctr: u64, len: u64 }
pub type Mark { ctr: u64, volume: u64 }

pub type Sender
pub fn sender(suite: Suite, framing: ranges.Framing, scheme: Scheme, take keys: schedule.Schedule, at: u64, from: Mark, l: Limits) -> Result[Sender, (SframeError, schedule.Schedule)]
pub fn current_kid(s: Sender) -> u64
pub fn next_ctr(s: Sender) -> u64
pub fn reserve(mut s: Sender, frames: u64, volume: u64) -> Result[Mark, SframeError]
pub fn ratchet(mut s: Sender) -> Result[(), SframeError]
pub fn seal(mut s: Sender, metadata: Bytes, plain: Bytes, mut out: Bytes) -> Result[Sealed, SframeError]
pub fn seal_ranged(mut s: Sender, layout: ranges.Layout, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Sealed, SframeError]

pub type Receiver
pub fn receiver(suite: Suite, framing: ranges.Framing, l: Limits) -> Result[Receiver, SframeError]
pub fn install(mut r: Receiver, kid: u64, keys: schedule.Schedule, floor: u64) -> Result[(), SframeError]
pub fn install_from(mut r: Receiver, kid: u64, s: Sender, floor: u64) -> Result[(), SframeError]
pub fn install_ratcheted(mut r: Receiver, f: Follow, take keys: schedule.Schedule) -> Result[(), (SframeError, schedule.Schedule)]
pub fn advance(mut r: Receiver, bits: u8, generation: u64, step: u64, take keys: schedule.Schedule, at: u64, floor: u64) -> Result[(), (SframeError, schedule.Schedule)]
pub fn remove(mut r: Receiver, kid: u64) -> bool
pub fn remove_generation(mut r: Receiver, bits: u8, generation: u64) -> bool
pub fn open(mut r: Receiver, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Opened, OpenError]
pub fn open_ranged(mut r: Receiver, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Opened, OpenError]

pub fn ratchet_key(base: Bytes) -> Bytes                // RFC 9605 §5.1's next base key, for distribution only (SFRAME-14)

// module ranges
pub type Escape = Bare | AnnexB
pub type Framing = Rfc | Ranged(Escape)
pub type Range { at: u64, len: u64 }
pub type Layout { ranges: List[Range] }
pub type RangeError = Malformed | TooMany | Unstable | Capacity(Capacity)
pub type Nals { work: Bytes, starts: List[u64] }          // an Annex B frame's working form
pub fn wire_bound(f: Framing, n: u64) -> u64            // worst wire length of n working bytes
pub fn trailer_bound(l: sframe.Limits) -> u64           // T(l)
pub fn none() -> Layout
pub fn canonical(layout: Layout, n: u64, most: u64) -> bool
pub fn layout_len(layout: Layout) -> u64
pub fn encode_layout(layout: Layout, mut out: Bytes)
pub fn decode_layout(b: Bytes, lo: u64, hi: u64, n: u64, most: u64) -> Option[Layout]
pub fn wire_escape(src: Bytes, lo: u64, hi: u64, mut out: Bytes)
pub fn codec_escape(src: Bytes, lo: u64, hi: u64, mut out: Bytes)
pub fn wire_unescape(w: Bytes, lo: u64, hi: u64, mut out: Bytes) -> bool
pub fn working(frame: Bytes) -> Result[Nals, RangeError]   // a sender's frame, input rules checked
pub fn unwire(frame: Bytes) -> Option[Nals]                 // a received frame
pub fn nal_end(nals: Nals, k: u64) -> u64
pub fn starts_clear(nals: Nals, layout: Layout) -> bool
pub fn vp8(frame: Bytes) -> Layout
pub fn h264(frame: Bytes, l: sframe.Limits) -> Result[Layout, RangeError]
pub fn h265(frame: Bytes, l: sframe.Limits) -> Result[Layout, RangeError]
pub fn normalize_h264(frame: Bytes, mut out: Bytes) -> Result[u64, RangeError]   // start codes, trailing zeros, SPS (SFRAME-10)
pub fn normalize_h265(frame: Bytes, mut out: Bytes) -> Result[u64, RangeError]   // start codes, trailing zeros
```

`Limits` is checked when a sender or receiver is made: `frame` 1 to `2^30`, `window` 64 to
4096, `keys` 1 to 256, `ranges` 0 to 1024, `jump` 0 (off) or at least `window`. Anything else
is `Invalid`.

`Sender` and `Receiver` are plain records that own TLS-8 schedules, sealers and openers, so
they are move-only and neither `key` nor `codec`. Neither type can be compared, stored or encoded. A sender or
receiver fixes its framing when it is made, and that framing is part of its keys' purpose
(SFRAME-9).

### SFRAME-3: The header

`encode_header` writes RFC 9605 §4.3's minimal form and `decode_header` accepts only that
form, so every (KID, CTR) pair has exactly one header.

- **Config byte**: `X K K K Y C C C`, from the most significant bit down.
  - KID < 8: `X = 0` and `K` is the KID.
  - Otherwise `X = 1` and `K` is the KID's minimal big-endian byte length minus one, and the
    KID bytes follow the config byte.
  - CTR works the same way through `Y` and `C`, and its bytes follow the KID's.
- **Length.** A header is 1 to 17 bytes. `header_len` gives it without encoding.
- **`decode_header(b, at)`** reads from `b[at]` and answers the header and its length:
  - `Short` when `b` ends before the config byte or before the bytes the config byte
    announces;
  - `Malformed` when an extended value is not minimal. That is a value below 8 sent
    extended, or a first value byte of zero.

A non-minimal header cannot verify unless its sender produced it, because the header is AAD.
Refusing it gives one meaning per header and drops the frame before any key work.

### SFRAME-4: The key schedule

For a base key and a KID, a TLS-8 SFrame schedule derives the AEAD key and the 12-byte salt
under RFC 9605 §4.4.2's labels, inside core. The library never sees either.

With `kid8` the KID as 8 bytes big-endian and `cs` the suite's id as 2 bytes big-endian
(`00 04`):

```text
sframe_secret = HKDF-Extract(salt = "", ikm = base_key)
sframe_key    = HKDF-Expand(sframe_secret, "SFrame 1.0 Secret key "  ‖ kid8 ‖ cs, 16)
sframe_salt   = HKDF-Expand(sframe_secret, "SFrame 1.0 Secret salt " ‖ kid8 ‖ cs, 12)
```

Each label ends with a space before `kid8`.

- **Base key.** A base key is the secret of an `sframe_import(<dir>)` grant or seal, 16 to 64
  bytes; any other length is `Invalid` from `import_sframe` (TLS-8 PC14). RFC 9605 fixes no
  length. Sender keys and ratchet outputs are 32 bytes, and an MLS exporter's are `AEAD.Nk`
  (16).
- **Direction.** The import's policy fixes which KIDs the schedule may seal and which it may
  open (SFRAME-14), so a receive key never seals, as RFC 9605 §4.4.1 asks.
- **When derivation happens.** Once per KID and step, when core issues a sealer or an opener:
  at `sender`, `ratchet`, `install`, `install_from`, `install_ratcheted` and `advance`, and
  for a forward candidate (SFRAME-8). Frames do no derivation.
- **Where the key lives.** Each sending key is one TLS-8 `Sealer` and each receiving key one
  `Opener`, holding that KID's `sframe_key` and `sframe_salt`. A `ScheduleError` from an
  issue is `Schedule(e)`, and nothing is installed.

Check against the vector (RFC 9605 C.3, suite 0x0004, recomputed with Python `hmac` and
`cryptography` for this text):

- base key `00..0f` and KID `0x123` give `sframe_secret` `d926952c…314c40c0`;
- `sframe_key` is `d34f547f4ca4f9a7447006fe7fcbf768`;
- `sframe_salt` is `75234edefe07819026751816`.

Law V2 checks these through the ciphertext a schedule's sealer gives; TLS-8 PC12 checks the
schedule directly.

### SFRAME-5: Sealing

A sender seals frame `n` with nonce `sframe_salt XOR be96(ctr)` and AAD
`header ‖ metadata`. It never seals a CTR twice under one key, and a refusal consumes no
CTR.

- **Nonce.** `be96(ctr)` is the CTR as a 12-byte big-endian integer. CTR is at most 64 bits,
  so it lands in the salt's last 8 bytes.
- **`sender(suite, framing, scheme, take keys, at, from, l)`** makes one sending key from a
  TLS-8 SFrame schedule whose imported bytes are the key at step `at` (SFRAME-14):
  - `Fixed(kid)` sends under `kid`, at step 0.
  - `Ratcheted(Ratchet(generation, bits, step))` sends under
    `(generation << bits) | (step mod 2^bits)`, with step `step`'s key (SFRAME-8). `bits` is 1
    to 16, and `generation` must fit in `64 − bits` bits.
  - `at` past the scheme's step, or more than `2^16` below it, is `Invalid`. Otherwise core
    issues one sealer, `sframe_sealer(keys, step − at, kid, from.ctr)`, which ratchets the
    schedule to `step` in the same call. A refusal there is `Schedule(e)`.
  - Every refusal returns the schedule unmoved with the error.
  - `from` is where the key's counters start: `Mark(0, 0)` for a key never used, or the last
    mark the caller stored for a key it resumes (below).
- **Reservations.** RFC 9605 §9.1 asks a sender to store its next counter before using it.
  A durable write per frame is not something a media sender does, so the sender spends
  counters only inside a **reservation** the caller has stored.
  - A sender holds a **ceiling** `Mark(ctr, volume)`. It starts equal to `from`, so a new
    sender reserves nothing and every seal is `Unreserved`.
  - `reserve(mut s, frames, volume)` raises the ceiling to `next + frames` CTRs and
    `spent + volume` volume units (below), each capped at the key's limit, and answers the new
    ceiling. It changes nothing else, and it is `Exhausted` when the key has nothing left to
    reserve.
  - The caller stores the answered ceiling durably before it seals past the old one. A sender
    that resumes passes that stored ceiling as `from`. Counters between the last seal and the
    stored ceiling are skipped, never reused, so a crash at any point cannot repeat a nonce
    and the volume limit holds across runs.
  - `next_ctr` gives the next CTR.
  - The default is a fresh base key per run, which needs no stored state at all. Resuming a
    key is for a sender that cannot get a new one.
- **`seal(mut s, metadata, plain, mut out)`** is the RFC framing. It appends
  `header ‖ ciphertext ‖ tag` to the spare of `out` and answers `Sealed(kid, ctr, len)`, with
  `len` the exact number of bytes appended. The results, in this order, all before the AEAD
  call, with no state changed on a refusal:
  - `Framing`: the sender's framing is not `Rfc` (for `seal_ranged`, not `Ranged`).
  - `Invalid`: `plain` is longer than `l.frame`.
  - `Exhausted`: the next CTR is `2^64 − 1` or more, or the key's volume is at its limit or would
    pass it (below). The key is spent; the caller ratchets or rekeys. A sender resumed from a
    `Mark` whose volume is already at or past the limit is `Exhausted` from `reserve` and from
    every seal; it never traps.
  - `Unreserved`: the next CTR or the volume would pass the ceiling. The caller reserves more.
  - `Capacity(c)`: the spare of `out` is shorter than the result, with the result's length as
    `requested`.
  - Otherwise the sealer seals with `packet.seal_into`, the header in `out` as AAD and
    `metadata` as `extra`. Its `Capacity(c)`, for the AAD scratch it admits, is `Capacity(c)`
    with nothing changed. On success the CTR advances by one and the volume grows. The sealer
    makes no other refusal after these checks: the CTR is above its floor and unused, and the
    plain bound and spare were checked.
- **Volume limit.** Each sending key counts `q + σ`: frames sealed, plus 16-byte blocks of
  plaintext and AAD. A seal costs `1 + blocks(plain) + blocks(aad)`, where `blocks(x)` is
  `⌈x / 16⌉` and `aad` is the whole AAD (for `seal_ranged`, with `metadata′` of SFRAME-9 and
  the secret bytes as `plain`). It refuses with `Exhausted` past `2^34`, about 256 GiB.
  - Basis: draft-irtf-cfrg-aead-limits-09 §5.1.1, the AES-GCM confidentiality bound
    `CA ≤ (σ + q + 1)^2 / 2^129`. At `2^34` the advantage is about `2^-61`, the margin
    TLS 1.3 takes. The integrity bound (§5.1.2) allows about `2^52` forgery attempts against
    4 MiB frames, which no receiver reaches, so receivers count nothing.
  - At 20 Mbit/s the limit is about 30 hours. The caller ratchets or rekeys before then, so
    a sender exposes no lifetime timer.
- **Simulcast and SVC.** Each layer's frame is its own ciphertext with its own CTR (RFC 9605
  §6.1.2, §6.1.3). A sender shared across the encodings or layers of one media stream counts
  one CTR sequence for all of them. Sharing one key across different media streams is a
  caller error with two consequences, stated in SFRAME-13: frames splice between the streams,
  and the replay window is counted in the wrong unit.

### SFRAME-6: Opening

`open(mut r, metadata, frame, mut out)` decodes the header, finds the key for its KID,
rejects replays, opens with the KID's TLS-8 opener, and only then updates the window. A frame that
fails is `NoKey(kid)`, `Capacity(c)` or `Discarded(d)`, with nothing appended and no state
changed.

The steps, in this order:

1. `Discarded(Malformed)` when `frame` is longer than `l.frame + 17 + 16`, or the header
   does not decode (SFRAME-3), or fewer than 16 bytes follow the header.
2. **Key lookup.** The KID names a fixed key, a retained or current ratchet step, or a
   forward candidate (SFRAME-8). None of these gives `NoKey(kid)`. The RFC lets the caller
   buffer such a frame and retry once a key arrives (§4.4.4).
3. **Window check** (SFRAME-7): `Discarded(TooOld)`, `Discarded(Replayed)` or
   `Discarded(TooFar)`. A forward candidate's window is empty with the floor 0, so only
   `TooFar` can refuse it.
4. `Capacity(c)` when the spare of `out` is shorter than the plaintext, which is the frame
   length minus the header length minus 16.
5. **The opener opens** with `packet.open_into`, which uses nonce `sframe_salt XOR be96(ctr)`
   and AAD `header ‖ metadata`. When the tag fails the result is `Discarded(Tampered)`, and
   the opener appends nothing and releases no unverified plaintext. Its `Capacity(c)`, for the
   AAD scratch, is `Capacity(c)`.
6. **On success:** a forward candidate commits (SFRAME-8), which can still be `Capacity(c)`
   with nothing changed, then the window marks the CTR and `open` answers
   `Opened(kid, ctr, len)`.

The `Discard` reasons stay local. They exist so counters can report them (SFRAME-13), and a
receiver never sends them back to a sender (RFC 9605 §7.5). Steps 1 to 3 do no AEAD work, so
a malformed frame or a replay is refused faster than a forgery. RFC 9605 §4.4.4 asks for
equal time only against an outside observer, and SRTP checks replay before authenticating
for the same reason (RFC 3711 §3.3).

### SFRAME-7: The replay window

Each receiving key keeps an RFC 3711 §3.3.2 window over CTR, above a floor the caller
gives. It accepts a CTR it has not accepted before that is at or above the floor and above
`highest − W`, and it advances only after a frame authenticates.

- **Size.** `W` is `l.window`, 64 to 4096, default 128. The state is the floor, the highest
  accepted CTR and a `W`-bit map below it.
- **Floor.** `install`, `install_ratcheted` and `advance` take `floor`, and a CTR below it is
  `TooOld` from the start. Only forgery needs no floor: a frame cannot authenticate without the
  key. Replay does need one. A receiver that restarts, re-subscribes, or removes and
  reinstalls a key has an empty window, and anyone who recorded the stream can then replay
  genuine frames from minutes ago, which authenticate and are shown as live. The key
  distribution message carries the sender's current CTR, and the receiver installs with it as
  the floor. A floor of 0 accepts the whole history once.
- **First frame.** A key's first authenticated frame at or above the floor sets `highest`.
- **Classifying a CTR** against the state:
  - below the floor: `TooOld`;
  - when `l.jump` is not 0: above `highest + l.jump`, or, before the first frame, above
    `floor + l.jump`: `TooFar`. The floor is the anchor until a frame sets `highest`;
  - above `highest`: accepted, and the map shifts;
  - in `(highest − W, highest]`: accepted only when its bit is clear, otherwise `Replayed`;
  - at or below `highest − W`: `TooOld`.
- **Gaps** of any size are fine when `l.jump` is 0: an SFU that drops a layer or a simulcast
  encoding leaves gaps in CTR, and a jump forward only shifts the map.
- **A member can black out a sender** (RFC 9605 §7.2: any holder of a key can forge under it).
  An outsider cannot move the window (law W1). A member can seal one frame under the victim's
  KID at CTR `2^64 − 2`. It sets `highest` there, and every later genuine frame is `TooOld` at
  each receiver that saw it, until the victim rekeys, even after the forger has left. A member
  can forge media anyway, so what this adds is persistence. `l.jump` bounds it: with `jump`
  set, a frame more than `jump` above `highest`, or above the floor before the first frame, is
  `TooFar` and moves nothing. Every window is bounded alike, including a key that has not yet
  received and a ratchet step's new window, so a blackout needs a stream of forged frames and
  ends when the forger stops or is removed. The costs:
  - A genuine gap larger than `jump`, such as a sender paused for a long time with its CTR
    advancing, is discarded until the receiver reinstalls.
  - With `jump` set, the floor passed to `install`, `install_ratcheted` and `advance` must be
    within `jump` below the sender's current CTR, or the sender's frames are `TooFar` until it
    is (SFRAME-13). A ratchet step reached by a forward commit starts with floor 0, which is
    right for a sender that ratchets, since its CTR restarts at 0; a step whose first frame
    arrives above CTR `jump` is `TooFar` until `advance` installs it with a floor.

  The default is off; a deployment that sends gaps of a known size turns it on above them.
- **Scope.** Each KID's key has its own window, because CTR restarts under each new key. A
  ratchet step's window starts empty with floor 0, and removing a key drops its window.
- **Order.** The window is read at step 3 of SFRAME-6 and written at step 6. A forged frame
  with a high CTR therefore never moves `highest` (law W1).

**Openers and the window.** A TLS-8 `Opener` is immutable and holds no window ("replay windows
are the protocols'", TLS-8). `srtp` opens through TLS-8 openers and keeps its window itself.
`sframe` opens through TLS-8 SFrame openers and keeps its window itself too (SFRAME-14). This
settles the plan's "TLS-8 opener-window decision": TLS-8 needs no window.

### SFRAME-8: Sender keys and the ratchet

A ratcheted KID is `(generation << R) | (step mod 2^R)`, and the ratchet is RFC 9605 §5.1's
`base_key[i+1] = HKDF-Expand(HKDF-Extract("", base_key[i]), "SFrame 1.0 Ratchet", 32)`. A
receiver follows a bounded number of steps forward and keeps a step only after a frame
authenticates under it.

- **Who decides when.** The library supplies the mechanism and the caller decides when to
  use it. A new receiver joining is a reason to ratchet. A receiver leaving needs a new
  generation, which is a new base key sent out of band (RFC 9605 §5.1).
- **What `at` means.** For `sender`, `install_ratcheted` (`Follow.at`) and `advance`, the
  schedule's imported bytes are the key **at** step `at`, and the call reaches `step` in the
  one core call that issues its sealer or opener, at most `2^16` steps (SFRAME-14). A joiner
  is handed the current step's key with `at` equal to that step, never step 0's, so it cannot
  read the generation's earlier media, which is what ratcheting on a join is for. A party
  that hands keys out as bytes keeps the latest `(base, step)` pair, with `ratchet_key` for
  the next step's key.
- **Sending.** `ratchet(mut s)` on a `Ratcheted` sender:
  - issues `sframe_sealer(keys, 1, next_kid, 0)`, one core call that moves the schedule one
    step and derives the new KID's key and salt, and adds 1 to `step`;
  - restarts the counters at `Mark(0, 0)` under the new key, with the ceiling there too, so
    the caller reserves (SFRAME-5) and stores `(step, ceiling)` before the next seal. A crash
    between `ratchet` and that write resumes the old step from its stored ceiling, which is
    still safe.
  - The old step's sealer is dropped.
  - It is `Invalid` on a `Fixed` sender and `Exhausted` when `step` is `2^64 − 1`. A
    `Schedule(e)` result, `Capacity(..)` or `Invalid` for a KID past the import's block,
    leaves the sender as it was, still sealing under the old step (law I1).
  - Within one generation the KID repeats every `2^R` steps, each time with a different base
    key, so `(base_key, KID, CTR)` stays unique (RFC 9605 §9.1).
- **Following.** `install_ratcheted(mut r, Follow(generation, bits, step, at, forward, keep, floor), take keys)`
  installs a generation whose current step is `step`, with `floor` for that step's window. It
  needs `1 ≤ forward ≤ 16`, `forward ≤ keep ≤ 16`, `keep + 1 + forward ≤ 2^bits` and `at` as
  for `sender`; anything else is `Invalid`, and a refusal returns the schedule. The last bound gives every low-bit value at most one meaning. For a frame
  whose KID's high bits name the generation and whose low bits are `low`, with current step
  `s`:
  - If `low` is a retained or the current step's low bits, the frame uses that step's key.
    Retained steps are the up to `keep` steps before `s` that the receiver already holds.
  - Otherwise `d = (low − s) mod 2^bits`. When `1 ≤ d ≤ forward`, the frame opens under a
    **candidate**, `sframe_opener(keys, d, kid(s + d))`, which does not move the schedule. A
    generation keeps its last candidate and reuses it for the next frame at the same `d`.
    When `d` is larger, the frame is `NoKey(kid)`.
  - **Commit.** A candidate that authenticates becomes the current step `s + d`. The library
    issues openers for the intermediate steps the new step retains, all or nothing; a
    refusal is the frame's `Capacity(c)` with nothing moved. Then `sframe_ratchet(keys, d)`
    commits. The retained set keeps only the `keep` steps below the new current one, and
    released steps drop their opener and window.
  - **No commit.** A candidate that fails to authenticate is kept for its `d` and changes
    nothing else.
  - **Why `keep ≥ forward`.** Any member can seal a genuine frame at `s + forward` (RFC 9605
    §7.2). If `keep` were smaller, that one commit would release step `s`, and the real
    sender's frames would be `NoKey` until it ratcheted past. With `keep ≥ forward` the step a
    commit jumped from is always retained. Two commits are not bounded this way: member frames
    at `s + forward` and then `s + 2·forward` release step `s` whenever `keep < 2·forward`, and
    only `advance` or a new generation brings the real sender back.
- **Catching up.** `advance(mut r, bits, generation, step, take keys, at, floor)` moves an
  installed generation to a later step with a supplied key: the key distribution's recovery
  path for a receiver that missed more than `forward` steps, or any rekey within a generation.
  Held steps in `[step − keep, step)` stay retained with their windows; older ones are
  released. It is `Invalid` when the generation is not installed or `step` is not above the
  current step, and it issues the new step's opener before it changes anything, so a
  `Schedule(e)` leaves the receiver as it was and returns the new schedule.
- **Cost of a forgery.** An attacker who picks a KID can make the first frame at a new `d`
  cost one opener issue of at most `forward` ratchet steps and one AEAD open; later frames at
  that `d` cost one AEAD open each. It can never move `s` (law K2).
- **Generations.**
  - A ratcheted generation claims the `2^bits` KIDs `generation << bits` to
    `(generation << bits) | (2^bits − 1)`.
  - `install` of a fixed KID inside a claimed range is `Invalid`, as is `install` of a fixed
    KID already installed and `install_ratcheted` over an installed KID or generation. The
    caller removes first. `remove` then `install` of the same key reopens replay unless the
    new install's floor is above the old window (SFRAME-7).
  - Old generations stay until `remove_generation`, so frames reordered across a rekey still
    open. `remove_generation` answers `false`, and removes nothing, when `bits` is out of range,
    when `generation` is `2^(64 − bits)` or more, or when no generation of that `bits` is
    installed at `generation << bits`.
  - The table holds at most `l.keys` keys. A fixed key counts one, and a ratcheted generation
    counts `keep + 1` from its install, the most it can ever hold, so commits and `advance`
    never need to evict. An install that would pass `l.keys` refuses with `Keys`.
- **Fixed KIDs.** `install(mut r, kid, keys, floor)` adds one fixed key: an opener issued
  from the schedule it is lent. The caller drops the schedule once its KIDs are installed.
  `install_from(mut r, kid, s, floor)` installs an opener from a sender's own schedule, for a
  KID outside its send block. KID assignment across senders (RFC 9605 §9.2) is the caller's.
- **Unchecked.** Core refuses a second sender of equal bytes in one run, but not equal bytes
  in another run or bytes one ratchet step apart ([keying](#sframe-end-to-end-media-encryption)).

### SFRAME-9: The ranged framing

The ranged framing leaves the ranges of a layout in place and in the clear, encrypts the
rest of the frame in place, and appends `tag ‖ header ‖ layout ‖ L`. It authenticates the
layout and the clear bytes as part of RFC 9605's `metadata`, so the crypto is RFC 9605's
`encrypt` unchanged.

**Why.** Over WebRTC Encoded Transform, the browser's codec packetizer runs after the
transform. It reads codec structure in the frame: VP8's frame tag, and H.264's start codes and
NAL headers. The SFU reads the same bytes for keyframes. The RFC framing puts the header at the
front and encrypts everything, which breaks them. This is the mode LiveKit's FrameCryptor and
DAVE use (both read in source). RFC 9605 defines no wire format for it, so this
framing is Hirsel's own. It interoperates with Hirsel's JavaScript transform and with other
`sframe` users.

**Layout.** A layout is a list of clear ranges `(at, len)` over the frame. It is
**canonical** when every range has `len ≥ 1`, the ranges are in order and neither overlap nor
touch, the last range ends within the frame, and there are at most `l.ranges` of them. Its
encoding `enc(layout)` is ULEB128 of the range count, then for each range ULEB128 of `gap` and
`len`. `gap` is the distance from the end of the previous range, or from 0 for the first.
Every ULEB128 is minimal, and every value is below `2^32`.

**Lengths.** `wire_bound(f, n)` is the longest wire form of `n` working-form bytes: `n` for `Rfc`
and `Ranged(Bare)`, and `n + n/2 + 1` (integer division) for `Ranged(AnnexB)`, because the
escape adds at most one byte per two zeros and one at a NAL's end. `T(l)` is the longest
trailer, `16 + 17 + 5 + 10·l.ranges + 2`: the tag, the longest header, the longest layout
(ULEB128 count and two ULEB128 values per range, each below `2^32`) and `L`.

**Sealing.** `seal_ranged(mut s, layout, metadata, frame, mut out)`. Let `clear` be the bytes
of the frame's working form inside the layout's ranges and `secret` the bytes outside them, both
in order. For `Ranged(Bare)` the working form is `frame` itself. For `Ranged(AnnexB)` it is
the **unescaped form**: `frame` with each NAL's emulation-prevention bytes removed (below). The
steps:

1. Refusals before any work, with no state changed:
   - `Framing`: the sender's framing is not `Ranged`;
   - `Invalid`: the layout is not canonical for the working form, or `frame` is longer than
     `l.frame`, or, under `AnnexB`, `frame` breaks a rule of [Annex B frames](#annex-b-frames);
   - then `Exhausted` and `Unreserved`, as in SFRAME-5;
   - then `Capacity` when the spare of `out` is shorter than
     `wire_bound(framing, len(working form) + len(trailer))`, with that bound as `requested`.
     The exact wire length depends on the ciphertext, which does not exist yet, so the check
     is against the bound. Nothing can refuse after the AEAD call, so a refusal never spends a
     CTR and no CTR is ever sealed twice.
2. Seal `secret` as RFC 9605 `encrypt(CTR, KID, metadata′, secret)`, with
   `metadata′ = enc(layout) ‖ enc(starts) ‖ clear ‖ metadata`. So the AAD is
   `header ‖ enc(layout) ‖ enc(starts) ‖ clear ‖ metadata`. `enc(layout)` determines the
   length of `clear`, and both encodings end on their own, so the split is unambiguous.
   - `enc(starts)` is ULEB128 of the number of NALs, then, for each NAL in order, ULEB128 of
     its start code's working-form offset minus the end of the previous start code (minus 0
     for the first). Every ULEB128 is minimal. Start codes at 0 and 8 give `02 00 04`.
   - Under `Ranged(Bare)` there are no NALs, so `enc(starts)` is `00` and both ranged framings
     have one AAD shape.
   - It binds where NALs begin. The working form alone does not: the bytes `00 00 00 01` in it
     are the same whether they are a start code or body bytes escaped on the wire as
     `00 00 03 00 01`.
3. Write **the body**: the working form with each secret byte replaced, in order, by the
   ciphertext byte at the same index. GCM is a stream cipher, so the body is as long as the
   working form.
4. Append the **trailer** `tag ‖ header ‖ enc(layout) ‖ L`. `L` is
   `len(header) + len(enc(layout))` as 2 bytes big-endian. A canonical layout within
   `l.ranges ≤ 1024` keeps `L` below `2^16`.
5. Under `AnnexB`, apply the **wire escape** at the frame's own NAL boundaries (below). The
   trailer is part of the last NAL.
6. Answer `Sealed(kid, ctr, len)` with `len` the exact number of bytes appended.

**Opening.** `open_ranged(mut r, metadata, frame, mut out)`. Every refusal before step 5 is
`Discarded(Malformed)` unless named otherwise, with nothing appended and no state changed:

1. `frame` is longer than `wire_bound(framing, l.frame + T(l))`. This bounds every pre-authentication
   pass by `l.frame` and `l.ranges`, and two receivers with equal limits accept the same
   frames.
2. Under `AnnexB`, find the start codes and unescape each NAL with the wire rule. A NAL that
   is not in the wire escape's image is refused.
3. With `n` the length of the working form: `n < 20`, or `L` (its last 2 bytes) below 2, or
   `n < L + 18`, is refused. Otherwise the header starts `L + 2` bytes from the end. Decode it
   (SFRAME-3), and decode the layout from the rest of those `L` bytes. The layout must end
   exactly at `L` and be canonical for a body of `n − 2 − L − 16` bytes. Under `AnnexB`, each
   start code found in step 2 must lie inside a clear range. The 16 bytes before the header
   are the tag.
4. `Capacity(c)` when the spare of `out` is shorter than `wire_bound(framing, len(body))`, with
   that bound as `requested`. The output is re-escaped plaintext, whose length is not known
   before authentication.
5. Then apply SFRAME-6 from step 2, to the reconstructed RFC ciphertext
   `header ‖ secret(body) ‖ tag` with
   `metadata′ = enc(layout) ‖ enc(starts) ‖ clear(body) ‖ metadata`, `starts` being the start
   codes found in step 2. Step 1 bounds their number by a quarter of the frame, so no separate
   limit is needed.
6. On success, `out` receives the original frame: the clear bytes in place and the opened
   plaintext at the secret positions, and under `AnnexB` escaped with the **codec escape**
   (below). `Opened.len` is the exact length.

**What the binding covers.** The header, the layout, the NAL boundaries and every clear byte
are in the AAD. A change to any of them, or to the ciphertext or the tag, fails the tag.
- Binding the layout matters, not only the clear bytes: two layouts can give equal `clear`
  and equal ciphertext bytes and still put the opened bytes in different places. Law T2 builds
  such a pair.
- Binding the starts matters for the same reason. Rewriting a start code `00 00 00 01` as
  `00 00 03 00 01` merges two NALs, and deleting the `03` of a clear `00 00 03 00 01` splits
  one, with an unchanged working form. Each changes which bytes a decoder reads as a NAL, so
  an SFU could drop a slice, an SEI or a parameter set behind another NAL. Law T1 makes every
  such edit.
- **Interoperability.** The ranged framing is Hirsel's own and has no other implementer, so
  this AAD is a wire-format decision that only Hirsel peers must share. Chromium's
  depacketizer never reads the AAD: G2's recapture under v4 shows the frames arrive unchanged
  and open.

**Framing is a property of the key.** A sender or receiver has one framing for its whole
life. `seal` on a ranged sender and `seal_ranged` on an RFC sender are both `Framing`. So no
key the library derives is used under two framings. The framing itself is not in the AAD: an
RFC frame's AAD can equal a zero-range ranged AAD, and `Bare` and `AnnexB` share one shape.
Using the same base key in a sender and a receiver of different framings is a caller error
(SFRAME-13) that the library cannot see.

#### Annex B frames

H.264 and H.265 byte streams find NAL units by start codes, so ciphertext and trailer bytes
must never contain one. In uniform bytes a start-code emulation (`00 00` followed by a byte
`≤ 0x03`) occurs at about `2^-22` per position. That is about one per 4 MiB: rare per frame,
but certain within minutes of a video stream. `Ranged(AnnexB)` handles it with the codecs'
own emulation prevention (H.264 §7.4.1 and §7.4.1.1, H.265 §7.4.2 and §7.4.2.1) and one extra
rule at each NAL's end.

- **Input rules.** `seal_ranged` refuses with `Invalid`:
  - a frame that does not begin with a start code;
  - a 3-byte start code (`00 00 01`), anywhere. WebRTC's H.264 and H.265 depacketizers rebuild
    every NAL with a 4-byte start code whatever the sender sent (libwebrtc
    `h264_sps_pps_tracker.cc`, and LiveKit's `normalizeAnnexBStartCodes` in client-sdk-js
    `src/e2ee/worker/naluUtils.ts`). A 3-byte start code inside a clear range would reach the
    receiver changed and fail the tag.
  - a NAL whose body ends in `00`. Annex B allows `trailing_zero_8bits` after a NAL, and
    depacketizers drop them, so they would not survive the trip either.
  - a NAL whose body is not canonically escaped under the codec escape. A conforming
    encoder's output always is, because the codecs forbid `00 00 03` before a byte above
    `0x03`.
  - a layout under which a start code is not inside a clear range.

  `ranges.normalize_h264` and `normalize_h265` rewrite 3-byte start codes to 4 and strip
  trailing zero bytes, so a sender passes every frame through one of them first.
- **Working form.** Each NAL body (the bytes between its start code and the next) is
  unescaped by removing the `03` of every `00 00 03`. Layout offsets, `clear`, `secret` and
  the AAD are all over this form.
- **Wire escape** (sealing step 5). The sender takes NAL boundaries from the input frame's
  start codes, never by scanning the sealed bytes, which may contain `00 00 01` by chance. In
  each NAL body it:
  - inserts an `03` after every `00 00` that is followed by a byte `≤ 0x03` (the interior
    rule);
  - then, if the result ends in `00` followed by zero or more `03`, appends one `03` (the tail
    rule).
- **Wire unescape** (opening step 2). The receiver splits at `00 00 00 01`. For each NAL body
  `w` it refuses a `w` that ends in `00`; if `w` ends in `00` followed by one or more `03`, it
  removes the last `03`; then it removes the `03` of every interior `00 00 03`. The NAL is
  accepted only when wire-escaping the result gives `w` back, so every refused stream is
  outside the escape's image and each NAL body has exactly one wire form. The escape alone
  does not fix where the NALs split, since one working form can be cut into NALs in more than
  one way; `enc(starts)` in the AAD fixes the split, so every frame that opens has the
  sender's wire form.
- **Why the tail rule.** The interior rule alone is not injective at a NAL's end: a sealed
  body ending `41 00` and one ending `41 00 03` would both go on the wire as `41 00 03`. About
  one NAL in 256 that is not the last in its frame and ends in ciphertext would then fail to
  open, and the last NAL too whenever `L`'s low byte is 0. No tail rule that is independent of
  the layout can be injective and also leave every NAL not ending in `00` unchanged. Under
  this one, a wholly clear NAL whose working form ends in `00` and then `03`s carries one more
  `03` on the wire than the encoder wrote. The receiver removes it before authentication, so
  the AAD and `out` are unaffected; only pre-decryption parsers (the SFU, Chrome's
  depacketizer) see it, after the last bit they read.
- **Codec escape** (opening step 6). `out` is escaped with the codecs' own rule: the interior
  rule, plus an `03` after a final `00 00`. For a frame that met the input rules this returns
  the sender's bytes exactly.
- **What parsers see.** For a conforming input, wire-escaping a NAL changes no clear range's
  unescaped bytes. Only an `03` next to a clear/secret boundary or at a NAL's end can come or
  go. The codec parsers that read clear bytes (the packetizer, the receiver's parameter-set
  tracking and the SFU) all unescape before parsing, so they read exactly the encoder's values.
- **Why this and not retrying.** Escaping is deterministic and spends no CTR. The other
  approach, resealing under a fresh nonce until no emulation appears, fails outright when the
  trailer holds an emulation, because the header and layout bytes do not change between
  attempts. DAVE reseals up to 10 times (libdave `encryptor.cpp`). LiveKit escapes its
  ciphertext and trailer (`writeRbsp`, `src/e2ee/worker/FrameCryptor.ts`).

### SFRAME-10: Codec range rules

`ranges` computes a frame's layout from its codec's structure using `media_bits`. Only the
sender computes one, because the receiver reads the layout from the trailer.

| Codec | Framing | Clear ranges | Who needs them |
| --- | --- | --- | --- |
| Opus | `Ranged(Bare)`, `none()` | none: the whole frame is encrypted | nothing parses an Opus frame before the decoder; the SFU reads audio level from the header extension |
| VP8 | `Ranged(Bare)`, `vp8(frame)` | the first 10 bytes of a keyframe (frame tag, start code, dimensions; RFC 6386 §9.1), the first 3 bytes of an interframe, or the whole frame when shorter | the SFU's and the receiver depacketizer's keyframe and size checks (`media_bits.vp8`, webrtc router) |
| H.264 | `Ranged(AnnexB)`, `h264(frame, l)` after `normalize_h264` | every start code; all of every SPS (type 7) and PPS (type 8); for each slice (types 1 and 5) its NAL header and slice header through `pic_parameter_set_id`; for any other NAL its 1-byte header | the RFC 6184 packetizer, the receiver's SPS/PPS tracking before decryption, and the SFU's keyframe and parameter-set checks |
| H.265 | `Ranged(AnnexB)`, `h265(frame, l)` after `normalize_h265` | every start code; all of every VPS, SPS and PPS (types 32 to 34); for each slice segment (types 0 to 9 and 16 to 21) its 2-byte header and slice header through `slice_pic_parameter_set_id`; for any other NAL its 2-byte header | the RFC 7798 packetizer, the receiver's parameter-set tracking, and the same SFU checks |

- **VP8.** `vp8` reads the frame tag's `P` bit (bit 0 of byte 0, `0` for a keyframe). It is
  total: any frame gives a layout.
- **H.264 and H.265.** `h264` and `h265` run over the working form, using a splitter shared
  by both and kept in `ranges` because it applies the input rules above. They check each
  header with `media_bits.h264.nal` or `media_bits.hevc.nal`. They answer:
  - `Malformed` for a frame that breaks the input rules of [Annex B frames](#annex-b-frames),
    for a NAL shorter than its header, for a slice header that ends before its PPS id, or for
    an H.264 SPS that does not parse through its VUI;
  - `Unstable` for an H.264 SPS that Chrome would rewrite (below);
  - `TooMany` for more than `l.ranges` ranges. Adjacent ranges merge, so a start code, its
    NAL header and a clear slice-header prefix are one range.
- **The slice-header prefix.** The clear prefix is the bytes that hold the slice header's
  `ue(v)` fields up to and including the PPS id:
  - H.264 §7.3.3: `first_mb_in_slice`, `slice_type`, `pic_parameter_set_id`.
  - H.265 §7.3.6.1: `first_slice_segment_in_pic_flag`, `no_output_of_prior_pics_flag` (for
    IRAP types 16 to 23), and `slice_pic_parameter_set_id`.

  The prefix is the whole bytes covering those bits. Its cause is in libwebrtc's H.264
  depacketizer, which runs before the receive transform: it parses the slice header of every
  slice (types 1 and 5, not only IDR) and drops the packet when that parse fails
  (`video_rtp_depacketizer_h264.cc`), and the SPS/PPS tracker drops a keyframe whose PPS id is
  unknown. LiveKit keeps the NAL header and one more byte clear and reseals until the
  encrypted bits happen to read the right id (`FrameCryptor.ts`, LiveKit issue #2180);
  keeping the parsed bits clear removes that retry. Chrome's H.265 depacketizer reads only
  `first_slice_segment_in_pic_flag` and rewrites nothing (`video_rtp_depacketizer_h265.cc`),
  so the H.265 prefix is more than Chrome needs; it is kept for the SFU and leaks nothing
  beyond the parameter sets.
- **H.264 SPS stability.** libwebrtc's H.264 depacketizer runs
  `SpsVuiRewriter::ParseAndRewriteSps(…, kIncoming)` on every SPS and replaces its bytes
  before the receive transform sees the frame (`video_rtp_depacketizer_h264.cc`,
  `common_video/h264/sps_vui_rewriter.cc`). The SPS is clear and in the AAD, so a rewritten
  SPS fails the tag, and every keyframe of the track is `Tampered`. With no colour space
  given, as on receive, it rewrites exactly when:
  - the SPS has no VUI (`vui_parameters_present_flag = 0`);
  - the VUI has no `bitstream_restriction_flag`;
  - or the bitstream restriction has `max_num_reorder_frames ≠ 0` or
    `max_dec_frame_buffering > max_num_ref_frames`.

  A Chrome sender is never hit, because Chrome rewrites its own SPS the same way before its
  send transform (`frame_encode_metadata_writer.cc`, key frames only). A native sender, a
  server-side member or any encoder with B-frames or a bare SPS is hit on every keyframe.
  So:
  - `h264` answers `Unstable` for an SPS meeting any condition.
  - `normalize_h264` rewrites such an SPS as libwebrtc does: it adds a VUI or a bitstream
    restriction, or replaces the restriction's last two values, with
    `max_num_reorder_frames = 0` and `max_dec_frame_buffering = max_num_ref_frames`, and the
    defaults libwebrtc writes for the rest (`motion_vectors_over_pic_boundaries_flag = 1`,
    `max_bytes_per_pic_denom = 2`, `max_bits_per_mb_denom = 1`, both
    `log2_max_mv_length_* = 16`). It then re-escapes the SPS and strips its trailing zero
    bytes. Its output is a fixed point: libwebrtc's incoming pass leaves it unchanged.
  - A rewritten SPS tells the decoder not to reorder, so an encoder that really emits
    B-frames will show them late or out of order at every libwebrtc receiver, encrypted or
    not. A native sender sends no B-frames, as WebRTC encoders do not.
- **Parameter sets are clear.** The receiver's depacketizer keeps SPS and PPS before
  decryption, and the webrtc router's parameter-set check (`media_bits.unit.h264_sent`) reads
  them, so E2EE tracks keep that check. LiveKit keeps them clear too: it leaves every byte
  before the first slice clear. They reveal the profile, level and resolution, which VP8's
  clear keyframe bytes and the RTP stream reveal anyway. SEI, AUD and the other NALs are
  encrypted beyond their headers, which keeps SEI user data private. LiveKit leaves leading SEI
  clear; this text does not.
- **Browser evidence (law G2).** The H.264 rule is checked against Chromium itself:
  a capture script drives headless Chromium through a loopback peer connection with
  `RTCRtpScriptTransform` on both ends, sets each sender frame's data to a given byte string,
  and records what the receiver's transform is handed. A Chrome-to-Chrome capture cannot see
  M1-class failures, because Chrome's own sender has already normalized, so the capture sends
  frames from a non-libwebrtc encoder (`ffmpeg`'s `libx264` with B-frames, whose SPS meets a
  rewrite condition) as well as Chrome's own:
  - Chrome's own H.264 frames, sealed by `oracle.py` with the rules here, arrive unchanged
    and open.
  - The native frames sealed without normalizing arrive with their SPS rewritten and are
    discarded: `Malformed` when the rewrite changes the SPS length, as `libx264`'s does,
    since the trailer's ranges no longer meet the start codes, and `Tampered` otherwise.
    `h264` refuses them as `Unstable`. This is the failure the gate exists to see.
  - The same frames after `normalize_h264` arrive unchanged and open, and `normalize_h264`'s
    SPS equals the SPS Chrome produced from the raw one, trailing zeros aside.
  - If Chrome inserts or drops NALs (for example out-of-band `sprop-parameter-sets`, which
    Hirsel's SDP never signals), the codec's rule is re-gated like B3.
  - H.265 is not captured: headless Chromium 153 offers no H.265 encoder, so no H.265 frame can
    be sent. Its rule rests on G1 and on libwebrtc's H.265 depacketizer, which rewrites
    nothing. A Chromium build with an H.265 encoder adds those cases to G2.
  - The capture (`capture.mjs`, run by `oracle.py g2`) uses Chromium's fake camera, and the
    receiver's transform records frames without decoding them. It never calls
    `generateKeyFrame()`, which stops Chromium 153's encoder; the receiver's keyframe requests
    bring keyframes instead.
- **Deferred.** VP9 and AV1 are not in the first build (slice B3). Neither is known to need
  ranges, and VP9 may need none; the B3 capture decides:
  - libwebrtc's VP9 packetizer takes its descriptor from encoder metadata.
  - AV1's packetizer parses OBU headers and sizes, drops temporal delimiters, and its
    depacketizer is reported to rewrite OBU size fields. If so, a receiver's frame is not the
    sender's byte for byte, and the trailer after the last OBU breaks its size.
  - A browser capture of each round trip decides their rules.
- **Pre-authentication parsing.** The sender's parsers read its own encoder's output. The
  receiver parses only the trailer and, for Annex B, the start codes and escapes. Both are
  linear, bounded by `l.frame` and `l.ranges`, and total.
- **Where the rules live.** In `sframe.ranges`, not in `media_bits`. What to leave clear for
  a packetizer is an E2EE framing decision, while `media_bits` stays a codec parser with no
  E2EE knowledge (decision D4).

### SFRAME-11: RTP per-packet mode, layered later

draft-ietf-avtcore-rtp-sframe-02's per-packet and per-frame modes both use the RFC framing
behind the draft's 1-byte payload descriptor. They need no ranges, and they layer on this
library as slice P1 without changing SFRAME-3 to SFRAME-8.

- **The draft's model.** The RTP payload is a descriptor byte `S E T r r r r r` followed by an
  SFrame fragment, and the payload type names the inner codec.
  - **Per-frame:** one RFC-framed ciphertext of the frame is split across packets. `S` marks
    the first fragment, `E` the last, and `T = 0` says the frame is raw.
  - **Per-packet:** each codec-packetized payload is its own RFC-framed ciphertext, with
    `S = E = 1` and `T = 1`.
  - The SFrame packetizer is codec-agnostic, so nothing needs to be clear. The SFU routes on
    the RTP header and the dependency descriptor (DD).
- **What P1 adds to `sframe`:**
  - `descriptor(s, e, t) -> u8` and its parse. Reserved bits are sent as 0 and ignored on
    receipt.
  - `ssrc_key(base, ssrc, mut out)`, the draft's §7 per-SSRC key
    `HKDF-Expand(HKDF-Extract(be32(ssrc), base_key), "SFrame 1.0 RTP Stream", 32)`, in
    library HKDF. Its output is the base key for SFRAME-4, and the ratchet then runs on it
    (draft §8). v5 withdraws it before it is built: P1 derives the per-SSRC key inside a
    TLS-8 schedule (SFRAME-14).
- **What P1 puts elsewhere:**
  - fragment reassembly (draft §5.2: consecutive sequence numbers from `S` to `E`, with one
    `T` and one payload type) belongs to `rtp`, because it is RTP sequencing;
  - `a=sframe` belongs to webrtc's SDP, which serializes behind the AV1 follow-ups.
- **The SSRC.** The Hirsel SFU rewrites SSRCs per down-track (RTC), so a receiver derives
  `ssrc_key` from the *sender's* SSRC, which signaling gives it, not from the SSRC in the
  packet.
- **Metadata.** The SFU rewrites sequence numbers, SSRCs and some header extensions, so
  per-packet metadata binds only fields that pass through end to end, or nothing.
- **When the ranged framing goes.** Once browsers ship native rtp-sframe, browser tracks move
  to it, and SFRAME-9's ranged framing and SFRAME-10 are deleted with their laws. The RFC
  framing stays.

### SFRAME-12: Supersedes `moq_e2ee`

Wave 2.3's planned `moq_e2ee` ("SFrame-style epoch AES-128-GCM") is withdrawn. MoQ end-to-end
encryption is `sframe`'s RFC framing, one ciphertext per MoQ object.

- **What gets deleted.** `moq_e2ee` exists only as a row in
  `research-1124/realtime-media/PLAN.md` (Wave 2.3), outside the repository. Nothing in the
  repository is deleted; the row is amended to name `sframe`. With two frame formats the stack
  would treat like things differently.
- **What the `moq` spec decides:**
  - which object identifiers it binds as `metadata`, for example track alias, group and
    object ids, which relays must not rewrite;
  - how its epochs map to KIDs: either a fixed KID per epoch and sender, or a ratcheted
    generation per sender.
- `sframe` itself needs no MoQ-specific code.

### SFRAME-13: What the caller does

The caller owns key distribution, KID assignment, the choice of framing per track,
packetization, buffering of `NoKey` frames and every clock. The SFU's E2EE guarantees are the
webrtc package's.

- **Keys.**
  - Distribute base keys over an end-to-end secure channel, such as LiveKit-style
    per-participant keys sent to members over `runner.room` under each member's key
    agreement.
  - Give each sender its own base key or its own KIDs (RFC 9605 §9.2), and issue each sender
    key once, to one run. Core refuses a second `send` import of equal bytes within a run and
    marks each KID's direction by policy, but cannot connect a key with the same key in
    another run, or `base_key[i]` with `base_key[i + 1]` (SFRAME-14).
  - Seal a key that arrives as program bytes into a schedule at once, as
    `sframe_import(receive)` or `sframe_import(send,<kid>,<bits>)`, and name its step as `at`.
  - Treat a granted send key as a resumed key unless its issuer states it is fresh for this
    run, and take its `from` from the reservation store (SFRAME-5, SFRAME-14).
  - Send a new generation when a member leaves.
  - Send each key with the sender's current step and CTR, and install with that CTR as the
    floor (SFRAME-7). With `l.jump` set, the floor must be within `jump` below the sender's
    current CTR, or its frames are `TooFar`.
  - Give a sender and a receiver of one base key the same framing (SFRAME-9).
  - Use a fresh base key per run. A resumed key needs its stored step and ceiling
    (SFRAME-5).
  - Keep a receiver that missed more than `forward` steps alive with `advance` (SFRAME-8).
- **One key per media stream.** Callers MUST give each media stream (camera, screen share,
  microphone) its own `Sender` and KID; the encodings of one simulcast stream or the layers of
  one SVC stream may share one. Nothing in the AAD names the track, so with one key across
  streams:
  - **frames splice:** an SFU can forward camera frames on the screen-share track, and they
    authenticate, so the viewer sees the camera as the shared screen;
  - **the window is in the wrong unit:** one `Receiver` for audio at 50 frames/s and three
    video layers at 30 counts 140 CTRs a second, so `W = 128` is under a second of video, and
    a frame held a second in the jitter path is `TooOld`.

  A deployment that must share a key binds a track id in `metadata`, which removes the splice,
  and sizes `l.window` for the combined rate. RFC 9605 §6.1.1 only ties a frame to its sender
  by KID.
- **Tapes.** A recording of an E2EE member holds every base key it received as program bytes,
  in the `aead_open` that delivered it ([keying](#sframe-end-to-end-media-encryption)). Keep
  such tapes as secret as the media keys themselves. A granted key leaves no copy.
- **Counters.** Count `Discard` reasons and `NoKey` per track. An elevated `Tampered` rate is
  a forgery signal (RFC 9605 §7.5).
- **Metadata.** Use authenticated metadata only after `open` succeeds (RFC 9605 §9.4).
- **What the SFU must guarantee for an E2EE track** (C9b, a separate lane after the AV1
  follow-ups, about size S):
  - route on the RTP header and the DD only;
  - read only the clear ranges of SFRAME-10: VP8's 3 or 10 bytes, and H.264 and H.265 NAL
    headers, parameter sets and slice-header prefixes. The parameter-set check stays;
  - treat RED blocks as opaque.
  - Law: W9's vectors with every byte outside the clear ranges replaced by random bytes route
    identically.

## Keying end state

This section was specified by sframe-import-spec-2696 (v5), and slice E1 builds it. The clean
end state is a TLS-8 SFrame schedule
([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement), SFrame schedules). Base keys,
`sframe_secret`, salts and ratchet outputs never become program bytes, and core guarantees one
sealer per ratchet step and KID. TLS-8 prefers schedules to generic derivation sinks for the
same reasons: fixed labels, fixed output boundaries and one purpose per output. E1 is built
(sframe-import-build-2702), and SFRAME-2 to SFRAME-13 describe it.

### SFRAME-14: Keys from a TLS-8 schedule

`sframe` takes its keys from TLS-8 schedules, and only from them. Core holds each base key and
derives from it. The library keeps its window, its policy for following a ratchet, its
reservations, its volume limit and its framing.

- **Core surface** (TLS-8, SFrame schedules):

  ```hirsel unbuilt
  // module schedule
  pub type SframeSuite = Aes128GcmSha256_128()
  pub fn import_sframe(suite: SframeSuite, take s: Secret) -> Result[Schedule, (ScheduleError, Option[Secret])]
  pub fn sframe_sealer(mut s: Schedule, steps: u64, kid: u64, floor: u64) -> Result[Sealer, ScheduleError]
  pub fn sframe_opener(s: Schedule, ahead: u64, kid: u64) -> Result[Opener, ScheduleError]
  pub fn sframe_ratchet(mut s: Schedule, steps: u64) -> Result[(), ScheduleError]
  ```

- **The byte-keyed API goes.** No sender or receiver takes a `Bytes` base key.
  - `salt_of` is deleted, `ssrc_key` is never built, and the library's key derivation over
    `std.hmac` goes with them.
  - `ratchet_key(base: Bytes) -> Bytes` stays, as a **distribution** helper only. A party
    that hands out a sender key holds it as bytes: it made the key in the run or received it,
    and a key distribution sends bytes. On a join it must hand the joiner the current step's
    key, never step 0's (SFRAME-8). A sender resumed after a crash is imported at its stored
    step, because `sender` issues at the schedule's current step. Both need `base_key[step]`
    as bytes, and no schedule returns bytes. A sender or receiver that resumes does not need
    it: `sender` and `Follow` take `at`, the step its imported bytes are, and core ratchets
    from there (sending and following, below). So the helper is for keys the program made
    or received as bytes, and never a route for a granted key, which cannot be handed to a
    joiner from inside the run at all.
  - **Open: a core key-seal operation** (attack 2699 S7, not built in E1). The end state is
    `sframe_seal_key(s: Schedule, kek: Secret, nonce, aad) -> Bytes`, an AEAD seal of the
    schedule's current base key under a `wrap`-use KEK, computed in core and authorised by
    the import's own policy (for instance `sframe_import(send,<kid>,<bits>,distribute)`, so a
    holder of a send schedule cannot seal the key under a KEK it made and read it back). A
    recipient unwraps it into `sframe_import(receive)` under an opaque KEK, or opens it with
    its own AEAD in a browser. That slice deletes `ratchet_key`, which has no other use once
    `at` exists.
  - No test or interop surface keeps them. A test vector, or a key that arrives as bytes (a
    browser peer's key over `runner.room`), becomes a schedule through
    `secret.seal(bytes, "sframe_import(..)")` and `import_sframe`. That path is two calls, and
    it goes through the same spent mark and ledger as a granted key. `ratchet_key`'s output
    is used the same way.
  - A byte path kept for tests would be a second key path without the guarantees, which is the
    stepping stone /wholehog rules out. The vectors lose nothing by going through the import:
    they check the schedule's derivation, not a library copy of it.
  - `SframeError`'s `Secret(secret.SecretError)` becomes `Schedule(schedule.ScheduleError)`.
- **New signatures.** The other signatures of SFRAME-2 are unchanged.

  ```hirsel unbuilt
  // module sframe, at E1
  pub type Follow { generation: u64, bits: u8, step: u64, at: u64, forward: u8, keep: u8, floor: u64 }
  pub fn sender(suite: Suite, framing: ranges.Framing, scheme: Scheme, take keys: schedule.Schedule, at: u64, from: Mark, l: Limits) -> Result[Sender, (SframeError, schedule.Schedule)]
  pub fn install(mut r: Receiver, kid: u64, keys: schedule.Schedule, floor: u64) -> Result[(), SframeError]
  pub fn install_from(mut r: Receiver, kid: u64, s: Sender, floor: u64) -> Result[(), SframeError]
  pub fn install_ratcheted(mut r: Receiver, f: Follow, take keys: schedule.Schedule) -> Result[(), (SframeError, schedule.Schedule)]
  pub fn advance(mut r: Receiver, bits: u8, generation: u64, step: u64, take keys: schedule.Schedule, at: u64, floor: u64) -> Result[(), (SframeError, schedule.Schedule)]
  ```

  `sframe.opener` is not built: the library never holds an opener it did not issue, so an
  opener of another KID, step or profile cannot be installed (attack 2699 S2).

  Every refusal returns what the call took, as TLS-8's constructors return a share. The
  library's own `Invalid` and `Keys` checks come first. A schedule refusal is
  `Schedule(e)`, and TLS-8 makes each one before issue and with no effect.
- **Sending.**
  - `at` is the ratchet step of the imported bytes: 0 for a grant, a fresh key or a key
    received at step 0, and the step a distributor named for a key it handed on. `sender` is
    `Invalid` when `at` is past the scheme's step (a `Fixed` scheme is step 0), or when the
    distance is over `2^16`. It then issues `sframe_sealer(keys, step − at, kid, from.ctr)`,
    where `kid` is the scheme's current KID, so a sender resumed at a stored step from a
    granted key reaches that step's key in the one call that issues its sealer, and a
    refusal leaves the schedule where it was (attack 2699 M3). The sender then owns the
    schedule and the sealer. A KID outside the import's block is `Schedule(Invalid)`.
  - Within the run, one schedule is owned by one `Sender`, a second import of the same
    designation is `Spent`, and a second `send` import of equal bytes by any route is
    `Spent` from the send-key registry (TLS-8, sender ownership). Equal bytes in another
    run, and bytes one ratchet step apart, stay the issuer's (what stays the caller's,
    below).
  - The sealer's floor is the resumed ceiling's CTR. Core therefore refuses a CTR below a
    stored ceiling even if the library has a bug. Reservations, `Mark` and the `2^34`
    volume limit stay library state (SFRAME-5), because TLS-8 leaves lifetime limits to
    protocols.
  - `ratchet(mut s)` is one `sframe_sealer(keys, 1, next_kid, 0)`. On success the old sealer is
    dropped and the counters restart at `Mark(0, 0)` as in SFRAME-8. On a refusal, which is
    `Schedule(Capacity(..))`, or `Schedule(Invalid)` when the next KID leaves the import's
    block, the sender is unchanged and still seals under the old step (law I1).
  - `seal` passes the header and metadata to `packet.seal_into` as TLS-8's SFrame row says.
    `seal_ranged` builds SFRAME-9's whole AAD at the front of its scratch output, with `extra`
    empty, then scatters as before.
  - `seal_into` admits the AAD scratch, `len(header) + len(metadata)`, under the RFC framing
    with metadata (TLS-8's AAD scratch), and that admission can be refused. A
    `PacketError.Capacity(c)` from `seal_into` is `Capacity(c)` from `seal`, and from
    `open_into` or `open` it is `OpenError.Capacity(c)`. The library advances its CTR,
    volume and window only after the core call succeeds, so such a refusal changes nothing
    and the next seal uses the same CTR (law S1, attack 2699 M4). `Reused`, `Range` and
    `TooLong` are ruled out by the library's checks, so one of them is a failure of the
    library, as the AEAD sink's refusal is today.
  - `install_from(r, kid, s, floor)` installs `sframe_opener(keys, 0, kid)` on the sender's
    schedule. A member that holds a shared group key as `send,<own kid>,0` uses it to open
    the other members' KIDs. It is `Schedule(Invalid)` for a KID inside the block.
- **Receiving fixed KIDs.**
  - `install` issues `sframe_opener(keys, 0, kid)` on the schedule it is lent, a `receive`
    import or any other SFrame schedule, and keeps the opener with the KID's window. A
    schedule of another kind is `Schedule(Invalid)` (TLS-8's kind table), so the KID and the
    profile cannot disagree with the opener.
  - The caller drops the schedule once it has installed its KIDs, which erases the base
    key.
- **Following a ratchet.**
  - `Follow.at` is the step of the imported bytes, as for `sender`; `Invalid` when it is
    past `f.step` or `f.step − at` is over `2^16`. `install_ratcheted` issues the opener for
    `s = f.step` as `sframe_opener(keys, s − at, kid(s))`, which does not move the schedule,
    and only then commits with `sframe_ratchet(keys, s − at)` when that is not 0, which
    cannot fail. A refusal returns the schedule unmoved.
  - A forward candidate at distance `d` is `sframe_opener(keys, d, kid(s + d))`. The
    generation keeps its last candidate, `(d, opener)`, until a commit or `advance` and
    reuses it for the next frame at that `d`, so a stream of forged frames at one `d` costs
    one ratchet run and one key expansion, then one AEAD open a frame (attack 2699 S3). The
    candidate is admitted from the current limit, 1024 bytes, and is not counted in
    `l.keys`.
  - **When the candidate authenticates**, the library issues openers for the intermediate
    steps that the new current step retains, `max(1, d − keep) ≤ j < d`, all or nothing: if
    one is refused with `Capacity(c)`, the ones issued are dropped, the schedule does not
    move, the frame's window is not marked, and `open` answers `Capacity(c)`. The caller
    frees memory and offers the frame again (attack 2699 S1). Then `sframe_ratchet(keys, d)`
    commits, which cannot fail, and the candidate becomes the step's opener.
  - **When the candidate fails**, it is kept for the next frame at that `d`, the schedule
    never moved, and its 1024 bytes stay admitted until a commit, `advance` or removal.
  - **What survives a ratchet** (RFC 9605 §5.1, "deleted promptly"; attack 2699 S5). Once
    the schedule passes a step, that step's base key and PRKs are gone, and so are the
    imported bytes, which the import zeroed (TLS-8, imports). What remains is the
    `sframe_key` and `sframe_salt` of each retained step and KID, in its opener, for up to
    `keep` steps, and for an old generation until `remove_generation`. They open that step's
    frames for that KID and derive no other step or KID. On a CPU without VAES, ring's AEAD
    key of each dropped sealer and opener stays wherever its slot is not reused (TLS-8,
    erasure).
  - `advance` takes `at` as `Follow` does. It issues the new step's opener from the new
    schedule, ratcheted on a copy, before it drops anything; on a refusal it returns the new
    schedule and the generation is unchanged. Then it commits the new schedule's ratchet,
    keeps the retained openers in `[step − keep, step)`, and drops the old schedule, which
    erases its base key. `Keys` is decided on the count after the retained set is trimmed,
    before any issue.
- **Directions and deployments.** The policy, not the call, decides each KID's direction
  (TLS-8, SFrame schedules):
  - **Sender keys** (RFC 9605 §5.1). A sender's own key is `sframe_import(send,<g·2^R>,<R>)`
    with `R` equal to its `Ratcheted` scheme's `bits`, and each receiver holds the key as
    `sframe_import(receive)`. With a smaller block, the first ratchet out of it is
    `Schedule(Invalid)`.
  - **A fixed KID per sender key.** The sender holds `send,<kid>,0`; receivers hold `receive`.
  - **A shared group key with one KID per member.** Each member holds one grant,
    `send,<own kid>,0`, with a `Fixed` sender and `opener` for the other KIDs. The library
    does not ratchet a shared key, because its ratchet is the sender-key ratchet of
    RFC 9605 §5.1. A rekey is a new key.
  - **MLS** (§5.2) stays out of scope (SFRAME-1). An exporter inside a future MLS schedule
    would reach this library as an import grant.
- **Per-stream keys.** Slice P1 derives draft-ietf-avtcore-rtp-sframe-02's per-SSRC key inside
  a TLS-8 schedule, as a child schedule for one SSRC that is then used like any other. P1
  specifies that operation, with a ledger of SSRCs on a send parent (TLS-8, SFrame
  schedules). `ssrc_key` is never built.
- **What stays the caller's** (SFRAME-13 at E1):
  - KID assignment, and issuing each sender its key once, to one run. Core cannot connect
    `base_key[i]` with `base_key[i + 1]`, or a key with the same key in another run.
  - A step for each key: `at` for the bytes, and the stored step for the sender (SFRAME-5).
  - The volume, the reservation store and the framing per key. A granted send key is a
    resumed key unless its issuer states it is fresh for this run: a `file:` or `env:` grant
    is the same bytes in every run. A sender on a granted key takes `from` from the
    reservation store, and `Mark(0, 0)` is for a key the run made or was just handed (attack
    2699 S6).
  - A key that reaches a member as program bytes is sealed into a schedule at once. Sealing
    copies the bytes and zeroes the program's source buffer (TLS-5), and the import zeroes
    the sealed copy. What stays is any other copy the program made, and the recording of
    the `aead_open` that delivered them, so such a tape is still key-bearing. Only a granted
    key leaves no copy in the run.
- **Cost.** Each installed key or retained step holds an `Opener` (1024 bytes) instead of
  48 bytes of `Secret` admission and a salt. A ratcheted generation also holds its schedule
  (512 bytes) instead of a 32-byte base key, and each sender holds its schedule and its sealer
  (1536 bytes and a 16-byte ledger entry). With `l.keys` at its maximum of 256 that is
  256 KiB per receiver, plus one 1024-byte candidate per ratcheted generation, admitted from
  the current limit as each opener is issued. A `send` import admits 48 more for its
  send-key registry entry, held to the run's end. Frame steps are unchanged: a seal or open
  is one core call that takes one step. A ratchet or candidate is one call of at most 16
  ratchet steps (`forward` is at most 16), and a resume is one call of at most `2^16`, where
  the library HKDF ran one call per step.
- **Laws at E1.** These are in the same harness, beside TLS-8's PC12 to PC14, and repeat none
  of them.
  - **V2** becomes C.3 through the import. `00..0f` is sealed as `sframe_import(send,291,0)`,
    and a `Fixed(0x123)` sender from `Mark(0x4567, 0)`, with a reservation, gives `ct`. A
    `receive` import's opener, installed for `0x123`, opens `pt`. One metadata byte changed
    gives `Tampered`. The library-HKDF checks go with the library HKDF.
  - **K1** keeps its check of `ratchet_key(00..0f)` against Python's `fb75d8d5…0f87`. It takes
    its step-1 sender key from those bytes sealed as
    `sframe_import(send,4,2)` (generation 1, `bits = 2`). It takes the receiver's key from
    `00..0f` sealed as `sframe_import(receive)`, installed at step 0 with `forward = 1`. The
    KID sequence and agreement are as before.
  - **K2** gains a case. After a forged candidate at `s + forward` fails, a genuine frame at
    `s + 1` commits and opens, so the schedule did not move. Red when the library ratchets
    the schedule before the tag check.
  - **S1** gains a case. RFC framing with 40 bytes of metadata under
    `bound bytes n { seal(..) }` one byte short of the AAD scratch gives `Capacity`, and the
    next seal outside the bound uses the same CTR. Red when the library advances the CTR
    before `seal_into`.
  - **K1** gains a resume case. A sender over `00..0f` sealed as `sframe_import(send,4,2)`,
    `Ratcheted(Ratchet(1, 2, 3))` and `at = 0`, seals `hirsel` at CTR 0 with KID 7 to Python's
    step-3 frame `70bc317de7760a88b1039fee32a291b1b8933de0c10a30`; a receiver `Follow { step: 3, at: 0, .. }` over a `receive` import of the
    same bytes opens it. `at` past the step is `Invalid`. Red when `sender` ignores `at` (the
    frame is step 0's, under KID 7).
  - **K2** gains the candidate cases. After a forged frame at `s + 2`, a second forged frame
    at `s + 2` is `Tampered` with no new admission (the candidate is reused), and a genuine
    frame at `s + 2` commits. With `keep = 2` and a limit that admits the candidate but not
    the intermediate opener for `s + 1`, the genuine frame at `s + 2` is `Capacity`, the
    schedule did not move (a frame at `s` still opens), and after the limit is raised the
    same frame opens and a frame at `s + 1` opens. Red when an intermediate refusal is
    skipped (the `s + 1` frame is `NoKey`).
  - **I1, ratchet atomicity.** The sender is `Ratcheted(Ratchet(1, 2, 0))` over `00..0f`
    sealed as `sframe_import(send,4,2)`, so its KID is 4. Under
    `bound bytes 1039 { ratchet(s) }` it gives `Schedule(Capacity(..))` with 1040 requested.
    Its next `seal` of `hirsel`, at CTR 0 with empty metadata, is still TLS-8 PC12's step-0
    frame, which is Python `cryptography`'s. A ratchet with 1040 available is followed by
    PC12's step-1 frame (KID 5). Red when the library calls `sframe_ratchet` and then
    `sframe_sealer(keys, 0, ..)` as two calls. The step-0 seal still matches under that bug,
    but the failed attempt has already moved the schedule. The later ratchet then seals KID 5
    under step 2's key, which is not PC12's step-1 frame.

## Consumer contract coverage

| Plan item (C9) | Where |
| --- | --- |
| RFC 9605 header encode/decode | SFRAME-3 |
| Key schedule | SFRAME-4, inside TLS-8 SFrame schedules (SFRAME-14) |
| `AES_128_GCM_SHA256_128`; `AES_256_GCM_SHA512_128` | SFRAME-1: the first is built; the second waits on an HKDF-SHA512 sink |
| Sender ratchet; receiver key table with bounded KIDs | SFRAME-8 |
| `UnencryptedRanges` hook over `media_bits` | SFRAME-9, SFRAME-10 |
| Replay window; TLS-8 opener-window decision | SFRAME-7 |
| `moq_e2ee` superseded | SFRAME-12 |
| Per-packet mode | SFRAME-11 (slice P1) |
| SFU E2EE-safe routing law | SFRAME-13, lane C9b |

## Prior art

- **RFC 9605 and its test vectors**: the C.1 header cases and C.3's suite 0x0004 case, also
  published as JSON in the sframe-wg repository at commit `025d568`.
- **draft-ietf-avtcore-rtp-sframe-02** (January 2026): the payload descriptor, the two modes,
  `a=sframe`, and per-SSRC keys.
- **Discord DAVE** (daveprotocol.com, libdave):
  - codec-aware unencrypted ranges authenticated as AAD, with Opus fully encrypted;
  - an MLS-exported per-sender key with a hash ratchet;
  - a truncated 8-byte tag and a custom footer, which are not taken.
  - It handles H.26x start-code emulation by resealing under a new nonce, up to 10 times
    (libdave `encryptor.cpp`), and leaves H.265 slices clear for the 2-byte header only.
- **LiveKit E2EE** (client-sdk-js FrameCryptor, its native FrameCryptor in webrtc-sdk): it
  keeps VP8's 10 or 3 bytes clear, and for H.264 and H.265 every byte up to two bytes into the
  first slice. It reseals H.264 IDR frames until the PPS id reads right, normalizes 3-byte start
  codes, and escapes ciphertext and trailer with `writeRbsp` (read in `src/e2ee/worker/`
  `FrameCryptor.ts` and `naluUtils.ts` on main, 2026-11-25). Its trailer is LiveKit's own, not SFrame,
  and the SFU strips its `LKTS` trailer (`pkg/sfu/packettrailer/packet_trailer.go`), which
  shows that SFU-side awareness of E2EE framing is needed in practice.
- **Hirsel `srtp`**: the replay window after authentication, local `Discard` reasons, and
  refusals that consume no counter. The ranged framing's local discard names are `srtp`'s.

## Decisions

Each decision gives its reason and how hard it is to reverse.

| # | Decision | Why | Reversal |
| --- | --- | --- | --- |
| D1 | No core change; the base key and salt are program bytes, and the AEAD key sits behind the secret boundary | Required by the brief, and the salt cannot come from a `Secret` | Reversed by D16 when slice E1 lands |
| D2 | The per-KID AEAD key comes from `derive_hkdf_sha256` and not from sealing a library-computed key | It names the sink the brief asks for, gives receivers an `open`-only key, and C.3 checks both HKDF computations against each other | Trivial: one call site |
| D3 | Suite 0x0004 only | TLS-5 has no truncated tag and no HKDF-SHA512 sink | Adding 0x0005 is S plus a core sink |
| D4 | Range rules, the Annex B splitter and the SPS normalizer live in `sframe.ranges`; `media_bits` is unchanged | A clear range is an E2EE packetizer decision, not codec parsing | Moving the rules into `media_bits` is mechanical |
| D5 | Annex B emulation is handled by escaping, not by retrying, with a tail rule that makes the wire escape a bijection and a separate codec escape for the output | Deterministic, wastes no CTR, works when the trailer holds an emulation, and every NAL body has one wire form; `enc(starts)` in the AAD (D15) fixes the split into NALs. The uniform tail rule adds one `03` to a clear NAL ending in `00 03`, which pre-decryption parsers read past; the alternative, a layout-dependent tail rule, makes unescape depend on the trailer | Wire format: fine before the first deployed client, hard after |
| D6 | The trailer is `tag ‖ header ‖ layout ‖ L` and a key has one framing for life | It parses from the end with no magic number, keeps the codec's own leading bytes in place, and stops cross-framing confusion | Wire format: fine before the first deployed client, hard after |
| D7 | H.264 and H.265 keep start codes, parameter sets, NAL headers and each slice header through its PPS id clear; everything else is encrypted | Chrome parses parameter sets and every slice's PPS id before decryption (libwebrtc source), and the SFU keeps its parameter-set check; the PPS-id prefix removes LiveKit's reseal retry. Checked against Chromium by law G2 | A layout change with no wire-format change, since layouts are sent |
| D8 | `NoKey(kid)` is separate from `Discarded(_)`; discard reasons stay local | RFC 9605 §4.4.4 lets the caller buffer a frame with no key and requires a discard otherwise | Trivial |
| D9 | A receiver commits a forward ratchet step only after authentication, with `keep + 1 + forward ≤ 2^bits` | An attacker who picks a KID cannot move the receiver, and every KID's low bits have one meaning | Trivial |
| D10 | A sender volume limit of `2^34` blocks plus frames per key | The AES-GCM confidentiality bound (aead-limits §5.1.1) at about TLS 1.3's margin | A constant |
| D11 | An H.264 SPS Chrome would rewrite is refused (`Unstable`), and `normalize_h264` applies libwebrtc's rewrite before sealing | Keeps the SPS authenticated and the track working with a Chrome receiver. Leaving the SPS out of the AAD instead would let an SFU change the resolution and profile the decoder sees | Moving the SPS out of the AAD is a framing change; adding more normalizations is S |
| D12 | A ratcheted `base` is the key at its step, and `advance` re-keys a following receiver | A joiner never needs step 0's key, construction is one derivation, and a receiver that falls behind recovers without dropping its retained steps | Trivial before deployment |
| D13 | A sender seals only within a reserved ceiling the caller has stored | A per-frame durable write is not practical; a ceiling makes a crash skip counters instead of reusing them, and carries the volume across runs | Removing reservations is trivial; it reopens nonce reuse on resume |
| D14 | Receive floors, `keep ≥ forward`, and an optional `jump` bound, off by default, that applies to every window, anchored at the floor before the first frame | Floors stop replay into a fresh window; `keep ≥ forward` stops one member frame from evicting the real step (two can; only `advance` recovers); `jump` bounds a member's one-frame blackout, of a key before its first frame and of a ratchet step too, at the cost of discarding genuine large gaps and of a floor within `jump` of the sender's CTR, so a deployment opts in | Constants and a default |
| D15 | The ranged AAD binds the NAL start codes as `enc(starts)` (v4) | The working form does not say where NALs begin, so without it an on-path party merges or splits NALs and the frame opens (recheck 2678 B1). Refusing clear `00 00 00 01` instead stops only the split and refuses real SPSs | Wire format: fine before the first deployed client, hard after |
| D16 | Senders and receivers take keys only from TLS-8 SFrame schedules. `take base: Bytes`, `salt_of` and the library key derivation are deleted, and `ssrc_key` is withdrawn, with no byte-keyed test or interop surface left. `ratchet_key` stays as a distribution helper (SFRAME-14) | One key path with one set of guarantees. Bytes become a schedule in one `secret.seal`, and the vectors then check the schedule's derivation, not a library copy of it. A distributor that holds a sender key as bytes still has to hand out the current step's key | S before deployment, since the API is the library's. A byte path back would reopen every v4 keying cost. `ratchet_key` goes when core can seal a schedule's current key for a recipient |
| D17 | The policy fixes direction per KID: `sframe_import(receive)` opens every KID, and `sframe_import(send,<kid>,<bits>)` seals its block and opens the rest | RFC 9605 §4.4.1 marks each stored (KID, base key) entry. A shared group key needs both directions from one grant, because the spent mark cannot connect two grants of one source | Policy text is recorded and granted, so a new spelling is a format change; adding a use is S |
| D18 | A receiver commits a candidate step by `sframe_ratchet` after authentication. Intermediate openers are issued all or nothing, and a refusal is the frame's `Capacity` with nothing moved (v6) | The commit then cannot fail, and a step is never lost to a transient limit: best-effort retention made reordered frames of a skipped step `NoKey` for good, with the caller told nothing (attack 2699 S1) | Trivial |
| D19 | `sender`, `Follow` and `advance` take `at`, the step of the imported bytes, and core ratchets `step − at` in the call that issues the sealer or opener, up to `2^16` | A granted key is step 0's in every run, and a resumed sender at step `n` must reach `base_key[n]`; two calls could leave a moved schedule on a refusal, and a library that cannot see the bytes' step would seal step 0's key under step `n`'s KID (attack 2699 M3) | A parameter; the cap is a constant |
| D20 | One-sender-per-key in a run is a core registry of send-key fingerprints (TLS-8), not only the spent mark | Equal bytes sealed twice are two designations to the mark, and SFRAME-14 makes `seal` of program bytes the normal path for non-granted keys; the registry treats like bytes alike whatever their route (/wholehog), for 48 bytes and one hash per send import (attack 2699 M1) | Removing it reopens the in-run route; the charge is a constant |
| D21 | `install` takes the schedule and issues the opener; there is no `opener` | The library never holds an opener it did not issue, so a wrong KID or profile cannot be installed (attack 2699 S2) | Trivial before deployment |

## Build laws

Each law names the bug it alone catches, its oracle, and a seeded bug that turns it red.
The harness is `core/programs/sframe/laws.hirsel`, with K1, K2 and I1 in
`core/programs/sframe/key_laws.hirsel` and L1, G1, A1 and G2 in
`core/programs/sframe/codec_laws.hirsel` (module `laws`). Every key enters through
`secret.seal` and `import_sframe`. A test imports a given send key once, as the send-key
registry requires, so senders in one test use distinct base keys. Vectors are in
`vectors.hirsel`, generated from the RFC text and the JSON at `025d568`. Ranged-framing vectors
are generated by `oracle.py`, an independent Python reference written from SFRAME-9 over
`cryptography`'s `AESGCM` and the stdlib `hmac`, which never imports Hirsel output.

| Law | Bug it catches | Oracle and case | Seeded bug that goes red |
| --- | --- | --- | --- |
| **V1** header vectors | a wrong length field, KID and CTR in the wrong order, or non-minimal output | All 289 C.1 cases: `encode_header` gives `header`, and `decode_header` gives `kid` and `ctr` with the full length. Hand-made non-minimal headers (`80 05`, `08 00`, `90 00 01`, `81 00 ff`, …) give `Malformed`, and each prefix of a C.1 header gives `Short`. `Short` is checked first: `88 05` announces 3 bytes and is `Short` | `K` set to the length instead of the length minus one |
| **V2** C.3 suite 0x0004 | a wrong label (compressed KID, missing suite, missing space), the salt XORed at the little end, or metadata left out of the AAD | C.3's case through the import: `00..0f` sealed as `sframe_import(send,291,0)`, a `Fixed(0x123)` sender from `Mark(0x4567, 0)` with a reservation, gives `ct`; a `receive` import installed for `0x123` opens `pt`. The same `ct` with one metadata byte changed is `Tampered` | the metadata passed outside the AAD |
| **R1** round trip | a framing, layout, escape or scatter bug that loses or moves bytes | Seeded property: random frames (0 to 64 KiB, heavy in `00` runs), canonical layouts, both framings, `Bare` and `AnnexB`, KIDs and CTRs across header-length edges (7/8, 255/256, `2^56`). `open` of `seal` is the frame, and `Sealed.len` is the bytes appended and at most the bound. Ranged frames also equal `oracle.py`'s bytes for 32 fixed cases | no `03` appended after a NAL's final `00` |
| **T1** tamper rejection | a header, ciphertext, tag, clear byte, layout, `L` or NAL boundary not authenticated, or a start code inside a hidden range accepted | For a sealed frame of each framing with a 3-range layout, every single-bit flip of every byte is `Discarded(_)` or, for a KID bit, `NoKey(_)`, and never `Ok`. Metadata changed by one byte is `Tampered`. For a 4-NAL Annex B frame whose clear range holds a working `00 00 00 01` and whose third NAL ends in ciphertext, every insertion of one `03` at every wire position and every deletion of a wire `03` is refused or opens to exactly the sender's frame (SFRAME-9 opening step 6). A frame whose chosen ciphertext holds `00 00 00 01`, with the wire `03` deleted, is `Malformed` | the clear bytes left out of `metadata′` (a clear-range flip opens); `enc(starts)` left out (a merge opens); the open-side start-code check removed (the hidden start is `Tampered`, not `Malformed`) |
| **T2** layout binding | a layout not in the AAD | A constructed pair: a frame `[c][x]` with layout `{(0,1)}`, sealed under successive CTRs until the ciphertext byte equals `c` (expected 256 tries, deterministic under the fixed seed). The relayout `{(1,1)}` of the same wire bytes is `Tampered` | `enc(layout)` left out of `metadata′` |
| **W1** replay window | a window that marks before authentication, an off-by-one at `W`, a shift that drops bits on a large gap, a floor not applied by `install`, `install_ratcheted` or `advance`, a reinstall over a live key, or `jump` off by one or not applied to an empty window | Reference model, as `srtp`'s `replay_reference_model`: seeded CTR sequences with reorder, duplicates, gaps up to `2^20`, floors and `jump` on and off, and the edges `W − 1`, `W`, `W + 1`, `jump`, `jump + 1`, against a model set "accepted iff unseen, at or above the floor, `ctr > highest − W` and, with `jump`, `ctr ≤ highest + jump`, `highest` being the floor before the first frame". A forged frame at `highest + 1000` followed by a genuine `highest + 1` opens. With `jump 64` and floor 10, a first frame at 75 is `TooFar` and 74 opens. `Follow.floor 4`: CTR 3 is `TooOld`, 4 opens; after `advance` with floor 7, CTR 6 is `TooOld`, 7 opens. A second `install` of a KID is `Invalid` and a frame opened before it is still `Replayed` | the window marked before the tag check |
| **K1** ratchet | a wrong KID mask, `at` ignored, or a resumed sender sealing the wrong step's key | `ratchet_key(00..0f)` is Python `hmac`'s `base_key[1] = fb75d8d5782da6c6cbf18ac43eca5da9e47f7e6ac7926a78e486226bd2af0f87`. A sender at step 1 over those bytes sealed as `sframe_import(send,4,2)` and a receiver over `00..0f` as `sframe_import(receive)`, installed at step 0 with `forward = 1`, agree. A sender with `bits = 2` ratchets five times; its KIDs are `g<<2 \| 1, 2, 3, 0, 1`, and the receiver opens each frame in order. Resume: `Ratcheted(Ratchet(1, 2, 3))` with `at = 0` seals `hirsel` at CTR 0 to Python's step-3 frame `70bc317de7760a88b1039fee32a291b1b8933de0c10a30`, a `Follow { step: 3, at: 0, .. }` opens it, and `at = 4` is `Invalid` for both | `sender` issuing at distance 0 whatever `at` is |
| **K2** forward bounds | a candidate committed before authentication, a forward step past the bound, the jumped-from step evicted, `advance` losing retained steps, a removal whose oversized generation aliases an installed one, a candidate reissued per frame, or an intermediate opener's refusal skipped or reported as `Tampered` | A forged frame at `d = forward` leaves `s` unchanged, and a genuine frame at the old step still opens. `d = forward + 1` is `NoKey`. A genuine member frame at `s + forward` commits and the real step `s` still opens. `remove_generation(4, 3 \| 2^60)` with generation 3 installed at `bits 4` is `false` and step 5 still opens. `install_ratcheted` with `keep < forward` or `keep + 1 + forward > 2^bits` is `Invalid`. A receiver behind by `forward + 3` steps is `NoKey`, then opens after `advance`, and a frame at a step still within `keep` opens too. With `jump 64`, a step-`s + 2` frame at CTR 65 is `TooFar` and leaves `s` current; at CTR 64 it opens. After a forged frame at `s + 2`, a second one under `bound bytes 0` is still `Tampered` (the candidate is reused), and a genuine frame at `s + 2` commits. With `keep = 2` and `bound bytes 2047`, the genuine `s + 2` frame is `Capacity` with 1024 requested and nothing appended; then a frame at `s` opens, the `s + 2` frame opens, and a frame at `s + 1` opens | the candidate committed before the open, or the schedule ratcheted before the tag check (a step passed over does not open); a new candidate per frame; an intermediate refusal skipped (the `s + 1` frame is `NoKey`); an intermediate `Capacity` mapped to `Tampered` |
| **S1** sender counters | a refusal that consumes a CTR, a reused CTR, a seal past the reservation, or exhaustion off by one | `Capacity` from a short spare, then a successful seal with the same `ctr`. A fresh sender is `Unreserved` until `reserve`. A sender resumed from a stored ceiling seals first at that ceiling's CTR, never below. A sender from `Mark(2^64 − 2, 0)` seals once, then `Exhausted`. A volume one block below the limit seals, and one over is `Exhausted`. A sender from `Mark(0, 2^34 + 1)` is `Exhausted` from `reserve` and `seal`. From volume `2^34 − 3`, a 16-byte plaintext (cost 3, counted by hand from SFRAME-5's formula) seals and a 17-byte one (cost 4) is `Exhausted`, under `seal` and under `seal_ranged` with `Bare` and no ranges. RFC framing with 40 bytes of metadata under `bound bytes 40 { seal(..) }`, one byte short of the 41-byte AAD scratch, gives `Capacity` with 41 requested, and the next seal outside the bound uses the same CTR | CTR advanced before the `Capacity` check; the CTR advanced before `seal_into` |
| **L1** ranged lengths | a capacity known only after the AEAD call, an unbounded or mismatched input bound, or an underflow on a short frame | `seal_ranged` with a spare one byte under `wire_bound(AnnexB, n + trailer)` is `Capacity` with that bound as `requested`, and the next seal at the bound uses the same CTR. A frame of all-zero working bytes reaches the bound's order (`3n/2`) on the wire and opens. `open_ranged` refuses, as `Malformed` and with nothing appended, a frame one byte over `wire_bound(l.frame + T(l))`, every frame shorter than 20 bytes, and `L` values 0, 1, `n − 17` and `0xffff`. A spare one byte under the open bound is `Capacity` | the seal's spare checked against the unescaped length |
| **G1** codec ranges | a wrong VP8 keyframe bit, a start code left out, a parameter set encrypted, a PPS id cut by a wrong `ue(v)` length, a wrong H.265 header length, or an SPS Chrome would rewrite accepted | Hand-built frames: a VP8 keyframe and an interframe (RFC 6386 §9.1) give `{(0,10)}` and `{(0,3)}`, and a 2-byte frame gives `{(0,2)}`. An H.264 access unit SPS, PPS, SEI, IDR slice (`first_mb 0`, `slice_type 7`, `pps_id 300`: 1 + 7 + 17 = 25 bits of `ue(v)`, so 4 bytes after the NAL header) gives ranges counted by hand from H.264 §7.3.3 and Exp-Golomb §9.1. The same for H.265 with 2-byte headers. libwebrtc's four `sps_vui_rewriter_unittest.cc` SPS modes (no VUI, no bitstream restriction, suboptimal restriction, optimal) give `Unstable` three times and a layout once. The expected rewrite of the three is `oracle.py`'s construction from its reading of libwebrtc; only G2's libx264 shape is checked against Chromium's own rewrite | the PPS id's `ue(v)` read as a fixed 8 bits |
| **G2** Chromium round trip | a rule that Chrome's depacketizer breaks: an SPS rewritten, a NAL dropped or added, a start code changed, or a slice header that fails libwebrtc's parse | Vectors captured from headless Chromium 153 (loopback, `RTCRtpScriptTransform`, the capture script in the package): Chrome's own H.264 keyframe and delta frames, and `libx264` frames with B-frames, each sealed by `oracle.py` (H.265 is not captured: SFRAME-10). The receiver-side bytes of Chrome's frames and of normalized `libx264` frames equal the sent bytes, equal `seal_ranged` over `h264`'s layout, and open to their plaintext. The raw `libx264` keyframe arrives with its SPS rewritten and is `Malformed`, and `h264` refuses it as `Unstable`. `normalize_h264` of the raw frame equals Chrome's rewrite of it, start codes and trailing zeros aside | `normalize_h264` that keeps `num_reorder_frames` |
| **A1** Annex B escaping | an emulation left in the wire, a boundary taken from the sealed bytes, a non-injective tail rule, or a non-canonical stream accepted | Exhaustive over every body of length 0 to 8 on the alphabet `00 01 02 03 41`: wire unescape of wire escape is the identity, the escaped body never contains `00 00 0x` (`x ≤ 2`) and never ends in `00`, and no two bodies share a wire form. Exhaustive over every wire string of length 0 to 8 on the same alphabet: unescape accepts it exactly when it is the escape of some body. Named cases: a body ending in a single `00`, a clear NAL ending `00 03`, and a trailer with `L = 256` round-trip. A sealed frame whose ciphertext contains `00 00 01` keeps its NAL count. 3-byte start codes and trailing zeros are refused by `seal_ranged` and rewritten by `normalize_h264` | a wire tail rule that appends `03` only after a final `00` |

| **I1** ratchet atomicity | a ratchet that moves the schedule when the new sealer is refused | `Ratcheted(Ratchet(1, 2, 0))` over `00..0f` as `sframe_import(send,4,2)`, KID 4. Under `bound bytes 1039 { ratchet(s) }` it is `Schedule(Capacity(..))` with 1040 requested, and the KID is still 4. The next `seal` of `hirsel` at CTR 0 is TLS-8 PC12's step-0 frame `40bc3483bd855c43d811deeb3085fc86274cff324394b9`, and after a ratchet with 1040 available the next seal is PC12's step-1 frame `501779b9607a5841322c5952a56cb455d5803b545e1d1b` (KID 5), both Python `cryptography`'s | `sframe_ratchet` then `sframe_sealer(keys, 0, ..)` as two calls |

No law repeats TLS-8's PC12 to PC14 schedule vectors. V2 and K1 reach the schedule only through
SFrame's own inputs.

## Cost targets

- **Copies.** One `seal` or `open` makes at most two passes over the frame besides the AEAD
  sink's: gathering the secret bytes, and scattering them, with escaping fused into the
  scatter. Each call reserves its output exactly once (`Capacity` checked before work).
- **Steps.** A seal or open is one core call that takes one step whatever the length. A
  ratchet or candidate is one call of at most 16 ratchet steps, and a resume one call of at
  most `2^16`; key derivation happens only there, never per frame.
- **Memory.** Each installed key or retained step holds an `Opener` (1024 bytes) and its
  window (`W / 8` bytes), and a ratcheted generation also holds its schedule (512 bytes) and
  at most one 1024-byte candidate. A sender holds its schedule and its sealer (1536 bytes and
  a 16-byte ledger entry), and a `send` import admits 48 more for its registry entry until the
  run ends. `l.keys` bounds the table: at 256, 256 KiB per receiver.

## Build slices

| Slice | Size | Depends on | Delivers | Laws |
| --- | --- | --- | --- | --- |
| **B1** core | M | — | `hirsel.toml` (std and `media_bits`), `sframe` module: header, library HKDF, key schedule, `Sender`, `Receiver`, `seal`, `open`, window, ratchet; `vectors.hirsel` from RFC C.1 and C.3 | V1, V2, W1, K1, K2, S1, and R1 and T1 for the RFC framing |
| **B2** ranged | M | B1 | `ranges` module: layout encoding, `seal_ranged`, `open_ranged`, Annex B escaping, `vp8`, `h264`, `h265`, `normalize_h264`, `normalize_h265`; `oracle.py`; the Chromium capture script and its vectors | R1 and T1 for ranged framing, T2, L1, G1, G2, A1 |
| **B3** VP9 and AV1 | S | B2, a browser capture of each codec's round trip through Encoded Transform | their range rules, or `none()` for VP9 if the capture confirms it | G1 cases |
| **P1** rtp-sframe | S | B1, E1, a named consumer (the Hirsel native SDK) | `descriptor`; a TLS-8 child-schedule operation for per-SSRC keys, specified with P1 (SFRAME-14), in place of `ssrc_key`; reassembly in `rtp`; `a=sframe` in webrtc | draft §7 per-SSRC key by Python `hmac` through that operation, and a repeated SSRC on a send parent refused; descriptor round trip |
| **C9b** SFU routing law | S | B2, the AV1 follow-ups | SFRAME-13's routing guarantees in `webrtc_sfu` | the W9 random-payload law |
| **E1** keying end state (built, sframe-import-build-2702) | M | TLS-8 slice T16 | SFRAME-14: `sframe` takes schedules; the byte-keyed API, `salt_of` and the library key derivation are deleted; SFRAME-2 to SFRAME-13, the keying cost, the cost targets and these laws are rewritten to match | V2 and K1 as SFRAME-14 restates them, K2's and S1's added cases, I1 |

B1 and B2 make up this lane's build. Some test reds may ride along when landing unblocks other
work. Compile, lint, spec-fence and docs reds never may. The package ships in the toolchain bundle
as `sframe@1` (sframe-activate-2688).
