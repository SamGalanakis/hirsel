# hirsel-core-ir

CoreIR: the lowered form the one-pass front end emits and both native emitters read.
Data types (`src/ir.rs`, extracted from `hirsel-lir`), the text form (`src/text.rs`),
a test-only well-formedness function (`src/wf.rs`, feature `wf`) and one sample per
operation family (`samples/*.core`, each with its source program and hand-derived
`run:` lines that the tests execute). The crate docs (`src/lib.rs`) state which parts
are frozen at FORM v0 and which wait for FORM v1, the calling convention and the
instance rules.
