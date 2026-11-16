# Base64

[API overview](../API.md). [Operations and contracts](../base64.hirsel).

`encode` appends RFC 4648's standard alphabet and padding to a caller-owned
buffer. Capacity may leave an encoded prefix. `decode` validates the whole
input before allocating its result. `Ok(None)` is malformed input; the outer
error is Capacity. Whitespace, missing or misplaced padding, and nonzero pad
bits are rejected.

The arithmetic alphabet and grouping adapt Goose's `stdlib/base64.goose`.
Checked ranges replace wrapping offsets, and canonical pad-bit validation
makes every accepted spelling unique. Tests use RFC 4648 section 10.
