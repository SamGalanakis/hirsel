# Direct HTTP ingress

These examples listen on port 8080 and forward to a Hirsel listener on
127.0.0.1:8081. Configure that listener's ingress prefixes to trust only the
proxy's actual source address. TLS deployment belongs at this proxy.

Both examples replace X-Forwarded-For with one client address. They do not
trust client-supplied forwarded headers. For an upstream load balancer,
configure its actual trusted CIDRs with Caddy trusted_proxies or nginx
real_ip, and forward the resulting canonical client address. Do not enable
an arbitrary trusted chain. Net authority remains a separate listener grant.

`python3 tools/tests/wire_proxy.py` runs Caddy 2.10.2 and nginx 1.28.0 in
containers against an HTTP listener. It sends a spoofed 9.9.9.9 field and
requires the listener to observe exactly one 127.0.0.1 field. Ports 8080 and
8081 must be free. The source ingress law also rejects missing, duplicate,
list-valued and noncanonical metadata from a trusted peer.
