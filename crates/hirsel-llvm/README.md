# hirsel-llvm

The LLVM engine: CoreIR emission through owned LLVM handles. `core::emit_ir*` prints a
module's LLVM IR and `core::emit_object*` compiles it to an ELF object for x86-64 or
AArch64 Linux (`core::supports_target`). `Optimization::None` is the development level and
`Optimization::Speed` the release level; both keep checked arithmetic. The emitter trusts
its producer's CoreIR and layout recipes: production emission neither validates CoreIR
nor runs the LLVM verifier. Executable differential tests use the CoreIR evaluator and
independent C callers.

Dependencies: `hirsel-core-ids`, `hirsel-core-layout` and `hirsel-core-ir`, and
`llvm-sys = "=231.0.1"` with `strict-versioning` and `force-dynamic`, bound to the shared
libLLVM 23.1.5 pinned in `tools/native-toolchain/llvm-23.lock`. `.cargo/config.toml`
points `LLVM_SYS_231_PREFIX` at the pinned install; an environment value wins, for
example a local `tools/native-toolchain/install --unpack` prefix. There is no fallback to
another LLVM.

Invariants:

- No raw LLVM handle is public. `context.rs` owns every handle (`Context`, `Module`,
  `TargetMachine`) and disposes of each on drop; a `Module` borrows its `Context`, so no
  handle outlives its owner.
- Every function takes FORM's hidden task context before its destination and explicit
  parameters, matching `hirsel-cranelift`. Exports and function pointers return scalar
  results followed by the failure status; proven infallible bodies use a private
  direct-call ABI without it. `core::symbol` is the FORM instance link name every
  declaration, definition and caller shares.
- C results follow the Linux target convention (`abi.rs`, adapted from rustc_target):
  SysV eightbyte classes on x86-64, homogeneous floating aggregates or integer carriers
  and otherwise the sret register on AArch64.
- Integer arithmetic uses LLVM's overflow intrinsics with explicit failure successors;
  floats are single IEEE instructions with no fast-math flags.
- FORM storage roles are disjoint within one activation (`alias.rs`); scope declarations
  let LLVM clone those domains when it inlines. `HIRSEL_LLVM_NO_FACTS` suppresses every
  emitter-supplied optimization fact while keeping ABI attributes and stack probes, and
  drivers key native objects on that choice (`core::facts_enabled`).
- Objects install 4 KiB stack probes, so native stack exhaustion is a run stop at a probed
  guard.
- Variant tags are read and written only through `hirsel-core-layout`'s `read_case` and
  `case_word` (`case.rs`), so niche and direct variants are told apart in one place.
- The runtime prefix's fast paths (`fast.rs`) cross the C ABI only for allocation and
  refill, nonlocal payers, compound resource construction and last-holder cleanup.
  Buffer growth (`growth.rs`) is one threshold test per site; large buffers call the
  shared runtime replacement.
- `sink.rs` is the one construction interface: lowering describes each item once, in
  final module order. Objects use the native LLVM builder; the text producer exists only
  for the printed-module oracle and diagnostic IR dumps, and no product path parses it.
- `debug.rs` extracts LLVM's DWARF line rows once, before a native product is published.

Source-program agreement lives in `hirsel-core-program-tests`, whose native
fixtures emit the driver's reached definition units. Hand-built CoreIR and target
ABI laws remain here. Admission injection and cleanup traces are explicit test
emission options, so moving a law between crates preserves its observations.
