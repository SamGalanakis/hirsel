# hirsel-core-ir

CoreIR: the lowered form the one-pass front end emits and both native emitters read.
Data types (`src/ir.rs`, extracted from `hirsel-lir`), the text form (`src/text.rs`),
a test-only well-formedness function (`src/wf.rs`, feature `wf`) and one sample per
operation family (`samples/*.core`, each with its source program and hand-derived
`run:` lines that the tests execute). The crate docs (`src/lib.rs`) state which parts
are frozen (FORM v0, v1a, v1b) and which are still a draft (compact sequences and the
destination writer), the failure,
storage and admission rules, the runtime calls, the calling convention, the native
ABI and the instance rules. The samples are `failure`, `cleanup`, `admission`,
`counted`, `custody` and `variants` (v1b: `case`, `set_case`, inline arrays, `never`
calls); their run lines check exact `used` totals and the order resources were cleaned.
