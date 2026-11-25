# Base64

[API overview](../API.md). [Operations and contracts](../base64.hirsel).

`encode` appends RFC 4648's standard alphabet and padding to a caller-owned
buffer. Capacity leaves the existing buffer unchanged. `encode_string` returns
the same encoding in a new `String` and can fail with Capacity. `decode` validates the whole
input before allocating its result. `Ok(None)` is malformed input; the outer
error is Capacity. Whitespace, missing or misplaced padding, and nonzero pad
bits are rejected.

Full blocks use portable SIMD vectors. The encoder writes only ASCII and constructs
the `String` from the bytes it produced. Tail padding follows Goose's Base64
codec, with RFC 4648's zero pad bits required for a canonical spelling.

`encode_url(bytes, padded)` returns a string using RFC 4648 section 5's
`-` and `_` alphabet. `decode_url(bytes, padded)` takes that same alphabet.
Pass `true` to require canonical padding or `false` to omit it. Decoding
rejects the standard alphabet's `+` and `/`, misplaced or unexpected padding,
whitespace, and nonzero pad bits. Malformed input returns `None` before
allocation. Both operations can raise Capacity.
