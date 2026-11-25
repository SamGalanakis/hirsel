//! Release-optimized IR text for lowering checks in the program tests.

/// The release-optimized IR text of the same native law fixture, for lowering checks.
#[doc(hidden)]
#[cfg(not(hirsel_lazy_core))]
pub fn emit_optimized_ir_for_test(
    module: &super::Module,
    target: super::Target,
) -> super::Result<String> {
    super::compile_observation(
        module,
        target,
        super::Optimization::Speed,
        false,
        super::SourceEmission {
            batch_steps: true,
            test_hooks: true,
            relocatable_sites: true,
            debug_lines: false,
            fixed_sites: &[],
        },
        |llvm| llvm.text(),
        |_| (),
    )
    .map(|(_, ir)| ir)
}
