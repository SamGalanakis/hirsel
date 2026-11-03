//! Immutable inputs of a source transaction. Consumers retain this capture;
//! they never refresh its declarations, instance bindings or source owners.
use super::*;
use crate::store::{Domain, EntryKey, Reference};
use crate::{NativeRecordContext, NativeRecords};

const GENERATION: Domain = Domain {
    kind: 0x4601_0008,
    schema: 1,
};
const MANIFEST: Domain = Domain {
    kind: 0x4601_0009,
    schema: 1,
};

const WRITTEN: Domain = Domain {
    kind: 0x4601_000a,
    schema: 1,
};

pub(crate) struct BodyKeys {
    pub implementation: ContentHash,
    pub site: Span,
    pub modes: EntryKey,
    pub lower: EntryKey,
    pub evaluate: EntryKey,
}
pub(crate) struct BodyInput {
    pub syntax: Reference,
    pub implementation: ContentHash,
    pub executable: bool,
    pub generated: bool,
    pub queries: Option<Arc<BodyKeys>>,
}
pub(crate) struct HeaderInput {
    pub output: Arc<QueryOutput<hirsel_types::SignatureRef>>,
    pub reads: Option<Arc<Reads>>,
    pub semantic: Vec<DefId>,
}

/// A capture owns observations and portable bindings, never verifier authority.
/// Successor captures are distinct values, including when they share maps.
pub struct NamespaceCapture {
    pub(crate) snapshot: Arc<SignatureSnapshot>,
    pub(crate) declarations: Arc<crate::snapshot::Declarations>,
    pub(crate) source_paths: Box<[DefId]>,
    pub(crate) roots: Box<[DefId]>,
    pub(crate) sources: SourceMap,
    pub(crate) anchors: Arc<NativeRecordContext>,
    store: Arc<dyn crate::store::ResultStore>,
    pub(crate) instances: Arc<hirsel_tir::InstanceBodies>,
    pub(crate) bodies: BTreeMap<DefId, Arc<BodyInput>>,
    pub(crate) registrations: BTreeMap<DefId, Arc<AcceptedFrontRegistration>>,
    private_sources: BTreeMap<(DefId, u32), hirsel_span::Anchor>,
    pub(crate) records: crate::NativeRecords,
    pub(crate) limits: crate::ComptimeLimits,
    environment: ContentHash,
    verify: Verify,
    pub(crate) manifest: Reference,
    identity: ContentHash,
    retained: u64,
    admission: std::sync::OnceLock<NamespaceAdmission>,
}

struct NamespaceAdmission {
    _reservation: crate::runtime::MemoryReservation,
    _sources: SourceMap,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    base: ContentHash,
    dependencies: Vec<(DefId, Reference)>,
    trace: Vec<DefId>,
    instances: hirsel_tir::InstanceBodies,
    budget: u64,
    depth: usize,
    charging_version: u32,
}

impl NamespaceCapture {
    pub fn validation_input_digest(
        &self,
        package: &hirsel_syntax::PackageId,
    ) -> Result<ContentHash, hirsel_validate::ViewRefusal> {
        self.snapshot.validation_input_digest(package)
    }
    pub(crate) fn revalidate_execution(
        &self,
        receipt: &hirsel_validate::ExecutionReceipt,
    ) -> Option<hirsel_validate::ExecutionReceipt> {
        self.snapshot
            .revalidate_execution(receipt, |path, occurrence| {
                self.private_sources
                    .get(&(DefId::intern(path), occurrence))
                    .copied()
                    .or_else(|| {
                        crate::NativeRecordSource::record_source_anchor(
                            &*self.anchors,
                            path,
                            occurrence,
                        )
                    })
            })
    }
    pub(crate) fn verification(&self) -> Verify {
        self.verify
    }
    pub(crate) fn admits_successor(&self, successor: &Self) -> bool {
        successor.admission.get().is_some()
            && self.environment == successor.environment
            && self.verify == successor.verify
            && self.anchors.bindings_hold(&successor.anchors)
            && self.bodies.iter().all(|(id, previous)| {
                successor.bodies.get(id).is_some_and(|current| {
                    previous.syntax == current.syntax
                        && previous.implementation == current.implementation
                })
            })
            && self.registrations.iter().all(|(id, previous)| {
                successor
                    .registrations
                    .get(id)
                    .is_some_and(|current| previous.text() == current.text())
            })
    }
    pub(crate) fn input_bytes(definitions: &[DefPath]) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            size_of::<NamespaceCapture>()
                + size_of::<crate::snapshot::Declarations>()
                + 2 * size_of::<usize>(),
        );
        weight.tree::<DefId, Arc<BodyInput>>(definitions.len());
        weight.add(definitions.len().checked_mul(
            size_of::<BodyInput>() + size_of::<BodyKeys>() + 4 * size_of::<usize>(),
        )?);
        weight.tree::<DefId, Arc<AcceptedFrontRegistration>>(definitions.len());
        weight.tree::<(DefId, u32), hirsel_span::Anchor>(definitions.len());
        Some(weight.bytes())
    }

    pub(crate) fn admit(
        &self,
        allowance: &mut crate::runtime::MemoryReservation,
        sources: SourceMap,
    ) -> Result<(), QueryError> {
        if self.admission.get().is_none() {
            let reservation = allowance
                .split(self.retained)
                .ok_or_else(|| refused("capture reservation"))?;
            if let Err(admission) = self.admission.set(NamespaceAdmission {
                _reservation: reservation,
                _sources: sources,
            }) {
                allowance
                    .join(admission._reservation)
                    .map_err(|_| refused("capture reservation partition"))?;
            }
        }
        Ok(())
    }
    pub(crate) fn require_environment(&self, engine: &QueryEngine) -> Result<(), QueryError> {
        if self.environment != engine.environment_value() || self.verify != engine.verify {
            return Err(refused(
                "consumer environment differs from namespace capture",
            ));
        }
        Ok(())
    }
    pub(crate) fn file_holds(&self, sources: &SourceMap, file: FileId) -> bool {
        self.sources
            .file(file)
            .zip(sources.file(file))
            .is_some_and(|(captured, current)| {
                captured.logical_path() == current.logical_path()
                    && captured.text() == current.text()
            })
    }
    pub fn identity(&self) -> ContentHash {
        self.identity
    }
    pub fn generation_manifest(&self) -> Reference {
        self.manifest
    }
    pub(crate) fn syntax(&self, path: &DefPath) -> Result<Arc<ItemSyntax>, QueryError> {
        let input = self
            .bodies
            .get(&DefId::intern(path))
            .ok_or_else(|| refused("body outside capture"))?;
        if input.syntax.domain != WRITTEN {
            return Err(refused("written syntax schema"));
        }
        let bytes = self
            .store
            .get_content(input.syntax)
            .ok_or_else(|| refused("missing captured syntax"))?;
        let (mut syntax, occurrence): (ItemSyntax, u32) = self
            .records
            .decode(&*self.anchors, &bytes)
            .ok_or_else(|| refused("invalid captured syntax"))?;
        if syntax.item.definition != *path
            || syntax.text_hash != input.implementation
            || !syntax.structurally_valid(|span| self.sources.slice(span).is_some())
        {
            return Err(refused("captured source binding changed"));
        }
        syntax.occurrence = occurrence;
        Ok(Arc::new(syntax))
    }
    pub(crate) fn header(&self, path: &DefPath) -> Result<HeaderInput, QueryError> {
        let id = DefId::intern(path);
        if let Some(product) = self
            .snapshot
            .header_products()
            .and_then(|headers| headers.product(id))
        {
            let product = product?;
            return Ok(HeaderInput {
                output: product.result.clone()?,
                reads: product.entry.as_ref().map(|entry| entry.memo.reads.clone()),
                semantic: product.entry.as_ref().map_or_else(Vec::new, |entry| {
                    entry.detail.semantic_definitions().collect()
                }),
            });
        }
        let signature = self
            .snapshot
            .signature(id)
            .ok_or(QueryError::NotImplemented)??;
        Ok(HeaderInput {
            output: Arc::new(QueryOutput {
                value: signature,
                diagnostics: vec![],
                dependencies: vec![],
            }),
            reads: None,
            semantic: vec![],
        })
    }
    pub(crate) fn representation(
        &self,
        path: &DefPath,
    ) -> Option<crate::representation::OwnerRecord> {
        self.snapshot
            .header_products()?
            .product(DefId::intern(path))?
            .ok()?
            .entry
            .as_ref()?
            .products
            .representation
            .clone()
    }
    pub(crate) fn anchors(&self) -> &Arc<NativeRecordContext> {
        &self.anchors
    }
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.retained
    }
    pub(crate) fn consistent(&self, other: &Self) -> bool {
        self.identity == other.identity
            && Arc::ptr_eq(&self.snapshot, &other.snapshot)
            && self.anchors().bindings_hold(other.anchors())
            && other.anchors().bindings_hold(self.anchors())
    }
    pub(crate) fn require_consistent(&self, other: &Self) -> Result<(), QueryError> {
        if self.consistent(other) {
            Ok(())
        } else {
            Err(refused("consumer captures differ"))
        }
    }
    /// Byte equality includes the complete independent base and each accepted
    /// generation. A missing manifest is a miss, never ambient source authority.
    pub(crate) fn manifest_holds(&self, reference: Reference, limit: u64) -> bool {
        if reference != self.manifest || reference.domain != MANIFEST || reference.len > limit {
            return false;
        }
        let store = &self.store;
        let Some(bytes) = store.get_content(reference) else {
            return false;
        };
        if MANIFEST.reference(&bytes) != Some(reference) {
            return false;
        }
        let Some(manifest): Option<Manifest> = self.records.decode(&**self.anchors(), &bytes)
        else {
            return false;
        };
        manifest.charging_version == hirsel_tir::CHARGING_VERSION
            && manifest.dependencies.iter().all(|(_, reference)| {
                reference.domain == GENERATION
                    && reference.len <= limit
                    && store
                        .get_content(*reference)
                        .is_some_and(|bytes| GENERATION.reference(&bytes) == Some(*reference))
            })
    }
}
fn refused(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "namespace capture".into(),
        message: message.into(),
    })
}
impl QueryEngine {
    pub(crate) fn prepare_namespace_registrations(
        &mut self,
        registrations: &[Arc<AcceptedFrontRegistration>],
    ) -> Result<(), QueryError> {
        for registration in registrations {
            registration.validate(self)?;
        }
        if !private_unique::registrations_covered(&self.private_summary_inputs, registrations) {
            self.private_summary_inputs = private_unique::registration_inputs(registrations);
            self.private_summary_revision = None;
            self.retire_snapshot();
        }
        Ok(())
    }
    pub fn capture_namespace(
        &mut self,
        definitions: &[DefPath],
        registrations: &[Arc<AcceptedFrontRegistration>],
        limit: u64,
    ) -> Result<Arc<NamespaceCapture>, QueryError> {
        for path in definitions {
            self.materialize(path)?;
        }
        self.register_specializations(definitions);
        for registration in registrations {
            registration.validate(self)?;
        }
        let declarations = self.frozen_declarations().headers_only();
        let anchors = Arc::new(NativeRecordContext::capture(self));
        let store = self
            .result_store
            .clone()
            .ok_or_else(|| refused("missing content store"))?;
        let records = NativeRecords::new(self.types.clone(), store.clone());
        let mut bodies = BTreeMap::new();
        let mut private_sources = BTreeMap::new();
        let mut base = Vec::new();
        // Header/source identities are canonical paths and hashes, never arena
        // indices, source placement or insertion order.
        for path in self.source_paths() {
            let id = DefId::intern(&path);
            base.push((id, self.candidates_value(&path)));
        }
        let base_bytes = records
            .encode(&*anchors, &base)
            .ok_or_else(|| refused("base encoding"))?;
        if base_bytes.len() as u64 > limit {
            return Err(refused("base exceeds admission"));
        }
        let base = ContentHash::of_parts(
            "hirsel.namespace.base.v1",
            [self.environment_value().0.as_slice(), base_bytes.as_slice()],
        );
        for path in definitions {
            let syntax = self.snapshot(path)?;
            let registration = registrations
                .iter()
                .find(|accepted| accepted.definition() == path);
            let syntax_bytes = records
                .encode(&*anchors, &(&syntax, syntax.occurrence))
                .ok_or_else(|| refused("written syntax encoding"))?;
            if syntax_bytes.len() as u64 > limit {
                return Err(refused("written syntax exceeds admission"));
            }
            let syntax_ref = store
                .put_content(WRITTEN, &syntax_bytes)
                .filter(|reference| WRITTEN.reference(&syntax_bytes) == Some(*reference))
                .ok_or_else(|| refused("written syntax publication"))?;
            let generated =
                generation::generator(&syntax).is_some() && !generation::instance_bodied(&syntax);
            if generated {
                private_sources.insert(
                    (
                        DefId::intern(&generation::task::GeneratorInput::private_path(path)),
                        0,
                    ),
                    syntax.anchor,
                );
            }
            let queries = if generated && registration.is_none() {
                None
            } else {
                let mut key = |kind| match registration {
                    Some(accepted) => self.accepted_front_store_key(accepted, kind),
                    None if generation::instance_bodied(&syntax) => {
                        self.front_declaration_store_key(path, kind)
                    }
                    None => self
                        .store_key(path, kind)
                        .ok_or_else(|| refused("missing body key")),
                };
                Some(Arc::new(BodyKeys {
                    implementation: syntax.text_hash,
                    site: syntax.item.name_span,
                    modes: key(QueryKind::Modes)?,
                    lower: key(QueryKind::Lower)?,
                    evaluate: key(QueryKind::Evaluate)?,
                }))
            };
            bodies.insert(
                DefId::intern(path),
                Arc::new(BodyInput {
                    syntax: syntax_ref,
                    implementation: syntax.text_hash,
                    executable: generation::executable(&syntax),
                    generated,
                    queries,
                }),
            );
        }
        self.prepare_namespace_registrations(registrations)?;
        let snapshot = self.signature_inputs();
        let mut instances = self.instance_bodies(&definitions.iter().cloned().collect());
        let mut dependencies = BTreeMap::new();
        for registration in registrations {
            let id = DefId::intern(registration.definition());
            let bytes = records
                .encode(
                    &*anchors,
                    &(
                        id,
                        registration.text(),
                        &registration.syntax,
                        registration.reads(),
                        registration.checked_prefix(),
                        registration.depth(),
                    ),
                )
                .ok_or_else(|| refused("generation encoding"))?;
            if bytes.len() as u64 > limit {
                return Err(refused("generation exceeds admission"));
            }
            let reference = store
                .put_content(GENERATION, &bytes)
                .filter(|r| GENERATION.reference(&bytes) == Some(*r))
                .ok_or_else(|| refused("generation publication"))?;
            if dependencies.insert(id, reference).is_some() {
                return Err(refused("duplicate accepted registration"));
            }
        }
        // Portable generation dependencies name only their generating
        // declarations. The capture identity retains the complete
        // written base; unrelated headers must not change ordinary records.
        let generation_headers = registrations
            .iter()
            .map(|registration| {
                let id = DefId::intern(registration.definition());
                (id, self.candidates_value(id.path()))
            })
            .chain(
                instances
                    .declarations
                    .iter()
                    .map(|id| (*id, self.candidates_value(id.path()))),
            )
            .collect::<BTreeMap<_, _>>();
        let generation_base = records
            .encode(&*anchors, &(self.environment_value(), generation_headers))
            .ok_or_else(|| refused("generation header encoding"))?;
        if generation_base.len() as u64 > limit {
            return Err(refused("generation headers exceed admission"));
        }
        let manifest = Manifest {
            base: ContentHash::of_parts("hirsel.generation.base.v1", [generation_base.as_slice()]),
            dependencies: dependencies.into_iter().collect(),
            trace: registrations
                .iter()
                .map(|r| DefId::intern(r.definition()))
                .collect(),
            instances: instances.clone(),
            budget: self.comptime_limits.budget,
            depth: self.comptime_limits.call_depth,
            charging_version: hirsel_tir::CHARGING_VERSION,
        };
        let bytes = records
            .encode(&*anchors, &manifest)
            .ok_or_else(|| refused("manifest encoding"))?;
        if bytes.len() as u64 > limit {
            return Err(refused("manifest exceeds admission"));
        }
        let manifest = store
            .put_content(MANIFEST, &bytes)
            .filter(|r| MANIFEST.reference(&bytes) == Some(*r))
            .ok_or_else(|| refused("manifest publication"))?;
        let body_identities = bodies
            .iter()
            .map(|(id, body)| {
                (
                    *id,
                    body.implementation,
                    body.queries.as_ref().map(|keys| {
                        (
                            keys.modes.0,
                            keys.modes.1,
                            keys.lower.0,
                            keys.lower.1,
                            keys.evaluate.0,
                            keys.evaluate.1,
                        )
                    }),
                )
            })
            .collect::<Vec<_>>();
        let body_bytes = records
            .encode(&*anchors, &body_identities)
            .ok_or_else(|| refused("body identity encoding"))?;
        let identity = ContentHash::of_parts(
            "hirsel.namespace.capture.v1",
            [
                base.0.as_slice(),
                manifest.output.0.0.as_slice(),
                body_bytes.as_slice(),
            ],
        );
        let registrations: BTreeMap<_, _> = registrations
            .iter()
            .map(|r| (DefId::intern(r.definition()), r.clone()))
            .collect();
        let source_paths = self
            .source_paths()
            .iter()
            .map(DefId::intern)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let roots = self
            .front_source_roots()
            .iter()
            .map(DefId::intern)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of_val(&*source_paths) + size_of_val(&*roots));
        weight.instance_bodies(&mut instances);
        let retained = NamespaceCapture::input_bytes(definitions)
            .and_then(|bytes| bytes.checked_add(weight.bytes()))
            .ok_or_else(|| refused("capture size overflow"))?;
        if retained > limit {
            return Err(refused("capture exceeds admission"));
        }
        Ok(Arc::new(NamespaceCapture {
            snapshot,
            declarations,
            source_paths,
            roots,
            sources: self.sources.clone(),
            anchors,
            store,
            instances: Arc::new(instances),
            bodies,
            registrations,
            private_sources,
            records,
            limits: self.comptime_limits,
            environment: self.environment_value(),
            verify: self.verify,
            manifest,
            identity,
            retained,
            admission: std::sync::OnceLock::new(),
        }))
    }
}
