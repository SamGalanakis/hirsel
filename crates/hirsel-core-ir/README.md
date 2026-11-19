# hirsel-core-ir

CoreIR is the lowered form the one-pass front end emits and the evaluator, Cranelift
and LLVM consume.
Data types (`src/ir.rs`), the text form (`src/text.rs`),
a test-only well-formedness function (`src/wf.rs`, feature `wf`) and one sample per
operation family (`samples/*.core`, each with its source program and hand-derived
`run:` lines that the tests execute). The crate docs (`src/lib.rs`) state which parts
are frozen (FORM v0, v1a, v1b) and which are still a draft (compact sequences and the
destination writer), the failure,
storage and admission rules, the runtime calls, the calling convention, the native
ABI and the instance rules. The samples are `failure`, `cleanup`, `admission`,
`counted`, `custody` and `variants` (v1b: `case`, `set_case`, inline arrays, `never`
calls); their run lines check exact `used` totals and the order resources were cleaned.

The `net` sample uses explicit harness-owned descriptors and grant rules instead
of ordinary run lines. `src/net_tests.rs` checks descriptor release and zero
admission on split. The evaluator-only `eval` feature uses the shared socket
ownership and rule code in `hirsel-core-net`; the data-only build does not.

The evaluator uses core-net's deterministic model for connect, listen, accept,
recv and send. Network readiness precedes virtual timers, and idle time jumps
to the next model event or deadline. Network harnesses configure the shared
model through `Network.sockets`; descriptor-only ownership fixtures remain
supported by the same registry.

SEM-11 vectors are 16- or 32-byte values of a fixed integer lane type. The
text form prints them as `v16xu8`, `v8xi16`, and so on. Each vector value
operation has its own `SimdOp`, and `PutLanes` writes a prefix of a byte vector.
The front end emits one bounds check for each list block operation before
the load or store. The store reserves only the appended length.

A function's text header carries `versioned` when the checker finds a vector
type after typing, including those it inferred. Closures carry their own
marks. The evaluator and Cranelift operate lane by lane. LLVM emits fixed
vector IR and one function body for the target.
