# hirsel-codec

The canonical S-expression snapshot codec shared by every level of the compiler:
one text encoding serves as the normalized dump, the round-trip format and the
digest input (see the crate documentation for the invariants).

A shared data crate. It depends only on `hirsel-core-ids`. Contexts (`Snapshot<C>`)
let encoders carry lookup tables without changing the text format.
