//! Body-dependent private headers are settled before immutable front snapshots.
//! The public callable rows are unchanged; ordinary signature subscribers carry
//! invalidation when a body's proved bits change.
use super::*;
use hirsel_types::{SignatureKind, UniqueInference};
pub(in crate::queries) mod scheduled;

/// The accepted generated bodies one private inference read, sorted by row.
pub(super) type RegistrationInputs = Box<[[ContentHash; 3]]>;

pub(super) fn registration_inputs(
    registrations: &[Arc<AcceptedFrontRegistration>],
) -> RegistrationInputs {
    let mut rows = registrations
        .iter()
        .map(|registration| {
            // The generated body's own header carries the proof this inference
            // publishes. Reading it back would make each publication a new input.
            let id = DefId::intern(registration.definition());
            let mut reads = hirsel_query_products::reads::ReadsBuilder::new();
            for &(key, value) in registration.reads().iter() {
                if inherited_registration_read(&key)
                    && !matches!(key, ReadKey::Signature(d) | ReadKey::ExecutedSignature(d) if d == id)
                {
                    reads.record(key, value);
                }
            }
            [
                registration.definition().fingerprint(),
                registration.syntax.text_hash,
                reads
                    .finish()
                    .map_or_else(|_| registration.reads().digest(), |reads| reads.digest()),
            ]
        })
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows.dedup();
    rows.into_boxed_slice()
}

/// Generated text is checked again by inference. The generator's verifier
/// receipt is not an inference input.
pub(super) fn inherited_registration_read(key: &ReadKey) -> bool {
    !matches!(
        key,
        ReadKey::ValidationView(_)
            | ReadKey::ValidationUniverse(_)
            | ReadKey::UniverseErrors
            | ReadKey::HeapRole
    )
}

/// A front batch selects only the registrations of its own definitions. Facts
/// inferred from a superset still hold for it; only a new or changed accepted
/// body invalidates them.
pub(super) fn registrations_covered(
    inputs: &[[ContentHash; 3]],
    registrations: &[Arc<AcceptedFrontRegistration>],
) -> bool {
    registration_inputs(registrations)
        .iter()
        .all(|row| inputs.binary_search(row).is_ok())
}

impl QueryEngine {
    pub(super) fn private_body_may_change_signature(&self, id: DefId) -> bool {
        let Some(Ok(signature)) = self.unmarked_signature(id) else {
            return true;
        };
        let SignatureKind::Function(function) = &signature.kind else {
            return false;
        };
        let inert = |ty| match &self.types.row(ty).kind {
            hirsel_types::TypeKind::Primitive(primitive) => !matches!(
                primitive,
                hirsel_types::Primitive::String | hirsel_types::Primitive::Bytes
            ),
            hirsel_types::TypeKind::Tuple(fields) => fields.is_empty(),
            _ => false,
        };
        function.parameters.iter().any(|parameter| {
            parameter.passing == hirsel_syntax::PassingMode::Mut
                || parameter.passing == hirsel_syntax::PassingMode::Borrow && !inert(parameter.ty)
        }) || !inert(function.result)
    }
    /// Lend the host's existing partition to demand inference. Installing the
    /// allowance does not normalize a header or compute private facts.
    pub fn admit_private_inference(
        &mut self,
        allowance: hirsel_engine_admission::MemoryReservation,
    ) {
        assert!(self.private_inference_allowance.is_none());
        self.private_inference_hosted = true;
        self.private_inference_allowance = Some(Ok(allowance));
    }

    /// Return the partition before the host admits its query executor.
    pub fn release_private_inference_admission(
        &mut self,
    ) -> Result<hirsel_engine_admission::MemoryReservation, QueryError> {
        self.private_inference_allowance.take().unwrap_or_else(|| {
            Err(self
                .current_private_summary_failure()
                .cloned()
                .unwrap_or(QueryError::NotImplemented))
        })
    }
    pub(super) fn private_summary(&self, id: DefId) -> Option<hirsel_types::UniqueSummary> {
        self.private_summaries.get(&id).cloned().or_else(|| {
            self.header_products.as_ref().and_then(|headers| {
                headers.summary(id).unwrap_or_else(|refusal| {
                    self.record_header_refusal(Some(refusal));
                    None
                })
            })
        })
    }
    /// A header or proof read refused while this engine produces a header.
    /// The producing demand fails with it instead of publishing a conservative row.
    pub(super) fn record_header_refusal(&self, refusal: Option<QueryError>) {
        if let Some(refusal) = refusal {
            self.header_refusal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert(refusal);
        }
    }
    pub(super) fn take_header_refusal(&self) -> Option<QueryError> {
        self.header_refusal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
    pub(super) fn attach_private_summary(
        &self,
        signature: &mut hirsel_types::Signature,
        dependencies: &[SignatureDependency],
    ) {
        if let Some(summary) = self.private_summary(signature.definition) {
            signature.metadata.inferred_unique = summary;
            if let Ok(fingerprint) =
                hirsel_types::encoding::fingerprint(&self.types, signature, dependencies)
            {
                signature.fingerprint = fingerprint;
            }
        }
    }

    pub(super) fn observe_private_summary(
        &self,
        signature: &hirsel_types::Signature,
        observed: &mut Observed,
    ) {
        let id = signature.definition;
        if self.private_summary(id).is_some()
            || signature.metadata.visibility == hirsel_syntax::Visibility::Private
                && matches!(&signature.kind, SignatureKind::Function(f) if f.foreign.is_none())
                && self
                    .source_facts(id.path())
                    .is_some_and(|facts| facts.body_kind == BodyKind::Function)
        {
            // A declaration normalized without a Modes adapter still observes
            // its conservative proof. It cannot be a wildcard cache variant
            // after inference publishes a stronger result for the same header.
            let summary = self.private_summary(id).unwrap_or_default();
            observed.merged.record(
                hirsel_query_products::reads::ReadKey::PrivateSummary(id),
                memos::private_summary_value(&summary),
            );
        }
    }

    pub(crate) fn prepare_private_summaries(&mut self) {
        self.note_environment();
        if !self.private_summary_active
            && !self.private_bootstrap_active
            && self.private_summary_revision != Some(self.revision)
        {
            match self.private_inference_allowance.take() {
                Some(Ok(allowance)) => {
                    self.private_inference_allowance =
                        Some(self.prepare_private_inference(allowance));
                }
                Some(Err(error)) => self.private_inference_allowance = Some(Err(error)),
                None => self.standalone_private_inference(),
            }
        }
    }

    pub(crate) fn publish_private_summaries(
        &mut self,
        inferred: hirsel_syntax::DefMap<UniqueInference>,
    ) -> Result<(), QueryError> {
        self.adopted_private = None;
        // A failed source still gets its ordinary per-definition diagnostics.
        // No failed computation publishes optimistic candidate bits.
        {
            self.private_unique_failures = inferred
                .iter()
                .map(|(id, proof)| (*id, proof.failures.clone()))
                .collect();
            let summaries = inferred
                .into_iter()
                .map(|(id, proof)| (id, proof.summary))
                .collect();
            let old = std::mem::replace(&mut self.private_summaries, summaries);
            let mut changed = Vec::new();
            let ids: BTreeSet<_> = old
                .keys()
                .chain(self.private_summaries.keys())
                .copied()
                .collect();
            for id in ids {
                let summary = self.private_summaries.get(&id).cloned().unwrap_or_default();
                let previous = old.get(&id).cloned().unwrap_or_default();
                if let Some(mut entry) = self.signatures.get(&id).cloned()
                    && let Ok(output) = entry.result()
                {
                    if summary == previous && output.value.metadata.inferred_unique == summary {
                        continue;
                    }
                    let complete = if entry.products.resolution.is_none()
                        || entry.products.signature_persisted
                    {
                        self.complete_signature_source(&entry)
                    } else {
                        Some(entry.clone())
                    };
                    if let Some(complete) = complete {
                        entry = complete;
                    } else {
                        // A restored header can omit syntax-backed auxiliary
                        // products. Restore or recompute the complete product
                        // under the new proof instead of rewriting that projection.
                        self.signatures.remove(&id);
                        self.adopted_headers = None;
                        self.private_signature_publications.insert(id);
                        changed.push(id.path().clone());
                        continue;
                    }
                    let mut output = (*output).clone();
                    let mut signature = (*output.value).clone();
                    signature.metadata.inferred_unique = summary.clone();
                    if let Ok(fingerprint) =
                        hirsel_types::encoding::fingerprint(&self.types, &signature, &entry.covered)
                    {
                        signature.fingerprint = fingerprint;
                        let signature_changed = output.value.fingerprint != fingerprint;
                        output.value = Arc::new(signature);
                        let mut observed = Observed::default();
                        observed.products = Arc::new(PhaseProducts {
                            signature_persisted: false,
                            ..(*entry.products).clone()
                        });
                        for (key, value) in entry.reads().iter() {
                            if !matches!(key, hirsel_query_products::reads::ReadKey::PrivateSummary(d) if *d == id)
                            {
                                observed.merged.record(*key, *value);
                            }
                        }
                        self.observe_private_summary(&output.value, &mut observed);
                        let mut next = self.record(
                            entry.memo.key,
                            entry.memo.text,
                            Ok(Arc::new(output)),
                            entry.detail.clone(),
                            &observed,
                        );
                        next.covered = entry.covered.clone();
                        if self.verify_store != hirsel_engine_store::VerifyStore::Recompute {
                            self.store_signature(id.path(), &next)?;
                        }
                        self.private_signature_publications.insert(id);
                        self.signatures.insert(id, Arc::new(next));
                        self.adopted_headers = None;
                        if signature_changed || summary != previous {
                            changed.push(id.path().clone());
                        }
                    }
                }
            }
            if !changed.is_empty() {
                self.snapshot_inputs_revision += 1;
                self.note_private_signature_values(&changed);
            }
        }
        Ok(())
    }
}
