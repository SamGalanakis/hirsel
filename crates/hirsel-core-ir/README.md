# hirsel-core-ir

CoreIR: the lowered form the one-pass front end emits and both native emitters read.
Data types (`src/ir.rs`, extracted from `hirsel-lir`), the text form (`src/text.rs`),
a test-only well-formedness function (`src/wf.rs`, feature `wf`) and one sample per
operation family (`samples/*.core`, each with its source program and hand-derived
`run:` lines that the tests execute). The crate docs (`src/lib.rs`) state which parts
are frozen (FORM v0, FORM v1a) and which are still a draft (FORM v1b), the failure,
storage and admission rules, the runtime calls, the calling convention, the native
ABI and the instance rules. The v1a samples are `failure`, `cleanup`, `admission`,
`counted` and `custody`; their run lines check exact `used` totals and the order
resources were cleaned.
