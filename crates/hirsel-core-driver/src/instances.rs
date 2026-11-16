//! Reachable native instances are collected separately from one-pass source checking.
//! Each is named by its [`Identity`] and built by `hirsel_core_materialize` from its
//! arguments' closures, which the walk's [`Resolver`] finds by hash.
use super::*;
use hirsel_core_ir::Identity;
use hirsel_core_materialize::{Child, Published, Resolver};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceStop {
    Resource {
        limit: usize,
        next: Identity,
    },
    OutsideReached(DefPath),
    Input {
        definition: DefPath,
        message: String,
    },
    Incomplete(DefPath),
}
/// The emitter supplies mechanical instance dependencies. In particular, a growing
/// f[T] -> f[List[T]] family stops as a compiler resource outcome, not a language rule.
pub fn collect_instances(
    roots: impl IntoIterator<Item = Identity>,
    limit: usize,
    mut dependencies: impl FnMut(&Identity) -> Result<Vec<Identity>, InstanceStop>,
) -> Result<Vec<Identity>, InstanceStop> {
    let mut pending: BTreeSet<_> = roots.into_iter().collect();
    let mut reached = BTreeSet::new();
    while let Some(key) = pending.pop_first() {
        if reached.contains(&key) {
            continue;
        }
        if reached.len() == limit {
            return Err(InstanceStop::Resource { limit, next: key });
        }
        hirsel_guard::with_instance_context(key.definition.0.name.clone(), || {
            let reads = dependencies(&key)?;
            reached.insert(key);
            pending.extend(reads);
            Ok(())
        })?;
    }
    Ok(reached.into_iter().collect())
}
/// Per-build instances retain their materialized modules for native emission.
/// Nongeneric bodies keep the already-published module by reference. A walk
/// over instance records (`Records`) materialises only its misses: a hit keeps
/// its record instead.
#[derive(Default)]
pub(crate) struct InstanceSet {
    rows: std::collections::BTreeMap<Identity, usize>,
    bodies: Vec<InstanceBody>,
    /// Each walked instance's key and the store its record lives in, when the
    /// walk was over instance records.
    pub keys: std::collections::BTreeMap<Identity, (ContentHash, Location)>,
    /// The record of each instance whose record hit. Its object is read
    /// only by a link, through its descriptor.
    pub hits: std::collections::BTreeMap<Identity, crate::native_object::Instance>,
    /// Each instance expanded from the reach record's table, by its index there.
    pub table: std::collections::BTreeMap<Identity, u32>,
    /// The record fields of each materialised instance, for its record.
    pub fields: std::collections::BTreeMap<Identity, Fields>,
}
/// Which store an instance's object and record live in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Location {
    /// The request's store: a program definition or a glue instance.
    Request,
    /// The package image's store: an image definition.
    Library,
}
/// The fields of an instance record that are read from its module.
pub(crate) struct Fields {
    pub own: Option<ContentHash>,
    pub children: Vec<Identity>,
    pub closures: Vec<Arc<hirsel_core_ir::Closure>>,
}
/// A walk over Cranelift development instance records: the encoded
/// environment every key starts with, and its digest.
pub(crate) struct Records {
    pub env: Vec<u8>,
    pub digest: ContentHash,
    pub store: Store,
    pub library: Option<Store>,
}
impl Records {
    /// An instance's key: `hirsel.core-instance.v1` over the encoded
    /// `(env, identity, own)` (a tuple encodes as its fields in order).
    pub fn key(&self, identity: &Identity, own: Option<ContentHash>) -> io::Result<ContentHash> {
        let mut bytes = self.env.clone();
        bytes.extend(encode(&(identity, own))?);
        Ok(ContentHash::of_parts(
            "hirsel.core-instance.v1",
            [&bytes[..]],
        ))
    }
    pub fn store(&self, location: Location) -> &Store {
        match location {
            Location::Library => self.library.as_ref().unwrap_or(&self.store),
            Location::Request => &self.store,
        }
    }
}
/// The reach record a walk over instance records expands from: an instance
/// whose definition's row is live keeps its table entry. `damaged` names the
/// instances whose stored object failed its check, which are rebuilt.
#[derive(Clone, Copy)]
pub(crate) struct Expand<'r> {
    pub prior: Option<&'r crate::reach::Reach>,
    pub changed: &'r [bool],
    pub damaged: &'r BTreeSet<Identity>,
}
impl Expand<'_> {
    fn entry(&self, key: &Identity) -> Option<usize> {
        let prior = self.prior?;
        if self.damaged.contains(key) {
            return None;
        }
        let entry = prior.instance(key)?;
        if !hirsel_core_front::is_glue(&key.definition) {
            let row = prior.row(&key.definition)?;
            if self.changed[row] {
                return None;
            }
        }
        Some(entry)
    }
}
pub(crate) struct InstanceBody {
    /// The published definition; None for a glue instance.
    pub product: Option<Arc<DefinitionProduct>>,
    module: Option<Module>,
    pub function: hirsel_core_ir::FuncId,
    /// The module's signature-only rows the walk reached, by ascending row, with
    /// the instance each names.
    externals: Vec<(hirsel_core_ir::FuncId, Identity)>,
}
impl InstanceBody {
    pub fn ir(&self) -> io::Result<&Module> {
        match (&self.module, &self.product) {
            (Some(module), _) => Ok(module),
            (None, Some(product)) => product.ir(),
            (None, None) => unreachable!("a glue instance is materialised"),
        }
    }
    /// A published body's module is hashed in its record; a materialized
    /// one is hashed here, by the same rule, through the caller's `scratch`.
    pub fn ir_hash(&self, scratch: &mut Vec<u8>) -> io::Result<ContentHash> {
        match (&self.module, &self.product) {
            (Some(module), _) => crate::body::ir_hash(module, scratch),
            (None, Some(product)) => Ok(product.ir_hash()),
            (None, None) => unreachable!("a glue instance is materialised"),
        }
    }
}
impl InstanceSet {
    /// Each of `instances`' index in the reach record's table, if expanded,
    /// by one walk of both: `instances` is in identity order, as the table is.
    pub(crate) fn expansions<'a>(
        &'a self,
        instances: &'a [Identity],
    ) -> impl Iterator<Item = Option<u32>> + 'a {
        debug_assert!(instances.is_sorted());
        let mut table = self.table.iter().peekable();
        instances.iter().map(move |instance| {
            while table.next_if(|(k, _)| *k < instance).is_some() {}
            table.next_if(|(k, _)| *k == instance).map(|(_, &i)| i)
        })
    }
    pub fn get(&self, key: &Identity) -> &InstanceBody {
        &self.bodies[self.rows[key]]
    }
    /// The source items `key`'s module names: its record's, on a hit.
    pub fn items(&self, key: &Identity) -> io::Result<&[DefPath]> {
        match self.hits.get(key) {
            Some(record) => Ok(&record.items),
            None => Ok(&self.get(key).ir()?.items),
        }
    }
    /// `key`'s module's [`Module::link_symbols`]. Its root and every external row
    /// the walk reached name an instance of this set, whose symbol `names` holds
    /// once per set row, so no row's closure is taken again; a callable code
    /// variant names its variant, and any other row is named by its module.
    pub fn link_symbols(
        &self,
        key: &Identity,
        names: &mut Vec<Option<String>>,
    ) -> io::Result<Vec<String>> {
        names.resize(self.bodies.len(), None);
        let mut name = |identity: &Identity| -> String {
            match self.rows.get(identity) {
                Some(&row) => names[row].get_or_insert_with(|| identity.symbol()).clone(),
                None => identity.symbol(),
            }
        };
        let body = self.get(key);
        let module = body.ir()?;
        Ok((0..module.functions.len() as u32)
            .map(hirsel_core_ir::FuncId)
            .map(|f| {
                if let Some(&code) = module.code_variants.get(&f) {
                    hirsel_core_ir::symbol(code)
                } else if f == body.function {
                    name(key)
                } else if let Ok(i) = body.externals.binary_search_by_key(&f, |(g, _)| *g) {
                    name(&body.externals[i].1)
                } else {
                    hirsel_core_ir::symbol(module.code_identity(f))
                }
            })
            .collect())
    }
    /// The materialised instance of `definition` whose type arguments' row names are
    /// `arguments`, for tests that name an instance by its source text.
    #[cfg(test)]
    pub fn named(&self, definition: &DefPath, arguments: &[&str]) -> Option<Identity> {
        self.rows.iter().find_map(|(identity, &row)| {
            let body = &self.bodies[row];
            let module = body.ir().ok()?;
            let key = &module.instances[module.functions[body.function.index()].instance.index()];
            (&identity.definition == definition && module.argument_names(key) == arguments)
                .then(|| identity.clone())
        })
    }
}
impl CheckProduct {
    pub fn instances(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_into(roots, limit, &mut InstanceSet::default())
    }
    pub(crate) fn instances_into(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        instances: &mut InstanceSet,
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_observed(roots, limit, instances, None, |_, _| {})
    }
    /// Collect instances over Cranelift development instance records: a hit
    /// takes its children and closures from its record and materialises
    /// nothing; a miss is materialised from its template and its arguments'
    /// closures, which the walk's records and materialised modules supply.
    pub(crate) fn instances_over_records(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        instances: &mut InstanceSet,
        records: &Records,
        expand: Expand<'_>,
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_observed(roots, limit, instances, Some((records, expand)), |_, _| {})
    }
    /// Measure library loading and mechanical specialization without changing the
    /// native collection path. The ordinary path erases the no-op observer.
    pub fn observe_instances(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        observe: impl FnMut(&Identity, &'static str),
    ) -> Result<Vec<Identity>, InstanceStop> {
        self.instances_observed(roots, limit, &mut InstanceSet::default(), None, observe)
    }
    fn instances_observed(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
        instances: &mut InstanceSet,
        records: Option<(&Records, Expand<'_>)>,
        mut observe: impl FnMut(&Identity, &'static str),
    ) -> Result<Vec<Identity>, InstanceStop> {
        let mut resolver = Resolver::default();
        collect_instances(roots, limit, |key| {
            self.instance_step(key, instances, records, &mut resolver, &mut observe)
        })
    }
    /// Walk one instance: its record on a hit, else its materialised body,
    /// into `instances`; the instances it reaches.
    pub(crate) fn instance_step(
        &self,
        key: &Identity,
        instances: &mut InstanceSet,
        records: Option<(&Records, Expand<'_>)>,
        resolver: &mut Resolver,
        observe: &mut impl FnMut(&Identity, &'static str),
    ) -> Result<Vec<Identity>, InstanceStop> {
        {
            observe(key, "start");
            if let Some((_, expand)) = records
                && let Some(i) = expand.entry(key)
            {
                // An unchanged instance: its table entry, by M2 rule 1.
                let prior = expand.prior.unwrap();
                let entry = &prior.instances[i];
                observe(key, "table");
                resolver.add_closures(prior.closures_of(entry));
                instances.table.insert(key.clone(), i as u32);
                return Ok(entry
                    .children
                    .iter()
                    .map(|&c| prior.instances[c as usize].identity.clone())
                    .collect());
            }
            let input = |message: String| InstanceStop::Input {
                definition: key.definition.clone(),
                message,
            };
            let glue = hirsel_core_front::is_glue(&key.definition);
            // A glue instance has no definition; an image definition's
            // instance is the image's.
            let product = if glue {
                None
            } else {
                let installed = self
                    .image
                    .as_ref()
                    .map(|i| i.has_body(&key.definition))
                    .transpose()
                    .map_err(|e| input(e.to_string()))?
                    .unwrap_or(false);
                if !self.definitions.contains_key(&key.definition) && !installed {
                    return Err(InstanceStop::OutsideReached(key.definition.clone()));
                }
                let product = self
                    .demand_body(&key.definition)
                    .map_err(|e| input(e.to_string()))?;
                if !product.check.refusals.is_empty() || !product.incomplete.is_empty() {
                    return Err(InstanceStop::Incomplete(key.definition.clone()));
                }
                if product.check.function.is_none() {
                    return Err(InstanceStop::Incomplete(key.definition.clone()));
                }
                Some(product)
            };
            let own = product.as_ref().map(|p| p.record);
            if let Some((records, expand)) = records {
                let location = if glue || self.definitions.contains_key(&key.definition) {
                    Location::Request
                } else {
                    Location::Library
                };
                let instance_key = records.key(key, own).map_err(|e| input(e.to_string()))?;
                instances.keys.insert(key.clone(), (instance_key, location));
                let hit = match expand.damaged.contains(key) {
                    true => None,
                    false => {
                        crate::native_object::load_instance(records.store(location), instance_key)
                            .map_err(|e| input(e.to_string()))?
                            .filter(|r| {
                                r.identity == *key && r.own == own && r.env == records.digest
                            })
                    }
                };
                if let Some(record) = hit {
                    observe(key, "hit");
                    resolver.add_closures(&record.closures);
                    let pending = record.children.clone();
                    instances.hits.insert(key.clone(), record);
                    return Ok(pending);
                }
            }
            let arguments = resolver.arguments(key).map_err(|hash| {
                input(format!(
                    "no closure {} reached this instance",
                    hirsel_core_ir::symbol(hash)
                ))
            })?;
            let built = match &product {
                None => {
                    observe(key, "load");
                    hirsel_core_materialize::materialize_glue(key, &arguments).map_err(input)?
                }
                Some(product) => {
                    let own = Published {
                        module: product.ir().map_err(|e| input(e.to_string()))?,
                        function: product.check.function.unwrap(),
                        template: product.check.template.as_ref(),
                    };
                    observe(key, "load");
                    hirsel_core_materialize::materialize(&own, key, &arguments).map_err(input)?
                }
            };
            observe(key, "materialize");
            resolver.add(&built.children);
            if records.is_some() {
                let mut closures = std::collections::BTreeMap::new();
                for closure in built.children.iter().flat_map(|c| &c.closures) {
                    closures
                        .entry(closure.hash)
                        .or_insert_with(|| Arc::clone(closure));
                }
                instances.fields.insert(
                    key.clone(),
                    Fields {
                        own,
                        children: built
                            .children
                            .iter()
                            .map(|c| c.identity.clone())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect(),
                        closures: closures.into_values().collect(),
                    },
                );
            }
            let externals: Vec<_> = built
                .children
                .into_iter()
                .map(
                    |Child {
                         function, identity, ..
                     }| (function, identity),
                )
                .collect();
            let pending = externals
                .iter()
                .map(|(_, identity)| identity.clone())
                .collect::<BTreeSet<_>>();
            let row = instances.bodies.len();
            instances.rows.insert(key.clone(), row);
            instances.bodies.push(InstanceBody {
                product,
                module: built.module,
                function: built.function,
                externals,
            });
            observe(key, "dependencies");
            Ok(pending.into_iter().collect())
        }
    }
}
