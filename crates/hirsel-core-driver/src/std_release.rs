//! The release std: every plain instance of the library image a program is
//! checked against, and every instance they reach, lowered as one unit from
//! the image alone. Its roots are the image's non-generic functions the image
//! checked whole, read from the image, never from a program's reach, so what
//! the unit holds and how it is optimised is a function of the image and the
//! generator. Every root is an export; a program calls it by its symbol and
//! holds none of its bodies. Every optimised LLVM build takes one: a program
//! lowers every other library instance it reaches itself, so its optimiser
//! sees the closure and dictionary arguments it passes them, and the held
//! roots with what they reach: those [`ALWAYS_IMPORT`] names, those that
//! take code a caller can bind ([`callable_variants::takes_code`]), whose
//! calls through it a program's private bodies make direct, and those that
//! make an owned network wait, whose returnable variants a program builds.
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
//! ([`hirsel_llvm_lazy::Lend`]). A body copied into a program keeps the
//! unit's site references and step charges; each call it keeps reaches the
//! unit's part under the same five contracts.
use super::recipe_build::Unit;
use super::*;
use crate::native::{NativeOptions, NativeStop, ObjectProduct};
use hirsel_core_ir::step_contexts::Summary;
use hirsel_core_ir::{FuncId, Identity};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

const STD_SCHEMA: u32 = 6;

/// How many std sides each toolchain and variant's index names, most recent
/// first. One that falls off is lowered again when a build needs it.
const INDEX_LENGTH: usize = 8;

/// The largest body, in LLVM instructions after the std's optimiser, the std
/// lends a program's inliner. LLVM's inline cost decides each call site; this
/// bounds what each program reads and links.
const LEND_LIMIT: usize = 2000;

/// Definitions whose callers need the program to lower them, with every
/// plain root they reach, to pass Guard B: their hot loops call through
/// std bodies the program's optimizer must see whole.
const ALWAYS_IMPORT: &[&str] = &[
    "std@1/json/parse",
    "std@1/regex/next_captures",
    "std@1/string/next_split",
];

fn always_imported(definition: &DefPath) -> bool {
    let path = definition.to_string();
    ALWAYS_IMPORT.contains(&path.as_str())
}

/// A plain root a program lowers itself, with what it reaches: one
/// [`ALWAYS_IMPORT`] names, one taking code a caller can bind, or one making
/// an owned network wait, whose returnable variant (`returnable.rs`) only a
/// program's unit can build. Both sides decide it from the root's own
/// function alone.
fn held_root(definition: &DefPath, module: &hirsel_core_ir::Module, root: FuncId) -> bool {
    always_imported(definition)
        || crate::callable_variants::takes_code(module, root)
        || hirsel_core_ir::returnable::waits_owned(module, root)
}

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
    /// Whether the side was lowered from a whole library image, so it holds
    /// every plain root a program over that image reaches.
    whole: bool,
    /// The plain roots the side was asked to lower, in identity order.
    requested: Vec<Identity>,
    /// The plain roots the side defines under their instance symbols, in
    /// identity order.
    roots: Vec<Identity>,
    /// Every unit the side lowered, with its owner (its body, annex and
    /// builtins), in identity order. The side's code is a function of these
    /// and of its toolchain and variant alone.
    units: Vec<(Identity, ContentHash)>,
    /// Each root's step summary, by the root's code identity.
    summaries: Vec<(ContentHash, Summary)>,
    /// The source points the unit's site references name, by file.
    points: BTreeMap<String, Vec<(u32, u32)>>,
    /// The symbols the unit exports, which keep their names.
    exports: BTreeSet<String>,
    /// The unit's parts, in link order. Each part's bitcode is stored under
    /// [`code_name`]; its object, generated when a program first links the
    /// part ([`StdSide::objects`]), under [`part_name`] in the shared cache.
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
    pub lent: Vec<u8>,
    /// How many units this build lowered for the side: none when it was
    /// read from a store. A build over a prebuilt std product that lowers
    /// its std pays the whole side, and reports it here.
    pub lowered: u64,
}

/// The units a side lowers, each reached from its roots.
struct SideUnits {
    whole: bool,
    roots: Vec<Identity>,
    units: BTreeMap<Identity, Unit>,
}

/// The plain library roots a program's `units` hold besides its `entry`, and
/// the units they reach.
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
            for function in unit.module.functions.iter().filter(|f| f.body.is_none()) {
                let callee = unit.module.identity(function.instance);
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
    let held = reach(
        units
            .iter()
            .filter(|(i, unit)| plain(i) && held_root(&i.definition, &unit.module, FuncId(0)))
            .map(|(i, _)| i.clone())
            .collect(),
    );
    let roots: Vec<Identity> = units
        .keys()
        .filter(|i| plain(i) && !held.contains(*i))
        .cloned()
        .collect();
    let reached = reach(roots.clone());
    (roots, reached)
}

/// The suffix of the std's own hidden names.
const STD_NAMESPACE: &str = hirsel_llvm_lazy::LENT_NAMESPACE;

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

    /// The units of the release std of this product's library image: every
    /// plain root the image checked whole and what it reaches, prepared as a
    /// recipe names them. Reads the library image only.
    fn image_side(&self) -> Result<SideUnits, NativeStop> {
        let plain = self.std_plain_roots()?;
        let mut materialized = crate::instances::InstanceSet::default();
        // A root whose walk stops is left out alone; a program reaching it
        // lowers it itself and stops there as it does today.
        let mut walks = Vec::new();
        for root in plain {
            if let Ok(walked) =
                self.instances_into([root.clone()], SIDE_INSTANCE_LIMIT, &mut materialized)
            {
                walks.push((root, walked));
            }
        }
        // What a held root reaches stays the program's to lower.
        let mut held = BTreeSet::new();
        for (root, walked) in &walks {
            let instance = materialized.get(root);
            if held_root(&root.definition, instance.ir()?, instance.function) {
                held.extend(walked.iter().cloned());
                held.insert(root.clone());
            }
        }
        let mut reached = BTreeSet::new();
        let mut roots = Vec::new();
        for (root, walked) in walks {
            if !held.contains(&root) {
                reached.extend(walked);
                roots.push(root);
            }
        }
        let items = reached
            .iter()
            .map(|i| {
                materialized
                    .get(i)
                    .ir()
                    .map(hirsel_core_ir::source::anchored_items)
            })
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let sources = self.native_sources(&items)?;
        let sources = sources.positions();
        let mut units = BTreeMap::new();
        for identity in reached {
            let instance = materialized.get(&identity);
            let unit =
                super::recipe_build::prepare_unit(instance.ir()?, instance.function, &sources);
            units.insert(identity, unit);
        }
        Ok(SideUnits {
            whole: true,
            roots,
            units,
        })
    }

    /// Lower and publish the release std of this product's library image for
    /// release builds on `target` for each runtime archive, into the
    /// image's own store, which every build over the image reads first. A
    /// variant with a side already there keeps it.
    pub(crate) fn prebuild_release_std(
        &self,
        target: &str,
        runtimes: &[(ContentHash, bool)],
    ) -> Result<(), NativeStop> {
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("the release std is an image's".into()))?;
        let store = Store::new(&image.store_root().join(PREBUILT))?;
        // Each variant walks the side afresh, so no lowering holds another
        // variant's copy of the std's units through its optimiser.
        for &(runtime, observe_charge) in runtimes {
            let generator = Generator {
                target,
                observe_charge,
                runtime,
            };
            let side = self.image_side()?;
            lower(&generator, generator.kin()?, side, &store)?;
        }
        Ok(())
    }
}

/// The release std a program's publication assembles against under
/// `options`' generator, linking the runtime whose identity is `runtime`. Of
/// the sides stored in `library` (a library image's own store), then in the
/// shared parts cache, most recent first, one each of whose units the program
/// also holds is the program's own (equal owner): a whole library's first;
/// for a program checked without a library image, one lowered for the plain
/// roots it reaches next. Else one lowered from `product`'s library image, or
/// without one from the program's own plain library units, and stored in the
/// shared cache. A program checked without a library image that holds no
/// plain library unit takes the empty side, which defines nothing.
pub(crate) fn std_side(
    units: &BTreeMap<Identity, Unit>,
    entry: &Identity,
    product: Option<&CheckProduct>,
    library: Option<&Path>,
    options: &NativeOptions,
    runtime: ContentHash,
) -> Result<StdSide, NativeStop> {
    let (roots, reached) = program_side(units, entry);
    let generator = Generator::of(options, runtime);
    let kin = generator.kin()?;
    let image = product.filter(|product| product.image.is_some());
    if roots.is_empty() && image.is_none() {
        return Ok(StdSide {
            key: side_key(kin, false, &[], &[])?,
            roots: BTreeSet::new(),
            summaries: BTreeMap::new(),
            points: BTreeMap::new(),
            parts: StdParts::default(),
            lent: Vec::new(),
            lowered: 0,
        });
    }
    let owners = units
        .iter()
        .map(|(identity, unit)| Ok((identity, unit.owner()?)))
        .collect::<io::Result<BTreeMap<_, _>>>()?;
    let own = |record: &StdRecord| {
        record
            .units
            .iter()
            .all(|(identity, owner)| owners.get(identity).is_none_or(|own| own == owner))
    };
    let covers = |record: &StdRecord| {
        roots
            .iter()
            .all(|root| record.requested.binary_search(root).is_ok())
    };
    let shared = Store::new(&options.parts)?;
    let prebuilt = library
        .map(|root| root.join(PREBUILT))
        .or_else(|| {
            image
                .and_then(|p| p.image.as_ref())
                .map(|i| i.store_root().join(PREBUILT))
        })
        .filter(|path| path.is_dir())
        .map(|path| Store::new(&path))
        .transpose()?;
    let mut candidates = Vec::new();
    for store in prebuilt.iter().chain([&shared]) {
        for key in read_index(store, kin)? {
            if let Some(record) = load_record(store, key, kin)?
                && own(&record)
            {
                candidates.push((store, key, record));
            }
        }
    }
    let chosen = match candidates.iter().position(|(_, _, record)| record.whole) {
        Some(i) => Some(i),
        None if image.is_none() => candidates.iter().position(|(_, _, record)| covers(record)),
        None => None,
    };
    if let Some(i) = chosen {
        let (store, key, record) = candidates.swap_remove(i);
        return side(store, &shared, key, record, 0);
    }
    let lowered = match image {
        Some(product) => product.image_side()?,
        None => SideUnits {
            whole: false,
            roots,
            units: reached
                .into_iter()
                .map(|identity| {
                    let unit = units[&identity].clone();
                    (identity, unit)
                })
                .collect(),
        },
    };
    let (key, record, lowered) = lower(&generator, kin, lowered, &shared)?;
    if !own(&record) {
        return Err(NativeStop::Entry(
            "the release std lowered for this program holds other bodies than it".into(),
        ));
    }
    side(&shared, &shared, key, record, lowered)
}

/// Join, specialize and lower a side's units, publish its parts and record
/// into `store` under the key its content derives, and name it first in its
/// toolchain and variant's index there. Also returns how many units it
/// lowered: none when `store` already holds the side.
fn lower(
    options: &Generator<'_>,
    kin: ContentHash,
    side: SideUnits,
    store: &Store,
) -> Result<(ContentHash, StdRecord, u64), NativeStop> {
    let SideUnits {
        whole,
        roots,
        mut units,
    } = side;
    let owners = units
        .iter()
        .map(|(identity, unit)| Ok((identity.clone(), unit.owner()?)))
        .collect::<io::Result<Vec<_>>>()?;
    let requested = roots.clone();
    let key = side_key(kin, whole, &requested, &owners)?;
    if let Some(record) = load_record(store, key, kin)? {
        write_index(store, kin, key)?;
        return Ok((key, record, 0));
    }
    let reached: Vec<Identity> = units.keys().cloned().collect();
    let points =
        super::recipe_build::bind_annexes(&mut units, &BTreeMap::new(), &BTreeMap::new(), false)?
            .0
            .points;
    let mut modules: Vec<_> = units
        .values()
        .map(|unit| Cow::Borrowed(&unit.module))
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
        .ok_or_else(|| NativeStop::Entry("the image has no plain function".into()))?;
    let (mut module, _) = release::join_modules(&first, &reached, &modules, |_| FuncId(0))?;
    // The joined module holds every body LLVM lowers; the per-unit modules
    // are not read again, so they leave before the optimiser runs.
    drop(modules);
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
    let program = lowered
        .emit(
            &exports,
            hirsel_guard::jobs() as u32,
            &mut hirsel_llvm_lazy::Unheld,
            hirsel_llvm_lazy::Lend::Lend { limit: LEND_LIMIT },
        )
        .map_err(emit)?;
    // Each part's object is generated when a program first links it: a
    // program links few of the unit's parts.
    let mut parts = Vec::with_capacity(program.deferred.len());
    for (i, part) in program.deferred.into_iter().enumerate() {
        store.put_keyed(code_name(key, i)?, &part.code)?;
        parts.push(StdPart {
            name: part.name,
            strong: part.strong,
        });
    }
    if parts.is_empty() {
        return Err(NativeStop::Entry("the release std has no part".into()));
    }
    if body_keys.is_empty() {
        return Err(NativeStop::Entry("the release std holds no body".into()));
    }
    let lent = lent_name(key)?;
    store.put_keyed(lent, &program.lent)?;
    let record = StdRecord {
        schema: STD_SCHEMA,
        whole,
        requested,
        roots,
        units: owners,
        summaries: codes.into_iter().zip(summaries).collect(),
        points,
        exports: exports.into_iter().collect(),
        parts,
        lent,
    };
    store.put_keyed(key, &encode(&record)?)?;
    write_index(store, kin, key)?;
    let lowered = record.units.len() as u64;
    Ok((key, record, lowered))
}

/// What a release std's lowering reads besides its units: the toolchain and
/// variant its key names. A side is lowered under `SIDE_INSTANCE_LIMIT`,
/// whatever a program's own limit.
struct Generator<'a> {
    target: &'a str,
    observe_charge: bool,
    runtime: ContentHash,
}

/// The instance limit every release std is walked and lowered under: the
/// side is a function of the std's content, never of the program it serves.
const SIDE_INSTANCE_LIMIT: usize = crate::native::DEFAULT_INSTANCE_LIMIT;

impl<'a> Generator<'a> {
    fn of(options: &'a NativeOptions, runtime: ContentHash) -> Self {
        Self {
            target: &options.target,
            observe_charge: options.logical_charge_report.is_some(),
            runtime,
        }
    }

    /// The toolchain and variant a side's code is a function of besides its
    /// units: every side of one kin is lowered by the same generator.
    fn kin(&self) -> io::Result<ContentHash> {
        let compiler = crate::reuse::compiler_identity()?;
        let llvm = crate::tool_inputs::llvm_library()?;
        crate::native::key(
            "hirsel.core-std-kin.v1",
            &(
                STD_SCHEMA,
                compiler,
                llvm,
                self.runtime,
                self.target,
                self.observe_charge,
                !hirsel_llvm_lazy::core::facts_enabled(),
            ),
        )
    }
}

/// A side's key: its kin's, and its own units' content.
fn side_key(
    kin: ContentHash,
    whole: bool,
    roots: &[Identity],
    units: &[(Identity, ContentHash)],
) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-side.v1", &(kin, whole, roots, units))
}

/// The keys of the sides of `kin` stored in `store`, most recent first.
fn read_index(store: &Store, kin: ContentHash) -> io::Result<Vec<ContentHash>> {
    let index = crate::native::key("hirsel.core-std-index.v1", &kin)?;
    Ok(store
        .get(index)?
        .and_then(|bytes| decode::<Vec<ContentHash>>(&bytes))
        .unwrap_or_default())
}

/// Name the side `key` first in `kin`'s index in `store`. Two builds that
/// write at once each keep their own; the side the other named is lowered
/// again when a build needs it.
fn write_index(store: &Store, kin: ContentHash, key: ContentHash) -> io::Result<()> {
    let mut keys = read_index(store, kin)?;
    if keys.first() == Some(&key) {
        return Ok(());
    }
    keys.retain(|k| *k != key);
    keys.insert(0, key);
    keys.truncate(INDEX_LENGTH);
    let index = crate::native::key("hirsel.core-std-index.v1", &kin)?;
    store.put_keyed(index, &encode(&keys)?)
}

/// Where the std product's prebuild publishes, inside the image's store.
pub(crate) const PREBUILT: &str = "release-std";

/// The std side `record`, stored in `store` under `key`, names: its lent
/// bodies, checked under the name its own key derives, and its parts, whose
/// objects `objects` (the shared cache) holds or gets. This build lowered
/// `lowered` of its units.
fn side(
    store: &Store,
    objects: &Store,
    key: ContentHash,
    record: StdRecord,
    lowered: u64,
) -> Result<StdSide, NativeStop> {
    let instance = record
        .roots
        .first()
        .cloned()
        .ok_or_else(|| NativeStop::Entry("the image has no plain function".into()))?;
    let lent = store
        .get(record.lent)?
        .ok_or_else(|| NativeStop::Entry("the release std's lent bodies are missing".into()))?;
    Ok(StdSide {
        key,
        lent,
        lowered,
        roots: record.roots.into_iter().collect(),
        summaries: record.summaries.into_iter().collect(),
        points: record.points,
        parts: StdParts {
            key,
            instance: Some(instance),
            codes: Some(store.clone()),
            objects: Some(objects.clone()),
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
    /// The root each part's object is recorded under.
    instance: Option<Identity>,
    /// The store holding each part's bitcode.
    codes: Option<Store>,
    /// The shared cache holding each part's object once generated.
    objects: Option<Store>,
    exports: BTreeSet<String>,
    parts: Vec<StdPart>,
}

impl StdParts {
    /// The name a part's symbol has in a program's link: a strong
    /// definition the unit does not export is in the std's namespace.
    fn linked(&self, part: &StdPart) -> Vec<u8> {
        let mut name = part.name.clone().into_bytes();
        if part.strong && !self.exports.contains(&part.name) {
            name.extend_from_slice(STD_NAMESPACE.as_bytes());
        }
        name
    }
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
        let (Some(instance), Some(codes), Some(objects)) =
            (&parts.instance, &parts.codes, &parts.objects)
        else {
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
            .map(|(i, part)| (parts.linked(part), i))
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
        let mut next = if let [part] = parts.parts.as_slice()
            && part.name.is_empty()
        {
            // A unit generated whole: its one object.
            vec![0]
        } else {
            let mut named = Vec::new();
            for object in program {
                let bytes = std::fs::read(&object.path)?;
                named.extend(hirsel_llvm_lazy::core::undefined_symbols(&bytes).map_err(emit)?);
            }
            reach(named)?
        };
        let mut linked = BTreeMap::new();
        while !next.is_empty() {
            let mut named = Vec::new();
            let mut missed = Vec::new();
            for i in next {
                match crate::native_object::load(objects, part_name(self.key, i)?)? {
                    Some((bare, object, file)) => {
                        named.extend(
                            hirsel_llvm_lazy::core::undefined_symbols(&object.bytes)
                                .map_err(emit)?,
                        );
                        linked.insert(i, (bare, object.debug, file));
                    }
                    None => missed.push(i),
                }
            }
            if !missed.is_empty() {
                let bitcode = missed
                    .iter()
                    .map(|&i| {
                        codes.get(code_name(self.key, i)?)?.ok_or_else(|| {
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
                for (&i, object) in missed.iter().zip(&generated) {
                    let object = self.finish(i, object).map_err(emit)?;
                    named.extend(
                        hirsel_llvm_lazy::core::undefined_symbols(&object.bytes).map_err(emit)?,
                    );
                    let (bare, file) =
                        crate::native_object::publish(objects, part_name(self.key, i)?, &object)?;
                    linked.insert(i, (bare, object.debug, file));
                }
            }
            next = reach(named)?;
        }
        Ok(linked
            .into_values()
            .map(|(bare, debug, file)| ObjectProduct {
                private_variants: 0,
                instance: instance.clone(),
                path: crate::native_object::path(objects, bare.address),
                identity: bare.address,
                check: bare.check,
                held: Some(file),
                debug,
                debug_record: None,
                compiled: false,
            })
            .collect())
    }

    /// Part `i`'s object as a program links it, from the object generated
    /// from its bitcode: cut, with its debug rows, and its names under the
    /// link contract. The part defines strong only its own symbol, when its
    /// record says so, and its debug symbol.
    fn finish(
        &self,
        i: usize,
        object: &[u8],
    ) -> Result<hirsel_core_ir::source::NativeObject, String> {
        let parts = &self.parts;
        let part = &parts.parts[i];
        let key = part_name(self.key, i).map_err(|e| e.to_string())?;
        let mut cut =
            hirsel_llvm_lazy::core::split_native_object(object, &[(&part.name, key)], true)?;
        let mut object = cut.pop().ok_or("a release std part cuts to no object")?;
        let strong = hirsel_llvm_lazy::core::strong_definitions(&object.bytes)?;
        let own: BTreeSet<Vec<u8>> = strong
            .into_iter()
            .filter(|name| {
                !std::str::from_utf8(name).is_ok_and(|name| parts.exports.contains(name))
            })
            .collect();
        if !part.name.is_empty() {
            let debug = hirsel_core_ir::source::debug_symbol(key, 0).into_bytes();
            let mut expected = BTreeSet::from([debug]);
            if part.strong && !parts.exports.contains(&part.name) {
                expected.insert(part.name.clone().into_bytes());
            }
            if own != expected {
                return Err(format!(
                    "release std part {} defines other strong symbols than its record names",
                    part.name
                ));
            }
        }
        let renamed: BTreeSet<Vec<u8>> = parts
            .parts
            .iter()
            .filter(|part| part.strong && !parts.exports.contains(&part.name))
            .map(|part| part.name.clone().into_bytes())
            .chain(own)
            .collect();
        let rename = |name: &[u8]| {
            renamed.contains(name).then(|| {
                let mut renamed = name.to_vec();
                renamed.extend_from_slice(STD_NAMESPACE.as_bytes());
                renamed
            })
        };
        object.bytes = hirsel_llvm_lazy::core::rename_symbols(&object.bytes, &rename)?;
        if let Some(debug) = &mut object.debug {
            for section in &mut debug.sections {
                if renamed.contains(section.symbol.as_bytes()) {
                    section.symbol.push_str(STD_NAMESPACE);
                }
            }
        }
        Ok(object)
    }

    /// Each part's bitcode, in part order.
    #[cfg(test)]
    pub(crate) fn codes(&self) -> Vec<Vec<u8>> {
        let parts = &self.parts;
        let codes = parts.codes.as_ref().expect("a std with parts");
        (0..parts.parts.len())
            .map(|i| {
                codes
                    .get(code_name(self.key, i).unwrap())
                    .unwrap()
                    .expect("a part's bitcode")
            })
            .collect()
    }
}

/// The record stored under `key` when it decodes, is of `kin` and its own
/// content derives `key`, and names its parts and lent bodies by the names
/// `key` derives; anything else is a miss.
fn load_record(store: &Store, key: ContentHash, kin: ContentHash) -> io::Result<Option<StdRecord>> {
    let Some(record) = store
        .get(key)?
        .and_then(|bytes| decode::<StdRecord>(&bytes))
    else {
        return Ok(None);
    };
    let sound = record.schema == STD_SCHEMA
        && !record.parts.is_empty()
        && record.lent == lent_name(key)?
        && side_key(kin, record.whole, &record.requested, &record.units)? == key;
    Ok(sound.then_some(record))
}

fn part_name(key: ContentHash, i: usize) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-part.v1", &(key, i as u64))
}

fn code_name(key: ContentHash, i: usize) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-code.v1", &(key, i as u64))
}

fn lent_name(key: ContentHash) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-lent.v1", &key)
}
