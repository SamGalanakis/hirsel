# JWS, JWK and JWT

[API overview](../API.md). [Operations and contracts](../jwt.hirsel). [HMAC](hmac.md#secret-keys).
[Base64](base64.md). [JSON](json.md). [TLS and secrets](../../../docs/spec/tls.md#tls-5-secrets).

`std.jwt` verifies and signs JWS compact tokens
([RFC 7515](https://www.rfc-editor.org/rfc/rfc7515)) and JWTs
([RFC 7519](https://www.rfc-editor.org/rfc/rfc7519)), and reads verification keys from
JWKs and JWK Sets ([RFC 7517](https://www.rfc-editor.org/rfc/rfc7517)). It is built over
`std.base64` (URL alphabet, unpadded, strict), `std.json`, the `Secret` HMAC sink and the
core `ed25519`, `p256` and `rsa` verifiers.

```
pub type Alg: key = Hs256 | EdDsa | Es256 | Rs256 | Hs384 | Hs512
pub type Allow { first: Alg, rest: List[Alg] }
pub type Error = Malformed(Fault) | UnsupportedAlg | UnsupportedCrit | WrongKey
    | BadSignature | Expired | NotYetValid | Mismatch(Claim) | NoKey | AmbiguousKey
    | BadKey | UnsupportedKey | PrivateKey | Denied | Sign(KeyError)
pub fn verify_jws(token: String, accept: Allow, k: Key) -> Result[Jws, Error]
pub fn verify_jwt(token: String, r: Rules, k: Key) -> Result[Jwt, Error]
pub fn verify_jws_in / verify_jwt_in(..., keys: KeySet)
pub fn sign_jws(s: Signer, header: String, payload: Bytes) -> Result[String, Error]
pub fn sign_jwt(s: Signer, claims: String) -> Result[String, Error]
pub fn jwk(text: String) -> Result[Key, Error]
pub fn jwks(text: String) -> Result[KeySet, Error]
```

## Algorithms

| `Alg` | Name | Verify | Sign |
|---|---|---|---|
| `Hs256` | `HS256` | `secret.jws_hmac_verify`, 32-byte tag | `secret.jws_hmac_sign`, key at least 32 bytes |
| `Hs384` | `HS384` | `secret.jws_hmac_verify`, 48-byte tag | `secret.jws_hmac_sign`, key at least 48 bytes |
| `Hs512` | `HS512` | `secret.jws_hmac_verify`, 64-byte tag | `secret.jws_hmac_sign`, key at least 64 bytes |
| `EdDsa` | `EdDSA` (Ed25519 only, [RFC 8037](https://www.rfc-editor.org/rfc/rfc8037)) | `ed25519.verify`, 64-byte signature | `secret.sign` (`sign(ed25519)`, PKCS#8) |
| `Es256` | `ES256` | `p256.verify`, 64-byte P1363 `r‖s` | `secret.sign` (`sign(p256)`, PKCS#8) |
| `Rs256` | `RS256` | `rsa.verify_pkcs1_sha256`, signature as long as `n` | `secret.sign` (`sign(rs256)`, PKCS#8) |

`RS384`, `RS512`, `PS*`, `ES384` and `ES512` follow [core Open item 3](../../../docs/spec/core.md).

## Invariants

Each invariant is a property of the types, not a caller's discipline.

1. **Explicit allowlist, no `none`.** Every verification takes an `Allow`, which holds at
   least one `Alg` by construction. `Alg` has no `none` variant, so a header naming `none`
   (or any other name) gives `UnsupportedAlg` before a key is touched. An algorithm missing
   from the allowlist gives `UnsupportedAlg` too.
2. **Keys carry their family.** A `Key` is opaque and built only by `hmac_key`,
   `ed25519_key`, `p256_key`, `rsa_key`, `jwk` or `jwks`. Each records its family (MAC, RSA,
   EC or OKP) and, from a JWK `alg`, an optional pinned algorithm. A header whose algorithm
   is of another family, or differs from the pin, gives `WrongKey`. An RSA public key can
   therefore never be used as an HMAC key, the RFC 8725 §2.1 confusion.
3. **Constant-time MAC.** The runtime checks HMAC tags against `Secret` keys.
   It requires the key minimum in the table for both signing and verification.
   An undersized key gives `BadKey`. Signing requires the `mac` use. A JWK `oct`
   key is sealed with the `verify` use only.
4. **Time and audience.** `Rules` holds `now_secs` and `leeway_secs`; the module reads no
   clock. With `t = now_secs`:
   - `exp ≤ t − leeway` gives `Expired` (so a token is valid up to `exp + leeway − 1`).
   - `nbf > t + leeway` or `iat > t + leeway` gives `NotYetValid`.
   - A missing `exp` when `require_exp` is set gives `Mismatch(Exp)`.
   - `iss` must equal `Rules.iss` when that is set (`Mismatch(Iss)`).
   - `aud` is a string or an array of strings. When `Rules.aud` is set it must be listed;
     when it is not and the token has `aud`, the token is refused, since RFC 7519 §4.1.3
     says a recipient that does not identify itself with a value in `aud` must reject it.
   Leeway is added to the trusted clock with saturation, so no claim value can overflow it.
   NumericDate values must be non-negative integers; fractions and negatives are `Malformed(Json)`.
5. **One key per `kid`.** `find` and `verify_*_in` return exactly one key. Two keys with
   the token's `kid` give `AmbiguousKey`; no match gives `NoKey`. A token without `kid`
   selects the set's only key, and is `AmbiguousKey` when the set holds more than one.
6. **Strict parsing.** A token is at most `MAX_TOKEN` (64 KiB) and has exactly three
   segments. Each segment is canonical unpadded base64url: padding, other alphabets and
   nonzero trailing bits are `Malformed(Encoding)`. The header must be a UTF-8 JSON object
   with unique member names and an `alg`. Any `crit` gives `UnsupportedCrit`, because this
   module understands no header extension (RFC 7515 §4.1.11). A `b64` value of `false`
   also gives `UnsupportedCrit` (RFC 7797). The `std.json` 128-level limit counts
   containers from the document root. A JWK Set is at most `MAX_JWKS` (1 MiB).

Errors distinguish malformed input, an unsupported algorithm, the wrong key, a bad
signature, expiry, not-yet-valid and claim mismatch. Signature checks run only after the
header has been fully admitted, and claims are read only after the signature verifies.

## Keys

- `jwk` reads one public key; `jwks` reads `{"keys": [...]}`. A JWK whose `use` is not
  `sig`, whose `key_ops` lacks `verify`, or whose `kty`, `crv` or `alg` this module does
  not use is skipped by `jwks` (RFC 7517 §5) and is `UnsupportedKey` from `jwk`.
  Duplicate `key_ops` entries or operations inconsistent with `use` give `BadKey`.
- A JWK with private members (`d`, `p`, `q`, `dp`, `dq`, `qi`, `oth`) is `PrivateKey`, and
  a set holding one is refused as a whole: a published key set that leaks a private key is
  a configuration error, not a key to skip.
- An `oct` key must be at least 32 bytes (RFC 7518 §3.2). A pinned `alg` outside the key's
  family is `BadKey`. RSA moduli are odd, have 2048 to 8192 significant bits and
  use minimal unsigned encodings. RSA exponents are odd and at least 3. Ed25519
  keys with small order give `BadKey`.
- Signing keys are `Secret`s: `signer(alg, key)` takes a sealed HMAC key or PKCS#8 private
  key. A use missing from the secret's policy gives `Denied`.

## Signing

`sign_jws` takes the protected header text as given, so the signing input is the caller's
exact bytes (RFC 7515 A.1's CRLFs survive). The header must parse under the rules above
and name the signer's algorithm, otherwise `WrongKey`. `sign_jwt` writes
`{"alg":…,"typ":"JWT"}` plus the signer's `kid`.

- **Authority.** Verification needs none. Signing and HMAC verification use the secret's
  policy ([TLS-5](../../../docs/spec/tls.md#tls-5-secrets)).
- **Charging.** Work is linear in the token or set size; both are bounded above.
- **Replay.** Parsing and public-key verification are pure. The `Secret` operations are
  provider operations and record their tag, verdict or signature, never the key.

## Oracle

`core/std/tests/jwt.hirsel` checks RFC 7515 A.1–A.3, RFC 8037 A.4, RFC 7517 A.1–A.3,
and Python-generated HS384 and HS512 tokens at their minimum key sizes. It also covers
`alg: none`, MAC/RSA key confusion, the expiry and audience rules, and malformed headers.
