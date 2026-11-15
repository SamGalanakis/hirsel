# hirsel-cranelift

The Cranelift engine, for fast development builds: CoreIR modules emitted as relocatable
ELF objects through Cranelift 0.136.3 (`cranelift-codegen`, `-frontend`, `-module`,
`-object`, pinned `=0.136.3`). It links with the same runtime
archive and calls at the same ABI as `hirsel-llvm`. Development builds use it by default;
`hirsel build --backend cranelift` selects it explicitly.

Dependencies: `hirsel-core-ids`, `hirsel-core-layout` and `hirsel-core-ir`, the four
Cranelift crates. No other implementation crate.

Interface (`core`):

- `emit_object(module, options)` for scalar CoreIR, and `emit_object_with_layouts`, which
  also takes the producer's layout recipes for declared storage and destinations. Both
  return a relocatable object whose symbols are CoreIR instance identities.
  `emit_object_profiled` and `emit_object_with_charge_observation` are the measurement
  and charge-observation variants.
- `Options { target, optimize, debug_lines }`: x86-64 or AArch64 Linux
  (`supports_target`); `optimize` is Cranelift's `speed`, otherwise `none`;
  `debug_lines` emits source-independent line and subprogram rows for link-time binding.

Invariants:

- Every function takes the hidden task context before its destination and parameters
  and returns its C scalar-result struct with a trailing u8 failure status. A call tests
  that status once; only the instruction that raises a failure records a new cause
  (`failure.rs`).
- C results follow aggregate classification on System V and AAPCS64, not source arity
  (`abi.rs`).
- Place lowering reads the producer's layout recipe (`place.rs`); it never derives a
  second layout, infers ownership or checks its producer's decisions. Variant tags go
  through `hirsel-core-layout`'s `read_case` and `case_word` (`case.rs`).
- Counted cleanup yields one descendant at a time to the runtime's intrusive worklist,
  so pointer depth uses no native stack (`drop.rs`).
- Frozen runtime prefixes supply the common allocation path (`fast.rs`); buffers above
  the native payload threshold call the shared runtime growth routine (`growth.rs`).
- Failure code names stable item identities only; files and lines are debug and link
  inputs (`sites.rs`).
- Objects install 4 KiB inline stack probes, tested against the runtime guard. The
  Cranelift verifier does not run in production emission.
- On x86-64 the one canonical object stream (`object.rs`) validates RIP-relative GOT
  load and indirect call/jump encodings before marking them `REX_GOTPCRELX`/`GOTPCRELX`,
  so lld can relax references that bind locally. Other references keep their original
  relocation.

Native tests compile every CoreIR sample and front-end run case to an object, link it
with a separately compiled C caller and the core runtime archive, and compare values,
cause chains and logical balances with the CoreIR evaluator, on x86-64 and on AArch64
under qemu (`//:hirsel-cranelift-test-core_native`, `-core_aarch64`).
