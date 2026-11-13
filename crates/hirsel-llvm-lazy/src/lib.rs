//! CoreIR text emission without loading LLVM. The embedded native component is
//! mapped once, on the first object emission; its bytes participate in the
//! compiler executable's link identity. There is no external backend selection.
#[cfg(test)]
hirsel_guard::install!();

#[path = "../../hirsel-llvm/src/core/mod.rs"]
pub mod core;
mod loader;

/// Compile compiler-produced LLVM text through the same once-loaded component.
/// This is also the object-parity boundary used by the direct engine's tests.
pub fn compile_ir(ir: &str, speed: bool) -> Result<Vec<u8>, String> {
    loader::compile(ir, speed)
}

/// An already-published body, emitted with its unit-local type/static tables.
pub struct Unit<'a> {
    pub ir: &'a str,
    /// Imported bodies are available for inlining; surviving calls link their
    /// separately cached library object instead of emitting a second copy.
    pub imported: bool,
}

/// One release codegen unit. LLVM resolves instance symbols, internalizes all
/// owned definitions except `exports`, then runs the ordinary O3 pipeline.
pub fn emit_program(units: &[Unit<'_>], exports: &[String]) -> Result<Vec<u8>, String> {
    loader::program(units, exports)
}

#[cfg(test)]
mod tests {
    use crate::core;
    #[test]
    fn llvm_is_unmapped_until_first_object_and_loaded_once() {
        let maps = || std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(!maps().contains("libLLVM"));
        let module = hirsel_core_ir::Module::default();
        core::emit_ir(&module).unwrap();
        assert!(!maps().contains("libLLVM"));
        core::emit_object(&module, core::Optimization::None).unwrap();
        let loaded = maps();
        assert!(loaded.contains("libLLVM"));
        assert!(loaded.contains("memfd:hirsel-llvm"));
        core::emit_object(&module, core::Optimization::Speed).unwrap();
        let component_lines = |s: &str| {
            s.lines()
                .filter(|line| line.contains("memfd:hirsel-llvm"))
                .count()
        };
        assert_eq!(component_lines(&loaded), component_lines(&maps()));
    }
}
