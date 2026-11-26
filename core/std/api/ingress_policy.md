# Ingress policy

`source` selects the normalized IP key used by admission. `Prefix` is trusted
ingress policy, separate from Net's direction and port authority. Untrusted
peers' forwarded fields are ignored. Trusted peers must supply exactly one
X-Forwarded-For field with one canonical IP literal of at most 256 bytes.
Missing, repeated, list-valued or malformed fields return HTTP 400. IPv4
mapped IPv6 addresses share their IPv4 key. IPv6 spelling follows RFC 5952.
The parser uses eight inline words and allocates nothing.

This adapts Go's reverse proxy replacement policy and Rust's typed IP and
prefix comparisons. Goose's hypothetical socket sketch supplies no ingress
policy. Deployment examples in deploy/wire replace client-provided XFF.

`source_for_head(bytes, head, peer, ingress)` applies the same source policy to a parsed head. It borrows the existing head and performs no I/O.

`trusted(peer, ingress)` checks the transport peer against the ingress prefixes,
including the IPv4 form of an IPv4-mapped IPv6 peer.
