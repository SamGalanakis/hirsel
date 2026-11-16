# SHA-1

[API overview](../API.md). [Operations and contracts](../sha1.hirsel).

`hash` computes RFC 3174's twenty network-order digest bytes. It reads the
input without copying it, holds its schedule and chaining words inline, and
allocates only the result. Capacity is explicit. SHA-1 exists for RFC 6455
accept keys and must not be used for integrity or authentication.

The scalar schedule follows RFC 3174 section 6.1 and RustCrypto's
`sha1` 0.10.6 `src/compress/soft.rs`. Hirsel uses checked indexing and explicit
wrapping additions; it does not select CPU-specific SHA instructions.
