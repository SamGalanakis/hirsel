# TLS front on 443

These fronts terminate TLS on 443 for one HTTPS name, `web.test`, and one
stream name, `stream.test` (launch.md H1). They pick the route from the
ClientHello's SNI alone, with no ALPN. The HTTPS name goes to the HTTP listener
on 127.0.0.1:18201 with one `X-Forwarded-For`. The stream name goes to the
stream listener on 127.0.0.1:18202, which accepts with `std.proxy.accept`,
behind a PROXY v2 header. With no SNI, or with any other name, the
connection reaches no listener.

`haproxy.cfg` is the output of platform's `haproxy` generator for those two
routes, not a hand copy. Regenerate it from the catalog for a real place. The
generator writes only HAProxy, so `nginx.conf` (nginx 1.31.4 or later, for
`proxy_protocol v2`) is written by hand. Replace the names, the certificate
paths and the listener addresses with the place's own. Keep each stream
listener reachable only from the front, and trust only the front's address in
its `proxy.config`.

`python3 tools/tests/ingress_front.py --compiler HIRSEL --runtime
libhirsel_core_rt.a` runs the `tls_front_stream` law. It runs HAProxy 3.2.27
and nginx 1.31.4 in turn with Docker on loopback port 443, and it first checks
that `haproxy.cfg` still equals the generator's output. Ports 443, 18201,
18202 and 18203 must be free.
