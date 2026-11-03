//! Host admission for a demanded source scope. The host waits for the same
//! instance producer that suspended Lower and Front consumers use.
use super::*;
use crate::runtime::*;
use hirsel_intern::jobs::Jobs;

pub struct FrontDemandAdmission {
    pub carry: Option<Arc<FrontCarry>>,
    pub jobs: Jobs,
    pub weight: MemoryWeight,
    pub max_reads: usize,
    pub runtime: MemoryReservation,
    pub front: MemoryReservation,
    pub lower: MemoryReservation,
    pub text: MemoryReservation,
    pub syntax: MemoryReservation,
    pub discovery: MemoryReservation,
    pub metadata: MemoryReservation,
}

pub enum FrontDemandError {
    Generator(FrontRegistrationBootstrapError),
    Front {
        reference: crate::store::Reference,
        error: Option<QueryError>,
        sources: SourceMap,
        owner: Arc<FrontBodyQueries>,
    },
    Discovery {
        error: QueryError,
        sources: SourceMap,
        owner: Arc<FrontBodyQueries>,
    },
}
impl std::fmt::Debug for FrontDemandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generator(error) => f.debug_tuple("Generator").field(error).finish(),
            Self::Front {
                reference, error, ..
            } => f
                .debug_struct("Front")
                .field("reference", reference)
                .field("error", error)
                .finish_non_exhaustive(),
            Self::Discovery { error, .. } => f.debug_tuple("Discovery").field(error).finish(),
        }
    }
}
pub struct FrontDemandScope {
    carry: Option<Arc<FrontCarry>>,
    definitions: Vec<DefPath>,
    registrations: Vec<Arc<AcceptedFrontRegistration>>,
    namespace: Arc<NamespaceCapture>,
    front: Option<Arc<FrontBodyQueries>>,
    rounds: usize,
    stats: RuntimeStats,
    failure: Option<FrontDemandError>,
    _metadata: MemoryReservation,
}
impl FrontDemandScope {
    pub(crate) fn carry(&self) -> Option<&Arc<FrontCarry>> {
        self.carry.as_ref()
    }
    pub fn definitions(&self) -> &[DefPath] {
        &self.definitions
    }
    pub fn registrations(&self) -> &[Arc<AcceptedFrontRegistration>] {
        &self.registrations
    }
    pub fn namespace(&self) -> &Arc<NamespaceCapture> {
        &self.namespace
    }
    pub(crate) fn front(&self) -> Option<&Arc<FrontBodyQueries>> {
        self.front.as_ref()
    }
    pub fn rounds(&self) -> usize {
        self.rounds
    }
    pub fn stats(&self) -> RuntimeStats {
        self.stats
    }
    pub fn require_complete_front(mut self) -> Result<Self, FrontDemandError> {
        match self.failure.take() {
            Some(failure) => Err(failure),
            None => Ok(self),
        }
    }
    pub fn input_bytes(definitions: &[DefPath], generators: &[DefPath]) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<Self>() + size_of::<FrontDemandAdmission>());
        weight.add(
            (definitions.len().checked_mul(3)? + generators.len())
                .checked_mul(size_of::<DefPath>())?,
        );
        for path in definitions.iter().chain(generators) {
            weight.path(path);
        }
        Some(weight.bytes())
    }
}
impl QueryEngine {
    pub fn demand_front_scope(
        &mut self,
        definitions: &[DefPath],
        roots: &[DefPath],
        generators: &[DefPath],
        admission: FrontDemandAdmission,
        errors: RuntimeErrors,
    ) -> Result<FrontDemandScope, FrontDemandError> {
        self.demand_front_scope_with_policy(
            definitions,
            roots,
            generators,
            LoweringPolicy::Reachable,
            false,
            admission,
            errors,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn demand_front_scope_with_policy(
        &mut self,
        definitions: &[DefPath],
        roots: &[DefPath],
        generators: &[DefPath],
        policy: LoweringPolicy,
        stop_on_error: bool,
        admission: FrontDemandAdmission,
        errors: RuntimeErrors,
    ) -> Result<FrontDemandScope, FrontDemandError> {
        let admission_error = |message: &str| {
            FrontDemandError::Generator(FrontRegistrationBootstrapError::Admission(
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "generated instance demand admission".into(),
                    message: message.into(),
                }),
            ))
        };
        let required = FrontDemandScope::input_bytes(definitions, generators)
            .ok_or_else(|| admission_error("metadata size overflow"))?;
        if required > admission.metadata.bytes()
            || roots.iter().any(|root| !definitions.contains(root))
        {
            return Err(admission_error(
                "scope cursors exceed metadata or source inventory",
            ));
        }
        let FrontDemandAdmission {
            carry,
            jobs,
            weight,
            max_reads,
            runtime,
            mut front,
            mut lower,
            text,
            syntax,
            discovery,
            mut metadata,
        } = admission;
        drop(discovery);
        let front_grant = front
            .split(front.bytes() / 2)
            .ok_or_else(|| admission_error("Front partition"))?;
        let lower_grant = lower
            .split(lower.bytes() / 2)
            .ok_or_else(|| admission_error("Lower partition"))?;
        let previous = self.private_bootstrap_active;
        self.private_bootstrap_active = true;
        let prepared = (|| {
            let capture = self.capture_namespace(definitions, &[], front_grant.bytes())?;
            let front =
                self.front_body_queries_in(definitions, capture, weight, front_grant, errors)?;
            if let Some(carry) = &carry {
                // An optional carry miss leaves ordinary production active.
                let _ = front.install_carry(carry.clone());
            }
            self.front_lower_queries(definitions, front, weight, max_reads, lower_grant, errors)
        })();
        self.private_bootstrap_active = previous;
        let initial = prepared.map_err(|error| {
            FrontDemandError::Generator(FrontRegistrationBootstrapError::Admission(error))
        })?;
        let producer = demand::DemandProducer::new(
            self,
            initial.clone(),
            demand::DemandAdmission {
                carry: carry.clone(),
                front,
                lower,
                text,
                syntax,
            },
            weight,
            max_reads,
            errors,
        )
        .map_err(|error| {
            FrontDemandError::Generator(FrontRegistrationBootstrapError::Admission(error))
        })?;
        let query = producer
            .scope_query(initial.namespace().clone(), roots, policy, stop_on_error)
            .map_err(|error| {
                FrontDemandError::Generator(FrontRegistrationBootstrapError::Admission(error))
            })?;
        let mut runtime = AdmittedRuntime::new(jobs, runtime, errors)
            .map_err(|_| admission_error("runtime partition"))?;
        runtime
            .register(FRONT_BODY_KIND, initial.body_front().factory())
            .map_err(|_| admission_error("Front factory"))?;
        runtime
            .register_local(FRONT_LOWER_KIND, initial.factory())
            .map_err(|_| admission_error("Lower factory"))?;
        runtime
            .register_local(FRONT_EVALUATE_KIND, initial.factory())
            .map_err(|_| admission_error("Evaluate factory"))?;
        let request = runtime.request(&[query]);
        let selected = runtime.wait(request);
        let result = producer
            .scope_completed(query)
            .ok_or_else(|| admission_error("scope outcome unavailable"))?;
        runtime.shutdown();
        let stats = runtime.stats();
        drop(runtime);
        let successor = match result {
            Ok(successor) if selected.is_ok() => successor,
            result => {
                if let Some(failure) = producer.take_failure() {
                    return Err(FrontDemandError::Generator(failure));
                }
                let error = result.err().unwrap_or(QueryError::NotImplemented);
                return Err(FrontDemandError::Discovery {
                    error,
                    sources: self.sources.clone(),
                    owner: initial.body_front().clone(),
                });
            }
        };
        let namespace = successor.namespace().clone();
        let failure = producer.front_failure(roots);
        let rounds = producer.rounds();
        let definitions = namespace
            .bodies
            .keys()
            .map(|id| id.path().clone())
            .collect();
        let registrations = namespace.registrations.values().cloned().collect();
        self.adopt_namespace(&namespace).map_err(|error| {
            FrontDemandError::Generator(FrontRegistrationBootstrapError::Admission(error))
        })?;
        let metadata = metadata
            .split(required)
            .ok_or_else(|| admission_error("completed scope metadata"))?;
        let front = carry
            .as_ref()
            .map(|_| successor.owner().body_front().clone());
        Ok(FrontDemandScope {
            carry,
            definitions,
            registrations,
            namespace,
            front,
            rounds,
            stats,
            failure,
            _metadata: metadata,
        })
    }
    pub(crate) fn adopt_namespace(
        &mut self,
        namespace: &Arc<NamespaceCapture>,
    ) -> Result<(), QueryError> {
        namespace.require_environment(self)?;
        self.sources = namespace.sources.clone();
        self.instance_bodies = (*namespace.instances).clone();
        if let Some(headers) = namespace.snapshot.header_products() {
            self.staged_constants = headers.staged_constants().clone();
        }
        for id in namespace.bodies.keys() {
            self.items
                .insert(id.path().clone(), vec![namespace.syntax(id.path())?]);
        }
        for (id, registration) in &namespace.registrations {
            self.generated
                .insert(id.path().clone(), registration.source());
        }
        self.private_summary_inputs = private_unique::registration_inputs(
            &namespace
                .registrations
                .values()
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.snapshot = Some((self.snapshot_stamp(), namespace.snapshot.clone()));
        self.adopt_completed_headers();
        Ok(())
    }
}
