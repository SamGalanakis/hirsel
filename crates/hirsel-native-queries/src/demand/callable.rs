//! Callable work binds one definition and its direct interfaces at a time.
//! The SCC walk and rewrite equations are shared with the ordinary RC pipeline.
use super::*;
use hirsel_lir::{
    ir::{Callee, Symbol, SymbolKind, UnitOrigin},
    products::Binding,
    stage::Value,
};
use hirsel_rc::closure_calls::{CallableEntry, CallableRecipe, FieldSummary, TaskCaptureRecipe};
use std::collections::{HashMap, VecDeque};

const WINDOW_RECORD: Domain = Domain {
    kind: super::super::instance_tasks::CALLABLE_GRAPH_KIND,
    schema: 3,
};

/// Only an ordinary addressed manifest survives the callable producer. Reading
/// a row reopens its facts; it does not transfer the producer's C acceptance.
pub(crate) struct PublishedCallables {
    records: BTreeMap<ContentHash, Reference>,
    _index: Option<Arc<window::WindowRead>>,
    target: ContentHash,
    policy: ContentHash,
}
#[derive(Serialize, Deserialize)]
struct Row {
    key: NamedKey,
    origin: NamedKey,
    request: Request,
    output: window::StoredRecord,
}
impl PublishedCallables {
    fn rows<'a>(
        &'a self,
        store: &'a dyn ResultStore,
    ) -> impl Iterator<Item = Result<Row, String>> + 'a {
        self.records
            .values()
            .map(|reference| read_json(store, *reference))
    }
    pub(crate) fn find(
        &self,
        store: &dyn ResultStore,
        address: hirsel_engine_store::Address,
    ) -> Result<Option<StoredInstance>, String> {
        let Some(reference) = self.records.get(&address.0) else {
            return Ok(None);
        };
        let row: Row = read_json(store, *reference)?;
        StoredInstance::from_record(store, row.output).map(Some)
    }
    pub(crate) fn outputs(
        &self,
        store: &dyn ResultStore,
    ) -> Result<Vec<(InstanceAddress, StoredInstance)>, String> {
        let mut outputs = self
            .rows(store)
            .map(|row| {
                let row = row?;
                let key = row.key.bind()?;
                let stored = StoredInstance::from_record(store, row.output)?;
                Ok((
                    InstanceAddress {
                        target_abi: self.target,
                        policy: self.policy,
                        instance: key,
                    },
                    stored,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        outputs.sort_by(|a, b| a.0.canonical_cmp(&b.0));
        Ok(outputs)
    }
    pub(crate) fn apply(&self, products: &mut DemandInstances) -> Result<(), String> {
        products.queries = products.queries.callable();
        let rows = self.records.values().copied().collect::<Vec<_>>();
        for reference in rows {
            let row: Row = read_json(products.store.as_ref(), reference)?;
            let stored = StoredInstance::from_record(products.store.as_ref(), row.output)?;
            let selected = row.key.bind()?;
            let origin = row.origin.bind()?;
            if !selected.is_same_or_derived_from(&origin) || row.request.key.bind()? != selected {
                return Err("callable manifest request has another origin".into());
            }
            // Receipt-aware originals keep the ordinary request checks. A
            // callable selection is a separately C-published transformation,
            // not another original import requiring new receipt roots.
            let mut original = row.request.clone();
            Arc::make_mut(&mut original.0).key = Arc::new(row.origin);
            products.admit(original)?;
            let key = products.address(selected);
            if let Some(previous) = products.requests.get(key.instance.encoding()) {
                if previous != &row.request {
                    return Err("callable manifest request differs".into());
                }
            } else {
                if products.requests.len() >= hirsel_rc::instance::MAX_INSTANCES {
                    return Err("native instance limit".into());
                }
                products
                    .requests
                    .insert(key.instance.encoding().to_vec(), row.request);
            }
            products.attach_stored(&key, stored)?;
        }
        Ok(())
    }
}
// Target and policy are explicit manifest context, never inferred from an ABI id.

fn read_json<T: for<'de> Deserialize<'de>>(
    store: &dyn ResultStore,
    reference: Reference,
) -> Result<T, String> {
    let bytes = store
        .get_content(reference)
        .ok_or("missing callable window record")?;
    if reference.domain != WINDOW_RECORD || WINDOW_RECORD.reference(&bytes) != Some(reference) {
        return Err("callable window record identity differs".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
#[derive(Serialize, Deserialize)]
struct Interface {
    abi: ContentHash,
    source: bool,
    op_free: bool,
    layouts: Reference,
}
#[derive(Serialize, Deserialize)]
enum Seeds {
    Parameters(CallableRecipe),
    Capture(TaskCaptureRecipe),
}
#[derive(Serialize, Deserialize)]
struct Selection {
    origin: usize,
    key: NamedKey,
    seeds: Option<Seeds>,
    layouts: Option<Reference>,
}
#[derive(Serialize, Deserialize)]
struct Rewritten {
    selection: Reference,
    value: Reference,
    inline: Vec<ContentHash>,
}
fn address_weight(w: &mut hirsel_query_products::retained::MetadataWeight, key: &InstanceAddress) {
    w.path(key.instance.definition());
    w.add(key.instance.retained_encoding_bytes());
}
fn select_invocations(
    instances: &DemandInstances,
    key: &InstanceAddress,
    records: &BTreeMap<hirsel_syntax::DefPath, Reference>,
    required: bool,
    operations: &mut hirsel_layout::operation::OperationRegistry,
) -> Result<(), String> {
    operations.invocations.clear();
    if let Some(reference) = records.get(key.instance.definition()) {
        operations.invocations.insert(
            key.instance.definition().clone(),
            instances.reopen(*reference)?,
        );
    } else if required {
        // Preserve the registry's strict structural-reference requirement even
        // for a library window. Its InstanceOperations admits no source calls.
        operations
            .invocations
            .insert(key.instance.definition().clone(), Vec::new());
    }
    Ok(())
}
/// Every specialized product of one scope is published under this action. It
/// names everything the rewrite reads besides the current environment, which
/// C's own lookup checks: each original's accepted product, the specializing
/// setting and the compiler. Any changed original changes every product's subject.
fn callable_action(
    originals: &[(InstanceAddress, StoredInstance)],
    options: &hirsel_native::Options,
) -> Result<Action, String> {
    let bytes = serde_json::to_vec(&(
        hirsel_compiler_identity::COMPILER_IDENTITY.as_slice(),
        originals
            .iter()
            .map(|(_, stored)| {
                stored
                    .product
                    .product
                    .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                    .map(|archive| archive.reference())
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        options.scoped_specialization,
    ))
    .map_err(|e| e.to_string())?;
    Ok(Action(ContentHash::of_parts(
        "hirsel.native.callable-scope-action.v1",
        [bytes.as_slice()],
    )))
}
fn resize(lease: &mut Option<Arc<window::WindowRead>>, bytes: u64) -> Result<(), String> {
    if let Some(lease) = lease {
        Arc::get_mut(lease)
            .ok_or("window charge unexpectedly shared")?
            .resize(bytes)?;
    }
    Ok(())
}
impl DemandInstances {
    pub(crate) fn layout_metadata(
        &self,
        reference: Reference,
        target: &hirsel_lir::TargetSpec,
    ) -> Result<Arc<hirsel_lir::products::CheckedLayouts>, String> {
        self.queries.layouts(reference, target)
    }
    fn json<T: Serialize>(&self, value: &T) -> Result<Reference, String> {
        self.put(
            WINDOW_RECORD,
            &serde_json::to_vec(value).map_err(|e| e.to_string())?,
        )
    }
    fn snapshot<T: hirsel_codec::Snapshot>(&self, value: &T) -> Result<Reference, String> {
        self.put(WINDOW_RECORD, hirsel_codec::to_text(value).as_bytes())
    }
    fn reopen<T: hirsel_codec::Snapshot>(&self, reference: Reference) -> Result<T, String> {
        let bytes = self
            .store
            .get_content(reference)
            .ok_or("missing callable interface")?;
        if reference.domain.reference(&bytes) != Some(reference) {
            return Err("callable interface identity differs".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        hirsel_codec::from_text(text).map_err(|e| format!("callable interface: {e:?}"))
    }
    fn callable_request(
        &self,
        reference: Reference,
    ) -> Result<(Request, Option<Arc<window::WindowRead>>), String> {
        let request: Request = read_json(self.store.as_ref(), reference)?;
        let charge = self
            .window
            .as_ref()
            .map(|window| window.read_for("request", request.retained_metadata_bytes()))
            .transpose()?;
        Ok((request, charge))
    }
    /// A whole scope published earlier under this action, admitted again
    /// without decoding or rewriting a Value. The stored closure is walked from
    /// the originals through each product's discovery requests; every request
    /// is an original's request under a key derived from it; and C's lookup
    /// accepts each product under the current environment, as for any hit.
    /// An ordinary association never accepts anything by itself. Anything
    /// absent or different is a miss and the scope is specialized again.
    #[allow(clippy::too_many_arguments)]
    fn stored_callables(
        &self,
        originals: &[(InstanceAddress, StoredInstance)],
        action: Action,
        options: &hirsel_native::Options,
        manifest: &hirsel_core_contract::Manifest,
        invocation_records: &BTreeMap<hirsel_syntax::DefPath, Reference>,
        structural_reference: bool,
        operations: &mut hirsel_layout::operation::OperationRegistry,
        source: &mut impl FnMut(
            &InstanceAddress,
            &mut hirsel_layout::operation::OperationRegistry,
        ) -> Result<(), String>,
    ) -> Result<Option<PublishedCallables>, String> {
        let original_request = |origin: usize, key: &InstanceKey| -> Option<Request> {
            let mut request = self
                .requests
                .get(originals[origin].0.instance.encoding())?
                .clone();
            Arc::make_mut(&mut request.0).key = Arc::new(NamedKey::of(key));
            Some(request)
        };
        // Each selected key, with the original it specializes and its request.
        let mut selected = BTreeMap::new();
        for (origin, (key, _)) in originals.iter().enumerate() {
            let Some(request) = original_request(origin, &key.instance) else {
                return Ok(None);
            };
            selected.insert(key.instance.encoding().to_vec(), (origin, request));
        }
        let mut publisher = self.queries.callable();
        let mut pending = originals
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<VecDeque<_>>();
        let mut candidates = BTreeMap::new();
        while let Some(key) = pending.pop_front() {
            if candidates.contains_key(key.instance.encoding()) {
                continue;
            }
            if candidates.len() >= originals.len() + hirsel_rc::closure_calls::MAX_SPECIALIZATIONS {
                return Ok(None);
            }
            let Some(product) = publisher.candidate(&key) else {
                return Ok(None);
            };
            if product.product.action != action {
                return Ok(None);
            }
            let Ok(discovery) = self.descriptor(&key, &product) else {
                return Ok(None);
            };
            for request in discovery.requests {
                let Ok(dependency) = request.key.bind() else {
                    return Ok(None);
                };
                match selected.get(&request.key.encoding) {
                    Some((_, previous)) if *previous != request => return Ok(None),
                    Some(_) => {}
                    None => {
                        let Some(origin) = originals.iter().position(|(original, _)| {
                            original.instance.definition() == dependency.definition()
                                && original.instance.body() == dependency.body()
                                && dependency.is_same_or_derived_from(&original.instance)
                        }) else {
                            return Ok(None);
                        };
                        if original_request(origin, &dependency).as_ref() != Some(&request) {
                            return Ok(None);
                        }
                        selected.insert(request.key.encoding.clone(), (origin, request));
                    }
                }
                pending.push_back(self.address(dependency));
            }
            candidates.insert(key.instance.encoding().to_vec(), (key, product));
        }
        let mut rows = BTreeMap::new();
        for (encoding, (key, candidate)) in candidates {
            let (origin, request) = &selected[&encoding];
            let (original, stored) = &originals[*origin];
            select_invocations(
                self,
                original,
                invocation_records,
                structural_reference,
                operations,
            )?;
            if source(&key, operations).is_err() {
                return Ok(None);
            }
            let adapters = stored.adapters(self)?;
            let environment = NativeEnvironment {
                target: &self.target,
                manifest,
                operations,
                discharge: Some(&options.discharge),
            };
            let Ok(scoped) = self.operations_input(&key, &adapters, request, &environment) else {
                return Ok(None);
            };
            let Some(product) = publisher.lookup(
                &key,
                action,
                &NativeEnvironment {
                    operations: &scoped,
                    ..environment
                },
            )?
            else {
                return Ok(None);
            };
            if product.product != candidate.product {
                return Ok(None);
            }
            let Ok(output) = self.decode_completed(&key, product, adapters) else {
                return Ok(None);
            };
            rows.insert(
                key.address(hirsel_native::query_model::CALLABLE_VALUE_KIND)
                    .0,
                self.json(&Row {
                    key: NamedKey::of(&key.instance),
                    origin: NamedKey::of(&original.instance),
                    request: request.clone(),
                    output: StoredInstance::capture(self.store.as_ref(), &output)?.record()?,
                })?,
            );
        }
        let mut w = hirsel_query_products::retained::MetadataWeight::default();
        w.map(&rows, |_, _, _| ());
        let index = self
            .window
            .as_ref()
            .map(|window| window.read_for("published-index", w.bytes()))
            .transpose()?;
        Ok(Some(PublishedCallables {
            records: rows,
            _index: index,
            target: self.target.digest(),
            policy: self.policy,
        }))
    }
    pub(crate) fn specialize_callables(
        &mut self,
        options: &hirsel_native::Options,
        manifest: &hirsel_core_contract::Manifest,
        mut operations: hirsel_layout::operation::OperationRegistry,
        mut source: impl FnMut(
            &InstanceAddress,
            &mut hirsel_layout::operation::OperationRegistry,
        ) -> Result<(), String>,
    ) -> Result<PublishedCallables, String> {
        let structural_reference = !operations.invocations.is_empty();
        let invocation_records = std::mem::take(&mut operations.invocations)
            .into_iter()
            .map(|(key, rows)| Ok((key, self.snapshot(&rows)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut invocation_weight = hirsel_query_products::retained::MetadataWeight::default();
        invocation_weight.map(&invocation_records, |w, path, _| w.path(path));
        let mut originals = self.completed_inputs()?;
        originals.sort_by(|a, b| a.0.canonical_cmp(&b.0));
        let action = callable_action(&originals, options)?;
        if self.verify_store != hirsel_engine_store::VerifyStore::Recompute
            && options.validation != hirsel_native::Validation::EveryStage
            && let Some(published) = self.stored_callables(
                &originals,
                action,
                options,
                manifest,
                &invocation_records,
                structural_reference,
                &mut operations,
                &mut source,
            )?
        {
            self.requests.clear();
            self.completed.clear();
            return Ok(published);
        }
        // The original request snapshot has finished discovery and receipt
        // comparison. Keep ordinary addressed requests, reopen one at its next
        // consumer, and release the duplicate decoded maps before window work.
        let request_records = originals
            .iter()
            .map(|(key, _)| {
                let request = self
                    .requests
                    .get(key.instance.encoding())
                    .ok_or("callable original request is missing")?;
                self.json(request)
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.requests.clear();
        self.completed.clear();
        let index_bytes = self.retained_index_bytes() + invocation_weight.bytes();
        let mut graph_charge = self
            .window
            .as_ref()
            .map(|w| w.read_for("callable-index", index_bytes))
            .transpose()?;
        let mut operations_charge = self
            .window
            .as_ref()
            .map(|w| {
                w.read_for(
                    "operations",
                    super::super::ownership_compilation::metadata::operations(&mut operations),
                )
            })
            .transpose()?;
        if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
            eprintln!(
                "NATIVE_WINDOW_OWNERS input-index {index_bytes} operations {}",
                operations_charge.as_ref().map_or(0, |c| c.bytes())
            );
        }
        let index = originals
            .iter()
            .enumerate()
            .map(|(i, (key, _))| {
                (
                    (key.instance.definition().clone(), key.instance.digest()),
                    i,
                )
            })
            .collect::<BTreeMap<_, _>>();
        // The only graph-wide facts are identities and edges. Decoded equations
        // are reopened one SCC at a time, then replaced by their boolean summary.
        let mut edges = Vec::new();
        for (key, stored) in &originals {
            let output = stored
                .read(self, key)
                .map_err(|e| format!("callable graph edges: {e}"))?;
            edges.push(
                output
                    .facts
                    .body
                    .operation_freedom
                    .dependencies()
                    .iter()
                    .filter_map(|name| index.get(name).copied())
                    .collect::<Vec<_>>(),
            );
        }
        let components = hirsel_graph::components(&edges);
        let mut graph_weight = hirsel_query_products::retained::MetadataWeight::default();
        graph_weight.flat_vec(&request_records);
        graph_weight.vec(&originals, |w, (key, stored)| {
            address_weight(w, key);
            w.add(
                stored
                    .retained_bytes()
                    .saturating_sub(size_of::<StoredInstance>() as u64) as usize,
            );
        });
        graph_weight.map(&index, |w, (path, _), _| w.path(path));
        graph_weight.vec(&edges, |w, edges| w.flat_vec(edges));
        graph_weight.vec(&components, |w, members| w.flat_vec(members));
        let graph_bytes = index_bytes + graph_weight.bytes();
        resize(&mut graph_charge, graph_bytes)?;
        if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
            eprintln!(
                "NATIVE_WINDOW_OWNERS SCC-graph {graph_bytes} operations {}",
                operations_charge.as_ref().map_or(0, |c| c.bytes())
            );
        }
        let mut freedom = vec![None; originals.len()];
        let mut largest_scc = 0;
        let mut largest_members = 0;
        for members in &components {
            let mut equations = BTreeMap::new();
            let mut scc_demand = 0;
            let mut equation_charge = self
                .window
                .as_ref()
                .map(|w| w.read_for("scc", 0))
                .transpose()?;
            for &i in members {
                let (key, stored) = &originals[i];
                let output = stored.read(self, key).map_err(|e| {
                    format!(
                        "callable SCC {} members {:?}: {e}",
                        members.len(),
                        members
                            .iter()
                            .map(|&i| originals[i].0.instance.definition().to_string())
                            .collect::<Vec<_>>()
                    )
                })?;
                scc_demand = scc_demand.max(
                    equation_charge.as_ref().map_or(0, |c| c.bytes())
                        + output.retained_metadata_bytes(),
                );
                equations.insert(
                    (key.instance.definition().clone(), key.instance.digest()),
                    output.facts.body.operation_freedom.clone(),
                );
                // The equation is the last consumer of this decoded record.
                // An SCC keeps equations, not every member's unrelated facts.
                drop(output);
                let mut equation_weight =
                    hirsel_query_products::retained::MetadataWeight::default();
                equation_weight.map(&equations, |w, (path, _), facts| {
                    w.path(path);
                    w.add(facts.retained_bytes() as usize)
                });
                resize(&mut equation_charge, equation_weight.bytes()).map_err(|error| {
                    format!(
                        "callable SCC {} members {:?}: {error}",
                        members.len(),
                        members
                            .iter()
                            .map(|&i| originals[i].0.instance.definition().to_string())
                            .collect::<Vec<_>>()
                    )
                })?;
            }
            scc_demand = scc_demand.max(equation_charge.as_ref().map_or(0, |c| c.bytes()));
            let outgoing = equations
                .values()
                .flat_map(|equation| equation.dependencies())
                .filter(|name| !equations.contains_key(*name))
                .filter_map(|name| {
                    index
                        .get(name)
                        .and_then(|&i| freedom[i])
                        .map(|safe| (name.clone(), safe))
                })
                .collect::<BTreeMap<_, _>>();
            let mut w = hirsel_query_products::retained::MetadataWeight::default();
            w.map(&equations, |w, (path, _), facts| {
                w.path(path);
                w.add(facts.retained_bytes() as usize);
            });
            w.map(&outgoing, |w, (path, _), _| w.path(path));
            resize(&mut equation_charge, w.bytes())?;
            let solved = hirsel_rc::closure_calls::settle_operation_freedom(&equations, &outgoing)?;
            let mut solved_weight = hirsel_query_products::retained::MetadataWeight::default();
            solved_weight.map(&solved, |w, (path, _), _| w.path(path));
            let _solved_charge = self
                .window
                .as_ref()
                .map(|window| window.read_for("scc", solved_weight.bytes()))
                .transpose()?;
            scc_demand = scc_demand.max(
                equation_charge.as_ref().map_or(0, |c| c.bytes())
                    + _solved_charge.as_ref().map_or(0, |c| c.bytes()),
            );
            if scc_demand > largest_scc || members.len() > largest_members {
                largest_scc = largest_scc.max(scc_demand);
                largest_members = largest_members.max(members.len());
                if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
                    eprintln!(
                        "NATIVE_SCC_DEMAND bytes={scc_demand} members={} names={:?}",
                        members.len(),
                        members
                            .iter()
                            .map(|&i| originals[i].0.instance.definition().to_string())
                            .collect::<Vec<_>>()
                    );
                }
            }
            for (name, safe) in solved {
                freedom[*index.get(&name).ok_or("missing callable SCC member")?] = Some(safe);
            }
        }
        let mut freedom_weight = hirsel_query_products::retained::MetadataWeight::default();
        freedom_weight.flat_vec(&freedom);
        let graph_bytes = graph_bytes + freedom_weight.bytes();
        resize(&mut graph_charge, graph_bytes)?;
        if std::env::var_os("HIRSEL_NATIVE_RETENTION").is_some() {
            eprintln!(
                "NATIVE_WINDOW_OWNERS signatures {graph_bytes} operations {}",
                operations_charge.as_ref().map_or(0, |c| c.bytes())
            );
        }
        let mut interfaces = Vec::new();
        for (i, (key, stored)) in originals.iter().enumerate() {
            select_invocations(
                self,
                key,
                &invocation_records,
                structural_reference,
                &mut operations,
            )?;
            resize(
                &mut operations_charge,
                super::super::ownership_compilation::metadata::operations(&mut operations),
            )?;
            let env = NativeEnvironment {
                target: &self.target,
                manifest,
                operations: &operations,
                discharge: Some(&options.discharge),
            };
            let (request, _request_charge) = self.callable_request(request_records[i])?;
            let (output, linkage) = self
                .admitted_input(key, stored, &request, &env)
                .map_err(|e| format!("callable signatures: {e}"))?;
            interfaces.push(self.json(&Interface {
                abi: linkage.abi(),
                source: !output.facts.body.templates.library,
                op_free: freedom[i].ok_or("unsettled callable interface")?,
                layouts: {
                    let closure = self
                        .queries
                        .layouts(stored.product.layout_closure, &self.target)?;
                    let binding = closure.binding();
                    let abi = binding
                        .abi(linkage.abi())
                        .ok_or("missing callable signature ABI")?;
                    self.snapshot(&LayoutClosure::select(
                        &binding.table,
                        &self.target,
                        &[],
                        &[abi],
                    )?)?
                },
            })?);
        }
        drop(freedom);
        // Originals are rewritten callee-first; a field summary is visible to
        // a caller in another component, and to every specialization.
        let mut component = vec![0usize; originals.len()];
        for (c, members) in components.iter().enumerate() {
            for &m in members {
                component[m] = c;
            }
        }
        let mut summaries: Vec<Option<FieldSummary>> = vec![None; originals.len()];
        let mut pending = VecDeque::new();
        for members in &components {
            for &origin in members {
                pending.push_back(self.json(&Selection {
                    origin,
                    key: NamedKey::of(&originals[origin].0.instance),
                    seeds: None,
                    layouts: None,
                })?);
            }
        }
        drop(components);
        drop(edges);
        let mut graph_weight = hirsel_query_products::retained::MetadataWeight::default();
        graph_weight.flat_vec(&request_records);
        graph_weight.vec(&originals, |w, (key, stored)| {
            address_weight(w, key);
            w.add(
                stored
                    .retained_bytes()
                    .saturating_sub(size_of::<StoredInstance>() as u64) as usize,
            );
        });
        graph_weight.map(&index, |w, (path, _), _| w.path(path));
        let graph_bytes = index_bytes + graph_weight.bytes();
        let mut counts = vec![0usize; originals.len()];
        let mut selected = BTreeSet::new();
        let mut rewritten = Vec::new();
        while let Some(selection_record) = pending.pop_front() {
            let mut w = hirsel_query_products::retained::MetadataWeight::default();
            w.flat_vec(&interfaces);
            w.flat_vec(&counts);
            w.tree::<ContentHash, ()>(selected.len());
            w.add(pending.capacity() * size_of::<Reference>());
            w.flat_vec(&rewritten);
            w.flat_vec(&component);
            w.add(
                summaries.capacity() * size_of::<Option<FieldSummary>>()
                    + summaries
                        .iter()
                        .flatten()
                        .map(FieldSummary::retained_bytes)
                        .sum::<usize>(),
            );
            resize(&mut graph_charge, graph_bytes + w.bytes())?;
            let selection: Selection = read_json(self.store.as_ref(), selection_record)?;
            let key = self.address(selection.key.bind()?);
            let origin = selection.origin;
            let (original, stored) = &originals[origin];
            let output = stored
                .read(self, original)
                .map_err(|e| format!("callable rewrite: {e}"))?;
            select_invocations(
                self,
                original,
                &invocation_records,
                structural_reference,
                &mut operations,
            )?;
            resize(
                &mut operations_charge,
                super::super::ownership_compilation::metadata::operations(&mut operations),
            )?;
            let env = NativeEnvironment {
                target: &self.target,
                manifest,
                operations: &operations,
                discharge: Some(&options.discharge),
            };
            let (request, _request_charge) = self.callable_request(request_records[origin])?;
            let scoped =
                self.operations_input(original, &output.facts.source.adapters, &request, &env)?;
            let (closure, body) = self.queries.read(
                original,
                &output.product,
                &NativeEnvironment {
                    operations: &scoped,
                    ..env
                },
            )?;
            let own = closure.binding();
            let original_unit = own.materialize::<Value>(&body)?;
            if hirsel_rc::closure_calls::OperationFreedom::extract(&original_unit, &own.table)
                != output.facts.body.operation_freedom
            {
                return Err(
                    "callable operation-freedom equations differ from accepted Value".into(),
                );
            }
            let neighbours = original_unit
                .tables
                .symbols
                .iter()
                .filter_map(|symbol| {
                    symbol
                        .instance
                        .and_then(|id| index.get(&(symbol.definition.clone(), id)).copied())
                })
                .collect::<BTreeSet<_>>();
            drop(original_unit);
            let mut closures = vec![closure];
            let mut declarations = Vec::new();
            for &i in &neighbours {
                let interface: Interface = read_json(self.store.as_ref(), interfaces[i])?;
                closures.push(Arc::new(hirsel_lir::products::CheckedLayouts::new(
                    self.reopen::<LayoutClosure>(interface.layouts)?,
                    &self.target,
                )?));
                declarations.push((i, interface));
            }
            if let Some(layouts) = selection.layouts {
                closures.push(Arc::new(hirsel_lir::products::CheckedLayouts::new(
                    self.reopen(layouts)?,
                    &self.target,
                )?));
            }
            let binding = Binding::from_checked(&self.target, closures.into_iter().map(Ok))?;
            let mut unit = binding.materialize::<Value>(&body)?;
            unit.header.instance = key.instance.digest();
            let mut entries = HashMap::new();
            for (i, interface) in declarations {
                let original = &originals[i].0;
                let fields = match &summaries[i] {
                    Some(summary)
                        if selection.seeds.is_some() || component[i] != component[origin] =>
                    {
                        summary.clone()
                    }
                    _ => FieldSummary::default(),
                };
                entries.insert(
                    Symbol {
                        kind: SymbolKind::Function,
                        definition: original.instance.definition().clone(),
                        instance: Some(original.instance.digest()),
                        charged: original.instance.uncharged().is_some(),
                    },
                    CallableEntry {
                        key: original.instance.clone(),
                        abi: binding
                            .abi(interface.abi)
                            .ok_or("missing window interface ABI")?,
                        origin: if interface.source {
                            UnitOrigin::Source
                        } else {
                            UnitOrigin::Library
                        },
                        op_free: interface.op_free,
                        charged: original.instance.uncharged().is_some(),
                        fields,
                    },
                );
            }
            let original_selection = selection.seeds.is_none();
            let (seeds, flags) = match selection.seeds {
                None => (Vec::new(), 0),
                Some(Seeds::Parameters(recipe)) => {
                    let request = recipe.bind(&key.instance, &binding)?;
                    (
                        hirsel_rc::closure_calls::bind_seeds(&unit, &request.seeds)?,
                        request.spawn_flags,
                    )
                }
                Some(Seeds::Capture(recipe)) => {
                    let request = recipe.bind(&key.instance, &binding)?;
                    (
                        vec![(
                            *unit
                                .captures
                                .first()
                                .ok_or("task_main has no callable capture")?,
                            Vec::new(),
                            request.target,
                        )],
                        0,
                    )
                }
            };
            // Requests carry portable seeds and their reached ABI closure. They
            // never retain a cloned Value or the caller's numeric binding table.
            let additions = std::cell::RefCell::new(Vec::new());
            let mut enqueue = |entry: &CallableEntry,
                               derived: InstanceKey,
                               seeds: Result<Seeds, String>|
             -> Result<bool, String> {
                let owner = *index
                    .get(&(entry.key.definition().clone(), entry.key.digest()))
                    .ok_or("callable request has no original")?;
                if selected.contains(&derived.digest()) {
                    return Ok(true);
                }
                if counts[owner] >= hirsel_rc::closure_calls::MAX_PER_FUNCTION
                    || selected.len() >= hirsel_rc::closure_calls::MAX_SPECIALIZATIONS
                {
                    return Ok(false);
                }
                let seeds = seeds?;
                let selected_layouts = self.snapshot(&LayoutClosure::select(
                    &binding.table,
                    &self.target,
                    &[],
                    &(0..binding.table.fn_abis.len())
                        .map(|i| hirsel_lir::FnAbiId(i as u32))
                        .collect::<Vec<_>>(),
                )?)?;
                let derived = self.address(derived);
                let record = self.json(&Selection {
                    origin: owner,
                    key: NamedKey::of(&derived.instance),
                    seeds: Some(seeds),
                    layouts: Some(selected_layouts),
                })?;
                selected.insert(derived.instance.digest());
                counts[owner] += 1;
                additions.borrow_mut().push(record);
                Ok(true)
            };
            let enqueue = std::cell::RefCell::new(&mut enqueue);
            let failure = std::cell::RefCell::new(None);
            let (inline, summary) = hirsel_rc::closure_calls::rewrite_window(
                &mut unit,
                &binding.table,
                &entries,
                &seeds,
                flags,
                |entry, request| match (enqueue.borrow_mut())(
                    entry,
                    request.key.clone(),
                    TaskCaptureRecipe::capture(&entry.key, request, &binding.table)
                        .map(Seeds::Capture),
                ) {
                    Ok(value) => value,
                    Err(e) => {
                        *failure.borrow_mut() = Some(e);
                        false
                    }
                },
                |entry, request| match (enqueue.borrow_mut())(
                    entry,
                    request.key.clone(),
                    CallableRecipe::capture(&entry.key, request, &binding.table)
                        .map(Seeds::Parameters),
                ) {
                    Ok(value) => value,
                    Err(e) => {
                        *failure.borrow_mut() = Some(e);
                        false
                    }
                },
            )?;
            if let Some(error) = failure.into_inner() {
                return Err(error);
            }
            pending.extend(additions.into_inner());
            if original_selection {
                summaries[origin] = Some(summary);
            }
            // Ordinary staging holds the transformed body until the dependency
            // identities close. Only publication below grants C acceptance.
            let value = self.snapshot(&(binding.table.into_table(), unit))?;
            rewritten.push(self.json(&Rewritten {
                selection: selection_record,
                value,
                inline: inline.into_iter().collect(),
            })?);
        }
        drop(pending);
        let mut keys = BTreeMap::new();
        for reference in &rewritten {
            let row: Rewritten = read_json(self.store.as_ref(), *reference)?;
            let selection: Selection = read_json(self.store.as_ref(), row.selection)?;
            let key = selection.key.bind()?;
            if keys
                .insert((key.definition().clone(), key.digest()), row.selection)
                .is_some()
            {
                return Err("duplicate callable window selection".into());
            }
        }
        let request =
            |selection: &Selection| -> Result<(Request, Option<Arc<window::WindowRead>>), String> {
                let reference = request_records
                    .get(selection.origin)
                    .ok_or("callable selection has no original")?;
                let (mut request, mut charge) = self.callable_request(*reference)?;
                Arc::make_mut(&mut request.0).key = Arc::new(selection.key.clone());
                resize(&mut charge, request.retained_metadata_bytes())?;
                Ok((request, charge))
            };
        let mut w = hirsel_query_products::retained::MetadataWeight::default();
        w.flat_vec(&rewritten);
        w.map(&keys, |w, (path, _), _| w.path(path));
        w.flat_vec(&interfaces);
        w.flat_vec(&counts);
        w.tree::<ContentHash, ()>(selected.len());
        resize(&mut graph_charge, graph_bytes + w.bytes())?;
        let mut rows = BTreeMap::new();
        let mut publisher = self.queries.callable();
        for reference in rewritten {
            let row: Rewritten = read_json(self.store.as_ref(), reference)?;
            let selection: Selection = read_json(self.store.as_ref(), row.selection)?;
            let key = self.address(selection.key.bind()?);
            let (selected_request, _selected_request_charge) = request(&selection)?;
            let origin = selection.origin;
            select_invocations(
                self,
                &originals[origin].0,
                &invocation_records,
                structural_reference,
                &mut operations,
            )?;
            source(&key, &mut operations)?;
            resize(
                &mut operations_charge,
                super::super::ownership_compilation::metadata::operations(&mut operations),
            )?;
            let original = originals[origin]
                .1
                .read(self, &originals[origin].0)
                .map_err(|e| format!("callable publication: {e}"))?;
            let (table, unit): (hirsel_lir::LayoutTable, hirsel_lir::LirUnit<Value>) =
                self.reopen(row.value)?;
            let inline = super::super::emission::InlineInstances::new(
                row.inline,
                self.retained_owner.clone(),
            );
            let inline_reference = inline.publish(self.store.as_ref())?;
            let callees = unit
                .tables
                .calls
                .iter()
                .filter_map(|call| match call.callee {
                    Callee::Direct(s) => {
                        let s = &unit.tables.symbols[s.index()];
                        s.instance.map(|id| (s.definition.clone(), id))
                    }
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let templates = super::super::optimizer::TemplateFacts::of(
                &unit,
                original
                    .facts
                    .body
                    .dependencies
                    .iter()
                    .map(|k| (k.instance.definition().clone(), k.instance.digest())),
            );
            let mut dependencies = templates
                .references
                .iter()
                .filter_map(|name| keys.get(name))
                .map(|reference| {
                    let selection: Selection = read_json(self.store.as_ref(), *reference)?;
                    Ok(self.address(selection.key.bind()?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            dependencies.sort_by(InstanceAddress::canonical_cmp);
            dependencies.dedup();
            let seal_demand =
                hirsel_rc::seal_demand::equations::LocalDemandEquations::extract(&unit)?;
            let operation_freedom =
                hirsel_rc::closure_calls::OperationFreedom::extract(&unit, &table);
            let mut emission = super::super::emission::EmissionFacts::extract(&unit, []);
            emission.inline = inline;
            let discovery_requests = dependencies
                .iter()
                .map(|key| {
                    let name = (key.instance.definition().clone(), key.instance.digest());
                    let selection: Selection = read_json(self.store.as_ref(), keys[&name])?;
                    request(&selection)
                })
                .collect::<Result<Vec<_>, String>>()?;
            let discovery = self.put(
                INSTANCE_DISCOVERY_DOMAIN,
                &serde_json::to_vec(&Discovery {
                    schema: INSTANCE_DISCOVERY_DOMAIN.schema,
                    owner: NamedKey::of(&key.instance),
                    ready_source: original.facts.source.ready_source.encode()?,
                    seal_demand: seal_demand.clone(),
                    callees: callees.clone(),
                    requests: discovery_requests
                        .iter()
                        .map(|(request, _)| request.clone())
                        .collect(),
                    selection_reads: original.facts.source.selection_reads.clone(),
                    constant_reads: original.facts.source.constant_reads.clone(),
                    templates: templates.clone(),
                    operation_freedom: operation_freedom.clone(),
                    emission: super::super::emission::EmissionRecord::new(
                        &emission,
                        inline_reference,
                    )?,
                    discharge: original.facts.source.discharge.clone(),
                })
                .map_err(|e| e.to_string())?,
            )?;
            drop(discovery_requests);
            let (statics, finalizers): (
                super::super::local::StaticRows,
                super::super::local::FinalizerRows,
            ) = serde_json::from_slice(
                &self
                    .store
                    .get_content(original.product.statics_finalizers)
                    .ok_or("missing callable static/finalizer input")?,
            )
            .map_err(|e| e.to_string())?;
            let layout_keys = LayoutKeys::new(&table);
            let mut additional_abis = finalizers
                .iter()
                .map(|row| {
                    (0..table.fn_abis.len())
                        .map(|i| hirsel_lir::FnAbiId(i as u32))
                        .find(|id| layout_keys.fn_abi(&table.fn_abis[id.0 as usize]) == row.3)
                        .ok_or("missing callable finalizer ABI")
                })
                .collect::<Result<Vec<_>, _>>()?;
            for key in statics.iter().flat_map(|(_, abis, _)| abis) {
                let abi = (0..table.fn_abis.len())
                    .map(|i| hirsel_lir::FnAbiId(i as u32))
                    .find(|id| layout_keys.fn_abi(&table.fn_abis[id.0 as usize]) == *key)
                    .ok_or("missing callable static ABI")?;
                additional_abis.push(abi);
            }
            let original_request: Request =
                read_json(self.store.as_ref(), request_records[origin])?;
            let original_operations = self.operations_input(
                &originals[origin].0,
                &originals[origin].1.adapters(self)?,
                &original_request,
                &NativeEnvironment {
                    target: &self.target,
                    manifest,
                    operations: &operations,
                    discharge: Some(&options.discharge),
                },
            )?;
            let pretransfer = super::super::instance_operations::bind_pretransfer(
                &original_operations,
                &table,
                &self.target,
            )?;
            additional_abis.extend(pretransfer.iter().map(|row| row.abi));
            let selected_operations = super::super::instance_operations::InstanceOperations {
                inventory: Some(&self.seal_inventory),
                declarations: &operations,
                receipts: Some(&self.receipt_sources),
                source: selected_request
                    .source_definition()
                    .map(|_| (key.instance.definition(), key.instance.digest())),
                all_sources: false,
                pretransfer: Some(
                    original
                        .facts
                        .source
                        .adapters
                        .iter()
                        .map(|symbol| {
                            let mut rows = pretransfer.iter().filter(|row| &row.adapter == symbol);
                            let selected = rows.next().ok_or(
                                "callable publication lost an independently planned adapter",
                            )?;
                            if rows.next().is_some() {
                                return Err(
                                    "callable publication repeats an independently planned adapter",
                                );
                            }
                            Ok(selected.clone())
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            };
            let inputs = Inputs {
                memo: Default::default(),
                target: &self.target,
                layouts: &table,
                operations: &selected_operations,
                manifest,
                discharge: Some(&options.discharge),
            };
            let (_, product) = publisher.publish(
                &key,
                action,
                unit,
                &inputs,
                &additional_abis,
                discovery,
                original.product.statics_finalizers,
            )?;
            let output = DiscoveredInstance::with_facts(
                product,
                InstanceFacts {
                    source: original.facts.source.clone(),
                    body: Arc::new(
                        InstanceBodyFacts {
                            seal_demand: StoredSealEquations::capture(
                                self.store.clone(),
                                discovery,
                                seal_demand,
                            )?,
                            callees,
                            dependencies,
                            templates,
                            operation_freedom,
                            _owner: self.retained_owner.clone(),
                            _window: None,
                        }
                        .packed(),
                    ),
                    emission,
                    _owner: self.retained_owner.clone(),
                    _window: None,
                },
                Some(&original),
            );
            rows.insert(
                key.address(hirsel_native::query_model::CALLABLE_VALUE_KIND)
                    .0,
                self.json(&Row {
                    key: NamedKey::of(&key.instance),
                    origin: NamedKey::of(&originals[origin].0.instance),
                    request: selected_request.clone(),
                    output: StoredInstance::capture(self.store.as_ref(), &output)?.record()?,
                })?,
            );
        }
        let mut w = hirsel_query_products::retained::MetadataWeight::default();
        w.map(&rows, |_, _, _| ());
        let index = self
            .window
            .as_ref()
            .map(|window| window.read_for("published-index", w.bytes()))
            .transpose()?;
        Ok(PublishedCallables {
            records: rows,
            _index: index,
            target: self.target.digest(),
            policy: self.policy,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_callable_facts_share_backing_and_changed_dependencies_remain_distinct() {
        let types = hirsel_types::TypeInterner::new(None);
        let path = hirsel_syntax::DefPath::core("facts");
        let key = InstanceKey::new(&path, 0, &[], &types, None, None).unwrap();
        let equations: hirsel_rc::seal_demand::equations::LocalDemandEquations =
            serde_json::from_value(serde_json::json!({
                "schema": 1, "owner": (&path, key.digest()), "values": 1025,
                "parameters": [0], "captures": [],
                "flow": (0..1024).map(|i| (i + 1, i)).collect::<Vec<_>>(),
                "closures": [], "calls": [], "returns": [1024], "seals": [1024]
            }))
            .unwrap();
        let store: Arc<dyn ResultStore> = Arc::new(hirsel_engine_store::MemStore::default());
        let bytes = serde_json::to_vec(&serde_json::json!({"seal_demand": &equations})).unwrap();
        let record = store
            .put_content(INSTANCE_DISCOVERY_DOMAIN, &bytes)
            .unwrap();
        let stored =
            StoredSealEquations::capture(store.clone(), record, equations.clone()).unwrap();
        assert_eq!(stored.open().unwrap(), equations);
        let emission_record = store
            .put_content(
                INSTANCE_DISCOVERY_DOMAIN,
                &serde_json::to_vec(
                    &serde_json::json!({"seal_demand": &equations, "emission": true}),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            stored,
            StoredSealEquations::capture(store.clone(), emission_record, equations.clone())
                .unwrap()
        );
        let changed = serde_json::from_value(serde_json::json!({
            "schema": 1, "owner": (&path, key.digest()), "values": 1025,
            "parameters": [0], "captures": [], "flow": [], "closures": [],
            "calls": [], "returns": [1024], "seals": []
        }))
        .unwrap();
        assert!(
            StoredSealEquations::capture(store, record, changed)
                .unwrap()
                .open()
                .is_err()
        );
        let product = Arc::new(InstanceProduct {
            product: crate::fixture_product_bytes(
                hirsel_engine_store::Address(record.output.0),
                Action(record.output.0),
                record.domain,
                &bytes,
            ),
            value: record,
            layout_closure: record,
            local_equations: record,
            discovery: record,
            statics_finalizers: record,
        });
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let owner = Arc::new(budget.reserve(1 << 20).unwrap());
        let source = Arc::new(
            InstanceSourceFacts {
                _owner: Some(owner.clone()),
                _window: None,
                selection_reads: Vec::new(),
                constant_reads: Vec::new(),
                discharge: Default::default(),
                adapters: Vec::new(),
                ready_source: ReadySourceFacts::decode(
                    &serde_json::to_vec(&(
                        1u32,
                        &path,
                        0u32,
                        key.encoding(),
                        0u32,
                        false,
                        Vec::<u32>::new(),
                    ))
                    .unwrap(),
                )
                .unwrap(),
            }
            .packed(),
        );
        let facts = InstanceFacts {
            _owner: Some(owner.clone()),
            _window: None,
            source,
            body: Arc::new(
                InstanceBodyFacts {
                    seal_demand: stored,
                    callees: Vec::new(),
                    dependencies: Vec::new(),
                    templates: hirsel_native_cache::optimizer::TemplateFacts {
                        small: true,
                        execution: false,
                        library: false,
                        references: Vec::new(),
                    },
                    operation_freedom: serde_json::from_value(serde_json::json!({
                        "locally_safe": true, "callees": []
                    }))
                    .unwrap(),
                    _owner: Some(owner.clone()),
                    _window: None,
                }
                .packed(),
            ),
            emission: crate::emission::EmissionFacts {
                inline: crate::emission::InlineInstances::new(
                    [record.output.0],
                    Some(owner.clone()),
                ),
                ..Default::default()
            },
        };
        assert!(
            facts.body.retained_metadata_bytes() < equations.retained_bytes(),
            "completed facts release decoded seal graph allocations"
        );
        drop(owner);
        let original = DiscoveredInstance::with_facts(product.clone(), facts.clone(), None);
        let unchanged =
            DiscoveredInstance::with_facts(product.clone(), facts.clone(), Some(&original));
        assert!(Arc::ptr_eq(&original.facts, &unchanged.facts));
        let mut changed_emission = facts.clone();
        changed_emission.emission.tracking = true;
        let emission_only =
            DiscoveredInstance::with_facts(product.clone(), changed_emission, Some(&original));
        assert!(!Arc::ptr_eq(&original.facts, &emission_only.facts));
        assert!(Arc::ptr_eq(&original.facts.body, &emission_only.facts.body));
        drop(emission_only);
        let facts_before_edit = original.facts.clone();
        let mut edited = facts;
        Arc::make_mut(&mut edited.body)
            .templates
            .references
            .push((path, record.output.0));
        let changed = DiscoveredInstance::with_facts(product, edited, Some(&original));
        assert!(!Arc::ptr_eq(&original.facts, &changed.facts));
        assert!(Arc::ptr_eq(&original.facts.source, &changed.facts.source));
        let changed = Arc::new(changed);
        drop(original);
        assert!(facts_before_edit.body.templates.references.is_empty());
        assert_eq!(changed.facts.body.templates.references.len(), 1);
        let retained_source = changed.facts.source.clone();
        let retained_body = changed.facts.body.clone();
        drop(changed);
        drop(unchanged);
        drop(facts_before_edit);
        assert_eq!(
            budget.reserved(),
            1 << 20,
            "the small source view retains its complete original grant"
        );
        drop(retained_source);
        assert_eq!(
            budget.reserved(),
            1 << 20,
            "the body view also retains the complete grant"
        );
        drop(retained_body);
        assert_eq!(budget.reserved(), 0);
    }
}
