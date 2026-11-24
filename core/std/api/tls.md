# TLS client and secrets

[API overview](../API.md). [TLS and secrets specification](../../../docs/spec/tls.md).

Connect a numeric address with `host.connect`, then pass the owned socket, its
verification name, a trust policy and the ordered ALPN offer to `tls.connect`.
The handshake consumes the socket and returns the read and write halves,
the selected zero-based ALPN index if there is one, and the SHA-256 digest
of the peer's leaf certificate in four big-endian words. Use `System` for
the system roots or `Roots(pem)` for an exact PEM bundle. The Net grant must
cover the verification name, port and trust policy.

Use ordinary `host.recv_bounded` and `host.send_pair` on the returned halves.
A receive of zero bytes means the peer closed TLS cleanly. Transport EOF
without a TLS close notification gives `Tls(Truncated)`.

`tls.send_secret(w, wire, secrets, marks, until)` writes one message: the
public `wire` bytes with insertions that `marks` places, in groups of four
words `[at, kind, a, b]`. Kind 0 inserts `secrets[a]`, kind 1 inserts it
form-urlencoded, and kinds 2 and 3 insert the decimal or hexadecimal length of
the message bytes made from `wire[a..b]`, so a `Content-Length` can count a
secret ([TLS-5](../../../docs/spec/tls.md#tls-5-secrets)). Malformed marks raise
`Bounds`. Every secret must cover the stream's verified name. A plain TCP half
or a name outside any secret's patterns gives `Denied` before sending any bytes.
Failure after a partial commit spends the write half. Later sends on it give
`Reset`.

The launcher can grant a `Secret` from the environment or a file. Use
`secret.seal` to transfer owned bytes into secret storage. If sealing refuses, its
`Error((reason, bytes))` gives back the original bytes. Use `secret.narrow`
to restrict the permitted names. `secret.render` always returns `[secret]`.
A grant or seal whose `to` text enables cryptographic uses also lets the secret key
HMAC-SHA256 (`mac`/`verify`, `secret.verify_hmac_sha256`, [HMAC](hmac.md#secret-keys)),
key derivation (`secret.derive_hmac_sha256`, `secret.derive_hkdf_sha256`,
`secret.derive_argon2id`), AEAD wrap/unwrap (`secret.wrap`/`secret.unwrap`),
asymmetric signing (`secret.sign`), and SCRAM-SHA-256 (`secret.scram_sha256`,
`secret.scram_final_len`, `secret.scram_verify`). For dynamic credential grants (`Cred`),
see [cred.md](cred.md).

For HTTP/1.1 over TLS, use [the HTTP client](http_client.md).
