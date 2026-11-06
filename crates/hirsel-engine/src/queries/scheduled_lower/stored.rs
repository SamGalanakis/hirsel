//! Ordinary durable variants for admitted Lower and constant producers.
//! The variant index follows the front producer's exact-read lookup. V still
//! validates every restored body and owns execution-reuse authority.
use super::*;
use hirsel_engine_store::Entry;

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    manifest: Reference,
    descriptor: Reference,
}

fn stored_key(key: &EntryKey) -> EntryKey {
    EntryKey(
        ContentHash::of_parts("hirsel.front.lower-variants.v4", [key.0.0.as_slice()]),
        key.1,
        key.2,
    )
}

impl FrontLowerQueries {
    pub(super) fn manifest_for_outcome(&self, id: QueryId, reads: &Reads) -> Option<Reference> {
        let owner = self
            .paths
            .get(&(id.kind, id.address.0), &self.inputs, &self.namespace)
            .or_else(|| self.staged.selection(id).map(|selection| selection.owner))?;
        self.namespace
            .manifest_for_reads(owner, reads, self.weight.scratch)
    }

    /// Materialize a durable variant only for a persistence-capable request.
    /// Ordinary consumers keep the original admitted live values.
    pub(super) fn persist_live(
        &self,
        id: QueryId,
        key: &EntryKey,
        outcome: &Outcome,
        raw: &Option<RawLowerOutput>,
        verification: Option<&hirsel_validate::VerifiedDefinition>,
    ) -> Option<()> {
        if !self.store.persistence().retains() {
            return None;
        }
        let manifest = self.manifest_for_outcome(id, &outcome.reads)?;
        evidence::persisted_reads(&outcome.reads, outcome.execution.as_ref())?;
        let reads = if outcome.execution.is_some() {
            Reads::from_vec(
                outcome
                    .reads
                    .observed()
                    .map(|(key, hash)| {
                        (
                            *key,
                            if matches!(key, ReadKey::ValidationUniverse(_)) {
                                ContentHash::default()
                            } else {
                                *hash
                            },
                        )
                    })
                    .collect(),
            )
            .ok()?
        } else {
            outcome.reads.clone()
        };
        let depth = outcome.depth.evidence();
        let (domain, bytes) = match &outcome.result {
            Ok(Value::Lower(lower)) => {
                let (output, materialized) = raw.as_ref()?;
                let body = self
                    .records
                    .encode_tir(&*self.namespace.anchors, &output.value)?;
                if body.len() as u64 > self.weight.scratch {
                    return None;
                }
                let body = self.store.put_content(crate::NATIVE_ORIGINAL_BODY, &body)?;
                let constants = self
                    .records
                    .encode(&*self.namespace.anchors, materialized)?;
                if constants.len() as u64 > self.weight.scratch {
                    return None;
                }
                let constants = self.store.put_content(LOWER_MATERIALIZED, &constants)?;
                lower.body.set(body).ok();
                lower.materialized.set(constants).ok();
                let bytes = self.records.encode(
                    &*self.namespace.anchors,
                    &(
                        lower.definition,
                        body,
                        &lower.diagnostics,
                        lower.dependencies.packed(),
                        constants,
                        &reads,
                        &depth,
                        outcome.executed,
                        &outcome.selections,
                        manifest,
                    ),
                )?;
                (LOWER_DESCRIPTOR, bytes)
            }
            Ok(Value::Constant(constants)) => {
                let (value, substitutions) = constants.decoded(
                    &self.records,
                    &self.namespace.anchors,
                    &*self.store,
                    self.weight.scratch,
                )?;
                let bytes = self.records.encode(
                    &*self.namespace.anchors,
                    &(
                        value,
                        substitutions,
                        &reads,
                        &depth,
                        outcome.executed,
                        &outcome.selections,
                        manifest,
                    ),
                )?;
                (CONSTANT_DESCRIPTOR, bytes)
            }
            Err(_) => return None,
        };
        if bytes.len() as u64 > self.weight.scratch {
            return None;
        }
        let descriptor = self.store.put_content(domain, &bytes)?;
        if let Some(evidence) = verification
            && let Some((output, _)) = raw
            && let Some(cache) = self.store.verifier()
            && let Ok(views) = self
                .namespace
                .snapshot
                .validation(&output.value.path.path().0.package)
        {
            cache.publish_definition(
                persistent::evaluation_subject(key, &bytes),
                &output.value,
                self.namespace.snapshot.types().clone(),
                self.namespace.snapshot.source_verification_policy(),
                &*views,
                evidence,
                &[],
            );
        }
        if let Some(receipt) = &outcome.execution
            && let Some(cache) = self.store.verifier()
        {
            cache.publish_execution(persistent::evaluation_subject(key, &bytes), receipt);
        }
        self.remember_stored(id, key, descriptor, outcome);
        Some(())
    }

    pub(super) fn remember_stored(
        &self,
        id: QueryId,
        key: &EntryKey,
        descriptor: Reference,
        outcome: &Outcome,
    ) {
        if !self.store.persistence().retains() {
            return;
        }
        if outcome.result.is_err() {
            return;
        }
        // Executed outputs without verifier-owned evidence are deliberately
        // recomputed. An ordinary variant cannot grant execution authority.
        if self.verify == Verify::On && outcome.executed && outcome.execution.is_none() {
            return;
        }
        let Some(manifest) = self.manifest_for_outcome(id, &outcome.reads) else {
            return;
        };
        let record = Record {
            manifest,
            descriptor,
        };
        let Ok(bytes) = bincode::serialize(&record) else {
            return;
        };
        let key = stored_key(key);
        let Some(reads) = evidence::persisted_reads(&outcome.reads, outcome.execution.as_ref())
        else {
            return;
        };
        self.store.cache_query(&key, reads, &bytes);
        debug_assert!(matches!(id.kind, FRONT_LOWER_KIND | FRONT_EVALUATE_KIND));
    }

    pub(super) async fn load_stored(
        self: &Arc<Self>,
        id: QueryId,
        path: &DefPath,
        key: &EntryKey,
        query_reads: &QueryReads,
    ) -> Option<Result<ProductRef, Reference>> {
        if !self.store.persistence().retains() {
            return None;
        }
        let storage = stored_key(key);
        let entry = Entry::load(&*self.store, &storage);
        for variant in &entry.variants {
            if variant.inputs != storage.1
                || !variant.valid(&storage)
                || variant.output.len > self.weight.scratch
            {
                continue;
            }
            let Some(candidate) = variant.resolve(&*self.store, &storage) else {
                continue;
            };
            let Ok(record) = bincode::deserialize::<Record>(&candidate.payload) else {
                continue;
            };
            if self
                .namespace
                .manifest_inputs_hold(record.manifest, self.weight.scratch)
                || !self.namespace.manifest_source_reads_hold(
                    record.manifest,
                    self.weight.scratch,
                    &variant.reads,
                    |read| self.observe(path, read),
                )
            {
                continue;
            }
            let Some(producer) = self.demand_producer() else {
                continue;
            };
            if let Some(successor) = producer
                .reopen_manifest(&self.namespace, record.manifest, query_reads.clone())
                .await
            {
                let owner = successor.owner().clone();
                let actual = owner.id(path, id.kind)?;
                return Some(self.forward_capture(id, actual, owner, query_reads).await);
            }
        }
        // A candidate may name staged headers that are absent from the written
        // capture. Demand those headers from their source producer, then let the
        // successor validate every read and V receipt before reopening a value.
        for variant in &entry.variants {
            if variant.inputs != storage.1 || !variant.valid(&storage) {
                continue;
            }
            let mut staged = BTreeSet::new();
            let compatible = variant.reads.iter().all(|(read, expected)| {
                if let ReadKey::Signature(declaration) = read
                    && declaration.path().0.name.starts_with("$comptime:")
                    && !self.staged.declarations.contains_key(declaration)
                {
                    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
                    weight.tree::<DefId, ()>(
                        staged.len() + usize::from(!staged.contains(declaration)),
                    );
                    if staged.len() >= self.max_reads || weight.bytes() > self.weight.scratch {
                        return false;
                    }
                    staged.insert(*declaration);
                    true
                } else {
                    self.observe(path, read) == Some(*expected)
                }
            });
            if !compatible || staged.is_empty() {
                continue;
            }
            if variant.output.len > self.weight.scratch {
                continue;
            }
            let candidate = variant.resolve(&*self.store, &storage)?;
            let record: Record = bincode::deserialize(&candidate.payload).ok()?;
            let domain = if id.kind == FRONT_LOWER_KIND {
                LOWER_DESCRIPTOR
            } else {
                CONSTANT_DESCRIPTOR
            };
            if record.descriptor.domain != domain || record.descriptor.len > self.weight.scratch {
                continue;
            }
            let producer = self.demand_producer()?;
            let mut capture = self.namespace.clone();
            let mut successor = None;
            for declaration in staged {
                if let Some(next) = producer
                    .stage_declaration(&capture, declaration.path(), query_reads.clone())
                    .await
                    .ok()?
                {
                    capture = next.namespace().clone();
                    successor = Some(next);
                }
            }
            if let Some(successor) = successor {
                let owner = successor.owner().clone();
                let actual = owner.id(path, id.kind)?;
                return Some(self.forward_capture(id, actual, owner, query_reads).await);
            }
        }
        let variant =
            entry.resolve_current(&*self.store, &storage, |read| self.observe(path, read))?;
        // This outer record contains only fixed-size references.
        let record: Record = bincode::deserialize(&variant.payload).ok()?;
        let domain = if id.kind == FRONT_LOWER_KIND {
            LOWER_DESCRIPTOR
        } else {
            CONSTANT_DESCRIPTOR
        };
        if record.descriptor.domain != domain || record.descriptor.len > self.weight.scratch {
            return None;
        }
        let descriptor = self.store.get_content(record.descriptor)?;
        if let Some(producer) = self.demand_producer() {
            producer
                .charge_manifest(
                    &self.namespace,
                    record.manifest,
                    self.inputs
                        .get(path)
                        .map_or(Span::default(), |input| input.site),
                    query_reads,
                )
                .await
                .ok()?;
        }
        let (value, reads, depth, executed, selections, reads_decode_bytes) = if id.kind
            == FRONT_LOWER_KIND
        {
            let (
                (
                    definition,
                    body,
                    diagnostics,
                    dependencies,
                    materialized,
                    reads,
                    depth,
                    executed,
                    selections,
                    manifest,
                ),
                reads_decode_bytes,
            ): (reads::Descriptor, u64) = self.records.decode_admitted(
                &*self.namespace.anchors,
                descriptor.clone(),
                self.weight.scratch,
            )?;
            if !self
                .namespace
                .manifest_inputs_hold(manifest, self.weight.scratch)
                || definition.path() != path
                || body.domain != crate::NATIVE_ORIGINAL_BODY
                || body.len > self.weight.scratch
                || materialized.domain != LOWER_MATERIALIZED
                || materialized.len > self.weight.scratch
            {
                return None;
            }
            let body_bytes = self.store.get_content(body)?;
            let body_required = hirsel_product_codec::tir_decode_bytes(&body_bytes)?;
            if reads_decode_bytes.checked_add(body_required)? > self.weight.scratch {
                return None;
            }
            let _body_admission = self.store.try_admit_product_memory(body_required)?;
            let source = self.namespace.anchors.clone();
            let decoded =
                Arc::new(
                    self.records
                        .open_tir(source.clone(), body_bytes, body, source)?,
                );
            if decoded.header()?.path != definition {
                return None;
            }
            let materialized_bytes = self.store.get_content(materialized)?;
            let materialized_required = self
                .records
                .required_record::<Vec<(hirsel_syntax::ExprId, Materialized)>>(
                    materialized_bytes,
                )?;
            if materialized_required > self.weight.scratch {
                return None;
            }
            let definition_value = decoded.materialize()?;
            let retained = hirsel_query_products::retained::decoded_tir_bytes(&definition_value);
            let layout = DefinitionDecodeLayout {
                retained,
                transient: body_required.max(retained),
            };
            if layout.total()? > self.weight.scratch {
                return None;
            }
            let constant_selection_bytes = constant_selection_bytes(&definition_value).ok()?;
            drop(definition_value);
            let mut verified = None;
            let mut acceptance = None;
            if self.verify == Verify::On {
                // A refused universe reads no stored body: it is a miss.
                let views = self.namespace.snapshot.validation(&path.0.package).ok()?;
                if self.lowering.is_none() {
                    let output = self.store.verifier()?.definition_output(
                        persistent::evaluation_subject(key, &descriptor),
                        definition,
                        self.namespace.snapshot.types(),
                        self.namespace.snapshot.source_verification_policy(),
                        &*views,
                    )?;
                    if !output.matches_archive(
                        &decoded,
                        self.namespace.snapshot.types().clone(),
                        |anchor| self.namespace.anchors.record_owner(anchor),
                        self.namespace.snapshot.source_verification_policy(),
                        &*views,
                    ) {
                        return None;
                    }
                }
                let (report, ran, accepted, evidence) = self
                    .namespace
                    .snapshot
                    .validate_source_artifact(&decoded, |anchor| {
                        self.namespace.anchors.record_owner(anchor)
                    })
                    .ok()?;
                if ran {
                    timings::count(Count::VRuns);
                    self.count(path, |work| work.validation_runs += 1);
                }
                if hirsel_validate::tooling_limit_error(&report).is_some()
                    || !report.violations.is_empty()
                {
                    return None;
                }
                acceptance = accepted;
                if let Some(evidence) = evidence
                    && let Some(cache) = self.store.verifier()
                    && let Some(output) = cache.publish_definition(
                        persistent::evaluation_subject(key, &descriptor),
                        &decoded,
                        self.namespace.snapshot.types().clone(),
                        self.namespace.snapshot.source_verification_policy(),
                        &*views,
                        &evidence,
                        &[],
                    )
                    && let Some((authority, bytes)) = cache.compact_definition_output(&output)
                    && bytes.len() as u64 <= self.weight.scratch
                    && let Some(metadata) = self.store.put_content(LOWER_VERIFICATION, &bytes)
                {
                    verified = Some(StoredVerification {
                        authority,
                        metadata,
                    });
                }
            }
            (
                Value::Lower(Box::new(LowerProduct {
                    live: None,
                    local_verified: None,
                    definition,
                    body: Arc::new(std::sync::OnceLock::from(body)),
                    diagnostics,
                    dependencies: dependencies.into_inner(),
                    archived_metadata: None,
                    verified,
                    acceptance,
                    materialized: Arc::new(std::sync::OnceLock::from(materialized)),
                    definition_decode_layout: layout,
                    constant_selection_bytes,
                })),
                reads,
                depth,
                executed,
                selections,
                reads_decode_bytes,
            )
        } else {
            let (
                (value, substitutions, reads, depth, executed, selections, manifest),
                reads_decode_bytes,
            ): (reads::ConstantDescriptor, u64) = self.records.decode_admitted(
                &*self.namespace.anchors,
                descriptor.clone(),
                self.weight.scratch,
            )?;
            if !self
                .namespace
                .manifest_inputs_hold(manifest, self.weight.scratch)
            {
                return None;
            }
            let mut constants = Constants::new(value, substitutions);
            constants.set_working_bytes(reads_decode_bytes);
            (
                Value::Constant(Box::new(constants)),
                reads,
                depth,
                executed,
                selections,
                reads_decode_bytes,
            )
        };
        if depth.first.len() != dependency_limit(self.namespace.limits.call_depth) as usize + 1 {
            return None;
        }
        let execution_subject = persistent::evaluation_subject(key, &descriptor);
        let execution = if self.verify == Verify::On && executed {
            match self.store.verifier()?.execution_revalidated(
                execution_subject,
                self.namespace.snapshot.types().clone(),
                |path, occurrence| {
                    crate::NativeRecordSource::record_source_anchor(
                        &*self.namespace.anchors,
                        path,
                        occurrence,
                    )
                },
                &*self.namespace.snapshot,
            ) {
                Ok(Some(receipt)) => Some(receipt),
                Ok(None) => return None,
                Err(error) => {
                    let error = QueryError::from(error);
                    let bytes = self.records.encode(
                        &*self.namespace.anchors,
                        &(
                            path,
                            &error,
                            Some(QueryKind::Lower.phase_name()),
                            &reads,
                            &depth,
                            executed,
                        ),
                    )?;
                    let outcome = Outcome {
                        diagnostic_sources: None,
                        phase: Some(QueryKind::Lower),
                        result: Err(error),
                        reads,
                        reads_decode_bytes,
                        selections,
                        depth: depth.into(),
                        executed,
                        execution: None,
                        execution_subject,
                        action: hirsel_artifact_store::Action(execution_subject),
                        observations: None,
                    };
                    return Some(
                        self.publish(
                            id,
                            key,
                            outcome,
                            Publication {
                                domain: FAILURE,
                                bytes: bytes.into(),
                            },
                            query_reads,
                        )
                        .await,
                    );
                }
            }
        } else {
            None
        };
        let reads = evidence::current_reads(reads, execution.as_ref())?;
        if evidence::persisted_reads(&reads, execution.as_ref())? != variant.reads {
            return None;
        }
        let outcome = Outcome {
            diagnostic_sources: None,
            phase: Some(QueryKind::Lower),
            result: Ok(value),
            reads,
            reads_decode_bytes,
            selections,
            depth: depth.into(),
            executed,
            execution,
            execution_subject,
            action: hirsel_artifact_store::Action(execution_subject),
            observations: None,
        };
        if let Err(error) = self
            .restore_selections(&outcome.selections, query_reads)
            .await
        {
            return Some(Err(error));
        }
        self.remember_imported_carry(id, key, &outcome);
        // A store view can pin an entire packed container. The descriptor's
        // length was bounded by this task's scratch partition above; detach
        // that payload before its archive takes a settled retention lease.
        let descriptor = descriptor.to_vec().into();
        Some(
            self.publish(
                id,
                key,
                outcome,
                Publication {
                    domain,
                    bytes: descriptor,
                },
                query_reads,
            )
            .await,
        )
    }
}
