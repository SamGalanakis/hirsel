//! Direct emission of CoreIR functions, failure edges and storage recipes.

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
mod split;
pub use split::split_native_object;
mod empty;
mod expose;
mod fast;
mod flow;
mod forward;
mod growth;
mod scalar;
mod simd;
mod sink;
mod sites;
mod steps;
mod storage;
#[cfg(all(test, not(hirsel_lazy_core)))]
mod target_tests;
mod task_entry;
mod tasks;
#[cfg(all(test, not(hirsel_lazy_core)))]
mod tests;
mod values;
mod versions;

use std::collections::BTreeSet;
use std::fmt::{self, Write};

use sink::{
    Binary, CONTEXT, CallAttrs, Cast, Extension, FnAttrs, FnSig, Global, Inline, Label, Linkage,
    Local, MdItem, Memory, Metadata, Operation, Ordering, Parameter, Predicate, Rmw, Scoped, Sink,
    Type, Value,
};

use hirsel_core_ir::failure;
use hirsel_core_ir::*;
use hirsel_core_layout::{
    Target,
    isa::{Level, X86_64_LEVELS},
};

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
    batch_steps: bool,
    test_hooks: bool,
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
    hirsel_core_ir::symbol(module.code_identity(function))
}

/// The link name of `function`'s static descriptor (`Inst::DescriptorRef`).
fn descriptor_symbol(function: &str) -> String {
    format!("{function}$static")
}

/// The `"hirsel-carriers"` marker: each LLVM parameter whose place the bridge
/// may pass as words on cold calls (FORM #111), `m` when the callee may change it (`mut`,
/// `take`, so its words come back) and `v` when it is frozen (`view`).
///
/// A parameter qualifies when its place is a carrier (`roles::carrier`: an own
/// record whose one field is a sequence of plain elements, so the place is data
/// pointer, length, capacity, three initialised words, and the copy is exact)
/// and the body is place-blind there (`roles::Facts::place_blind`): its address
/// is never a value, so no anchored result, stored pointer or callee can tell a
/// stand-in from the place. Every other parameter, and every parameter when the
/// emitter has no role facts, keeps the pointer ABI.
fn carriers(
    module: &Module,
    recipes: &[hirsel_core_layout::Recipe],
    facts: Option<&hirsel_core_ir::roles::Facts>,
    f: FuncId,
    sig: &Signature,
    locals: &[Local],
) -> String {
    let mut out = String::new();
    let Some(facts) = facts else {
        return out;
    };
    for (i, p) in sig.params.iter().enumerate() {
        let Param::Address {
            place: PlaceTy::Declared(t),
            loan,
        } = p
        else {
            continue;
        };
        if !hirsel_core_ir::roles::carrier(module, *t) || !facts.place_blind(f, i as u32) {
            continue;
        }
        let Some(at) = locals.iter().position(|l| *l == Local::Param(i)) else {
            continue;
        };
        if !out.is_empty() {
            out.push(',');
        }
        let kind = if *loan == Loan::View { 'v' } else { 'm' };
        write!(out, "{at}:{kind}").unwrap();
        // The words travel as values, so the bound a len or capacity read
        // carries (FORM #113) goes with them.
        if let hirsel_core_layout::Body::Record(fields) = &module.types[t.index()].body
            && let [
                hirsel_core_layout::Field {
                    ty: hirsel_core_layout::FieldTy::Seq(e),
                    ..
                },
            ] = &fields[..]
            && let Some(bound) = hirsel_core_layout::charge::seq_len_bound(
                hirsel_core_layout::field_size(&module.types, recipes, e).2,
            )
        {
            write!(out, ":{}", bound + 1).unwrap();
        }
    }
    out
}

/// Whether to use the producer's optimization facts. Setting the diagnostic
/// switch suppresses every emitter-supplied fact, while retaining ABI attributes
/// and stack probes. Drivers include this choice in native object cache keys,
/// and the variable is on the driver's keyed environment list.
pub fn facts_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(NO_FACTS_VARIABLE).is_none())
}
/// The diagnostic switch [`facts_enabled`] reads.
pub const NO_FACTS_VARIABLE: &str = "HIRSEL_LLVM_NO_FACTS";

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
            batch_steps: true,
            test_hooks: cfg!(test),
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
            batch_steps: true,
            test_hooks: cfg!(test),
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
        EmissionFacts {
            roles: provided_roles,
            ..EmissionFacts::default()
        },
        &mut text,
    )?;
    Ok(text.finish())
}

/// Deferred module items in the order of their LLVM text lines: globals, then
/// declarations by result type and name, the shared byte order, module asm.
struct Items {
    function_symbols: Vec<String>,
    versioned: Option<Vec<bool>>,
    pending: std::collections::BTreeMap<String, Item>,
    release_sites: Option<ReleaseSites>,
}
#[derive(Default)]
struct ReleaseSites {
    names: std::collections::HashMap<(ItemId, u32), String>,
    next: std::collections::HashMap<ItemId, u32>,
    bindings: Vec<source::ReleaseSite>,
}
enum Item {
    Global(Global),
    Declare(String, FnSig, FnAttrs),
    CompareBytes(bool),
    Asm(String),
    /// A counted type's out-of-line last release, defined after every body
    /// that may call it (cleanup::release_lasts).
    ReleaseLast(u32, bool),
}
impl Items {
    fn new(module: &Module) -> Self {
        Self {
            function_symbols: module.link_symbols(),
            versioned: None,
            pending: Default::default(),
            release_sites: None,
        }
    }
    fn is_versioned(&self, function: FuncId) -> bool {
        self.versioned.as_ref().is_some_and(|v| v[function.index()])
    }
    fn function_symbol(&self, function: FuncId) -> &str {
        &self.function_symbols[function.index()]
    }
    fn insert(&mut self, key: String, item: Item) {
        self.pending.entry(key).or_insert(item);
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
    /// Request `t`'s last-release body and return its name.
    pub fn release_last(&mut self, t: u32, atomic: bool) -> String {
        let name = release_last_name(t, atomic);
        self.insert(
            format!("define private void @{name}("),
            Item::ReleaseLast(t, atomic),
        );
        name
    }
    fn take_release_lasts(&mut self) -> Vec<(u32, bool)> {
        let mut lasts = Vec::new();
        self.pending.retain(|_, item| match item {
            Item::ReleaseLast(t, atomic) => {
                lasts.push((*t, *atomic));
                false
            }
            _ => true,
        });
        lasts
    }
    pub fn asm(&mut self, text: String) {
        self.insert(format!("module asm \"{text}\""), Item::Asm(text));
    }
    fn emit(self, sink: &mut dyn Sink) -> Vec<source::ReleaseSite> {
        for item in self.pending.into_values() {
            match item {
                Item::Global(global) => sink.global(&global),
                Item::Declare(name, sig, attrs) => sink.declare(&name, &sig, &attrs),
                Item::CompareBytes(facts) => bytes::define_compare_bytes(sink, facts),
                Item::Asm(text) => sink.module_asm(&text),
                Item::ReleaseLast(..) => unreachable!("last releases are defined before items"),
            }
        }
        self.release_sites
            .map_or_else(Vec::new, |sites| sites.bindings)
    }
}

fn release_last_name(t: u32, atomic: bool) -> String {
    format!(
        "core_release_last_{t}_{}",
        if atomic { "arc" } else { "rc" }
    )
}

#[derive(Clone, Copy, Default)]
struct EmissionFacts<'a> {
    roles: Option<&'a hirsel_core_ir::roles::Facts>,
    neutral: Option<&'a BTreeSet<hirsel_core_ids::ContentHash>>,
    elide_steps: bool,
    prepay_step_trees: bool,
    release_sites: bool,
    versions: Option<&'a BTreeSet<hirsel_core_ids::ContentHash>>,
}
#[cfg(hirsel_lazy_core)]
pub(crate) fn versioned_program(modules: &[&Module]) -> BTreeSet<hirsel_core_ids::ContentHash> {
    versions::program(modules)
}
#[cfg(hirsel_lazy_core)]
pub(crate) fn step_neutral_program(modules: &[&Module]) -> BTreeSet<hirsel_core_ids::ContentHash> {
    steps::program_neutral(modules)
}

/// Lower one CoreIR module, item by item in module order, into `sink`.
fn emit_module(
    module: &Module,
    target: Target,
    observe_charge: bool,
    facts: bool,
    source: SourceEmission,
    provided: EmissionFacts<'_>,
    sink: &mut dyn Sink,
) -> Result<Vec<source::ReleaseSite>> {
    let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
    let native = hirsel_core_ir::native_layout::resolve(module, &recipes);
    let module = native.as_ref();
    let exposed = source
        .batch_steps
        .then(|| expose::module(module, provided.prepay_step_trees))
        .flatten();
    let module = exposed.as_ref().unwrap_or(module);
    let mut items = Items::new(module);
    if let Some(known) = provided.versions.filter(|known| !known.is_empty()) {
        items.versioned = Some(
            module
                .functions
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    f.versioned || known.contains(&module.code_identity(FuncId(i as u32)))
                })
                .collect(),
        );
        sink.global(&Global {
            name: "hirsel_core_isa_level".into(),
            linkage: Linkage::External,
            constant: false,
            externally_initialized: false,
            ty: Type::I8,
            init: None,
            align: 1,
            absolute_symbol: false,
        });
    }
    items.release_sites = provided.release_sites.then(ReleaseSites::default);
    let batch_steps = source.batch_steps && !provided.elide_steps;
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
        let (align, bytes) = data.native(&module.types, &recipes);
        sink.global(&Global {
            name: format!("s{i}").into(),
            linkage: Linkage::Private,
            constant: true,
            externally_initialized: false,
            ty: Type::bytes(bytes.len() as u64),
            init: Some(Value::Bytes(bytes.into_owned())),
            align,
            absolute_symbol: false,
        });
    }
    // Each function a body of this unit describes, its descriptor `{code,
    // length, revision}` then the identity bytes, defined weak beside the
    // function's code identity: every unit that describes it defines it, and
    // the linker keeps one per program, as it keeps one body. `revision`
    // addresses its revision root's carrier (`Module::revision_root`): an
    // item's is defined here plain weak as `{0, 0, 0}` so that a link that
    // knows the reach defines over it; an intrinsic's holds its revision.
    let mut described = BTreeSet::new();
    let mut revisions = BTreeSet::new();
    for body in module.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            let Inst::DescriptorRef {
                function, identity, ..
            } = *inst
            else {
                continue;
            };
            let function_symbol = items.function_symbol(function);
            let name = descriptor_symbol(function_symbol);
            if !described.insert(name.clone()) {
                continue;
            }
            let root = module.revision_root(function);
            let revision = hirsel_core_ir::revision_symbol(root.identity().digest());
            if revisions.insert(revision.clone()) {
                let init = match &root {
                    hirsel_core_ir::RevisionRoot::Item(_) => Value::Zero,
                    hirsel_core_ir::RevisionRoot::Intrinsic(identity) => {
                        let bytes = format!("{revision}.bytes");
                        sink.global(&Global {
                            name: bytes.clone().into(),
                            linkage: Linkage::Private,
                            constant: true,
                            externally_initialized: false,
                            ty: Type::bytes(32),
                            init: Some(Value::Bytes(
                                hirsel_core_ir::intrinsic_revision(identity).0.to_vec(),
                            )),
                            align: 1,
                            absolute_symbol: false,
                        });
                        Value::Struct {
                            packed: false,
                            fields: vec![
                                (Type::Ptr, Value::global(bytes)),
                                (Type::I64, Value::Int(32)),
                                (Type::I64, Value::Int(0)),
                            ],
                        }
                    }
                };
                sink.global(&Global {
                    name: revision.clone().into(),
                    linkage: Linkage::Weak,
                    constant: true,
                    externally_initialized: false,
                    ty: Type::structure([Type::Ptr, Type::I64, Type::I64]),
                    init: Some(init),
                    align: 8,
                    absolute_symbol: false,
                });
            }
            let bytes = module.statics[identity.index()].literal_bytes().unwrap();
            let identity = Type::bytes(bytes.len() as u64);
            sink.global(&Global {
                name: name.into(),
                linkage: Linkage::WeakOdr,
                constant: true,
                externally_initialized: false,
                ty: Type::structure([Type::Ptr, Type::I64, Type::Ptr, identity.clone()]),
                init: Some(Value::Struct {
                    packed: false,
                    fields: vec![
                        (Type::Ptr, Value::global(function_symbol.to_owned())),
                        (Type::I64, Value::Int(bytes.len() as i128)),
                        (Type::Ptr, Value::global(revision)),
                        (identity, Value::Bytes(bytes.to_vec())),
                    ],
                }),
                align: 8,
                absolute_symbol: false,
            });
        }
    }
    let mut next_scope = alias::private_steps(sink);
    let failures = failure::Failures::new(module);
    let step_neutral =
        (!provided.elide_steps).then(|| steps::neutral_functions(module, provided.neutral));
    let neutral_functions = step_neutral.as_deref().unwrap_or_default();
    let debug = if source.debug_lines {
        sites::debug_plan(
            module,
            &items.function_symbols,
            items.versioned.as_deref(),
            &failures,
            &mut next_scope,
            sink,
        )
    } else {
        sites::DebugPlan::default()
    };
    let local_roles =
        (facts && provided.roles.is_none()).then(|| hirsel_core_ir::roles::Facts::new(module));
    let call_layouts = hirsel_core_ir::native_layout::CallLayouts::new(&module.types, &recipes);
    let role_facts = provided.roles.or(local_roles.as_ref()).filter(|_| facts);
    for (i, f) in module.functions.iter().enumerate() {
        let sig = &module.signatures[f.signature.index()];
        let has_status = failures.may_fail[i];
        let abi =
            abi::Return::for_signature(target, sig, &call_layouts, has_status || f.body.is_none());
        let mut name = items.function_symbols[i].clone();
        let versioned = items.is_versioned(FuncId(i as u32));
        let (mut params, locals) = abi.parameters(sig, &call_layouts, facts, |place| match place {
            PlaceTy::Value(t) => size(t),
            PlaceTy::Declared(t)
                if matches!(
                    module.types[t.index()].repr,
                    hirsel_core_layout::Repr::Resource | hirsel_core_layout::Repr::Region
                ) =>
            {
                8
            }
            PlaceTy::Declared(t) => match &recipes[t.index()].layout {
                hirsel_core_layout::Layout::Fixed(layout) => layout.size,
                _ => unreachable!("address parameter has a fixed native place"),
            },
        });
        let Some(body) = &f.body else {
            let signature = abi.signature(params);
            if !versioned || !sig.has_vectors() {
                sink.declare(&name, &signature, &FnAttrs::default());
            }
            if versioned {
                for &level in X86_64_LEVELS {
                    let attrs = FnAttrs {
                        level: Some(level),
                        ..FnAttrs::default()
                    };
                    sink.declare(&versions::symbol(&name, level), &signature, &attrs);
                }
            }
            continue;
        };
        if versioned && !sig.has_vectors() {
            if !has_status {
                versions::stub(
                    sink,
                    &name,
                    &abi.signature(params.clone()),
                    &locals,
                    Linkage::Internal,
                    true,
                );
            }
            let public = abi::Return::for_signature(target, sig, &call_layouts, true);
            let (params, locals) = public.parameters(sig, &call_layouts, false, |_| 0);
            versions::stub(
                sink,
                &name,
                &public.signature(params),
                &locals,
                Linkage::WeakOdr,
                false,
            );
        }
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
        let variants = if versioned { X86_64_LEVELS.len() } else { 1 };
        let mut abi = Some(abi);
        for variant in 0..variants {
            let level = versioned.then(|| X86_64_LEVELS[variant]);
            let name = match level {
                Some(level) => versions::symbol(&name, level),
                None => std::mem::take(&mut name),
            };
            let (signature_abi, signature_params) = if variant + 1 == variants {
                (abi.take().unwrap(), std::mem::take(&mut params))
            } else {
                (
                    abi::Return::for_signature(target, sig, &call_layouts, has_status),
                    params.clone(),
                )
            };
            let function_debug = debug.get(i, level);
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
                level,
                call_layouts: Some(&call_layouts),
                append_fast,
                shadow_callee,
                caller_site: Value::Local(Local::CALLER_SITE),
                debug_location: function_debug.map(|d| d.location(None)),
                anchor: None,
                relocatable_sites: source.relocatable_sites,
                test_hooks: source.test_hooks,
                value_slots: values::slots(body, &flow),
                static_vectors: f.versioned.then(|| simd::static_vectors(body)),
                literals: Vec::new(),
                address_slots: values::address_slots(body, &flow),
                scoped: hirsel_core_ir::extent::scoped_slots(body, &module.signatures),
                live: None,
                scoped_ended: false,
                step_labels: Default::default(),
                unchecked_steps: false,
                precharge: None,
                step_neutral: step_neutral.as_deref(),
                step_counter: None,
                neutral_step_call: false,
                view_invariants: Vec::new(),
                aliases,
                module,
                body,
                recipes: &recipes,
                abi: signature_abi,
                has_status,
                failures: &failures,
                constants: &failures.constants[i],
                observe_charge,
                receive_loops: hirsel_core_ir::receive_loops::ReceiveLoops::new(body),
                facts,
                empty_bounds: if observe_charge {
                    empty::EmptyBounds::default()
                } else {
                    empty::EmptyBounds::new(body, &failures.constants[i])
                },
                sink: &mut *sink,
                items: &mut items,
                next: 0,
                fixed_point: None,
                fixed_rank: None,
            };
            let body_name = if has_status {
                name.clone()
            } else {
                format!("{name}.infallible")
            };
            let attrs = FnAttrs {
            linkage: if has_status {
                Linkage::WeakOdr
            } else {
                Linkage::Internal
            },
            // Inline the reserved ASCII append and share the UTF-8/growth body.
            // A loop-free callable variant exists only to bind its call site's
            // closure, so it is inlined there unless that site is cold. A variant
            // with a loop amortizes its call and stays out of line. Single-call adapters
            // expose their result staging and cleanup to the caller's optimizer.
            inline: match definition.as_str() {
                "core@1/string/push_char" => Inline::Always,
                "core@1/string/push_scalar" => Inline::Never,
                // pivot.rs leaves the recursive sample to the inliner.
                "core@1/list/median3_rec" => Inline::Default,
                "std@1/float/shortest64" | "std@1/float/push_fixed64" => Inline::Always,
                "std@1/float/shortest_decimal64" | "std@1/float/push_fixed_decimal64" => Inline::Never,
                // Share regex construction across call sites. The parser and
                // translator each have a fixed workspace and independent error paths.
                "std@1/regex/read"
                | "std@1/regex/write"
                | "std@1/regex/insert"
                | "std@1/regex/ast"
                | "std@1/regex/check_depth"
                | "std@1/regex/translate"
                | "std@1/regex/translate_atom"
                | "std@1/regex/compile_hir"
                | "std@1/regex/compile_new" => Inline::Never,
                // Revisit bounded mutable loops after borrow-scope emission.
                // LLVM inlining carries their parameter scopes into the caller.
                _ if source.batch_steps
                    && body.insts.len() <= 4096
                    && sig.params.iter().any(|p| {
                        matches!(p, Param::Address { place, loan: Loan::Mut }
                            if matches!(place, PlaceTy::Value(Ty::Scalar(_)))
                                || matches!(place, PlaceTy::Declared(t) if hirsel_core_ir::roles::carrier(module, *t)))
                    })
                    && !empty::loop_free(body) =>
                {
                    Inline::Always
                }
                _ if (module.code_variants.contains_key(&FuncId(i as u32))
                    || body.calls.len() <= 1)
                    && empty::loop_free(body) =>
                {
                    Inline::Always
                }
                _ => Inline::Default,
            },
            carriers: carriers(module, &recipes, role_facts, FuncId(i as u32), sig, &locals),
            subprogram: function_debug.map(|d| d.scope),
            level,
            ..FnAttrs::default()
        };
            let signature = e.abi.signature(signature_params);
            e.sink
                .begin_function(&body_name, &signature, &locals, &attrs);
            if !provided.elide_steps && body.insts.iter().any(|i| matches!(i, Inst::Step { .. })) {
                let counter = Local::Named("hirsel.step.counter");
                e.sink.alloca(counter, &Type::I64, 8);
                e.step_counter = Some(counter.into());
                e.reload_steps();
            }
            // Eager and lazy objects use the same exact-metering CFG at every
            // optimization level. LLVM promotes its joins to registers at Speed.
            let step_loops = if batch_steps {
                steps::loops(module, body, e.constants, neutral_functions)
            } else {
                Vec::new()
            };
            let step_regions = if batch_steps {
                steps::regions(module, body, neutral_functions, &step_loops)
            } else {
                Vec::new()
            };
            let step_groups = body
                .blocks
                .iter()
                .map(|b| {
                    if batch_steps {
                        steps::groups(module, body, neutral_functions, b.insts)
                    } else {
                        Default::default()
                    }
                })
                .collect::<Vec<_>>();
            if !step_loops.is_empty()
                || !step_regions.is_empty()
                || step_groups.iter().any(|g| !g.is_empty())
            {
                // Both loop copies define the same source values and addresses.
                // Entry allocas give mem2reg their joins before ordinary O3 runs.
                e.value_slots.fill(true);
                e.address_slots.fill(true);
            }
            for (i, t) in body.values.iter().enumerate() {
                if e.value_slots[i] {
                    e.sink.alloca(Local::Value(i), &ty(*t), 0);
                }
            }
            for (i, t) in body.slots.iter().enumerate() {
                let (bytes, align) = e.place_layout(*t);
                e.sink
                    .alloca(Local::Slot(i), &Type::bytes(bytes), align as u32);
            }
            for i in 0..body.addresses.len() {
                if e.address_slots[i] {
                    e.sink.alloca(Local::Address(i), &Type::Ptr, 0);
                }
            }
            let step_ends = step_loops
                .iter()
                .map(|_| e.step_window())
                .collect::<Vec<_>>();
            let region_windows = step_regions
                .iter()
                .map(|_| e.step_window())
                .collect::<Vec<_>>();
            // A constant is a literal at each use: it has no effects or operands, and
            // a value defined once for the whole function would be live, and spilled,
            // across all of it.
            e.literals = vec![None; body.values.len()];
            for inst in &body.insts {
                if let Inst::Const { result, bits } = *inst {
                    e.literals[result.index()] = Some(e.literal(result, bits));
                }
            }
            if let Some(record) = e.abi.record {
                e.sink.alloca(Local::DEST, &Type::bytes(record.size), 8);
            }
            for (i, v) in body.blocks[0]
                .params
                .get(&body.pools.values)
                .unwrap()
                .iter()
                .enumerate()
            {
                let parameter = if sig.dest.is_some() && i == 0 {
                    Local::DEST.into()
                } else {
                    let index = i - usize::from(sig.dest.is_some());
                    if let Some(record) = call_layouts.parameter(sig.params[index]) {
                        let place = e.temp(Type::bytes(record.size));
                        let values: Vec<_> = (0..record.fields().len())
                            .map(|j| Local::ParamField(index, j).into())
                            .collect();
                        e.store_record(record, &place, &values);
                        place
                    } else {
                        Local::Param(index).into()
                    }
                };
                e.put(*v, &parameter);
            }
            if let Some(scopes) = e.aliases.as_ref().map(|a| a.declared().to_vec()) {
                e.declare_scopes(&scopes);
            }
            let mut forwarding = forward::Forwarding::native(body, sig, &call_layouts);
            // A view freezes the carrier header throughout the activation, even
            // across calls through an opaque capture. End the invariant on every
            // return; retain existing frame-safe tail-call functions unchanged.
            if facts
                && !(0..body.blocks.len()).any(|b| {
                    forwarding
                        .ending(module, &recipes, sig, e.constants, b)
                        .is_some_and(|ending| ending.frame_safe)
                })
            {
                for (index, param) in sig.params.iter().enumerate() {
                    if let Param::Address {
                        place: PlaceTy::Declared(t),
                        loan: Loan::View,
                    } = *param
                        && hirsel_core_ir::roles::carrier(module, t)
                    {
                        let bytes = e.place_layout(PlaceTy::Declared(t)).0;
                        let pointer = Value::Local(Local::Param(index));
                        let token = e.intrinsic(
                            "llvm.invariant.start.p0",
                            Type::Ptr,
                            &[word(int(bytes)), ptr(pointer.clone())],
                        );
                        e.view_invariants.push((token, pointer, bytes));
                    }
                }
            }
            e.br(Label::block(0u32));
            for (i, b) in body.blocks.iter().enumerate() {
                e.block(Label::block(i));
                e.enter_block(i);
                if e.receive_loops.exit(i) {
                    e.native_call("hirsel_core_receive_loop_end", &[], &[]);
                }
                if let Some((number, plan)) = step_loops
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.reserve == i && p.reserve != p.header)
                {
                    e.step_dispatch(plan, step_ends[number], number);
                }
                if let Some((number, plan)) =
                    step_regions.iter().enumerate().find(|(_, p)| p.header == i)
                {
                    e.region_dispatch(plan, region_windows[number], number);
                }
                let ending = forwarding.ending(module, &recipes, sig, e.constants, i);
                let mut returned = false;
                let groups = &step_groups[i];
                let skipped = groups
                    .values()
                    .flat_map(|g| g.start + 1..g.end)
                    .collect::<BTreeSet<_>>();
                for (offset, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
                    let index = b.insts.start as usize + offset;
                    if skipped.contains(&index)
                        || (provided.elide_steps && matches!(inst, Inst::Step { .. }))
                    {
                        continue;
                    }
                    e.debug_location = function_debug.map(|d| d.location(body.inst_anchors[index]));
                    if let Some(group) = groups.get(&index) {
                        e.step_effect_group(*group, |anchor| {
                            function_debug.map(|d| d.location(anchor))
                        })?;
                        continue;
                    }
                    if let Some(ending) = &ending
                        && offset == ending.prefix
                        && {
                            e.anchor = body.inst_anchors[b.insts.start as usize + offset];
                            e.return_call(&body.calls[ending.call.index()], ending, &call_layouts)
                        }
                    {
                        returned = true;
                        break;
                    }
                    if !matches!(inst, Inst::Const { .. }) {
                        e.inst(inst, index)?;
                    }
                }
                if !returned {
                    e.anchor = b.anchor;
                    e.debug_location = function_debug.map(|d| d.location(b.anchor));
                    if let Some((number, plan)) = step_loops
                        .iter()
                        .enumerate()
                        .find(|(_, p)| p.header == i && p.reserve == i)
                    {
                        e.step_dispatch(plan, step_ends[number], number);
                    } else {
                        e.term(&body.terminators[i])?;
                    }
                }
            }
            for (number, plan) in step_loops.iter().enumerate() {
                e.step_versions(plan, step_ends[number], number, |anchor| {
                    function_debug.map(|d| d.location(anchor))
                })?;
            }
            for (number, plan) in step_regions.iter().enumerate() {
                e.region_version(plan, region_windows[number], number, |anchor| {
                    function_debug.map(|d| d.location(anchor))
                })?;
            }
            e.sink.end_function();
            e.step_counter = None;
            if !has_status {
                e.infallible_wrapper(
                    &name,
                    sig,
                    &call_layouts,
                    facts,
                    function_debug.and_then(|d| d.wrapper),
                );
            }
        }
    }
    cleanup::drop_glue(
        module,
        target,
        &recipes,
        &failures,
        facts,
        source.test_hooks,
        sink,
        &mut items,
    )?;
    task_entry::emit(
        module,
        target,
        &recipes,
        &call_layouts,
        &failures,
        facts,
        sink,
        &mut items,
    )?;
    let lasts = items.take_release_lasts();
    cleanup::release_lasts(
        module, target, &recipes, &failures, facts, &lasts, sink, &mut items,
    )?;
    Ok(items.emit(sink))
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
    neutral: Option<&BTreeSet<hirsel_core_ids::ContentHash>>,
    versions: Option<&BTreeSet<hirsel_core_ids::ContentHash>>,
) -> Result<crate::construction::NativeModule> {
    native_module(
        module,
        target,
        context,
        observe_charge,
        SourceEmission {
            batch_steps: true,
            test_hooks: cfg!(test),
            relocatable_sites: true,
            debug_lines,
        },
        EmissionFacts {
            roles: Some(roles),
            neutral,
            elide_steps: neutral.is_none(),
            prepay_step_trees: neutral.is_some(),
            release_sites: false,
            versions,
        },
    )
}

#[cfg(hirsel_lazy_core)]
pub(crate) fn emit_release_unit(
    module: &Module,
    context: std::rc::Rc<crate::construction::Context>,
    roles: &hirsel_core_ir::roles::Facts,
    observe_charge: bool,
    neutral: Option<&BTreeSet<hirsel_core_ids::ContentHash>>,
) -> Result<(crate::construction::NativeModule, Vec<source::ReleaseSite>)> {
    let mut native = sink::native::NativeSink::new(context);
    let versions = versions::program(&[module]);
    let sites = emit_module(
        module,
        Target::X86_64Linux,
        observe_charge,
        facts_enabled(),
        SourceEmission {
            batch_steps: true,
            test_hooks: cfg!(test),
            relocatable_sites: true,
            debug_lines: false,
        },
        EmissionFacts {
            roles: Some(roles),
            neutral,
            elide_steps: neutral.is_none(),
            prepay_step_trees: neutral.is_some(),
            release_sites: true,
            versions: Some(&versions),
        },
        &mut native,
    )?;
    Ok((native.finish().map_err(Error::Llvm)?, sites))
}

#[cfg(hirsel_lazy_core)]
fn native_module(
    module: &Module,
    target: Target,
    context: std::rc::Rc<crate::construction::Context>,
    observe_charge: bool,
    source: SourceEmission,
    provided: EmissionFacts<'_>,
) -> Result<crate::construction::NativeModule> {
    let facts = facts_enabled();
    let mut native = sink::native::NativeSink::new(context);
    emit_module(
        module,
        target,
        observe_charge,
        facts,
        source,
        provided,
        &mut native,
    )?;
    if let Some(dir) = std::env::var_os("HIRSEL_LLVM_PRINTED_ORACLE") {
        printed_oracle(
            std::path::Path::new(&dir),
            module,
            &native.print().map_err(Error::Llvm)?,
            |text| {
                emit_module(
                    module,
                    target,
                    observe_charge,
                    facts,
                    source,
                    provided,
                    text,
                )
                .map(|_| ())
            },
        )?;
    }
    native.finish().map_err(Error::Llvm)
}

/// The printed-module oracle (ORACLE.md), a check that never changes emission:
/// the raw native module must print exactly as the text producer's module
/// parsed in a fresh context. Each compared unit appends a row to `units.tsv`;
/// a mismatch keeps its text, both prints and the first differing line, and
/// fails this unit's compilation so no later object equality can hide it.
#[cfg(hirsel_lazy_core)]
fn printed_oracle(
    dir: &std::path::Path,
    module: &Module,
    native: &str,
    produce: impl FnOnce(&mut sink::TextSink) -> Result<()>,
) -> Result<()> {
    use std::io::Write as _;
    let mut text = sink::TextSink::default();
    produce(&mut text)?;
    let text = text.finish();
    let parsed = crate::construction::print_text(&text).map_err(Error::Llvm)?;
    let identity = module
        .instances
        .first()
        .map_or_else(String::new, |i| i.definition.to_string());
    let digest = hirsel_core_ids::ContentHash::of_bytes(text.as_bytes()).0[..8]
        .iter()
        .fold(String::new(), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        });
    let difference = native
        .lines()
        .zip(parsed.lines())
        .position(|(a, b)| a != b)
        .or_else(|| (native.lines().count() != parsed.lines().count()).then_some(0));
    let io = |e: std::io::Error| Error::Llvm(format!("printed oracle {}: {e}", dir.display()));
    std::fs::create_dir_all(dir).map_err(io)?;
    let row = format!(
        "{}\t{digest}\t{identity}\t{}\t{}\n",
        if difference.is_some() { "DIFF" } else { "SAME" },
        native.lines().count(),
        difference.map_or(String::from("-"), |l| (l + 1).to_string()),
    );
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("units.tsv"))
        .and_then(|mut f| f.write_all(row.as_bytes()))
        .map_err(io)?;
    let Some(line) = difference else {
        return Ok(());
    };
    for (suffix, body) in [
        ("ll", &text),
        ("native.ll", &native.to_owned()),
        ("parsed.ll", &parsed),
    ] {
        std::fs::write(dir.join(format!("{digest}.{suffix}")), body).map_err(io)?;
    }
    Err(Error::Llvm(format!(
        "printed oracle: unit {identity} ({digest}) differs at printed line {}",
        line + 1
    )))
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
            batch_steps: true,
            test_hooks: cfg!(test),
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
            batch_steps: true,
            test_hooks: cfg!(test),
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
            batch_steps: true,
            test_hooks: cfg!(test),
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
            batch_steps: true,
            test_hooks: cfg!(test),
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
        let native = native_module(
            module,
            target,
            context,
            observe_charge,
            source,
            EmissionFacts::default(),
        )?;
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
            batch_steps: true,
            test_hooks: cfg!(test),
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
        Ty::Vector(v) => simd::vector_type(v.lanes(), v.lane().size() as u32 * 8),
    }
}
fn bits_ty(t: Ty) -> Type {
    Type::Int(size(t) as u32 * 8)
}
fn size(t: Ty) -> u64 {
    match t {
        Ty::Ptr => 8,
        Ty::Scalar(s) => s.size(),
        Ty::Vector(v) => v.size(),
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
    level: Option<Level>,
    call_layouts: Option<&'a hirsel_core_ir::native_layout::CallLayouts>,
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
    receive_loops: hirsel_core_ir::receive_loops::ReceiveLoops,
    facts: bool,
    aliases: Option<alias::Scopes>,
    caller_site: Value,
    debug_location: Option<usize>,
    anchor: Option<Anchor>,
    relocatable_sites: bool,
    test_hooks: bool,
    value_slots: Vec<bool>,
    /// Static inline arrays converted directly into constant vectors.
    static_vectors: Option<Box<[(ValueId, StaticId)]>>,
    /// Each constant's literal, used in place of the value.
    literals: Vec<Option<Value>>,
    address_slots: Vec<bool>,
    /// Slots whose `StorageLive` and `StorageDead` begin and end their storage
    /// (hirsel_core_ir::extent::scoped_slots). Every return ends those that
    /// may be live there too, so an inlined copy's storage ends where the
    /// callee would have returned.
    scoped: hirsel_core_ir::extent::ScopedSlots,
    /// The slots that may be live at the point being emitted, one bit per
    /// slot, or `None` where unknown (a return then ends every scoped slot).
    live: Option<Vec<u64>>,
    /// Whether a forwarded tail call has already ended the scoped slots
    /// before the return that follows it.
    scoped_ended: bool,
    step_labels: std::collections::BTreeMap<usize, Label>,
    unchecked_steps: bool,
    precharge: Option<steps::Precharge>,
    step_neutral: Option<&'a [bool]>,
    step_counter: Option<Value>,
    neutral_step_call: bool,
    view_invariants: Vec<(Value, Value, u64)>,
    sink: &'a mut dyn Sink,
    items: &'a mut Items,
    next: usize,
    /// In fixed counted glue, the cursor after each suspension point numbered
    /// so far (None inside an array), in the order hirsel_core_layout::frames
    /// lists them; None elsewhere.
    fixed_point: Option<Vec<Option<u64>>>,
    /// In fixed counted glue, the step of the field being walked counted from
    /// the block's first; None inside an array.
    fixed_rank: Option<u64>,
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
    /// `name` denotes `x`. A local is bound by name, so uses before its
    /// definition (CoreIR blocks need not be in dominance order) resolve too.
    /// LLVM has no scalar copy instruction, so a constant goes through the
    /// identity select, which preserves poison and adds no representation promise.
    fn copy(&mut self, name: Local, t: &Type, x: &Value) {
        if let Value::Local(value) = x {
            self.sink.bind(name, t, *value);
        } else {
            self.emit_to(name, Operation::Select(t, &true.into(), x, &Value::Poison));
        }
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
        self.load_with(t, p, &Memory::aligned(align))
    }
    fn load_with(&mut self, t: &Type, p: &Value, m: &Memory) -> Value {
        let m = alias::program_memory(m, self.facts);
        self.emit(Operation::Load(t, p, &m))
    }
    fn store(&mut self, t: &Type, v: &Value, p: &Value, align: u32) {
        self.store_with(t, v, p, &Memory::aligned(align));
    }
    fn store_with(&mut self, t: &Type, v: &Value, p: &Value, m: &Memory) {
        let m = alias::program_memory(m, self.facts);
        self.effect(Operation::Store(t, v, p, &m));
    }
    fn gep(&mut self, t: &Type, p: &Value, index: &Value, inbounds: bool) -> Value {
        self.emit(Operation::Gep {
            ty: t,
            base: p,
            indices: &[(Type::I64, index.clone())],
            inbounds,
        })
    }
    /// Address a byte within a place. At offset zero the base is the address.
    fn offset(&mut self, p: &Value, n: impl Into<i128>) -> Value {
        let n = n.into();
        if n == 0 {
            return p.clone();
        }
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
        self.end_view_invariants();
        self.flush_steps();
        self.end_scoped();
        self.effect(Operation::Return(Some((t, v))));
    }
    fn ret_void(&mut self) {
        self.end_view_invariants();
        self.flush_steps();
        self.end_scoped();
        self.effect(Operation::Return(None));
    }
    /// Ends every scoped slot's storage that may be live before a return, once.
    fn end_scoped(&mut self) {
        if std::mem::take(&mut self.scoped_ended) {
            return;
        }
        for slot in 0..self.scoped.slots.len() {
            let ended = self
                .live
                .as_ref()
                .is_some_and(|live| live[slot / 64] & (1 << (slot % 64)) == 0);
            if !self.scoped.slots[slot] || ended {
                continue;
            }
            let home = Value::from(Local::Slot(slot));
            self.runtime_void("llvm.lifetime.end.p0", &[ptr(home)]);
        }
    }
    fn end_view_invariants(&mut self) {
        for (token, pointer, bytes) in self.view_invariants.clone() {
            self.intrinsic(
                "llvm.invariant.end.p0",
                Type::Void,
                &[ptr(token), word(int(bytes)), ptr(pointer)],
            );
        }
    }
    fn call(&mut self, sig: &FnSig, callee: &Value, args: &[Value], attrs: &CallAttrs) -> Value {
        let flush = !self.neutral_step_call
            && !matches!(callee, Value::Global(name) if name.starts_with("llvm."));
        if flush {
            self.flush_steps();
        }
        let mut attrs = attrs.clone();
        attrs.step_neutral |= self.neutral_step_call;
        let op = Operation::Call(sig, callee, args, &attrs);
        let value = if sig.result.is_void() {
            self.effect(op);
            Value::Poison
        } else {
            self.emit(op)
        };
        if flush {
            self.reload_steps();
        }
        value
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
        if let Some(Some(x)) = self.literals.get(v.index()) {
            return x.clone();
        }
        if self.value_slots.get(v.index()).copied().unwrap_or(true) {
            self.load(&ty(self.vty(v)), &Local::Value(v.index()).into(), 0)
        } else {
            Local::Value(v.index()).into()
        }
    }
    fn literal(&self, v: ValueId, bits: u64) -> Value {
        let t = self.vty(v);
        if t.is_float() {
            Value::Bitcast {
                from: bits_ty(t),
                bits,
                to: ty(t),
            }
        } else if t == Ty::Ptr {
            Value::IntToPtr(bits)
        } else {
            int(bits)
        }
    }
    fn put(&mut self, v: ValueId, x: &Value) {
        let t = ty(self.vty(v));
        if self.value_slots.get(v.index()).copied().unwrap_or(true) {
            self.store(&t, x, &Local::Value(v.index()).into(), 0);
        } else {
            self.copy(Local::Value(v.index()), &t, x);
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
        self.call_declared(
            name,
            result,
            args,
            FnAttrs::default(),
            &CallAttrs::default(),
        )
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
        if let (None, Some(fail), None) = (cause, fail, &self.precharge) {
            // The callee has already recorded its cause. This edge has no
            // arguments or cleanup to perform before the existing successor.
            self.cond(flag, Label::block(fail.0), ok);
        } else {
            let bad = Label::new("bad", n);
            self.cond(flag, bad, ok);
            self.block(bad);
            self.step_refund();
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
            if let Some(sites) = &self.items.release_sites {
                let anchor = self.anchor.unwrap();
                sites.names[&(anchor.item, anchor.start)].clone()
            } else {
                let anchor = self.anchor.unwrap();
                format!("{}_{}", anchor.item.0, anchor.start)
            }
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
        let initialized =
            fixed && self.relocatable_sites && self.abi.target == Target::Aarch64Linux;
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

    /// Starts tracking which slots may be live at `block`'s entry.
    fn enter_block(&mut self, block: usize) {
        self.live = Some(self.scoped.may_live_at(block));
    }

    fn inst(&mut self, inst: &Inst, index: usize) -> Result<()> {
        self.anchor = self.body.inst_anchors[index];
        if let (Inst::StorageLive { slot } | Inst::StorageDead { slot }, Some(live)) =
            (inst, self.live.as_mut())
        {
            let (word, bit) = (slot.index() / 64, 1u64 << (slot.index() % 64));
            if matches!(inst, Inst::StorageLive { .. }) {
                live[word] |= bit;
            } else {
                live[word] &= !bit;
            }
        }
        match *inst {
            // Its literal stands at each use.
            Inst::Const { .. } => {}
            Inst::LayoutConst { .. } => unreachable!("engine-private operands are resolved"),
            Inst::ReadStatic { result, data } => {
                self.put(result, &Value::global(format!("s{}", data.0)))
            }
            Inst::FunctionRef { result, function } => {
                let name = self.items.function_symbol(function).to_owned();
                self.put(result, &Value::global(name))
            }
            Inst::DescriptorRef {
                result, function, ..
            } => self.put(
                result,
                &Value::global(descriptor_symbol(self.items.function_symbol(function))),
            ),
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
            Inst::Simd {
                result,
                op,
                first,
                second,
                fail,
            } => self.simd(result, op, first, second, fail)?,
            Inst::PutLanes { to, vector, count } => self.put_lanes(to, vector, count),
            Inst::Address { address, fail } => self.project(address, fail)?,
            Inst::Read {
                result, address, ..
            } => {
                let p = self.address(address);
                let memory = Memory {
                    align: 1,
                    range: self.seq_word_range(address),
                    scopes: self.access_facts(&[address]),
                    tbaa: self
                        .facts
                        .then(|| {
                            alias::writer_state_memory(self.module, self.body, address)
                                .or_else(|| self.storage_memory(address))
                        })
                        .flatten(),
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
                    tbaa: self
                        .facts
                        .then(|| {
                            alias::writer_state_memory(self.module, self.body, address)
                                .or_else(|| self.storage_memory(address))
                        })
                        .flatten(),
                    ..Memory::default()
                };
                self.store_with(&ty(self.vty(value)), &x, &p, &memory);
            }
            Inst::StorageLive { slot } if self.scoped.slots[slot.index()] => {
                let home = Value::from(Local::Slot(slot.index()));
                self.runtime_void("llvm.lifetime.start.p0", &[ptr(home)]);
            }
            Inst::StorageDead { slot } if self.scoped.slots[slot.index()] => {
                let home = Value::from(Local::Slot(slot.index()));
                self.runtime_void("llvm.lifetime.end.p0", &[ptr(home)]);
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
            Inst::Call { call } => {
                self.neutral_step_call = self.step_neutral.is_some_and(|neutral| {
                    steps::neutral_inst(self.module, self.body, inst, neutral)
                });
                let result = self.call_site(
                    &self.body.calls[call.index()],
                    self.receive_loops.call(call),
                );
                self.neutral_step_call = false;
                result?;
            }
            _ => {
                // Preserve the same no-Step proof used by region planning through
                // allocation, growth and inert cleanup runtime calls too.
                self.neutral_step_call = self.step_neutral.is_some_and(|neutral| {
                    steps::neutral_inst(self.module, self.body, inst, neutral)
                });
                let result = self.storage(inst, index);
                self.neutral_step_call = false;
                result?;
            }
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
        self.step_label(e.target.index())
    }
    /// Whether the front marked `edge` cold (core-ir Edge::cold), or it enters a
    /// block that makes a call the front marked cold (core-ir CallSite::cold: the
    /// true arm of a growth check).
    fn cold_edge(&self, edge: EdgeId) -> bool {
        if self.body.edges[edge.index()].cold {
            return true;
        }
        let target = self.body.edges[edge.index()].target;
        let block = &self.body.blocks[target.index()];
        block
            .insts
            .get(&self.body.insts)
            .unwrap()
            .iter()
            .any(|i| matches!(i, Inst::Call { call } if self.body.calls[call.index()].cold))
    }
    fn edge_label(&mut self, edge: EdgeId) -> Label {
        let e = &self.body.edges[edge.index()];
        if e.args.len == 0 {
            return self.step_label(e.target.index());
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
                let y = self.edge_label(yes);
                let n = self.edge_label(no);
                // An edge into a block that makes a cold call is unlikely.
                match (self.cold_edge(yes), self.cold_edge(no)) {
                    (true, false) => {
                        let c = self.cmp(Predicate::Eq, &Type::I8, &c, &int(0));
                        self.likely(&c, n, y);
                    }
                    (false, true) => {
                        let c = self.cmp(Predicate::Ne, &Type::I8, &c, &int(0));
                        self.likely(&c, y, n);
                    }
                    _ => {
                        let c = self.cmp(Predicate::Ne, &Type::I8, &c, &int(0));
                        self.cond(&c, y, n);
                    }
                }
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
                self.step_refund();
                let xs = values
                    .get(&self.body.pools.values)
                    .unwrap()
                    .iter()
                    .map(|v| self.numeric(*v))
                    .collect::<Vec<_>>();
                if let Some(record) = self.abi.record {
                    let fields = self.load_record(record, &Local::DEST.into());
                    self.return_values(&fields);
                } else {
                    self.return_values(&xs);
                }
            }
            Terminator::Unreachable => self.effect(Operation::Unreachable),
            Terminator::Propagate => {
                self.step_refund();
                self.return_failure();
            }
            Terminator::Raise { cause, cleanup } => {
                self.step_refund();
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

/// Source-law diagnostic control with exactly the requested producer facts.
#[doc(hidden)]
pub fn emit_ir_for_test_with_facts(
    module: &Module,
    target: Target,
    observe_charge: bool,
    debug_lines: bool,
    facts: bool,
) -> Result<String> {
    emit_ir_target_with_role_facts(
        module,
        target,
        observe_charge,
        facts,
        SourceEmission {
            batch_steps: true,
            test_hooks: true,
            relocatable_sites: true,
            debug_lines,
        },
        None,
    )
}
/// Native law fixture: admission injection and cleanup traces are explicit,
/// independent of which crate owns the source law.
#[doc(hidden)]
pub fn emit_ir_for_test(
    module: &Module,
    target: Target,
    observe_charge: bool,
    debug_lines: bool,
    roles: Option<&hirsel_core_ir::roles::Facts>,
) -> Result<String> {
    emit_ir_target_with_role_facts(
        module,
        target,
        observe_charge,
        facts_enabled(),
        SourceEmission {
            batch_steps: true,
            test_hooks: true,
            relocatable_sites: true,
            debug_lines,
        },
        roles,
    )
}
/// The direct engine's object for the same native law fixture.
#[doc(hidden)]
pub fn emit_object_for_test(
    module: &Module,
    target: Target,
    optimization: Optimization,
    observe_charge: bool,
    debug_lines: bool,
) -> Result<Vec<u8>> {
    emit_object_site_binding(
        module,
        target,
        optimization,
        observe_charge,
        SourceEmission {
            batch_steps: true,
            test_hooks: true,
            relocatable_sites: true,
            debug_lines,
        },
    )
}
