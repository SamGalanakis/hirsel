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
        emit_program_parts(units, exports, target, 0, &mut Unheld, Lend::None)?
            .objects
            .pop()
            .expect("an unreused program is generated"),
    )
}

/// Checked operations folded by CounterRangePass in named functions, keyed by LLVM symbol bytes.
pub type CounterRangeFolds = std::collections::BTreeMap<Vec<u8>, usize>;

/// The composed release object and folds recorded during that compilation.
#[doc(hidden)]
pub fn emit_program_with_counter_range_folds_for_test(
    units: &[Unit<'_>],
    exports: &[String],
    target: hirsel_core_layout::Target,
) -> Result<(Vec<u8>, CounterRangeFolds), String> {
    let mut folds = CounterRangeFolds::new();
    let mut program = loader::program(
        lower_program_units(units, target)?,
        exports,
        target,
        0,
        &mut Unheld,
        Some(&mut folds),
        Lend::None,
    )?;
    Ok((
        program
            .objects
            .pop()
            .expect("an unreused program is generated"),
        folds,
    ))
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
    /// The bodies a library program lends ([`Lend::Lend`]), as bitcode.
    pub lent: Vec<u8>,
    /// A library program's parts ([`Lend::Lend`]), in part order, none
    /// generated: a program over the library generates the parts it links
    /// ([`generate_parts`]).
    pub deferred: Vec<Deferred>,
    /// Source-neutral decisions reported by the passes that made them.
    pub remarks: Vec<hirsel_core_ir::source::CodegenRemark>,
}

/// What a release program lends or borrows of a separately generated library.
#[derive(Clone, Copy)]
pub enum Lend<'a> {
    None,
    /// The program is over a library that lent these bodies: its inliner may
    /// copy one into a caller; each call it keeps reaches the library's part.
    Borrow(&'a [u8]),
    /// The program is a library: [`Program::lent`] gets each body of at most
    /// `limit` instructions, under the name its part's object defines once
    /// each local definition's name has [`LENT_NAMESPACE`] after it, and
    /// [`Program::deferred`] its parts, which are not generated.
    Lend {
        limit: usize,
        namespace: &'a str,
    },
}

/// Join the bodies lent by independently optimized modules into one import.
pub fn join_lent(modules: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    loader::join_lent(modules)
}

/// Compress a release std store record using the LLVM engine's zlib.
pub fn compress_std_record(bytes: &[u8]) -> Result<Vec<u8>, String> {
    loader::std_object_codec(bytes, 0)
}

/// Decompress a release std store record to its recorded byte size.
pub fn decompress_std_record(
    bytes: &[u8],
    size: std::num::NonZeroUsize,
) -> Result<Vec<u8>, String> {
    loader::std_object_codec(bytes, size.get())
}

/// What a library's link contract appends to each of its strong definitions
/// that it does not export.
pub const LENT_NAMESPACE: &str = "$std";
/// One out-of-line definition, by its content-derived symbol. The empty name
/// is a whole module that cannot be cut per definition: its only part.
pub struct Part {
    pub name: String,
    pub reused: bool,
}

/// A library part not generated: its definition's symbol (empty for a whole
/// module that cannot be cut), whether its object defines that symbol strong,
/// and the bitcode [`generate_parts`] generates its object from.
pub struct Deferred {
    pub name: String,
    pub strong: bool,
    pub code: Vec<u8>,
}

/// The objects of deferred library parts ([`Deferred::code`]), in order,
/// generated for `target` on up to `jobs` threads as their program generates
/// a part it does not defer, each alone. A part's object is the one its
/// program generates; [`core::split_native_object`] cuts it.
pub fn generate_parts(
    codes: &[&[u8]],
    target: hirsel_core_layout::Target,
    jobs: u32,
) -> Result<Vec<Vec<u8>>, String> {
    loader::generate(codes, target, jobs)
}

/// What a release program's caller holds from earlier programs.
pub trait Held {
    /// Whether the caller holds the object of the part named `name`, whose
    /// key (what its bitcode records) is `ir`, generated by the machine
    /// `machine` states.
    fn part(&mut self, name: &str, machine: &str, ir: &[u8; 32]) -> bool;
    /// The definition's part after the release tail, when the caller holds
    /// it for `key` (the machine text and the linkage the part does not keep)
    /// and the key `ir` of the definition's part at the cut.
    fn tail(&mut self, key: &[u8], ir: &[u8; 32]) -> Option<Vec<u8>>;
    /// The part after the tail for `key` and `ir`, which `tail` did not hold.
    /// `input` is the part's bitcode at the cut when `witness` asked for it,
    /// and otherwise empty.
    fn keep_tail(&mut self, key: &[u8], ir: &[u8; 32], input: &[u8], tail: &[u8]);
    /// Whether `keep_tail` is to get the part's bitcode at the cut.
    fn witness(&self) -> bool {
        false
    }
}

/// A caller that holds nothing and keeps nothing.
pub struct Unheld;
impl Held for Unheld {
    fn part(&mut self, _: &str, _: &str, _: &[u8; 32]) -> bool {
        false
    }
    fn tail(&mut self, _: &[u8], _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
    fn keep_tail(&mut self, _: &[u8], _: &[u8; 32], _: &[u8], _: &[u8]) {}
}

pub struct ReleaseUnit {
    module: construction::NativeModule,
    pub sites: Vec<hirsel_core_ir::source::ReleaseSite>,
    pub debug_sites: Vec<hirsel_core_ir::source::ReleaseSite>,
}

impl ReleaseUnit {
    pub fn lower(
        module: &hirsel_core_ir::Module,
        facts: &hirsel_core_ir::roles::Facts,
        observe_charge: bool,
    ) -> Result<Self, String> {
        let context = std::rc::Rc::new(construction::Context::with_anonymous_locals(true)?);
        let neutral = core::step_neutral_program(&[module]);
        let (module, sites, debug_sites) =
            core::emit_release_unit(module, context, facts, observe_charge, &neutral)
                .map_err(|error| error.to_string())?;
        Ok(Self {
            module,
            sites,
            debug_sites,
        })
    }

    /// What the unit's module records before whole-program optimisation.
    pub fn ir_key(&self) -> hirsel_core_ids::ContentHash {
        self.module.ir_key()
    }

    pub fn emit(
        self,
        exports: &[String],
        jobs: u32,
        held: &mut dyn Held,
        lend: Lend<'_>,
    ) -> Result<Program, String> {
        loader::program(
            vec![loader::ProgramUnit::Native(self.module)],
            exports,
            hirsel_core_layout::Target::X86_64Linux,
            jobs,
            held,
            None,
            lend,
        )
    }
}

/// Compose a release program for `target` and generate it in parts, one per
/// out-of-line definition after whole-program optimisation. `held` is asked
/// for each definition's release tail and each part's object ([`Held`]). Each
/// part not held is generated from its bitcode alone on up to `jobs` threads;
/// with `jobs` 0 they are generated together as one object.
pub fn emit_program_parts(
    units: &[Unit<'_>],
    exports: &[String],
    target: hirsel_core_layout::Target,
    jobs: u32,
    held: &mut dyn Held,
    lend: Lend<'_>,
) -> Result<Program, String> {
    lower_program(units, target)?.emit(exports, jobs, held, lend)
}

/// A release program's units built as LLVM modules for `target`. They no
/// longer read the Core modules and facts they were built from, so a caller
/// may drop those before [`Lowered::emit`] runs the optimiser and the code
/// generator, which [`emit_program_parts`] runs at once.
pub struct Lowered {
    units: Vec<loader::ProgramUnit>,
    target: hirsel_core_layout::Target,
}

pub fn lower_program(
    units: &[Unit<'_>],
    target: hirsel_core_layout::Target,
) -> Result<Lowered, String> {
    Ok(Lowered {
        units: lower_program_units(units, target)?,
        target,
    })
}

impl Lowered {
    /// Optimise the whole program and generate it in parts, as
    /// [`emit_program_parts`] does.
    pub fn emit(
        self,
        exports: &[String],
        jobs: u32,
        held: &mut dyn Held,
        lend: Lend<'_>,
    ) -> Result<Program, String> {
        loader::program(self.units, exports, self.target, jobs, held, None, lend)
    }
}

fn lower_program_units(
    units: &[Unit<'_>],
    target: hirsel_core_layout::Target,
) -> Result<Vec<loader::ProgramUnit>, String> {
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
    let versions = (target == hirsel_core_layout::Target::X86_64Linux)
        .then(|| core::versioned_program(&modules));
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
                    versions.as_ref(),
                )
                .map_err(|e| e.to_string())?,
            )),
            Unit::Text(ir) => lowered.push(loader::ProgramUnit::Text((*ir).to_owned())),
        }
    }
    Ok(lowered)
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
    #[test]
    fn release_generated_caller_keeps_inlined_source_locations() {
        let module = hirsel_core_ir::text::parse(
            r#"core v1
item i0 = app@1/main/scale
sig g0 = (u64) -> (u64)
inst n0 = app@1/main/adapter [] []
inst n1 = app@1/main/scale [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = call f1 g0 (v0)
  return v1
}
fn f1 = n1 g0 {
b0(v0: u64):
  v1: u64 = const 3
  v2: u64 = mul v0, v1 at i0 18..23
  return v2
}
"#,
        )
        .unwrap();
        let program = super::emit_program_parts(
            &[super::Unit::Core {
                module: &module,
                facts: &hirsel_core_ir::roles::Facts::new(&module),
                observe_charge: false,
                debug_lines: true,
            }],
            &[core::symbol(&module, hirsel_core_ir::FuncId(0))],
            hirsel_core_layout::Target::X86_64Linux,
            1,
            &mut super::Unheld,
            super::Lend::None,
        )
        .unwrap();
        let [bytes] = program.objects.try_into().unwrap();
        let object = core::finish_native_object(
            bytes,
            hirsel_core_ids::ContentHash::of_bytes(b"release line table"),
            true,
        )
        .unwrap();
        let debug = object.debug.unwrap();
        let scale = hirsel_core_ids::DefPath::from_full("app@1/main/scale").unwrap();
        assert!(
            debug.sequences.iter().flat_map(|s| &s.rows).any(|row| {
                row.source.is_some_and(|location| {
                    debug.items[location.item as usize] == scale && location.start == 18
                })
            }),
            "release code lost the inlined multiplication at byte 18"
        );
    }
}
