//! The release std: plain library instances, and every instance they reach,
//! lowered apart from the program. Every root is an export; a
//! program calls it by its symbol and holds none of its bodies. Every
//! optimised LLVM build takes one, of two kinds:
//! - prebuilt: `std-product` lowers every plain function its image checked
//!   whole, less the held roots, in several modules with
//!   their dependencies. It runs the full optimizer on each and generates every
//!   part's machine code into the image's own store ([`PREBUILT`]), once per
//!   toolchain and target. A build over that image whose units agree with the
//!   side's takes it, and lowers and generates none of it.
//! - reached: any other build lowers the plain roots its own closure reaches
//!   into its side of the content store, keeping in the program the plain roots its
//!   helpers and specialised library instances call, so their call chains
//!   optimise together; scalar floating-point routines are lent instead.
//!
//! A program lowers every other library instance it reaches itself, so its
//! optimiser sees the closure and dictionary arguments it passes them, and
//! the held roots alone: those that take code a caller can bind
//! ([`callable_variants::takes_code`]) and those that make an owned network
//! wait, whose private and returnable variants a program builds.
//!
//! The unit and a program meet at five contracts:
//! - callable: each root is defined under its instance symbol, and one
//!   with a step-free body also under that body's code symbol;
//! - step: each root's [`Summary`] (step-free body, opens a bound,
//!   launches) stands in the program's step specialisation for the body it
//!   calls;
//! - width: neither side resolves a blob's `W` over the other's bodies;
//! - link: every other strong definition of the unit is renamed into its
//!   own namespace (`$std`), so no name the unit defines meets a program's;
//! - site: the unit's site references name source items, whose words the
//!   program's source map assigns over the unit's points and its own.
//!
//! The unit also lends its bodies of at most [`LEND_LIMIT`] instructions,
//! after its own optimiser, to each program's inliner
//! ([`hirsel_llvm_lazy::Lend`]) as `available_externally` definitions: a
//! call site LLVM's inline cost takes inlines the body, and every other call
//! reaches the unit's part, whose machine code is generated apart from the
//! program. A body copied into a program keeps the unit's site references
//! and step charges; each call it keeps reaches the unit's part under the
//! same five contracts.
use super::*;
use super::{recipe_build::Unit, release::LentBodies};
use crate::native::{NativeOptions, NativeStop, ObjectProduct};
use crate::store::remote::{self, Remote};
use hirsel_core_ir::step_contexts::Summary;
use hirsel_core_ir::{Callee, FuncId, Identity, Param, source::NativeObject};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
#[path = "std_release/held.rs"]
mod held;
#[path = "std_release/stored.rs"]
mod stored;
use stored::{
    code_name, lent_name, load_record, part_name, read_compressed, read_prebuilt_object, whole,
    write_compressed,
};

const STD_SCHEMA: u32 = 12;

/// The largest body, in LLVM instructions after the std's optimiser, the std
/// lends a program's inliner. LLVM's inline cost decides each call site; this
/// bounds what each program reads and links.
const LEND_LIMIT: usize = 2000;

/// The source law runner places program modules in the core and std packages.
fn library(definition: &DefPath) -> bool {
    (definition.package_is("std", "1") || definition.package_is("core", "1"))
        && definition
            .0
            .modules
            .first()
            .is_none_or(|name| name != "tests")
}

/// A release std's publication record, stored under its side key.
#[derive(Serialize, Deserialize)]
struct StdRecord {
    schema: u32,
    prebuilt: bool,
    /// The plain roots the side was asked to lower, in identity order.
    requested: Vec<Identity>,
    /// The plain roots the side defines under their instance symbols, in
    /// identity order.
    roots: Vec<Identity>,
    /// Every unit the side lowered, with its owner (its body, annex, builtins
    /// and bound constant values), in identity order. The side's code is a
    /// function of these and of its toolchain and variant alone.
    units: Vec<(Identity, ContentHash)>,
    /// Each root's step summary, by the root's code identity.
    summaries: Vec<(ContentHash, Summary)>,
    /// The source points the unit's site references name, by file.
    points: BTreeMap<String, Vec<(u32, u32)>>,
    /// The symbols the unit exports, which keep their names.
    exports: Vec<BTreeSet<String>>,
    /// The unit's parts, in link order. Each part's bitcode is stored under
    /// [`code_name`]; its object, generated when a program first links the
    /// part ([`StdSide::objects`]), under [`part_name`] in the side's store.
    parts: Vec<StdPart>,
    /// The store key of the bodies the unit lends, as bitcode.
    lent: ContentHash,
}

/// One part of a release std: its definition's symbol (empty for a unit
/// generated whole) and whether its object defines that symbol strong.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct StdPart {
    name: String,
    strong: bool,
    module: usize,
}

/// A separately lowered release std, as a program's assembly takes it.
pub(crate) struct StdSide {
    /// The key the std's record is stored under: its own content's.
    pub key: ContentHash,
    pub roots: BTreeSet<Identity>,
    pub summaries: BTreeMap<ContentHash, Summary>,
    pub points: BTreeMap<String, Vec<(u32, u32)>>,
    /// The std's parts, which [`StdSide::objects`] links.
    pub(crate) parts: StdParts,
    /// The bodies the std lends a program's inliner, as bitcode.
    pub lent: LentBodies,
    /// How many units this build lowered for the side: none when the shared
    /// cache held it.
    pub lowered: u64,
}

/// The units a side lowers, each reached from its roots.
struct SideUnits {
    roots: Vec<Identity>,
    units: BTreeMap<Identity, Unit>,
}

#[derive(Default)]
struct ModuleRoots {
    roots: Vec<Identity>,
    reached: BTreeSet<Identity>,
}

/// The plain library roots assigned to the side and the units they reach.
fn program_side(
    units: &BTreeMap<Identity, Unit>,
    entry: &Identity,
) -> (Vec<Identity>, BTreeSet<Identity>) {
    let reach = |from: Vec<Identity>| {
        let mut seen = BTreeSet::new();
        let mut work = from;
        while let Some(identity) = work.pop() {
            if !library(&identity.definition) {
                continue;
            }
            let Some(unit) = units.get(&identity) else {
                continue;
            };
            if !seen.insert(identity) {
                continue;
            }
            for function in unit.module().functions.iter().filter(|f| f.body.is_none()) {
                let callee = unit.module().identity(function.instance);
                if units.contains_key(&callee) && !seen.contains(&callee) {
                    work.push(callee);
                }
            }
        }
        seen
    };
    let plain = |identity: &Identity| {
        identity != entry
            && library(&identity.definition)
            && identity.closures.is_empty()
            && identity.dictionaries.is_empty()
    };
    let mut local_roots = Vec::new();
    for (identity, unit) in units {
        if plain(identity) {
            continue;
        }
        // Specialised library instances already lower in the program unit.
        // Keep their plain callees there too, as for program helpers and
        // closures. The entry can still call the side directly.
        let first_helper = usize::from(identity == entry);
        for function in &unit.module().functions[first_helper..] {
            let Some(body) = &function.body else { continue };
            for call in &body.calls {
                if let Callee::Direct(callee) = call.callee {
                    let function = &unit.module().functions[callee.index()];
                    let signature = &unit.module().signatures[function.signature.index()];
                    // These calls pass only floating-point values: the side
                    // can optimise the routine before the caller inlines it.
                    if signature.dest.is_none()
                        && matches!(signature.results.as_slice(), [ty] if ty.is_float())
                        && signature
                            .params
                            .iter()
                            .all(|param| matches!(param, Param::Value(ty) if ty.is_float()))
                    {
                        continue;
                    }
                    let callee = unit.module().identity(function.instance);
                    if plain(&callee) {
                        local_roots.push(callee);
                    }
                }
            }
        }
    }
    let held = reach(local_roots);
    let roots: Vec<Identity> = units
        .iter()
        .filter(|(i, u)| plain(i) && !held.contains(*i) && !held::root(u.module(), FuncId(0)))
        .map(|(i, _)| i.clone())
        .collect();
    let reached = reach(roots.clone());
    (roots, reached)
}

/// The suffix of the std's own hidden names.
const STD_NAMESPACE: &str = hirsel_llvm_lazy::LENT_NAMESPACE;

fn namespace(module: usize) -> Cow<'static, str> {
    if module == 0 {
        Cow::Borrowed(STD_NAMESPACE)
    } else {
        Cow::Owned(format!("$part{module}{STD_NAMESPACE}"))
    }
}

/// The release std a program's publication assembles against under
/// `options`' generator: the side `library` (a library image's store)
/// prebuilt for this generator when each unit it holds that the program also
/// holds is the program's own (equal owner); else the selected plain library
/// roots with every unit they reach, lowered once into the shared parts
/// cache. A program that selects no roots takes the empty side.
pub(crate) fn std_side(
    units: &BTreeMap<Identity, Unit>,
    entry: &Identity,
    library: Option<&Path>,
    options: &NativeOptions,
) -> Result<StdSide, NativeStop> {
    let generator = Generator::of(options);
    let kin = generator.kin()?;
    let empty = |key| StdSide {
        key,
        roots: BTreeSet::new(),
        summaries: BTreeMap::new(),
        points: BTreeMap::new(),
        parts: StdParts::default(),
        lent: LentBodies::default(),
        lowered: 0,
    };
    // A coverage build of a library package flags its bodies in the
    // program's own module: a side lends its bodies, so it is cut per
    // definition and holds no records, and its key omits the switch.
    let covered = options.coverage.as_ref().is_some_and(|scope| {
        units
            .keys()
            .any(|i| self::library(&i.definition) && scope.contains(&i.definition.0.package))
    });
    if covered {
        return Ok(empty(side_key(kin, false, &[], &[])?));
    }
    if let Some(side) = prebuilt_side(units, library, kin)? {
        return Ok(side);
    }
    let (roots, reached) = program_side(units, entry);
    if roots.is_empty() {
        return Ok(empty(side_key(kin, false, &[], &[])?));
    }
    let side_units = SideUnits {
        roots,
        units: reached
            .into_iter()
            .map(|identity| {
                let unit = units[&identity].clone();
                (identity, unit)
            })
            .collect(),
    };
    let (shared, remote) = (Store::cas(&options.cas)?, remote::for_build(options));
    let (store, key, record, lowered) =
        lower(&generator, kin, side_units, &shared, Vec::new(), remote)?;
    side(&store, key, record, lowered)
}

/// The side `library`'s store holds prebuilt for `kin`, when each unit it
/// holds that `units` also holds has the same owner.
fn prebuilt_side(
    units: &BTreeMap<Identity, Unit>,
    library: Option<&Path>,
    kin: ContentHash,
) -> Result<Option<StdSide>, NativeStop> {
    let Some(path) = library
        .map(|root| root.join(PREBUILT))
        .filter(|path| path.is_dir())
    else {
        return Ok(None);
    };
    let store = Store::cas_artifact(&path)?;
    let Some(key) = store
        .get(prebuilt_name(kin)?)?
        .and_then(|bytes| decode::<ContentHash>(&bytes))
    else {
        return Ok(None);
    };
    let Some(record) = load_record(&store, key, kin)? else {
        return Ok(None);
    };
    for (identity, owner) in &record.units {
        if let Some(unit) = units.get(identity)
            && unit.owner()? != *owner
        {
            return Ok(None);
        }
    }
    side(&store, key, record, 0).map(Some)
}

/// Where `std-product` publishes its image's prebuilt release std, inside
/// the image's store.
pub(crate) const PREBUILT: &str = "release-std";

/// The name the key of the side prebuilt for `kin` is stored under.
fn prebuilt_name(kin: ContentHash) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-prebuilt.v1", &kin)
}

impl CheckProduct {
    /// The library image's plain roots: a plain identity of each non-generic
    /// function the image checked whole, in identity order.
    fn std_plain_roots(&self) -> Result<Vec<Identity>, NativeStop> {
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("the release std is an image's".into()))?;
        let directory = image.directory().map_err(NativeStop::Input)?;
        let mut roots = Vec::new();
        for definition in directory.definitions.keys() {
            let product = self.demand_body(definition).map_err(NativeStop::Input)?;
            if product.check.function.is_none() || product.check.template.is_some() {
                continue;
            }
            roots.push(Identity::plain(definition.clone()));
        }
        roots.sort();
        Ok(roots)
    }

    /// The units of the prebuilt release std of this product's library
    /// image: every plain root the image checked whole and what it reaches,
    /// less each held root, prepared as a recipe names them. Reads the library
    /// image only.
    fn image_side(
        &self,
        target: hirsel_core_layout::Target,
    ) -> Result<(SideUnits, Vec<ModuleRoots>), NativeStop> {
        let plain = self.std_plain_roots()?;
        // Every reached instance stays materialised until partitioning, so
        // each keeps a compact recipe, decoded only while it is used.
        let mut materialized = crate::instances::InstanceSet::compact().for_target(Some(target));
        // A root whose walk stops is left out alone; a program reaching it
        // lowers it itself and stops there.
        let mut walks = Vec::new();
        for root in plain {
            if let Ok(walked) =
                self.instances_into([root.clone()], SIDE_INSTANCE_LIMIT, &mut materialized)
            {
                walks.push((root, walked));
            }
        }
        let mut held = BTreeSet::new();
        for (root, _) in &walks {
            let instance = materialized.get(root);
            if held::root(&*instance.transient_ir()?, instance.function) {
                held.insert(root.clone());
            }
        }
        let mut reached = BTreeSet::new();
        let mut roots = Vec::new();
        let mut groups = BTreeMap::<_, ModuleRoots>::new();
        for (root, walked) in walks {
            if !held.contains(&root) {
                let path = &root.definition.0;
                let group = groups
                    .entry((path.package.clone(), path.modules.clone()))
                    .or_default();
                group.roots.push(root.clone());
                group.reached.extend(walked.iter().cloned());
                reached.extend(walked);
                roots.push(root);
            }
        }
        let items = reached
            .iter()
            .map(|i| materialized.get(i).anchored_items())
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let sources = self.native_sources(&items)?;
        let sources = sources.positions();
        let mut units = BTreeMap::new();
        for identity in reached {
            let instance = materialized.get(&identity);
            let unit = super::recipe_build::prepare_std_unit(instance, &sources)?;
            units.insert(identity, unit);
        }
        let weights: BTreeMap<_, usize> = units
            .iter()
            .map(|(identity, unit)| {
                let size = unit
                    .module()
                    .functions
                    .iter()
                    .filter_map(|f| f.body.as_ref())
                    .map(|body| body.insts.len())
                    .sum();
                (identity, size)
            })
            .collect();
        // Keep siblings together for interprocedural optimization. Place the
        // largest groups first and count each shared dependency only once
        // toward the module holding it.
        let mut groups: Vec<_> = groups.into_values().collect();
        groups.sort_by_cached_key(|group| {
            std::cmp::Reverse(
                group
                    .reached
                    .iter()
                    .map(|identity| weights[identity])
                    .sum::<usize>(),
            )
        });
        let mut modules: Vec<ModuleRoots> = (0..4).map(|_| ModuleRoots::default()).collect();
        for group in groups {
            let module = modules
                .iter_mut()
                .min_by_key(|module| {
                    module
                        .reached
                        .union(&group.reached)
                        .map(|identity| weights[identity])
                        .sum::<usize>()
                })
                .expect("the prebuild partitions its roots");
            module.roots.extend(group.roots);
            module.reached.extend(group.reached);
        }
        modules.retain(|module| !module.roots.is_empty());
        for module in &mut modules {
            module.roots.sort();
        }
        Ok((SideUnits { roots, units }, modules))
    }

    /// The prebuilt release std of this product's library image for release
    /// builds on `target`, walked and prepared from the image.
    /// [`PrebuiltStd::lower`] publishes it, with every part's machine code,
    /// into the image's own store. The side holds its own units, so the
    /// caller can drop this product, and the bodies its image decoded for the
    /// walk, before the side's optimiser runs.
    pub(crate) fn prebuilt_std(&self, target: &str) -> Result<PrebuiltStd, NativeStop> {
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("the release std is an image's".into()))?;
        let build = hirsel_core_layout::Target::from_triple(target)
            .ok_or_else(|| NativeStop::Entry(format!("`target` names no target {target}")))?;
        let (units, modules) = self.image_side(build)?;
        Ok(PrebuiltStd {
            target: target.to_owned(),
            store: Store::cas_artifact(&image.store_root().join(PREBUILT))?,
            units,
            modules,
        })
    }
}

/// A library image's release std, prepared to lower and generate for one
/// target.
pub(crate) struct PrebuiltStd {
    target: String,
    store: Store,
    units: SideUnits,
    modules: Vec<ModuleRoots>,
}

impl PrebuiltStd {
    /// Lower the side, generate every part's object, and publish them.
    pub(crate) fn lower(self) -> Result<(), NativeStop> {
        // The shipping release builds, which observe no logical charges.
        let generator = Generator {
            target: &self.target,
            observe_charge: false,
            recording: false,
        };
        let kin = generator.kin()?;
        let (_, key, _, _) = lower(&generator, kin, self.units, &self.store, self.modules, None)?;
        self.store.put_keyed(prebuilt_name(kin)?, &encode(&key)?)?;
        Ok(())
    }
}

/// Lower the side into its pinned cache directory, or the installation's
/// store for a prebuilt side. A hit lowers no units.
fn lower(
    options: &Generator<'_>,
    kin: ContentHash,
    side: SideUnits,
    cache: &Store,
    modules: Vec<ModuleRoots>,
    remote: Option<std::sync::Arc<Remote>>,
) -> Result<(Store, ContentHash, StdRecord, u64), NativeStop> {
    let SideUnits {
        roots: requested,
        units,
    } = side;
    let prebuilt = !modules.is_empty();
    let owners = units
        .iter()
        .map(|(identity, unit)| Ok((identity.clone(), unit.owner()?)))
        .collect::<io::Result<Vec<_>>>()?;
    let key = side_key(kin, prebuilt, &requested, &owners)?;
    let store = if prebuilt {
        cache.clone()
    } else {
        cache.side(key)?
    };
    if let Some(record) = load_record(&store, key, kin)?
        && whole(&store, key, &record)?
    {
        remote.iter().for_each(|r| r.offer_side(&store, key));
        return Ok((store, key, record, 0));
    }
    let received = remote.as_deref().map(|r| r.std_side(cache, key));
    if let Some(store) = received.transpose()?.flatten()
        && let Some(record) = load_record(&store, key, kin)?
        && whole(&store, key, &record)?
    {
        return Ok((store, key, record, 0));
    }
    let lent_key = lent_name(key)?;
    let mut record = StdRecord {
        schema: STD_SCHEMA,
        prebuilt,
        requested,
        roots: Vec::new(),
        units: owners,
        summaries: Vec::new(),
        points: BTreeMap::new(),
        exports: Vec::new(),
        parts: Vec::new(),
        lent: lent_key,
    };
    let mut lent = Vec::new();
    if prebuilt {
        for module in modules {
            let units = module
                .reached
                .into_iter()
                .map(|identity| {
                    let unit = units[&identity].clone();
                    (identity, unit)
                })
                .collect();
            lent.push(lower_module(
                options,
                module.roots,
                units,
                &store,
                key,
                &mut record,
            )?);
        }
    } else {
        lent.push(lower_module(
            options,
            record.requested.clone(),
            units,
            &store,
            key,
            &mut record,
        )?);
    }
    let lent = if lent.len() == 1 {
        lent.pop().expect("one module")
    } else {
        hirsel_llvm_lazy::join_lent(&lent).map_err(|message| NativeStop::Emit {
            definition: record.requested[0].definition.clone(),
            message,
        })?
    };
    record.roots.sort();
    record.summaries.sort_by_key(|(code, _)| *code);
    for points in record.points.values_mut() {
        points.sort_unstable();
        points.dedup();
    }
    if prebuilt {
        write_compressed(&store, lent_key, &lent)?;
    } else {
        store.put_keyed_blob(lent_key, &lent)?;
    }
    store.put_keyed(key, &encode(&record)?)?;
    remote.iter().for_each(|r| r.offer_side(&store, key));
    let lowered = record.units.len() as u64;
    Ok((store, key, record, lowered))
}

fn lower_module(
    options: &Generator<'_>,
    roots: Vec<Identity>,
    mut units: BTreeMap<Identity, Unit>,
    store: &Store,
    key: ContentHash,
    record: &mut StdRecord,
) -> Result<Vec<u8>, NativeStop> {
    let reached: Vec<Identity> = units.keys().cloned().collect();
    let bindings = super::recipe_build::bind_annexes(
        &mut units,
        &BTreeMap::new(),
        &BTreeMap::new(),
        false,
        None,
    )?;
    let points = std::sync::Arc::unwrap_or_clone(bindings.sources).points;
    let mut modules: Vec<_> = units
        .values()
        .map(|unit| Cow::Borrowed(unit.module()))
        .collect();
    // Only the roots are exports: a program lowers every other instance it
    // reaches, so its optimiser sees the arguments it passes them.
    let body_keys = crate::callable_variants::prepare_inputs(
        &mut modules,
        &reached,
        crate::callable_variants::Roots::All(&roots),
        SIDE_INSTANCE_LIMIT,
        |identity| {
            let owner = units[identity].owner()?;
            Ok((
                FuncId(0),
                owner,
                !identity.closures.is_empty() || !identity.dictionaries.is_empty(),
            ))
        },
    )?;
    let first = roots
        .first()
        .cloned()
        .ok_or_else(|| NativeStop::Entry("the release std has no root".into()))?;
    let (mut module, _) = release::join_modules(&first, &reached, modules, |_| FuncId(0))?;
    module.law_observation = options.recording;
    // The joined module holds every body LLVM lowers; the per-unit modules
    // are not read again, so they leave before the optimiser runs.
    drop(units);
    // Each root's function: the row holding its own body.
    let mut functions: BTreeMap<Identity, FuncId> = BTreeMap::new();
    let wanted: BTreeSet<&Identity> = roots.iter().collect();
    for (f, function) in module.functions.iter().enumerate() {
        let id = FuncId(f as u32);
        if function.body.is_none() || module.code_variants.contains_key(&id) {
            continue;
        }
        let identity = module.identity(function.instance);
        if wanted.contains(&identity) {
            functions.entry(identity).or_insert(id);
        }
    }
    drop(wanted);
    let roots: Vec<Identity> = roots
        .into_iter()
        .filter(|unit| functions.contains_key(unit))
        .collect();
    let ids: Vec<FuncId> = roots.iter().map(|root| functions[root]).collect();
    let codes: Vec<ContentHash> = ids.iter().map(|&f| module.code_identity(f)).collect();
    let summaries = hirsel_core_ir::step_contexts::specialize_library(&mut module, &ids);
    held::descriptors(&mut module);
    let mut exports = Vec::with_capacity(roots.len() * 2);
    for ((root, code), summary) in roots.iter().zip(&codes).zip(&summaries) {
        exports.push(root.symbol());
        if summary.free {
            exports.push(hirsel_core_ir::symbol(
                hirsel_core_ir::step_contexts::free_identity(*code),
            ));
        }
    }
    let target =
        hirsel_core_layout::Target::from_triple(options.target).expect("supported native target");
    let facts = hirsel_core_ir::roles::Facts::program(&[&module], &[]);
    let emit = |message: String| NativeStop::Emit {
        definition: first.definition.clone(),
        message,
    };
    // The LLVM modules read nothing of the Core module or its facts, which
    // leave before the optimiser and the code generator run.
    let lowered = hirsel_llvm_lazy::lower_program(
        &[hirsel_llvm_lazy::Unit::Core {
            module: &module,
            facts: &facts[0],
            observe_charge: options.observe_charge,
            debug_lines: true,
        }],
        target,
    )
    .map_err(emit)?;
    drop(facts);
    drop(module);
    let index = record.exports.len();
    let program = lowered
        .emit(
            &exports,
            hirsel_guard::jobs() as u32,
            &mut hirsel_llvm_lazy::Unheld,
            hirsel_llvm_lazy::Lend::Lend {
                limit: LEND_LIMIT,
                namespace: &namespace(index),
            },
        )
        .map_err(emit)?;
    if program.deferred.is_empty() {
        return Err(NativeStop::Entry(
            "the release std module has no part".into(),
        ));
    }
    if body_keys.is_empty() {
        return Err(NativeStop::Entry(
            "the release std module holds no body".into(),
        ));
    }
    let offset = record.parts.len();
    record.exports.push(exports.into_iter().collect());
    record
        .parts
        .extend(program.deferred.iter().map(|part| StdPart {
            name: part.name.clone(),
            strong: part.strong,
            module: index,
        }));
    if record.prebuilt {
        let private_names = private_names(&record.parts, &record.exports);
        // Generated in batches, so the objects in hand stay few.
        for (batch, codes) in program.deferred.chunks(256).enumerate() {
            let codes: Vec<&[u8]> = codes.iter().map(|part| part.code.as_slice()).collect();
            let generated =
                hirsel_llvm_lazy::generate_parts(&codes, target, hirsel_guard::jobs() as u32)
                    .map_err(emit)?;
            for (j, object) in generated.iter().enumerate() {
                let i = offset + batch * 256 + j;
                let part_key = part_name(key, i)?;
                let object = finish(
                    crate::native::debug_alias(codes[j]),
                    &record.parts[i],
                    object,
                    &record.exports[index],
                    &private_names[index],
                )
                .map_err(emit)?;
                write_compressed(store, part_key, &encode(&object)?)?;
            }
        }
    } else {
        for (i, part) in program.deferred.iter().enumerate() {
            store.put_keyed_blob(code_name(key, i)?, &part.code)?;
        }
    }
    record.roots.extend(roots);
    record.summaries.extend(codes.into_iter().zip(summaries));
    for (file, points) in points {
        record.points.entry(file).or_default().extend(points);
    }
    Ok(program.lent)
}

/// What a release std's lowering reads besides its units: the toolchain and
/// variant its key names. A side is lowered under `SIDE_INSTANCE_LIMIT`,
/// whatever a program's own limit.
struct Generator<'a> {
    target: &'a str,
    observe_charge: bool,
    recording: bool,
}

/// The instance limit every release std is walked and lowered under: the
/// side is a function of the std's content, never of the program it serves.
const SIDE_INSTANCE_LIMIT: usize = crate::native::DEFAULT_INSTANCE_LIMIT;

impl<'a> Generator<'a> {
    fn of(options: &'a NativeOptions) -> Self {
        Self {
            target: &options.target,
            observe_charge: options.logical_charge_report.is_some(),
            recording: options.recording,
        }
    }

    /// The toolchain and variant a side's code is a function of besides its
    /// units: every side of one kin is lowered by the same generator. The
    /// runtime a program links is not one: the side's code reads none of it.
    fn kin(&self) -> io::Result<ContentHash> {
        let compiler = crate::reuse::compiler_identity();
        let llvm = crate::tool_inputs::llvm_library()?;
        crate::native::key(
            "hirsel.core-std-kin.v1",
            &(
                STD_SCHEMA,
                compiler,
                llvm,
                self.target,
                self.observe_charge,
                self.recording,
                !hirsel_llvm_lazy::core::facts_enabled(),
            ),
        )
    }
}

/// A side's key: its kin's, its roots', and its own units' content.
fn side_key(
    kin: ContentHash,
    prebuilt: bool,
    roots: &[Identity],
    units: &[(Identity, ContentHash)],
) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-side.v3", &(kin, prebuilt, roots, units))
}

/// Load the side's lent bodies and hold the store through object generation.
fn side(
    store: &Store,
    key: ContentHash,
    record: StdRecord,
    lowered: u64,
) -> Result<StdSide, NativeStop> {
    let instance = record
        .roots
        .first()
        .cloned()
        .ok_or_else(|| NativeStop::Entry("the release std has no root".into()))?;
    let lent = if record.prebuilt {
        read_compressed(store, record.lent)?
    } else {
        store.get_keyed_blob(record.lent)?
    }
    .map(|(_, lent)| lent)
    .ok_or_else(|| NativeStop::Entry("the release std's lent bodies are missing".into()))?;
    Ok(StdSide {
        key,
        lent: LentBodies::new(lent),
        lowered,
        roots: record.roots.into_iter().collect(),
        summaries: record.summaries.into_iter().collect(),
        points: record.points,
        parts: StdParts {
            key,
            prebuilt: record.prebuilt,
            instance: Some(instance),
            store: Some(store.clone()),
            exports: record.exports,
            parts: record.parts,
        },
    })
}

/// A release std's parts, as [`StdSide::objects`] links them.
#[derive(Default)]
pub(crate) struct StdParts {
    /// The std's key, which each part's names derive from.
    key: ContentHash,
    prebuilt: bool,
    /// The root each part's object is recorded under.
    instance: Option<Identity>,
    /// The shared cache holding each part's bitcode, and its object once
    /// generated.
    store: Option<Store>,
    exports: Vec<BTreeSet<String>>,
    parts: Vec<StdPart>,
}

fn private_names<'a>(
    parts: &'a [StdPart],
    exports: &[BTreeSet<String>],
) -> Vec<BTreeSet<&'a [u8]>> {
    let mut names = vec![BTreeSet::new(); exports.len()];
    for part in parts {
        if part.strong && !exports[part.module].contains(&part.name) {
            names[part.module].insert(part.name.as_bytes());
        }
    }
    names
}

/// Cut the object, detach its debug rows and put its private definitions and
/// references under its module's namespace. A named part defines only its
/// own symbol, when its record marks it strong, and its debug symbol. `alias`
/// keys the part's ELF debug alias: content the libLLVM file never enters,
/// unlike the cache key the part's object is stored under.
fn finish(
    alias: ContentHash,
    part: &StdPart,
    object: &[u8],
    exports: &BTreeSet<String>,
    private_names: &BTreeSet<&[u8]>,
) -> Result<hirsel_core_ir::source::NativeObject, String> {
    let mut cut =
        hirsel_llvm_lazy::core::split_native_object(object, &[(&part.name, alias)], true, None)?;
    let mut object = cut.pop().ok_or("a release std part cuts to no object")?;
    let strong = hirsel_llvm_lazy::core::strong_definitions(&object.bytes)?;
    let own: BTreeSet<Vec<u8>> = strong
        .into_iter()
        .filter(|name| !std::str::from_utf8(name).is_ok_and(|name| exports.contains(name)))
        .collect();
    if !part.name.is_empty() {
        // Rows name `debug_symbol` on ELF, the function itself on Mach-O.
        let rows = object.debug.iter().flat_map(|d| &d.sections);
        let named = (part.strong && !exports.contains(&part.name)).then_some(&part.name);
        let names = rows.map(|s| &s.symbol).filter(|&s| *s != part.name);
        let expected: BTreeSet<_> = names.chain(named).map(|s| s.as_bytes().to_vec()).collect();
        if own != expected {
            return Err(format!(
                "release std part {} defines other strong symbols than its record names",
                part.name
            ));
        }
    }
    let namespace = namespace(part.module);
    let is_renamed = |name: &[u8]| private_names.contains(name) || own.contains(name);
    let rename = |name: &[u8]| {
        is_renamed(name).then(|| {
            let mut renamed = name.to_vec();
            renamed.extend_from_slice(namespace.as_bytes());
            renamed
        })
    };
    object.bytes = hirsel_llvm_lazy::core::rewrite_std_symbols(&object.bytes, &rename)?;
    if let Some(debug) = &mut object.debug {
        for section in &mut debug.sections {
            if is_renamed(section.symbol.as_bytes()) {
                section.symbol.push_str(&namespace);
            }
        }
    }
    Ok(object)
}

impl StdSide {
    /// The std's objects a link of `program` takes, in part order: each part
    /// defining a symbol `program`'s objects name and do not define, and
    /// each part defining one those parts name, in turn. A unit generated
    /// whole is its one part. The shared cache holds each part's object, or
    /// gets it, generated from the part's bitcode alone, as its program
    /// would have generated it, under the link contract.
    pub(crate) fn objects(
        &self,
        program: &[ObjectProduct],
        options: &NativeOptions,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        let parts = &self.parts;
        let (Some(instance), Some(store)) = (&parts.instance, &parts.store) else {
            return Ok(Vec::new());
        };
        let emit = |message: String| NativeStop::Emit {
            definition: instance.definition.clone(),
            message,
        };
        let by_name: BTreeMap<Vec<u8>, usize> = parts
            .parts
            .iter()
            .enumerate()
            .filter(|(_, part)| !part.name.is_empty())
            .map(|(i, part)| {
                let mut name = part.name.clone().into_bytes();
                if part.strong && !parts.exports[part.module].contains(&part.name) {
                    name.extend_from_slice(namespace(part.module).as_bytes());
                }
                (name, i)
            })
            .collect();
        let mut taken = vec![false; parts.parts.len()];
        // The parts that define what `names` name, not taken before.
        let mut reach = |names: Vec<Vec<u8>>| -> Result<Vec<usize>, NativeStop> {
            let mut next = Vec::new();
            for name in names {
                match by_name.get(&name) {
                    Some(&i) => {
                        if !std::mem::replace(&mut taken[i], true) {
                            next.push(i);
                        }
                    }
                    // A name in the std's namespace is a part's.
                    None if name.ends_with(STD_NAMESPACE.as_bytes()) => {
                        return Err(emit(format!(
                            "no part of the release std defines {}",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                    None => {}
                }
            }
            Ok(next)
        };
        let mut next = if by_name.is_empty() {
            // The target emits one whole object per module.
            (0..parts.parts.len()).collect()
        } else {
            let mut named = Vec::new();
            for object in program {
                let bytes = object.bytes.as_deref().expect("a held release object");
                named.extend(hirsel_llvm_lazy::core::undefined_symbols(bytes).map_err(emit)?);
            }
            reach(named)?
        };
        let mut linked = BTreeMap::new();
        let mut private_names = None;
        while !next.is_empty() {
            let mut named = Vec::new();
            let mut missed = Vec::new();
            for i in next {
                let key = part_name(self.key, i)?;
                let object = if parts.prebuilt {
                    read_prebuilt_object(store, key)?
                } else {
                    crate::native_object::load(store, key)?
                };
                match object {
                    Some((bare, object)) => {
                        named.extend(
                            hirsel_llvm_lazy::core::undefined_symbols(&object.bytes)
                                .map_err(emit)?,
                        );
                        linked.insert(i, (bare, object));
                    }
                    None => missed.push(i),
                }
            }
            if !missed.is_empty() {
                let bitcode = missed
                    .iter()
                    .map(|&i| {
                        let code = store.get_keyed_blob(code_name(self.key, i)?)?;
                        code.map(|(_, code)| code).ok_or_else(|| {
                            NativeStop::Entry("a release std part's bitcode is missing".into())
                        })
                    })
                    .collect::<Result<Vec<_>, NativeStop>>()?;
                let target = hirsel_core_layout::Target::from_triple(&options.target)
                    .expect("supported native target");
                let generated = hirsel_llvm_lazy::generate_parts(
                    &bitcode.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                    target,
                    hirsel_guard::jobs() as u32,
                )
                .map_err(emit)?;
                let private_names = private_names
                    .get_or_insert_with(|| self::private_names(&parts.parts, &parts.exports));
                for ((&i, object), code) in missed.iter().zip(&generated).zip(&bitcode) {
                    let part = &parts.parts[i];
                    let object = finish(
                        crate::native::debug_alias(code),
                        part,
                        object,
                        &parts.exports[part.module],
                        &private_names[part.module],
                    )
                    .map_err(emit)?;
                    named.extend(
                        hirsel_llvm_lazy::core::undefined_symbols(&object.bytes).map_err(emit)?,
                    );
                    let bare =
                        crate::native_object::publish(store, part_name(self.key, i)?, &object)?;
                    linked.insert(i, (bare, object));
                }
            }
            next = if by_name.is_empty() {
                Vec::new()
            } else {
                reach(named)?
            };
        }
        let product = |(i, (bare, object)): (usize, (Bare, NativeObject))| {
            let named = (instance.clone(), part_name(self.key, i)?);
            let debug = object.debug.map(std::sync::Arc::new);
            let held = (bare, object.bytes.into());
            io::Result::Ok(ObjectProduct::shared(named, store, held, debug, false))
        };
        Ok(linked.into_iter().map(product).collect::<io::Result<_>>()?)
    }

    /// Each part's bitcode, in part order.
    #[cfg(test)]
    pub(crate) fn codes(&self) -> Vec<Vec<u8>> {
        let parts = &self.parts;
        let store = parts.store.as_ref().expect("a std with parts");
        (0..parts.parts.len())
            .map(|i| {
                let code = store.get_keyed_blob(code_name(self.key, i).unwrap());
                code.unwrap().expect("a part's bitcode").1
            })
            .collect()
    }
}
