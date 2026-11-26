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
    pub(super) step_variants: std::collections::BTreeMap<FuncId, FuncId>,
    pub(super) failures: failure::Failures,
    pub(super) called_in_loop: Vec<bool>,
    pub(super) step_neutral: Vec<bool>,
    pub(super) call_layouts: hirsel_core_ir::native_layout::CallLayouts,
    pub(super) local_roles: Option<hirsel_core_ir::roles::Facts>,
    /// Read from every body before the first partition, which a later
    /// partition may no longer hold.
    pub(super) descriptors: Descriptors,
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
                        f.versioned || known.contains(&module.code_identity(FuncId(i as u32)))
                    })
                    .collect(),
            );
        }
        for (&f, free) in &module.step_free {
            if free.dispatch && !items.is_versioned(f) {
                items.step_dispatch[f.index()] = Some(free.body);
            }
        }
        let step_variants = hirsel_core_ir::step_contexts::variants(module);
        items.callback_sites = source
            .batch_steps
            .then(|| inlining::CallbackSites::new(module, &step_variants));
        let descriptors = Descriptors::new(module, &items);
        Self {
            items,
            step_variants,
            failures: failure::Failures::new(module),
            called_in_loop: empty::called_in_loop(module),
            step_neutral: steps::neutral_functions(module, provided.neutral),
            call_layouts: hirsel_core_ir::native_layout::CallLayouts::new(&module.types, recipes),
            local_roles: (facts && provided.roles.is_none())
                .then(|| hirsel_core_ir::roles::Facts::new(module)),
            descriptors,
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

/// Record how `body`'s symbols are declared: its bodies and stubs with its
/// own convention, its public wrappers and public stub with the public one.
pub(super) fn foreign_symbols(
    foreign: &mut Foreign,
    body: ForeignBody<'_, impl FnOnce() -> FnSig>,
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
    let public = (private || stubs).then(public);
    let mut own = |symbol: String, level: Option<Level>| {
        let attrs = FnAttrs {
            level,
            ..attrs.clone()
        };
        foreign.insert(symbol, (body.clone(), attrs));
    };
    let levels: Vec<Option<Level>> = if versioned {
        X86_64_LEVELS.iter().copied().map(Some).collect()
    } else {
        vec![None]
    };
    let variant = |level: Option<Level>| match level {
        Some(level) => versions::symbol(name, level),
        None => name.to_owned(),
    };
    for &level in &levels {
        if free {
            own(
                format!("{}{suffix}", step_free::checked(&variant(level))),
                level,
            );
        }
        own(format!("{}{suffix}", variant(level)), level);
    }
    if stubs && private {
        own(format!("{name}{suffix}"), None);
    }
    let Some(public) = public else {
        return;
    };
    // Wrappers, one per variant, and the public stub.
    let wrappers = levels.into_iter().filter(|_| private);
    for level in wrappers.chain(stubs.then_some(None)) {
        let attrs = FnAttrs {
            linkage: Linkage::Hidden,
            level,
            ..FnAttrs::default()
        };
        foreign.insert(variant(level), (public.clone(), attrs));
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

/// Lower a release unit for x86-64 Linux, whole or, over `bound` Core
/// instructions ([`partition_instructions`]), in [`partitions`]: each partition is a module
/// of its own context that `each` takes as soon as it is built, so one
/// partition's LLVM module is live at a time. Partitions keep every call
/// between them: each defines its bodies hidden and declares the others' it
/// calls. Returns the unit's release sites and debug sites, numbered over the
/// partitions in order.
#[cfg(hirsel_lazy_core)]
pub(crate) fn emit_release_unit(
    module: Module,
    roles: &hirsel_core_ir::roles::Facts,
    observe_charge: bool,
    neutral: &BTreeSet<hirsel_core_ids::ContentHash>,
    bound: usize,
    each: &mut dyn FnMut(crate::construction::NativeModule) -> std::result::Result<(), String>,
) -> Result<(Vec<source::ReleaseSite>, Vec<source::ReleaseSite>)> {
    let versions = versions::program(&[&module]);
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
        release_sites: true,
        versions: Some(&versions),
    };
    let recipes = hirsel_core_layout::compute(&module.types).expect("producer layouts");
    let mut debug = sink::native::ReleaseDebug::new(module.items.clone());
    let mut module = prepare(Cow::Owned(module), &recipes, source, provided);
    let plan = partitions(&module, bound);
    let analyses = Analyses::new(&module, &recipes, source, provided, facts_enabled());
    let mut sites = Some(ReleaseSites::default());
    for partition in 0..plan.as_ref().map_or(1, |(count, _)| *count) {
        let members = plan
            .as_ref()
            .map(|(_, of)| of.iter().map(|&p| p == partition).collect::<Vec<_>>());
        let context = std::rc::Rc::new(
            crate::construction::Context::with_anonymous_locals(true).map_err(Error::Llvm)?,
        );
        let mut native = sink::native::NativeSink::release(context, debug);
        let foreign = emit_prepared(
            &module,
            &recipes,
            &analyses,
            Target::X86_64Linux,
            observe_charge,
            facts_enabled(),
            source,
            provided,
            members.as_deref(),
            &mut sites,
            &mut native,
        )?;
        if members.is_some() {
            close_partition(&module, &recipes, &foreign, &mut native)?;
        }
        debug = native.take_release_debug();
        if let Some(members) = &members {
            release_bodies(module.to_mut(), members);
        }
        each(native.finish().map_err(Error::Llvm)?).map_err(Error::Llvm)?;
    }
    Ok((sites.unwrap().bindings, debug.sites()))
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
    sink: &mut sink::native::NativeSink,
) -> Result<()> {
    for name in sink.unresolved().map_err(Error::Llvm)? {
        if !foreign.contains_key(&name)
            && let Some(i) = name.strip_prefix('s').and_then(|i| i.parse::<usize>().ok())
            && i < module.statics.len()
        {
            define_static(module, recipes, i, sink);
        }
    }
    for name in sink.unresolved().map_err(Error::Llvm)? {
        if let Some((signature, attrs)) = foreign.get(&name) {
            sink.declare(&name, signature, attrs);
        }
    }
    sink.named_metadata("hirsel.partition", &[]);
    Ok(())
}
