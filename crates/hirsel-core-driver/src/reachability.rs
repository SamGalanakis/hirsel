//! Build acceptance uses demanded definitions; package check keeps every diagnostic.
use super::*;
use crate::reach::Graph;
use hirsel_core_ir::Identity;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
pub struct GapStop {
    pub definition: String,
    pub rule: String,
    pub construct: String,
    pub primary: SourceSite,
}
/// A census counts the first stop of each demanded definition, not every message.
/// Behind an incomplete body, resolved source names expose the remaining closure.
/// Completed bodies use their emitted calls and function references.
pub struct Reachable<'a> {
    /// How many bodies and declarations the build demands.
    pub bodies: usize,
    pub declarations: usize,
    pub diagnostics: Vec<&'a Refusal>,
    pub incomplete: Vec<&'a Incomplete>,
    /// Resource refusals the build needs: the product's, and instance collection's.
    pub resources: Vec<ResourceRefusal>,
    pub first_stops: Vec<GapStop>,
    /// The stop a refused build reports, rendered with its source location:
    /// a source refusal ahead of a checker gap, and either ahead of an
    /// instance-collection stop, which a refusal elsewhere can cause (a body
    /// that calls a refused definition has no native product of its own).
    pub(crate) leading: Option<String>,
    pub instances: Result<Vec<Identity>, InstanceStop>,
    pub counters: crate::reach::ReachCounters,
    pub(crate) materialized: crate::instances::InstanceSet,
    /// The header walk's nodes, when this walk ran it.
    pub(crate) graph: Option<Arc<Graph>>,
    /// The build's native inputs, when `reachable_for` collected this set.
    pub(crate) native: Option<crate::native::NativeInputs>,
    /// A Cranelift development request's reach record.
    pub(crate) reach: Option<crate::reach::Use>,
}

/// A complete recipe header walk. The instance walk still supplies the
/// current bodies. Its result and the source constants are the graph roots.
pub(crate) struct RecipeGraph {
    instances: Vec<Identity>,
    constants: Vec<DefPath>,
    rows: rustc_hash::FxHashMap<DefPath, crate::reach::Row>,
    graph: Arc<Graph>,
    counts: (usize, usize),
}

impl RecipeGraph {
    fn reuse(
        &mut self,
        product: &CheckProduct,
        instances: &[Identity],
        constants: &[DefPath],
        image: Option<&crate::image::ImageDirectory>,
    ) -> io::Result<bool> {
        if self.instances != instances || self.constants != constants {
            return Ok(false);
        }
        let mut rows = crate::reach::WalkRows::new(product);
        for (path, row) in &self.rows {
            if rows.is_current(path, row) {
                continue;
            }
            for body in [false, true] {
                if let Some(edges) = self.graph.get(&(path.clone(), body))
                    && *edges != product.expand(&mut rows, path, body, image, false)?
                {
                    return Ok(false);
                }
            }
        }
        self.rows.extend(rows.rows);
        Ok(true)
    }
}
impl Reachable<'_> {
    /// A published recipe owns the units it will assemble. Release the
    /// instance bodies while keeping the request's reach report.
    pub(crate) fn finish(&mut self) -> crate::reach::ReachCounters {
        self.materialized = crate::instances::InstanceSet::default();
        self.counters
    }

    /// The bodyless foreign definitions selected by this entry's checked slot closure.
    pub fn foreign_functions(&self) -> impl Iterator<Item = &DefPath> {
        self.materialized
            .foreign
            .iter()
            .map(|identity| &identity.definition)
    }

    pub(crate) fn recording_stop(
        &self,
        product: &CheckProduct,
        entry: &DefPath,
    ) -> Result<Option<NativeStop>, NativeStop> {
        if !self.accepted() {
            return Ok(None);
        }
        let Some(function) = self
            .foreign_functions()
            .min_by_key(|function| function.definition_identity())
        else {
            return Ok(None);
        };
        let reason =
            crate::native::recipe_build::recording_refusal(entry, std::iter::once(function))
                .expect("one reached foreign function");
        // Reach records decide the refusal. Read the checked bodies for its
        // site only, so cached call sites never outlive the current source.
        let fresh = if self.reach.is_some() || !self.materialized.hits.is_empty() {
            let limit = self.require()?.len() + self.materialized.foreign.len();
            Some(product.reachable(entry, limit)?)
        } else {
            None
        };
        let reached = fresh.as_ref().unwrap_or(self);
        let site = reached.foreign_site(product, function)?;
        Ok(Some(NativeStop::ForeignRecording { reason, site }))
    }

    fn foreign_site(
        &self,
        product: &CheckProduct,
        function: &DefPath,
    ) -> Result<String, NativeStop> {
        use hirsel_core_ir::{Callee, Inst};

        for identity in self.require()? {
            let instance = self.materialized.get(identity);
            let module = instance.ir()?;
            let Some(body) = &module.functions[instance.function.index()].body else {
                continue;
            };
            let anchors = body
                .insts
                .iter()
                .zip(&body.inst_anchors)
                .filter_map(|(inst, anchor)| {
                    let Inst::Call { call } = inst else {
                        return None;
                    };
                    let Callee::Direct(callee) = body.calls[call.index()].callee else {
                        return None;
                    };
                    let path = module
                        .identity(module.functions[callee.index()].instance)
                        .definition;
                    (&path == function).then_some(*anchor).flatten()
                })
                .collect::<Vec<_>>();
            if anchors.is_empty() {
                continue;
            }
            let items = anchors
                .iter()
                .map(|anchor| module.items[anchor.item.index()].clone())
                .collect::<Vec<_>>();
            let sources = product.native_sources(&items)?;
            let positions = sources.positions();
            if let Some((file, line, column)) = anchors
                .iter()
                .zip(&items)
                .filter_map(|(anchor, item)| positions.position(item, anchor.start))
                .min_by_key(|(file, line, column)| (&file.path, *line, *column))
            {
                return Ok(format!("{}:{line}:{column}", file.path));
            }
        }
        // An implicit release can contribute a slot without a source call.
        Ok(function.definition_identity())
    }

    pub(crate) fn require_native(
        &self,
        product: &CheckProduct,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<&[Identity], NativeStop> {
        let instances = self.require()?;
        if options.recording
            && let Some(stop) = self.recording_stop(product, entry)?
        {
            return Err(stop);
        }
        Ok(instances)
    }

    /// The entry's code address. A complete reach hit holds it in the record head.
    pub(crate) fn address(&self, entry: &DefPath) -> Result<ContentHash, NativeStop> {
        if let Some(crate::reach::Use {
            hit: true,
            prior: Some(prior),
            ..
        }) = &self.reach
        {
            return Ok(prior.head.address);
        }
        let prior = self.reach.as_ref().and_then(|r| r.prior.as_ref());
        self.materialized
            .addresses(self.require()?, prior)
            .and_then(|addresses| addresses.get(&Identity::plain(entry.clone())).copied())
            .ok_or_else(|| {
                NativeStop::Entry(format!("{entry}: a reached instance has no code address"))
            })
    }
    /// The code revision of each instance this walk's instances describe:
    /// the rows a link's entry object defines (`ir::revision_symbol`).
    pub(crate) fn revisions(&self) -> Result<BTreeMap<Identity, ContentHash>, NativeStop> {
        let prior = self.reach.as_ref().and_then(|r| r.prior.as_ref());
        self.materialized
            .revisions(self.require()?, prior)
            .ok_or_else(|| NativeStop::Entry("a described instance has no revision row".into()))
    }
    pub fn accepted(&self) -> bool {
        self.diagnostics.is_empty()
            && self.incomplete.is_empty()
            && self.resources.is_empty()
            && self.instances.is_ok()
    }
    /// Whether the walk demanded `path`'s declaration part.
    #[cfg(test)]
    pub(crate) fn declares(&self, path: &DefPath) -> bool {
        match (
            &self.graph,
            self.reach.as_ref().and_then(|r| r.prior.as_ref()),
        ) {
            (Some(graph), _) => graph.contains_key(&(path.clone(), false)),
            (None, Some(prior)) => prior.node(path, false).is_some(),
            (None, None) => false,
        }
    }
    pub(crate) fn require(&self) -> Result<&[Identity], NativeStop> {
        if !self.diagnostics.is_empty() || !self.incomplete.is_empty() {
            let stop = self
                .leading
                .clone()
                .expect("a refused walk has a leading stop");
            return Err(NativeStop::Check(stop));
        }
        if let Some(r) = self.resources.first() {
            return Err(NativeStop::Check(r.message.clone()));
        }
        self.instances
            .as_deref()
            .map_err(|e| NativeStop::Instances(e.clone()))
    }
    pub fn census(&self) -> BTreeMap<&str, usize> {
        let mut counts = BTreeMap::new();
        for stop in &self.first_stops {
            *counts.entry(stop.construct.as_str()).or_default() += 1;
        }
        counts
    }
}
/// Which nodes the walk reached: its own graph, or the reach record's.
enum Reached<'g> {
    Graph(&'g Graph),
    Record(&'g crate::reach::Reach),
}
impl Reached<'_> {
    fn body(&self, path: &DefPath) -> bool {
        self.has(path, true)
    }
    fn declaration(&self, path: &DefPath) -> bool {
        self.has(path, false)
    }
    fn has(&self, path: &DefPath, body: bool) -> bool {
        match self {
            Reached::Graph(graph) => graph.contains_key(&(path.clone(), body)),
            Reached::Record(reach) => reach.node(path, body).is_some(),
        }
    }
    fn bodies(&self) -> Vec<DefPath> {
        match self {
            Reached::Graph(graph) => graph
                .keys()
                .filter(|(_, b)| *b)
                .map(|(p, _)| p.clone())
                .collect(),
            Reached::Record(reach) => reach
                .nodes()
                .filter(|n| n.body)
                .map(|n| reach.row_path(n.row as usize).clone())
                .collect(),
        }
    }
    fn unavailable(&self) -> Vec<DefPath> {
        match self {
            Reached::Graph(graph) => graph
                .iter()
                .filter(|(_, (_, u))| *u)
                .map(|((p, _), _)| p.clone())
                .collect(),
            Reached::Record(reach) => reach
                .nodes()
                .filter(|n| n.unavailable)
                .map(|n| reach.row_path(n.row as usize).clone())
                .collect(),
        }
    }
}
impl CheckProduct {
    pub fn reachable(&self, entry: &DefPath, limit: usize) -> io::Result<Reachable<'_>> {
        hirsel_core_ids::published::catch_io(|| {
            self.reachable_inner(entry, limit, None, None, false)
        })
    }

    pub(crate) fn reachable_recipe(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> io::Result<Reachable<'_>> {
        let limit = options.instance_limit;
        if !crate::resident::serving() {
            return self.reachable(entry, limit);
        }
        let key = crate::native::key(
            "hirsel.recipe-reach.v1",
            &(
                self.environment,
                self.image.as_ref().map(|image| image.identity),
                entry,
                limit,
            ),
        )?;
        let record = crate::resident::RECIPE_REACH
            .get_or(Some(key), || Ok::<_, io::Error>(Arc::default()))?;
        self.reachable_recipe_record(
            entry,
            limit,
            matches!(options.backend, crate::native::Backend::Cranelift),
            &mut record.lock().unwrap(),
        )
    }

    pub(crate) fn reachable_recipe_record(
        &self,
        entry: &DefPath,
        limit: usize,
        compact_recipe: bool,
        record: &mut Option<RecipeGraph>,
    ) -> io::Result<Reachable<'_>> {
        hirsel_core_ids::published::catch_io(|| {
            self.reachable_inner(entry, limit, None, Some(record), compact_recipe)
        })
    }
    /// The build's reachable set for `options`. A Cranelift development build
    /// reads its reach record: if every row it holds is live, the walk's result
    /// is the record's; otherwise unchanged instances expand from its table and
    /// the rest walk their instance records, so a hit materialises nothing.
    pub fn reachable_for(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> io::Result<Reachable<'_>> {
        self.reachable_rebuilding(entry, options, &BTreeSet::new())
    }
    /// [`Self::reachable_for`], rebuilding the instances in `damaged`.
    pub(crate) fn reachable_rebuilding(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
        damaged: &BTreeSet<Identity>,
    ) -> io::Result<Reachable<'_>> {
        let inputs = crate::native::native_inputs(self, options)?;
        let mut reached = hirsel_core_ids::published::catch_io(|| {
            let Some(records) = inputs.records.as_ref() else {
                return self.reachable_inner(entry, options.instance_limit, None, None, false);
            };
            let Some(selection) = crate::reach::selection(self, records, entry, options)? else {
                return self.reachable_inner(
                    entry,
                    options.instance_limit,
                    Some((records, None, damaged)),
                    None,
                    false,
                );
            };
            let loaded = crate::reach::load(&records.store, selection)?;
            let record_bytes = loaded.as_ref().map_or(0, |(_, n)| *n);
            let prior = loaded.map(|(r, _)| r);
            let clean = self.diagnostics.is_empty()
                && self.incomplete.is_empty()
                && self.resources.is_empty();
            let constants = self.own_constants()?;
            let changed = prior.as_ref().map(|p| p.changed(self)).unwrap_or_default();
            let entry_live = prior
                .as_ref()
                .is_some_and(|p| p.row(entry).is_some_and(|i| !changed[i]));
            let hit = prior.as_ref().is_some_and(|prior| {
                clean
                    && damaged.is_empty()
                    && prior.constants() == constants
                    && !changed.contains(&true)
            });
            if let (true, Some(held)) = (hit, &prior) {
                let (bodies, declarations) = held.counts();
                let instances = (0..held.len())
                    .map(|i| held.entry(i).identity.clone())
                    .collect();
                let mut materialized = crate::instances::InstanceSet::default();
                materialized
                    .foreign
                    .extend(held.foreign().cloned().map(Identity::plain));
                self.bind_foreign_addresses(&mut materialized)?;
                return Ok(Reachable {
                    bodies,
                    declarations,
                    diagnostics: vec![],
                    incomplete: vec![],
                    resources: vec![],
                    first_stops: vec![],
                    leading: None,
                    instances: Ok(instances),
                    counters: crate::reach::ReachCounters {
                        expanded: held.len() as u64,
                        record_bytes: record_bytes as u64,
                        vector: held.rows() as u64,
                        ..Default::default()
                    },
                    materialized,
                    graph: None,
                    native: None,
                    reach: Some(crate::reach::Use {
                        selection,
                        prior: Some(prior.expect("a hit holds its record")),
                        hit: true,
                        entry_live,
                        constants,
                        // A hit keeps the record's sections; its build
                        // writes them under a new head when the head moved.
                        draft: None,
                    }),
                });
            }
            let mut reached = self.reachable_inner(
                entry,
                options.instance_limit,
                Some((
                    records,
                    Some((prior.as_ref(), &changed, &constants)),
                    damaged,
                )),
                None,
                false,
            )?;
            reached.counters.record_bytes = record_bytes as u64;
            let complete = reached.accepted() && reached.first_stops.is_empty();
            let draft = match (complete, reached.graph.take()) {
                (false, _) => None,
                (true, Some(graph)) => {
                    let mut rows = crate::reach::WalkRows::new(self);
                    // The walk's rows, recorded again over the nodes it reached.
                    for (path, _) in graph.keys() {
                        rows.header(path);
                    }
                    let draft = crate::reach::Draft::walked(rows.rows, &graph);
                    reached.graph = Some(graph);
                    Some(draft)
                }
                (true, None) => Some(crate::reach::patched(
                    self,
                    prior.as_ref().unwrap(),
                    &changed,
                )),
            };
            reached.counters.vector = draft.as_ref().map_or(0, |d| d.len(prior.as_ref())) as u64;
            reached.reach = Some(crate::reach::Use {
                selection,
                prior,
                hit: false,
                entry_live,
                constants,
                draft,
            });
            Ok(reached)
        })?;
        reached.native = Some(inputs);
        Ok(reached)
    }
    /// LANG-20's source constants: every own constant, which check evaluates
    /// before a body, including unused ones. Installed constants already carry
    /// their published value.
    fn own_constants(&self) -> io::Result<Vec<DefPath>> {
        let image_directory = self.image.as_ref().map(|i| i.directory()).transpose()?;
        let mut constants = Vec::new();
        for (id, definition) in self
            .index
            .definitions
            .own_iter()
            .enumerate()
            .map(|(i, d)| (self.index.definitions.base_len() + i, d))
        {
            if definition.kind == hirsel_core_resolve::DefinitionKind::Constant {
                let path = self
                    .index
                    .definition_path(hirsel_core_resolve::DefinitionId(id as u32));
                if !image_directory
                    .as_ref()
                    .is_some_and(|i| i.headers.contains_key(&path))
                {
                    constants.push(path);
                }
            }
        }
        constants.sort();
        Ok(constants)
    }
    /// One node of the header walk: the nodes it reaches, and whether its body
    /// is unavailable. Every index read goes through `rows`.
    fn expand(
        &self,
        rows: &mut crate::reach::WalkRows<'_>,
        path: &DefPath,
        body: bool,
        image_directory: Option<&crate::image::ImageDirectory>,
        stopped: bool,
    ) -> io::Result<(Vec<(DefPath, bool)>, bool)> {
        let mut edges = Vec::new();
        let header = rows.header(path);
        if !body {
            if let Some(header) = header {
                edges.extend(
                    header
                        .reads
                        .iter()
                        .map(|&id| (self.index.definition_path(id), false)),
                );
            }
            return Ok((edges, false));
        }
        edges.push((path.clone(), false));
        if let Some(header) = header {
            // Named types/signatures are needed even when folding removes a call.
            edges.extend(
                header
                    .body_reads
                    .iter()
                    .map(|&id| (self.index.definition_path(id), false)),
            );
        }
        if !rows.defined(path) && !image_directory.is_some_and(|i| i.definitions.contains_key(path))
        {
            return Ok((edges, true));
        }
        let product = self.demand_body(path)?;
        if let Some(function) = product.check.function {
            // A complete walk reached every callee as an instance; one that
            // stopped reaches the rest through the body's module.
            if stopped {
                let ir = product.ir()?;
                edges.extend(
                    hirsel_core_materialize::external_functions(ir, function)
                        .into_iter()
                        .map(|f| &ir.instances[ir.functions[f.index()].instance.index()])
                        .filter(|key| !hirsel_core_front::is_glue(&key.definition))
                        .map(|key| (key.definition.clone(), !self.is_foreign(&key.definition))),
                );
            }
        } else if let Some(header) = header {
            // Failed lowering has no call graph. Keep its resolved references
            // for diagnostics/census rather than hiding everything after its gap.
            for id in &header.body_reads {
                let read = self.index.definition_path(*id);
                if rows.header(&read).is_some_and(|h| h.has_body) {
                    edges.push((read, true));
                }
            }
        }
        Ok((edges, false))
    }
    #[allow(clippy::type_complexity)]
    fn reachable_inner(
        &self,
        entry: &DefPath,
        limit: usize,
        records: Option<(
            &crate::instances::Records,
            Option<(Option<&crate::reach::Reach>, &[bool], &[DefPath])>,
            &BTreeSet<Identity>,
        )>,
        mut recipe_record: Option<&mut Option<RecipeGraph>>,
        compact_recipe: bool,
    ) -> io::Result<Reachable<'_>> {
        let mut materialized = crate::instances::InstanceSet::for_reach(compact_recipe);
        let roots = [Identity::plain(entry.clone())];
        let prior = records.and_then(|(_, p, _)| p);
        let instances = match records {
            Some((records, _, damaged)) => {
                let expand = crate::instances::Expand {
                    prior: prior.and_then(|(p, _, _)| p),
                    changed: prior.map_or(&[], |(_, c, _)| c),
                    damaged,
                };
                self.instances_over_records(roots, limit, &mut materialized, records, expand)
            }
            None => self.instances_into(roots, limit, &mut materialized),
        };
        let image_directory = self.image.as_ref().map(|i| i.directory()).transpose()?;
        let constants = match prior {
            Some((_, _, constants)) => constants.to_vec(),
            None => self.own_constants()?,
        };
        // The record's graph is this walk's when its roots are equal and each
        // changed node reaches what it reached: the walk is a function of its
        // roots and of each node's edges.
        let mut reuse = None;
        if let (Some((Some(prior), changed, _)), Ok(reached)) = (prior, &instances)
            && prior.constants() == constants
        {
            let definitions: BTreeSet<&DefPath> = reached
                .iter()
                .map(|i| &i.definition)
                .filter(|d| !hirsel_core_front::is_glue(d))
                .collect();
            if definitions == prior.definitions() {
                let mut rows = crate::reach::WalkRows::new(self);
                let mut same = true;
                for node in prior.nodes() {
                    if changed[node.row as usize] {
                        let path = prior.row_path(node.row as usize);
                        let live = self.expand(
                            &mut rows,
                            path,
                            node.body,
                            image_directory.as_deref(),
                            false,
                        )?;
                        if live != (prior.edges(&node), node.unavailable) {
                            same = false;
                            break;
                        }
                    }
                }
                if same {
                    reuse = Some(prior);
                }
            }
        }
        let clean =
            self.diagnostics.is_empty() && self.incomplete.is_empty() && self.resources.is_empty();
        let mut recipe_counts = None;
        let mut recipe_rows = None;
        let graph = match reuse {
            Some(_) => None,
            None => {
                if clean
                    && let (Some(Some(record)), Ok(instances)) =
                        (recipe_record.as_deref_mut(), &instances)
                    && record.reuse(self, instances, &constants, image_directory.as_deref())?
                {
                    recipe_counts = Some(record.counts);
                    Some(Arc::clone(&record.graph))
                } else {
                    let mut rows = crate::reach::WalkRows::new(self);
                    let mut pending = BTreeSet::from([(entry.clone(), true)]);
                    // A glue instance has no definition; the hooks its body calls
                    // are reached instances of their own.
                    pending.extend(
                        instances
                            .iter()
                            .flatten()
                            .filter(|i| !hirsel_core_front::is_glue(&i.definition))
                            .map(|i| (i.definition.clone(), true)),
                    );
                    pending.extend(constants.iter().map(|c| (c.clone(), false)));
                    let mut graph = Graph::new();
                    while let Some(node) = pending.pop_first() {
                        if graph.contains_key(&node) {
                            continue;
                        }
                        let (edges, unavailable) = self.expand(
                            &mut rows,
                            &node.0,
                            node.1,
                            image_directory.as_deref(),
                            instances.is_err(),
                        )?;
                        pending.extend(edges.iter().filter(|e| !graph.contains_key(e)).cloned());
                        graph.insert(node, (edges, unavailable));
                    }
                    recipe_rows = Some(rows.rows);
                    Some(Arc::new(graph))
                }
            }
        };
        let reached = match (&graph, reuse) {
            (Some(graph), _) => Reached::Graph(graph),
            (None, Some(prior)) => Reached::Record(prior),
            (None, None) => unreachable!("a walk without a graph reuses the record's"),
        };
        let (bodies_len, declarations_len) = recipe_counts.unwrap_or_else(|| match &reached {
            Reached::Graph(graph) => {
                let bodies = graph.keys().filter(|(_, b)| *b).count();
                (bodies, graph.len() - bodies)
            }
            Reached::Record(prior) => prior.counts(),
        });
        let mut first_stops = BTreeMap::new();
        // Each stop's site, to render the leading one.
        let mut stop_sites: BTreeMap<String, Site> = BTreeMap::new();
        let mut diagnostics = vec![];
        let mut incomplete = vec![];
        let mut resources: Vec<ResourceRefusal> = vec![];
        // A reused graph is an accepted record's: its bodies, the image's and
        // the request's, carried no refusal, and the product has none.
        if recipe_counts.is_none() && !(clean && matches!(reached, Reached::Record(_))) {
            let owners: BTreeMap<_, _> = self
                .headers
                .rows
                .available()
                .into_iter()
                .filter_map(|(id, h)| {
                    let h = h.as_ref()?;
                    let path = self
                        .index
                        .definition_path(hirsel_core_resolve::DefinitionId(id as u32));
                    Some(((h.anchor.1, h.anchor.2), (path, h)))
                })
                .collect();
            let needed = |site: &Site| -> bool {
                if let Some((path, header)) = crate::item_at(site).and_then(|at| owners.get(&at)) {
                    reached.body(path)
                        || reached.declaration(path) && site.span.start < header.header_end
                } else {
                    // A file-wide decoding/resource failure affects each demand
                    // from its file; a module's own failure each from its module.
                    let within = |h: &reuse::Header| match site.source {
                        Some(source) => h.anchor.1 == source,
                        None => h.anchor.0 == site.module,
                    };
                    site.item == u32::MAX
                        && owners
                            .values()
                            .any(|(p, h)| within(h) && (reached.body(p) || reached.declaration(p)))
                }
            };
            let mut library_diagnostics = vec![];
            let mut library_incomplete = vec![];
            if let Some(image) = &self.image {
                for path in &reached.bodies() {
                    if !self.definitions.contains_key(path) && image.has_body(path)? {
                        let body = image.demand_ref(path)?;
                        library_diagnostics.extend(
                            body.check
                                .refusals
                                .iter()
                                .filter(|d| !self.diagnostics.contains(d)),
                        );
                        library_incomplete.extend(body.incomplete.iter().filter(|d| {
                            !self.incomplete.iter().any(|other| {
                                other.primary == d.primary && other.message == d.message
                            })
                        }));
                    }
                }
            }
            diagnostics = self
                .diagnostics
                .iter()
                .chain(library_diagnostics)
                .filter(|d| needed(&d.primary))
                .collect();
            incomplete = self
                .incomplete
                .iter()
                .chain(library_incomplete)
                .filter(|d| needed(&d.primary))
                .collect();
            for (site, rule, message) in diagnostics
                .iter()
                .map(|d| (&d.primary, d.rule.as_str(), d.message.as_str()))
                .chain(
                    incomplete
                        .iter()
                        .map(|d| (&d.primary, "checker gap", d.message.as_str())),
                )
            {
                let primary = self.source_site(site)?;
                let name = crate::item_at(site)
                    .and_then(|at| owners.get(&at))
                    .map(|(p, _)| p.to_string())
                    .unwrap_or_else(|| primary.module.clone());
                if let std::collections::btree_map::Entry::Vacant(slot) =
                    first_stops.entry(name.clone())
                {
                    stop_sites.insert(name.clone(), *site);
                    slot.insert(GapStop {
                        definition: name,
                        rule: rule.to_owned(),
                        construct: message.to_owned(),
                        primary,
                    });
                }
            }
            resources = self
                .resources
                .iter()
                .filter(|r| needed(&r.primary))
                .cloned()
                .collect();
        }
        let unavailable = if recipe_counts.is_some() {
            Vec::new()
        } else {
            reached.unavailable()
        };
        let complete_graph = unavailable.is_empty();
        for path in unavailable {
            if let Some(h) = self.headers.get(&path) {
                stop_sites.entry(path.to_string()).or_insert(h.site);
                first_stops.entry(path.to_string()).or_insert(GapStop {
                    definition: path.to_string(),
                    rule: "instances".into(),
                    construct: "native body is unavailable".into(),
                    primary: self.source_site(&h.site)?,
                });
            }
        }
        if let Err(InstanceStop::Resource { limit, next }) = &instances
            && let Some(h) = self.headers.get(&next.definition)
        {
            resources.push(ResourceRefusal {
                primary: h.site,
                message: format!(
                    "instance collection resource refusal: `{}` needs instance {} past the limit of {limit}",
                    next.definition,
                    limit + 1
                ),
            });
        }
        if let Err(stop) = &instances
            && !matches!(stop, InstanceStop::Resource { .. })
        {
            let (path, construct) = match stop {
                InstanceStop::OutsideReached(p) => (p, "body is unavailable".to_owned()),
                InstanceStop::Incomplete {
                    definition,
                    message,
                }
                | InstanceStop::Input {
                    definition,
                    message,
                } => (definition, message.clone()),
                InstanceStop::Resource { .. } => unreachable!("a resource refusal, above"),
            };
            if let Some(h) = self.headers.get(path)
                && let std::collections::btree_map::Entry::Vacant(slot) =
                    first_stops.entry(path.to_string())
            {
                stop_sites.insert(path.to_string(), h.site);
                slot.insert(GapStop {
                    definition: path.to_string(),
                    rule: "instances".into(),
                    construct,
                    primary: self.source_site(&h.site)?,
                });
            }
        }
        let counters = crate::reach::ReachCounters {
            expanded: materialized.table.len() as u64,
            instance_path: materialized.keys.len() as u64,
            ..Default::default()
        };
        if instances.is_ok()
            && diagnostics.is_empty()
            && incomplete.is_empty()
            && resources.is_empty()
        {
            self.bind_foreign_addresses(&mut materialized)?;
        }
        let rank = |rule: &str| match rule {
            "instances" => 2,
            "checker gap" => 1,
            _ => 0,
        };
        let leading = first_stops
            .iter()
            .min_by_key(|(name, stop)| (rank(&stop.rule), *name))
            .map(|(name, stop)| -> io::Result<String> {
                let location = self.render_site(&stop_sites[name])?;
                Ok(if stop.definition == stop.primary.module {
                    format!("{location}: {} {}", stop.rule, stop.construct)
                } else {
                    format!(
                        "{}: {location}: {} {}",
                        stop.definition, stop.rule, stop.construct
                    )
                })
            })
            .transpose()?;
        if let (Some(record), Some(rows), Some(graph), Ok(instances)) =
            (recipe_record, recipe_rows, &graph, &instances)
            && clean
            && complete_graph
            && diagnostics.is_empty()
            && incomplete.is_empty()
            && resources.is_empty()
            && first_stops.is_empty()
        {
            *record = Some(RecipeGraph {
                instances: instances.clone(),
                constants,
                rows,
                graph: Arc::clone(graph),
                counts: (bodies_len, declarations_len),
            });
        }
        Ok(Reachable {
            bodies: bodies_len,
            declarations: declarations_len,
            diagnostics,
            incomplete,
            resources,
            first_stops: first_stops.into_values().collect(),
            leading,
            instances,
            counters,
            materialized,
            graph,
            native: None,
            reach: None,
        })
    }
}
