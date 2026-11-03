//! Ordinary durable variants for admitted Lower and constant producers.
//! The variant index follows the front producer's exact-read lookup. V still
//! validates every restored body and owns execution-reuse authority.
use super::*;
use crate::store::Entry;

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    descriptor: Reference,
    reads_decode_bytes: u64,
    constant_decode_bytes: u64,
    lower: Option<(DefinitionDecodeLayout, u64)>,
}

fn stored_key(key: &EntryKey) -> EntryKey {
    EntryKey(
        ContentHash::of_parts("hirsel.front.lower-variants.v1", [key.0.0.as_slice()]),
        key.1,
        key.2,
    )
}

impl FrontLowerQueries {
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
        let (lower, constant_decode_bytes) = match &outcome.result {
            Ok(Value::Lower(lower)) => (
                Some((
                    lower.definition_decode_layout,
                    lower.constant_selection_bytes,
                )),
                0,
            ),
            Ok(Value::Constant(value)) => (None, value.working_bytes()),
            Err(_) => return,
        };
        // Executed outputs without verifier-owned evidence are deliberately
        // recomputed. An ordinary variant cannot grant execution authority.
        if self.verify == Verify::On && outcome.executed && outcome.execution.is_none() {
            return;
        }
        let record = Record {
            descriptor,
            reads_decode_bytes: outcome.reads_decode_bytes,
            constant_decode_bytes,
            lower,
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
                    let mut weight = crate::NativeMetadataWeight::default();
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
        // This outer record contains only fixed-size references and bounds.
        let record: Record = bincode::deserialize(&variant.payload).ok()?;
        let domain = if id.kind == FRONT_LOWER_KIND {
            LOWER_DESCRIPTOR
        } else {
            CONSTANT_DESCRIPTOR
        };
        if record.descriptor.domain != domain
            || record.descriptor.len > self.weight.scratch
            || record.reads_decode_bytes > self.weight.scratch
            || record.constant_decode_bytes > self.weight.scratch
        {
            return None;
        }
        let descriptor = self.store.get_content(record.descriptor)?;
        let (value, reads, depth, executed, selections) = if id.kind == FRONT_LOWER_KIND {
            let (layout, constant_selection_bytes) = record.lower?;
            if layout.total()? > self.weight.scratch {
                return None;
            }
            let (
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
            ): reads::Descriptor = self.records.decode(&*self.namespace.anchors, &descriptor)?;
            if !self.namespace.manifest_holds(manifest, self.weight.scratch)
                || definition.path() != path
                || body.domain != crate::NATIVE_ORIGINAL_BODY
                || body.len > self.weight.scratch
                || materialized.domain != LOWER_MATERIALIZED
                || materialized.len > self.weight.scratch
            {
                return None;
            }
            let source = self.namespace.anchors.clone();
            let decoded = Arc::new(self.records.open_tir(
                source.clone(),
                self.store.get_content(body)?,
                body,
                source,
            )?);
            if decoded.header()?.path != definition {
                return None;
            }
            self.store.get_content(materialized)?;
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
                        views,
                    )?;
                    if !output.matches_archive(
                        &decoded,
                        self.namespace.snapshot.types().clone(),
                        |anchor| self.namespace.anchors.record_owner(anchor),
                        self.namespace.snapshot.source_verification_policy(),
                        views,
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
                        views,
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
                    definition,
                    body,
                    diagnostics,
                    dependencies: dependencies.into_inner(),
                    archived_metadata: None,
                    verified,
                    acceptance,
                    materialized,
                    definition_decode_layout: layout,
                    constant_selection_bytes,
                })),
                reads,
                depth,
                executed,
                selections,
            )
        } else {
            if record.lower.is_some() {
                return None;
            }
            let (value, substitutions, reads, depth, executed, selections, manifest): (
                Materialized,
                Vec<(hirsel_syntax::ExprId, Materialized)>,
                Reads,
                DepthEvidence<Span>,
                bool,
                staged::Selections,
                Reference,
            ) = self.records.decode(&*self.namespace.anchors, &descriptor)?;
            if !self.namespace.manifest_holds(manifest, self.weight.scratch) {
                return None;
            }
            let mut constants = Constants::new(value, substitutions);
            constants.set_working_bytes(record.constant_decode_bytes);
            (
                Value::Constant(Box::new(constants)),
                reads,
                depth,
                executed,
                selections,
            )
        };
        if depth.first.len() != dependency_limit(self.namespace.limits.call_depth) as usize + 1 {
            return None;
        }
        let execution_subject = persistent::evaluation_subject(key, &descriptor);
        let execution = if self.verify == Verify::On && executed {
            Some(self.store.verifier()?.execution_revalidated(
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
            )?)
        } else {
            None
        };
        let reads = evidence::current_reads(reads, execution.as_ref())?;
        if evidence::persisted_reads(&reads, execution.as_ref())? != variant.reads {
            return None;
        }
        let outcome = Outcome {
            diagnostic_sources: None,
            result: Ok(value),
            reads,
            reads_decode_bytes: record.reads_decode_bytes,
            selections,
            depth: depth.into(),
            executed,
            execution,
            execution_subject,
        };
        if let Err(error) = self
            .restore_selections(&outcome.selections, query_reads)
            .await
        {
            return Some(Err(error));
        }
        self.remember_carry(id, key, &descriptor, &outcome);
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
                    persist: false,
                    raw: None,
                },
                query_reads,
            )
            .await,
        )
    }
}
