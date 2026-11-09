//! Native compilation follows dependency-ready source data. A separate join
//! requires complete source acceptance before publishing executable readiness.
use super::native_lower;
use super::native_routing::NativeRouting;
use hirsel_engine::FrontLowerQueries;
use hirsel_engine::FrontNativeProducts;
use hirsel_engine::FrontNativeQueries;
use hirsel_engine::FrontProofExport;
use hirsel_engine_runtime::*;
use hirsel_engine_store::*;

use hirsel_native::query_model::NativeRootKey;
use hirsel_native::root_source::NativeLinkFacts;
use hirsel_native::root_source::NativeLinkSource;
use hirsel_native_cache::declarations::NativeDeclarations;
use hirsel_native_cache::source_inputs::NativeInstanceInputs;
use hirsel_native_queries::arena_tasks::ArenaTasks;
use hirsel_native_queries::compilation_tasks::CompilationBackend;
use hirsel_native_queries::compilation_tasks::CompilationOutput;
use hirsel_native_queries::compilation_tasks::CompilationTasks;
use hirsel_native_queries::instance_tasks::InstanceTasks;
use hirsel_native_queries::products::EmissionOptions;
use hirsel_syntax::{ContentHash, DefPath};
use std::sync::{Arc, Mutex, OnceLock};

pub(super) const KIND: u32 = 0x5200_0030;
const OUTPUT: Domain = Domain {
    kind: 0x5200_0031,
    schema: 2,
};
const FAILURE: Domain = Domain {
    kind: 0x5200_0032,
    schema: 1,
};

/// Compiler-owned policy and captured platform inputs. Public request clients
/// cannot supply this plan, a factory, an executor or an admission capability.
pub(super) struct NativePlan {
    pub options: hirsel_native::Options,
    pub policy: ContentHash,
    pub verify_store: hirsel_engine_store::VerifyStore,
    pub opt: hirsel_lir::emission::OptLevel,
    pub debug: bool,
    pub verify_backend: bool,
    pub measurement: bool,

    pub backends: Vec<CompilationBackend>,
    pub requested_run: Option<Arc<hirsel_run_config::ValidatedRun>>,
    pub test_entries: Vec<(DefPath, u64)>,
    pub output: CompilationOutput,
}
/// Selected evidence and the PS9 export captured over this bootstrap's native
/// source root. Assessment reads only that root's completed product.
pub(super) struct NativeProofs {
    pub evidence: Arc<hirsel_native::Evidence>,
    pub export: Arc<FrontProofExport>,
}
pub(super) struct NativeAdmission {
    pub instances: hirsel_engine_admission::MemoryReservation,
    pub arena: hirsel_engine_admission::MemoryReservation,
    pub compilation: hirsel_engine_admission::MemoryReservation,
}
struct Prepared {
    source: ProductRef,
    facts: Arc<FrontNativeProducts>,
    compilation: Arc<CompilationTasks>,
    proofs: Option<hirsel_native::Proofs>,
}
struct Completed {
    product: ProductRef,
    compilation: ProductRef,
}
/// Owns only immutable producer views and compact dispatch/completion records.
/// C acceptance stays inside Instance/Ready and remains independent of source V.
pub(super) struct NativeBootstrap {
    source: Arc<FrontNativeQueries>,
    lower: Arc<FrontLowerQueries>,
    definitions: Vec<DefPath>,
    routing: Arc<NativeRouting>,
    store: Arc<dyn ResultStore>,
    query: QueryId,
    weight: MemoryWeight,
    pending: Mutex<Option<(NativePlan, NativeAdmission)>>,
    proofs: Option<NativeProofs>,
    prepared: OnceLock<Prepared>,
    completed: OnceLock<Completed>,
    failure: Mutex<Option<(Reference, String)>>,
    errors: RuntimeErrors,
    _allowance: hirsel_engine_admission::MemoryReservation,
    output_allowance: Mutex<hirsel_engine_admission::MemoryReservation>,
}
impl NativeBootstrap {
    pub fn new(
        source: Arc<FrontNativeQueries>,
        lower: Arc<FrontLowerQueries>,
        definitions: Vec<DefPath>,
        routing: Arc<NativeRouting>,
        store: Arc<dyn ResultStore>,
        mut plan: NativePlan,
        mut admission: NativeAdmission,
        proofs: Option<NativeProofs>,
        weight: MemoryWeight,
        allowance: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> Result<Arc<Self>, String> {
        // Evidence selects its discharge only after the source root settles;
        // a caller-planned discharge would bypass that assessment.
        if proofs.is_some() && !plan.options.discharge.is_empty() {
            return Err("native evidence and a planned discharge are exclusive".into());
        }
        let discharge = discharge_rows(&plan.options.discharge);
        let native_policy = serde_json::to_vec(&(
            match plan.options.validation {
                hirsel_native::Validation::Ready => 0u8,
                hirsel_native::Validation::EveryStage => 1,
            },
            &plan.options.launch,
            plan.options.target.digest(),
            plan.options.regions,
            plan.options.elide,
            plan.options.reuse,
            plan.options.flat_walk,
            plan.options.scoped_specialization,
            discharge,
            proofs
                .as_ref()
                .map(|proofs| super::native_products::evidence_identity(&proofs.evidence)),
        ))
        .map_err(|error| error.to_string())?;
        plan.policy = ContentHash::of_parts(
            "hirsel.host.native-policy.v1",
            [plan.policy.0.as_slice(), native_policy.as_slice()],
        );
        let selection = serde_json::to_vec(&(
            source.query().address.0,
            plan.policy,
            plan.options.target.digest(),
            match plan.opt {
                hirsel_lir::emission::OptLevel::None => 0u8,
                hirsel_lir::emission::OptLevel::Default => 1,
                hirsel_lir::emission::OptLevel::Speed => 2,
            },
            plan.requested_run.as_ref().map(|run| run.identity()),
            &plan.test_entries,
            plan.output,
            plan.debug,
            plan.verify_backend,
            plan.measurement,
            plan.backends
                .iter()
                .map(|backend| {
                    (
                        &backend.backend,
                        backend.backend_version,
                        backend.target_features,
                        backend.profile_options,
                        backend.runtime_abi,
                        backend.linker_options,
                        &backend.runtime_archives,
                        &backend.foreign_inputs,
                    )
                })
                .collect::<Vec<_>>(),
        ))
        .map_err(|error| error.to_string())?;
        let mut table_weight = hirsel_query_products::retained::MetadataWeight::default();
        table_weight.vec(&plan.test_entries, |weight, (path, _)| weight.path(path));
        let metadata = definitions
            .iter()
            .try_fold(
                std::mem::size_of::<Self>() as u64 + selection.len() as u64 + table_weight.bytes(),
                |bytes, path| {
                    bytes.checked_add(
                        u64::try_from(serde_json::to_vec(path).ok()?.len())
                            .ok()?
                            .checked_add(128)?,
                    )
                },
            )
            .ok_or("native bootstrap inventory overflow")?;
        if metadata > allowance.bytes() || weight.retained < std::mem::size_of::<Task>() as u64 {
            return Err("native bootstrap inventory exceeds admission".into());
        }
        // Reserve output copies from the existing native compilation partition;
        // leave the bootstrap's own inventory charge whole. Constructors still
        // enforce the remaining compilation bound without increasing any cap.
        let output_allowance = admission
            .compilation
            .split(admission.compilation.bytes() / 8)
            .unwrap();
        let query = QueryId {
            kind: KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.host.native-bootstrap.v1",
                [selection.as_slice()],
            )),
        };
        Ok(Arc::new(Self {
            source,
            lower,
            definitions,
            routing,
            store,
            query,
            weight,
            pending: Mutex::new(Some((plan, admission))),
            proofs,
            prepared: OnceLock::new(),
            completed: OnceLock::new(),
            failure: Mutex::new(None),
            errors,
            _allowance: allowance,
            output_allowance: Mutex::new(output_allowance),
        }))
    }
    pub fn reserve_output(
        &self,
        bytes: u64,
    ) -> Result<hirsel_engine_admission::MemoryReservation, String> {
        self.output_allowance
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .split(bytes)
            .ok_or_else(|| "native output metadata exceeds its original admission".into())
    }
    pub fn selected_store(&self) -> Arc<dyn ResultStore> {
        self.store.clone()
    }
    pub fn query(&self) -> QueryId {
        self.query
    }
    fn preparation_query(&self) -> QueryId {
        QueryId {
            kind: KIND,
            address: Address(ContentHash::of_parts(
                "hirsel.host.native-preparation.v1",
                [self.query.address.0.0.as_slice()],
            )),
        }
    }
    pub fn factory(self: &Arc<Self>) -> Arc<dyn QueryFactory> {
        Arc::new(Factory(self.clone()))
    }
    /// Exact selected compiler completion and retained ordinary provider. Link
    /// materialization still revalidates its selected descriptor through S4.
    pub fn outcome(
        &self,
        product: ProductRef,
    ) -> Result<(Arc<CompilationTasks>, ProductRef, Arc<FrontNativeProducts>), String> {
        let completed = self
            .completed
            .get()
            .ok_or("native bootstrap has not completed")?;
        if completed.product != product {
            return Err("native bootstrap received another completion".into());
        }
        let prepared = self
            .prepared
            .get()
            .ok_or("native bootstrap dispatch is unavailable")?;
        Ok((
            prepared.compilation.clone(),
            completed.compilation.clone(),
            prepared.facts.clone(),
        ))
    }
    /// The assessment selected for this bootstrap's completed source root.
    pub fn proofs(&self) -> Option<&hirsel_native::Proofs> {
        self.prepared
            .get()
            .and_then(|prepared| prepared.proofs.as_ref())
    }
    pub fn failure(&self, reference: Reference) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|(selected, message)| (*selected == reference).then(|| message.clone()))
            .or_else(|| self.prepared.get()?.compilation.failure(reference))
    }
    fn fail(&self, message: String) -> Reference {
        if message.len() as u64 > self.weight.retained {
            return self.errors.resource;
        }
        let Some(reference) = self.store.put_content(FAILURE, message.as_bytes()) else {
            return self.errors.resource;
        };
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some((reference, message));
        reference
    }
    fn prepare(&self, product: ProductRef) -> Result<&Prepared, String> {
        if let Some(prepared) = self.prepared.get() {
            return if prepared.source == product {
                Ok(prepared)
            } else {
                Err("native source changed within immutable bootstrap".into())
            };
        }
        let facts = self
            .source
            .outcome(product.clone())
            .ok_or("native source root is unavailable")?
            .products;
        hirsel_intern::memory::checkpoint("front complete");
        let (mut plan, admission) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or("native bootstrap initialization already consumed")?;
        let proofs = self
            .proofs
            .as_ref()
            .map(|proofs| {
                hirsel_native::Proofs::assess_native(
                    &proofs.export,
                    &self.source,
                    product.clone(),
                    &proofs.evidence,
                    self.store.clone(),
                )
                .map_err(|error| error.to_string())
            })
            .transpose()?;
        if let Some(proofs) = &proofs {
            // Instance and Ready identities read the assessed removals, not
            // only the sidecar that selected them for this source revision.
            let assessed = serde_json::to_vec(&discharge_rows(&proofs.discharge))
                .map_err(|error| error.to_string())?;
            plan.policy = ContentHash::of_parts(
                "hirsel.host.native-assessed-policy.v1",
                [plan.policy.0.as_slice(), assessed.as_slice()],
            );
            plan.options.discharge = proofs.discharge.clone();
        }
        let declarations = NativeDeclarations::from_front(facts.clone())
            .map_err(|e| format!("native declarations: {e:?}"))?;
        let reader = facts
            .original_context
            .reader(facts.originals.iter().map(|p| (**p).clone()))
            .map_err(|e| format!("native original products: {e:?}"))?;
        let entries = declarations
            .context
            .sources
            .iter()
            // Source contracts also describe generic bodies and nominal
            // finalizers. The launch table contains ordinary monomorphic
            // functions; finalizer instances remain in discovery/layout facts.
            .filter(|(path, _)| {
                matches!(declarations.program.signatures.get(*path).map(|signature| &signature.kind),
                    Some(hirsel_types::SignatureKind::Function(function)) if function.generics.is_empty())
            })
            .map(|(path, context)| (path.clone(), context.entry.0))
            .collect();
        let operations = Arc::new(declarations.program.operation_registry());
        let callees =
            hirsel_native::settled_callee_metadata(facts.run.callees.clone(), &facts.run.runtime)
                .map_err(|e| e.to_string())?;
        let mut source = NativeLinkSource::from_settled(
            NativeLinkFacts {
                program: &declarations.program,
                operations,
                roots: &declarations.roots,
                declarations: &declarations.sources,
                entries: &entries,
                anchors: &declarations.source_anchors,
                signatures: facts.signatures.clone(),
                semantic: facts.semantic.clone(),
                selected: std::slice::from_ref(&facts.run.entry),
                callees: Some(callees),
            },
            &reader,
        )
        .map_err(|e| e.to_string())?;
        let library_roots = if plan
            .requested_run
            .as_ref()
            .is_some_and(|run| run.record().launch == hirsel_run_schema::LaunchKind::Call)
        {
            let (callee, form) = facts
                .run
                .entries
                .get(&facts.run.entry.to_string())
                .ok_or("native call has no checked Run entry")?;
            let roots = hirsel_engine_admission::entry_roots(
                &declarations.program.types,
                &declarations.program.signatures[&facts.run.entry],
                hirsel_run_schema::LaunchKind::Call,
            )
            .map_err(|e| e.to_string())?;
            if callee.program != facts.run.revision.digest()
                || *form
                    != if roots.is_empty() {
                        hirsel_run_schema::CallForm::Compute
                    } else {
                        hirsel_run_schema::CallForm::Call
                    }
            {
                return Err("native call differs from checked Run entry".into());
            }
            vec![
                source
                    .bind_call(
                        &declarations.program,
                        &facts.run.entry,
                        &plan.options.target,
                    )
                    .map_err(|e| e.to_string())?,
            ]
        } else {
            Vec::new()
        };
        if let Some(run) = plan.requested_run {
            source
                .bind_run(&facts.run.entry, run)
                .map_err(|e| e.to_string())?;
        }
        if !plan.test_entries.is_empty() {
            source
                .bind_tests(&plan.test_entries)
                .map_err(|error| error.to_string())?;
        }
        let emission = EmissionOptions::request(
            source.input().clone(),
            plan.opt,
            plan.debug,
            source.foreign_dispatch(),
            plan.verify_backend,
            plan.measurement,
        );
        let root = NativeRootKey {
            roots: facts.roots.definitions.clone(),
            target_abi: plan.options.target.digest(),
            policy: plan.policy,
        };
        let inputs = NativeInstanceInputs::from_products(declarations, reader)
            .map_err(|e| format!("native instance inputs: {e:?}"))?;
        let source_paths = inputs
            .declarations
            .sources
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if source_paths
            .iter()
            .any(|path| !self.definitions.contains(path))
        {
            return Err("native source escaped the admitted Lower namespace".into());
        }
        let front = native_lower::inputs(
            self.lower.clone(),
            facts.original_context.clone(),
            &source_paths,
        )?;
        let instances = InstanceTasks::with_library_roots(
            inputs,
            self.store.clone(),
            plan.options,
            plan.policy,
            self.weight,
            admission.instances,
            &library_roots,
        )?
        .with_store_verification(plan.verify_store)?
        .with_front_lower(front)?;
        let arena = ArenaTasks::new(
            instances.clone(),
            self.store.clone(),
            0,
            self.weight,
            admission.arena,
        )?;
        let compilation = CompilationTasks::new_with_store_verification(
            instances,
            arena,
            Arc::new(source),
            root,
            emission,
            plan.backends,
            self.weight,
            admission.compilation,
            plan.verify_store,
        )?;
        let compilation = compilation.with_output(plan.output)?;
        self.routing
            .bind(compilation.factories())
            .map_err(str::to_owned)?;
        self.prepared
            .set(Prepared {
                source: product,
                facts,
                compilation,
                proofs,
            })
            .map_err(|_| "native bootstrap dispatch was constructed twice")?;
        Ok(self.prepared.get().unwrap())
    }
    fn publish(&self, compilation: ProductRef) -> Result<ProductRef, String> {
        self.prepared
            .get()
            .ok_or("native dispatch is unavailable")?
            .compilation
            .outputs(compilation.clone())?;
        let action = Action(ContentHash::of_parts(
            "hirsel.host.native-bootstrap-action.v2",
            [
                self.query.address.0.0.as_slice(),
                compilation.address.0.0.as_slice(),
                compilation.action.0.0.as_slice(),
            ],
        ));
        let product = ProductRef::dependencies(
            LiveDependencyProduct::new(
                self.store.as_ref(),
                self.query.address,
                action,
                OUTPUT,
                vec![compilation.clone()],
            )
            .map_err(|e| format!("native bootstrap completion admission: {e}"))?,
        );
        self.completed
            .set(Completed {
                product: product.clone(),
                compilation,
            })
            .map_err(|_| "duplicate native bootstrap completion")?;
        Ok(product)
    }
}
pub(super) fn discharge_rows(
    discharge: &hirsel_lir::discharge::Discharge,
) -> Vec<(&DefPath, Vec<(u32, u32, u32, (u8, &str, u32))>)> {
    use hirsel_lir::discharge::Form;
    discharge
        .0
        .iter()
        .map(|(path, sites)| {
            let sites = sites
                .iter()
                .map(|site| {
                    let form = match &site.form {
                        Form::Check(rule) => (0u8, rule.as_str(), 0u32),
                        Form::Integer => (1, "", 0),
                        Form::Negation => (2, "", 0),
                        Form::Projection { place, rule } => (3, rule.as_str(), *place),
                    };
                    (site.body, site.block, site.statement, form)
                })
                .collect();
            (path, sites)
        })
        .collect()
}
struct Factory(Arc<NativeBootstrap>);
impl QueryFactory for Factory {
    fn create(&self, query: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        if query == self.0.query {
            return Ok(Box::new(acceptance::Task::new(self.0.clone())));
        }
        if query != self.0.preparation_query() {
            return Err(self.0.errors.protocol);
        }
        Ok(Box::new(Task {
            owner: self.0.clone(),
            terminal: None,
        }))
    }
}
struct Task {
    owner: Arc<NativeBootstrap>,
    terminal: Option<Result<ProductRef, Reference>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.owner.preparation_query()
    }
    fn weight(&self) -> MemoryWeight {
        self.owner.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        let result = if let Some(result) = self.terminal.clone() {
            result
        } else {
            let source = match cx.read(self.owner.source.preparation_query()) {
                Err(error) => {
                    self.terminal = Some(Err(error));
                    return Err(error);
                }
                Ok(product) => product,
            };
            match self.owner.prepare(source) {
                Err(error) => Err(self.owner.fail(error)),
                Ok(prepared) => match cx.read(prepared.compilation.query_id()) {
                    Err(error) => Err(error),
                    Ok(compilation)
                        if compilation.address == prepared.compilation.query_id().address =>
                    {
                        let action = Action(ContentHash::of_parts(
                            "hirsel.host.native-preparation-action.v1",
                            [
                                compilation.address.0.0.as_slice(),
                                compilation.action.0.0.as_slice(),
                            ],
                        ));
                        LiveDependencyProduct::new(
                            self.owner.store.as_ref(),
                            self.id().address,
                            action,
                            OUTPUT,
                            vec![compilation],
                        )
                        .map(ProductRef::dependencies)
                        .map_err(|_| self.owner.errors.resource)
                    }
                    _ => Err(self.owner.errors.protocol),
                },
            }
        };
        self.terminal = Some(result.clone());
        result
    }
}

mod acceptance;
#[cfg(test)]
mod tests;
