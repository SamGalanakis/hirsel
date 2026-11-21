//! The release std: every plain instance of the library image a program is
//! checked against, and every instance they reach, lowered as one unit from
//! the image alone. Its roots are the image's non-generic functions the image
//! checked whole, read from the image, never from a program's reach, so what
//! the unit holds and how it is optimised is a function of the image and the
//! generator. Every instance the unit holds is an export; a program calls it
//! by its symbol and holds none of its bodies. Every optimised LLVM build
//! takes one: a program lowers only the library instances the unit does not
//! hold, which its own type and closure arguments make.
//!
//! The unit and a program meet at five contracts:
//! - callable: each instance is defined under its instance symbol, and one
//!   with a step-free body also under that body's code symbol;
//! - step: each instance's [`Summary`] (step-free body, opens a bound,
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

const STD_SCHEMA: u32 = 4;

/// How many std sides each toolchain and variant's index names, most recent
/// first. One that falls off is lowered again when a build needs it.
const INDEX_LENGTH: usize = 8;

/// The largest body, in LLVM instructions after the std's optimiser, the std
/// lends a program's inliner. LLVM's inline cost decides each call site; this
/// bounds what each program reads and links.
const LEND_LIMIT: usize = 2000;

/// Whether `definition` is the library's: a side holds these units only.
fn library(definition: &DefPath) -> bool {
    definition.package_is("std", "1") || definition.package_is("core", "1")
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
    /// The units the side defines under their instance symbols, in identity
    /// order: every unit it holds, plain or not.
    roots: Vec<Identity>,
    /// Every unit the side lowered, with its owner (its body, annex and
    /// builtins), in identity order. The side's code is a function of these
    /// and of its toolchain and variant alone.
    units: Vec<(Identity, ContentHash)>,
    /// Each export's step summary, by its code identity.
    summaries: Vec<(ContentHash, Summary)>,
    /// The source points the unit's site references name, by file.
    points: BTreeMap<String, Vec<(u32, u32)>>,
    /// The unit's parts in the store, in link order.
    parts: Vec<ContentHash>,
    /// The store key of the bodies the unit lends, as bitcode.
    lent: ContentHash,
}

/// A separately lowered release std, as a program's assembly takes it.
pub(crate) struct StdSide {
    /// The key the std's record is stored under: its own content's.
    pub key: ContentHash,
    pub roots: BTreeSet<Identity>,
    pub summaries: BTreeMap<ContentHash, Summary>,
    pub points: BTreeMap<String, Vec<(u32, u32)>>,
    pub objects: Vec<ObjectProduct>,
    /// The bodies the std lends a program's inliner, as bitcode.
    pub lent: Vec<u8>,
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
    let roots: Vec<Identity> = units.keys().filter(|i| plain(i)).cloned().collect();
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
        let mut reached = BTreeSet::new();
        let mut roots = Vec::new();
        // A root whose walk stops is left out alone; a program reaching it
        // lowers it itself and stops there as it does today.
        for root in plain {
            if let Ok(walked) =
                self.instances_into([root.clone()], SIDE_INSTANCE_LIMIT, &mut materialized)
            {
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
    /// release builds on `target` in each of `variants` (the identity of the
    /// runtime archive linked, and whether debug lines are on), into the
    /// image's own store, which every build over the image reads first. A
    /// variant with a side already there keeps it.
    pub(crate) fn prebuild_release_std(
        &self,
        target: &str,
        variants: &[(ContentHash, bool)],
    ) -> Result<(), NativeStop> {
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("the release std is an image's".into()))?;
        let store = Store::new(&image.store_root().join(PREBUILT))?;
        let side = self.image_side()?;
        for &(runtime, debug_lines) in variants {
            let generator = Generator {
                target,
                debug_lines,
                observe_charge: false,
                runtime,
            };
            lower(&generator, generator.kin()?, side.clone_units(), &store)?;
        }
        Ok(())
    }
}

/// The release std a program's publication assembles against under
/// `options`' generator, linking the runtime whose identity is `runtime`. Of
/// the sides stored in `prebuilt` (a library image's own store), then in the
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
    prebuilt: Option<&Path>,
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
            objects: Vec::new(),
            lent: Vec::new(),
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
    let prebuilt = prebuilt
        .map(Path::to_owned)
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
        return side(store, key, record);
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
    let (key, record) = lower(&generator, kin, lowered, &shared)?;
    if !own(&record) {
        return Err(NativeStop::Entry(
            "the release std lowered for this program holds other bodies than it".into(),
        ));
    }
    side(&shared, key, record)
}

impl SideUnits {
    fn clone_units(&self) -> SideUnits {
        SideUnits {
            whole: self.whole,
            roots: self.roots.clone(),
            units: self.units.clone(),
        }
    }
}

/// Join, specialize and lower a side's units, publish its parts and record
/// into `store` under the key its content derives, and name it first in its
/// toolchain and variant's index there.
fn lower(
    options: &Generator<'_>,
    kin: ContentHash,
    side: SideUnits,
    store: &Store,
) -> Result<(ContentHash, StdRecord), NativeStop> {
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
        return Ok((key, record));
    }
    let reached: Vec<Identity> = units.keys().cloned().collect();
    let points = super::recipe_build::bind_annexes(&mut units, &BTreeMap::new(), false)?.points;
    let mut modules: Vec<_> = units
        .values()
        .map(|unit| Cow::Borrowed(&unit.module))
        .collect();
    // Every unit the side holds is an export: a program calls it by its
    // instance symbol and lowers only the instances the side does not hold.
    let body_keys = crate::callable_variants::prepare_inputs(
        &mut modules,
        &reached,
        crate::callable_variants::Roots::All(&reached),
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
    drop(modules);
    // Each unit's function: the row holding its own body.
    let mut functions: BTreeMap<Identity, FuncId> = BTreeMap::new();
    let wanted: BTreeSet<&Identity> = reached.iter().collect();
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
    let roots: Vec<Identity> = reached
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
    let program = hirsel_llvm_lazy::emit_program_parts(
        &[hirsel_llvm_lazy::Unit::Core {
            module: &module,
            facts: &facts[0],
            observe_charge: options.observe_charge,
            debug_lines: options.debug_lines,
        }],
        &exports,
        target,
        hirsel_guard::jobs() as u32,
        &mut hirsel_llvm_lazy::Unheld,
        hirsel_llvm_lazy::Lend::Lend { limit: LEND_LIMIT },
    )
    .map_err(emit)?;
    let parts: Vec<ContentHash> = (0..program.parts.len())
        .map(|i| part_name(key, i))
        .collect::<io::Result<_>>()?;
    let named = program
        .parts
        .iter()
        .zip(&parts)
        .map(|(part, &key)| (part.name.as_str(), key))
        .collect::<Vec<_>>();
    let cut = |object: &[u8], parts: &[(&str, ContentHash)]| {
        hirsel_llvm_lazy::core::split_native_object(object, parts, options.debug_lines)
            .map_err(emit)
    };
    let mut objects = match program.objects.as_slice() {
        [object] => cut(object, &named)?,
        objects => {
            let mut made = Vec::with_capacity(named.len());
            for (object, part) in objects.iter().zip(&named) {
                made.extend(cut(object, std::slice::from_ref(part))?);
            }
            made
        }
    };
    // The link contract: every other strong definition of the unit leaves
    // the program's namespace; its exports keep theirs.
    let exported: BTreeSet<&[u8]> = exports.iter().map(|e| e.as_bytes()).collect();
    let mut own = BTreeSet::new();
    for object in &objects {
        for name in hirsel_llvm_lazy::core::strong_definitions(&object.bytes).map_err(emit)? {
            if !exported.contains(name.as_slice()) {
                own.insert(name);
            }
        }
    }
    let rename = |name: &[u8]| {
        own.contains(name).then(|| {
            let mut renamed = name.to_vec();
            renamed.extend_from_slice(STD_NAMESPACE.as_bytes());
            renamed
        })
    };
    for object in &mut objects {
        object.bytes =
            hirsel_llvm_lazy::core::rename_symbols(&object.bytes, &rename).map_err(emit)?;
        if let Some(debug) = &mut object.debug {
            for section in &mut debug.sections {
                if own.contains(section.symbol.as_bytes()) {
                    section.symbol.push_str(STD_NAMESPACE);
                }
            }
        }
    }
    for (object, &part) in objects.iter().zip(&parts) {
        crate::native_object::publish(store, part, object)?;
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
        parts,
        lent,
    };
    store.put_keyed(key, &encode(&record)?)?;
    write_index(store, kin, key)?;
    Ok((key, record))
}

/// What a release std's lowering reads besides its units: the toolchain and
/// variant its key names. A side is lowered under `SIDE_INSTANCE_LIMIT`,
/// whatever a program's own limit.
struct Generator<'a> {
    target: &'a str,
    debug_lines: bool,
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
            debug_lines: options.debug_lines,
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
                self.debug_lines,
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

/// The std side `record`, stored in `store` under `key`, names: its parts and
/// lent bodies, each checked under the name its own key derives.
fn side(store: &Store, key: ContentHash, record: StdRecord) -> Result<StdSide, NativeStop> {
    let instance = record
        .roots
        .first()
        .cloned()
        .ok_or_else(|| NativeStop::Entry("the image has no plain function".into()))?;
    let mut objects = Vec::with_capacity(record.parts.len());
    for &part in &record.parts {
        let (bare, object, file) = crate::native_object::load(store, part)?
            .ok_or_else(|| NativeStop::Entry("a release std part is missing".into()))?;
        objects.push(ObjectProduct {
            private_variants: 0,
            instance: instance.clone(),
            path: crate::native_object::path(store, bare.address),
            identity: bare.address,
            check: bare.check,
            held: Some(file),
            debug: object.debug,
            debug_record: None,
            compiled: false,
        });
    }
    let lent = store
        .get(record.lent)?
        .ok_or_else(|| NativeStop::Entry("the release std's lent bodies are missing".into()))?;
    Ok(StdSide {
        key,
        lent,
        roots: record.roots.into_iter().collect(),
        summaries: record.summaries.into_iter().collect(),
        points: record.points,
        objects,
    })
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
    let mut sound = record.schema == STD_SCHEMA
        && !record.parts.is_empty()
        && record.lent == lent_name(key)?
        && side_key(kin, record.whole, &record.requested, &record.units)? == key;
    for (i, &part) in record.parts.iter().enumerate() {
        sound &= part == part_name(key, i)?;
    }
    Ok(sound.then_some(record))
}

fn part_name(key: ContentHash, i: usize) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-part.v1", &(key, i as u64))
}

fn lent_name(key: ContentHash) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-lent.v1", &key)
}
