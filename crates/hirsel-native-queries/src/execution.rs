//! Certified execution is a request dependency of ordinary Codegen and Link.
//! The complete C-admitted closure binds both planners and final object checks.
use super::{root_tasks::NativeRootTasks, *};
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Action, Address, Domain};

use hirsel_native::ReadyProgram;
use hirsel_native::execution_plan::ExecutionPlan;
use hirsel_native::execution_plan::{ExecutionProduct, identity};
use hirsel_native::object_queries::CodegenKey;
use std::{collections::BTreeMap, sync::OnceLock, task::Poll};

pub const EXECUTION_KIND: u32 = 0x4e00_0031;
pub const EXECUTION_OBJECTS_KIND: u32 = 0x4e00_0032;
const OBJECTS: Domain = Domain {
    kind: EXECUTION_OBJECTS_KIND,
    schema: 1,
};

pub struct ExecutionTasks {
    roots: Arc<NativeRootTasks>,
    query: QueryId,
    objects: BTreeMap<ContentHash, CodegenKey>,
    weight: MemoryWeight,
    completed: OnceLock<ProductRef>,
}
impl ExecutionTasks {
    pub fn retained_dispatch_bytes(&mut self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<Self>() + 2 * size_of::<usize>());
        weight.packed_map(&mut self.objects, |weight, _, key| {
            super::ownership_compilation::metadata::address(weight, &key.owner);
            weight.add(key.backend.capacity());
        });
        weight.bytes()
    }
    pub fn new(
        roots: Arc<NativeRootTasks>,
        keys: &[CodegenKey],
        weight: MemoryWeight,
    ) -> Arc<Self> {
        let query = QueryId {
            kind: EXECUTION_KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.native.execution-address.v1",
                [roots.query_id().address.0.0.as_slice()],
            )),
        };
        Arc::new(Self {
            roots,
            query,
            objects: keys
                .iter()
                .map(|key| (Self::objects_id(key).address.0, key.clone()))
                .collect(),
            weight,
            completed: OnceLock::new(),
        })
    }
    pub fn query_id(&self) -> QueryId {
        self.query
    }
    /// Explicit persistence/remote boundary. A reopened descriptor never
    /// substitutes for the current request's independently admitted live plan.
    pub fn archive_plan(
        &self,
        product: &ProductRef,
        admission: ArchiveAdmission<'_>,
    ) -> Result<ProductArchive, String> {
        self.roots
            .native
            .snapshot
            .execution
            .get()
            .filter(|plan| plan.product == *product)
            .ok_or("execution archive differs from admitted live owner")?;
        hirsel_native::execution_plan::archive_plan(
            self.roots.native.snapshot.store.as_ref(),
            product,
            admission,
            self.roots.native.snapshot.verify_store,
        )
    }

    pub fn objects_id(key: &CodegenKey) -> QueryId {
        QueryId {
            kind: EXECUTION_OBJECTS_KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.native.execution-objects-address.v1",
                [key.address().0.0.as_slice()],
            )),
        }
    }
    fn publish(
        &self,
        query: QueryId,
        domain: Domain,
        action: Action,
        bytes: Vec<u8>,
    ) -> Result<ProductRef, String> {
        let store = &self.roots.native.snapshot.store;
        if self.roots.native.snapshot.verify_store == hirsel_engine_store::VerifyStore::Recompute
            && let Some(previous) = store
                .lookup_product(query.address)
                .filter(|p| p.action == action)
            && domain.reference(&bytes) != Some(previous.reference)
        {
            return Err("recomputed execution product differs from cached output".into());
        }
        let record = store
            .put_content(domain, &bytes)
            .ok_or("execution publication exceeds admission")?;
        if domain.reference(&bytes) != Some(record) {
            return Err("execution content identity differs".into());
        }
        store.associate_product(query.address, action, record);
        Ok(ProductRef::import_archive(
            query.address,
            action,
            store
                .admit_product_archive(record, bytes.into())
                .ok_or("native product archive is unavailable or exceeds admission")?,
        ))
    }
    fn plan(&self, root: ProductRef, reads: &[ProductRef]) -> Result<ProductRef, String> {
        let (mut native, selected) = self.roots.products(root.clone(), reads)?;
        let snapshot = &self.roots.native.snapshot;
        let mut owners = snapshot.owners.values().cloned().collect::<Vec<_>>();
        owners.sort_by(InstanceAddress::canonical_cmp);
        let products = owners
            .iter()
            .map(|owner| native.ready(owner))
            .collect::<Result<Vec<_>, _>>()?;
        let (units, table) = native.with_ready(&products, |units, inputs| {
            Ok((
                units.iter().map(|u| (*u).clone()).collect::<Vec<_>>(),
                inputs.layouts.clone(),
            ))
        })?;
        let mut program =
            native.with_link_context(&selected, &products, |program| Ok(program.clone()))?;
        if program.table != table {
            return Err("execution Ready and Link bindings differ".into());
        }
        program.units = units;
        program.inline_instances = snapshot.emission.inline_instances.clone();
        program.ahead_environments = hirsel_lir::ahead::environments(
            &program
                .units
                .iter()
                .map(|u| u.unit().clone())
                .collect::<Vec<_>>(),
        );
        let closures = products
            .iter()
            .map(|p| {
                let owner = snapshot
                    .owners
                    .get(&p.product.address.0)
                    .ok_or("missing execution layout owner")?;
                let operations = snapshot
                    .instances
                    .operations(owner, &snapshot.environment())?;
                let view = super::products::DemandProducts::new(snapshot.clone());
                view.local.queries.assembly_layouts(
                    owner,
                    p,
                    &hirsel_lir::validate::NativeEnvironment {
                        operations: &operations,
                        ..snapshot.environment()
                    },
                )
            })
            .collect::<Result<Vec<_>, String>>()?;
        let binding = hirsel_lir::products::Binding::from_checked(
            &program.target,
            &closures.iter().map(Arc::as_ref).collect::<Vec<_>>(),
        )?;
        if binding.table != program.table {
            return Err("execution binding differs from admitted Ready".into());
        }
        let ahead = hirsel_native::ahead::plan(&program);
        if !hirsel_lir::ahead::validate_cpu_workers(
            &program.units.iter().collect::<Vec<_>>(),
            &program.table,
            &program.target,
            &ahead,
            program.fuel,
        ) {
            return Err("invalid CPU ahead certificate".into());
        }
        let continuation = hirsel_native::continuation::plans(&program);
        hirsel_native::continuation::validate(&program, &continuation)
            .map_err(|e| e.to_string())?;
        let dependencies = reads
            .iter()
            .map(|p| {
                p.archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                    .map(|archive| archive.reference())
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let action = Action(ContentHash::of_parts(
            "hirsel.native.execution-action.v1",
            [serde_json::to_vec(&(
                root.archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                    .map_err(|e| e.to_string())?
                    .reference(),
                dependencies,
                snapshot.context,
            ))
            .map_err(|e| e.to_string())?
            .as_slice()],
        ));
        let plan = ExecutionPlan::new(
            snapshot.store.as_ref(),
            program.ahead_environments,
            binding,
            root,
            reads.to_vec(),
            ahead,
            continuation,
            snapshot.context,
        )?;
        let product = ProductRef::live(self.query.address, action, plan.clone());
        // An explicitly persisted descriptor is still checked on recomputation.
        // Compare its checked fields with the live identity without encoding.
        if snapshot.verify_store == hirsel_engine_store::VerifyStore::Recompute
            && let Some(previous) = snapshot
                .store
                .lookup_product(self.query.address)
                .filter(|previous| previous.action == action)
        {
            let archive = snapshot
                .store
                .copy_product_archive(previous.reference)
                .map_err(|error| format!("persisted execution plan: {error}"))?;
            let imported = ProductRef::import_archive(self.query.address, action, archive);
            if identity(&imported)? != identity(&product)? {
                return Err("recomputed execution product differs from cached output".into());
            }
        }
        snapshot
            .execution
            .set(Arc::new(ExecutionProduct {
                product: product.clone(),
                plan,
            }))
            .map_err(|_| "execution plans published twice")?;
        self.completed
            .set(product.clone())
            .map_err(|_| "execution output published twice")?;
        Ok(product)
    }
    pub fn with_program<R>(
        &self,
        use_program: impl FnOnce(&ReadyProgram) -> Result<R, String>,
    ) -> Result<R, String> {
        let execution = self
            .roots
            .native
            .snapshot
            .execution
            .get()
            .ok_or("execution plans have not settled")?;
        let (mut native, root) = self
            .roots
            .products(execution.plan.root().clone(), execution.plan.reads())?;
        let products = execution
            .plan
            .reads()
            .iter()
            .map(|read| {
                super::ready_product(self.roots.native.snapshot.store.as_ref(), read.clone())
                    .map(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        native.with_execution_context(&root, &products, use_program)
    }
    fn emit(&self, query: QueryId, plan: ProductRef) -> Result<ProductRef, String> {
        let _timing = hirsel_intern::timings::phase(hirsel_intern::timings::Phase::Codegen);
        let snapshot = &self.roots.native.snapshot;
        snapshot
            .execution
            .get()
            .filter(|p| p.product == plan)
            .ok_or("unread execution plan")?;
        let key = self
            .objects
            .get(&query.address.0)
            .ok_or("unregistered execution backend")?;
        self.with_program(|program| {
            let store = snapshot.store.as_ref();
            let action = Action(ContentHash::of_parts(
                "hirsel.native.execution-objects-action.v2",
                [
                    identity(&plan)?.0.as_slice(),
                    snapshot.context.output.0.0.as_slice(),
                ],
            ));
            if snapshot.verify_store == hirsel_engine_store::VerifyStore::Off
                && let Some(previous) = store
                    .lookup_product(query.address)
                    .filter(|p| p.action == action)
                && let Ok(archive) = store.copy_product_archive(previous.reference)
            {
                let product = ProductRef::import_archive(query.address, action, archive);
                if let Ok(objects) = self.load_objects(product.clone()) {
                    hirsel_native::ahead::check_objects(program, &objects)
                        .map_err(|e| e.to_string())?;
                    return Ok(product);
                }
            }
            let backend = match key.backend.as_str() {
                "llvm" => hirsel_native::Backend::Llvm,
                "cranelift" => hirsel_native::Backend::Cranelift,
                _ => return Err("unknown execution backend".into()),
            };
            let registry = hirsel_lir::descriptor_registry::DescriptorRegistry::new(
                &program.table,
                &program.finalizers,
            );
            let objects = hirsel_native::execution_objects::objects(
                program,
                backend,
                snapshot.emission.opt,
                snapshot.emission.measurement,
                &registry,
            )
            .map_err(|e| e.to_string())?;
            hirsel_native::ahead::check_objects(program, &objects).map_err(|e| e.to_string())?;
            // These are checked before transport, then checked again from exact bytes at Link.
            let certificates = hirsel_native::continuation::ahead_certificates(program, &objects);
            if certificates.len()
                != objects
                    .iter()
                    .find_map(|o| o.ahead.as_ref())
                    .map_or(0, |e| e.bounds.len())
            {
                return Err("ahead objects lack continuation certificates".into());
            }
            let text = hirsel_codec::to_text(&objects);
            let blob = hirsel_native::object_queries::store_link_input(store, text.as_bytes())?;
            let bytes = serde_json::to_vec(&blob).map_err(|e| e.to_string())?;
            let product = self.publish(query, OBJECTS, action, bytes)?;
            Ok(product)
        })
    }
    pub fn load_objects(
        &self,
        product: ProductRef,
    ) -> Result<Vec<hirsel_lir::stage::ObjectArtifact>, String> {
        self.objects_from_reference(
            product
                .archive(hirsel_engine_runtime::ArchiveAdmission::Retained)
                .map_err(|e| e.to_string())?
                .reference(),
        )
    }
    pub fn objects_from_reference(
        &self,
        record: Reference,
    ) -> Result<Vec<hirsel_lir::stage::ObjectArtifact>, String> {
        if record.domain != OBJECTS {
            return Err("wrong execution objects domain".into());
        }
        let store = self.roots.native.snapshot.store.as_ref();
        let bytes = store
            .get_content(record)
            .ok_or("missing execution object descriptor")?;
        let blob = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let bytes = hirsel_native::object_queries::link_input_bytes(store, blob)?;
        hirsel_codec::from_text(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
            .map_err(|e| e.message)
    }
}
pub struct Factory(pub Arc<ExecutionTasks>);
impl QueryFactory for Factory {
    fn create(&self, query: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if query != self.0.query
            && !(query.kind == EXECUTION_OBJECTS_KIND
                && self.0.objects.contains_key(&query.address.0))
        {
            return Err(self.0.roots.native.failure("unregistered execution query"));
        }
        Ok(Box::new(Task {
            tasks: self.0.clone(),
            query,
            terminal: None,
        }))
    }
}
struct Task {
    tasks: Arc<ExecutionTasks>,
    query: QueryId,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.query
    }
    fn weight(&self) -> MemoryWeight {
        self.tasks.weight
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if self.terminal.is_none() {
            let ids = if self.query.kind == EXECUTION_KIND {
                let mut owners = self
                    .tasks
                    .roots
                    .native
                    .snapshot
                    .owners
                    .values()
                    .collect::<Vec<_>>();
                owners.sort_by(|a, b| a.canonical_cmp(b));
                std::iter::once(self.tasks.roots.query_id())
                    .chain(owners.iter().map(|o| o.ready_query_id()))
                    .collect::<Vec<_>>()
            } else {
                vec![self.tasks.query]
            };
            let mut reads = Vec::new();
            let mut waiting = Vec::new();
            for id in ids {
                match cx.read(id) {
                    Poll::Pending => waiting.push(id),
                    Poll::Ready(Ok(p)) => reads.push(p),
                    Poll::Ready(Err(e)) => {
                        self.terminal = Some(Err(e));
                        break;
                    }
                }
            }
            if self.terminal.is_none() {
                if !waiting.is_empty() {
                    return QueryPoll::Waiting(waiting);
                }
                let result = if self.query.kind == EXECUTION_KIND {
                    self.tasks.plan(reads[0].clone(), &reads[1..])
                } else {
                    self.tasks.emit(self.query, reads[0].clone())
                };
                self.terminal = Some(result.map_err(|e| self.tasks.roots.native.failure(&e)));
            }
        }
        match self.terminal.clone().unwrap() {
            Ok(p) => QueryPoll::Complete(p),
            Err(e) => QueryPoll::Failed(e),
        }
    }
}
