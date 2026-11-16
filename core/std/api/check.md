# Byte checksums

`check.xxh3(b, lo, hi)` returns XXH3-64 with seed zero and the default secret over
`b[lo,hi)`. `check.start()` makes a streaming `Xxh3`; `check.add(mut s, b, lo, hi)`
appends a range, and `check.finish(s)` reads its digest without changing the state.
A finished state accepts further input. None of these operations allocates or
retains a view of its input. Invalid ranges have `list.slice`'s checked semantics.

The scalar implementation uses `int.mul_wide`, a fixed secret, eight accumulation
lanes and fixed buffers. Tests compare all lengths 0–257 and long boundaries
through 4096 with independent libxxhash 0.8.2 vectors, then split a stream at every
byte and append it one byte at a time. The store binds each checksum to its store,
file and byte offset as specified in STORE-SPEC v2.2.
