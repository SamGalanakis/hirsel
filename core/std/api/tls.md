# TLS client and secrets

[API overview](../API.md). [TLS and secrets specification](../../../docs/spec/tls.md).

Connect a numeric address with `net.connect`, then pass the owned socket, its
verification name, a trust policy and the ordered ALPN offer to `tls.connect`.
The handshake consumes the socket and returns the read and write halves,
the selected zero-based ALPN index if there is one, and the SHA-256 digest
of the peer's leaf certificate in four big-endian words. Use `System` for
the system roots or `Roots(pem)` for an exact PEM bundle. The Net grant must
cover the verification name, port and trust policy.

Use ordinary `net.recv_bounded` and `net.send_pair` on the returned halves.
A receive of zero bytes means the peer closed TLS cleanly. Transport EOF
without a TLS close notification gives `Tls(Truncated)`.

`tls.send_secret` writes a public prefix, a `Secret` and a public suffix.
The secret must cover the stream's verified name. A plain TCP half or a name
outside the secret's patterns gives `Denied` before sending any bytes.
Failure after a partial commit spends the write half. Later sends on it give
`Reset`.

The launcher can grant a `Secret` from the environment or a file. Use
`secret.seal` to transfer owned bytes into secret storage and `secret.narrow`
to restrict the permitted names. `secret.render` always returns `[secret]`.
A grant or seal whose `to` text ends in `mac` or `verify` also lets the secret key an
HMAC-SHA256 ([HMAC](hmac.md#secret-keys)).

For HTTP/1.1 over TLS, use [the HTTP client](http_client.md).
