# Replay

[API overview](../API.md). [Operations and contracts](../replay.hirsel).

A Window is an opaque receive window over absolute u64 indices, as RFC 3711 §3.3.2,
RFC 6347 §4.1.2.6 and RFC 9605 §9.3 describe it. It holds the highest committed index
and one bit for each of the `width` indices at or below it. `seen` classifies an index
without changing the window: Fresh above the highest or at an unmarked offset, Duplicate
at a marked one, TooOld at `width` or more behind. `mark` commits an index the caller has
already authenticated and admitted. Protocol rules (floors, forward bounds, index
estimation) stay with each protocol.

Widths through 128 keep their bits inline and allocate nothing. A wider window reserves
ceil(width / 64) words once, when it is made, under the current limit; `try_copy`
reserves the same for an independent copy. `seen` and `mark` never allocate.
