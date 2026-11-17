# Base64

[API overview](../API.md). [Operations and contracts](../base64.hirsel).

`encode` appends RFC 4648's standard alphabet and padding to a caller-owned
buffer. Capacity leaves the existing buffer unchanged. `decode` validates the whole
input before allocating its result. `Ok(None)` is malformed input; the outer
error is Capacity. Whitespace, missing or misplaced padding, and nonzero pad
bits are rejected.

Full blocks use little-endian word loads and appends. Their scalar alphabet
tables and packing adapt base64-simd's fallback. Tail padding follows Goose's
base64 codec, with RFC 4648's zero pad bits required for a canonical spelling.
