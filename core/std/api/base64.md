# Base64

[API overview](../API.md). [Operations and contracts](../base64.hirsel).

`encode` appends RFC 4648's standard alphabet and padding to a caller-owned
buffer. Capacity leaves the existing buffer unchanged. `encode_string` returns
the same encoding in a new `String` and can fail with Capacity. `decode` validates the whole
input before allocating its result. `Ok(None)` is malformed input; the outer
error is Capacity. Whitespace, missing or misplaced padding, and nonzero pad
bits are rejected.

Full blocks use SEM-11 vectors. The encoder writes only ASCII and constructs
the `String` from the bytes it produced. Tail padding follows Goose's Base64
codec, with RFC 4648's zero pad bits required for a canonical spelling.
