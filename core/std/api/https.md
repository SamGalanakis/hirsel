# HTTPS identities

[Standard library](../API.md). [Operations](../https.hirsel).
[Specification](../../../docs/spec/http.md#http-1-the-server-its-task-group-and-its-limits).

An `Identity` is one certificate a TLS server presents: the DNS names it
answers, its DER chain leaf first, and its private key, whose policy must
hold `tls_serve`. `server(take identities)` gathers them, the first being the
default, and gives `None` for no identities, an empty chain, or a name that is
empty or ends with a dot. `http.serve_https` serves HTTP/1.1 with a `Server`.

`handshake(take socket, server, until)` runs the TLS 1.3 server handshake
(TLS-9) offering `http/1.1`. It chooses the identity whose names hold the
client's server name, compared without regard to ASCII case, and otherwise the
first. A client that sent no name, or an IP literal, gets the first.
`share(server)` makes another holder of every key, for one connection.

`run_ca(random, common_name, not_before, not_after)` mints a P-256 key that
signs only and a self-signed CA certificate for it. `issue(mut ca, random,
common_name, names)` issues a leaf for `names`, a list of
`x509_build.San`: a fresh P-256 key whose policy is `tls_serve` alone, the
leaf signed by the CA with the CA's validity and the next serial, and the DNS
names among `names` as the identity's names. `roots(ca)` gives the CA
certificate as one PEM block, the bytes a client passes as `Roots` and pins in
its `Tls` rule; `certificate(ca)` gives its DER. The CA key never leaves the
run's provider, so a run's certificates are trusted only by clients handed
that run's roots.
