//! Opaque private execution evidence, following S1 TaskWorker's acceptance
//! composition. Missing evidence disables reuse; it never disables actual V.
use crate::Verify;
use hirsel_query_products::reads::{ReadKey, Reads};
use hirsel_validate::{AcceptedArtifact, ExecutionReceipt};

/// Unexecuted Lower products retain their validation guards. Only an opaque
/// execution receipt permits replacing those guards with local V revalidation.
pub(super) fn persisted_reads(reads: &Reads, receipt: Option<&ExecutionReceipt>) -> Option<Reads> {
    match receipt {
        Some(receipt) => super::persistent::persisted_execution_reads(reads, Some(receipt)),
        None => Some(reads.clone()),
    }
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
        .observed()
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
