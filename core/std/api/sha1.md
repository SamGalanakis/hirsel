# SHA-1

[API overview](../API.md). [Operations and contracts](../sha1.hirsel).

`hash` returns RFC 3174's twenty network-order digest bytes. It borrows the
complete input blocks, copies the remaining bytes into at most two inline
padding blocks, and allocates only the result. Capacity is explicit. SHA-1
exists for RFC 6455 accept keys and for protocols that require HMAC-SHA1.
Use [SHA-256](sha256.md) for new protocols.

`start` and `start_block` prepare a streaming state. `add` takes a checked
byte range, `add_byte` takes one byte, and `finish` returns twenty inline
digest bytes without changing the state. The streaming operations allocate
nothing.

The core compression uses SHA instructions on x86-64 and ARMv8 when available,
with a bit-identical portable implementation. The call takes one step in either
case. Set `HIRSEL_SHA1=portable` to exercise the portable implementation.
