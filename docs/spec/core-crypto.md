# Core cryptography

## SHA-512

The [core blocks rule](core.md#contracts-the-signatures-do-not-state) defines the
SHA-512 kernel. `std.sha512` and `std.sha384` implement the hashes over it.

### SHA-512 laws

The CAVP files are `SHA384ShortMsg.rsp`, `SHA384LongMsg.rsp`, `SHA512ShortMsg.rsp` and
`SHA512LongMsg.rsp` in [shabytetestvectors.zip](https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/shs/shabytetestvectors.zip),
copied byte for byte.

| law | bug | oracle and setup |
|---|---|---|
| **H1** CAVP | a wrong round constant, rotation or initial word, or SHA-384 returning the wrong 48 bytes | Every record of the four files: `sha512.hash` or `sha384.hash` of `Msg` (the first `Len / 8` bytes) equals `MD`. Also check the published NIST `abc` and 896-bit two-block examples for both hashes. |
| **H2** padding and streaming | a wrong pad at the 112-byte boundary, a 64-bit length field in the 128-bit slot, or a pending block lost between `add` calls | For every length 0 to 300 of the bytes `i mod 251`, and every split of the input into two `add` calls, `sha512.hash` and `sha384.hash` equal Python `hashlib`'s `sha512` and `sha384` digests, which the test reads from a checked-in table that `hashlib` generated, not from this build. A million `a` bytes streamed 1000 bytes per `add` give the million-`a` digests published with FIPS 180-2's SHA-384 and SHA-512 examples. |
| **H3** `finish` keeps the state | `finish` mutating the state it was given | After `finish` on the first 100 bytes, adding the remaining 200 and finishing equals the one-shot digest of all 300, for both hashes. |
| **H4** steps grow by the work-step formula only | a charge per block, no charge for bulk bytes, or `add` compressing block by block in user code | On the reference evaluator, let `k` be the least bound under which `bound steps k { let mut s = take st; sha512.add(mut s, b, 0, list.len(b)); s }` succeeds for a 256-byte `b`, with `st` from `start()` and `b` bound outside the scope. For a 1 MiB `b` (8192 blocks, 32768 work steps where the 256-byte `b` takes 8) the same scope fails under `k + 32759` with `OutOfTime` and succeeds under `k + 32760`. The invariant is MEM-30's: `add` makes one `blocks` call over all its whole blocks, whose work steps are `ceil(bytes / 32)` by the SHA-512 row of the work table, and has no loop over blocks. |

The laws are cases in [sha512.hirsel](../../core/std/tests/sha512.hirsel), run by the shared walker with
`--backend all`.

## Signature verification

The modules `ed25519`, `p256`, `p384` and `rsa` check signatures. The curve modules
provide functions of type `(key: Bytes, msg: Bytes, sig: Bytes) -> bool`: `ed25519.verify`,
`p256.verify`, `p256.verify_der`, `p384.verify` and `p384.verify_der`. The `rsa` module
takes the key as its two integers, `(n: Bytes, e: Bytes, msg: Bytes, sig: Bytes) -> bool`:
`rsa.verify_pkcs1_sha256` and `rsa.verify_pss_sha256`. Each returns `true` exactly when
`sig` is a valid signature of all of `msg` under the key by the rule below, and `false`
otherwise. Every input that is not well formed gives `false`, including a key or signature
of the wrong length, and no input makes the call fail. The answer does not say which check
failed. That is how ring reports it, and a caller checking untrusted bytes
needs no failure path for them. Together these verify every signature an OpenID Connect
provider issues under the algorithms JWS registers for it: `RS256`, `PS256`, `ES256`,
`ES384` and `EdDSA` with Ed25519 ([RFC 7518 §3.1](https://www.rfc-editor.org/rfc/rfc7518#section-3.1),
[RFC 8037](https://www.rfc-editor.org/rfc/rfc8037)).
Signing is `secret.sign` ([TLS-5](tls.md#tls-5-secrets)), and ephemeral key agreement is
`secret.key_share`, consumed by a `schedule` constructor ([TLS-8](tls.md#tls-8-packet-ciphers-and-key-agreement)):
both keep the private key in a `Secret` or a `KeyShare`, never in program bytes, and the
agreed value never leaves the schedule.

- **Ed25519.** `ed25519.verify` is pure Ed25519 ([RFC 8032 §5.1.7](https://www.rfc-editor.org/rfc/rfc8032#section-5.1.7)),
  without context or prehash. `key` is the 32-byte encoding of the point `A`, and `sig` is
  64 bytes `R || S`. It returns `false` unless all of these hold:
  - `S`, read as a little-endian integer, is below the group order `L`. It is not reduced.
  - `key` decodes to a point. `y` is the low 255 bits taken mod `p`, so an encoding of
    `y ≥ p` is accepted. `x` is the root that the top bit selects, and `false` results when
    there is none. When `x = 0` the top bit is ignored.
  - The encoding of `[S]B − [k]A` equals the 32 bytes of `R`, where
    `k = SHA-512(R || key || msg) mod L` hashes `R` and `key` exactly as given.

  This is the cofactorless equation. `R` is never decoded, so a non-canonical or
  off-curve `R` never matches. `A`'s order is not checked, so small-order and mixed-order
  keys are accepted whenever the equation holds. That is ring's `ED25519`, and RFC 8032
  permits it.
- **P-256.** `p256.verify` and `p256.verify_der` are ECDSA over NIST P-256 with SHA-256
  ([FIPS 186-5 §6.4.2](https://csrc.nist.gov/pubs/fips/186-5/final)). The kernel hashes
  `msg`, and `e` is its SHA-256 digest mod `n`.
  - `key` is an uncompressed SEC 1 point: 65 bytes `0x04 || X || Y` with `X` and `Y` below
    `p`, on the curve. A compressed (`0x02`, `0x03`) or hybrid (`0x06`, `0x07`) point, a
    bare 64-byte `X || Y` and the one-byte point at infinity give `false`. ring parses only
    the uncompressed form, and JWK and COSE keys carry `X` and `Y` whole.
  - `p256.verify` takes `sig` as 64 bytes `r || s`, each 32 bytes big-endian. That is the
    IEEE P1363 form that JWS `ES256` uses ([RFC 7518 §3.4](https://www.rfc-editor.org/rfc/rfc7518#section-3.4)).
  - `p256.verify_der` takes the DER `ECDSA-Sig-Value`, `SEQUENCE { r INTEGER, s INTEGER }`
    ([RFC 3279 §2.2.3](https://www.rfc-editor.org/rfc/rfc3279#section-2.2.3)). That is the
    form X.509, WebAuthn and most package signatures use. Only strict DER is accepted:
    minimal definite lengths, minimal non-negative integers and no trailing bytes. Any BER
    variation gives `false`.
  - Both forms need `1 ≤ r, s < n`. The result is `true` exactly when
    `u1·G + u2·Q` is not the point at infinity and its `x` is `r` mod `n`, where
    `w = s⁻¹ mod n`, `u1 = e·w` and `u2 = r·w`. A high `s` (`s > n/2`) is accepted, as
    FIPS 186-5 accepts it, so `(r, s)` and `(r, n − s)` both verify. A caller that needs one
    signature per message must enforce that itself.

  Both forms are kernels because the conversion between them is where malleability bugs
  live: Wycheproof's BER cases test exactly that conversion. ring parses strict DER itself
  (`ECDSA_P256_SHA256_ASN1`), and its `r || s` path is `ECDSA_P256_SHA256_FIXED`.
- **P-384.** `p384.verify` and `p384.verify_der` are the P-256 rule over NIST P-384 with
  SHA-384, word for word, with these sizes: `e` is the SHA-384 digest mod `n`, `key` is 97
  bytes `0x04 || X || Y`, and `p384.verify`'s `sig` is 96 bytes `r || s`, each 48 bytes
  big-endian, the JWS `ES384` form. The same forms give `false` (compressed, hybrid, bare
  96-byte and infinity keys), only strict DER is accepted, and a high `s` is accepted. The
  kernel is ring 0.17.14's `ECDSA_P384_SHA384_FIXED` and `ECDSA_P384_SHA384_ASN1` through
  `UnparsedPublicKey::verify`, as P-256's is. ring parses only the uncompressed point
  (`parse_uncompressed_point` refuses any first byte but `0x04` and any other length), and
  hashes `msg` with its own SHA-384, so no digest truncation rule applies: SHA-384's output
  is the curve's size.
- **RSA.** `n` and `e` are unsigned big-endian integers: a JWK's `n` and `e` after base64url
  decoding ([RFC 7518 §6.3.1](https://www.rfc-editor.org/rfc/rfc7518#section-6.3.1)), or
  the two fields of an X.509 `RSAPublicKey`. Leading zero bytes in either are ignored, as
  their values are what is specified. Each function returns `false` unless all of these hold:
  - `n` is odd, and its form without leading zero bytes is 256 to 1024 bytes long, so it
    has 2041 to 8192 significant bits. That is ring's `2048_8192` parameter set exactly:
    ring compares the modulus's length rounded up to whole bytes with 2048 bits, and its
    exact bit length with 8192. A smaller modulus is not a key a verifier should trust, and
    8192 bounds the work.
  - `e` is odd and `3 ≤ e < 2^33`, ring's range.
  - `sig` has exactly `k` bytes, `k` being the byte length of `n`, and read as an integer it
    is at least 1 and below `n` ([RFC 8017 §8.2.2](https://www.rfc-editor.org/rfc/rfc8017#section-8.2.2)
    steps 1 and 2a; a zero signature never verifies, and ring refuses it before the
    exponentiation). A shorter signature is not padded with zeros, and `n` is not
    subtracted from a larger one.

  `rsa.verify_pkcs1_sha256` is RSASSA-PKCS1-v1_5 with SHA-256 (JWS `RS256`). The encoded
  message `sig^e mod n` must equal, byte for byte, the EMSA-PKCS1-v1_5 encoding of `msg`'s
  SHA-256 digest ([RFC 8017 §9.2](https://www.rfc-editor.org/rfc/rfc8017#section-9.2)),
  whose `DigestInfo` carries the SHA-256 OID with a NULL parameter. The check encodes and
  compares; it never parses the padding, so no BER variant, no garbage after the digest and
  no `DigestInfo` without its NULL is accepted, though RFC 8017's note permits the last.
  `rsa.verify_pss_sha256` is RSASSA-PSS with SHA-256, MGF1 with SHA-256 and a salt of
  exactly 32 bytes (JWS `PS256`, which fixes the salt to the hash length, [RFC 7518 §3.5](https://www.rfc-editor.org/rfc/rfc7518#section-3.5)),
  verified by EMSA-PSS-VERIFY ([RFC 8017 §9.1.2](https://www.rfc-editor.org/rfc/rfc8017#section-9.1.2)):
  trailer `0xbc`, the bits above `emBits` zero, the padding `PS` zero then `0x01`, and `H`
  equal to the recomputed hash. The salt length is not detected from the signature, so a
  signature made with another salt length gives `false`.

  The kernel is ring 0.17.14's `RsaPublicKeyComponents { n, e }.verify` with
  `RSA_PKCS1_2048_8192_SHA256` or `RSA_PSS_2048_8192_SHA256`. ring refuses a leading zero
  byte in `n` or `e`, so the wrapper strips them first; an `e` of only zero bytes is then
  empty, which ring refuses. ring checks the rest of the rules above itself: the modulus
  size, odd `n`, `e`'s range and parity, `sig`'s length and range, and, for PKCS#1 v1.5,
  encode-and-compare (`pkcs1_encode`, then a byte comparison). ring's PSS takes the salt
  length from the hash: its `PSSMetrics` sets `sLen = hLen`, 32 bytes for SHA-256, which is
  the fixed 32-byte salt this rule and `PS256` require, so the rule needs no salt argument
  and no check of the wrapper's own. MGF1 uses the same hash. Verification uses only public
  values.
- **Charges.** Each call admits no bytes and takes one step plus its work steps
  ([MEM-30](budgets.md#mem-30-step-bounds)): the fixed weight of its algorithm and one step
  for each quantum, or part, of `msg`, both from MEM-30's work table. A weight
  covers the largest key the rules above accept, 8192 bits for RSA, so the steps bound the
  work whatever the key.
- **Determinism.** The answer is a function of the input byte strings alone. Verification
  is not a host or provider operation, and nothing about it is recorded: replay computes it
  again ([recording](../recording.md#what-a-recording-holds)). An engine may use ring's
  CPU-specific code. Every path gives the same answer, so by
  [SEM-8](semantics.md#sem-8-determinism) a run behaves the same on every CPU. The inputs
  are public, so the time a call takes may depend on them.

### Laws

Each law names the bug it catches and the oracle its expected value comes from. The
vectors are copied from their sources byte for byte. The Wycheproof files are `testvectors_v1/`
at [C2SP/wycheproof](https://github.com/C2SP/wycheproof) commit
`65ca494727a52944c204441c860b039e6786b99b`, cited by `tcId`. Keys come from the test's
group: `publicKey.pk` for Ed25519, `publicKey.uncompressed` for ECDSA. NIST records are
counted from 1 within the `[P-256,SHA-256]` section of `SigVer.rsp` in
[186-4ecdsatestvectors.zip](https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/dss/186-4ecdsatestvectors.zip),
with key `0x04 || Qx || Qy` and signature `R || S`.

| law | bug | oracle and setup |
|---|---|---|
| **SV1** Ed25519 accepts | the wrong hash input order, a prehash or context variant, or a wrong base point | RFC 8032 §7.1 TEST 1 (empty message), TEST 2 (`72`) and TEST 3 (`af82`), with their public keys and signatures: each gives `true`. |
| **SV2** message bound | `msg` not hashed, or hashed only in part | TEST 3's key and signature over `af83`, TEST 3's message with the low bit of its last byte flipped, give `false`. The pinned file has no message-change case. By hand: changing `msg` changes `k`, so `[S]B − [k]A` no longer encodes `R`. |
| **SV3** S below L | `S` reduced mod `L` instead of checked | `ed25519_test.json` tcId 63 (`S + L`) gives `false`. tcId 3, the same signature with the original `S`, gives `true`. |
| **SV4** small-order key | a key-order check that ring and this rule do not make | `ed25519_test.json` tcId 152 gives `true`. The key is the identity, `R` is the encoding of `B` and `S = 1`, so `[1]B − [k]·identity = B`, whose encoding is `R`. Wycheproof marks this `acceptable`, so the expected value comes from the equation. |
| **SV5** Ed25519 lengths | a wrong length that fails the call, reads past the carrier, or is accepted | Each of these gives `false` and the call does not fail: TEST 1's key cut to 31 bytes, TEST 1's key with `00` appended, TEST 1's signature cut to 63 bytes, and `ed25519_test.json` tcIds 30 (empty signature) and 33 (signature too long). |
| **SV6** P-256 SigVer | a wrong curve, a truncated digest, or a message passed where the digest belongs | `p256.verify` gives `true` for NIST record 4 (`Result = P`) and `false` for record 1 (`F (3 - S changed)`). |
| **SV7** high S | low-S normalisation that rejects a valid signature | `p256.verify` gives `true` for `ecdsa_secp256r1_sha256_p1363_test.json` tcId 64, which is valid with `s > n/2`. `p256.verify_der` gives `true` for `ecdsa_secp256r1_sha256_test.json` tcId 7, the high-S twin of tcId 5. |
| **SV8** strict DER | a BER-lenient parser, or a sign bit read as magnitude | In `ecdsa_secp256r1_sha256_test.json`, `p256.verify_der` gives `true` for tcId 5. It gives `false` for tcId 8 (the sequence length in long form) and for tcId 6 (`s` without its leading zero, so negative). |
| **SV9** P-256 forms and lengths | a compressed or bare key decoded on one engine only, or a wrong length that fails the call | Each of these gives `false` and the call does not fail. With NIST record 4's message and signature: the key compressed (`0x02` or `0x03` by `Qy`'s parity, then `Qx`) and the bare 64-byte `Qx \|\| Qy`. With record 4's key and message: the signature cut to 63 bytes. And `ecdsa_secp256r1_sha256_p1363_test.json` tcId 121, a 2-byte signature. |
| **SV10** reference charge | a kernel lowered as library code that loops, a charge per byte or per block, or a weight missing | In a core test body, whose calls of core functions take no call step, on the reference evaluator `bound bytes 0 { bound steps 256 { ed25519.verify(k, m, s) } }` gives `Ok(Ok(true))` for TEST 1, whose message is empty, and `bound steps 255` gives `Error(OutOfTime(steps: 255))`. Under `bound steps 513` and `512`, `p256.verify` gives the same for NIST record 4, whose message is 128 bytes, and `p256.verify_der` for tcId 5. The arguments are locals bound outside the scopes. The expected value follows by hand from MEM-30's table: Ed25519's weight 256 and P-256's 512, one step for a nonempty message under the quantum (64 and 256 bytes) and none for an empty one, and nothing admitted. |
| **SV11** P-384 accepts and rejects | a wrong curve or hash, a P-256 routine reused, or a Wycheproof case read the other way | Over all of `ecdsa_secp384r1_sha384_p1363_test.json` with `p384.verify` and all of `ecdsa_secp384r1_sha384_test.json` with `p384.verify_der`: each `valid` case gives `true` and each `invalid` case gives `false`. Neither file has an `acceptable` case. With NIST records counted within `[P-384,SHA-384]` of the same `SigVer.rsp`, each record gives `true` exactly when its `Result` is `P`. |
| **SV12** P-384 forms, DER and high S | a compressed key decoded, a BER-lenient parser, or low-S normalisation | In `ecdsa_secp384r1_sha384_test.json`, `p384.verify_der` gives `true` for tcIds 5 and 7 (its high-S twin), and `false` for tcIds 6 (`s` without its leading zero), 8 and 9 (BER sequence lengths). In the P1363 file, tcIds 1 and 147 (high `s`) give `true`, and tcIds 164 (`r > n`) and 165 (`s > n`) give `false`. With tcId 1's message and signature, the key compressed by `Qy`'s parity, and the bare 96-byte `Qx \|\| Qy`, give `false`; tcId 1's signature cut to 95 bytes gives `false`. None of them fails the call. |
| **SV13** RS256 accepts | a wrong DigestInfo, a hash over the wrong bytes, or a big-endian/little-endian slip in `n` or `e` | `rsa_signature_2048_sha256_test.json` tcIds 1 to 7, with `n` and `e` from the group's `publicKey.modulus` and `publicKey.publicExponent`, give `true`. The JWS of [RFC 7515 Appendix A.2](https://www.rfc-editor.org/rfc/rfc7515#appendix-A.2), with `msg` its ASCII JWS Signing Input and `n` and `e` from its JWK, gives `true`, and with the last byte of the signing input changed gives `false`. tcId 1 with one leading zero byte added to `n` gives `true`. |
| **SV14** PKCS#1 v1.5 is encode-and-compare | a padding parser that accepts BER, trailing garbage or a missing NULL (Bleichenbacher 2006 forgeries against `e = 3`) | Over all of `rsa_signature_2048_sha256_test.json`, each `invalid` case gives `false`, among them tcIds 236 and 237 (the message not hashed), 244 and 245 (`sig ≥ n`) and 246 (two bytes too long). tcId 8, which Wycheproof marks `acceptable` (`MissingNull`), gives `false`: the expected value follows from §9.2's encoding, which the rule compares byte for byte. |
| **SV15** PS256 with a fixed salt | salt-length auto-detection, a wrong MGF hash, or a wrong `emBits` mask | Over all of `rsa_pss_2048_sha256_mgf1_32_test.json` with `rsa.verify_pss_sha256`, each `valid` case gives `true` (tcIds 73 and 74 among them: salts of all zero and all one bits) and each `invalid` case gives `false`. Each `valid` case of `rsa_pss_2048_sha256_mgf1_0_test.json` gives `false`, since its salt is empty and the rule fixes 32 bytes. |
| **SV16** RSA key bounds | a small or even key accepted, or a bad key that fails the call | Each gives `false` and none fails the call: the nine SHA-256 cases of `rsa_pkcs1_1024_sig_gen_test.json` (marked `acceptable` with `SmallModulus`), SV13's tcId 1 with `e` replaced by `01 00 00` (even) and by `01` (below 3), with `e` given as nine bytes `01 00 00 00 00 00 00 00 01` (≥ 2^33), with its signature cut to 255 bytes, and with the modulus replaced by either an even 256-byte integer or the 8201-bit odd integer encoded as `01`, 1023 zero bytes, then `01`. |
| **SV17** reference charge | a per-byte charge, a missing weight, or RSA's modular exponentiation lowered as a loop | In a core test body, on the reference evaluator, `bound bytes 0 { bound steps 2048 { rsa.verify_pkcs1_sha256(n, e, m, s) } }` gives `Ok(Ok(true))` for SV13's tcId 1 and `bound steps 2047` gives `Error(OutOfTime(steps: 2047))`; the same two scopes over `rsa.verify_pss_sha256` give the same for `rsa_pss_2048_sha256_mgf1_32_test.json` tcId 1. Both messages are empty. `p384.verify` for its P1363 tcId 1, whose message is 6 bytes, gives `Ok(Ok(true))` under 8193 and refuses under 8192. The arguments are locals bound outside the scopes. By hand from MEM-30's table: the weight (RSA 2048, P-384 8192), one step for a nonempty message under the quantum, and nothing admitted. |
| **SV18** Ed25519 edge vectors | a verifier change that switches between cofactorless, cofactored (ZIP-215) or strict-point-decoding acceptance sets, or checks `S <= L` instead of `S < L` | All 12 vectors of "Taming the many EdDSAs" (`novifinancial/ed25519-speccheck` commit `65519336fda78a3d016e947df6d82848aca0c9da`, `cases.json` 0–11), selected ZIP-215 vectors (`hdevalence/ed25519consensus` commit `15731de461e4d21d6a79d5b7aaf473454f9d2009`, `zip215_test.go` cases 0, 8, 12, 14, 112, 168, 182), ZIP-215 case 0 with `S = L` (`[L]B = [0]B`), and Wycheproof `ed25519_test.json` tcIds 18 and 151. Expected verdicts follow the cofactorless rule above (matching ring 0.17.14): `true` for speccheck cases 0–3 and 11 and ZIP-215 cases 0, 112, 168, 182; `false` for `S = L`, `S > L`, `S >> L` (cases 6–7, tcId 18), cofactored-only cases (cases 4–5, ZIP-215 case 14), non-canonical `R` (cases 8–9, ZIP-215 cases 8, 12, tcId 151) and canonicalized-key hash case 10. |

The laws are cases in the core contract harness, in `core/core/tests/ed25519.hirsel`,
`core/core/tests/p256.hirsel`, `core/core/tests/p384.hirsel` and `core/core/tests/rsa.hirsel`.
SV11, SV14 and SV15 walk whole Wycheproof files, which are checked in under
`core/core/tests/vectors/` byte for byte from the pinned commit. They take no capability, so the shared walker's `--backend all`
runs each on the evaluator, Cranelift dev and LLVM release ([README](../../core/std/tests/README.md)).

## Argon2id

```hirsel core
pub type Argon2Cost { memory_kib: u32, passes: u32, lanes: u32 }
pub type Argon2Error = Params | Capacity(Capacity)
pub fn id(password: Bytes, salt: Bytes, key: Bytes, data: Bytes, cost: Argon2Cost, len: u64, mut out: Bytes) -> Result[(), Argon2Error]
```

`argon2.id` computes Argon2id version `0x13`, the function `H` of
[RFC 9106](https://www.rfc-editor.org/rfc/rfc9106) §3, with `password` as `P`, `salt` as
`S`, `key` as the secret value `K`, `data` as the associated data `X`, `memory_kib` as `m`,
`passes` as `t`, `lanes` as `p` and `len` as the tag length `T`. It appends the `len` tag
bytes to the spare capacity of `out`. `key` and `data` are empty in ordinary password
hashing; they are there because RFC 9106's own test vector uses them. Lanes are computed
one after another on the calling thread, which gives the same tag as parallel lanes. Only
version `0x13` exists here: RFC 9106 specifies no other.

The results, decided in this order:

- `Params` before any work, admitting nothing and taking no step beyond the call's own,
  when any of these fails: `1 ≤ lanes ≤ 2^24 − 1`, `memory_kib ≥ 8 × lanes`, `passes ≥ 1`,
  `4 ≤ len ≤ 2^32 − 1`, `8 ≤ len(salt) ≤ 2^32 − 1`, `len(password) ≤ 2^32 − 1`,
  `len(key) ≤ 2^32 − 1` and `len(data) ≤ 32`. All but the last two bounds are RFC 9106 §3.1's;
  the eight-byte salt floor and the 32-byte associated-data ceiling are the RustCrypto
  crate's, refused here explicitly rather than inherited as a hidden failure.
- `Capacity(c)` before any work, with `len` as `requested`, when the tag does not fit the
  spare of `out`. Nothing is appended.
- The work charge, below. When the current bound has too few steps it raises `OutOfTime` at
  the call, having admitted and computed nothing.
- `Capacity(c)` when the limit refuses the working memory, below, with nothing computed.
- `Ok(())` with the tag appended.

**Charge.** The call takes one step, as every core call does, and then one further step
for each 1 KiB block compression the parameters choose: `passes × m′`, where
`m′ = 4 × lanes × ⌊memory_kib / (4 × lanes)⌋` is the block count RFC 9106 §3.2 rounds
`memory_kib` down to. The steps are taken at the call, before the memory is admitted and
before any work, as `secret.scram_sha256` takes its iteration steps
([MEM-30](budgets.md#mem-30-step-bounds)). This is MEM-30's per-unit exception and not the
one step of `sha256.blocks`, because here the work is not bounded by any length a limit
bounds: `passes` alone can make a call with small arguments run for an hour, and a
stored hash that names its own cost is outside input. The
initial and final BLAKE2b hashing is bounded by the argument lengths and is covered by the
call's own step. The call then admits `1024 × m′` bytes of working memory from the
current limit ([MEM-24](budgets.md#mem-24-logical-charges)), zeroes it after the tag is
computed and refunds it before it returns. The tag lands in capacity the caller holds, so
nothing else is admitted.

**Determinism and timing.** The tag is a function of the arguments alone. The kernel is a
pure computation: it passes no replay point and replay computes it again. The engine may
use the crate's vector code; the tag is the same on every CPU (SEM-8). Argon2id's first
half pass addresses memory independently of the password, and the rest addresses it by
the data, as RFC 9106 §3.4 designs; no further constant-time property is required or
claimed. The kernel is RustCrypto `argon2` 0.6.0 with `Algorithm::Argon2id`,
`Version::V0x13` and `Argon2::new_with_secret` for a nonempty `key`, computing with
`hash_password_into_with_memory` over the admitted blocks.

**Std.** `std.argon2` is library code over the kernel. Its `PhcError` has two cases,
`Malformed` and `Argon2(Argon2Error)`.

```hirsel
fn hash(password: Bytes, salt: Bytes, cost: Argon2Cost) -> Result[String, std.argon2.PhcError] {
  std.argon2.hash(password, salt, cost)
}
fn verify(password: Bytes, phc: Bytes) -> Result[bool, std.argon2.PhcError] {
  std.argon2.verify(password, phc)
}
fn recommended() -> Argon2Cost { std.argon2.recommended() }
```

`hash` computes a 32-byte tag and writes
`$argon2id$v=19$m=<m>,t=<t>,p=<p>$<salt>$<tag>`, the PHC string format that libargon2 and
RustCrypto write, with decimal parameters without leading zeros and the salt and tag in
standard base64 without padding. `verify` parses exactly that form, recomputes with the
decoded salt, the parameters and the decoded tag's length, and compares the tags with
`hmac.equal`, whose time depends only on the lengths. Any other text is `Malformed`: another
variant (`argon2i`, `argon2d`), a version other than `19` or none, parameters out of order
or repeated, a `keyid` or `data` field, base64 with padding or outside the alphabet, and a
tag shorter than 4 bytes. A well-formed string whose parameters the kernel refuses gives
`Argon2(Params)`. The salt is the caller's, drawn with `host.random`; `std.argon2` takes no
capability. `recommended` returns RFC 9106 §4's second recommendation, `m = 65536`, `t = 3`,
`p = 4`, which costs `196,609` steps and 64 MiB.

A password sealed as a `Secret` derives a key with `secret.derive_argon2id`
([TLS-5](tls.md#tls-5-secrets)). It uses the same Argon2id parameters, block steps
and working-memory charge. TLS-5 defines the child policy, storage charge and
refusal order.

### Argon2id laws

In the scoped laws, `out` is moved into the scope and returned with the result, since
MEM-30 refuses a body that lends `mut` a place declared outside it; the other arguments are
locals bound outside.

| law | bug | oracle and setup |
|---|---|---|
| **AR1** RFC vector | a wrong `H0` (missing `K` or `X`, wrong length prefixes), wrong addressing, or Argon2i or Argon2d computed | `argon2.id` with `P` 32 bytes `01`, `S` 16 bytes `02`, `K` 8 bytes `03`, `X` 12 bytes `04`, `m = 32`, `t = 3`, `p = 4` and `len = 32` appends RFC 9106 §5.3's tag. |
| **AR2** reference vectors and PHC | a lane-count or memory-size slip that only shows at larger `m`, or a PHC encoding that other implementations do not read | The seven `Argon2_id` `hashtest` cases in [phc-winner-argon2](https://github.com/P-H-C/phc-winner-argon2) `src/test.c` (password `password` or `differentpassword`, salt `somesalt`, `m` from 2^8 to 2^18, `t` from 1 to 4, `p` 1 or 2): `argon2.hash` returns each case's PHC string exactly, and `argon2.verify` of it gives `true`. The same string with its tag's last character changed so the decoded tag differs gives `false`. |
| **AR3** parameter refusals | a refusal after work, or a crate error that fails the call | Each gives `Params`, appends nothing and, under `bound bytes 0 { bound steps 1 { ... } }` with its arguments bound outside, returns `Ok(Ok(Error(Params)))`: `m = 15, p = 2`; `t = 0`; `p = 0`; `p = 2^24`; a 7-byte salt; `len = 3`; 33 bytes of `data`. |
| **AR4** per-block charge | the work counted as one step, `m` not rounded down, or the steps taken after the work | With AR2's `m = 256, t = 2, p = 2` case (`m′ = 256`, so 512 blocks), `bound steps 513 { ... }` succeeds and `bound steps 512 { ... }` gives `Error(OutOfTime(steps: 512))` with nothing appended to `out`. With `m = 259` and otherwise the same, `m′` is still 256 and the same two bounds give the same two results. By hand from RFC 9106 §3.2 and MEM-30. |
| **AR5** memory admission | working memory taken from nowhere, kept after return, or admitted before the steps | Under a limit with `262,143` bytes free, AR4's call gives `Capacity(c)` with `c.requested = 262,144`; with `262,144` free it succeeds, and afterwards the limit has `262,144` free again. |
| **AR6** PHC strictness | a lenient parser that accepts a downgraded variant or version | Starting from AR2's first string, `argon2.verify` gives `Malformed` for `$argon2i$` in its place, `v=16`, `v=` removed, `t=2,m=65536` swapped, `m=065536`, a trailing `=` on the salt, and `,keyid=AAAA` after the parameters. |

The laws are cases in `core/core/tests/argon2.hirsel` and `core/std/tests/argon2.hirsel`
with `--backend all`. AR2's larger cases take a quarter of a GiB and a fraction of a second
each on native engines; on the evaluator only the `m = 256` cases run.

## Key state erasure

Hirsel zeroes the secret bytes and Argon2id working memory that it owns under the
respective rules. The AEAD, derivation, MAC, wrap and sign operations use uncharged,
run-owned memory. Their erasure guarantees exclude state held by the cryptographic
backend and earlier copies in registers and on the stack. TLS-8 defines which
[packet cipher state Hirsel erases](tls.md#tls-8-packet-ciphers-and-key-agreement).
