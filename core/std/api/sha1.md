# SHA-1

[API overview](../API.md). [Operations and contracts](../sha1.hirsel).

`hash` returns RFC 3174's twenty network-order digest bytes. It borrows the
complete input blocks, copies the remaining bytes into at most two inline
padding blocks, and allocates only the result. Capacity is explicit. SHA-1
exists for RFC 6455 accept keys and must not be used for integrity or authentication.

The core compression uses SHA instructions on x86-64 and ARMv8 when available,
with a bit-identical portable implementation. The call takes one step in either
case. Set `HIRSEL_SHA1=portable` to exercise the portable implementation.
