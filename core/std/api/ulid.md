# Ulid

[API overview](../API.md). [Operations and contracts](../ulid.hirsel).

`std.ulid` implements the [ULID specification](https://github.com/ulid/spec).
`Ulid` stores the 128 bits in network order. The high 48 bits hold Unix
milliseconds and the low 80 bits hold randomness. The derived `key` and
`ordered` traits use the full value, so ascending order compares timestamps
first. Within a millisecond, it compares the randomness.

`generate` reads ten bytes starting at the supplied offset. The caller
supplies the timestamp and randomness. For a second ULID in the same
millisecond, `increment` increments the randomness and refuses overflow,
preserving the timestamp.

`parse` accepts 26 ASCII characters from the ULID alphabet in either case.
It rejects I, L, O and U, and rejects encodings above
`7ZZZZZZZZZZZZZZZZZZZZZZZZZ`. `push` and `to_string` emit uppercase text.
