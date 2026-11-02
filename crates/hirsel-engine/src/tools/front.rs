//! Compact program products. Pending generation-stamped notifications retire
//! products before reuse, even when another query already settled its memo.
//! The aggregate owns values, not body memos; ordinary per-definition demand
//! still uses the graph's normal verification and miss path.
use super::*;
use hirsel_syntax::{DefId, DefMap};

#[derive(Default)]
pub(crate) struct FrontProducts {
    outcomes: DefMap<PublishedResult>,
    pub(crate) checked: DefMap<CheckedSummary>,
    selection: DefMap<std::sync::Arc<PublishedSummary>>,
    pub(crate) dead_arms: DefMap<DeadArmUse>,
    pub(crate) lowering_inputs: DefMap<crate::queries::lowering::LoweringUse>,
    invalidated: DefMap<u64>,
}
/// The body facts the package's dead-error-arm lint consumes, independently
/// of the H body that produced them.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct DeadArmUse {
    pub(crate) errors: std::collections::BTreeSet<DefPath>,
    pub(crate) constructed: std::collections::BTreeSet<(DefPath, u32)>,
}
impl FrontProducts {
    fn invalidate(&mut self, definition: DefId, generation: u64) {
        if self.invalidated.get(&definition) == Some(&generation) {
            return;
        }
        self.outcomes.remove(&definition);
        self.checked.remove(&definition);
        // Keep diagnostic facts available if a later edit restores their reads.
        // Every use rechecks their text and pending inputs; Lower publication
        // retires them independently through forget().
        self.selection.remove(&definition);
        self.dead_arms.remove(&definition);
        self.lowering_inputs.remove(&definition);
        self.invalidated.insert(definition, generation);
    }
    pub(crate) fn forget(&mut self, key: &crate::memo::MemoKey) {
        match key.kind {
            QueryKind::Summary => {
                self.outcomes.remove(&key.def);
                self.selection.remove(&key.def);
                self.lowering_inputs.remove(&key.def);
            }
            QueryKind::CheckedSummary => {
                self.checked.remove(&key.def);
                self.dead_arms.remove(&key.def);
            }
            _ => {
                if self.outcomes.get(&key.def).is_some_and(|outcome| {
                    outcome.result.is_err() && outcome.phase == Some(key.kind)
                }) {
                    self.outcomes.remove(&key.def);
                }
            }
        }
    }
}
impl QueryEngine {
    pub(crate) fn invalidate_front_products(&mut self) -> crate::queries::FrontChanges {
        let changes = self.frontier_changes();
        self.invalidate_front_changes(changes)
    }
    pub(crate) fn invalidate_front_definition(
        &mut self,
        definition: DefId,
    ) -> crate::queries::FrontChanges {
        let changes = self.frontier_change(definition);
        self.invalidate_front_changes(changes)
    }
    fn invalidate_front_changes(
        &mut self,
        changes: crate::queries::FrontChanges,
    ) -> crate::queries::FrontChanges {
        for (definition, generation) in changes.generations() {
            if self.front_products.invalidated.get(&definition) == Some(&generation) {
                continue;
            }
            // S1 can already have settled the phase memo that caused this
            // notification. Revalidate its complete earlier P product before
            // dropping compact lint/lowering inputs with the old generation.
            // Retained Lower facts recheck their own execution/V inputs on use.
            let previous = self.front_products.outcomes.get(&definition).cloned();
            let previous = previous.and_then(|mut outcome| {
                let published = outcome.result.as_mut().ok()?;
                published.summary =
                    self.revalidate_front_summary(&published.summary, &outcome.reads)?;
                Some((
                    outcome,
                    self.front_products.checked.get(&definition).cloned(),
                    self.front_products.dead_arms.get(&definition).cloned(),
                    self.front_products
                        .lowering_inputs
                        .get(&definition)
                        .cloned(),
                ))
            });
            if self.front_products.invalidated.get(&definition) != Some(&generation) {
                self.work.front_invalidations += 1;
            }
            self.front_products.invalidate(definition, generation);
            if let Some((outcome, checked, dead_arms, lowering_inputs)) = previous {
                if let Ok(published) = &outcome.result {
                    self.front_products
                        .selection
                        .insert(definition, published.summary.clone());
                }
                self.front_products.outcomes.insert(definition, outcome);
                if let Some(checked) = checked {
                    self.front_products.checked.insert(definition, checked);
                }
                if let Some(dead_arms) = dead_arms {
                    self.front_products.dead_arms.insert(definition, dead_arms);
                }
                if let Some(lowering_inputs) = lowering_inputs {
                    self.front_products
                        .lowering_inputs
                        .insert(definition, lowering_inputs);
                }
            }
        }
        let removed: Vec<_> = changes.removed.iter().copied().collect();
        self.acknowledge_frontier(&changes, &removed);
        for definition in removed {
            self.front_products.invalidated.remove(&definition);
        }
        changes
    }

    /// Compact input of program selection. A hit reads no body memo, but
    /// contributes exactly the same complete closure to an executing caller.
    pub(crate) fn front_summary(
        &mut self,
        definition: DefId,
    ) -> Result<std::sync::Arc<PublishedSummary>, QueryError> {
        self.work.front_summary_visits += 1;
        self.invalidate_front_products();
        let summary = match self.front_products.selection.get(&definition) {
            Some(summary) => summary.clone(),
            None => {
                let summary = self.definition_summary(definition)?;
                if self.current_front_summary(&summary) {
                    self.front_products
                        .selection
                        .insert(definition, summary.clone());
                }
                summary
            }
        };
        self.merge_execution(&summary.reads);
        Ok(summary)
    }

    fn current_front_summary(&self, summary: &std::sync::Arc<PublishedSummary>) -> bool {
        !summary.reads.is_empty()
            && self
                .summaries
                .get(&summary.definition)
                .is_some_and(|entry| {
                    entry.consistent
                        && !self.graph.is_dirty(&entry.memo.key)
                        && entry
                            .result()
                            .is_ok_and(|output| std::sync::Arc::ptr_eq(&output.value, summary))
                })
    }

    /// S1 body tasks publish the same complete diagnostic/summary product as
    /// P. Keep their refreshed result when its summary is current, so a later
    /// unchanged P request does not demand a deliberately released body.
    pub(crate) fn retain_front_completion(
        &mut self,
        outcome: &PublishedResult,
        changes: &crate::queries::FrontChanges,
    ) {
        let id = DefId::intern(&outcome.definition);
        if let Ok(published) = &outcome.result
            && !outcome.reads.is_empty()
            && self.current_front_summary(&published.summary)
        {
            self.front_products.outcomes.insert(id, outcome.clone());
            self.front_products
                .selection
                .insert(id, published.summary.clone());
            self.acknowledge_frontier(changes, &[id]);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_generation_invalidates_once_without_consuming_another_products_work() {
        let definition = DefId::intern(&entry_path(&default_context()));
        let mut products = FrontProducts::default();
        products.dead_arms.insert(definition, DeadArmUse::default());
        products.invalidate(definition, 1);
        assert!(!products.dead_arms.contains_key(&definition));
        // A summary or lint refresh must survive later observations of the
        // same pending generation while P/S1 has not acknowledged its work.
        products.dead_arms.insert(definition, DeadArmUse::default());
        products.invalidate(definition, 1);
        assert!(products.dead_arms.contains_key(&definition));
        products.invalidate(definition, 2);
        assert!(!products.dead_arms.contains_key(&definition));
    }
    #[test]
    fn retiring_the_receipt_owner_retires_its_compact_failure() {
        let path = entry_path(&default_context());
        let id = DefId::intern(&path);
        let failure = PublishedResult {
            definition: path,
            result: Err(QueryError::Diagnostics(vec![])),
            phase: Some(QueryKind::Modes),
            diagnostic_phases: vec![],
            reads: std::sync::Arc::new(Reads::empty()),
        };
        let mut products = FrontProducts::default();
        products.outcomes.insert(id, failure);
        products.forget(&crate::memo::MemoKey::new(QueryKind::Check, id));
        assert!(products.outcomes.contains_key(&id));
        products.forget(&crate::memo::MemoKey::new(QueryKind::Modes, id));
        assert!(!products.outcomes.contains_key(&id));
    }
}
