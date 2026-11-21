# Byte checksums

`check.xxh3(b, lo, hi)` returns XXH3-64 with seed zero and the default secret over
`b[lo,hi)`. `check.start()` makes a streaming `Xxh3`. `check.add(mut s, b, lo, hi)`
appends a range. `check.finish(s)` reads its digest without changing the state,
which can accept further input. These operations allocate nothing and retain no
view of the input. Invalid ranges have `list.slice`'s checked semantics.

One-shot hashing reads directly from the borrowed range. Streaming keeps its
short buffer, incomplete stripe and last 64 bytes inline. Both APIs use the
same short hash for up to 240 bytes. Longer inputs accumulate complete stripes
in groups of up to 16 and separately account for the final, possibly overlapping
stripe. Stripe accumulation uses AVX2 or ARM NEON when available, with the same
values and step rules on the portable path. Set `HIRSEL_XXH3=portable` to exercise it.

The store binds each checksum to its store, file and byte offset as specified in
STORE-SPEC v2.2. Laws check one-shot and split streams against libxxhash 0.8.2
and the published xxHash sanity vectors.
