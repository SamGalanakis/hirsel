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
    compile_ir_for_target(ir, speed, hirsel_core_layout::Target::X86_64Linux)
}

/// Compile LLVM text using the same target as its producer's calling convention.
pub fn compile_ir_for_target(
    ir: &str,
    speed: bool,
    target: hirsel_core_layout::Target,
) -> Result<Vec<u8>, String> {
    loader::compile(ir, speed, target)
}

/// Version of the atomic object, raw bitcode and semantic-summary product.
pub const NATIVE_LIBRARY_SCHEMA: u32 = 2;
/// Raw module transport; ordinary O3 and its existing bounded late round.
pub const IMPORT_PIPELINE_ID: &str = "raw-module-v1-o3-late";
/// Preserve the pre-SSA 2048 selection after local/cross-block SSA: float.scan
/// shrank 1758 -> 1384 -> 1130; excluded json.write_string 2167 -> 1731 -> 1469.
/// This remains far above LLVM's 250 weighted O3 inline-cost threshold.
pub const INLINE_IMPORT_INSTRUCTION_LIMIT: u64 = 1280;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImportFunction {
    pub symbol: String,
    /// Instructions in the public body and its private infallible implementation.
    pub instructions: u64,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImportSummary {
    pub functions: Vec<ImportFunction>,
    pub facts: Vec<hirsel_core_ir::roles::BodyFacts>,
}
impl ImportSummary {
    /// Whether a public body is an inline candidate. Keep the unit itself: its
    /// declarations and private helpers still determine composition order.
    pub fn selected(&self) -> bool {
        self.functions
            .iter()
            .any(|f| f.instructions <= INLINE_IMPORT_INSTRUCTION_LIMIT)
    }
}
#[derive(Clone, Debug)]
pub struct NativeLibrary {
    pub object: hirsel_core_ir::source::NativeObject,
    pub bitcode: Vec<u8>,
    pub summary: ImportSummary,
}
/// Publish the canonical object and the exact raw emitter module together.
/// The caller derives facts over the complete publication/dependency graph.
pub fn compile_library(
    module: &hirsel_core_ir::Module,
    facts: &hirsel_core_ir::roles::Facts,
    target: hirsel_core_layout::Target,
    observe_charge: bool,
    debug_lines: bool,
    object_key: hirsel_core_ids::ContentHash,
) -> Result<NativeLibrary, String> {
    let ir = core::emit_ir_relocatable_with_role_facts_for_target(
        module,
        target,
        observe_charge,
        debug_lines,
        facts,
    )
    .map_err(|e| e.to_string())?;
    let output = loader::library(&ir, target)?;
    Ok(NativeLibrary {
        object: core::finish_native_object(output.object, object_key, debug_lines)?,
        bitcode: output.bitcode,
        summary: ImportSummary {
            functions: output.functions,
            facts: facts.record(module),
        },
    })
}
/// Application-owned text or an inline candidate from a published library.
pub enum Unit<'a> {
    Text {
        ir: &'a str,
        imported: bool,
    },
    Import {
        bitcode: &'a [u8],
        summary: &'a ImportSummary,
    },
}

/// One release codegen unit. LLVM resolves instance symbols, internalizes all
/// owned definitions except `exports`, then runs the ordinary O3 pipeline.
pub fn emit_program(units: &[Unit<'_>], exports: &[String]) -> Result<Vec<u8>, String> {
    emit_program_for_target(units, exports, hirsel_core_layout::Target::X86_64Linux)
}

/// Compose a release unit for the requested native target.
pub fn emit_program_for_target(
    units: &[Unit<'_>],
    exports: &[String],
    target: hirsel_core_layout::Target,
) -> Result<Vec<u8>, String> {
    loader::program(units, exports, target)
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
