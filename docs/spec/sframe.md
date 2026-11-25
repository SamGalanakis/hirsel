# SFrame: end-to-end media encryption

Version note (2026-11-25, sframe-c9a-spec-2642 v2): amended after the crypto attack
(sframe-attack-2649). The Annex B escape is now a bijection (SFRAME-9), H.264 parameter sets
are made stable against Chrome's receive-side SPS rewrite (SFRAME-10), a ratcheted `base` is
the key at its step and a receiver can be advanced (SFRAME-8), ranged capacity and length rules
are bounded before any CTR is spent (SFRAME-9), and sender reservations, receive floors, a
jump bound and the per-track rule close S1 to S5. v1 (same day) was the first text of SFRAME-1
to SFRAME-13, the C9a item of the SOTA video-calls plan. The sections end with
[decisions](#decisions), [build laws](#build-laws), [cost targets](#cost-targets) and
[build slices](#build-slices).

This text defines a new package, `core/programs/sframe/` <!-- doc-ref: planned -->. It is a pure library implementing
[RFC 9605](https://www.rfc-editor.org/rfc/rfc9605) (SFrame) with the cipher suite
`AES_128_GCM_SHA256_128` (0x0004). It covers:

- the SFrame header;
- the key schedule;
- sealing and opening through TLS-5's AEAD sink;
- a replay window;
- the sender-key ratchet of RFC 9605 §5.1;
- a **ranged framing**, so a frame stays parseable by its codec's RTP packetizer.

The ranged framing keeps codec-chosen byte ranges in the clear and authenticates them as AAD.
The package takes keys and frames only. It reaches no `Foreign` code and does no I/O, so
`hirsel test --sim`, recording and replay hold for every program built on it
([FFI-8](foreign.md)).

**Keying cost, read first.** TLS-5's sinks never give a program the bytes of a `Secret`
([TLS-5](tls.md), key operations). RFC 9605's nonce is `sframe_salt XOR CTR`, and the salt is
an HKDF output. So a library that builds the nonce must hold the salt in program bytes. The
salt and the base key it comes from therefore cannot come from a `Secret`. This build makes
no change to core, and the cost is this:

- The **base key is program bytes** (`Bytes`). A base key that reaches the run only as an
  opaque `Secret`, such as an entry grant or a future MLS exporter inside a schedule, cannot
  be used.
- The salt, the ratchet and the per-SSRC derivation run in library HKDF over `std.hmac`.
- Only the per-KID AEAD key lives behind the secret boundary, made with
  `secret.derive_hkdf_sha256` and used with `secret.aead_seal` and `secret.aead_open`
  (SFRAME-4).
- **One sender per key is library state, not a core guarantee.** Core cannot see one base
  key installed in two senders, or in a sender and a receiver, within a run or across runs.
- Program-held key bytes are not zeroed when released, so a ratchet's forward secrecy
  against a later memory read is weaker than a core schedule's. A ratcheted sender and every
  receiver following it hold the current base key for the generation's life, because
  `ratchet` and the forward search need it.
- **The secret boundary gives use-marking, not confidentiality.** `sframe_secret` is computed
  in library code, and the AEAD key is one HKDF-Expand from it. Holding that key behind
  `secret.seal` stops it being used for the wrong purpose; it does not keep it from the
  program.
- **Recordings hold base keys.** A base key reaches a member over `runner.room` or TLS and is
  opened by `aead_open`, and a recording of `aead_open` holds the plaintext it opened
  ([TLS-5](tls.md)). So a tape of an E2EE member holds every base key it received, at rest,
  and anyone who reads the tape can decrypt the recorded media. Treat every such tape as
  key-bearing.
- **RFC 9605 §4.4.1's base-key duties are the caller's.** The RFC says a base key "MUST NOT be
  used for encryption by multiple senders" and that implementations MUST mark each base key
  for encryption or decryption, never both. The library marks only the keys it derives. The
  same bytes passed to `sender` in two processes, or to `sender` and `install` in one, are
  accepted. Two senders on one base key and one KID both start at CTR 0: the same key and
  nonce from the first frame, which gives away the XOR of the plaintexts and GCM's
  authentication key.

[Keying end state](#keying-end-state) names the core change that removes every one of these
costs: a TLS-8 `import_sframe` schedule. The library API keeps its shape, and the base key
changes from `Bytes` to `take Secret`.

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
pub type Suite = Aes128GcmSha256_128
pub fn suite_id(s: Suite) -> u16                     // 0x0004

pub type Header { kid: u64, ctr: u64 }
pub type HeaderError = Short | Malformed
pub fn header_len(h: Header) -> u64                   // 1 to 17
pub fn encode_header(h: Header, mut out: Bytes) -> Result[u64, Capacity]
pub fn decode_header(b: Bytes, at: u64) -> Result[(Header, u64), HeaderError]

pub type Scheme = Fixed(u64) | Ratcheted(Ratchet)
pub type Ratchet { generation: u64, bits: u8, step: u64 }      // base is the key at step
pub type Follow { generation: u64, bits: u8, step: u64, forward: u8, keep: u8, floor: u64 }

pub type Limits { frame: u64, window: u64, keys: u64, ranges: u64, jump: u64 }
pub fn limits() -> Limits                               // 4 MiB, 128, 16, 256, 0 (off)

pub type SframeError = Invalid | Framing | Unreserved | Exhausted | Keys | Secret(secret.SecretError) | Capacity(Capacity)
pub type Discard: key = Malformed | Replayed | TooOld | TooFar | Tampered
pub type OpenError = NoKey(u64) | Discarded(Discard) | Capacity(Capacity)
pub type Sealed { kid: u64, ctr: u64, len: u64 }
pub type Opened { kid: u64, ctr: u64, len: u64 }
pub type Mark { ctr: u64, volume: u64 }

pub type Sender
pub fn sender(suite: Suite, framing: ranges.Framing, scheme: Scheme, take base: Bytes, from: Mark, l: Limits) -> Result[Sender, SframeError]
pub fn kid(s: Sender) -> u64
pub fn next_ctr(s: Sender) -> u64
pub fn reserve(mut s: Sender, frames: u64, volume: u64) -> Result[Mark, SframeError]
pub fn ratchet(mut s: Sender) -> Result[(), SframeError]
pub fn seal(mut s: Sender, metadata: Bytes, plain: Bytes, mut out: Bytes) -> Result[Sealed, SframeError]
pub fn seal_ranged(mut s: Sender, layout: ranges.Layout, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Sealed, SframeError]

pub type Receiver
pub fn receiver(suite: Suite, framing: ranges.Framing, l: Limits) -> Result[Receiver, SframeError]
pub fn install(mut r: Receiver, kid: u64, take base: Bytes, floor: u64) -> Result[(), SframeError]
pub fn install_ratcheted(mut r: Receiver, f: Follow, take base: Bytes) -> Result[(), SframeError]
pub fn advance(mut r: Receiver, bits: u8, generation: u64, step: u64, take base: Bytes, floor: u64) -> Result[(), SframeError]
pub fn remove(mut r: Receiver, kid: u64) -> bool
pub fn remove_generation(mut r: Receiver, bits: u8, generation: u64) -> bool
pub fn open(mut r: Receiver, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Opened, OpenError]
pub fn open_ranged(mut r: Receiver, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Opened, OpenError]

pub fn ssrc_key(base: Bytes, ssrc: u32, mut out: Bytes) -> Result[u64, Capacity]   // SFRAME-11

// module ranges
pub type Escape = Bare | AnnexB
pub type Framing = Rfc | Ranged(Escape)
pub type Range { at: u64, len: u64 }
pub type Layout { ranges: List[Range] }
pub type RangeError = Malformed | TooMany | Unstable | Capacity(Capacity)
pub fn bound(f: Framing, n: u64) -> u64                 // worst wire length of n working bytes
pub fn none() -> Layout
pub fn vp8(frame: Bytes) -> Layout
pub fn h264(frame: Bytes, l: sframe.Limits) -> Result[Layout, RangeError]
pub fn h265(frame: Bytes, l: sframe.Limits) -> Result[Layout, RangeError]
pub fn normalize_h264(frame: Bytes, mut out: Bytes) -> Result[u64, RangeError]   // start codes, trailing zeros, SPS (SFRAME-10)
pub fn normalize_h265(frame: Bytes, mut out: Bytes) -> Result[u64, RangeError]   // start codes, trailing zeros
```

`Limits` is checked when a sender or receiver is made: `frame` 1 to `2^30`, `window` 64 to
4096, `keys` 1 to 256, `ranges` 0 to 1024, `jump` 0 (off) or at least `window`. Anything else
is `Invalid`.

`Sender` and `Receiver` are plain records that own `Secret` values, so they are move-only and
neither `key` nor `codec`. Neither type can be compared, stored or encoded. A sender or
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

For a base key and a KID, the library derives the AEAD key with
`secret.derive_hkdf_sha256` and the 12-byte salt with library HKDF over `std.hmac`, both under
RFC 9605 §4.4.2's labels.

With `kid8` the KID as 8 bytes big-endian and `cs` the suite's id as 2 bytes big-endian
(`00 04`):

```text
sframe_secret = HKDF-Extract(salt = "", ikm = base_key)
sframe_key    = HKDF-Expand(sframe_secret, "SFrame 1.0 Secret key "  ‖ kid8 ‖ cs, 16)
sframe_salt   = HKDF-Expand(sframe_secret, "SFrame 1.0 Secret salt " ‖ kid8 ‖ cs, 12)
```

Each label ends with a space before `kid8`.

- **Base key.** A base key is 16 to 64 bytes; any other length is `Invalid`. RFC 9605 fixes
  no length. Sender keys and ratchet outputs are 32 bytes, an MLS exporter's are `AEAD.Nk`
  (16), and the floor matches TLS-5's HKDF floor.
- **AEAD key.**
  - The library copies the base key into `secret.seal(copy, "kdf")`.
  - It calls `secret.derive_hkdf_sha256(parent, "", key_label, 16, uses)`. `uses` is
    `aead(aes128-gcm)` for a sender and `open(aes128-gcm)` for a receiver.
  - It drops the parent.
  - TLS-5's empty `salt` is the 32 zero bytes of RFC 5869 §2.2, which gives the RFC's
    `HKDF-Extract("", …)`.
  - A receiver's key can only open, so the RFC's "encryption or decryption, never both" holds
    for every key the library derives.
- **Salt.** Library HKDF computes `sframe_secret = hmac.sha256("", base_key)` and the first
  12 bytes of `hmac.sha256(sframe_secret, salt_label ‖ 0x01)`, held as `Inline[u8, 12]`.
- **Both computations** read the same `sframe_secret`. The C.3 vector checks the library's
  value directly and the sink's through the ciphertext (law V2).
- **When derivation happens.** Once per KID:
  - at `sender`, `ratchet` and `install`;
  - in a receiver's forward ratchet, for a candidate step (SFRAME-8).
- **Where the key lives.** Each sending or receiving key holds one child `Secret` (48 bytes
  admitted, TLS-5's `round_up_8(16 + 32)`) and the salt. A `SecretError` from the seal or the
  derivation is `Secret(e)`, and nothing is installed.

Check against the vector (RFC 9605 C.3, suite 0x0004, recomputed with Python `hmac` and
`cryptography` for this text):

- base key `00..0f` and KID `0x123` give `sframe_secret` `d926952c…314c40c0`;
- `sframe_key` is `d34f547f4ca4f9a7447006fe7fcbf768`;
- `sframe_salt` is `75234edefe07819026751816`.

### SFRAME-5: Sealing

A sender seals frame `n` with nonce `sframe_salt XOR be96(ctr)` and AAD
`header ‖ metadata`. It never seals a CTR twice under one key, and a refusal consumes no
CTR.

- **Nonce.** `be96(ctr)` is the CTR as a 12-byte big-endian integer. CTR is at most 64 bits,
  so it lands in the salt's last 8 bytes.
- **`sender(suite, framing, scheme, take base, from, l)`** makes one sending key:
  - `Fixed(kid)` sends under `kid`.
  - `Ratcheted(Ratchet(generation, bits, step))` sends under
    `(generation << bits) | (step mod 2^bits)`. `base` is the key **at** `step`: the sender
    does not ratchet when it is made (SFRAME-8). `bits` is 1 to 16, and `generation` must fit
    in `64 − bits` bits. Anything else is `Invalid`.
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
  - `Exhausted`: the next CTR is `2^64 − 1` or more, or the key's volume would pass its limit
    (below). The key is spent; the caller ratchets or rekeys.
  - `Unreserved`: the next CTR or the volume would pass the ceiling. The caller reserves more.
  - `Capacity(c)`: the spare of `out` is shorter than the result, with the result's length as
    `requested`.
  - Otherwise the AEAD sink seals. On success the CTR advances by one and the volume grows.
    The sink cannot refuse after these checks: the key's length, the nonce's length and the
    plain bound are fixed, and the spare was checked.
- **Volume limit.** Each sending key counts `q + σ`: frames sealed, plus 16-byte blocks of
  plaintext and AAD. It refuses with `Exhausted` past `2^34`, about 256 GiB.
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
rejects replays, opens with the AEAD sink, and only then updates the window. A frame that
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
5. **The AEAD sink opens** with nonce `sframe_salt XOR be96(ctr)` and AAD
   `header ‖ metadata`. When the tag fails the result is `Discarded(Tampered)`. TLS-5's sink
   then appends nothing and releases no unverified plaintext.
6. **On success:** the window marks the CTR, a forward candidate commits (SFRAME-8), and
   `open` answers `Opened(kid, ctr, len)`.

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
  - above `highest + l.jump`, when `l.jump` is not 0 and `highest` is set: `TooFar`;
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
  set, a frame more than `jump` above `highest` is `TooFar` and moves nothing, so a blackout
  needs a stream of forged frames and ends when the forger stops or is removed. The cost is
  that a genuine gap larger than `jump`, such as a sender paused for a long time with its CTR
  advancing, is discarded until the receiver reinstalls. The default is off; a deployment
  that sends gaps of a known size turns it on above them.
- **Scope.** Each KID's key has its own window, because CTR restarts under each new key. A
  ratchet step's window starts empty with floor 0, and removing a key drops its window.
- **Order.** The window is read at step 3 of SFRAME-6 and written at step 6. A forged frame
  with a high CTR therefore never moves `highest` (law W1).

**TLS-8 openers are not used.** A TLS-8 `Opener` is immutable and holds no window ("replay
windows are the protocols'", TLS-8). `sframe` opens through TLS-5's AEAD sink and keeps its
window itself, as `srtp` does. This settles the plan's "TLS-8 opener-window decision": no
TLS-8 change, and the build is not gated on one.

### SFRAME-8: Sender keys and the ratchet

A ratcheted KID is `(generation << R) | (step mod 2^R)`, and the ratchet is RFC 9605 §5.1's
`base_key[i+1] = HKDF-Expand(HKDF-Extract("", base_key[i]), "SFrame 1.0 Ratchet", 32)`. A
receiver follows a bounded number of steps forward and keeps a step only after a frame
authenticates under it.

- **Who decides when.** The library supplies the mechanism and the caller decides when to
  use it. A new receiver joining is a reason to ratchet. A receiver leaving needs a new
  generation, which is a new base key sent out of band (RFC 9605 §5.1).
- **What `base` means.** For `sender`, `install_ratcheted` and `advance`, `base` is the key
  **at** the given `step`. Nobody ratchets at construction, so making a sender or receiver
  costs one derivation whatever `step` is. A joiner is handed the current step's key, never
  step 0's, so it cannot read the generation's earlier media, which is what ratcheting on a
  join is for. A party that keeps `(base, step)` keeps the latest pair.
- **Sending.** `ratchet(mut s)` on a `Ratcheted` sender:
  - replaces the base key with the next one and adds 1 to `step`;
  - derives the new KID's key and salt;
  - restarts the counters at `Mark(0, 0)` under the new key, with the ceiling there too, so
    the caller reserves (SFRAME-5) and stores `(step, ceiling)` before the next seal. A crash
    between `ratchet` and that write resumes the old step from its stored ceiling, which is
    still safe.
  - The old key's `Secret` is released.
  - It is `Invalid` on a `Fixed` sender and `Exhausted` when `step` is `2^64 − 1`. A
    `Secret(e)` or `Capacity` result leaves the sender as it was.
  - Within one generation the KID repeats every `2^R` steps, each time with a different base
    key, so `(base_key, KID, CTR)` stays unique (RFC 9605 §9.1).
- **Following.** `install_ratcheted(mut r, Follow(generation, bits, step, forward, keep, floor), take base)`
  installs a generation whose current step is `step`, with `floor` for that step's window. It
  needs `1 ≤ forward ≤ 16`, `forward ≤ keep ≤ 16` and `keep + 1 + forward ≤ 2^bits`; anything
  else is `Invalid`. The last bound gives every low-bit value at most one meaning. For a frame
  whose KID's high bits name the generation and whose low bits are `low`, with current step
  `s`:
  - If `low` is a retained or the current step's low bits, the frame uses that step's key.
    Retained steps are the up to `keep` steps before `s` that the receiver already holds.
  - Otherwise `d = (low − s) mod 2^bits`. When `1 ≤ d ≤ forward`, the library ratchets a copy
    of the current base key `d` times and derives a **candidate** key. When `d` is larger,
    the frame is `NoKey(kid)`.
  - **Commit.** A candidate that authenticates becomes the current step `s + d`. The
    intermediate steps join the retained set, which keeps only the `keep` steps below the new
    current one. Released steps drop their `Secret` and window.
  - **No commit.** A candidate that fails to authenticate is discarded and changes nothing.
  - **Why `keep ≥ forward`.** Any member can seal a genuine frame at `s + forward` (RFC 9605
    §7.2). If `keep` were smaller, that one commit would release step `s`, and the real
    sender's frames would be `NoKey` until it ratcheted past. With `keep ≥ forward` the step a
    commit jumped from is always retained.
- **Catching up.** `advance(mut r, bits, generation, step, take base, floor)` moves an
  installed generation to a later step with a supplied key: the key distribution's recovery
  path for a receiver that missed more than `forward` steps, or any rekey within a generation.
  Held steps in `[step − keep, step)` stay retained with their windows; older ones are
  released. It is `Invalid` when the generation is not installed or `step` is not above the
  current step, and it derives before it changes anything, so a `Secret(e)` leaves the
  receiver as it was.
- **Cost of a forgery.** An attacker who picks a KID can make one frame cost at most
  `forward` ratchet steps, one `derive_hkdf_sha256` and one AEAD open, all refunded. It can
  never move `s` (law K2).
- **Generations.**
  - A ratcheted generation claims the `2^bits` KIDs `generation << bits` to
    `(generation << bits) | (2^bits − 1)`.
  - `install` of a fixed KID inside a claimed range is `Invalid`, as is `install` of a fixed
    KID already installed and `install_ratcheted` over an installed KID or generation. The
    caller removes first. `remove` then `install` of the same key reopens replay unless the
    new install's floor is above the old window (SFRAME-7).
  - Old generations stay until `remove_generation`, so frames reordered across a rekey still
    open.
  - The table holds at most `l.keys` keys. A fixed key counts one, and a ratcheted generation
    counts `keep + 1` from its install, the most it can ever hold, so commits and `advance`
    never need to evict. An install that would pass `l.keys` refuses with `Keys`.
- **Fixed KIDs.** `install(mut r, kid, take base, floor)` adds one fixed key. Its base key is
  not kept after derivation. KID assignment across senders (RFC 9605 §9.2) is the caller's.
- **Unchecked.** The library cannot tell whether two parties hold the same base key, which is
  the first [keying cost](#sframe-end-to-end-media-encryption).

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

**Lengths.** `bound(f, n)` is the longest wire form of `n` working-form bytes: `n` for `Rfc`
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
     `bound(framing, len(working form) + len(trailer))`, with that bound as `requested`.
     The exact wire length depends on the ciphertext, which does not exist yet, so the check
     is against the bound. Nothing can refuse after the AEAD call, so a refusal never spends a
     CTR and no CTR is ever sealed twice.
2. Seal `secret` as RFC 9605 `encrypt(CTR, KID, metadata′, secret)`, with
   `metadata′ = enc(layout) ‖ clear ‖ metadata`. So the AAD is
   `header ‖ enc(layout) ‖ clear ‖ metadata`. `enc(layout)` determines the length of `clear`,
   so the split is unambiguous.
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

1. `frame` is longer than `bound(framing, l.frame + T(l))`. This bounds every pre-authentication
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
4. `Capacity(c)` when the spare of `out` is shorter than `bound(framing, len(body))`, with
   that bound as `requested`. The output is re-escaped plaintext, whose length is not known
   before authentication.
5. Then apply SFRAME-6 from step 2, to the reconstructed RFC ciphertext
   `header ‖ secret(body) ‖ tag` with `metadata′ = enc(layout) ‖ clear(body) ‖ metadata`.
6. On success, `out` receives the original frame: the clear bytes in place and the opened
   plaintext at the secret positions, and under `AnnexB` escaped with the **codec escape**
   (below). `Opened.len` is the exact length.

**What the binding covers.** The header, the layout and every clear byte are in the AAD. A
change to any of them, or to the ciphertext or the tag, fails the tag. Binding the layout
matters, not only the clear bytes: two layouts can give equal `clear` and equal ciphertext
bytes and still put the opened bytes in different places. Law T2 builds such a pair.

**Framing is a property of the key.** A sender or receiver has one framing for its whole
life. `seal` on a ranged sender and `seal_ranged` on an RFC sender are both `Framing`. So no
key ever authenticates under both AAD shapes, and no frame sealed in one framing is opened in
the other.

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
  outside the escape's image and every sealed frame has exactly one wire form.
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
- **Browser evidence (law G2).** The H.264 and H.265 rules are checked against Chromium itself:
  a capture script drives headless Chromium through a loopback peer connection with
  `RTCRtpScriptTransform` on both ends, sets each sender frame's data to a given byte string,
  and records what the receiver's transform is handed. A Chrome-to-Chrome capture cannot see
  M1-class failures, because Chrome's own sender has already normalized, so the capture sends
  frames from a non-libwebrtc encoder (`ffmpeg`'s `libx264` with B-frames, whose SPS meets a
  rewrite condition) as well as Chrome's own:
  - Chrome's own H.264 and H.265 frames, sealed by `oracle.py` with the rules here, arrive
    unchanged and open.
  - The native frames sealed without normalizing arrive with their SPS rewritten and are
    `Tampered`, and `h264` refuses them as `Unstable`. This is the failure the gate exists to
    see.
  - The same frames after `normalize_h264` arrive unchanged and open, and `normalize_h264`'s
    SPS equals the SPS Chrome produced from the raw one, trailing zeros aside.
  - If Chrome inserts or drops NALs (for example out-of-band `sprop-parameter-sets`, which
    Hirsel's SDP never signals), the codec's rule is re-gated like B3.
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
    (draft §8). Its signature is fixed now; it is built with P1.
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
  - Give each sender its own base key or its own KIDs (RFC 9605 §9.2), and never pass one base
    key to two senders or to a sender and a receiver (RFC 9605 §4.4.1; the library cannot
    check it).
  - Send a new generation when a member leaves.
  - Send each key with the sender's current step and CTR, and install with that CTR as the
    floor (SFRAME-7).
  - Use a fresh base key per run. A resumed key needs its stored ceiling (SFRAME-5).
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
- **Tapes.** A recording of an E2EE member holds its base keys (keying cost). Keep such tapes
  as secret as the media keys themselves.
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

The clean end state is a TLS-8 key schedule for SFrame, so that base keys, salts and ratchet
outputs never become program bytes. It is not built now, and it is named so the build can move
to it without a redesign. This matches TLS-8's own reasoning for schedules over generic
derivation sinks: fixed labels, fixed output boundaries and one purpose per output.

```hirsel unbuilt
// module schedule (TLS-8), the end state
pub fn import_sframe(suite: SframeSuite, take s: Secret) -> Result[Schedule, (ScheduleError, Option[Secret])]
pub fn sframe_ratchet(mut s: Schedule) -> Result[(), ScheduleError]
pub fn sframe_sealer(mut s: Schedule, kid: u64, from: Mark) -> Result[Sealer, ScheduleError]
pub fn sframe_opener(s: Schedule, kid: u64) -> Result[Opener, ScheduleError]
```

- **The import.** An `sframe_import(<dir>)` use, exclusive and spending its bytes like
  `srtp_import`, imports a base key. The schedule derives each KID's key and salt internally
  under SFRAME-4's labels.
- **Sealers.** It issues one sealer per KID from a ledger, which gives core-enforced "one
  sender per key", with nonces built as `salt XOR CTR` inside TLS-8.
- **Ratchet.** It ratchets inside the schedule.
- **Library changes.** `sframe` keeps its window, ratchet-following and framing. Its
  `take base: Bytes` parameters become `take base: Secret`, and the library HKDF over `std.hmac`
  is deleted.
- **A mismatch to resolve in that spec.** TLS-8 fixes direction per imported secret, while
  RFC 9605 fixes it per (base key, KID). A sender-keys deployment gives each party one send
  base key and one receive base key per peer, which fits. A shared-group base key with
  per-member KIDs does not, unless the import names its sending KIDs.
- **Trigger.** Any of: the first consumer whose base key reaches the run only as a `Secret` (an
  entry grant, or MLS); a security review that requires core-enforced single-sender; or a
  requirement that recordings of E2EE members hold no base keys. It is
  its own Opus spec lane with a Fable attack (size M), amending TLS-8.

## Consumer contract coverage

| Plan item (C9) | Where |
| --- | --- |
| RFC 9605 header encode/decode | SFRAME-3 |
| Key schedule over `derive_hkdf_sha256` | SFRAME-4 (AEAD key), with the salt in library HKDF and the cost stated above |
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
| D1 | No core change; the base key and salt are program bytes, and the AEAD key sits behind the secret boundary | Required by the brief, and the salt cannot come from a `Secret` | S once TLS-8 gains `import_sframe` (keying end state); the API keeps its shape |
| D2 | The per-KID AEAD key comes from `derive_hkdf_sha256` and not from sealing a library-computed key | It names the sink the brief asks for, gives receivers an `open`-only key, and C.3 checks both HKDF computations against each other | Trivial: one call site |
| D3 | Suite 0x0004 only | TLS-5 has no truncated tag and no HKDF-SHA512 sink | Adding 0x0005 is S plus a core sink |
| D4 | Range rules, the Annex B splitter and the SPS normalizer live in `sframe.ranges`; `media_bits` is unchanged | A clear range is an E2EE packetizer decision, not codec parsing | Moving the rules into `media_bits` is mechanical |
| D5 | Annex B emulation is handled by escaping, not by retrying, with a tail rule that makes the wire escape a bijection and a separate codec escape for the output | Deterministic, wastes no CTR, works when the trailer holds an emulation, and every sealed frame has one wire form. The uniform tail rule adds one `03` to a clear NAL ending in `00 03`, which pre-decryption parsers read past; the alternative, a layout-dependent tail rule, makes unescape depend on the trailer | Wire format: fine before the first deployed client, hard after |
| D6 | The trailer is `tag ‖ header ‖ layout ‖ L` and a key has one framing for life | It parses from the end with no magic number, keeps the codec's own leading bytes in place, and stops cross-framing confusion | Wire format: fine before the first deployed client, hard after |
| D7 | H.264 and H.265 keep start codes, parameter sets, NAL headers and each slice header through its PPS id clear; everything else is encrypted | Chrome parses parameter sets and every slice's PPS id before decryption (libwebrtc source), and the SFU keeps its parameter-set check; the PPS-id prefix removes LiveKit's reseal retry. Checked against Chromium by law G2 | A layout change with no wire-format change, since layouts are sent |
| D8 | `NoKey(kid)` is separate from `Discarded(_)`; discard reasons stay local | RFC 9605 §4.4.4 lets the caller buffer a frame with no key and requires a discard otherwise | Trivial |
| D9 | A receiver commits a forward ratchet step only after authentication, with `keep + 1 + forward ≤ 2^bits` | An attacker who picks a KID cannot move the receiver, and every KID's low bits have one meaning | Trivial |
| D10 | A sender volume limit of `2^34` blocks plus frames per key | The AES-GCM confidentiality bound (aead-limits §5.1.1) at about TLS 1.3's margin | A constant |
| D11 | An H.264 SPS Chrome would rewrite is refused (`Unstable`), and `normalize_h264` applies libwebrtc's rewrite before sealing | Keeps the SPS authenticated and the track working with a Chrome receiver. Leaving the SPS out of the AAD instead would let an SFU change the resolution and profile the decoder sees | Moving the SPS out of the AAD is a framing change; adding more normalizations is S |
| D12 | A ratcheted `base` is the key at its step, and `advance` re-keys a following receiver | A joiner never needs step 0's key, construction is one derivation, and a receiver that falls behind recovers without dropping its retained steps | Trivial before deployment |
| D13 | A sender seals only within a reserved ceiling the caller has stored | A per-frame durable write is not practical; a ceiling makes a crash skip counters instead of reusing them, and carries the volume across runs | Removing reservations is trivial; it reopens nonce reuse on resume |
| D14 | Receive floors, `keep ≥ forward`, and an optional `jump` bound, off by default | Floors stop replay into a fresh window; `keep ≥ forward` stops one member frame from evicting the real step; `jump` bounds a member's one-frame blackout at the cost of discarding genuine large gaps, so a deployment opts in | Constants and a default |

## Build laws

Each law names the bug it alone catches, its oracle, and a seeded bug that turns it red.
The harness is `core/programs/sframe/laws.hirsel` <!-- doc-ref: planned --> (module `laws`). Vectors are in
`vectors.hirsel`, generated from the RFC text and the JSON at `025d568`. Ranged-framing vectors
are generated by `oracle.py`, an independent Python reference written from SFRAME-9 over
`cryptography`'s `AESGCM` and the stdlib `hmac`, which never imports Hirsel output.

| Law | Bug it catches | Oracle and case | Seeded bug that goes red |
| --- | --- | --- | --- |
| **V1** header vectors | a wrong length field, KID and CTR in the wrong order, or non-minimal output | All 289 C.1 cases: `encode_header` gives `header`, and `decode_header` gives `kid` and `ctr` with the full length. Hand-made non-minimal headers (`80 05`, `08 00`, `90 00 01`, `81 00 ff`, …) give `Malformed`, and each prefix of a C.1 header gives `Short`. `Short` is checked first: `88 05` announces 3 bytes and is `Short` | `K` set to the length instead of the length minus one |
| **V2** C.3 suite 0x0004 | a wrong label (compressed KID, missing suite, missing space), the salt XORed at the little end, metadata left out of the AAD, or the sink and library HKDF disagreeing | C.3's case: library HKDF gives `sframe_secret`, `sframe_salt` and `nonce`; `seal` under `Fixed(0x123)`, from `Mark(0x4567, 0)` with a reservation, gives `ct`; `open` gives `pt`. The same `ct` with one metadata byte changed is `Tampered` | the KID put in the label in its header form |
| **R1** round trip | a framing, layout, escape or scatter bug that loses or moves bytes | Seeded property: random frames (0 to 64 KiB, heavy in `00` runs), canonical layouts, both framings, `Bare` and `AnnexB`, KIDs and CTRs across header-length edges (7/8, 255/256, `2^56`). `open` of `seal` is the frame, and `Sealed.len` is the bytes appended and at most the bound. Ranged frames also equal `oracle.py`'s bytes for 32 fixed cases | no `03` appended after a NAL's final `00` |
| **T1** tamper rejection | a header, ciphertext, tag, clear byte, layout or `L` not authenticated | For a sealed frame of each framing with a 3-range layout, every single-bit flip of every byte is `Discarded(_)` or, for a KID bit, `NoKey(_)`, and never `Ok`. Metadata changed by one byte is `Tampered` | the clear bytes left out of `metadata′` (a clear-range flip opens) |
| **T2** layout binding | a layout not in the AAD | A constructed pair: a frame `[c][x]` with layout `{(0,1)}`, sealed under successive CTRs until the ciphertext byte equals `c` (expected 256 tries, deterministic under the fixed seed). The relayout `{(1,1)}` of the same wire bytes is `Tampered` | `enc(layout)` left out of `metadata′` |
| **W1** replay window | a window that marks before authentication, an off-by-one at `W`, a shift that drops bits on a large gap, a floor not applied, or `jump` off by one | Reference model, as `srtp`'s `replay_reference_model`: seeded CTR sequences with reorder, duplicates, gaps up to `2^20`, floors and `jump` on and off, and the edges `W − 1`, `W`, `W + 1`, `jump`, `jump + 1`, against a model set "accepted iff unseen, at or above the floor, `ctr > highest − W` and, with `jump`, `ctr ≤ highest + jump`". A forged frame at `highest + 1000` followed by a genuine `highest + 1` opens | the window marked before the tag check |
| **K1** ratchet | a wrong ratchet label, length or salt, a wrong KID mask, or `base` taken as step 0's key | Python `hmac` over RFC 9605 §5.1 for base `00..0f` gives `base_key[1] = fb75d8d5782da6c6cbf18ac43eca5da9e47f7e6ac7926a78e486226bd2af0f87`. A sender made at step 1 with that key and a receiver installed at step 0 with `00..0f` and `forward = 1` agree. A sender with `bits = 2` ratchets five times; its KIDs are `g<<2 \| 1, 2, 3, 0, 1`, and the receiver opens each frame in order | `step mod 2^bits` computed as `step mod bits` |
| **K2** forward bounds | a candidate committed before authentication, a forward step past the bound, the jumped-from step evicted, or `advance` losing retained steps | A forged frame at `d = forward` leaves `s` unchanged, and a genuine frame at the old step still opens. `d = forward + 1` is `NoKey`. A genuine member frame at `s + forward` commits and the real step `s` still opens. `install_ratcheted` with `keep < forward` or `keep + 1 + forward > 2^bits` is `Invalid`. A receiver behind by `forward + 3` steps is `NoKey`, then opens after `advance`, and a frame at a step still within `keep` opens too | the candidate committed before the open |
| **S1** sender counters | a refusal that consumes a CTR, a reused CTR, a seal past the reservation, or exhaustion off by one | `Capacity` from a short spare, then a successful seal with the same `ctr`. A fresh sender is `Unreserved` until `reserve`. A sender resumed from a stored ceiling seals first at that ceiling's CTR, never below. A sender from `Mark(2^64 − 2, 0)` seals once, then `Exhausted`. A volume one block below the limit seals, and one over is `Exhausted` | CTR advanced before the `Capacity` check |
| **L1** ranged lengths | a capacity known only after the AEAD call, an unbounded or mismatched input bound, or an underflow on a short frame | `seal_ranged` with a spare one byte under `bound(AnnexB, n + trailer)` is `Capacity` with that bound as `requested`, and the next seal at the bound uses the same CTR. A frame of all-zero working bytes reaches the bound's order (`3n/2`) on the wire and opens. `open_ranged` refuses, as `Malformed` and with nothing appended, a frame one byte over `bound(l.frame + T(l))`, every frame shorter than 20 bytes, and `L` values 0, 1, `n − 17` and `0xffff`. A spare one byte under the open bound is `Capacity` | the seal's spare checked against the unescaped length |
| **G1** codec ranges | a wrong VP8 keyframe bit, a start code left out, a parameter set encrypted, a PPS id cut by a wrong `ue(v)` length, a wrong H.265 header length, or an SPS Chrome would rewrite accepted | Hand-built frames: a VP8 keyframe and an interframe (RFC 6386 §9.1) give `{(0,10)}` and `{(0,3)}`, and a 2-byte frame gives `{(0,2)}`. An H.264 access unit SPS, PPS, SEI, IDR slice (`first_mb 0`, `slice_type 7`, `pps_id 300`: 1 + 7 + 17 = 25 bits of `ue(v)`, so 4 bytes after the NAL header) gives ranges counted by hand from H.264 §7.3.3 and Exp-Golomb §9.1. The same for H.265 with 2-byte headers. libwebrtc's four `sps_vui_rewriter_unittest.cc` SPS modes (no VUI, no bitstream restriction, suboptimal restriction, optimal) give `Unstable` three times and a layout once | the PPS id's `ue(v)` read as a fixed 8 bits |
| **G2** Chromium round trip | a rule that Chrome's depacketizer breaks: an SPS rewritten, a NAL dropped or added, a start code changed, or a slice header that fails libwebrtc's parse | Vectors captured from headless Chromium 153 (loopback, `RTCRtpScriptTransform`, the capture script in the package): Chrome's own H.264 and H.265 keyframes and delta frames, and `libx264` frames with B-frames, each sealed by `oracle.py`. The receiver-side bytes of Chrome's frames and of normalized `libx264` frames equal the sent bytes and open. The raw `libx264` frames arrive changed and are `Tampered`, and `h264` refuses them as `Unstable`. `normalize_h264` of the raw SPS equals Chrome's rewrite of it, trailing zeros aside | `normalize_h264` that leaves `max_dec_frame_buffering` unchanged |
| **A1** Annex B escaping | an emulation left in the wire, a boundary taken from the sealed bytes, a non-injective tail rule, or a non-canonical stream accepted | Exhaustive over every body of length 0 to 8 on the alphabet `00 01 02 03 41`: wire unescape of wire escape is the identity, the escaped body never contains `00 00 0x` (`x ≤ 2`) and never ends in `00`, and no two bodies share a wire form. Exhaustive over every wire string of length 0 to 8 on the same alphabet: unescape accepts it exactly when it is the escape of some body. Named cases: a body ending in a single `00`, a clear NAL ending `00 03`, and a trailer with `L = 256` round-trip. A sealed frame whose ciphertext contains `00 00 01` keeps its NAL count. 3-byte start codes and trailing zeros are refused by `seal_ranged` and rewritten by `normalize_h264` | a wire tail rule that appends `03` only after a final `00` |

No law repeats TLS-5's AEAD or HKDF vectors (AE2, KD laws). V2 and R1 reach the sinks only
through SFrame's own inputs.

## Cost targets

- **Copies.** One `seal` or `open` makes at most two passes over the frame besides the AEAD
  sink's: gathering the secret bytes, and scattering them, with escaping fused into the
  scatter. Each call reserves its output exactly once (`Capacity` checked before work).
- **Steps.** One AEAD sink call takes one step whatever the length (TLS-5). Library HKDF adds
  steps only at key derivation, which happens once per KID or ratchet step, never per frame.
- **Memory.** A fixed key holds 48 bytes of `Secret` admission plus its salt and window
  (`W / 8` bytes), and a ratcheted generation also holds its 32-byte base key. `l.keys`
  bounds the table.

## Build slices

| Slice | Size | Depends on | Delivers | Laws |
| --- | --- | --- | --- | --- |
| **B1** core | M | — | `hirsel.toml` (std and `media_bits`), `sframe` module: header, library HKDF, key schedule, `Sender`, `Receiver`, `seal`, `open`, window, ratchet; `vectors.hirsel` from RFC C.1 and C.3 | V1, V2, W1, K1, K2, S1, and R1 and T1 for the RFC framing |
| **B2** ranged | M | B1 | `ranges` module: layout encoding, `seal_ranged`, `open_ranged`, Annex B escaping, `vp8`, `h264`, `h265`, `normalize_h264`, `normalize_h265`; `oracle.py`; the Chromium capture script and its vectors | R1 and T1 for ranged framing, T2, L1, G1, G2, A1 |
| **B3** VP9 and AV1 | S | B2, a browser capture of each codec's round trip through Encoded Transform | their range rules, or `none()` for VP9 if the capture confirms it | G1 cases |
| **P1** rtp-sframe | S | B1, a named consumer (the Hirsel native SDK) | `descriptor`, `ssrc_key`; reassembly in `rtp`; `a=sframe` in webrtc | draft §7 per-SSRC key by Python `hmac`; descriptor round trip |
| **C9b** SFU routing law | S | B2, the AV1 follow-ups | SFRAME-13's routing guarantees in `webrtc_sfu` | the W9 random-payload law |
| **E1** keying end state | M | a trigger (keying end state) | TLS-8 `import_sframe`, then `sframe` moves to `Secret` base keys | TLS-8 PC laws for the new import |

B1 and B2 make up this lane's build. Some test reds may ride along when landing unblocks other
work. Compile, lint, spec-fence and docs reds never may. The package is not in the toolchain
bundle; it joins when a consumer ships with it.
