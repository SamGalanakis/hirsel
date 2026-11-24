# PROXY protocol v2 headers

Use `std.proxy.config(trusted, header_max, header_ms)` to configure a listener
that requires PROXY v2. Supply the front's trusted `wsock.Prefix` values.
`config` refuses an empty list or a byte limit outside 28–65,551.
`proxy.DEFAULT_HEADER_MAX` is 536 bytes and `proxy.DEFAULT_HEADER_MS` is
3,000 milliseconds.

Call `proxy.accept(take socket, peer, config, clock, accepted_ns)` in the
connection task with the peer returned by `host.accept` and the monotonic time
of that accept. It checks the peer against the trusted prefixes before reading
and handles IPv4-mapped IPv6 peers as `wsock` does. It returns the same socket
at the first application byte, along with the decoded `Header`.

Every refusal closes the socket. `Untrusted` means the transport peer is
outside the prefixes. The decoder reports `Missing`, `Malformed`, `Local`,
`Family`, or `TooLong`. `Silent` means the header was incomplete at the deadline
measured from `accepted_ns`. `Ended` means the receive returned end of stream
before the header was complete. `Failed` carries another receive error.

`std.proxy.step(bytes, header_max)` decodes a TCP IPv4 or IPv6 header from a
growing buffer. Pass `proxy.DEFAULT_HEADER_MAX` for the 536-byte default.
`Need(total)` gives the total buffer size to supply next. `Done(header, length)`
returns the client and destination addresses and the exact header length.
Bytes starting at `length` belong to the application stream.

The decoder checks TLV boundaries within the declared, bounded header and
skips TLV values, including CRC32C. It performs no I/O.
