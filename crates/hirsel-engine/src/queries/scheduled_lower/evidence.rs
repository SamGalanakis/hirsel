//! Opaque private execution evidence, following S1 TaskWorker's acceptance
//! composition. Missing evidence disables reuse; it never disables actual V.
use crate::Verify;
use hirsel_query_products::reads::{ReadKey, Reads};
use hirsel_syntax::ContentHash;
use hirsel_validate::{AcceptedArtifact, ExecutionReceipt};

/// Unexecuted Lower products retain their validation guards. Only an opaque
/// execution receipt permits replacing those guards with local V revalidation.
pub(in crate::queries) fn persisted_reads(
    reads: &Reads,
    receipt: Option<&ExecutionReceipt>,
) -> Option<Reads> {
    match receipt {
        Some(receipt) => super::persistent::persisted_execution_reads(reads, Some(receipt)),
        None => Some(reads.clone()),
    }
}

/// An unexecuted body's validation observations: exactly what V read for its
/// own artifact (`reads` of its `VerifiedDefinition`), in place of the owner's
/// package-wide universe digest.
///
/// V validated the body against the owner package's base universe, and each
/// `VRead` becomes the engine read of that same universe, naming the owner
/// package: a view digest, the heap-role fingerprint and the universe-errors
/// digest. The package is explicit, so the reads keep their meaning when a
/// consumer in another package merges them (execution-read unions, generator
/// acceptance). Executed bodies keep their universe contexts,
/// which V's receipt replay re-observes through each artifact's own (possibly
/// private-overlay) universe. Producer reads (front, layouts, registrations,
/// selections, executed bodies) are untouched.
///
/// `None` keeps the package-wide guard: a body whose reads would conflict, that
/// read another package's universe, or for which V read an absent view. An
/// accepted unexecuted body never has one: the universe holds a view of every
/// definition whose signature succeeded (snapshot `build_validation`), and
/// front refuses a body naming a definition whose signature failed.
pub(in crate::queries) fn narrowed_reads(
    reads: &Reads,
    owner: &hirsel_syntax::DefPath,
    validation: &[hirsel_validate::VRead],
) -> Option<Reads> {
    let package = hirsel_query_products::reads::PackageKey::of(&owner.0.package);
    let guard = ReadKey::ValidationUniverse(package);
    let mut narrowed = hirsel_query_products::reads::ReadsBuilder::new();
    for (key, value) in reads.iter() {
        match key {
            key if *key == guard => {}
            ReadKey::ValidationUniverse(_) => return None,
            _ => narrowed.record(*key, *value),
        }
    }
    for read in validation {
        let (key, value) = match read {
            hirsel_validate::VRead::View(d, digest) => {
                (ReadKey::ValidationView(package, *d), digest.0)
            }
            hirsel_validate::VRead::Missing(_) => return None,
            hirsel_validate::VRead::HeapRole(observation) => {
                (ReadKey::HeapRole(package), observation.fingerprint())
            }
            hirsel_validate::VRead::UniverseErrors(digest) => {
                (ReadKey::UniverseErrors(package), *digest)
            }
        };
        narrowed.record(key, value);
    }
    narrowed.finish().ok()
}

/// V's context for an unexecuted body's completion: the owner package and a
/// digest of every validation observation in the body's reads, in canonical
/// order. Those observations are exactly what V read (or the package-wide
/// guard, for a body that was not narrowed), so the completion reopens
/// whenever they all still hold and never at a bare package name.
pub(in crate::queries) fn completion_context(
    owner: &hirsel_syntax::PackageId,
    reads: &Reads,
) -> (hirsel_syntax::PackageId, ContentHash) {
    let mut bytes = Vec::new();
    for (key, value) in reads.iter() {
        if matches!(
            key,
            ReadKey::ValidationView(..)
                | ReadKey::HeapRole(_)
                | ReadKey::UniverseErrors(_)
                | ReadKey::ValidationUniverse(_)
        ) {
            key.encode(&mut bytes);
            bytes.extend(value.0);
        }
    }
    (
        owner.clone(),
        ContentHash::of_parts(
            "hirsel.front.lower.completion-context.v1",
            [bytes.as_slice()],
        ),
    )
}

/// Only V's current opaque receipt restores archived validation observations.
pub(in crate::queries) fn current_reads(
    reads: Reads,
    receipt: Option<&ExecutionReceipt>,
) -> Option<Reads> {
    let Some(receipt) = receipt else {
        return Some(reads);
    };
    let current = |key: &ReadKey, digest| match key {
        ReadKey::ValidationUniverse(package) => receipt
            .contexts()
            .iter()
            .find(|(id, _)| *id == package.id())
            .map(|(_, value)| *value),
        _ => Some(digest),
    };
    if receipt.contexts().iter().all(|(package, digest)| {
        reads.iter().any(|(key, value)| {
            *key == ReadKey::ValidationUniverse(hirsel_query_products::reads::PackageKey::of(
                package,
            )) && value == digest
        })
    }) && reads
        .iter()
        .all(|(key, digest)| current(key, *digest) == Some(*digest))
    {
        return Some(reads);
    }
    let mut values = reads
        .iter()
        .map(|(key, digest)| Some((*key, current(key, *digest)?)))
        .collect::<Option<Vec<_>>>()?;
    for (package, digest) in receipt.contexts() {
        let key =
            ReadKey::ValidationUniverse(hirsel_query_products::reads::PackageKey::of(package));
        if !values.iter().any(|(selected, _)| *selected == key) {
            values.push((key, *digest));
        }
    }
    Reads::from_vec(values).ok()
}
pub(super) struct Evidence {
    pub(super) executed: bool,
    proofs: Option<Vec<AcceptedArtifact>>,
    max_proofs: usize,
}
impl Evidence {
    pub(super) fn join(
        &mut self,
        successor: &super::SuccessorCapture,
        owner: &hirsel_syntax::DefPath,
        reads: &Reads,
        verify: Verify,
    ) -> Result<Reads, crate::QueryError> {
        let execution = self.finish(reads);
        if verify == Verify::On && self.executed && execution.is_none() {
            return Err(super::successor::refused(
                "executed artifacts lack join evidence",
            ));
        }
        let (reads, execution) = successor.revalidate(owner, reads, execution.as_ref())?;
        if let Some(execution) = execution {
            self.proofs = Some(execution.artifacts().to_vec());
        }
        Ok(reads)
    }
    pub(super) fn new(verify: Verify, bytes: u64) -> Self {
        Self {
            executed: false,
            proofs: (verify == Verify::On).then(Vec::new),
            max_proofs: (bytes / (std::mem::size_of::<AcceptedArtifact>() as u64 + 32))
                .try_into()
                .unwrap_or(usize::MAX),
        }
    }
    pub(super) fn accepted(&mut self, proof: Option<AcceptedArtifact>) {
        let Some(proof) = proof else {
            self.proofs = None;
            return;
        };
        let Some(proofs) = &mut self.proofs else {
            return;
        };
        if !proofs.contains(&proof) {
            if proofs.len() == self.max_proofs {
                self.proofs = None;
            } else {
                proofs.push(proof);
            }
        }
    }
    pub(super) fn dependency(&mut self, executed: bool, receipt: Option<&ExecutionReceipt>) {
        if !executed {
            return;
        }
        self.executed = true;
        if let Some(receipt) = receipt {
            for proof in receipt.artifacts() {
                self.accepted(Some(proof.clone()));
            }
        } else {
            self.proofs = None;
        }
    }
    pub(super) fn finish(&self, reads: &Reads) -> Option<ExecutionReceipt> {
        if !self.executed {
            return None;
        }
        let proofs = self.proofs.as_ref()?;
        let mut contexts: Vec<_> = reads
            .iter()
            .filter_map(|(key, digest)| match key {
                ReadKey::ValidationUniverse(package) => Some((package.id(), *digest)),
                _ => None,
            })
            .collect();
        contexts.sort_unstable();
        ExecutionReceipt::compose(&contexts, proofs)
    }
}
