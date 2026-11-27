//! Release units in partitions: the analyses every partition shares, how a
//! unit splits into partitions, and how a partition declares the bodies the
//! others define.

use super::*;

/// The module as its bodies lower: native layout resolved and, with batched
/// steps, exposed.
pub(super) fn prepare<'m>(
    module: Cow<'m, Module>,
    recipes: &[hirsel_core_layout::RecipeRow],
    source: SourceEmission,
    provided: EmissionFacts<'_>,
) -> Cow<'m, Module> {
    let module = hirsel_core_ir::native_layout::resolve(module, recipes);
    if source.batch_steps {
        expose::module(module, provided.prepay_step_trees)
    } else {
        module
    }
}

/// What lowering reads of the whole unit, which every partition shares.
pub(super) struct Analyses {
    /// The unit's symbols, levels, step dispatch and callback sites.
    pub(super) items: Items,
    pub(super) failures: failure::Failures,
    /// Each defined body's definition-inline decision, read before any
    /// partition body is freed.
    inline: Vec<Option<Inline>>,
    /// No lowering of the body returns to its caller.
    noreturn: Vec<bool>,
    pub(super) step_neutral: Vec<bool>,
    pub(super) call_layouts: hirsel_core_ir::native_layout::CallLayouts,
    pub(super) local_roles: Option<hirsel_core_ir::roles::Facts>,
    /// Read from every body before the first partition, which a later
    /// partition may no longer hold.
    pub(super) descriptors: Descriptors,
    no_jump_tables: bool,
}

impl Analyses {
    pub(super) fn new(
        module: &Module,
        recipes: &[hirsel_core_layout::RecipeRow],
        source: SourceEmission,
        provided: EmissionFacts<'_>,
        facts: bool,
    ) -> Self {
        let mut items = Items::new(module);
        if let Some(known) = provided.versions.filter(|known| !known.is_empty()) {
            items.versioned = Some(
                module
                    .functions
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        !f.pack_export
                            && (f.versioned
                                || known.contains(&module.code_identity(FuncId(i as u32))))
                    })
                    .collect(),
            );
        }
        for (&f, free) in &module.step_free {
            if free.dispatch && !items.is_versioned(f) && !module.functions[f.index()].pack_export {
                items.step_dispatch[f.index()] = Some(free.body);
            }
        }
        let step_variants = hirsel_core_ir::step_contexts::variants(module);
        items.callback_sites = source
            .batch_steps
            .then(|| inlining::CallbackSites::new(module, &step_variants));
        let descriptors = Descriptors::new(module, &items);
        let failures = failure::Failures::new(module);
        let called_in_loop = empty::called_in_loop(module);
        let noreturn = module
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| {
                !failures.may_fail[i]
                    && f.body.as_ref().is_some_and(|body| {
                        !body
                            .insts
                            .iter()
                            .any(|inst| matches!(inst, Inst::Step { .. }))
                            && body.terminators.iter().all(|terminator| {
                                !matches!(
                                    terminator,
                                    Terminator::Return(_)
                                        | Terminator::Raise { .. }
                                        | Terminator::Propagate
                                )
                            })
                    })
            })
            .collect();
        let inline = module
            .functions
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let body = f.body.as_ref()?;
                let definition = module.instances[f.instance.index()].definition.to_string();
                // FAIL-17: a checked body with more than a leaf's frame stays out of
                // line, so inlining cannot merge frames into one past the reservation.
                let slot_bytes = |t: PlaceTy| match t {
                    PlaceTy::Value(t) => size(t),
                    PlaceTy::Declared(t) => {
                        match (module.types[t.index()].repr, &recipes[t.index()].layout) {
                            (
                                hirsel_core_layout::Repr::Resource
                                | hirsel_core_layout::Repr::Region,
                                _,
                            ) => 8,
                            (_, hirsel_core_layout::Layout::Fixed(f)) => f.size,
                            _ => 0,
                        }
                    }
                };
                if body
                    .insts
                    .iter()
                    .any(|i| matches!(i, Inst::StackCheck { .. }))
                    && body.slots.iter().map(|&t| slot_bytes(t)).sum::<u64>()
                        > hirsel_core_ir::STACK_LEAF_SLOT_BYTES
                {
                    return Some(Inline::Never);
                }
                Some(inlining::definition(
                    &definition,
                    module,
                    f,
                    body,
                    source.batch_steps,
                    called_in_loop[i],
                    inlining::specialized(module, &step_variants, FuncId(i as u32)),
                ))
            })
            .collect();
        Self {
            items,
            failures,
            inline,
            noreturn,
            step_neutral: steps::neutral_functions(module, provided.neutral),
            call_layouts: hirsel_core_ir::native_layout::CallLayouts::new(&module.types, recipes),
            local_roles: (facts && provided.roles.is_none())
                .then(|| hirsel_core_ir::roles::Facts::new(module)),
            descriptors,
            no_jump_tables: provided.no_jump_tables,
        }
    }

    /// A partition's items: the unit's, with nothing pending.
    pub(super) fn items(&self) -> Items {
        let items = &self.items;
        Items {
            function_symbols: items.function_symbols.clone(),
            versioned: items.versioned.clone(),
            step_dispatch: items.step_dispatch.clone(),
            callback_sites: items.callback_sites.clone(),
            pending: Default::default(),
            release_sites: None,
            direct: BTreeSet::new(),
            track_site_owner: false,
        }
    }

    /// The attributes of a body, shared by its definition and declarations.
    pub(super) fn body_attrs(
        &self,
        module: &Module,
        recipes: &[hirsel_core_layout::RecipeRow],
        roles: Option<&hirsel_core_ir::roles::Facts>,
        function: FuncId,
        locals: &[Local],
        facts: bool,
    ) -> FnAttrs {
        let i = function.index();
        let f = &module.functions[i];
        let definition = module.instances[f.instance.index()].definition.to_string();
        FnAttrs {
            inline: self.inline[i].expect("a defined body has an inline decision"),
            cold: f.cold,
            noreturn: facts && self.noreturn[i],
            // Core and the runtime ABI report failure as a status, without unwinding.
            nounwind: facts,
            map_probe: definition == "core@1/map/probe",
            no_jump_tables: self.no_jump_tables,
            carriers: if f.pack_export {
                String::new()
            } else {
                carriers(
                    module,
                    recipes,
                    roles,
                    function,
                    &module.signatures[f.signature.index()],
                    locals,
                )
            },
            ..FnAttrs::default()
        }
    }
}

/// How a body this unit does not define is declared where another calls it:
/// each of its symbols, the signature and attributes its definition has.
pub(super) type Foreign = std::collections::HashMap<String, (FnSig, FnAttrs)>;

/// Static `i`'s bytes, a private constant `s{i}`.
pub(super) fn define_static(
    module: &Module,
    recipes: &[hirsel_core_layout::RecipeRow],
    i: usize,
    sink: &mut dyn Sink,
) {
    let (align, bytes) = module.statics[i].native(&module.types, recipes);
    sink.global(&Global {
        name: format!("s{i}").into(),
        linkage: Linkage::Private,
        constant: true,
        externally_initialized: false,
        ty: Type::bytes(bytes.len() as u64),
        init: Some(Value::Bytes(bytes.into_owned())),
        align,
        absolute_symbol: false,
        section: None,
    });
}

/// The bodies of `module`, or of its `members`, with their indices.
pub(super) fn bodies<'m>(
    module: &'m Module,
    members: Option<&'m [bool]>,
) -> impl Iterator<Item = (usize, &'m Body)> + 'm {
    module
        .functions
        .iter()
        .enumerate()
        .filter(move |(i, _)| members.is_none_or(|members| members[*i]))
        .filter_map(|(i, f)| Some((i, f.body.as_ref()?)))
}

/// The unit's descriptor references, each with the function its
/// descriptor's code word names, and that function's revision root.
///
/// Callable adapters of one generic instance share its descriptor symbol
/// ([`Module::descriptor_symbol`]), so one symbol can describe several
/// functions, which partitions may define apart. The unit's first
/// reference to a symbol, as the whole unit defines it, names the one
/// function every partition describes it by: only that function's
/// partition defines it strong, and the others weak. A `$callable`
/// adapter's revision root is read from its own body.
pub(super) struct Descriptors {
    /// Each reference: the body holding it, its function, identity, spawn.
    refs: Vec<(usize, FuncId, StaticId, Option<SpawnTypes>)>,
    pub(super) roots: std::collections::BTreeMap<FuncId, hirsel_core_ir::RevisionRoot>,
}

impl Descriptors {
    fn new(module: &Module, items: &Items) -> Self {
        let name = |f: FuncId| module.descriptor_symbol(f, items.function_symbol(f));
        let mut owners = std::collections::HashMap::new();
        let mut owner = vec![None; module.functions.len()];
        let mut refs = Vec::new();
        for (i, body) in bodies(module, None) {
            for inst in &body.insts {
                let Inst::DescriptorRef {
                    function,
                    identity,
                    spawn,
                    ..
                } = *inst
                else {
                    continue;
                };
                let owner = *owner[function.index()]
                    .get_or_insert_with(|| *owners.entry(name(function)).or_insert(function));
                refs.push((i, owner, identity, spawn));
            }
        }
        let mut roots = std::collections::BTreeMap::new();
        for &(_, function, _, _) in &refs {
            roots
                .entry(function)
                .or_insert_with(|| module.revision_root(function));
        }
        Self { refs, roots }
    }

    /// The references whose descriptors the unit, or the partition of its
    /// `members`, defines: its bodies' own, and in a partition every
    /// reference to a member's descriptor wherever it is, since the release
    /// cut keeps a descriptor (and its spawnable markers) only in its
    /// function's part.
    pub(super) fn refs<'a>(
        &'a self,
        members: Option<&'a [bool]>,
    ) -> impl Iterator<Item = (FuncId, StaticId, Option<SpawnTypes>)> + 'a {
        let member = move |i: usize| members.is_none_or(|members| members[i]);
        self.refs
            .iter()
            .filter(move |&&(i, function, _, _)| member(i) || member(function.index()))
            .map(|&(_, function, identity, spawn)| (function, identity, spawn))
    }
}

/// A body another partition defines, as [`emit_prepared`] defines it.
pub(super) struct ForeignBody<'n, P> {
    pub(super) name: &'n str,
    /// A selecting entry: its checked body has its own symbol (`step_free`).
    pub(super) free: bool,
    pub(super) versioned: bool,
    /// Versioned with dispatching stubs (`versions::stub`).
    pub(super) stubs: bool,
    pub(super) suffix: &'static str,
    /// Its convention is private, behind a public wrapper.
    pub(super) private: bool,
    pub(super) body: FnSig,
    pub(super) public: P,
    pub(super) attrs: FnAttrs,
}

/// Record each symbol's definition attributes and ABI. Every foreign symbol
/// is declared hidden and only its defining partition owns its debug metadata.
pub(super) fn foreign_symbols(
    foreign: &mut Foreign,
    body: ForeignBody<'_, impl Fn(bool) -> FnSig>,
) {
    let ForeignBody {
        name,
        free,
        versioned,
        stubs,
        suffix,
        private,
        body,
        public,
        attrs,
    } = body;
    let mut declare = |symbol, signature, mut attrs: FnAttrs| {
        attrs.linkage = Linkage::Hidden;
        attrs.subprogram = None;
        foreign.insert(symbol, (signature, attrs));
    };
    let public_wrapper = private.then(|| public(true));
    let levels: Vec<Option<Level>> = if versioned {
        X86_64_LEVELS.iter().copied().map(Some).collect()
    } else {
        vec![None]
    };
    let variant = |level: Option<Level>| match level {
        Some(level) => versions::symbol(name, level),
        None => name.to_owned(),
    };
    for level in levels {
        let name = variant(level);
        let body_name = format!("{name}{suffix}");
        let attrs = FnAttrs {
            level,
            ..attrs.clone()
        };
        if free {
            declare(
                format!("{}{suffix}", step_free::checked(&name)),
                body.clone(),
                attrs,
            );
            declare(
                body_name,
                body.clone(),
                step_free::stub_attrs(Linkage::Hidden),
            );
        } else {
            declare(body_name, body.clone(), attrs);
        }
        if let Some(signature) = &public_wrapper {
            declare(
                name,
                signature.clone(),
                FnAttrs::public_wrapper(level, None),
            );
        }
    }
    if stubs {
        let attrs = versions::stub_attrs(Linkage::Hidden);
        if private {
            declare(format!("{name}{suffix}"), body, attrs.clone());
        }
        declare(name.to_owned(), public(false), attrs);
    }
}

/// Each body's partition of a release unit over `bound` Core instructions,
/// with their count: a power of two, so each body's partition is its link
/// symbol's hash (FNV-1a) modulo the count. A body keeps its partition while
/// the unit grows or shrinks within a count. A unit within the bound is one
/// partition, lowered whole.
#[cfg(hirsel_lazy_core)]
fn partitions(module: &Module, bound: usize) -> Option<(usize, Vec<usize>)> {
    let total: usize = bodies(module, None).map(|(_, body)| body.insts.len()).sum();
    if total <= bound {
        return None;
    }
    let count = total.div_ceil(bound).next_power_of_two();
    let of = |symbol: &str| {
        let hash = symbol
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        (hash % count as u64) as usize
    };
    Some((count, module.link_symbols().iter().map(|s| of(s)).collect()))
}

#[cfg(hirsel_lazy_core)]
mod key;

#[cfg(hirsel_lazy_core)]
pub(crate) type LowerPartition<'a> =
    Box<dyn FnOnce() -> Result<(crate::construction::NativeModule, crate::ReleaseSites)> + 'a>;

/// Prepare a release unit, then visit its partitions in link order. A
/// partition's key is available before its LLVM context or module exists.
#[cfg(hirsel_lazy_core)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_release_unit(
    module: Module,
    roles: &hirsel_core_ir::roles::Facts,
    target: Target,
    observe_charge: bool,
    number_sites: bool,
    neutral: &BTreeSet<hirsel_core_ids::ContentHash>,
    bound: usize,
    each: &mut dyn FnMut(
        Option<hirsel_core_ids::ContentHash>,
        LowerPartition<'_>,
    ) -> std::result::Result<(), String>,
) -> Result<()> {
    let versions = match target.arch() {
        hirsel_core_layout::Arch::X86_64 => Some(versions::program(&[&module])),
        hirsel_core_layout::Arch::Aarch64 => None,
    };
    let source = SourceEmission {
        batch_steps: true,
        test_hooks: cfg!(test),
        relocatable_sites: true,
        debug_lines: true,
        fixed_sites: &[],
    };
    let provided = EmissionFacts {
        roles: Some(roles),
        neutral: Some(neutral),
        prepay_step_trees: true,
        release_sites: number_sites,
        versions: versions.as_ref(),
        no_jump_tables: false,
    };
    let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
    let debug_items = number_sites.then(|| std::rc::Rc::<[_]>::from(module.items.clone()));
    let mut module = prepare(Cow::Owned(module), &recipes, source, provided);
    let plan = partitions(&module, bound);
    let facts = facts_enabled();
    let analyses = Analyses::new(&module, &recipes, source, provided, facts);
    let printed = std::env::var_os("HIRSEL_LLVM_PRINTED_ORACLE");
    let keys = plan
        .as_ref()
        .filter(|_| printed.is_none())
        .map(|_| key::Keys::new(&module, &analyses, roles, facts))
        .transpose()
        .map_err(|e| Error::Llvm(e.to_string()))?;
    for partition in 0..plan.as_ref().map_or(1, |(count, _)| *count) {
        let members = plan
            .as_ref()
            .map(|(_, of)| of.iter().map(|&p| p == partition).collect::<Vec<_>>());
        let namespace = members
            .as_ref()
            .map(|_| u32::try_from(partition).expect("Core function indices"));
        let key = keys
            .as_ref()
            .zip(members.as_deref())
            .map(|(keys, members)| keys.partition(&module, &analyses, members, namespace.unwrap()))
            .transpose()
            .map_err(|e| Error::Llvm(e.to_string()))?;
        // Match the whole-unit emitter's context on the former composed
        // paths, and the direct emitter's context on the x86 executable path.
        let anonymous = number_sites;
        let mut lowered = false;
        each(
            key,
            Box::new(|| {
                let context = std::rc::Rc::new(
                    crate::construction::Context::with_anonymous_locals(anonymous)
                        .map_err(Error::Llvm)?,
                );
                let debug = || {
                    debug_items
                        .as_ref()
                        .map(|items| sites::ReleaseDebug::new(items.clone(), namespace))
                };
                let mut native = match debug() {
                    Some(debug) => sink::native::NativeSink::release(context, debug),
                    None => sink::native::NativeSink::new(context),
                };
                let produce = |sink: &mut dyn Sink| {
                    let mut sites = number_sites.then(|| ReleaseSites {
                        partition: namespace,
                        ..ReleaseSites::default()
                    });
                    let foreign = emit_prepared(
                        &module,
                        &recipes,
                        &analyses,
                        target,
                        observe_charge,
                        facts,
                        source,
                        provided,
                        members.as_deref(),
                        &mut sites,
                        sink,
                    )?;
                    Ok::<_, Error>((foreign, sites.map_or_else(Vec::new, |sites| sites.bindings)))
                };
                let (foreign, sites) = produce(&mut native)?;
                let unresolved = members
                    .as_ref()
                    .map(|_| native.unresolved().map_err(Error::Llvm))
                    .transpose()?;
                if let Some(names) = &unresolved {
                    close_partition(&module, &recipes, &foreign, names, &mut native);
                }
                if let Some(dir) = &printed {
                    let text =
                        debug().map_or_else(sink::TextSink::default, sink::TextSink::release);
                    printed_oracle(
                        std::path::Path::new(dir),
                        &module,
                        &native.print().map_err(Error::Llvm)?,
                        anonymous,
                        text,
                        |text| {
                            let (foreign, _) = produce(text)?;
                            if let Some(names) = &unresolved {
                                close_partition(&module, &recipes, &foreign, names, text);
                            }
                            Ok(())
                        },
                    )?;
                }
                let debug_sites = native.release_debug_sites();
                let native = native.finish().map_err(Error::Llvm)?;
                if let Some(members) = &members {
                    release_bodies(module.to_mut(), members);
                }
                lowered = true;
                Ok((native, crate::ReleaseSites { sites, debug_sites }))
            }),
        )
        .map_err(Error::Llvm)?;
        if !lowered && let Some(members) = &members {
            release_bodies(module.to_mut(), members);
        }
    }
    Ok(())
}

/// Free the bodies of a lowered partition's `members` that no later partition
/// reads: a partition reads another's body only to see that it is one
/// ([`foreign_symbols`]), and a caller's lowering reads a scalar callee's
/// ([`steps::read_by_callers`]). Every analysis that reads the whole unit's
/// bodies ran before the first partition ([`Analyses`]); a coverage build's
/// records read every body in each partition, so it keeps them.
#[cfg(hirsel_lazy_core)]
fn release_bodies(module: &mut Module, members: &[bool]) {
    if module.coverage.is_some() {
        return;
    }
    for (i, &member) in members.iter().enumerate() {
        if member && !steps::read_by_callers(module, FuncId(i as u32)) {
            if let Some(body) = &mut module.functions[i].body {
                *body = Body::default();
            }
        }
    }
}

/// Define the statics a partition's bodies read, declare the other
/// partitions' bodies they call ([`Foreign`]), and mark the module a release
/// partition for the optimiser (`hirsel_llvm_release_partition`).
#[cfg(hirsel_lazy_core)]
fn close_partition(
    module: &Module,
    recipes: &[hirsel_core_layout::RecipeRow],
    foreign: &Foreign,
    unresolved: &[String],
    sink: &mut dyn Sink,
) {
    for name in unresolved {
        if !foreign.contains_key(name)
            && let Some(i) = name.strip_prefix('s').and_then(|i| i.parse::<usize>().ok())
            && i < module.statics.len()
        {
            define_static(module, recipes, i, sink);
        }
    }
    for name in unresolved {
        if let Some((signature, attrs)) = foreign.get(name) {
            sink.declare(name, signature, attrs);
        }
    }
    sink.named_metadata("hirsel.partition", &[]);
}
