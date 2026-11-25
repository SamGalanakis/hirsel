# Spatial index

`spatial@1/spatial` indexes points of the u32 square in a store entry index,
keyed by their position on the order-32 Hilbert curve (8 big-endian bytes).
This is form A of the store end-state design: an ordinary `Many` index, built,
maintained and revisioned like any other.

Declare it with `spatial.index(mut d, mut table, name, by)`, where `by` gives
the points of a row. Inside a transaction or a `store.look`, call
`spatial.within(tx, ix, b)` with a box from `spatial.box(x_lo, y_lo, x_hi, y_hi)`.
Box edges are inclusive; a box whose low edge is above its high edge is `None`.
`within` returns each entry whose point lies in the box as `(point, row key)`,
once per entry, so a row with several points in the box appears once for each.

A query splits the box into at most `RANGES` (64) key ranges. It refines
aligned quadtree cells level by level while the cover fits that budget, and
reads the cells still partial at the cut whole. Every entry read is then tested
against the box, so the budget bounds work and never changes the answer.
`spatial.ranges(b, budget)` returns that cover.

Form B, a packed Hilbert R-tree projector generation, waits for a data set
that needs it.
