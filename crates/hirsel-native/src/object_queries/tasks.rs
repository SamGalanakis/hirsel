//! Typed Codegen/Link continuations. Creation observes immutable key metadata
//! only; actual dependency reads suspend before any backend/linker is entered.
use super::*;
use hirsel_engine_runtime::{
    MemoryWeight, QueryContext, QueryFactory, QueryId, QueryOutcome, QueryTask,
};

pub const CODEGEN_KIND: u32 = 0x4f000001;
pub const LINK_KIND: u32 = 0x4f000002;
impl CodegenKey {
    pub fn query_id(&self) -> QueryId {
        let query = QueryId {
            kind: CODEGEN_KIND,
            address: self.address(),
        };
        hirsel_engine_runtime::profile::definition(query, self.owner.instance.definition());
        query
    }
}
impl LinkKey {
    pub fn query_id(&self) -> QueryId {
        QueryId {
            kind: LINK_KIND,
            address: self.address(),
        }
    }
}

/// Source/native discovery produces the manifest query. Creating a Link task
/// does not enumerate bodies or rebuild a whole-program native plan.
#[derive(Clone, Debug)]
pub enum ObjectTaskRequest {
    WholeCodegen {
        key: super::whole::WholeCodegenKey,
        weight: MemoryWeight,
    },
    Codegen {
        key: CodegenKey,
        ready: QueryId,
        weight: MemoryWeight,
    },
    Link {
        key: LinkKey,
        manifest: QueryId,
        weight: MemoryWeight,
    },
}
impl ObjectTaskRequest {
    fn id(&self) -> QueryId {
        match self {
            Self::WholeCodegen { key, .. } => key.query_id(),
            Self::Codegen { key, .. } => key.query_id(),
            Self::Link { key, .. } => key.query_id(),
        }
    }
    fn weight(&self) -> MemoryWeight {
        match self {
            Self::WholeCodegen { weight, .. }
            | Self::Codegen { weight, .. }
            | Self::Link { weight, .. } => *weight,
        }
    }
}

/// Complete dependencies described by a settled native discovery/link manifest.
/// Preserve archive/foreign order. Fragment order is canonicalized by Link.
#[derive(Clone, Debug)]
pub struct LinkDependencies {
    pub execution: Vec<QueryId>,
    pub fragments: Vec<QueryId>,
    /// Native metadata consumed by Link assembly, read explicitly even on a
    /// Link hit. These products convey no proof; the native view re-admits C.
    pub native_reads: Vec<QueryId>,
    pub static_manifest: QueryId,
    pub runtime_archives: Vec<QueryId>,
    pub foreign_inputs: Vec<QueryId>,
    pub launch_manifest: QueryId,
}

/// Installed by the engine for one immutable request snapshot. Implementations
/// resolve descriptors from the one store; descriptor resolution grants no
/// native/V authority. `products` creates task-local producer state restricted
/// to the settled reads, never a mutable global controller behind a mutex.
pub trait ObjectTaskSnapshot: Send + Sync {
    fn store(&self) -> &dyn ResultStore;
    /// Bounded metadata lookup only: no dependency execution or decoding bodies.
    fn request(&self, id: QueryId) -> Result<ObjectTaskRequest, Reference>;
    fn execution_query(&self) -> Option<QueryId> {
        None
    }
    fn admit_execution(&self, _product: &ProductRef) -> Result<(), Reference> {
        Ok(())
    }
    fn ready_descriptor(&self, product: ProductRef) -> Result<ReadyProduct, Reference>;
    /// Resolve each optimizer read's registered native query kind and address.
    fn optimizer_query(&self, product: ProductRef) -> Result<QueryId, Reference>;
    /// Backend-specific copy queries, plus expected outputs where a producer
    /// already selected them. None means read the immutable request's current
    /// output; it never allows bypassing native admission in products().
    fn optimizer_queries(
        &self,
        _key: &CodegenKey,
        ready: &ReadyProduct,
    ) -> Result<Vec<(QueryId, Option<ProductRef>)>, Reference> {
        ready
            .optimizer_reads
            .iter()
            .map(|product| {
                Ok((
                    self.optimizer_query(product.clone())?,
                    Some(product.clone()),
                ))
            })
            .collect()
    }
    fn link_dependencies(&self, manifest: ProductRef) -> Result<LinkDependencies, Reference>;
    /// Resolve a settled static/runtime/foreign/launch record to its content.
    fn link_input(&self, product: ProductRef) -> Result<Reference, Reference>;
    fn products(&self, settled: &[ProductRef]) -> Result<Box<dyn ObjectProducts>, Reference>;
    fn failure(&self, message: &str) -> Reference;
}

pub struct ObjectTaskFactory<S> {
    snapshot: Arc<S>,
}
impl<S: ObjectTaskSnapshot> ObjectTaskFactory<S> {
    pub fn new(snapshot: Arc<S>) -> Self {
        Self { snapshot }
    }
}
impl<S: ObjectTaskSnapshot + 'static> QueryFactory for ObjectTaskFactory<S> {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        let request = self.snapshot.request(id)?;
        if request.id() != id {
            return Err(self.snapshot.failure("object task key/address mismatch"));
        }
        // Whole-module units are shared with their admitted registry. Link
        // retains its supplied frame grant; per-instance Codegen clones a key.
        let retained = match &request {
            ObjectTaskRequest::Codegen { key, .. } => (std::mem::size_of::<ObjectTask<S>>() as u64)
                .saturating_add(
                    (key.owner.instance.encoding().len() + key.backend.capacity()) as u64,
                ),
            ObjectTaskRequest::WholeCodegen { .. } => std::mem::size_of::<ObjectTask<S>>() as u64,
            ObjectTaskRequest::Link { .. } => request.weight().retained,
        };
        if retained > request.weight().retained {
            return Err(self
                .snapshot
                .failure("object task frame exceeds retained allowance"));
        }
        Ok(Box::new(ObjectTask {
            snapshot: self.snapshot.clone(),
            request,
            retained,
            terminal: None,
        }))
    }
}
struct ObjectTask<S> {
    snapshot: Arc<S>,
    request: ObjectTaskRequest,
    retained: u64,
    terminal: Option<Result<ProductRef, Reference>>,
}

// Read every dependency in the prescribed order; the first failure wins.
fn read(cx: &mut dyn QueryContext, ids: &[QueryId]) -> Result<Vec<ProductRef>, Reference> {
    ids.iter().map(|id| cx.read(*id)).collect()
}

impl<S: ObjectTaskSnapshot> ObjectTask<S> {
    fn read_codegen(
        &self,
        cx: &mut dyn QueryContext,
        ids: &[QueryId],
    ) -> Result<Vec<ProductRef>, Reference> {
        // Execution selects the first failure and gates all backend work.
        if let Some(execution) = self.snapshot.execution_query() {
            let product = cx.read(execution)?;
            self.snapshot.admit_execution(&product)?;
        }
        read(cx, ids)
    }
    fn compute(&self, cx: &mut dyn QueryContext) -> Result<ProductRef, Reference> {
        let product = match &self.request {
            ObjectTaskRequest::WholeCodegen { key, .. } => {
                let ids = key
                    .units()
                    .iter()
                    .map(|unit| unit.owner.ready_query_id())
                    .collect::<Vec<_>>();
                let settled = self.read_codegen(cx, &ids)?;
                for product in &settled {
                    let ready = self.snapshot.ready_descriptor(product.clone())?;
                    if ready.product != *product || !ready.optimizer_reads.is_empty() {
                        return Err(self
                            .snapshot
                            .failure("whole-module Ready dependency differs"));
                    }
                }
                let mut products = self.snapshot.products(&settled)?;
                products
                    .codegen_whole(key)
                    .map_err(|e| self.snapshot.failure(&e))?
                    .product
                    .clone()
            }
            ObjectTaskRequest::Codegen { key, ready, .. } => {
                let own = self.read_codegen(cx, &[*ready])?[0].clone();
                let descriptor = self.snapshot.ready_descriptor(own.clone())?;
                if descriptor.product != own {
                    return Err(self.snapshot.failure("Ready descriptor mismatch"));
                }
                let dependencies = self.snapshot.optimizer_queries(key, &descriptor)?;
                let ids = dependencies.iter().map(|(id, _)| *id).collect::<Vec<_>>();
                if ids.iter().any(|id| *id == *ready)
                    || ids
                        .iter()
                        .enumerate()
                        .any(|(index, id)| ids[..index].contains(id))
                {
                    return Err(self.snapshot.failure("duplicate or self optimizer read"));
                }
                let copies = read(cx, &ids)?;
                for (current, (id, expected)) in copies.iter().zip(&dependencies) {
                    let mismatch = if let Some(old) = expected {
                        let current_archive = current
                            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                            .map_err(|e| self.snapshot.failure(&e.to_string()))?;
                        let old_archive = old
                            .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                            .map_err(|e| self.snapshot.failure(&e.to_string()))?;
                        current.address != old.address
                            || current_archive.reference() != old_archive.reference()
                    } else {
                        false
                    };
                    if current.address != id.address || mismatch {
                        return Err(self.snapshot.failure("stale optimizer body dependency"));
                    }
                }
                let mut settled = vec![own];
                settled.extend(copies);
                let mut products = self.snapshot.products(&settled)?;
                products
                    .codegen(key)
                    .map_err(|e| self.snapshot.failure(&e))?
                    .product
                    .clone()
            }
            ObjectTaskRequest::Link { key, manifest, .. } => {
                let manifest = read(cx, &[*manifest])?[0].clone();
                let deps = self.snapshot.link_dependencies(manifest.clone())?;
                let mut ids = deps.fragments.clone();
                ids.push(deps.static_manifest);
                ids.extend(&deps.runtime_archives);
                ids.extend(&deps.foreign_inputs);
                ids.push(deps.launch_manifest);
                ids.extend(&deps.native_reads);
                ids.extend(&deps.execution);
                let outputs = read(cx, &ids)?;
                let n = deps.fragments.len();
                let fragments = outputs[..n]
                    .iter()
                    .map(|p| {
                        fragment_product(self.snapshot.store(), p.clone())
                            .map_err(|e| self.snapshot.failure(&e))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let resolve = |p| self.snapshot.link_input(p);
                let static_manifest = resolve(outputs[n].clone())?;
                let r = n + 1;
                let f = r + deps.runtime_archives.len();
                let l = f + deps.foreign_inputs.len();
                let runtime_archives = outputs[r..f]
                    .iter()
                    .map(|p| resolve(p.clone()))
                    .collect::<Result<_, _>>()?;
                let foreign_inputs = outputs[f..l]
                    .iter()
                    .map(|p| resolve(p.clone()))
                    .collect::<Result<_, _>>()?;
                let inputs = LinkInputs {
                    execution: outputs[outputs.len() - deps.execution.len()..]
                        .iter()
                        .map(|p| {
                            p.archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                                .map(|archive| archive.reference())
                                .map_err(|e| self.snapshot.failure(&e.to_string()))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    fragments,
                    static_manifest,
                    runtime_archives,
                    foreign_inputs,
                    launch_manifest: resolve(outputs[l].clone())?,
                };
                let mut settled = vec![manifest];
                settled.extend(outputs);
                let mut products = self.snapshot.products(&settled)?;
                products
                    .link(key, &inputs)
                    .map_err(|e| self.snapshot.failure(&e))?
                    .product
                    .clone()
            }
        };
        if product.address != self.request.id().address {
            return Err(self
                .snapshot
                .failure("object task returned another product"));
        }
        Ok(product)
    }
}
impl<S: ObjectTaskSnapshot> QueryTask for ObjectTask<S> {
    fn id(&self) -> QueryId {
        self.request.id()
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            retained: self.retained,
            ..self.request.weight()
        }
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        if let Some(result) = self.terminal.clone() {
            return result;
        }
        let result = self.compute(cx);
        self.terminal = Some(result.clone());
        result
    }
}
