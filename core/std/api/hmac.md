# HMAC

[API overview](../API.md). [Operations and contracts](../hmac.hirsel). [SHA-256](sha256.md).
[TLS and secrets](../../../docs/spec/tls.md#tls-5-secrets).

## Program-held keys

```
pub opaque type Hmac  // inner and outer SHA-256 states
pub fn hmac.start(key: Bytes) -> Hmac
pub fn hmac.add(mut h: Hmac, input: Bytes, lo: u64, hi: u64)
pub fn hmac.add_byte(mut h: Hmac, x: u8)
pub fn hmac.finish(h: Hmac) -> Inline[u8, 32]
pub fn hmac.sha256(key: Bytes, data: Bytes) -> Inline[u8, 32]
pub fn hmac.equal(a: Bytes, b: Bytes) -> bool
```

`std.hmac` is RFC 2104 HMAC over `std.sha256`, with RFC 4231's test vectors as its
oracle. A key longer than the 64-byte block is hashed first, and every key is zero-padded
to the block and masked with `0x36` and `0x5c`, as in Go's `crypto/hmac`. The streaming
operations follow `std.sha256`: `add` appends the range `[lo, hi)` with `list.slice`'s
checked semantics, and `finish` returns the tag without changing the state.

`equal` compares in time that depends only on the two lengths, never on where they first
differ, as Go's `hmac.Equal` and `subtle.ConstantTimeCompare` do. Unequal lengths give
`false`. Compare received tags with `equal`, never with `==`.

- **Authority.** None. The key is ordinary program bytes.
- **Charging.** Nothing is admitted. `Hmac` holds two SHA-256 states inline, so the key is
  not retained. `start` takes O(len(key)) work, `add` and `sha256` O(n), `add_byte`
  and `finish` O(1). A truncated tag is a slice of the 32 bytes.
- **Replay.** These are pure computation. They pass no replay point and are recomputed on
  replay.

For protocols that require HMAC-SHA1, the corresponding operations have
the same range, allocation and finish semantics and return a 20-byte tag.
The oracle is RFC 2202.

```
pub opaque type HmacSha1
pub fn hmac.start_sha1(key: Bytes) -> HmacSha1
pub fn hmac.add_sha1(mut h: HmacSha1, input: Bytes, lo: u64, hi: u64)
pub fn hmac.add_sha1_byte(mut h: HmacSha1, x: u8)
pub fn hmac.finish_sha1(h: HmacSha1) -> Inline[u8, 20]
pub fn hmac.sha1(key: Bytes, data: Bytes) -> Inline[u8, 20]
```

## Secret keys

```
pub type MacError = Denied()
pub fn secret.hmac_sha256(s: Secret, data: Bytes) -> Result[Inline[u8, 32], MacError]
pub fn secret.verify_hmac_sha256(s: Secret, data: Bytes, tag: Bytes) -> Result[bool, MacError]
```

These are the HMAC sink that TLS-5 reserves. The runtime computes the MAC from the
secret's run-owned bytes, and the key never enters program memory, a view, a
diagnostic or a recording. `verify_hmac_sha256` compares the tag in constant time
inside the runtime and returns only the verdict. A tag of any length other than 32 bytes
gives `Ok(false)`. Accepting a shorter tag as a prefix would let a 1-byte guess pass one
time in 256.

- **Authority.** A MAC with a secret's key is a signing oracle. Whoever can ask for tags
  over chosen data can forge what the key authenticates, wherever the tag is then sent.
  So a secret can be used as a key only when its policy holds the use
  ([TLS-5](../../../docs/spec/tls.md#tls-5-secrets)): `mac` permits both operations, and
  `verify` permits only `verify_hmac_sha256`, which reveals one bit per call.

  ```sh
  hirsel run app --grant 'Secret:hook=env:WEBHOOK_KEY verify'
  hirsel run app --grant 'Secret:aws=file:/run/secrets/aws to *.amazonaws.com mac'
  ```

  The second grant is `tls_send(*.amazonaws.com) mac`, the canonical text that
  `secret.seal` and `secret.narrow` take (`"tls_send(*.example.com) mac"`, `"verify"`).
  `narrow` intersects uses: `mac` with `verify` gives `verify`, and a use absent from
  either side is dropped. An empty result is `Invalid`. Without the needed use, the
  operation gives `Denied` before issue, with no effect.
- **Passwords.** A password used for SCRAM goes through `secret.scram_sha256`
  ([TLS-6](../../../docs/spec/tls.md#tls-6-scram-sha-256)), never through these. Under
  `mac`, a program could rebuild PBKDF2 one HMAC at a time and read `SaltedPassword`.
- **Charging.** Nothing is admitted. The tag is an inline result. Neither operation waits.
- **Failures.** `Denied` only. The secret's bytes are always present while the run lives.
- **Replay.** Each operation is a provider operation
  ([CAP-5](../../../docs/spec/capabilities.md#cap-5-provider-operations)) and passes the
  replay point at issue and at delivery
  ([CAP-8](../../../docs/spec/capabilities.md#cap-8-the-replay-point)). The issue holds the
  secret's designation and the canonical bytes of `data` (and of `tag`). The delivery
  holds the 32-byte tag or the verdict. When recording is on, the recording holds these
  and never the key or anything else computed from it. A replay, whose secret has no bytes
  behind it, delivers the recorded tag or verdict
  ([recording](../../../docs/recording.md#credentials)). Recording is off by default.
