//! Bounded scalar TIR ownership. Eviction drops artifacts, not the compact
//! receipts or the Lower graph record that program fronts subscribe to.
use super::*;
use crate::memo::retained::retained_lower_entry_bytes;
use crate::memo::{HoldStats, Retention};

pub(super) struct LowerReceipt {
    entry: Entry<()>,
    verdict: Option<Arc<hirsel_validate::VerifiedDefinition>>,
    retained_artifact: bool,
}
impl LowerReceipt {
    pub(super) fn retained_artifact(&self) -> bool {
        self.retained_artifact
    }
}
impl std::ops::Deref for LowerReceipt {
    type Target = Entry<()>;
    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

impl Entry<hirsel_tir::DefinitionRef> {
    fn lower_receipt(&self) -> Entry<()> {
        Entry {
            memo: self.memo.map_value(|output| QueryOutput {
                value: (),
                diagnostics: output.diagnostics.clone(),
                dependencies: output.dependencies.clone(),
            }),
            detail: self.detail.clone(),
            products: self.products.clone(),
            covered: self.covered.clone(),
            depth: self.depth.clone(),
            acceptance: self.acceptance.clone(),
            verification: std::sync::Mutex::new(self.verification.lock().unwrap().clone()),
            selections: self.selections.clone(),
            consistent: self.consistent,
        }
    }
}

impl QueryEngine {
    pub(crate) fn native_lower_owner(&self, path: &DefPath) -> Option<Arc<dyn Send + Sync>> {
        self.lowered
            .get(&DefId::intern(path))
            .cloned()
            .map(|entry| entry as Arc<dyn Send + Sync>)
            .or_else(|| {
                self.lower_receipts
                    .get(&DefId::intern(path))
                    .cloned()
                    .map(|entry| entry as Arc<dyn Send + Sync>)
            })
    }
    /// Evidence survives body eviction on the ordinary Lower receipt. The
    /// verifier still checks its weak body binding and complete environment.
    pub(super) fn lowered_verification(
        &self,
        id: DefId,
    ) -> Option<Arc<hirsel_validate::VerifiedDefinition>> {
        self.lowered
            .get(&id)
            .and_then(|entry| entry.verification.lock().unwrap().clone())
            .or_else(|| self.lower_receipts.get(&id)?.verdict.clone())
    }

    pub(crate) fn revalidate_front_summary(
        &mut self,
        summary: &Arc<PublishedSummary>,
        reads: &Reads,
    ) -> Option<Arc<PublishedSummary>> {
        let id = summary.definition;
        let path = id.path();
        // An absent generated declaration is a miss; revalidation must not
        // materialize it ahead of an earlier input's operational boundary.
        self.items.get(path)?.first()?;
        self.note_environment();
        let text = self.key_text(path, QueryKind::Summary).ok()?;
        if text != summary.text {
            return None;
        }
        let entry = self.summaries.get(&id)?.clone();
        // A Lower-only notification need not demand its unchanged P producer.
        // Reuse only the exact immutable summary still published by a clean,
        // consistent memo with the current owner key. Replacement or dirtiness
        // follows normal demand; the complete earlier outcome reads held above.
        if entry.consistent
            && entry.memo.text == text
            && !entry.memo.is_stale()
            && entry
                .result()
                .is_ok_and(|output| Arc::ptr_eq(&output.value, summary))
        {
            return Some(summary.clone());
        }
        if !self.execution_valid(path, reads) || !self.memo_valid(&entry, text) {
            return None;
        }
        Some(entry.result().ok()?.value.clone())
    }

    /// Physical LoweringPhase::lower calls in this engine, including worker
    /// retries, discarded speculative roots and scalar artifact demands.
    /// LoweringPhase::lower_evaluation is a separate operation.
    pub fn lower_adapter_calls(&self, path: &DefPath) -> u64 {
        self.scalar_lower_calls
            .get(&DefId::intern(path))
            .copied()
            .unwrap_or(0)
            + self.request_lower_work(path).adapter_calls
    }

    /// Physical adapter calls made by scheduled Lower requests only.
    pub fn request_lower_adapter_calls(&self, path: &DefPath) -> u64 {
        self.request_lower_work(path).adapter_calls
    }

    pub(crate) fn record_worker_recomputes(&mut self, path: &DefPath, kind: QueryKind, count: u64) {
        if count == 0 {
            return;
        }
        let count = usize::try_from(count).unwrap_or(usize::MAX);
        let total = self.recomputes.entry((path.clone(), kind)).or_default();
        *total = total.saturating_add(count);
        for _ in 0..count {
            timings::count(Count::Recomputes);
        }
    }

    /// Count scheduled Lower/Evaluate attempts in this engine's S1 work rows.
    pub(crate) fn record_worker_lower_work(
        &mut self,
        path: &DefPath,
        lowerings: u64,
        initializers: u64,
        validation_runs: u64,
    ) {
        let work = self
            .request_lower_work
            .entry(DefId::intern(path))
            .or_default();
        work.query_attempts = work
            .query_attempts
            .saturating_add(lowerings)
            .saturating_add(initializers);
        work.adapter_calls = work.adapter_calls.saturating_add(lowerings);
        work.validation_runs = work.validation_runs.saturating_add(validation_runs);
    }

    /// Semantic traversal consumes facts, not TIR. A completed lower receipt
    /// covers its inherited mode/resolver inputs and complete execution reads.
    /// Nested evaluators still demand bodies and replay their request traces.
    pub(super) fn semantic_lower_ready(&mut self, path: &DefPath) -> Result<bool, QueryError> {
        if self.retention != Retention::Batch || !self.s1.is_empty() {
            return Ok(false);
        }
        // Generated analyzed syntax contains the generated body; the indexed
        // declaration still contains `= comptime ...`. Until token evidence is
        // retained independently, use the original analyzed-syntax path.
        if self.generated.contains_key(path)
            || self
                .items
                .get(path)
                .and_then(|items| items.first())
                .is_some_and(|syntax| generation::generator(syntax).is_some())
        {
            return Ok(false);
        }
        let id = DefId::intern(path);
        let receipt = self.lower_receipts.get(&id).cloned().or_else(|| {
            self.lowered.get(&id).map(|entry| {
                Arc::new(LowerReceipt {
                    entry: entry.lower_receipt(),
                    verdict: entry.verification.lock().unwrap().clone(),
                    retained_artifact: true,
                })
            })
        });
        let Some(receipt) = receipt else {
            return Ok(false);
        };
        let text = self.key_text(path, QueryKind::Lower)?;
        if !receipt.consistent
            || receipt
                .depth
                .as_ref()
                .is_some_and(|depth| depth.limit() != self.depth_limit())
            || !self.memo_valid(&receipt, text)
        {
            return Ok(false);
        }
        let Ok(output) = receipt.result() else {
            return Ok(false);
        };
        if self.verify == Verify::On {
            let Some(evidence) = &receipt.verdict else {
                return Ok(false);
            };
            let snapshot = self.signature_snapshot();
            // A refused universe holds nothing: the receipt is not reused.
            if !snapshot.validation(&path.0.package).is_ok_and(|views| {
                evidence.environment_holds(self.source_verification_policy(), &*views)
            }) {
                return Ok(false);
            }
        }
        if has_errors(&output.diagnostics) {
            return Err(QueryError::Diagnostics(output.diagnostics.clone()));
        }
        let required = self.execution_requirement(path, receipt.reads());
        if !self.lower_acceptance_holds(receipt.acceptance.as_ref()) {
            return Ok(false);
        }
        self.merge_lower_acceptance(receipt.acceptance.as_ref(), required);
        self.merge_execution(receipt.reads());
        Ok(true)
    }

    /// Retained scalar lowering occupancy, including wrappers and V receipts.
    /// Active caller Arcs are in-flight memory and can outlive cache eviction.
    pub fn lower_hold_stats(&self) -> HoldStats {
        self.lower_holds.stats()
    }

    /// Worker frontiers share this session's allowance with scalar cache holds.
    pub fn available_hold_bytes(&self) -> u64 {
        self.hold_bytes
            .saturating_sub(self.lower_holds.stats().bytes)
    }

    pub(crate) fn set_hold_bytes(&mut self, bytes: u64) {
        self.hold_bytes = bytes;
        let evicted = self.lower_holds.set_budget(bytes);
        self.evict_lowerings(evicted);
    }

    pub(super) fn retain_lowering(&mut self, id: DefId) {
        if self.retention != Retention::Batch {
            return;
        }
        let Some(entry) = self.lowered.get(&id).cloned() else {
            return;
        };
        self.lower_receipts.remove(&id);
        let weight = retained_lower_entry_bytes(
            &entry,
            entry
                .verification
                .lock()
                .unwrap()
                .as_ref()
                .map(|e| e.reads()),
        );
        let evicted = self.lower_holds.hold(entry.memo.key, (), weight);
        self.evict_lowerings(evicted);
    }

    pub(super) fn rebuild_lower_holds(&mut self) {
        self.lower_holds.clear();
        // DefIds are session ordinals: use semantic order when changing policy.
        let mut ids: Vec<_> = self.lowered.keys().copied().collect();
        ids.sort_by(|a, b| a.path().cmp(b.path()));
        for id in ids {
            self.retain_lowering(id);
        }
    }

    fn evict_lowerings(&mut self, keys: Vec<MemoKey>) {
        for key in keys {
            if let Some(entry) = self.lowered.remove(&key.def) {
                let verdict = entry.verification.lock().unwrap().clone();
                self.lower_receipts.insert(
                    key.def,
                    Arc::new(LowerReceipt {
                        entry: entry.lower_receipt(),
                        verdict,
                        retained_artifact: true,
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_front_summary_reuse_keeps_exact_producers_and_rejects_dirty_or_edited_inputs() {
        let mut engine =
            QueryEngine::new(default_environment(), crate::phases::engine_only_adapters());
        let context = default_context();
        let file = engine
            .sources
            .add("front.hir", "fn probe() { 1 }".into())
            .unwrap();
        let parsed = engine.parse_file(file, &context).unwrap();
        let path = parsed.value.items[0].item.definition.clone();
        let id = DefId::intern(&path);
        let summary = Arc::new(PublishedSummary {
            definition: id,
            summary: DefinitionSummary {
                owner: id,
                frames: Default::default(),
                calls: Default::default(),
                staged: Default::default(),
            },
            growth: Default::default(),
            storage: Default::default(),
            checked: None,
            reads: Arc::new(Reads::empty()),
            text: engine.key_text(&path, QueryKind::Summary).unwrap(),
        });
        engine.record_summary(summary.clone(), Default::default());
        let reused = engine
            .revalidate_front_summary(&summary, &summary.reads)
            .unwrap();
        assert!(Arc::ptr_eq(&reused, &summary));

        let replacement = Arc::new((*summary).clone());
        engine.record_summary(replacement.clone(), Default::default());
        let reused = engine
            .revalidate_front_summary(&summary, &summary.reads)
            .unwrap();
        assert!(
            Arc::ptr_eq(&reused, &replacement),
            "a replaced producer follows normal demand"
        );

        let mut changed = crate::memo::Changes::default();
        changed.texts.insert(id);
        engine.apply_changes(&changed);
        assert!(
            engine
                .revalidate_front_summary(&replacement, &replacement.reads)
                .is_none()
        );

        engine.record_summary(replacement.clone(), Default::default());
        let file = engine
            .sources
            .add("front.hir", "fn probe() { 2 }".into())
            .unwrap();
        engine.parse_file(file, &context).unwrap();
        assert!(
            engine
                .revalidate_front_summary(&replacement, &replacement.reads)
                .is_none()
        );
    }
}

impl QueryEngine {
    /// Payload of retained S0 signature memo entries, including resolver syntax,
    /// private shapes and complete reads. Table buckets, source text, frozen
    /// declaration inputs and type/session interners are separate components.
    /// Non-source body artifacts refuse rather than receiving a partial bound.
    pub fn source_signature_entry_payload_bytes(&self) -> Option<u64> {
        self.signatures.values().try_fold(0u64, |bytes, entry| {
            bytes.checked_add(crate::memo::retained::retained_signature_entry_bytes(
                entry,
            )?)
        })
    }
}
