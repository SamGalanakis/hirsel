# Hex

[API overview](../API.md). [Operations and contracts](../hex.hirsel).

`hex.encode` returns two lower-case RFC 4648 base16 digits for each input byte.
`hex.decode` accepts either case and returns `None` for odd lengths or any
non-hex byte, including whitespace. Both accept empty input and can raise
Capacity when allocating the output.

`hex.encode_byte` returns the two ASCII digits of one byte without allocation.
Pass `true` for wire formats that require upper-case digits.

`hex.append_from(mut out, n, read)` appends the encoding of `n` bytes read in
index order by a scoped callback. It reserves all output space before calling
`read`. `hex.push_u64(mut out, n)` writes exactly sixteen lower-case digits
with leading zeros.
