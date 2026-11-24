# PROXY protocol v2 headers

`std.proxy.step(bytes, header_max)` decodes a TCP IPv4 or IPv6 header from a
growing buffer. Pass `proxy.DEFAULT_HEADER_MAX` for the 536-byte default.
`Need(total)` gives the total buffer size to supply next. `Done(header, length)`
returns the client and destination addresses and the exact header length.
Bytes starting at `length` belong to the application stream.

The decoder checks TLV boundaries within the declared, bounded header and
skips TLV values, including CRC32C. It performs no I/O.
