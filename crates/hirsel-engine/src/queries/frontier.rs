//! Pending changes for program-level aggregation. A query may settle a dirty
//! memo before the driver visits it, so graph dirtiness alone is not a durable
//! notification. Acknowledgments name observed generations, like Salsa's
//! revision stamps, and cannot consume a newer S1 publication.
use super::*;

/// Definitions whose retained program products need to be refreshed or retired.
#[derive(Clone, Debug, Default)]
pub struct FrontChanges {
    pub changed: BTreeSet<DefId>,
    pub removed: BTreeSet<DefId>,
    pub affected: BTreeSet<DefId>,
    observations: BTreeMap<DefId, u64>,
}
impl FrontChanges {
    /// Generation identity for each pending notification. An aggregate may
    /// retire stale products once without acknowledging work still pending.
    pub(crate) fn generations(&self) -> impl Iterator<Item = (DefId, u64)> + '_ {
        self.observations
            .iter()
            .map(|(&definition, &generation)| (definition, generation))
    }
}
#[derive(Default)]
pub(super) struct FrontTracker {
    active: bool,
    generation: u64,
    pending: BTreeMap<DefId, Pending>,
}
#[derive(Default)]
struct Pending {
    generation: u64,
    present: Option<bool>,
    affected: bool,
}
impl FrontTracker {
    pub(super) fn retained_bytes(&self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.tree::<DefId, Pending>(self.pending.len());
        weight.bytes()
    }
    pub(super) fn is_active(&self) -> bool {
        self.active
    }
    fn activate(&mut self) -> bool {
        let was_active = self.active;
        self.active = true;
        !was_active
    }
    pub(super) fn changed(&mut self, definition: DefId, present: bool) {
        if !self.active {
            return;
        }
        self.generation += 1;
        let pending = self.pending.entry(definition).or_default();
        pending.generation = self.generation;
        pending.present = Some(present);
    }
    fn affected(&mut self, definition: DefId) {
        self.generation += 1;
        let pending = self.pending.entry(definition).or_default();
        pending.generation = self.generation;
        pending.affected = true;
    }
    fn snapshot(&self) -> FrontChanges {
        let mut changes = FrontChanges::default();
        for (&definition, pending) in &self.pending {
            changes.observations.insert(definition, pending.generation);
            match pending.present {
                Some(true) => {
                    changes.changed.insert(definition);
                }
                Some(false) => {
                    changes.removed.insert(definition);
                }
                None => (),
            }
            if pending.affected {
                changes.affected.insert(definition);
            }
        }
        changes
    }
    fn acknowledge(&mut self, observed: &FrontChanges, definitions: &[DefId]) {
        for definition in definitions {
            if let Some(pending) = self.pending.get(definition)
                && observed.observations.get(definition) == Some(&pending.generation)
            {
                self.pending.remove(definition);
            }
        }
    }
}
impl QueryEngine {
    /// Changes not yet incorporated into the caller's compact program products.
    /// The first call starts tracking and returns all indexed definitions as
    /// changed. Signature settling may add affected subscribers; no body memo is read.
    pub fn frontier_changes(&mut self) -> FrontChanges {
        self.note_environment();
        self.settle_signatures();
        if self.front_changes.activate() {
            // A new aggregate has no prior products. Tracking starts on demand;
            // scalar-only sessions retain no unused notification history.
            for path in self.item_paths().into_iter().collect::<Vec<_>>() {
                self.front_changes.changed(DefId::intern(&path), true);
            }
        }
        let changes = self.front_changes.snapshot();
        self.work.frontier_observations += changes.observations.len() as u64;
        changes
    }
    /// One publication's notification. Compact completion consumes only its
    /// owner's generation, leaving every unrelated pending owner untouched.
    pub(crate) fn frontier_change(&mut self, definition: DefId) -> FrontChanges {
        self.note_environment();
        self.settle_signatures();
        let mut changes = FrontChanges::default();
        if let Some(pending) = self.front_changes.pending.get(&definition) {
            self.work.frontier_observations += 1;
            changes.observations.insert(definition, pending.generation);
            match pending.present {
                Some(true) => {
                    changes.changed.insert(definition);
                }
                Some(false) => {
                    changes.removed.insert(definition);
                }
                None => (),
            }
            if pending.affected {
                changes.affected.insert(definition);
            }
        }
        changes
    }
    /// Acknowledge only products actually refreshed from `observed`. Changes
    /// arriving during S1 remain pending for the following frontier.
    pub fn acknowledge_frontier(&mut self, observed: &FrontChanges, definitions: &[DefId]) {
        self.front_changes.acknowledge(observed, definitions);
    }
    /// Starts the next revision: forgets every memo that observed a change
    /// and remembers each forgotten signature's value, so settling recomputes
    /// it and forgets its readers only if the value changed.
    pub(super) fn apply_changes(&mut self, changes: &crate::memo::Changes) {
        if changes.is_empty() {
            return;
        }
        let mut changes = changes.clone();
        // Body-only changes deliberately do not touch broad V inputs.
        // Header/candidate/signature changes can affect every overlay's
        // verdict, even when the base universe remains error-free. A changed
        // signature's views are stale in every universe (`Changes::stales`).
        if changes
            .reads
            .iter()
            .any(|read| matches!(read, ReadKey::Signature(_) | ReadKey::FailedSignature(_)))
        {
            changes.validation_contexts = true;
        }
        self.revision = self.revision.next();
        let stale = self.stale_memos(&changes);
        for key in &stale {
            if matches!(
                key.kind,
                QueryKind::Check | QueryKind::CheckedSummary | QueryKind::Summary
            ) {
                self.source_growth.invalidate(key.def);
            }
            if key.kind == QueryKind::Signature {
                let before = self.signature_value_of(key.def.path());
                self.pending_signatures.entry(key.def).or_insert(before);
            }
        }
        for definition in &changes.texts {
            self.source_growth.invalidate(*definition);
        }
        for key in &stale {
            self.forget(key);
        }
        if self.front_changes.active {
            for definition in stale.iter().map(|key| key.def) {
                self.front_changes.affected(definition);
            }
        }
    }
    /// Every memo that observed one of `changes`, each marked stale for the
    /// holders of its value outside the tables.
    fn stale_memos(&self, changes: &crate::memo::Changes) -> BTreeSet<crate::memo::MemoKey> {
        fn scan<T>(
            table: &Table<T>,
            changes: &crate::memo::Changes,
            stale: &mut BTreeSet<crate::memo::MemoKey>,
        ) {
            for entry in table.values() {
                if changes.stales(&entry.memo.key, &entry.memo.reads) {
                    entry.memo.mark_stale();
                    stale.insert(entry.memo.key);
                }
            }
        }
        let mut stale = BTreeSet::new();
        scan(&self.signatures, changes, &mut stale);
        scan(&self.resolutions, changes, &mut stale);
        scan(&self.checked, changes, &mut stale);
        scan(&self.modes, changes, &mut stale);
        scan(&self.lowered, changes, &mut stale);
        scan(&self.summaries, changes, &mut stale);
        scan(&self.checked_summaries, changes, &mut stale);
        for receipt in self.lower_receipts.values() {
            if changes.stales(&receipt.memo.key, &receipt.memo.reads) {
                receipt.memo.mark_stale();
                stale.insert(receipt.memo.key);
            }
        }
        stale
    }
}
