//! Adoption of V's own issued rows from the previous registered generation of
//! the output root that a Precompute is about to register again.
//!
//! The previous generation is read only while this run holds the root's lock
//! and while its registration still authenticates against the root's current
//! products. A lookup that misses in the current generation may find V's own
//! earlier row there. The hit site runs its ordinary check on that row (exact
//! identity, policy and every read, against V's frozen environment) and only
//! then adopts it. Adoption is all or nothing:
//!
//! - every member of the row's closure is copied into owned bytes, checked
//!   against its expected reference, and decoded from exactly those bytes;
//! - when the whole closure is staged, every member is put into the current
//!   generation and each put must return the expected reference;
//! - only then are the row associations exposed.
//!
//! A missing or mismatching member is a refused adoption: the caller runs V.
//! Adoption never replaces fresh retention of the live artifact.
use super::*;
use hirsel_artifact_store::{Bytes, ContentReadError, Reference};
use hirsel_product_codec::{CheckedTir, CheckedTypeComponent, IndexedEnvelope, TypeRecords};
use std::collections::{BTreeMap, BTreeSet};

/// Why one adoption was refused. Each refusal is a miss: V runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AdoptionFailure {
    /// The previous generation holds no row or content for a closure member.
    Missing,
    /// Copied bytes are not the expected reference, or do not decode to the
    /// record their row names.
    Mismatch,
    /// The current generation did not store a member under its reference.
    Refused,
}

#[cfg(test)]
thread_local! {
    /// Refuse the n-th owned copy (0-based) of each adoption as missing.
    pub(super) static FAIL_COPY: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    /// Replace the n-th owned copy's bytes with a forged product whose own
    /// address is recomputed, instead of reading the previous generation.
    pub(super) static FORGE_COPY: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    /// The number of closure members each completed adoption put, in order.
    pub(super) static ADOPTED: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Type component dependency edges the body closure traversed.
    pub(super) static TYPE_EDGES: std::cell::RefCell<Vec<(Reference, Reference)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Owned, checked copies of previous-generation content for one adoption.
struct Copies {
    previous: Arc<installation::Rows>,
    copied: Mutex<BTreeMap<Reference, Bytes>>,
}
impl Copies {
    /// Copy first, then check the expected reference on the owned bytes. The
    /// returned bytes are those owned bytes; no previous mapping is retained.
    fn copy(&self, reference: Reference) -> Result<Bytes, AdoptionFailure> {
        let mut copied = self.copied.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(bytes) = copied.get(&reference) {
            return Ok(bytes.clone());
        }
        #[cfg(test)]
        if FAIL_COPY.with(|fail| fail.get() == Some(copied.len())) {
            return Err(AdoptionFailure::Missing);
        }
        #[allow(unused_mut)]
        let mut owned: Vec<u8> =
            hirsel_artifact_store::Contents::get_content(&*self.previous, reference)
                .map_err(|_| AdoptionFailure::Missing)?
                .to_vec();
        #[cfg(test)]
        if FORGE_COPY.with(|forge| forge.get() == Some(copied.len())) {
            owned.push(0);
        }
        if reference.domain.reference(&owned) != Some(reference) {
            return Err(AdoptionFailure::Mismatch);
        }
        let bytes = Bytes::from(owned);
        copied.insert(reference, bytes.clone());
        Ok(bytes)
    }
}
/// Type traversal reads components only through the owned copies.
impl hirsel_artifact_store::Contents for Copies {
    fn get_content(&self, reference: Reference) -> Result<Bytes, ContentReadError> {
        self.copy(reference)
            .map_err(|_| ContentReadError::Refused("previous generation adoption"))
    }
    fn put_content(&self, _: Domain, _: &[u8]) -> Option<Reference> {
        None
    }
    fn put_identified_content(
        &self,
        _: hirsel_artifact_store::IdentifiedContent<'_>,
    ) -> Option<Reference> {
        None
    }
}

/// One adoption's staged closure: owned contents and the rows naming them.
pub(super) struct Staging {
    copies: Arc<Copies>,
    rows: Vec<(ContentHash, Reference)>,
}
impl Staging {
    /// Stage one previous row and the owned copy of the content it names.
    fn row(
        &mut self,
        key: ContentHash,
        domain: Domain,
    ) -> Result<(Reference, Bytes), AdoptionFailure> {
        let reference = self
            .copies
            .previous
            .row(key)
            .ok_or(AdoptionFailure::Missing)?;
        if reference.domain != domain {
            return Err(AdoptionFailure::Mismatch);
        }
        let bytes = self.copies.copy(reference)?;
        self.rows.push((key, reference));
        Ok((reference, bytes))
    }
    /// Stage a row and decode exactly its owned, checked bytes.
    pub(super) fn record<T: serde::de::DeserializeOwned>(
        &mut self,
        key: ContentHash,
        domain: Domain,
    ) -> Result<(Reference, T), AdoptionFailure> {
        let (reference, bytes) = self.row(key, domain)?;
        let value = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(|_| AdoptionFailure::Mismatch)?;
        Ok((reference, value))
    }
    /// Stage the original receipt association of an issued action.
    pub(super) fn original(
        &mut self,
        artifact: ContentHash,
        policy: ContentHash,
        action: ContentHash,
    ) -> Result<(), AdoptionFailure> {
        let (_, receipt): (_, Receipt) = self.record(original_receipt_key(action), DOMAIN)?;
        if receipt.artifact != artifact
            || receipt.policy != policy
            || receipt.action != action
            || receipt.action != receipt_action(artifact, policy, &receipt.reads)
        {
            return Err(AdoptionFailure::Mismatch);
        }
        Ok(())
    }
    /// Stage a replay record, its checked body and the complete type closure
    /// of both the body and the overlay record. Returns the record reference.
    pub(super) fn replay(
        &mut self,
        types: &Arc<TypeInterner>,
        artifact: ContentHash,
        policy: ContentHash,
    ) -> Result<Reference, AdoptionFailure> {
        let (reference, record): (_, ReplayRecord) =
            self.record(replay_key(artifact, policy), REPLAY_DOMAIN)?;
        if record.artifact != artifact
            || record.policy != policy
            || record.body.domain != CheckedTir::DOMAIN
        {
            return Err(AdoptionFailure::Mismatch);
        }
        let body = self.copies.copy(record.body)?;
        let envelope = IndexedEnvelope::open(body, record.body).ok_or(AdoptionFailure::Mismatch)?;
        let mut pending = (0..envelope.type_count())
            .map(|n| envelope.type_reference(n))
            .collect::<Option<Vec<_>>>()
            .ok_or(AdoptionFailure::Mismatch)?;
        let mut seen = BTreeSet::new();
        while let Some(component) = pending.pop() {
            if !seen.insert(component.reference()) {
                continue;
            }
            let bytes = self.copies.copy(component.reference())?;
            let checked =
                CheckedTypeComponent::open(bytes, component).ok_or(AdoptionFailure::Mismatch)?;
            for n in 0..checked.dependency_count() {
                let dependency = checked.dependency(n).ok_or(AdoptionFailure::Mismatch)?;
                #[cfg(test)]
                TYPE_EDGES.with(|log| {
                    log.borrow_mut()
                        .push((component.reference(), dependency.reference()))
                });
                pending.push(dependency);
            }
        }
        let records = TypeRecords::new(types.clone(), self.copies.clone());
        let mut failure = None;
        let visited = records.visit_record_types(&record.bytes, |component| {
            match self.copies.copy(component) {
                Ok(_) => Some(seen.insert(component)),
                Err(error) => {
                    failure = Some(error);
                    None
                }
            }
        });
        match (visited, failure) {
            (Some(()), _) => Ok(reference),
            (None, Some(error)) => Err(error),
            (None, None) => Err(AdoptionFailure::Mismatch),
        }
    }
}

impl VerifierCache {
    /// A new staging over the previous registered generation, when this cache
    /// is a Precompute's installation that found one.
    pub(super) fn staging(&self) -> Option<Staging> {
        let previous = match &self.0 {
            Backend::Hosted(_, _, Some(previous)) => previous,
            // A request transport adopts into the installation it selected;
            // its own rows then copy from there like any selected row.
            Backend::Transport(selected, _, _) => return selected.staging(),
            _ => return None,
        };
        Some(Staging {
            copies: Arc::new(Copies {
                previous: previous.clone(),
                copied: Mutex::default(),
            }),
            rows: vec![],
        })
    }
    /// Adopt an execution record with every artifact's original receipt and
    /// replay closure, only when each context has V's frozen environment.
    pub(super) fn adopt_execution(
        &self,
        subject: ContentHash,
        types: &Arc<TypeInterner>,
        environment: &dyn ReplayEnvironment,
    ) -> Result<(), AdoptionFailure> {
        let mut staging = self.staging().ok_or(AdoptionFailure::Missing)?;
        let (_, record): (_, ExecutionRecord) =
            staging.record(execution_key(subject), EXECUTION_DOMAIN)?;
        if record.subject != subject || record.artifacts.is_empty() || record.contexts.is_empty() {
            return Err(AdoptionFailure::Mismatch);
        }
        for (package, _) in &record.contexts {
            match environment.checked(package) {
                Ok(Some(input)) if Arc::ptr_eq(&input.stamp().types, types) => {}
                _ => return Err(AdoptionFailure::Refused),
            }
        }
        for &(artifact, policy, action) in &record.artifacts {
            staging.original(artifact, policy, action)?;
            staging.replay(types, artifact, policy)?;
        }
        self.adopt(staging)
    }
    /// Put the whole staged closure into the current generation, checking that
    /// every put returns its expected reference, and only then expose its rows.
    pub(super) fn adopt(&self, staging: Staging) -> Result<(), AdoptionFailure> {
        let (rows, contents) = match &self.0 {
            Backend::Hosted(rows, contents, Some(_)) => (rows, contents),
            Backend::Transport(selected, _, _) => return selected.adopt(staging),
            _ => return Err(AdoptionFailure::Refused),
        };
        let _batch = rows.publication_batch();
        let copied = std::mem::take(
            &mut *staging
                .copies
                .copied
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for (reference, bytes) in &copied {
            let content = reference
                .domain
                .identify(bytes)
                .ok_or(AdoptionFailure::Refused)?;
            if contents.put_identified_content(content) != Some(*reference) {
                return Err(AdoptionFailure::Refused);
            }
        }
        #[cfg(test)]
        ADOPTED.with(|log| log.borrow_mut().push(copied.len()));
        for (key, reference) in staging.rows {
            rows.set_row(key, reference);
        }
        Ok(())
    }
}
