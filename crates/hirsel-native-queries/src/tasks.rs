//! Native Ready continuations over an immutable admitted input snapshot.
//! Input tasks re-admit the already produced base/SCC descriptors; source
//! discovery and their cold producers still precede this snapshot boundary.
use super::{
    products::{DemandProducts, NativeSnapshot, SettledProducts},
    *,
};
use hirsel_engine::store::ResultStore;
use hirsel_engine_runtime::*;

use hirsel_native::object_queries::CodegenKey;
use hirsel_native::object_queries::LinkDependencies;
use hirsel_native::object_queries::LinkInputs;
use hirsel_native::object_queries::LinkKey;
use hirsel_native::object_queries::ObjectProducts;
use hirsel_native::object_queries::ObjectQueries;
use hirsel_native::object_queries::ObjectTaskRequest;
use hirsel_native::object_queries::ObjectTaskSnapshot;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    task::Poll,
};

const FAILURE: hirsel_engine::store::Domain = hirsel_engine::store::Domain {
    kind: 0x4e00_001e,
    schema: 1,
};
type Id = (u32, ContentHash);
fn id(key: QueryId) -> Id {
    (key.kind, key.address.0)
}
fn query(kind: u32, product: ProductRef) -> QueryId {
    QueryId {
        kind,
        address: product.address,
    }
}
fn owner(key: &InstanceAddress) -> hirsel_rc::arena_credit::equations::Function {
    (key.instance.definition().clone(), key.instance.digest())
}
enum Work {
    Instance(InstanceAddress),
    Arena(ComponentKey),
    Ready(InstanceAddress),
}
#[derive(Serialize, Deserialize)]
enum WorkIndex {
    Instance(ContentHash),
    Arena(usize),
    Ready(ContentHash),
}
#[derive(Serialize, Deserialize)]
struct RequestRecipe {
    work: WorkIndex,
    reads: Vec<(u32, ContentHash, Reference)>,
    guard: Option<(u32, ContentHash)>,
}
struct Request {
    work: Work,
    reads: Vec<(QueryId, Reference)>,
    guard: Option<QueryId>,
}
impl Request {
    fn retained_bytes(&self) -> u64 {
        let mut w = hirsel_engine::NativeMetadataWeight::default();
        w.add(size_of::<Self>());
        w.flat_vec(&self.reads);
        match &self.work {
            Work::Instance(key) | Work::Ready(key) => {
                super::ownership_compilation::metadata::address(&mut w, key)
            }
            Work::Arena(key) => super::ownership_compilation::metadata::component(&mut w, key),
        }
        w.bytes()
    }
}
/// Immutable dispatch/read plans, built during snapshot admission. Factory
/// creation below performs only a bounded lookup and boxes one continuation.
pub struct NativeTasks {
    pub snapshot: Arc<NativeSnapshot>,
    requests: BTreeMap<Id, Reference>,
    weight: MemoryWeight,
    fallback: hirsel_engine::store::Reference,
    pub created: AtomicU64,
    pub ready_transformations: AtomicU64,
}
impl NativeTasks {
    pub fn retained_dispatch_bytes(&mut self) -> u64 {
        let mut w = hirsel_engine::NativeMetadataWeight::default();
        w.add(size_of::<Self>() + 2 * size_of::<usize>());
        w.packed_map(&mut self.requests, |_, _, _| ());
        w.bytes()
    }
    fn open_request(&self, key: QueryId) -> Result<Request, String> {
        let record = self
            .requests
            .get(&id(key))
            .ok_or("unregistered native task")?;
        let recipe: RequestRecipe = super::dispatch::read(self.snapshot.store.as_ref(), *record)?;
        let (work, expected) = match recipe.work {
            WorkIndex::Instance(owner) => {
                let owner = self
                    .snapshot
                    .owners
                    .get(&owner)
                    .ok_or("missing native task owner")?
                    .clone();
                let product = self
                    .snapshot
                    .instances
                    .completed_inputs()?
                    .into_iter()
                    .find(|(candidate, _)| candidate == &owner)
                    .ok_or("missing native task instance")?
                    .1;
                let expected = instance_query(&owner, product.product.product.clone());
                (Work::Instance(owner), expected)
            }
            WorkIndex::Ready(owner) => {
                let owner = self
                    .snapshot
                    .owners
                    .get(&owner)
                    .ok_or("missing Ready task owner")?
                    .clone();
                let expected = owner.ready_query_id();
                (Work::Ready(owner), expected)
            }
            WorkIndex::Arena(index) => {
                let component = self
                    .snapshot
                    .arena
                    .component_keys
                    .get(index)
                    .ok_or("missing native task component")?
                    .clone();
                let address = component.address()?;
                let summary = self
                    .snapshot
                    .summaries
                    .get(&address.0)
                    .ok_or("missing native task summary")?;
                let expected = query(
                    if component.stratum == ArenaStratum::Chosen {
                        ARENA_CHOSEN_KIND
                    } else {
                        ARENA_CANDIDATES_KIND
                    },
                    summary.product.clone(),
                );
                (Work::Arena(component), expected)
            }
        };
        if expected != key {
            return Err("native task recipe names another query".into());
        }
        Ok(Request {
            work,
            reads: recipe
                .reads
                .into_iter()
                .map(|(kind, address, record)| {
                    (
                        QueryId {
                            kind,
                            address: hirsel_engine::store::Address(address),
                        },
                        record,
                    )
                })
                .collect(),
            guard: recipe.guard.map(|(kind, address)| QueryId {
                kind,
                address: hirsel_engine::store::Address(address),
            }),
        })
    }
    pub fn new(snapshot: Arc<NativeSnapshot>, weight: MemoryWeight) -> Result<Arc<Self>, String> {
        Self::build(snapshot, weight, false)
    }
    pub fn for_ready(
        snapshot: Arc<NativeSnapshot>,
        weight: MemoryWeight,
    ) -> Result<Arc<Self>, String> {
        Self::build(snapshot, weight, true)
    }
    fn build(
        snapshot: Arc<NativeSnapshot>,
        weight: MemoryWeight,
        ready_only: bool,
    ) -> Result<Arc<Self>, String> {
        let fallback = snapshot
            .store
            .put_content(FAILURE, b"native task resource or protocol failure")
            .ok_or("native task error publication failed")?;
        let inputs = snapshot.instances.completed_inputs()?;
        let guard = if snapshot.needs_guard {
            if let Some(guard) = snapshot.guard.get() {
                Some(guard.clone())
            } else {
                let mut locals = inputs
                    .iter()
                    .map(|(key, stored)| {
                        let output = stored.read(&snapshot.instances, key)?;
                        Ok(super::seal_demand::Input {
                            query: instance_query(key, output.product.product.clone()),
                            product: output.product.product.clone(),
                            equations: output.facts.body.seal_demand.open()?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                locals.sort_by(|a, b| a.equations.owner().cmp(b.equations.owner()));
                let mut roots = snapshot
                    .instances
                    .original_roots()
                    .iter()
                    .map(owner)
                    .collect::<Vec<_>>();
                roots.sort();
                roots.dedup();
                if roots.is_empty() {
                    return Err("sealed guard graph has no original entry selection".into());
                }
                let plan = super::seal_demand::Plan::new(
                    locals,
                    &roots,
                    snapshot.options.target.digest(),
                    inputs[0].0.policy,
                )?;
                let guard = super::seal_demand::GuardTasks::new(
                    snapshot.store.clone(),
                    plan,
                    weight,
                    snapshot.guard_admitted_bytes,
                    snapshot.retained_allowances.clone(),
                )?;
                snapshot
                    .guard
                    .set(guard.clone())
                    .map_err(|_| "seal-demand dispatch installed twice")?;
                Some(guard)
            }
        } else {
            None
        };
        let by_owner = inputs
            .iter()
            .map(|(key, output)| (owner(key), (key, output)))
            .collect::<BTreeMap<_, _>>();
        let addresses = snapshot
            .arena
            .component_keys
            .iter()
            .map(ComponentKey::address)
            .collect::<Result<Vec<_>, _>>()?;
        let components = snapshot
            .arena
            .component_keys
            .iter()
            .enumerate()
            .flat_map(|(index, key)| {
                key.members.iter().map(move |member| {
                    ((key.stratum == ArenaStratum::Chosen, owner(member)), index)
                })
            })
            .collect::<BTreeMap<_, _>>();
        let mut requests = BTreeMap::new();
        let mut register = |q: QueryId,
                            work: WorkIndex,
                            reads: Vec<(QueryId, Reference)>,
                            guard: Option<QueryId>|
         -> Result<(), String> {
            if (size_of::<NativeTask>() + reads.len() * size_of::<(QueryId, Reference)>()) as u64
                > weight.scratch
            {
                return Err("native reads exceed declared scratch allowance".into());
            }
            let record = super::dispatch::put(
                snapshot.store.as_ref(),
                &RequestRecipe {
                    work,
                    reads: reads
                        .into_iter()
                        .map(|(q, r)| (q.kind, q.address.0, r))
                        .collect(),
                    guard: guard.map(|q: QueryId| (q.kind, q.address.0)),
                },
            )?;
            if requests.insert(id(q), record).is_some() {
                return Err("duplicate native task recipe".into());
            }
            Ok(())
        };
        let summary_read = |index: usize| -> Result<_, String> {
            let key = &snapshot.arena.component_keys[index];
            let address = addresses[index];
            let summary = snapshot
                .summaries
                .get(&address.0)
                .ok_or("missing native component result")?;
            Ok((
                query(
                    if key.stratum == ArenaStratum::Chosen {
                        ARENA_CHOSEN_KIND
                    } else {
                        ARENA_CANDIDATES_KIND
                    },
                    summary.product.clone(),
                ),
                summary
                    .product
                    .archive(ArchiveAdmission::Retained)
                    .map_err(|e| e.to_string())?
                    .reference(),
            ))
        };
        for (key, stored) in &inputs {
            let output = stored.read(&snapshot.instances, key)?;
            let q = instance_query(key, output.product.product.clone());
            if !ready_only {
                register(
                    q,
                    WorkIndex::Instance(key.ready_query_id().address.0),
                    vec![],
                    None,
                )?;
            }
            let mut reads = BTreeMap::new();
            for member in
                std::iter::once(owner(key)).chain(output.facts.body.callees.iter().cloned())
            {
                let (member_key, input) = by_owner
                    .get(&member)
                    .ok_or("missing Ready instance dependency")?;
                let q = instance_query(member_key, input.product.product.clone());
                reads.insert(
                    id(q),
                    (
                        q,
                        input
                            .product
                            .product
                            .archive(ArchiveAdmission::Retained)
                            .map_err(|e| e.to_string())?
                            .reference(),
                    ),
                );
                let component = components
                    .get(&(true, member))
                    .ok_or("missing Ready chosen component")?;
                let read = summary_read(*component)?;
                reads.insert(id(read.0), read);
            }
            let q = key.ready_query_id();
            register(
                q,
                WorkIndex::Ready(q.address.0),
                reads.into_values().collect(),
                guard.as_ref().map(|g| g.query_id()),
            )?;
        }
        for (index, key) in snapshot
            .arena
            .component_keys
            .iter()
            .enumerate()
            .filter(|_| !ready_only)
        {
            let mut reads = BTreeMap::new();
            for member in &key.members {
                let (_, input) = by_owner
                    .get(&owner(member))
                    .ok_or("missing component instance")?;
                let q = instance_query(member, input.product.product.clone());
                reads.insert(
                    id(q),
                    (
                        q,
                        input
                            .product
                            .product
                            .archive(ArchiveAdmission::Retained)
                            .map_err(|e| e.to_string())?
                            .reference(),
                    ),
                );
                let input = input.read(&snapshot.instances, member)?;
                for callee in &input.facts.body.callees {
                    let component = components
                        .get(&(key.stratum == ArenaStratum::Chosen, callee.clone()))
                        .ok_or("missing outgoing component")?;
                    if addresses[*component] != addresses[index] {
                        let read = summary_read(*component)?;
                        reads.insert(id(read.0), read);
                    }
                }
            }
            if key.stratum == ArenaStratum::Chosen {
                let candidate = components
                    .get(&(false, owner(&key.members[0])))
                    .ok_or("missing own Candidates component")?;
                let read = summary_read(*candidate)?;
                reads.insert(id(read.0), read);
            }
            let (q, _) = summary_read(index)?;
            register(
                q,
                WorkIndex::Arena(index),
                reads.into_values().collect(),
                None,
            )?;
        }
        if std::mem::size_of::<NativeTask>() as u64 > weight.retained {
            return Err("native continuation exceeds its declared retained allowance".into());
        }
        Ok(Arc::new(Self {
            snapshot,
            requests,
            weight,
            fallback,
            created: AtomicU64::new(0),
            ready_transformations: AtomicU64::new(0),
        }))
    }
    pub fn ownership_reads(
        &self,
        owner: &InstanceAddress,
    ) -> Result<Vec<(QueryId, Reference)>, String> {
        Ok(self.open_request(owner.ready_query_id())?.reads)
    }
    pub fn receipt_queries(&self) -> Vec<QueryId> {
        let mut owners = self.snapshot.owners.values().collect::<Vec<_>>();
        owners.sort_by(|a, b| a.canonical_cmp(b));
        owners
            .into_iter()
            .map(InstanceAddress::ready_query_id)
            .collect()
    }
    pub fn receipts(&self, reads: &[ProductRef]) -> Result<Vec<u8>, String> {
        use std::fmt::Write;
        let mut products = SettledProducts::new(self.snapshot.clone(), reads)?;
        let mut text = String::new();
        for read in reads {
            let owner = self
                .snapshot
                .owners
                .get(&read.address.0)
                .ok_or("receipt read has no native owner")?;
            let ready = products.ready(owner)?;
            products.with_ready(&[ready], |units, _| {
                writeln!(text, "{:?}", units[0].receipt().raw()).unwrap();
                Ok(())
            })?;
        }
        Ok(text.into_bytes())
    }
    pub fn guard_factory(&self) -> Option<(QueryId, Arc<dyn QueryFactory>)> {
        self.snapshot
            .guard
            .get()
            .map(|guard| (guard.query_id(), guard.factory()))
    }
    pub fn failure(&self, message: &str) -> hirsel_engine::store::Reference {
        self.snapshot
            .store
            .put_content(FAILURE, message.as_bytes())
            .unwrap_or(self.fallback)
    }
}
pub struct NativeTaskFactory {
    tasks: Arc<NativeTasks>,
}
impl NativeTaskFactory {
    pub fn new(tasks: Arc<NativeTasks>) -> Self {
        Self { tasks }
    }
}
impl QueryFactory for NativeTaskFactory {
    fn create(&self, key: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if !self.tasks.requests.contains_key(&id(key)) {
            return Err(self.tasks.failure("unregistered native task"));
        }
        self.tasks.created.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(NativeTask {
            tasks: self.tasks.clone(),
            key,
            terminal: None,
        }))
    }
}
struct NativeTask {
    tasks: Arc<NativeTasks>,
    key: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for NativeTask {
    fn id(&self) -> QueryId {
        self.key
    }
    fn weight(&self) -> MemoryWeight {
        // The immutable request and completed products retain their snapshot
        // reservations. Only these Arc handles and terminal state are local.
        MemoryWeight {
            retained: std::mem::size_of::<Self>() as u64,
            ..self.tasks.weight
        }
    }
    fn continuation_bytes(&self) -> u64 {
        std::mem::size_of::<Self>() as u64
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = self.terminal.clone() {
            return match result {
                Ok(p) => QueryPoll::Complete(p),
                Err(e) => QueryPoll::Failed(e),
            };
        }
        let request = match self.tasks.open_request(self.key) {
            Ok(request) => request,
            Err(message) => return QueryPoll::Failed(self.tasks.failure(&message)),
        };
        let bytes = request.retained_bytes();
        if bytes.saturating_add(size_of::<Self>() as u64) > self.tasks.weight.scratch {
            return QueryPoll::Failed(
                self.tasks
                    .failure("native task recipe exceeds scratch allowance"),
            );
        }
        let _lease = match self.tasks.snapshot.dispatch_read("native-plan", bytes) {
            Ok(lease) => lease,
            Err(message) => return QueryPoll::Failed(self.tasks.failure(&message)),
        };
        let mut waiting = Vec::new();
        for (key, record) in &request.reads {
            match cx.read(*key) {
                Poll::Pending => waiting.push(*key),
                Poll::Ready(Err(error)) => {
                    self.terminal = Some(Err(error));
                    return QueryPoll::Failed(error);
                }
                Poll::Ready(Ok(product))
                    if product.address == key.address
                        && product
                            .archive(ArchiveAdmission::Retained)
                            .is_ok_and(|archive| archive.reference() == *record) => {}
                Poll::Ready(Ok(_)) => {
                    let error = self
                        .tasks
                        .failure("native dependency output changed within the immutable snapshot");
                    self.terminal = Some(Err(error));
                    return QueryPoll::Failed(error);
                }
            }
        }
        if let Some(key) = request.guard {
            match cx.read(key) {
                Poll::Pending => waiting.push(key),
                Poll::Ready(Err(error)) => {
                    self.terminal = Some(Err(error));
                    return QueryPoll::Failed(error);
                }
                Poll::Ready(Ok(product))
                    if self
                        .tasks
                        .snapshot
                        .guard
                        .get()
                        .is_some_and(|guard| guard.accepts(product.clone())) =>
                {
                    ()
                }
                Poll::Ready(Ok(_)) => {
                    let error = self
                        .tasks
                        .failure("Ready seal-demand output differs from its settled dependency");
                    self.terminal = Some(Err(error));
                    return QueryPoll::Failed(error);
                }
            }
        }
        if !waiting.is_empty() {
            return QueryPoll::Waiting(waiting);
        }
        let mut products = DemandProducts::new(self.tasks.snapshot.clone());
        let result = match &request.work {
            Work::Instance(key) => products.instance(key).map(|p| p.product.clone()),
            Work::Arena(key) => products.arena_summary(key).map(|p| p.product.clone()),
            Work::Ready(key) => products.ready(key).map(|p| p.product.clone()),
        }
        .map_err(|error| self.tasks.failure(&error));
        self.tasks.ready_transformations.fetch_add(
            products.local.queries.counts.transformations,
            Ordering::Relaxed,
        );
        self.terminal = Some(result.clone());
        match result {
            Ok(p) => QueryPoll::Complete(p),
            Err(e) => QueryPoll::Failed(e),
        }
    }
}

/// Source-led Codegen tasks sharing this exact native snapshot. Link/root
/// manifests are a separate producer and are deliberately not registered here.
pub struct CodegenTasks {
    pub execution: Option<QueryId>,
    native: Arc<NativeTasks>,
    requests: BTreeMap<Id, CodegenKey>,
    whole: BTreeMap<Id, hirsel_native::object_queries::whole::WholeCodegenKey>,
    weight: MemoryWeight,
}
impl CodegenTasks {
    pub fn retained_dispatch_bytes(&mut self) -> u64 {
        use super::ownership_compilation::metadata as m;
        let mut w = hirsel_engine::NativeMetadataWeight::default();
        w.add(size_of::<Self>() + 2 * size_of::<usize>());
        let codegen = |w: &mut hirsel_engine::NativeMetadataWeight, key: &CodegenKey| {
            m::address(w, &key.owner);
            w.add(key.backend.capacity());
        };
        w.packed_map(&mut self.requests, |w, _, key| codegen(w, key));
        w.packed_map(&mut self.whole, |w, _, key| {
            w.add(2 * size_of::<usize>() + size_of_val(key.units()));
            for unit in key.units() {
                codegen(w, unit);
            }
        });
        w.bytes()
    }
    pub fn new(
        native: Arc<NativeTasks>,
        keys: Vec<CodegenKey>,
        weight: MemoryWeight,
    ) -> Result<Arc<Self>, String> {
        Self::with_whole_modules(native, keys, vec![], weight)
    }
    pub fn with_whole_modules(
        native: Arc<NativeTasks>,
        keys: Vec<CodegenKey>,
        whole: Vec<hirsel_native::object_queries::whole::WholeCodegenKey>,
        weight: MemoryWeight,
    ) -> Result<Arc<Self>, String> {
        let mut requests = BTreeMap::new();
        for key in keys {
            if !native
                .snapshot
                .owners
                .contains_key(&key.owner.ready_query_id().address.0)
                || requests.insert(id(key.query_id()), key).is_some()
            {
                return Err("unknown or duplicate Codegen request".into());
            }
        }
        let mut whole_requests = BTreeMap::new();
        for key in whole {
            // WholeCodegenKey clones one Arc. The admitted dispatch owns its units.
            if std::mem::size_of::<ObjectTaskRequest>() as u64 > weight.retained {
                return Err("whole-module Codegen exceeds retained allowance".into());
            }
            if key.units().iter().any(|unit| {
                !native
                    .snapshot
                    .owners
                    .contains_key(&unit.owner.ready_query_id().address.0)
            }) || whole_requests.insert(id(key.query_id()), key).is_some()
            {
                return Err("unknown or duplicate whole-module Codegen request".into());
            }
        }
        Ok(Arc::new(Self {
            execution: None,
            native,
            requests,
            whole: whole_requests,
            weight,
        }))
    }
}
fn no_link(
    _: &LinkKey,
    _: &LinkInputs,
    _: &[hirsel_lir::stage::ObjectArtifact],
) -> Result<Vec<u8>, String> {
    Err("Link is not registered in the Codegen snapshot".into())
}
impl ObjectTaskSnapshot for CodegenTasks {
    fn execution_query(&self) -> Option<QueryId> {
        self.execution
    }
    fn admit_execution(&self, product: &ProductRef) -> Result<(), Reference> {
        self.native
            .snapshot
            .execution
            .get()
            .filter(|execution| execution.product == *product)
            .map(|_| ())
            .ok_or_else(|| self.failure("Codegen execution plan differs from admitted live owner"))
    }
    fn store(&self) -> &dyn ResultStore {
        self.native.snapshot.store.as_ref()
    }
    fn request(&self, key: QueryId) -> Result<ObjectTaskRequest, Reference> {
        if let Some(key) = self.whole.get(&id(key)) {
            return Ok(ObjectTaskRequest::WholeCodegen {
                key: key.clone(),
                weight: self.weight,
            });
        }
        let key = self
            .requests
            .get(&id(key))
            .ok_or_else(|| self.failure("unregistered Codegen task"))?;
        Ok(ObjectTaskRequest::Codegen {
            key: key.clone(),
            ready: key.owner.ready_query_id(),
            weight: self.weight,
        })
    }
    fn ready_descriptor(&self, product: ProductRef) -> Result<ReadyProduct, Reference> {
        ready_product(self.store(), product).map_err(|e| self.failure(&e))
    }
    fn optimizer_query(&self, product: ProductRef) -> Result<QueryId, Reference> {
        self.native
            .snapshot
            .owners
            .contains_key(&product.address.0)
            .then_some(QueryId {
                kind: READY_KIND,
                address: product.address,
            })
            .ok_or_else(|| self.failure("unregistered Ready optimizer dependency"))
    }
    fn optimizer_queries(
        &self,
        key: &CodegenKey,
        ready: &ReadyProduct,
    ) -> Result<Vec<(QueryId, Option<ProductRef>)>, Reference> {
        if !ready.optimizer_reads.is_empty() {
            return Err(self.failure("unregistered Ready transformation reads"));
        }
        self.native
            .snapshot
            .codegen_owners(&key.owner, &key.backend)
            .map(|owners| {
                owners
                    .iter()
                    .skip(1)
                    .map(|owner| (owner.ready_query_id(), None))
                    .collect()
            })
            .map_err(|error| self.failure(&error))
    }
    fn link_dependencies(&self, _: ProductRef) -> Result<LinkDependencies, Reference> {
        Err(self.failure("native Link manifest is not registered"))
    }
    fn link_input(&self, _: ProductRef) -> Result<Reference, Reference> {
        Err(self.failure("native Link input is not registered"))
    }
    fn products(&self, settled: &[ProductRef]) -> Result<Box<dyn ObjectProducts>, Reference> {
        let native = SettledProducts::new(self.native.snapshot.clone(), settled)
            .map_err(|e| self.failure(&e))?;
        Ok(Box::new(
            ObjectQueries::new(self.native.snapshot.store.clone(), native, no_link)
                .with_verification(self.native.snapshot.verify_store),
        ))
    }
    fn failure(&self, message: &str) -> Reference {
        self.native.failure(message)
    }
}
