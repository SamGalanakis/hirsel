# SFrame: end-to-end media encryption

Version note (2026-11-25, sframe-c9a-spec-2642): first text of SFRAME-1 to SFRAME-13, the
C9a item of the SOTA video-calls plan. Not yet built: every fence is `unbuilt` and every test
path is planned. The sections end with [decisions](#decisions), [build laws](#build-laws),
[cost targets](#cost-targets) and [build slices](#build-slices).

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
  against a later memory read is weaker than a core schedule's.

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
pub type Ratchet { generation: u64, bits: u8, step: u64 }
pub type Follow { generation: u64, bits: u8, step: u64, forward: u8, keep: u8 }

pub type Limits { frame: u64, window: u64, keys: u64, ranges: u64 }
pub fn limits() -> Limits                               // 4 MiB, 128, 16, 256

pub type SframeError = Invalid | Framing | Exhausted | Keys | Secret(secret.SecretError) | Capacity(Capacity)
pub type Discard: key = Malformed | Replayed | TooOld | Tampered
pub type OpenError = NoKey(u64) | Discarded(Discard) | Capacity(Capacity)
pub type Sealed { kid: u64, ctr: u64, len: u64 }
pub type Opened { kid: u64, ctr: u64, len: u64 }

pub type Sender
pub fn sender(suite: Suite, framing: ranges.Framing, scheme: Scheme, take base: Bytes, floor: u64, l: Limits) -> Result[Sender, SframeError]
pub fn kid(s: Sender) -> u64
pub fn next_ctr(s: Sender) -> u64
pub fn ratchet(mut s: Sender) -> Result[(), SframeError]
pub fn seal(mut s: Sender, metadata: Bytes, plain: Bytes, mut out: Bytes) -> Result[Sealed, SframeError]
pub fn seal_ranged(mut s: Sender, layout: ranges.Layout, metadata: Bytes, frame: Bytes, mut out: Bytes) -> Result[Sealed, SframeError]

pub type Receiver
pub fn receiver(suite: Suite, framing: ranges.Framing, l: Limits) -> Receiver
pub fn install(mut r: Receiver, kid: u64, take base: Bytes) -> Result[(), SframeError]
pub fn install_ratcheted(mut r: Receiver, f: Follow, take base: Bytes) -> Result[(), SframeError]
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
pub type RangeError = Malformed | TooMany
pub fn none() -> Layout
pub fn vp8(frame: Bytes) -> Layout
pub fn h264(frame: Bytes, l: sframe.Limits) -> Result[Layout, RangeError]
pub fn h265(frame: Bytes, l: sframe.Limits) -> Result[Layout, RangeError]
pub fn normalize(frame: Bytes, mut out: Bytes) -> Result[u64, Capacity]   // 3-byte start codes to 4
```

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
- **`sender(suite, framing, scheme, take base, floor, l)`** makes one sending key:
  - `Fixed(kid)` sends under `kid`.
  - `Ratcheted(Ratchet(generation, bits, step))` sends under
    `(generation << bits) | (step mod 2^bits)`, after applying the ratchet `step` times to
    `base` (SFRAME-8). `bits` is 1 to 16, and `generation` must fit in `64 − bits` bits.
    Anything else is `Invalid`.
  - The first CTR is `floor`. A sender that resumes a key it used before passes a floor above
    every CTR it sent (RFC 9605 §9.1). `next_ctr` gives the next CTR, so the caller can store
    it before sealing.
- **`seal(mut s, metadata, plain, mut out)`** is the RFC framing. It appends
  `header ‖ ciphertext ‖ tag` to the spare of `out` and answers `Sealed(kid, ctr, len)`. The
  results, in this order, all before the AEAD call, with no state changed on a refusal:
  - `Framing`: the sender's framing is not `Rfc` (for `seal_ranged`, not `Ranged`).
  - `Invalid`: `plain` is longer than `l.frame`.
  - `Exhausted`: the next CTR is past `2^64 − 1`, or the key's volume would pass its limit
    (below).
  - `Capacity(c)`: the spare of `out` is shorter than the result, with the result's length as
    `requested`.
  - Otherwise the AEAD sink seals. On success the CTR advances by one and the volume grows.
    The sink cannot refuse after these checks: the key's length, the nonce's length and the
    plain bound are fixed, and the spare was checked.
- **Volume limit.** Each sending key counts `q + σ`: frames sealed, plus 16-byte blocks of
  plaintext and AAD. It refuses with `Exhausted` past `2^34`, about 256 GiB.
  - Basis: the AES-GCM confidentiality bound of draft-irtf-cfrg-aead-limits
    [section unverified]. Below an attacker advantage of about `2^-60` it allows
    `σ + q ≲ 2^34.5`, the same margin TLS 1.3 takes.
  - At 20 Mbit/s the limit is about 30 hours. The caller ratchets or rekeys before then, so
    a sender exposes no lifetime timer.
- **Simulcast and SVC.** Each layer's frame is its own ciphertext with its own CTR (RFC 9605
  §6.1.2, §6.1.3). A sender shared across layers counts one CTR sequence for all of them.

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
3. **Window check** for a fixed key or a retained or current step: `Discarded(Replayed)` or
   `Discarded(TooOld)` (SFRAME-7). A forward candidate's window is empty, so it passes.
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

Each receiving key keeps an RFC 3711 §3.3.2 window over CTR. It accepts a CTR it has not
accepted before that is above `highest − W`, and it advances only after a frame
authenticates.

- **Size.** `W` is `l.window`, 64 to 4096, default 128. The state is the highest accepted
  CTR and a `W`-bit map below it.
- **First frame.** A key's first authenticated frame sets `highest`, whatever its CTR. A
  frame cannot authenticate without the key, so no floor is needed.
- **Classifying a CTR** against the state:
  - above `highest`: accepted, and the map shifts;
  - in `(highest − W, highest]`: accepted only when its bit is clear, otherwise `Replayed`;
  - at or below `highest − W`: `TooOld`.
- **Gaps** of any size are fine: an SFU that drops a layer or a simulcast encoding leaves
  gaps in CTR, and a jump forward only shifts the map.
- **Scope.** Each KID's key has its own window, because CTR restarts under each new key. A
  ratchet step's window starts empty, and removing a key drops its window.
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
- **Sending.** `ratchet(mut s)` on a `Ratcheted` sender:
  - replaces the base key with the next one and adds 1 to `step`;
  - derives the new KID's key and salt;
  - restarts the CTR at 0 under the new key and resets the volume.
  - The old key's `Secret` is released.
  - It is `Invalid` on a `Fixed` sender. A `Secret(e)` or `Capacity` result leaves the sender
    as it was.
  - Within one generation the KID repeats every `2^R` steps, each time with a different base
    key, so `(base_key, KID, CTR)` stays unique (RFC 9605 §9.1).
- **Following.** `install_ratcheted(mut r, Follow(generation, bits, step, forward, keep), take base)`
  installs a generation at `step`. It needs `1 ≤ forward ≤ 16`, `keep ≤ 16` and
  `keep + 1 + forward ≤ 2^bits`; anything else is `Invalid`. That bound gives every low-bit
  value at most one meaning. For a frame whose KID's high bits name the generation and whose
  low bits are `low`, with current step `s`:
  - If `low` is a retained or the current step's low bits, the frame uses that step's key.
    Retained steps are the up to `keep` steps before `s` that the receiver already holds.
  - Otherwise `d = (low − s) mod 2^bits`. When `1 ≤ d ≤ forward`, the library ratchets a copy
    of the current base key `d` times and derives a **candidate** key. When `d` is larger,
    the frame is `NoKey(kid)`.
  - **Commit.** A candidate that authenticates becomes the current step `s + d`. The
    intermediate steps join the retained set, which keeps only the `keep` steps below the new
    current one. Released steps drop their `Secret` and window.
  - **No commit.** A candidate that fails to authenticate is discarded and changes nothing.
- **Cost of a forgery.** An attacker who picks a KID can make one frame cost at most
  `forward` ratchet steps, one `derive_hkdf_sha256` and one AEAD open, all refunded. It can
  never move `s` (law K2).
- **Generations.**
  - A ratcheted generation claims the `2^bits` KIDs `generation << bits` to
    `(generation << bits) | (2^bits − 1)`.
  - `install` of a fixed KID inside a claimed range is `Invalid`, as is
    `install_ratcheted` over an installed KID or generation.
  - Old generations stay until `remove_generation`, so frames reordered across a rekey still
    open.
  - The table holds at most `l.keys` keys (each fixed key and each held step counts one,
    1 to 256). A full table makes `install` and `install_ratcheted` refuse with `Keys`, and a
    commit drops retained steps first so it never exceeds `l.keys`.
- **Fixed KIDs.** `install(mut r, kid, take base)` adds one fixed key. Its base key is not
  kept after derivation. KID assignment across senders (RFC 9605 §9.2) is the caller's.
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
DAVE use (LiveKit read in source for this text; DAVE [unverified]). RFC 9605 defines no wire format for it, so this
framing is Hirsel's own. It interoperates with Hirsel's JavaScript transform and with other
`sframe` users.

**Layout.** A layout is a list of clear ranges `(at, len)` over the frame. It is
**canonical** when every range has `len ≥ 1`, the ranges are in order and neither overlap nor
touch, the last range ends within the frame, and there are at most `l.ranges` of them. Its
encoding `enc(layout)` is ULEB128 of the range count, then for each range ULEB128 of `gap` and
`len`. `gap` is the distance from the end of the previous range, or from 0 for the first.
Every ULEB128 is minimal, and every value is below `2^32`.

**Sealing.** `seal_ranged(mut s, layout, metadata, frame, mut out)`. Let `clear` be the bytes
of the frame's working form inside the layout's ranges and `secret` the bytes outside them, both
in order. For `Ranged(Bare)` the working form is `frame` itself. For `Ranged(AnnexB)` it is
the **unescaped form**: `frame` with each NAL's emulation-prevention bytes removed (below). The
steps:

1. Refusals before any work, with no state changed:
   - `Framing`: the sender's framing is not `Ranged`;
   - `Invalid`: the layout is not canonical for the working form, or `frame` is longer than
     `l.frame`, or, under `AnnexB`, `frame` breaks a rule of [Annex B frames](#annex-b-frames);
   - then `Exhausted` and `Capacity`, as in SFRAME-5. `Capacity` is checked against the
     escaped length, which the sender computes before sealing.
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
5. Under `AnnexB`, escape the result at the frame's own NAL boundaries (below). The trailer is
   part of the last NAL.

**Opening.** `open_ranged(mut r, metadata, frame, mut out)`:

1. Under `AnnexB`, find the start codes and unescape each NAL. A frame that is not canonically
   escaped is `Discarded(Malformed)`.
2. Read `L` from the last 2 bytes. The header starts `L + 2` bytes from the end. Decode it
   (SFRAME-3), and decode the layout from the rest of those `L` bytes. The layout must end
   exactly at `L` and be canonical for a body of `len − 2 − L − 16` bytes. Under `AnnexB`,
   each start code found in step 1 must lie inside a clear range. The 16 bytes before the
   header are the tag.
3. Any failure in step 2 is `Discarded(Malformed)`.
4. Then apply SFRAME-6 from step 2, to the reconstructed RFC ciphertext
   `header ‖ secret(body) ‖ tag` with `metadata′ = enc(layout) ‖ clear(body) ‖ metadata`.
5. On success, `out` receives the original frame, escaped again under `AnnexB`: the clear
   bytes in place, and the opened plaintext at the secret positions.

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
own emulation prevention (H.264 §7.4.1, H.265 §7.4.2 [section numbers unverified]).

- **Input rules.** `seal_ranged` refuses with `Invalid`:
  - a frame that does not begin with a start code;
  - a 3-byte start code (`00 00 01`), anywhere. WebRTC's H.264 and H.265 depacketizers rebuild
    every NAL with a 4-byte start code whatever the sender sent (verified in LiveKit's
    `normalizeAnnexBStartCodes`, client-sdk-js `src/e2ee/worker/naluUtils.ts`). A 3-byte start
    code inside a clear range would reach the receiver changed and fail the tag.
    `ranges.normalize(frame, mut out)` rewrites them first.
  - a NAL whose body is not canonically escaped. A conforming encoder's output always is,
    because the codecs forbid `00 00 03` before a byte above `0x03`.
  - a layout under which a start code is not inside a clear range.
- **Working form.** Each NAL body (the bytes between its start code and the next) is
  unescaped by removing the `03` of every `00 00 03`. Layout offsets, `clear`, `secret` and
  the AAD are all over this form.
- **Escape.** The sender takes NAL boundaries from the input frame's start codes, never by
  scanning the sealed bytes, which may contain `00 00 01` by chance. In each NAL body it
  inserts:
  - an `03` after every `00 00` that is followed by a byte `≤ 0x03`;
  - an `03` after a final `00`.
- **Unescape.** The receiver splits at `00 00 00 01` and removes those bytes. It refuses:
  - an `00 00 03` that is not followed by a byte `≤ 0x03` or by the NAL's end;
  - an `00 00` followed by a byte `≤ 0x02` inside a NAL;
  - a NAL ending in `00`.

  So escaping is a bijection onto canonical streams, and one sealed frame has one wire form.
- **What parsers see.** For a conforming input, re-escaping a NAL changes no clear range's
  unescaped bytes. Only an `03` next to a clear/secret boundary can come or go. The codec
  parsers that read clear bytes (the packetizer, the receiver's parameter-set tracking and the
  SFU) all unescape before parsing, so they read exactly the encoder's values.
- **Why this and not retrying.** Escaping is deterministic and spends no CTR. The other
  approach, resealing under a fresh nonce until no emulation appears, fails outright when the
  trailer holds an emulation, because the header and layout bytes do not change between
  attempts. LiveKit escapes its ciphertext and trailer the same way (`writeRbsp`, verified in
  `src/e2ee/worker/FrameCryptor.ts`).

### SFRAME-10: Codec range rules

`ranges` computes a frame's layout from its codec's structure using `media_bits`. Only the
sender computes one, because the receiver reads the layout from the trailer.

| Codec | Framing | Clear ranges | Who needs them |
| --- | --- | --- | --- |
| Opus | `Ranged(Bare)`, `none()` | none: the whole frame is encrypted | nothing parses an Opus frame before the decoder; the SFU reads audio level from the header extension |
| VP8 | `Ranged(Bare)`, `vp8(frame)` | the first 10 bytes of a keyframe (frame tag, start code, dimensions; RFC 6386 §9.1), the first 3 bytes of an interframe, or the whole frame when shorter | the SFU's and the receiver depacketizer's keyframe and size checks (`media_bits.vp8`, webrtc router) |
| H.264 | `Ranged(AnnexB)`, `h264(frame, l)` | every start code; all of every SPS (type 7) and PPS (type 8); for each slice (types 1 and 5) its NAL header and slice header through `pic_parameter_set_id`; for any other NAL its 1-byte header | the RFC 6184 packetizer, the receiver's SPS/PPS tracking before decryption, and the SFU's keyframe and parameter-set checks |
| H.265 | `Ranged(AnnexB)`, `h265(frame, l)` | every start code; all of every VPS, SPS and PPS (types 32 to 34); for each slice segment (types 0 to 9 and 16 to 21) its 2-byte header and slice header through `slice_pic_parameter_set_id`; for any other NAL its 2-byte header | the RFC 7798 packetizer, the receiver's parameter-set tracking, and the same SFU checks |

- **VP8.** `vp8` reads the frame tag's `P` bit (bit 0 of byte 0, `0` for a keyframe). It is
  total: any frame gives a layout.
- **H.264 and H.265.** `h264` and `h265` run over the working form, using a new splitter
  shared by both, `media_bits.annexb.next`. They check each header with `media_bits.h264.nal`
  or `media_bits.hevc.nal`. They answer:
  - `Malformed` for a frame that breaks the input rules of [Annex B frames](#annex-b-frames),
    for a NAL shorter than its header, or for a slice header that ends before its PPS id;
  - `TooMany` for more than `l.ranges` ranges. Adjacent ranges merge, so a start code, its
    NAL header and a clear slice-header prefix are one range.
- **The slice-header prefix.** The clear prefix is the bytes that hold the slice header's
  `ue(v)` fields up to and including the PPS id:
  - H.264 §7.3.3: `first_mb_in_slice`, `slice_type`, `pic_parameter_set_id`.
  - H.265 §7.3.6.1: `first_slice_segment_in_pic_flag`, `no_output_of_prior_pics_flag` (for
    IRAP types 16 to 23), and `slice_pic_parameter_set_id`.

  The prefix is the whole bytes covering those bits. Its cause is verified in LiveKit's source:
  Chrome reads `pic_parameter_set_id` from an H.264 IDR slice before the decrypt transform and
  drops the keyframe if it names an unknown PPS (`FrameCryptor.ts`, LiveKit issue #2180).
  LiveKit keeps the NAL header and one more byte clear, and reseals until the encrypted bits
  happen to read the right id. Keeping the parsed bits clear removes that retry. H.265 slices
  get the same prefix: Chrome's H.265 tracking is not checked [unverified], and the prefix
  leaks nothing beyond the parameter sets.
- **Parameter sets are clear.** The receiver's depacketizer keeps SPS and PPS before
  decryption, and the webrtc router's parameter-set check (`media_bits.unit.h264_sent`) reads
  them, so E2EE tracks keep that check. LiveKit keeps them clear too: it leaves every byte
  before the first slice clear. They reveal the profile, level and resolution, which VP8's
  clear keyframe bytes and the RTP stream reveal anyway. SEI, AUD and the other NALs are
  encrypted beyond their headers, which keeps SEI user data private. LiveKit leaves leading SEI
  clear; this text does not.
- **Browser evidence before the build.** The H.264 and H.265 rules are built in B2 only with
  a captured Chrome round trip per codec (sender transform output, then receiver transform
  input, for a keyframe and a delta frame) as a vector, and the B2 build checks it first.
  - The capture must show that the receiver's bytes equal the sender's after 4-byte
    normalization, and that the decoder runs.
  - If Chrome inserts or drops NALs (for example out-of-band `sprop-parameter-sets`, which
    Hirsel's SDP never signals), the codec's rule is re-gated like B3.
- **Deferred.** VP9 and AV1 are not in the first build (slice B3). Neither is known to need
  ranges, and VP9 may need none [unverified]:
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
  - Give each sender its own base key or its own KIDs (RFC 9605 §9.2).
  - Send a new generation when a member leaves.
  - Prefer a fresh base key per run to a resumed CTR floor.
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
pub fn sframe_sealer(mut s: Schedule, kid: u64, floor: u64) -> Result[Sealer, ScheduleError]
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
- **Trigger.** The first consumer whose base key reaches the run only as a `Secret` (an
  entry grant, or MLS), or a security review that requires core-enforced single-sender. It is
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
  - Its handling of H.26x start-code emulation is reported as retry-with-new-nonce
    [unverified].
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
| D4 | Range rules live in `sframe.ranges` over `media_bits` parsers; `media_bits` gains only `annexb.next` | A clear range is an E2EE packetizer decision, not codec parsing | Moving the rules into `media_bits` is mechanical |
| D5 | Annex B emulation is handled by escaping, not by retrying | Deterministic, wastes no CTR, and works when the trailer holds an emulation | Retrying would change the wire form, which is fine before any deployment |
| D6 | The trailer is `tag ‖ header ‖ layout ‖ L` and a key has one framing for life | It parses from the end with no magic number, keeps the codec's own leading bytes in place, and stops cross-framing confusion | Wire format: fine before the first deployed client, hard after |
| D7 | H.264 and H.265 keep start codes, parameter sets, NAL headers and each slice header through its PPS id clear; everything else is encrypted | Chrome parses parameter sets and the IDR slice's PPS id before decryption (LiveKit source), and the SFU keeps its parameter-set check; the PPS-id prefix removes LiveKit's reseal retry. Gated on a Chrome capture in B2 | A layout change with no wire-format change, since layouts are sent |
| D8 | `NoKey(kid)` is separate from `Discarded(_)`; discard reasons stay local | RFC 9605 §4.4.4 lets the caller buffer a frame with no key and requires a discard otherwise | Trivial |
| D9 | A receiver commits a forward ratchet step only after authentication, with `keep + 1 + forward ≤ 2^bits` | An attacker who picks a KID cannot move the receiver, and every KID's low bits have one meaning | Trivial |
| D10 | A sender volume limit of `2^34` blocks plus frames per key | The AES-GCM confidentiality bound at about TLS 1.3's margin | A constant |

## Build laws

Each law names the bug it alone catches, its oracle, and a seeded bug that turns it red.
The harness is `core/programs/sframe/laws.hirsel` <!-- doc-ref: planned --> (module `laws`). Vectors are in
`vectors.hirsel`, generated from the RFC text and the JSON at `025d568`. Ranged-framing vectors
are generated by `oracle.py`, an independent Python reference written from SFRAME-9 over
`cryptography`'s `AESGCM` and the stdlib `hmac`, which never imports Hirsel output.

| Law | Bug it catches | Oracle and case | Seeded bug that goes red |
| --- | --- | --- | --- |
| **V1** header vectors | a wrong length field, KID and CTR in the wrong order, or non-minimal output | All 289 C.1 cases: `encode_header` gives `header`, and `decode_header` gives `kid` and `ctr` with the full length. Hand-made non-minimal headers (`80 05`, `08 00`, `90 00 01`, `81 00 ff`, …) give `Malformed`, and each prefix of a C.1 header gives `Short`. `Short` is checked first: `88 05` announces 3 bytes and is `Short` | `K` set to the length instead of the length minus one |
| **V2** C.3 suite 0x0004 | a wrong label (compressed KID, missing suite, missing space), the salt XORed at the little end, metadata left out of the AAD, or the sink and library HKDF disagreeing | C.3's case: library HKDF gives `sframe_secret`, `sframe_salt` and `nonce`; `seal` under `Fixed(0x123)`, floor `0x4567`, gives `ct`; `open` gives `pt`. The same `ct` with one metadata byte changed is `Tampered` | the KID put in the label in its header form |
| **R1** round trip | a framing, layout, escape or scatter bug that loses or moves bytes | Seeded property: random frames (0 to 64 KiB, heavy in `00` runs), canonical layouts, both framings, `Bare` and `AnnexB`, KIDs and CTRs across header-length edges (7/8, 255/256, `2^56`). `open` of `seal` is the frame, under the identity invariant. Ranged frames also equal `oracle.py`'s bytes for 32 fixed cases | no `03` appended after a NAL's final `00` |
| **T1** tamper rejection | a header, ciphertext, tag, clear byte, layout or `L` not authenticated | For a sealed frame of each framing with a 3-range layout, every single-bit flip of every byte is `Discarded(_)` or, for a KID bit, `NoKey(_)`, and never `Ok`. Metadata changed by one byte is `Tampered` | the clear bytes left out of `metadata′` (a clear-range flip opens) |
| **T2** layout binding | a layout not in the AAD | A constructed pair: a frame `[c][x]` with layout `{(0,1)}`, sealed under successive CTRs until the ciphertext byte equals `c` (expected 256 tries, deterministic under the fixed seed). The relayout `{(1,1)}` of the same wire bytes is `Tampered` | `enc(layout)` left out of `metadata′` |
| **W1** replay window | a window that marks before authentication, an off-by-one at `W`, or a shift that drops bits on a large gap | Reference model, as `srtp`'s `replay_reference_model`: seeded CTR sequences with reorder, duplicates, gaps up to `2^20` and the edges `W − 1`, `W`, `W + 1`, against a model set "accepted iff unseen and `ctr > highest − W`". A forged frame at `highest + 1000` followed by a genuine `highest + 1` opens | the window marked before the tag check |
| **K1** ratchet | a wrong ratchet label, length or salt, or a wrong KID mask | Python `hmac` over RFC 9605 §5.1 for base `00..0f` gives `base_key[1] = fb75d8d5782da6c6cbf18ac43eca5da9e47f7e6ac7926a78e486226bd2af0f87`. A sender with `bits = 2` ratchets five times; its KIDs are `g<<2 | 1, 2, 3, 0, 1`, and a receiver with `forward = 1`, `keep = 2` opens each frame in order | `step mod 2^bits` computed as `step mod bits` |
| **K2** forward bounds | a candidate committed before authentication, or a forward step past the bound | A forged frame at `d = forward` leaves `s` unchanged, and a genuine frame at the old step still opens. `d = forward + 1` is `NoKey`. `install_ratcheted` with `keep + 1 + forward > 2^bits` is `Invalid` | the candidate committed before the open |
| **S1** sender counters | a refusal that consumes a CTR, a reused CTR, or exhaustion off by one | `Capacity` from a short spare, then a successful seal with the same `ctr`. A sender at floor `2^64 − 1` seals once, then `Exhausted`. A volume one block below the limit seals, and one over is `Exhausted` | CTR advanced before the `Capacity` check |
| **G1** codec ranges | a wrong VP8 keyframe bit, a start code left out, a parameter set encrypted, a PPS id cut by a wrong `ue(v)` length, or a wrong H.265 header length | Hand-built frames: a VP8 keyframe and an interframe (RFC 6386 §9.1) give `{(0,10)}` and `{(0,3)}`, and a 2-byte frame gives `{(0,2)}`. An H.264 access unit SPS, PPS, SEI, IDR slice (`first_mb 0`, `slice_type 7`, `pps_id 300`: 1 + 7 + 17 = 25 bits of `ue(v)`, so 4 bytes after the NAL header) gives ranges counted by hand from H.264 §7.3.3 and Exp-Golomb §9.1. The same for H.265 with 2-byte headers. The B2 Chrome captures open | the PPS id's `ue(v)` read as a fixed 8 bits |
| **A1** Annex B escaping | an emulation left in the wire, a boundary taken from the sealed bytes, or a non-canonical stream accepted | Seeded property: unescape of escape is the identity, and the escaped body never contains `00 00 0x` (`x ≤ 2`) outside start codes or ends in `00` (the H.264 §7.4.1 invariants). A sealed frame whose ciphertext contains `00 00 01` keeps its NAL count. Each hand-made non-canonical stream (`00 00 03 04`, a final `00`, `00 00 02`) is `Malformed`. 3-byte start codes are refused by `seal_ranged` and rewritten by `normalize` | NAL boundaries found by scanning the sealed body |

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
| **B2** ranged | M | B1 | `ranges` module: layout encoding, `seal_ranged`, `open_ranged`, Annex B escaping, `vp8`, `h264`, `h265`; `media_bits.annexb.next`; `oracle.py` | R1 and T1 for ranged framing, T2, G1, A1; the H.264 and H.265 Chrome capture vectors |
| **B3** VP9 and AV1 | S | B2, a browser capture of each codec's round trip through Encoded Transform | their range rules, or `none()` for VP9 if the capture confirms it | G1 cases |
| **P1** rtp-sframe | S | B1, a named consumer (the Hirsel native SDK) | `descriptor`, `ssrc_key`; reassembly in `rtp`; `a=sframe` in webrtc | draft §7 per-SSRC key by Python `hmac`; descriptor round trip |
| **C9b** SFU routing law | S | B2, the AV1 follow-ups | SFRAME-13's routing guarantees in `webrtc_sfu` | the W9 random-payload law |
| **E1** keying end state | M | a trigger (keying end state) | TLS-8 `import_sframe`, then `sframe` moves to `Secret` base keys | TLS-8 PC laws for the new import |

B1 and B2 make up this lane's build. Some test reds may ride along when landing unblocks other
work. Compile, lint, spec-fence and docs reds never may. The new package joins the toolchain
bundle after 2627 lands.
