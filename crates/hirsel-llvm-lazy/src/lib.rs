//! CoreIR text emission without loading LLVM. The embedded native component is
//! mapped once, on the first object emission; its bytes participate in the
//! compiler executable's link identity. There is no external backend selection.
#[cfg(test)]
hirsel_guard::install!();

mod construction;
#[path = "../../hirsel-llvm/src/core/mod.rs"]
pub mod core;
mod loader;
pub use loader::llvm_library;

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

/// A Core unit of the program, or foreign LLVM text. `Text` is for hand-written
/// engine composition cases and text differentials.
pub enum Unit<'a> {
    Core {
        module: &'a hirsel_core_ir::Module,
        facts: &'a hirsel_core_ir::roles::Facts,
        observe_charge: bool,
        debug_lines: bool,
    },
    Text(&'a str),
}
/// A release program, every part generated as one object: LLVM resolves
/// instance symbols, internalizes all owned definitions except `exports`, runs
/// the ordinary O3 pipeline over the whole program, then generates code once
/// ([`emit_program_parts`]). The object has a section per definition.
pub fn emit_program(units: &[Unit<'_>], exports: &[String]) -> Result<Vec<u8>, String> {
    emit_program_for_target(units, exports, hirsel_core_layout::Target::X86_64Linux)
}

/// [`emit_program`] for the requested native target.
pub fn emit_program_for_target(
    units: &[Unit<'_>],
    exports: &[String],
    target: hirsel_core_layout::Target,
) -> Result<Vec<u8>, String> {
    Ok(
        emit_program_parts(units, exports, target, 0, &mut |_, _, _| false)?
            .objects
            .pop()
            .expect("an unreused program is generated"),
    )
}

/// A release program's parts after whole-program optimisation, in order, and
/// the objects generated for the parts not reused.
#[derive(Default)]
pub struct Program {
    pub parts: Vec<Part>,
    /// One object per part not reused, in part order, or one object with a
    /// section per such part; [`core::split_native_object`] cuts either per
    /// part. Empty when every part was reused.
    pub objects: Vec<Vec<u8>>,
}
/// One out-of-line definition, by its content-derived symbol. The empty name
/// is a whole module that cannot be cut per definition: its only part.
pub struct Part {
    pub name: String,
    pub reused: bool,
}

/// Compose a release program for `target` and generate it in parts, one per
/// out-of-line definition after whole-program optimisation. `reuse` sees each
/// part's definition name, the code generator's machine text and the part's
/// bitcode, in order, and answers whether the caller holds its object. Each
/// part not held is generated from its bitcode alone on up to `jobs` threads;
/// with `jobs` 0 they are generated together as one object.
pub fn emit_program_parts(
    units: &[Unit<'_>],
    exports: &[String],
    target: hirsel_core_layout::Target,
    jobs: u32,
    reuse: &mut dyn FnMut(&str, &str, &[u8]) -> bool,
) -> Result<Program, String> {
    let context = units
        .iter()
        .any(|unit| matches!(unit, Unit::Core { .. }))
        .then(|| construction::Context::new().map(std::rc::Rc::new))
        .transpose()?;
    let modules = units
        .iter()
        .filter_map(|unit| match unit {
            Unit::Core { module, .. } => Some(*module),
            Unit::Text(_) => None,
        })
        .collect::<Vec<_>>();
    let neutral = core::step_neutral_program(&modules);
    let mut lowered = Vec::with_capacity(units.len());
    for unit in units {
        match unit {
            Unit::Core {
                module,
                facts,
                observe_charge,
                debug_lines,
            } => lowered.push(loader::ProgramUnit::Native(
                core::emit_native_module_in_context(
                    module,
                    target,
                    context.as_ref().unwrap().clone(),
                    facts,
                    *observe_charge,
                    *debug_lines,
                    &neutral,
                )
                .map_err(|e| e.to_string())?,
            )),
            Unit::Text(ir) => lowered.push(loader::ProgramUnit::Text(ir)),
        }
    }
    loader::program(lowered, exports, target, jobs, reuse)
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
