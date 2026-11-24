//! Reachable native instances are collected separately from one-pass source checking.
//! Each is named by its [`Identity`] and built by `hirsel_core_materialize` from its
//! arguments' closures, which the walk's [`Resolver`] finds by hash.
use super::*;
use hirsel_core_ir::{
    CodeRow, FuncId, Identity, address::code_groups_with_external, code_revision,
};
use hirsel_core_materialize::{Child, Published, Resolver};
use std::collections::BTreeSet;

/// The records-free walk's units, retained until their consumer finishes. Native
/// compilation and evaluation materialize the same per-definition products.
pub struct Units {
    reached: Vec<Identity>,
    instances: InstanceSet,
}
impl Units {
    pub fn modules(&self) -> io::Result<Vec<hirsel_core_materialize::Unit<'_>>> {
        self.reached
            .iter()
            .map(|identity| {
                let body = self.instances.get(identity);
                Ok(hirsel_core_materialize::Unit {
                    identity,
                    module: body.ir()?,
                    function: body.function,
                })
            })
            .collect()
    }
    /// The entry's revision, computed before any link remaps its units.
    pub fn revision(&self, root: &Identity) -> Option<ContentHash> {
        self.address(root).map(code_revision)
    }

    /// The code address before native linking.
    pub fn address(&self, root: &Identity) -> Option<ContentHash> {
        self.instances
            .addresses(&self.reached, None)?
            .get(root)
            .copied()
    }
    /// The code revision of each instance a reached unit describes, as a
    /// link of this walk gives its revision carriers.
    pub fn revisions(&self) -> Option<std::collections::BTreeMap<Identity, ContentHash>> {
        self.instances.revisions(&self.reached, None)
    }
}

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
    Incomplete {
        definition: DefPath,
        message: String,
    },
}
impl std::fmt::Display for InstanceStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource { limit, next } => write!(
                f,
                "`{}` needs instance {} past the limit of {limit}",
                next.definition,
                limit + 1
            ),
            Self::OutsideReached(definition) => write!(f, "{definition}: body is unavailable"),
            Self::Input {
                definition,
                message,
            }
            | Self::Incomplete {
                definition,
                message,
            } => write!(f, "{definition}: {message}"),
        }
    }
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
        hirsel_guard::with_memory_context(
            format!("instantiating function {}", key.definition.0.name),
            || {
                let reads = dependencies(&key)?;
                reached.insert(key);
                pending.extend(reads);
                Ok(())
            },
        )?;
    }
    Ok(reached.into_iter().collect())
}
/// Per-build instances retain their materialized modules for native emission.
/// Nongeneric bodies keep the already-published module by reference. A walk
/// over instance records (`Records`) materialises only its misses: a hit keeps
/// its record instead.
#[derive(Default)]
pub(crate) struct InstanceSet {
    addresses: std::sync::OnceLock<Option<BTreeMap<Identity, ContentHash>>>,
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
    summarize_foreign: bool,
    compact_recipe: bool,
    pub(crate) foreign: BTreeSet<Identity>,
    pub(crate) foreign_addresses: BTreeMap<Identity, ContentHash>,
}
/// Which store an instance's object and record live in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Location {
    /// The request's store: a program definition or a glue instance.
    Request,
    /// The package image's store: an image definition.
    Library,
}
/// The ABI of the reached definition and its external references. Intrinsic
/// uses have no native link ABI to compare.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodeSignatures {
    pub own: ContentHash,
    pub slots: Vec<(Identity, ContentHash)>,
}

/// A generated adapter may be emitted weak by several owners. Each owner
/// describes the same local body and the same ordered binding identities.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Adapter {
    identity: Identity,
    body: ContentHash,
    slots: Vec<Identity>,
}

type GroupRows<'a> = BTreeMap<&'a Identity, (ContentHash, &'a Vec<Identity>, &'a CodeSignatures)>;

/// The fields of an instance record that are read from its module.
pub(crate) struct Fields {
    pub own: Option<ContentHash>,
    /// The module's PROG-10 body hash.
    pub body: ContentHash,
    pub children: Vec<Identity>,
    pub slots: Vec<Identity>,
    pub signatures: CodeSignatures,
    pub adapters: Vec<Adapter>,
    /// The instances the module describes (`revision::described`).
    pub described: Vec<Identity>,
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
    /// An instance's key: `hirsel.core-instance.v2` over the encoded
    /// `(env, identity, own)` (a tuple encodes as its fields in order).
    pub fn key(&self, identity: &Identity, own: Option<ContentHash>) -> io::Result<ContentHash> {
        let mut bytes = self.env.clone();
        bytes.extend(encode(&(identity, own))?);
        Ok(ContentHash::of_parts(
            "hirsel.core-instance.v2",
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
    module: Option<Arc<MaterializedModule>>,
    pub function: hirsel_core_ir::FuncId,
    /// The module's PROG-10 body hash.
    pub body: ContentHash,
    /// The normal-form body bytes `body` hashes; the module is their decoding.
    pub code: Arc<[u8]>,
    pub slots: Vec<Identity>,
    pub signatures: CodeSignatures,
    pub adapters: Vec<Adapter>,
    /// The instances the module describes (`revision::described`).
    pub described: Vec<Identity>,
    /// The module's signature-only rows the walk reached, by ascending row, with
    /// the instance each names.
    externals: Vec<(hirsel_core_ir::FuncId, Identity)>,
}
impl InstanceBody {
    pub fn ir(&self) -> io::Result<&Module> {
        match (&self.module, &self.product) {
            (Some(module), _) => Ok(module.ir(&self.code)),
            (None, Some(product)) => product.ir(),
            (None, None) => unreachable!("a glue instance is materialised"),
        }
    }
    pub(crate) fn anchored_items(&self) -> io::Result<Vec<DefPath>> {
        match self.module.as_deref() {
            Some(MaterializedModule::Recipe { sites, .. }) => Ok(sites.anchored_items()),
            _ => Ok(hirsel_core_ir::source::anchored_items(self.ir()?)),
        }
    }

    pub(crate) fn annex(
        &self,
        sources: &hirsel_core_ir::source::SourcePositions<'_>,
    ) -> io::Result<hirsel_core_ir::normal::Annex> {
        match self.module.as_deref() {
            Some(MaterializedModule::Recipe { sites, .. }) => Ok(sites.annex(sources)),
            _ => Ok(hirsel_core_ir::normal::retained_annex(self.ir()?, sources)),
        }
    }

    pub(crate) fn prepare_assembly(
        &self,
        sources: &hirsel_core_ir::source::SourcePositions<'_>,
    ) -> io::Result<(hirsel_core_ir::normal::Annex, Module)> {
        use hirsel_core_ir::normal;
        match self.module.as_deref() {
            Some(MaterializedModule::Recipe { sites, .. }) => {
                Ok((sites.annex(sources), normal::assembly_module(&self.code)))
            }
            _ => Ok(normal::prepare_retained_assembly(self.ir()?, sources)),
        }
    }

    /// A published body's module is hashed in its record; a materialized
    /// one is hashed here, by the same rule, through the caller's `scratch`.
    pub fn ir_hash(&self, scratch: &mut Vec<u8>) -> io::Result<ContentHash> {
        match (&self.module, &self.product) {
            (Some(module), _) => crate::body::ir_hash(module.ir(&self.code), scratch),
            (None, Some(product)) => Ok(product.ir_hash()),
            (None, None) => unreachable!("a glue instance is materialised"),
        }
    }
}
impl InstanceSet {
    pub(crate) fn for_reach(compact_recipe: bool) -> Self {
        Self {
            summarize_foreign: true,
            compact_recipe,
            ..Self::default()
        }
    }

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
            None => {
                let body = self.get(key);
                match body.module.as_deref() {
                    Some(MaterializedModule::Recipe { sites, .. }) => Ok(sites.items()),
                    _ => Ok(&body.ir()?.items),
                }
            }
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
                hirsel_core_materialize::link_symbol(module, f, || {
                    if f == body.function {
                        name(key)
                    } else if let Ok(i) = body.externals.binary_search_by_key(&f, |(g, _)| *g) {
                        name(&body.externals[i].1)
                    } else {
                        hirsel_core_ir::symbol(module.code_identity(f))
                    }
                })
            })
            .collect())
    }
    /// One hash per group after the walk has chosen the current body for every
    /// identity. Warm records supply only local body hashes and slot order.
    pub(crate) fn addresses(
        &self,
        instances: &[Identity],
        prior: Option<&crate::reach::Reach>,
    ) -> Option<&BTreeMap<Identity, ContentHash>> {
        self.addresses
            .get_or_init(|| {
                self.groups(
                    instances,
                    prior,
                    instances.iter().cloned(),
                    &self.foreign_addresses,
                    |_, _| {},
                )
            })
            .as_ref()
    }

    /// Bind the canonical slots once, including the existing ABI and shared
    /// adapter agreement checks, and pass each group frame to the caller.
    pub(crate) fn groups(
        &self,
        instances: &[Identity],
        prior: Option<&crate::reach::Reach>,
        roots: impl IntoIterator<Item = Identity>,
        foreign: &BTreeMap<Identity, ContentHash>,
        group: impl FnMut(ContentHash, Vec<u8>),
    ) -> Option<BTreeMap<Identity, ContentHash>> {
        let rows = self.group_rows(instances, prior, foreign)?;
        code_groups_with_external(
            roots,
            |identity| {
                let (body, slots, _) = rows.get(identity)?;
                Some(CodeRow {
                    body: *body,
                    slots: (*slots).clone(),
                })
            },
            |identity| foreign.get(identity).copied(),
            group,
        )
    }

    pub(crate) fn group_plan(
        &self,
        entry: &Identity,
        instances: &[Identity],
    ) -> Option<hirsel_core_ir::address::GroupPlan> {
        let rows = self.group_rows(instances, None, &self.foreign_addresses)?;
        hirsel_core_ir::address::GroupPlan::new(
            [entry.clone()],
            |identity| {
                let (body, slots, _) = rows.get(identity)?;
                Some(CodeRow {
                    body: *body,
                    slots: (*slots).clone(),
                })
            },
            |identity| self.foreign_addresses.get(identity).copied(),
        )
    }

    fn group_rows<'a>(
        &'a self,
        instances: &'a [Identity],
        prior: Option<&'a crate::reach::Reach>,
        foreign: &BTreeMap<Identity, ContentHash>,
    ) -> Option<GroupRows<'a>> {
        let mut rows = BTreeMap::new();
        let mut generated = BTreeMap::new();
        for identity in instances {
            let (body, slots, signatures, adapters) = if let Some(&row) = self.rows.get(identity) {
                let body = &self.bodies[row];
                (body.body, &body.slots, &body.signatures, &body.adapters)
            } else if let Some(record) = self.hits.get(identity) {
                (
                    record.body,
                    &record.slots,
                    &record.signatures,
                    &record.adapters,
                )
            } else {
                let prior = prior?;
                let entry = prior.entry(prior.instance(identity)?);
                (entry.body, &entry.slots, &entry.signatures, &entry.adapters)
            };
            rows.insert(identity, (body, slots, signatures));
            for adapter in adapters {
                use std::collections::btree_map::Entry;
                match generated.entry(&adapter.identity) {
                    Entry::Vacant(entry) => {
                        entry.insert(adapter);
                    }
                    Entry::Occupied(entry) if *entry.get() == adapter => {}
                    Entry::Occupied(_) => return None,
                }
            }
        }
        for (_, _, signatures) in rows.values() {
            for (identity, expected) in &signatures.slots {
                match rows.get(identity) {
                    Some((_, _, actual)) if *expected == actual.own => (),
                    None if foreign.contains_key(identity) => (),
                    _ => return None,
                }
            }
        }
        Some(rows)
    }

    /// Each instance that one of `instances` describes, with its code
    /// revision over this walk (`revision`), by identity.
    pub(crate) fn revisions(
        &self,
        instances: &[Identity],
        prior: Option<&crate::reach::Reach>,
    ) -> Option<std::collections::BTreeMap<Identity, ContentHash>> {
        let mut described = BTreeSet::new();
        for identity in instances {
            if let Some(&row) = self.rows.get(identity) {
                described.extend(self.bodies[row].described.iter().cloned());
            } else if let Some(record) = self.hits.get(identity) {
                described.extend(record.described.iter().cloned());
            } else {
                let prior = prior?;
                let entry = prior.entry(prior.instance(identity)?);
                described.extend(
                    entry
                        .described
                        .iter()
                        .map(|&c| prior.entry(c as usize).identity.clone()),
                );
            }
        }
        let addresses = self.addresses(instances, prior)?;
        described
            .into_iter()
            .map(|identity| {
                let revision = code_revision(*addresses.get(&identity)?);
                Some((identity, revision))
            })
            .collect()
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
    pub(crate) fn is_foreign(&self, path: &DefPath) -> bool {
        self.foreign_inputs
            .get(&path.0.package)
            .is_some_and(|modules| modules.contains_key(&path.0.modules))
    }

    /// A body stopped before emission can have only a request-level refusal.
    /// Use the current sites, since a conflicting name can move in another item
    /// without changing this body's record.
    fn body_source_stop(&self, definition: &DefPath) -> io::Result<Option<String>> {
        let Some(header) = self.headers.get(definition) else {
            return Ok(None);
        };
        let (_, source, item) = header.anchor;
        let syntax = self
            .files
            .iter()
            .find(|f| f.file == source)
            .and_then(|f| f.syntax.items.get(item as usize))
            .filter(|range| range.has_errors);
        let owns = |site: &Site| {
            site.source == Some(source)
                && (site.item == item
                    || site.item == u32::MAX
                        && syntax.is_some_and(|range| range.owned.contains(&site.span.start)))
        };
        if let Some(refusal) = self.diagnostics.iter().find(|r| owns(&r.primary)) {
            return self.render_refusal(refusal).map(Some);
        }
        if let Some(resource) = self.resources.iter().find(|r| owns(&r.primary)) {
            return Ok(Some(format!(
                "{}: resource {}",
                self.render_site(&resource.primary)?,
                resource.message
            )));
        }
        Ok(None)
    }

    /// Materialize every unit reached from `roots`, without native object records.
    /// A program entry is one root; source corpora can select several definitions.
    pub fn units(
        &self,
        roots: impl IntoIterator<Item = Identity>,
        limit: usize,
    ) -> Result<Units, InstanceStop> {
        let mut instances = InstanceSet::default();
        let reached = self.instances_into(roots, limit, &mut instances)?;
        Ok(Units { reached, instances })
    }
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
        let mut reached = collect_instances(roots, limit, |key| {
            self.instance_step(key, instances, records, &mut resolver, &mut observe)
        })?;
        if !instances.foreign.is_empty() {
            reached.retain(|identity| !instances.foreign.contains(identity));
        }
        Ok(reached)
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
            if self.is_foreign(&key.definition) {
                if instances.summarize_foreign {
                    instances.foreign.insert(key.clone());
                    return Ok(Vec::new());
                }
                return Err(InstanceStop::Incomplete {
                    definition: key.definition.clone(),
                    message: "foreign code requires a native build".into(),
                });
            }
            if let Some((_, expand)) = records
                && let Some(i) = expand.entry(key)
            {
                // An unchanged instance: its table entry, by M2 rule 1.
                let prior = expand.prior.unwrap();
                let entry = prior.entry(i);
                observe(key, "table");
                resolver.add_closures(prior.closures_of(entry));
                instances.table.insert(key.clone(), i as u32);
                return Ok(entry
                    .children
                    .iter()
                    .map(|&c| &prior.entry(c as usize).identity)
                    .chain(
                        entry
                            .slots
                            .iter()
                            .filter(|slot| self.is_foreign(&slot.definition)),
                    )
                    .cloned()
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
                let message = if let Some(refusal) = product.check.refusals.first() {
                    Some(format!("{}: {}", refusal.rule, refusal.message))
                } else if let Some(gap) = product.incomplete.first() {
                    Some(gap.message.clone())
                } else if product.check.function.is_none() {
                    Some(
                        self.body_source_stop(&key.definition)
                            .map_err(|e| input(e.to_string()))?
                            .unwrap_or_else(|| "body has no complete native product".to_owned()),
                    )
                } else {
                    None
                };
                if let Some(message) = message {
                    return Err(InstanceStop::Incomplete {
                        definition: key.definition.clone(),
                        message,
                    });
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
            // A function of the instance's identity and its definition's
            // published record: a resident compiler keeps it across requests.
            let memo_key = (own != Some(ContentHash::default())).then(|| (key.clone(), own));
            let made = crate::resident::MATERIALISED.get_or(memo_key, || {
                materialise(
                    key,
                    product.as_deref(),
                    &arguments,
                    observe,
                    instances.compact_recipe,
                )
            })?;
            let Materialised {
                module,
                function,
                children,
                body,
                code,
                slots,
                signatures,
                adapters,
                described,
            } = made;
            resolver.add(&children);
            if records.is_some() {
                let mut closures = std::collections::BTreeMap::new();
                for closure in children.iter().flat_map(|c| &c.closures) {
                    closures
                        .entry(closure.hash)
                        .or_insert_with(|| Arc::clone(closure));
                }
                instances.fields.insert(
                    key.clone(),
                    Fields {
                        own,
                        body,
                        children: children
                            .iter()
                            .map(|c| c.identity.clone())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect(),
                        slots: slots.clone(),
                        signatures: signatures.clone(),
                        adapters: adapters.clone(),
                        described: described.clone(),
                        closures: closures.into_values().collect(),
                    },
                );
            }
            let mut externals: Vec<_> = children
                .into_iter()
                .map(
                    |Child {
                         function, identity, ..
                     }| (function, identity),
                )
                .collect();
            externals.sort_by_key(|(function, _)| *function);
            let pending = externals
                .iter()
                .map(|(_, identity)| identity.clone())
                .collect::<BTreeSet<_>>();
            let row = instances.bodies.len();
            instances.rows.insert(key.clone(), row);
            instances.bodies.push(InstanceBody {
                product,
                module,
                function,
                body,
                code,
                slots,
                signatures,
                adapters,
                described,
                externals,
            });
            observe(key, "dependencies");
            Ok(pending.into_iter().collect())
        }
    }
}

enum MaterializedModule {
    Decoded(Module),
    Recipe {
        sites: hirsel_core_ir::normal::RetainedSites,
        decoded: std::sync::OnceLock<Module>,
    },
}

impl MaterializedModule {
    fn ir(&self, code: &[u8]) -> &Module {
        match self {
            Self::Decoded(module) => module,
            Self::Recipe { sites, decoded } => decoded.get_or_init(|| sites.restore(code)),
        }
    }
}

/// An instance as the walk materialises it, before its walk-specific
/// bookkeeping: a function of its identity, its definition's published
/// record and its arguments' closures, which its identity names by hash.
#[derive(Clone)]
pub(crate) struct Materialised {
    module: Option<Arc<MaterializedModule>>,
    function: hirsel_core_ir::FuncId,
    children: Vec<Child>,
    body: ContentHash,
    code: Arc<[u8]>,
    slots: Vec<Identity>,
    signatures: CodeSignatures,
    adapters: Vec<Adapter>,
    described: Vec<Identity>,
}
fn materialise(
    key: &Identity,
    product: Option<&DefinitionProduct>,
    arguments: &[&hirsel_core_ir::Closure],
    observe: &mut impl FnMut(&Identity, &'static str),
    compact_recipe: bool,
) -> Result<Materialised, InstanceStop> {
    let input = |message: String| InstanceStop::Input {
        definition: key.definition.clone(),
        message,
    };
    let built = match product {
        None => {
            observe(key, "load");
            hirsel_core_materialize::materialize_glue(key, arguments).map_err(input)?
        }
        Some(product) => {
            let own = Published {
                module: product.ir().map_err(|e| input(e.to_string()))?,
                function: product.check.function.unwrap(),
                template: product.check.template.as_ref(),
            };
            observe(key, "load");
            hirsel_core_materialize::materialize(&own, key, arguments).map_err(input)?
        }
    };
    observe(key, "materialize");
    let (slots, signatures, adapters) = {
        use hirsel_core_ir::normal::{self, Slot};
        let module = built.module.as_ref().expect("materialized module");
        let mut hashes = rustc_hash::FxHashMap::default();
        let mut signature = |function: FuncId| {
            let row = module.functions[function.index()].signature;
            *hashes
                .entry(row)
                .or_insert_with(|| normal::signature(module, function))
        };
        let signatures = CodeSignatures {
            own: signature(built.function),
            slots: built
                .children
                .iter()
                .map(|child| (child.identity.clone(), signature(child.function)))
                .collect(),
        };
        let mut children = built.children.iter();
        let slots = built
            .code
            .slots
            .iter()
            .map(|slot| match slot {
                Slot::External(_) => children
                    .next()
                    .expect("a child for each external slot")
                    .identity
                    .clone(),
                Slot::Builtin(definition) => Identity::plain(definition.clone()),
            })
            .collect();
        let adapters = module
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, function)| {
                function.body.as_ref()?;
                let key = &module.instances[function.instance.index()];
                key.definition.0.name.contains("$callable[").then(|| {
                    let code = normal::encode(module, hirsel_core_ir::FuncId(i as u32));
                    Adapter {
                        identity: module.identity(function.instance),
                        body: code.hash(),
                        slots: code
                            .slots
                            .iter()
                            .map(|slot| slot.identity(module))
                            .collect(),
                    }
                })
            })
            .collect::<Vec<_>>();
        (slots, signatures, adapters)
    };
    let body = built.code.hash();
    let described = built.code.described;
    Ok(Materialised {
        module: built.module.map(|module| {
            Arc::new(if compact_recipe {
                MaterializedModule::Recipe {
                    sites: hirsel_core_ir::normal::RetainedSites::take(module),
                    decoded: std::sync::OnceLock::new(),
                }
            } else {
                MaterializedModule::Decoded(module)
            })
        }),
        function: built.function,
        children: built.children,
        body,
        code: built.code.bytes.into(),
        slots,
        signatures,
        adapters,
        described,
    })
}
