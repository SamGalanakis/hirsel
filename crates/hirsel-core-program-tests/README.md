# Whole-program laws

These tests check source through the driver, retain its per-definition products,
and walk their reached instances. The evaluator reads the joined units. Native
C callers compile the individual units; LLVM release composes those units with
the caller's requested exports. Values, failure sites, charges and fault oracles
stay in the laws.

`hirsel-core-program-tests-unittest` owns the front corpus, std seed and source
lowering laws. `hirsel-core-program-tests-test-cranelift_source` and its
`_aarch64` counterpart own Cranelift source agreement. The LLVM source target is
`hirsel-core-program-tests-test-llvm_source`; its ignored AArch64 bodies run in
`core-program-llvm-aarch64-runtime`. Hand-built IR laws remain in the engines.

The `core_emit` and `core_profile` examples write one object per reached unit,
as `<output>.<unit>`. `core_lower input.hirsel output-directory` writes separate
`unit-<n>.core` files for measurements. They use the same checked source route.
