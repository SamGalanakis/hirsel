//! Direct emission of CoreIR functions, failure edges and storage recipes. No old lowered program,
//! staging, manifest or receipt participates in this entry point.

/// Frozen native memory layout, shared with hirsel-core-rt.
pub mod memory_layout {
    include!("../../../hirsel-core-rt/src/abi_layout.rs");
}

mod abi;
mod alias;
mod bytes;
mod case;
mod cleanup;
mod collections;
mod empty;
mod fast;
mod forward;
mod scalar;
mod sites;
mod storage;
#[cfg(all(test, not(hirsel_lazy_core)))]
mod target_tests;
mod task_entry;
mod tasks;
#[cfg(all(test, not(hirsel_lazy_core)))]
mod tests;
mod values;

use std::collections::BTreeSet;
use std::fmt::{self, Write};

use hirsel_core_ir::failure;
use hirsel_core_ir::*;
use hirsel_core_layout::Target;

#[cfg(not(hirsel_lazy_core))]
use crate::context::{Context, Module as LlvmModule, TargetMachine};

/// Optimisation of the generated object. All levels preserve checked arithmetic.
#[derive(Clone, Copy, Debug, Default)]
pub enum Optimization {
    #[default]
    None,
    Speed,
}

#[derive(Clone, Copy)]
struct SourceEmission {
    relocatable_sites: bool,
    debug_lines: bool,
}

/// An operation whose native contract has not been implemented yet, or a tool error.
#[derive(Debug)]
pub enum Error {
    Unsupported(String),
    Llvm(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(s) => write!(f, "CoreIR LLVM emission does not yet implement {s}"),
            Self::Llvm(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// The FORM instance link name, shared by declarations, definitions and callers.
pub fn symbol(module: &Module, function: FuncId) -> String {
    let mut name = String::from("h");
    for byte in module.code_identity(function).0 {
        write!(name, "{byte:02x}").unwrap();
    }
    name
}

/// Whether to use the producer's optimization facts. Setting the diagnostic
/// switch suppresses every emitter-supplied fact, while retaining ABI attributes
/// and stack probes. Drivers include this choice in native object cache keys.
pub fn facts_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("HIRSEL_LLVM_NO_FACTS").is_none())
}

/// Whether this engine implements the requested target's native ABI.
pub fn supports_target(triple: &str) -> bool {
    Target::from_triple(triple).is_some()
}

/// Emit x86-64 System V LLVM IR directly from CoreIR.
///
/// Functions take FORM v1a's hidden task context before the destination.
/// Exports and function pointers return scalar results followed by failure status.
/// Proven infallible bodies use a private direct-call ABI without that status.
/// This function trusts its producer's IR and layout recipes.
pub fn emit_ir(module: &Module) -> Result<String> {
    emit_ir_with_charge_observation(module, false)
}

/// Optional diagnostic emission; timings of these objects are not performance samples.
pub fn emit_ir_with_charge_observation(module: &Module, observe_charge: bool) -> Result<String> {
    emit_ir_for_target(module, Target::X86_64Linux, observe_charge)
}

/// Emit CoreIR with the requested Linux calling convention.
pub fn emit_ir_for_target(module: &Module, target: Target, observe_charge: bool) -> Result<String> {
    emit_ir_target_with_facts(
        module,
        target,
        observe_charge,
        facts_enabled(),
        SourceEmission {
            relocatable_sites: false,
            debug_lines: false,
        },
    )
}

#[cfg(all(test, not(hirsel_lazy_core)))]
fn emit_ir_with_facts(module: &Module, observe_charge: bool, facts: bool) -> Result<String> {
    emit_ir_target_with_facts(
        module,
        Target::X86_64Linux,
        observe_charge,
        facts,
        SourceEmission {
            relocatable_sites: false,
            debug_lines: false,
        },
    )
}

fn emit_ir_target_with_facts(
    module: &Module,
    target: Target,
    observe_charge: bool,
    facts: bool,
    source: SourceEmission,
) -> Result<String> {
    let mut text = String::new();
    for (i, data) in module.statics.iter().enumerate() {
        let bytes = data
            .bytes
            .iter()
            .map(|b| format!("\\{b:02X}"))
            .collect::<String>();
        writeln!(
            text,
            "@s{i} = private constant [{} x i8] c\"{bytes}\", align {}",
            data.bytes.len(),
            data.align
        )
        .unwrap();
    }
    let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
    let mut intrinsics = BTreeSet::new();
    let mut next_scope = 2;
    let failures = failure::Failures::new(module);
    let debug = if source.debug_lines {
        sites::debug_plan(module, &failures, &mut next_scope, &mut intrinsics)
    } else {
        module.functions.iter().map(|_| None).collect()
    };
    let role_facts = facts.then(|| hirsel_core_ir::roles::Facts::new(module));
    for (i, f) in module.functions.iter().enumerate() {
        let sig = &module.signatures[f.signature.index()];
        let has_status = failures.may_fail[i];
        let abi = if has_status || f.body.is_none() {
            abi::Return::internal_on_target(target, &sig.results)
        } else {
            abi::Return::on_target(target, &sig.results)
        };
        let name = symbol(module, FuncId(i as u32));
        let params = abi.parameters(sig, facts);
        let Some(body) = &f.body else {
            writeln!(
                text,
                "declare {} @{name}({})",
                abi.return_type(),
                params.join(", ")
            )
            .unwrap();
            continue;
        };
        let definition = module.instances[f.instance.index()].definition.to_string();
        let append_fast = definition == "core@1/string/push_char";
        let function_debug = debug[i].as_ref();
        let aliases = role_facts.as_ref().and_then(|r| {
            alias::Scopes::new(
                module,
                r,
                FuncId(i as u32),
                &name,
                &mut next_scope,
                &mut intrinsics,
            )
        });
        let mut e = FunctionEmitter {
            function: Some(FuncId(i as u32)),
            append_fast,
            caller_site: "%caller_site",
            debug_location: function_debug.map(|d| d.location(None)),
            anchor: None,
            relocatable_sites: source.relocatable_sites,
            value_slots: values::slots(body),
            aliases,
            module,
            body,
            recipes: &recipes,
            abi,
            has_status,
            failures: &failures,
            constants: &failures.constants[i],
            observe_charge,
            facts,
            empty_bounds: if observe_charge {
                empty::EmptyBounds::default()
            } else {
                empty::EmptyBounds::new(body, &failures.constants[i])
            },
            code: String::new(),
            allocations: String::new(),
            next: 0,
            intrinsics: &mut intrinsics,
        };
        for (i, t) in body.values.iter().enumerate() {
            if e.value_slots[i] {
                writeln!(e.allocations, "  %v{i} = alloca {}", ty(*t)).unwrap();
            }
        }
        for (i, t) in body.slots.iter().enumerate() {
            let (bytes, align) = e.place_layout(*t);
            writeln!(
                e.allocations,
                "  %s{i} = alloca [{bytes} x i8], align {align}"
            )
            .unwrap();
        }
        for i in 0..body.addresses.len() {
            writeln!(e.allocations, "  %a{i} = alloca ptr").unwrap();
        }
        // Cleanup continuations may mention a branch-local constant even when
        // that branch is inactive. Constants have no effects or operands, so
        // define them in the entry rather than imposing LLVM dominance on the
        // producer's cleanup CFG.
        for (index, inst) in body.insts.iter().enumerate() {
            if matches!(inst, Inst::Const { .. }) {
                e.debug_location = function_debug.map(|d| d.location(body.inst_anchors[index]));
                e.inst(inst, index)?;
            }
        }
        for (i, v) in body.blocks[0]
            .params
            .get(&body.pools.values)
            .unwrap()
            .iter()
            .enumerate()
        {
            let parameter = if sig.dest.is_some() && i == 0 {
                "%dest".into()
            } else {
                format!("%p{}", i - usize::from(sig.dest.is_some()))
            };
            e.put(*v, &parameter);
        }
        if let Some(aliases) = &e.aliases {
            e.line(aliases.declaration());
        }
        e.line("br label %b0");
        let mut forwarding = forward::Forwarding::new(body);
        for (i, b) in body.blocks.iter().enumerate() {
            writeln!(e.code, "b{i}:").unwrap();
            let ending = forwarding.ending(module, &recipes, sig, e.constants, i);
            let mut returned = false;
            for (offset, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
                e.debug_location = function_debug
                    .map(|d| d.location(body.inst_anchors[b.insts.start as usize + offset]));
                if let Some(ending) = &ending
                    && offset == ending.prefix
                    && {
                        e.anchor = body.inst_anchors[b.insts.start as usize + offset];
                        e.return_call(&body.calls[ending.call.index()], ending)
                    }
                {
                    returned = true;
                    break;
                }
                if !matches!(inst, Inst::Const { .. }) {
                    e.inst(inst, b.insts.start as usize + offset)?;
                }
            }
            if !returned {
                e.anchor = b.anchor;
                e.debug_location = function_debug.map(|d| d.location(b.anchor));
                e.term(&body.terminators[i])?;
            }
        }
        let linkage = if has_status { "" } else { "internal " };
        let body_name = if has_status {
            name.clone()
        } else {
            format!("{name}.infallible")
        };
        // Inline the reserved ASCII append and share the UTF-8/growth body.
        let append_inline = match definition.as_str() {
            "core@1/string/push_char" => " alwaysinline",
            "core@1/string/push_scalar" => " noinline",
            _ => "",
        };
        let destination = sig
            .dest
            .filter(|_| sig.results.is_empty())
            .and_then(|t| {
                use hirsel_core_layout::{Layout, Repr};
                match (module.types[t.index()].repr, &recipes[t.index()].layout) {
                    (Repr::Resource | Repr::Stable, _) | (_, Layout::Fixed(_)) => {
                        let (bytes, _) = e.place_layout(PlaceTy::Declared(t));
                        matches!(bytes, 1..=16)
                            .then(|| format!(" \"hirsel-destination-size\"=\"{bytes}\""))
                    }
                    _ => None,
                }
            })
            .unwrap_or_default();
        writeln!(
            text,
            "define {linkage}{} @{body_name}({}){append_inline}{destination}{}{} {{\nentry:\n{}{} }}",
            e.abi.return_type(),
            params.join(", "),
            abi::view_input_marker(sig),
            function_debug
                .map(|d| format!(" !dbg !{}", d.scope))
                .unwrap_or_default(),
            e.allocations,
            e.code
        )
        .unwrap();
        if !has_status {
            e.infallible_wrapper(
                &name,
                sig,
                facts,
                &mut text,
                function_debug.and_then(|d| d.wrapper),
            );
        }
    }
    cleanup::drop_glue(
        module,
        target,
        &recipes,
        &failures,
        facts,
        &mut text,
        &mut intrinsics,
    )?;
    task_entry::emit(
        module,
        target,
        &recipes,
        &failures,
        facts,
        &mut text,
        &mut intrinsics,
    )?;
    for decl in intrinsics {
        writeln!(text, "{decl}").unwrap();
    }
    Ok(text)
}

/// Emit source-independent IR whose failure sites bind to the fresh link table.
/// `debug_lines` requests item-relative DWARF independently of cause-site emission.
pub fn emit_ir_relocatable_for_target(
    module: &Module,
    target: Target,
    observe_charge: bool,
    debug_lines: bool,
) -> Result<String> {
    emit_ir_target_with_facts(
        module,
        target,
        observe_charge,
        facts_enabled(),
        SourceEmission {
            relocatable_sites: true,
            debug_lines,
        },
    )
}

/// Compile CoreIR to an owned ELF object for x86-64 Linux. This uses the
/// retained LLVM target/handle machinery, without the old product pipeline.
/// The caller supplies its linker and entry harness; objects have FORM symbols.
pub fn emit_object(module: &Module, opt: Optimization) -> Result<Vec<u8>> {
    emit_object_with_charge_observation(module, opt, false)
}

/// Emit a labelled diagnostic object with exact logical-charge samples.
pub fn emit_object_with_charge_observation(
    module: &Module,
    opt: Optimization,
    observe_charge: bool,
) -> Result<Vec<u8>> {
    emit_object_for_target(module, Target::X86_64Linux, opt, observe_charge)
}

/// Compile for one supported Linux target with the same ABI on both LLVM paths.
pub fn emit_object_for_target(
    module: &Module,
    target: Target,
    opt: Optimization,
    observe_charge: bool,
) -> Result<Vec<u8>> {
    emit_object_site_binding(
        module,
        target,
        opt,
        observe_charge,
        SourceEmission {
            relocatable_sites: false,
            debug_lines: false,
        },
    )
}

/// Compile a cached native unit with item relocation symbols and optional synthetic DWARF.
/// File identities and byte offsets are source-independent; the driver binds both
/// cause words and debug rows against its current source table at the final link.
pub fn emit_object_relocatable_for_target(
    module: &Module,
    target: Target,
    opt: Optimization,
    observe_charge: bool,
    debug_lines: bool,
) -> Result<Vec<u8>> {
    emit_object_site_binding(
        module,
        target,
        opt,
        observe_charge,
        SourceEmission {
            relocatable_sites: true,
            debug_lines,
        },
    )
}

fn emit_object_site_binding(
    module: &Module,
    target: Target,
    opt: Optimization,
    observe_charge: bool,
    source: SourceEmission,
) -> Result<Vec<u8>> {
    #[cfg(not(hirsel_lazy_core))]
    {
        compile_observation(module, target, opt, observe_charge, source, |_| (), |_| ())
            .map(|(bytes, ())| bytes)
    }
    #[cfg(hirsel_lazy_core)]
    {
        crate::loader::compile(
            &emit_ir_target_with_facts(module, target, observe_charge, facts_enabled(), source)?,
            matches!(opt, Optimization::Speed),
            target,
        )
        .map_err(Error::Llvm)
    }
}

/// Exclusive compilation stages, for the developer measurement entry.
#[derive(Clone, Copy, Debug)]
pub enum CompilationStage {
    TextConstruction,
    TargetSetup,
    IrParse,
    Optimization,
    ObjectEmission,
    Disposal,
}

/// Observe stage boundaries without changing the generated program.
#[cfg(not(hirsel_lazy_core))]
pub fn emit_object_observed(
    module: &Module,
    opt: Optimization,
    observer: impl FnMut(CompilationStage),
) -> Result<Vec<u8>> {
    compile(module, opt, |_| (), observer).map(|(bytes, ())| bytes)
}

#[cfg(all(test, not(hirsel_lazy_core)))]
pub(crate) fn emitted_ir(module: &Module, opt: Optimization) -> Result<String> {
    compile(module, opt, |llvm| llvm.text(), |_| ()).map(|(_, ir)| ir)
}

#[cfg(not(hirsel_lazy_core))]
fn compile<R>(
    module: &Module,
    opt: Optimization,
    inspect: impl FnOnce(&LlvmModule<'_>) -> R,
    observer: impl FnMut(CompilationStage),
) -> Result<(Vec<u8>, R)> {
    compile_observation(
        module,
        Target::X86_64Linux,
        opt,
        false,
        SourceEmission {
            relocatable_sites: false,
            debug_lines: false,
        },
        inspect,
        observer,
    )
}

#[cfg(not(hirsel_lazy_core))]
fn compile_observation<R>(
    module: &Module,
    target: Target,
    opt: Optimization,
    observe_charge: bool,
    source: SourceEmission,
    inspect: impl FnOnce(&LlvmModule<'_>) -> R,
    mut observer: impl FnMut(CompilationStage),
) -> Result<(Vec<u8>, R)> {
    observer(CompilationStage::TextConstruction);
    let ir = emit_ir_target_with_facts(module, target, observe_charge, facts_enabled(), source)?;
    observer(CompilationStage::TargetSetup);
    let machine =
        TargetMachine::core(target, matches!(opt, Optimization::Speed)).map_err(Error::Llvm)?;
    observer(CompilationStage::IrParse);
    let context = Context::new();
    let module = LlvmModule::parse(&context, "hirsel-core", &ir).map_err(Error::Llvm)?;
    module.set_target(&machine);
    observer(CompilationStage::Optimization);
    if matches!(opt, Optimization::Speed) {
        module.run_core_passes(&machine).map_err(Error::Llvm)?;
    }
    observer(CompilationStage::ObjectEmission);
    let bytes = module.emit_object(&machine).map_err(Error::Llvm)?;
    let result = inspect(&module);
    observer(CompilationStage::Disposal);
    Ok((bytes, result))
}

fn ty(t: Ty) -> String {
    match t {
        Ty::Ptr => "ptr".into(),
        Ty::Scalar(Scalar::F32) => "float".into(),
        Ty::Scalar(Scalar::F64) => "double".into(),
        Ty::Scalar(s) => format!("i{}", s.size() * 8),
    }
}
fn bits_ty(t: Ty) -> String {
    format!("i{}", size(t) * 8)
}
fn size(t: Ty) -> u64 {
    match t {
        Ty::Ptr => 8,
        Ty::Scalar(s) => s.size(),
    }
}

struct FunctionEmitter<'a> {
    function: Option<FuncId>,
    module: &'a Module,
    body: &'a Body,
    recipes: &'a [hirsel_core_layout::Recipe],
    abi: abi::Return,
    has_status: bool,
    append_fast: bool,
    failures: &'a failure::Failures,
    constants: &'a [Option<u64>],
    empty_bounds: empty::EmptyBounds,
    observe_charge: bool,
    facts: bool,
    aliases: Option<alias::Scopes>,
    caller_site: &'static str,
    debug_location: Option<usize>,
    anchor: Option<Anchor>,
    relocatable_sites: bool,
    value_slots: Vec<bool>,
    code: String,
    allocations: String,
    next: usize,
    intrinsics: &'a mut BTreeSet<String>,
}
impl FunctionEmitter<'_> {
    fn c_argument<'a>(&self, t: &'a str) -> &'a str {
        if self.abi.target == Target::Aarch64Linux {
            t.strip_suffix(" zeroext")
                .or_else(|| t.strip_suffix(" signext"))
                .unwrap_or(t)
        } else {
            t
        }
    }
    fn line(&mut self, s: impl AsRef<str>) {
        let s = s.as_ref();
        let metadata = self.debug_location.filter(|_| {
            !s.ends_with(':')
                && !s.ends_with('[')
                && !s.as_bytes().first().is_some_and(|c| {
                    *c == b'i' && s.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                })
        });
        if let Some(location) = metadata {
            writeln!(self.code, "  {s}, !dbg !{location}").unwrap();
        } else {
            writeln!(self.code, "  {s}").unwrap();
        }
    }
    fn fresh(&mut self) -> String {
        let n = self.next;
        self.next += 1;
        format!("%t{n}")
    }
    fn calc(&mut self, s: impl AsRef<str>) -> String {
        let v = self.fresh();
        self.line(format!("{v} = {}", s.as_ref()));
        v
    }
    fn temp(&mut self, t: &str) -> String {
        let p = self.fresh();
        writeln!(self.allocations, "  {p} = alloca {t}, align 8").unwrap();
        p
    }
    fn vty(&self, v: ValueId) -> Ty {
        self.body.values[v.index()]
    }
    // Moves and numerical operations preserve arbitrary NaN representations.
    // Reinterpret from a float is the sole bit observer (FORM #97).
    fn numeric(&mut self, v: ValueId) -> String {
        if self.value_slots.get(v.index()).copied().unwrap_or(true) {
            self.calc(format!("load {}, ptr %v{}", ty(self.vty(v)), v.0))
        } else {
            format!("%v{}", v.0)
        }
    }
    fn put(&mut self, v: ValueId, x: &str) {
        if self.value_slots.get(v.index()).copied().unwrap_or(true) {
            self.line(format!("store {} {x}, ptr %v{}", ty(self.vty(v)), v.0));
        } else {
            // LLVM has no scalar copy instruction. This identity also permits
            // forward references when CoreIR blocks are not in dominance order.
            // It preserves poison; no freeze or representation promise is added.
            let t = ty(self.vty(v));
            self.line(format!("%v{} = select i1 true, {t} {x}, {t} poison", v.0));
        }
    }
    fn access_facts(&self, addresses: &[AddressId]) -> String {
        self.aliases
            .as_ref()
            .map_or_else(String::new, |a| a.access(addresses))
    }
    fn address(&mut self, a: AddressId) -> String {
        self.calc(format!("load ptr, ptr %a{}", a.0))
    }
    fn intrinsic(&mut self, name: &str, ret: &str, args: &[(String, String)]) -> String {
        self.intrinsics.insert(format!(
            "declare {ret} @{name}({})",
            args.iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        self.calc(format!(
            "call {ret} @{name}({})",
            args.iter()
                .map(|(t, v)| format!("{t} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
    fn canonical(&mut self, t: Ty, x: &str) -> String {
        if !t.is_float() {
            return x.into();
        }
        let nan = self.calc(format!("fcmp uno {} {x}, {x}", ty(t)));
        let bits = if size(t) == 4 {
            "2143289344"
        } else {
            "9221120237041090560"
        };
        self.calc(format!(
            "select i1 {nan}, {} bitcast ({} {bits} to {}), {} {x}",
            ty(t),
            bits_ty(t),
            ty(t),
            ty(t)
        ))
    }
    fn fail_if(&mut self, flag: &str, fail: Option<BlockId>, cause: u8) -> Result<()> {
        self.branch_failure(flag, fail, Some(cause));
        Ok(())
    }
    fn branch_failure(&mut self, flag: &str, fail: Option<BlockId>, cause: Option<u8>) {
        let n = self.next;
        self.next += 1;
        if let (None, Some(fail)) = (cause, fail) {
            // The callee has already recorded its cause. This edge has no
            // arguments or cleanup to perform before the existing successor.
            self.line(format!("br i1 {flag}, label %b{}, label %ok{n}", fail.0));
        } else {
            self.line(format!("br i1 {flag}, label %bad{n}, label %ok{n}"));
            writeln!(self.code, "bad{n}:").unwrap();
            if let Some(cause) = cause {
                self.raise(cause);
            }
            self.propagate_to(fail);
        }
        writeln!(self.code, "ok{n}:").unwrap();
    }
    fn propagate_to(&mut self, fail: Option<BlockId>) {
        if let Some(fail) = fail {
            self.line(format!("br label %b{}", fail.0));
        } else {
            self.return_failure();
        }
    }
    fn raise(&mut self, kind: u8) {
        let site = self.site_word();
        let fixed_site = site.parse::<u64>().ok();
        let shape = format!("[{} x i8]", memory_layout::CAUSE_BYTES);
        let argument = abi::composite_argument(self.abi.target, memory_layout::CAUSE_BYTES);
        self.intrinsics
            .insert(format!("declare void @hirsel_core_raise(ptr, {argument})"));
        // Built-in causes have fixed bytes. SysV byval supplies a caller copy;
        // AAPCS64 needs an explicit private copy before passing its pointer.
        // Neither convention lets the callee mutate the shared constant.
        let name = format!("hirsel_cause_{kind}_{}", fixed_site.unwrap_or(0));
        let prefix = memory_layout::CAUSE_KIND;
        let layout = format!("<{{ [{prefix} x i8], i64, i64 }}>");
        let word = memory_layout::cause_kind_word(kind) as i64;
        self.intrinsics.insert(format!(
            "@{name} = private constant {layout} <{{ [{prefix} x i8] zeroinitializer, i64 {word}, i64 {} }}>, align 8", fixed_site.unwrap_or(0)
        ));
        let pointer = if self.abi.target == Target::Aarch64Linux || fixed_site.is_none() {
            let pointer = self.temp(&shape);
            let words = format!("[{} x i64]", memory_layout::CAUSE_BYTES / 8);
            let value = self.calc(format!("load {words}, ptr @{name}, align 8"));
            self.line(format!("store {words} {value}, ptr {pointer}, align 8"));
            if fixed_site.is_none() {
                let at = self.calc(format!(
                    "getelementptr i8, ptr {pointer}, i64 {}",
                    memory_layout::CAUSE_SITE
                ));
                self.line(format!("store i64 {site}, ptr {at}, align 8"));
            }
            pointer
        } else {
            format!("@{name}")
        };
        self.line(format!(
            "call void @hirsel_core_raise(ptr %context, {argument} {pointer})"
        ));
    }

    fn inst(&mut self, inst: &Inst, index: usize) -> Result<()> {
        self.anchor = self.body.inst_anchors[index];
        match *inst {
            Inst::Const { result, bits } => {
                let t = self.vty(result);
                let x = if t.is_float() {
                    format!("bitcast ({} {bits} to {})", bits_ty(t), ty(t))
                } else if t == Ty::Ptr {
                    format!("inttoptr (i64 {bits} to ptr)")
                } else {
                    bits.to_string()
                };
                self.put(result, &x);
            }
            Inst::ReadStatic { result, data } => self.put(result, &format!("@s{}", data.0)),
            Inst::FunctionRef { result, function } => {
                self.put(result, &format!("@{}", symbol(self.module, function)))
            }
            Inst::Unary {
                result,
                op,
                operand,
                fail,
            } => {
                let t = self.vty(operand);
                let x = self.numeric(operand);
                let r = match op {
                    UnaryOp::Not => self.calc(format!("xor {} {x}, 1", ty(t))),
                    UnaryOp::BitNot => self.calc(format!("xor {} {x}, -1", ty(t))),
                    UnaryOp::Neg if t.is_float() => self.calc(format!("fneg {} {x}", ty(t))),
                    UnaryOp::Neg => {
                        let (v, flag) = self.overflow(t, ArithOp::Sub, "0", &x);
                        self.fail_if(&flag, fail, 0)?;
                        v
                    }
                };
                self.put(result, &r);
            }
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail,
            } => {
                let t = self.vty(left);
                let a = self.numeric(left);
                let x = self.binary(t, op, &a, right, fail)?;
                self.put(result, &x);
            }
            Inst::Compare {
                result,
                op,
                left,
                right,
            } => {
                let t = self.vty(left);
                let a = self.numeric(left);
                let b = self.numeric(right);
                let pred = match op {
                    CompareOp::Eq => "eq",
                    CompareOp::Ne => "ne",
                    CompareOp::Lt => "lt",
                    CompareOp::Le => "le",
                    CompareOp::Gt => "gt",
                    CompareOp::Ge => "ge",
                };
                let cmp = if t.is_float() {
                    format!(
                        "fcmp {}{pred} {} {a}, {b}",
                        if op == CompareOp::Ne { "u" } else { "o" },
                        ty(t)
                    )
                } else {
                    format!(
                        "icmp {}{pred} {} {a}, {b}",
                        if matches!(op, CompareOp::Eq | CompareOp::Ne) {
                            ""
                        } else if t.int().is_some_and(|x| x.0) {
                            "s"
                        } else {
                            "u"
                        },
                        ty(t)
                    )
                };
                let x = self.calc(cmp);
                let x = self.calc(format!("zext i1 {x} to i8"));
                self.put(result, &x);
            }
            Inst::IntArith {
                result,
                flag,
                op,
                mode,
                left,
                right,
            } => {
                let t = self.vty(left);
                let a = self.numeric(left);
                let b = self.numeric(right);
                let (r, over) = self.arith(t, op, mode, &a, &b);
                self.put(result, &r);
                if let Some(flag) = flag {
                    let over = self.calc(format!("zext i1 {over} to i8"));
                    self.put(flag, &over);
                }
            }
            Inst::Convert {
                result,
                flag,
                operand,
                mode,
            } => self.convert(result, flag, operand, mode),
            Inst::ScalarUnary {
                result,
                op,
                operand,
            } => {
                let t = self.vty(operand);
                let x = self.numeric(operand);
                let x = self.scalar_unary(t, op, &x);
                self.put(result, &x);
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                let t = self.vty(left);
                let a = self.numeric(left);
                let b = self.numeric(right);
                let c = self.numeric(addend);
                let x = self.intrinsic(
                    &format!("llvm.fma.{}", if size(t) == 4 { "f32" } else { "f64" }),
                    &ty(t),
                    &[(ty(t), a), (ty(t), b), (ty(t), c)],
                );
                self.put(result, &x);
            }
            Inst::Address { address, fail } => self.project(address, fail)?,
            Inst::Read {
                result, address, ..
            } => {
                let p = self.address(address);
                let metadata = self.access_facts(&[address]);
                let x = self.calc(format!(
                    "load {}, ptr {p}, align 1{metadata}",
                    ty(self.vty(result))
                ));
                self.put(result, &x);
            }
            Inst::Store { address, value } => {
                let p = self.address(address);
                let x = self.numeric(value);
                let metadata = self.access_facts(&[address]);
                self.line(format!(
                    "store {} {x}, ptr {p}, align 1{metadata}",
                    ty(self.vty(value))
                ));
            }
            // FORM v1c: a mark is the evaluator's alone; no code.
            Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::Mark { .. }
            | Inst::Unmark { .. } => {}
            Inst::UpdateMask {
                result,
                mask,
                field,
                initialized,
            } => {
                let t = ty(self.vty(mask));
                let x = self.numeric(mask);
                let k = if initialized {
                    1u64 << field
                } else {
                    !(1u64 << field)
                };
                let x = self.calc(format!(
                    "{} {t} {x}, {k}",
                    if initialized { "or" } else { "and" }
                ));
                self.put(result, &x);
            }
            Inst::Call { call } => self.call(&self.body.calls[call.index()])?,
            _ => self.storage(inst, index)?,
        }
        Ok(())
    }
    fn edge(&mut self, edge: EdgeId) -> String {
        let e = &self.body.edges[edge.index()];
        let args = e.args.get(&self.body.pools.values).unwrap();
        // Read every source before writing any target: loop parameters can swap.
        let values = args.iter().map(|v| self.numeric(*v)).collect::<Vec<_>>();
        for (v, x) in self.body.blocks[e.target.index()]
            .params
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(values)
        {
            self.put(*v, &x);
        }
        format!("b{}", e.target.0)
    }
    fn edge_label(&mut self, edge: EdgeId) -> String {
        let e = &self.body.edges[edge.index()];
        if e.args.len == 0 {
            return format!("b{}", e.target.0);
        }
        let n = self.next;
        self.next += 1;
        // Value transfers need separate blocks, including distinct switch arms
        // to the same target. Empty edges already name their final destination.
        format!("e{n}_{}", edge.0)
    }
    fn emit_edge(&mut self, label: &str, edge: EdgeId) {
        if self.body.edges[edge.index()].args.len == 0 {
            return;
        }
        writeln!(self.code, "{label}:").unwrap();
        let to = self.edge(edge);
        self.line(format!("br label %{to}"));
    }
    fn term(&mut self, term: &Terminator) -> Result<()> {
        match *term {
            Terminator::Jump(e) => {
                let to = self.edge(e);
                self.line(format!("br label %{to}"));
            }
            Terminator::Branch { condition, yes, no } => {
                let c = self.numeric(condition);
                let c = self.calc(format!("icmp ne i8 {c}, 0"));
                let y = self.edge_label(yes);
                let n = self.edge_label(no);
                self.line(format!("br i1 {c}, label %{y}, label %{n}"));
                self.emit_edge(&y, yes);
                self.emit_edge(&n, no);
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let v = self.numeric(value);
                let t = ty(self.vty(value));
                let other = self.edge_label(otherwise);
                let arms = arms
                    .get(&self.body.pools.arms)
                    .unwrap()
                    .iter()
                    .map(|a| (*a, self.edge_label(a.edge)))
                    .collect::<Vec<_>>();
                self.line(format!("switch {t} {v}, label %{other} ["));
                for (a, label) in &arms {
                    self.line(format!("{t} {}, label %{label}", a.value));
                }
                self.line("]");
                self.emit_edge(&other, otherwise);
                for (a, label) in arms {
                    self.emit_edge(&label, a.edge);
                }
            }
            Terminator::Return(values) => {
                let xs = values
                    .get(&self.body.pools.values)
                    .unwrap()
                    .iter()
                    .map(|v| self.numeric(*v))
                    .collect::<Vec<_>>();
                self.return_values(&xs);
            }
            Terminator::Unreachable => self.line("unreachable"),
            Terminator::Propagate => self.return_failure(),
            Terminator::Raise { cause, cleanup } => {
                match cause {
                    RaiseCause::Unwrap => self.raise(3),
                    RaiseCause::Bounds => self.raise(1),
                    RaiseCause::Fail(address) => {
                        let at = self.address(address);
                        let data = self.calc(format!("load ptr, ptr {at}, align 1"));
                        let length = self.calc(format!("getelementptr i8, ptr {at}, i64 8"));
                        let length = self.calc(format!("load i64, ptr {length}, align 1"));
                        let capacity = self.calc(format!("getelementptr i8, ptr {at}, i64 16"));
                        let capacity = self.calc(format!("load i64, ptr {capacity}, align 1"));
                        self.intrinsics.insert(
                            "declare void @hirsel_core_raise_string(ptr, i64, ptr, i64, i64)"
                                .into(),
                        );
                        let site = self.site_word();
                        self.line(format!("call void @hirsel_core_raise_string(ptr %context, i64 {site}, ptr {data}, i64 {length}, i64 {capacity})"));
                    }
                }
                self.propagate_to(cleanup);
            }
        }
        Ok(())
    }
}
