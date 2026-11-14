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
#[cfg(all(test, not(hirsel_lazy_core)))]
mod copy_tests;
mod debug;
pub use debug::finish_native_object;
mod empty;
mod fast;
mod flow;
mod forward;
mod growth;
mod scalar;
mod sink;
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

use sink::{
    Binary, CONTEXT, CallAttrs, Cast, Extension, FnAttrs, FnSig, Global, Inline, Label, Linkage,
    Local, MdItem, Memory, Metadata, Operation, Ordering, Parameter, Predicate, Scoped, Sink, Type,
    Value,
};

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
    emit_ir_target_with_role_facts(module, target, observe_charge, facts, source, None)
}

fn emit_ir_target_with_role_facts(
    module: &Module,
    target: Target,
    observe_charge: bool,
    facts: bool,
    source: SourceEmission,
    provided_roles: Option<&hirsel_core_ir::roles::Facts>,
) -> Result<String> {
    let mut text = sink::TextSink::default();
    emit_module(
        module,
        target,
        observe_charge,
        facts,
        source,
        provided_roles,
        &mut text,
    )?;
    Ok(text.finish())
}

/// Deferred module items in the order of their LLVM text lines: globals, then
/// declarations by result type and name, the shared byte order, module asm.
#[derive(Default)]
struct Items(std::collections::BTreeMap<String, Item>);
enum Item {
    Global(Global),
    Declare(String, FnSig, FnAttrs),
    CompareBytes(bool),
    Asm(String),
}
impl Items {
    fn insert(&mut self, key: String, item: Item) {
        self.0.entry(key).or_insert(item);
    }
    pub fn global(&mut self, global: Global) {
        let mut key = String::new();
        sink::write_global(&mut key, &global.name).unwrap();
        key.push(' ');
        self.insert(key, Item::Global(global));
    }
    pub fn declare(&mut self, name: &str, sig: FnSig, attrs: FnAttrs) {
        let mut key = String::from("declare ");
        sink::result_type(&mut key, &sig);
        key.push(' ');
        sink::write_global(&mut key, name).unwrap();
        key.push('(');
        self.insert(key, Item::Declare(name.into(), sig, attrs));
    }
    pub fn compare_bytes(&mut self, facts: bool) {
        self.insert(
            "define private signext i8 @core_compare_bytes(".into(),
            Item::CompareBytes(facts),
        );
    }
    pub fn asm(&mut self, text: String) {
        self.insert(format!("module asm \"{text}\""), Item::Asm(text));
    }
    fn emit(self, sink: &mut dyn Sink) {
        for item in self.0.into_values() {
            match item {
                Item::Global(global) => sink.global(&global),
                Item::Declare(name, sig, attrs) => sink.declare(&name, &sig, &attrs),
                Item::CompareBytes(facts) => bytes::define_compare_bytes(sink, facts),
                Item::Asm(text) => sink.module_asm(&text),
            }
        }
    }
}

/// Lower one CoreIR module, item by item in module order, into `sink`.
fn emit_module(
    module: &Module,
    target: Target,
    observe_charge: bool,
    facts: bool,
    source: SourceEmission,
    provided_roles: Option<&hirsel_core_ir::roles::Facts>,
    sink: &mut dyn Sink,
) -> Result<()> {
    // Shared nodes: !0 weights a likely branch, !1 is the invariant.load list.
    sink.metadata(
        0,
        &Metadata::Tuple {
            distinct: false,
            items: vec![
                MdItem::String("branch_weights".into()),
                MdItem::I32(2000),
                MdItem::I32(1),
            ],
        },
    );
    sink.metadata(
        1,
        &Metadata::Tuple {
            distinct: false,
            items: vec![],
        },
    );
    for (i, data) in module.statics.iter().enumerate() {
        sink.global(&Global {
            name: format!("s{i}").into(),
            linkage: Linkage::Private,
            constant: true,
            externally_initialized: false,
            ty: Type::bytes(data.bytes.len() as u64),
            init: Some(Value::Bytes(data.bytes.to_vec())),
            align: data.align,
            absolute_symbol: false,
        });
    }
    let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
    let mut items = Items::default();
    let mut next_scope = 2;
    let failures = failure::Failures::new(module);
    let debug = if source.debug_lines {
        sites::debug_plan(module, &failures, &mut next_scope, sink)
    } else {
        module.functions.iter().map(|_| None).collect()
    };
    let local_roles =
        (facts && provided_roles.is_none()).then(|| hirsel_core_ir::roles::Facts::new(module));
    let role_facts = provided_roles.or(local_roles.as_ref()).filter(|_| facts);
    for (i, f) in module.functions.iter().enumerate() {
        let sig = &module.signatures[f.signature.index()];
        let has_status = failures.may_fail[i];
        let abi = if has_status || f.body.is_none() {
            abi::Return::internal_on_target(target, &sig.results)
        } else {
            abi::Return::on_target(target, &sig.results)
        };
        let name = symbol(module, FuncId(i as u32));
        let (params, locals) = abi.parameters(sig, facts);
        let Some(body) = &f.body else {
            sink.declare(&name, &abi.signature(params), &FnAttrs::default());
            continue;
        };
        let definition = module.instances[f.instance.index()].definition.to_string();
        let append_fast = definition == "core@1/string/push_char";
        let shadow_callee = append_fast
            .then(|| {
                module.functions.iter().enumerate().find_map(|(i, callee)| {
                    let id = FuncId(i as u32);
                    (module.instances[callee.instance.index()]
                        .definition
                        .to_string()
                        == "core@1/string/push_scalar"
                        && role_facts.is_some_and(|r| r.place_blind(id, 0)))
                    .then_some(id)
                })
            })
            .flatten();
        let function_debug = debug[i].as_ref();
        let aliases = role_facts.as_ref().and_then(|r| {
            alias::Scopes::new(
                module,
                r,
                FuncId(i as u32),
                &name,
                &mut next_scope,
                sink,
                &mut items,
            )
        });
        let flow = flow::Flow::new(body);
        let mut e = FunctionEmitter {
            function: Some(FuncId(i as u32)),
            append_fast,
            shadow_callee,
            caller_site: Value::Local(Local::CALLER_SITE),
            debug_location: function_debug.map(|d| d.location(None)),
            anchor: None,
            relocatable_sites: source.relocatable_sites,
            value_slots: values::slots(body, &flow),
            address_slots: values::address_slots(body, &flow),
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
            sink: &mut *sink,
            items: &mut items,
            next: 0,
        };
        let body_name = if has_status {
            name.clone()
        } else {
            format!("{name}.infallible")
        };
        let destination = sig
            .dest
            .filter(|_| sig.results.is_empty())
            .and_then(|t| {
                use hirsel_core_layout::{Layout, Repr};
                match (module.types[t.index()].repr, &recipes[t.index()].layout) {
                    (Repr::Resource | Repr::Stable, _) | (_, Layout::Fixed(_)) => {
                        let (bytes, _) = e.place_layout(PlaceTy::Declared(t));
                        matches!(bytes, 1..=16).then_some(bytes)
                    }
                    _ => None,
                }
            });
        let attrs = FnAttrs {
            linkage: if has_status {
                Linkage::External
            } else {
                Linkage::Internal
            },
            // Inline the reserved ASCII append and share the UTF-8/growth body.
            inline: match definition.as_str() {
                "core@1/string/push_char" => Inline::Always,
                "core@1/string/push_scalar" => Inline::Never,
                _ => Inline::Default,
            },
            destination_size: destination,
            view_input: abi::view_input(sig),
            subprogram: function_debug.map(|d| d.scope),
            ..FnAttrs::default()
        };
        let signature = e.abi.signature(params);
        e.sink
            .begin_function(&body_name, &signature, &locals, &attrs);
        for (i, t) in body.values.iter().enumerate() {
            if e.value_slots[i] {
                e.sink.alloca(Local::Value(i), &ty(*t), 0);
            }
        }
        for (i, t) in body.slots.iter().enumerate() {
            let (bytes, align) = e.place_layout(*t);
            e.sink.alloca(Local::Slot(i), &Type::bytes(bytes), align as u32);
        }
        for i in 0..body.addresses.len() {
            if e.address_slots[i] {
                e.sink.alloca(Local::Address(i), &Type::Ptr, 0);
            }
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
                Local::DEST
            } else {
                Local::Param(i - usize::from(sig.dest.is_some()))
            };
            e.put(*v, &parameter.into());
        }
        if let Some(scopes) = e.aliases.as_ref().map(|a| a.declared().to_vec()) {
            e.declare_scopes(&scopes);
        }
        e.br(Label::block(0u32));
        let mut forwarding = forward::Forwarding::new(body);
        for (i, b) in body.blocks.iter().enumerate() {
            e.block(Label::block(i));
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
        e.sink.end_function();
        if !has_status {
            e.infallible_wrapper(&name, sig, facts, function_debug.and_then(|d| d.wrapper));
        }
    }
    cleanup::drop_glue(
        module,
        target,
        &recipes,
        &failures,
        facts,
        sink,
        &mut items,
    )?;
    task_entry::emit(
        module,
        target,
        &recipes,
        &failures,
        facts,
        sink,
        &mut items,
    )?;
    items.emit(sink);
    Ok(())
}

/// Lower one owned unit directly into a native module of `context`.
#[cfg(hirsel_lazy_core)]
pub(crate) fn emit_native_module_in_context(
    module: &Module,
    target: Target,
    context: std::rc::Rc<crate::construction::Context>,
    roles: &hirsel_core_ir::roles::Facts,
    observe_charge: bool,
    debug_lines: bool,
) -> Result<crate::construction::NativeModule> {
    native_module(
        module,
        target,
        context,
        observe_charge,
        SourceEmission {
            relocatable_sites: true,
            debug_lines,
        },
        Some(roles),
    )
}

#[cfg(hirsel_lazy_core)]
fn native_module(
    module: &Module,
    target: Target,
    context: std::rc::Rc<crate::construction::Context>,
    observe_charge: bool,
    source: SourceEmission,
    roles: Option<&hirsel_core_ir::roles::Facts>,
) -> Result<crate::construction::NativeModule> {
    let mut native = sink::native::NativeSink::new(context);
    emit_module(
        module,
        target,
        observe_charge,
        facts_enabled(),
        source,
        roles,
        &mut native,
    )?;
    native.finish().map_err(Error::Llvm)
}

/// The printed-module oracle (ORACLE.md): the native module and the text
/// producer's module parsed in a fresh context must print identically.
/// Returns both prints, native first.
#[cfg(hirsel_lazy_core)]
pub fn printed_module_pair(
    module: &Module,
    target: Target,
    roles: Option<&hirsel_core_ir::roles::Facts>,
    relocatable_sites: bool,
    debug_lines: bool,
) -> Result<(String, String)> {
    let source = SourceEmission {
        relocatable_sites,
        debug_lines,
    };
    let facts = facts_enabled();
    let context = std::rc::Rc::new(crate::construction::Context::new().map_err(Error::Llvm)?);
    let mut native = sink::native::NativeSink::new(context.clone());
    emit_module(module, target, false, facts, source, roles, &mut native)?;
    let native = native.print().map_err(Error::Llvm)?;
    let mut text = sink::TextSink::default();
    emit_module(module, target, false, facts, source, roles, &mut text)?;
    let parsed = context.print_text(&text.finish()).map_err(Error::Llvm)?;
    Ok((native, parsed))
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

/// Release emission with FORM facts derived over the publication/dependency
/// graph. The diagnostic facts-off switch still suppresses emitted attributes.
pub fn emit_ir_relocatable_with_role_facts_for_target(
    module: &Module,
    target: Target,
    observe_charge: bool,
    debug_lines: bool,
    roles: &hirsel_core_ir::roles::Facts,
) -> Result<String> {
    emit_ir_target_with_role_facts(
        module,
        target,
        observe_charge,
        facts_enabled(),
        SourceEmission {
            relocatable_sites: true,
            debug_lines,
        },
        Some(roles),
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
        let context = std::rc::Rc::new(crate::construction::Context::new().map_err(Error::Llvm)?);
        let native = native_module(module, target, context, observe_charge, source, None)?;
        crate::loader::compile_module(native, matches!(opt, Optimization::Speed), target)
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

fn ty(t: Ty) -> Type {
    match t {
        Ty::Ptr => Type::Ptr,
        Ty::Scalar(Scalar::F32) => Type::Float,
        Ty::Scalar(Scalar::F64) => Type::Double,
        Ty::Scalar(s) => Type::Int(s.size() as u32 * 8),
    }
}
fn bits_ty(t: Ty) -> Type {
    Type::Int(size(t) as u32 * 8)
}
fn size(t: Ty) -> u64 {
    match t {
        Ty::Ptr => 8,
        Ty::Scalar(s) => s.size(),
    }
}
/// A typed call argument.
type CArg = (Parameter, Value);
fn arg(ty: Type, value: impl Into<Value>) -> CArg {
    (Parameter::plain(ty), value.into())
}
fn ptr(value: impl Into<Value>) -> CArg {
    arg(Type::Ptr, value)
}
fn word(value: impl Into<Value>) -> CArg {
    arg(Type::I64, value)
}
fn int(n: impl Into<i128>) -> Value {
    Value::Int(n.into())
}
const FALSE: Value = Value::Bool(false);

struct FunctionEmitter<'a> {
    function: Option<FuncId>,
    module: &'a Module,
    body: &'a Body,
    recipes: &'a [hirsel_core_layout::Recipe],
    abi: abi::Return,
    has_status: bool,
    append_fast: bool,
    shadow_callee: Option<FuncId>,
    failures: &'a failure::Failures,
    constants: &'a [Option<u64>],
    empty_bounds: empty::EmptyBounds,
    observe_charge: bool,
    facts: bool,
    aliases: Option<alias::Scopes>,
    caller_site: Value,
    debug_location: Option<usize>,
    anchor: Option<Anchor>,
    relocatable_sites: bool,
    value_slots: Vec<bool>,
    address_slots: Vec<bool>,
    sink: &'a mut dyn Sink,
    items: &'a mut Items,
    next: usize,
}
impl FunctionEmitter<'_> {
    /// AAPCS64 leaves narrow C arguments' upper bits unspecified.
    fn c_param(&self, mut p: Parameter) -> Parameter {
        if self.abi.target == Target::Aarch64Linux {
            p.extension = Extension::None;
        }
        p
    }
    fn counter(&mut self) -> usize {
        let n = self.next;
        self.next += 1;
        n
    }
    fn fresh(&mut self) -> Local {
        Local::Temp(self.counter())
    }
    fn emit(&mut self, op: Operation<'_>) -> Value {
        let result = self.fresh();
        self.sink.instruction(Some(result), op, self.debug_location);
        result.into()
    }
    fn emit_to(&mut self, result: Local, op: Operation<'_>) {
        self.sink.instruction(Some(result), op, self.debug_location);
    }
    fn effect(&mut self, op: Operation<'_>) {
        self.sink.instruction(None, op, self.debug_location);
    }
    fn block(&mut self, label: Label) {
        self.sink.block(label);
    }
    fn temp(&mut self, t: Type) -> Value {
        let p = self.fresh();
        self.sink.alloca(p, &t, 8);
        p.into()
    }
    fn bin(&mut self, op: Binary, t: &Type, a: &Value, b: &Value) -> Value {
        self.emit(Operation::Binary(op, t, a, b))
    }
    fn cmp(&mut self, p: Predicate, t: &Type, a: &Value, b: &Value) -> Value {
        self.emit(Operation::Compare(p, t, a, b))
    }
    fn cast(&mut self, op: Cast, from: &Type, v: &Value, to: &Type) -> Value {
        self.emit(Operation::Cast(op, from, v, to))
    }
    fn zext(&mut self, from: &Type, v: &Value, to: &Type) -> Value {
        self.cast(Cast::ZExt, from, v, to)
    }
    fn select(&mut self, t: &Type, c: &Value, yes: &Value, no: &Value) -> Value {
        self.emit(Operation::Select(t, c, yes, no))
    }
    fn load(&mut self, t: &Type, p: &Value, align: u32) -> Value {
        self.emit(Operation::Load(t, p, &Memory::aligned(align)))
    }
    fn load_with(&mut self, t: &Type, p: &Value, m: &Memory) -> Value {
        self.emit(Operation::Load(t, p, m))
    }
    fn store(&mut self, t: &Type, v: &Value, p: &Value, align: u32) {
        self.effect(Operation::Store(t, v, p, &Memory::aligned(align)));
    }
    fn store_with(&mut self, t: &Type, v: &Value, p: &Value, m: &Memory) {
        self.effect(Operation::Store(t, v, p, m));
    }
    fn gep(&mut self, t: &Type, p: &Value, index: &Value, inbounds: bool) -> Value {
        self.emit(Operation::Gep {
            ty: t,
            base: p,
            indices: &[(Type::I64, index.clone())],
            inbounds,
        })
    }
    /// `getelementptr i8, ptr p, i64 n`.
    fn offset(&mut self, p: &Value, n: impl Into<i128>) -> Value {
        self.gep(&Type::I8, p, &int(n), false)
    }
    fn offset_by(&mut self, p: &Value, n: &Value) -> Value {
        self.gep(&Type::I8, p, n, false)
    }
    fn extract(&mut self, t: &Type, v: &Value, index: u32) -> Value {
        self.emit(Operation::Extract(t, v, index))
    }
    fn br(&mut self, to: Label) {
        self.effect(Operation::Branch(to));
    }
    fn cond(&mut self, c: &Value, yes: Label, no: Label) {
        self.effect(Operation::Conditional(c, yes, no, false));
    }
    fn ret(&mut self, t: &Type, v: &Value) {
        self.effect(Operation::Return(Some((t, v))));
    }
    fn ret_void(&mut self) {
        self.effect(Operation::Return(None));
    }
    fn call(&mut self, sig: &FnSig, callee: &Value, args: &[Value], attrs: &CallAttrs) -> Value {
        let op = Operation::Call(sig, callee, args, attrs);
        if sig.result.is_void() {
            self.effect(op);
            Value::Poison
        } else {
            self.emit(op)
        }
    }
    /// Declare `name` once and call it with `args` and `attrs`.
    fn call_declared(
        &mut self,
        name: &str,
        result: Type,
        args: &[CArg],
        declared: FnAttrs,
        attrs: &CallAttrs,
    ) -> Value {
        let sig = FnSig::new(result, args.iter().map(|(p, _)| p.clone()).collect());
        self.items.declare(name, sig.clone(), declared);
        let values: Vec<_> = args.iter().map(|(_, v)| v.clone()).collect();
        self.call(&sig, &Value::global(name.to_owned()), &values, attrs)
    }
    fn vty(&self, v: ValueId) -> Ty {
        self.body.values[v.index()]
    }
    // Moves and numerical operations preserve arbitrary NaN representations.
    // Reinterpret from a float is the sole bit observer (FORM #97).
    fn numeric(&mut self, v: ValueId) -> Value {
        if self.value_slots.get(v.index()).copied().unwrap_or(true) {
            self.load(&ty(self.vty(v)), &Local::Value(v.index()).into(), 0)
        } else {
            Local::Value(v.index()).into()
        }
    }
    fn put(&mut self, v: ValueId, x: &Value) {
        let t = ty(self.vty(v));
        if self.value_slots.get(v.index()).copied().unwrap_or(true) {
            self.store(&t, x, &Local::Value(v.index()).into(), 0);
        } else {
            // LLVM has no scalar copy instruction. This identity also permits
            // forward references when CoreIR blocks are not in dominance order.
            // It preserves poison; no freeze or representation promise is added.
            self.emit_to(
                Local::Value(v.index()),
                Operation::Select(&t, &true.into(), x, &Value::Poison),
            );
        }
    }
    fn access_facts(&self, addresses: &[AddressId]) -> Option<Scoped> {
        self.aliases.as_ref().and_then(|a| a.access(addresses))
    }
    fn address(&mut self, a: AddressId) -> Value {
        if self.address_slots.get(a.index()).copied().unwrap_or(true) {
            self.load(&Type::Ptr, &Local::Address(a.index()).into(), 0)
        } else {
            Local::Address(a.index()).into()
        }
    }
    /// An LLVM intrinsic or runtime function with plain parameters.
    fn intrinsic(&mut self, name: &str, result: Type, args: &[CArg]) -> Value {
        self.call_declared(name, result, args, FnAttrs::default(), &CallAttrs::default())
    }
    fn canonical(&mut self, t: Ty, x: &Value) -> Value {
        if !t.is_float() {
            return x.clone();
        }
        let lt = ty(t);
        let nan = self.cmp(Predicate::Uno, &lt, x, x);
        let bits = if size(t) == 4 {
            2143289344
        } else {
            9221120237041090560
        };
        let quiet = Value::Bitcast {
            from: bits_ty(t),
            bits,
            to: lt.clone(),
        };
        self.select(&lt, &nan, &quiet, x)
    }
    fn fail_if(&mut self, flag: &Value, fail: Option<BlockId>, cause: u8) -> Result<()> {
        self.branch_failure(flag, fail, Some(cause));
        Ok(())
    }
    fn branch_failure(&mut self, flag: &Value, fail: Option<BlockId>, cause: Option<u8>) {
        let n = self.counter();
        let ok = Label::new("ok", n);
        if let (None, Some(fail)) = (cause, fail) {
            // The callee has already recorded its cause. This edge has no
            // arguments or cleanup to perform before the existing successor.
            self.cond(flag, Label::block(fail.0), ok);
        } else {
            let bad = Label::new("bad", n);
            self.cond(flag, bad, ok);
            self.block(bad);
            if let Some(cause) = cause {
                self.raise(cause);
            }
            self.propagate_to(fail);
        }
        self.block(ok);
    }
    fn propagate_to(&mut self, fail: Option<BlockId>) {
        if let Some(fail) = fail {
            self.br(Label::block(fail.0));
        } else {
            self.return_failure();
        }
    }
    fn raise(&mut self, kind: u8) {
        let fixed_site = self.fixed_site_word();
        let fixed = fixed_site.is_some();
        let site = fixed_site.unwrap_or_else(|| self.site_word());
        let fixed = fixed || site == int(0);
        let shape = Type::bytes(memory_layout::CAUSE_BYTES as u64);
        let argument = abi::composite_argument(self.abi.target, memory_layout::CAUSE_BYTES);
        // Built-in causes have fixed bytes. SysV byval supplies a caller copy;
        // AAPCS64 needs an explicit private copy before passing its pointer.
        // Neither convention lets the callee mutate the shared constant.
        let suffix = if fixed && self.relocatable_sites && site != int(0) {
            let anchor = self.anchor.unwrap();
            format!("{}_{}", anchor.item.0, anchor.start)
        } else {
            match site {
                Value::Int(n) => n.to_string(),
                _ => "0".into(),
            }
        };
        let name = format!("hirsel_cause_{kind}_{suffix}");
        let prefix = Type::bytes(memory_layout::CAUSE_KIND as u64);
        let word = memory_layout::cause_kind_word(kind) as i64;
        // Keep AArch64's relocated site in this object's constant bytes;
        // folding that field into an external pointer would select the GOT.
        let initialized = fixed && self.relocatable_sites && self.abi.target == Target::Aarch64Linux;
        self.items.global(Global {
            name: name.clone().into(),
            linkage: Linkage::Private,
            constant: true,
            externally_initialized: initialized,
            ty: Type::Struct(vec![prefix.clone(), Type::I64, Type::I64], true),
            init: Some(Value::Struct {
                packed: true,
                fields: vec![
                    (prefix, Value::Zero),
                    (Type::I64, int(word)),
                    (Type::I64, if fixed { site.clone() } else { int(0) }),
                ],
            }),
            align: 8,
            absolute_symbol: false,
        });
        let pointer = if self.abi.target == Target::Aarch64Linux || !fixed {
            let pointer = self.temp(shape);
            let words = Type::words(memory_layout::CAUSE_BYTES as u64 / 8);
            let value = self.load(&words, &Value::global(name), 8);
            self.store(&words, &value, &pointer, 8);
            if !fixed {
                let at = self.offset(&pointer, memory_layout::CAUSE_SITE as i128);
                self.store(&Type::I64, &site, &at, 8);
            }
            pointer
        } else {
            Value::global(name)
        };
        self.call_declared(
            "hirsel_core_raise",
            Type::Void,
            &[ptr(CONTEXT), (argument, pointer)],
            FnAttrs::default(),
            &CallAttrs::default(),
        );
    }

    fn inst(&mut self, inst: &Inst, index: usize) -> Result<()> {
        self.anchor = self.body.inst_anchors[index];
        match *inst {
            Inst::Const { result, bits } => {
                let t = self.vty(result);
                let x = if t.is_float() {
                    Value::Bitcast {
                        from: bits_ty(t),
                        bits,
                        to: ty(t),
                    }
                } else if t == Ty::Ptr {
                    Value::IntToPtr(bits)
                } else {
                    int(bits)
                };
                self.put(result, &x);
            }
            Inst::ReadStatic { result, data } => {
                self.put(result, &Value::global(format!("s{}", data.0)))
            }
            Inst::FunctionRef { result, function } => {
                self.put(result, &Value::global(symbol(self.module, function)))
            }
            Inst::Unary {
                result,
                op,
                operand,
                fail,
            } => {
                let t = self.vty(operand);
                let lt = ty(t);
                let x = self.numeric(operand);
                let r = match op {
                    UnaryOp::Not => self.bin(Binary::Xor, &lt, &x, &int(1)),
                    UnaryOp::BitNot => self.bin(Binary::Xor, &lt, &x, &int(-1)),
                    UnaryOp::Neg if t.is_float() => self.emit(Operation::FNeg(&lt, &x)),
                    UnaryOp::Neg => {
                        let (v, flag) = self.overflow(t, ArithOp::Sub, &int(0), &x);
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
                let signed = t.int().is_some_and(|x| x.0);
                let pred = match (t.is_float(), op) {
                    (true, CompareOp::Eq) => Predicate::OEq,
                    (true, CompareOp::Ne) => Predicate::UNe,
                    (true, CompareOp::Lt) => Predicate::OLt,
                    (true, CompareOp::Le) => Predicate::OLe,
                    (true, CompareOp::Gt) => Predicate::OGt,
                    (true, CompareOp::Ge) => Predicate::OGe,
                    (false, CompareOp::Eq) => Predicate::Eq,
                    (false, CompareOp::Ne) => Predicate::Ne,
                    (false, CompareOp::Lt) if signed => Predicate::Slt,
                    (false, CompareOp::Le) if signed => Predicate::Sle,
                    (false, CompareOp::Gt) if signed => Predicate::Sgt,
                    (false, CompareOp::Ge) if signed => Predicate::Sge,
                    (false, CompareOp::Lt) => Predicate::Ult,
                    (false, CompareOp::Le) => Predicate::Ule,
                    (false, CompareOp::Gt) => Predicate::Ugt,
                    (false, CompareOp::Ge) => Predicate::Uge,
                };
                let x = self.cmp(pred, &ty(t), &a, &b);
                let x = self.zext(&Type::I1, &x, &Type::I8);
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
                    let over = self.zext(&Type::I1, &over, &Type::I8);
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
                let lt = ty(t);
                let a = self.numeric(left);
                let b = self.numeric(right);
                let c = self.numeric(addend);
                let x = self.intrinsic(
                    if size(t) == 4 {
                        "llvm.fma.f32"
                    } else {
                        "llvm.fma.f64"
                    },
                    lt.clone(),
                    &[arg(lt.clone(), a), arg(lt.clone(), b), arg(lt, c)],
                );
                self.put(result, &x);
            }
            Inst::Address { address, fail } => self.project(address, fail)?,
            Inst::Read {
                result, address, ..
            } => {
                let p = self.address(address);
                let memory = Memory {
                    align: 1,
                    scopes: self.access_facts(&[address]),
                    ..Memory::default()
                };
                let x = self.load_with(&ty(self.vty(result)), &p, &memory);
                self.put(result, &x);
            }
            Inst::Store { address, value } => {
                let p = self.address(address);
                let x = self.numeric(value);
                let memory = Memory {
                    align: 1,
                    scopes: self.access_facts(&[address]),
                    ..Memory::default()
                };
                self.store_with(&ty(self.vty(value)), &x, &p, &memory);
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
                let x = if initialized {
                    self.bin(Binary::Or, &t, &x, &int(1u64 << field))
                } else {
                    self.bin(Binary::And, &t, &x, &int(!(1u64 << field)))
                };
                self.put(result, &x);
            }
            Inst::Call { call } => self.call_site(&self.body.calls[call.index()])?,
            _ => self.storage(inst, index)?,
        }
        Ok(())
    }
    fn edge(&mut self, edge: EdgeId) -> Label {
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
        Label::block(e.target.0)
    }
    fn edge_label(&mut self, edge: EdgeId) -> Label {
        let e = &self.body.edges[edge.index()];
        if e.args.len == 0 {
            return Label::block(e.target.0);
        }
        // Value transfers need separate blocks, including distinct switch arms
        // to the same target. Empty edges already name their final destination.
        let n = self.counter();
        Label::edge(n, edge.0)
    }
    fn emit_edge(&mut self, label: Label, edge: EdgeId) {
        if self.body.edges[edge.index()].args.len == 0 {
            return;
        }
        self.block(label);
        let to = self.edge(edge);
        self.br(to);
    }
    fn term(&mut self, term: &Terminator) -> Result<()> {
        match *term {
            Terminator::Jump(e) => {
                let to = self.edge(e);
                self.br(to);
            }
            Terminator::Branch { condition, yes, no } => {
                let c = self.numeric(condition);
                let c = self.cmp(Predicate::Ne, &Type::I8, &c, &int(0));
                let y = self.edge_label(yes);
                let n = self.edge_label(no);
                self.cond(&c, y, n);
                self.emit_edge(y, yes);
                self.emit_edge(n, no);
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
                let cases: Vec<_> = arms
                    .iter()
                    .map(|(a, label)| (i128::from(a.value), *label))
                    .collect();
                self.effect(Operation::Switch(&t, &v, other, &cases));
                self.emit_edge(other, otherwise);
                for (a, label) in arms {
                    self.emit_edge(label, a.edge);
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
            Terminator::Unreachable => self.effect(Operation::Unreachable),
            Terminator::Propagate => self.return_failure(),
            Terminator::Raise { cause, cleanup } => {
                match cause {
                    RaiseCause::Unwrap => self.raise(3),
                    RaiseCause::Bounds => self.raise(1),
                    RaiseCause::Fail(address) => {
                        let at = self.address(address);
                        let data = self.load(&Type::Ptr, &at, 1);
                        let length = self.offset(&at, 8);
                        let length = self.load(&Type::I64, &length, 1);
                        let capacity = self.offset(&at, 16);
                        let capacity = self.load(&Type::I64, &capacity, 1);
                        let site = self.site_word();
                        self.intrinsic(
                            "hirsel_core_raise_string",
                            Type::Void,
                            &[
                                ptr(CONTEXT),
                                word(site),
                                ptr(data),
                                word(length),
                                word(capacity),
                            ],
                        );
                    }
                }
                self.propagate_to(cleanup);
            }
        }
        Ok(())
    }
}
