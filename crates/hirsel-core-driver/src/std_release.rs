//! The release std: every plain instance of the library image a program is
//! checked against, lowered as one unit from the image alone. Its roots are
//! the image's non-generic functions the image checked whole, read from the
//! image, never from a program's reach, so what the unit holds and how it is
//! optimised is a function of the image and the generator. Every root is an
//! export of the unit; a program calls it by its symbol and holds none of its
//! bodies.
//!
//! The unit and a program meet at five contracts:
//! - callable: each root is defined under its plain instance symbol, and a
//!   root with a step-free body also under that body's code symbol;
//! - step: each root's [`Summary`] (step-free body, opens a bound, launches)
//!   stands in the program's step specialisation for the body it calls;
//! - width: neither side resolves a blob's `W` over the other's bodies;
//! - link: every other strong definition of the unit is renamed into its
//!   own namespace (`$std`), so no name the unit defines meets a program's;
//! - site: the unit's site references name source items, whose words the
//!   program's source map assigns over the unit's points and its own.
use super::*;
use crate::native::{NativeOptions, NativeStop, ObjectProduct};
use hirsel_core_ir::step_contexts::Summary;
use hirsel_core_ir::{FuncId, Identity};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

const STD_SCHEMA: u32 = 1;

/// The release std's publication record, stored under the std key.
#[derive(Serialize, Deserialize)]
struct StdRecord {
    schema: u32,
    /// The plain roots the unit defines, in identity order.
    roots: Vec<Identity>,
    /// Each root's step summary, by the root's code identity.
    summaries: Vec<(ContentHash, Summary)>,
    /// The source points the unit's site references name, by file.
    points: BTreeMap<String, Vec<(u32, u32)>>,
    /// The unit's parts in the shared parts cache, in link order.
    parts: Vec<ContentHash>,
}

/// A separately lowered release std, as a program's assembly takes it.
pub(crate) struct StdSide {
    /// The key the std's record is stored under; it names the std product.
    pub key: ContentHash,
    pub roots: BTreeSet<Identity>,
    pub summaries: BTreeMap<ContentHash, Summary>,
    pub points: BTreeMap<String, Vec<(u32, u32)>>,
    pub objects: Vec<ObjectProduct>,
}

/// The suffix of the std's own hidden names.
const STD_NAMESPACE: &str = "$std";

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

    /// The release std of this product's library image under `options`'
    /// generator: its stored record when one is held, otherwise lowered from
    /// the image alone and published.
    pub(crate) fn std_side(
        &self,
        options: &NativeOptions,
        runtime: ContentHash,
    ) -> Result<StdSide, NativeStop> {
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("the release std is an image's".into()))?;
        let compiler = crate::reuse::compiler_identity()?;
        let llvm = crate::tool_inputs::llvm_library()?;
        let key = crate::native::key(
            "hirsel.core-std-instance.v1",
            &(
                STD_SCHEMA,
                image.identity,
                compiler,
                llvm,
                runtime,
                &options.target,
                options.debug_lines,
                options.logical_charge_report.is_some(),
                !hirsel_llvm_lazy::core::facts_enabled(),
            ),
        )?;
        let shared = Store::new(&options.parts)?;
        let record = match load_record(&shared, key)? {
            Some(record) => record,
            None => {
                let record = self.lower_std(options, key, &shared)?;
                shared.put_keyed(key, &encode(&record)?)?;
                record
            }
        };
        let instance = record
            .roots
            .first()
            .cloned()
            .ok_or_else(|| NativeStop::Entry("the image has no plain function".into()))?;
        let mut objects = Vec::with_capacity(record.parts.len());
        for &part in &record.parts {
            let (bare, object, file) = crate::native_object::load(&shared, part)?
                .ok_or_else(|| NativeStop::Entry("a release std part is missing".into()))?;
            objects.push(ObjectProduct {
                private_variants: 0,
                instance: instance.clone(),
                path: crate::native_object::path(&shared, bare.address),
                identity: bare.address,
                check: bare.check,
                held: Some(file),
                debug: object.debug,
                debug_record: None,
                compiled: false,
            });
        }
        Ok(StdSide {
            key,
            roots: record.roots.into_iter().collect(),
            summaries: record.summaries.into_iter().collect(),
            points: record.points,
            objects,
        })
    }

    /// Walk, prepare, join, specialize and lower the release std, and publish
    /// its parts. Reads the library image and `options`' generator only.
    fn lower_std(
        &self,
        options: &NativeOptions,
        key: ContentHash,
        shared: &Store,
    ) -> Result<StdRecord, NativeStop> {
        let plain = self.std_plain_roots()?;
        let mut materialized = crate::instances::InstanceSet::default();
        let mut reached = BTreeSet::new();
        let mut roots = Vec::new();
        // A root whose walk stops is left out alone; a program reaching it
        // lowers it itself and stops there as it does today.
        for root in plain {
            if let Ok(walked) =
                self.instances_into([root.clone()], options.instance_limit, &mut materialized)
            {
                reached.extend(walked);
                roots.push(root);
            }
        }
        let reached: Vec<Identity> = reached.into_iter().collect();
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
        for identity in &reached {
            let instance = materialized.get(identity);
            units.insert(
                identity.clone(),
                super::recipe_build::prepare_unit(instance.ir()?, instance.function, &sources),
            );
        }
        let points = super::recipe_build::bind_annexes(&mut units, &BTreeMap::new())?.points;
        let mut modules: Vec<_> = units.values().map(|unit| Cow::Borrowed(&unit.module)).collect();
        let body_keys = crate::callable_variants::prepare_inputs(
            &mut modules,
            &reached,
            crate::callable_variants::Roots::All(&roots),
            options.instance_limit,
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
            .filter(|root| functions.contains_key(root))
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
        let target = hirsel_core_layout::Target::from_triple(&options.target)
            .expect("supported native target");
        let facts = hirsel_core_ir::roles::Facts::program(&[&module], &[]);
        let emit = |message: String| NativeStop::Emit {
            definition: first.definition.clone(),
            message,
        };
        let program = hirsel_llvm_lazy::emit_program_parts(
            &[hirsel_llvm_lazy::Unit::Core {
                module: &module,
                facts: &facts[0],
                observe_charge: options.logical_charge_report.is_some(),
                debug_lines: options.debug_lines,
            }],
            &exports,
            target,
            hirsel_guard::jobs() as u32,
            &mut hirsel_llvm_lazy::Unheld,
        )
        .map_err(emit)?;
        let parts: Vec<ContentHash> = (0..program.parts.len())
            .map(|i| crate::native::key("hirsel.core-std-part.v1", &(key, i as u64)))
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
            object.bytes = hirsel_llvm_lazy::core::rename_symbols(&object.bytes, &rename).map_err(emit)?;
            if let Some(debug) = &mut object.debug {
                for section in &mut debug.sections {
                    if own.contains(section.symbol.as_bytes()) {
                        section.symbol.push_str(STD_NAMESPACE);
                    }
                }
            }
        }
        for (object, &part) in objects.iter().zip(&parts) {
            crate::native_object::publish(shared, part, object)?;
        }
        if body_keys.is_empty() {
            return Err(NativeStop::Entry("the release std holds no body".into()));
        }
        Ok(StdRecord {
            schema: STD_SCHEMA,
            roots,
            summaries: codes.into_iter().zip(summaries).collect(),
            points,
            parts,
        })
    }
}

fn load_record(shared: &Store, key: ContentHash) -> io::Result<Option<StdRecord>> {
    Ok(shared
        .get(key)?
        .and_then(|bytes| decode::<StdRecord>(&bytes))
        .filter(|record| record.schema == STD_SCHEMA && !record.parts.is_empty()))
}
