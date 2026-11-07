//! Live V capabilities use the same exact context boundary as VerifiedDefinition.
use super::*;
use hirsel_syntax::DefId;

impl VerifierCache {
    pub(super) fn live_acceptance(
        &self,
        accepted: &AcceptedArtifact,
        artifact: &Artifact<'_>,
        signatures: crate::SignatureInputs<'_>,
    ) -> Option<Arc<LiveAcceptance>> {
        let stamp = signatures.stamp()?;
        if !Arc::ptr_eq(&stamp.types, &artifact.types) {
            return None;
        }
        let bytes = crate::memory::arc::<LiveAcceptance>().checked_add(
            (artifact.owners.len() as u64).checked_mul(size_of::<(Anchor, DefId, u32)>() as u64)?,
        )?;
        // Optional admission may miss; it cannot turn an operational failure
        // into a semantic verdict or create an uncharged live cache owner.
        let lease = self.retain_fact(bytes).ok().flatten()?;
        let anchors = artifact
            .owners
            .iter()
            .map(|(anchor, (path, occurrence))| (*anchor, DefId::intern(path), *occurrence))
            .collect();
        Some(Arc::new(LiveAcceptance {
            binding: (accepted.artifact, accepted.policy, accepted.receipt),
            issuer: Arc::downgrade(&self.2),
            types: Arc::downgrade(&artifact.types),
            environment: Arc::downgrade(&stamp.identity),
            owner: artifact.definition.header()?.path,
            overlay: artifact.overlay.as_ref().map(Arc::downgrade),
            anchors,
            _lease: lease,
        }))
    }

    pub(super) fn live_execution(
        &self,
        original: &ExecutionReceipt,
        types: &Arc<TypeInterner>,
        anchor: &mut impl FnMut(&DefPath, u32) -> Option<Anchor>,
        environment: &dyn ReplayEnvironment,
    ) -> Result<bool, crate::FactError> {
        use crate::FactError;
        if original.artifacts.is_empty()
            || original.contexts.is_empty()
            || original
                .artifacts
                .iter()
                .any(|accepted| accepted.live.is_none())
        {
            return Ok(false);
        }
        for (package, digest) in original.contexts.iter() {
            let Some(base) = environment.checked(package).map_err(FactError::View)? else {
                return Ok(false);
            };
            if !Arc::ptr_eq(&base.stamp().types, types)
                || base.input_digest() != *digest
                || environment.base_digest(package).map_err(FactError::View)? != Some(*digest)
            {
                return Ok(false);
            }
            let mut covered = false;
            for accepted in original.artifacts.iter() {
                let live = accepted.live.as_ref().unwrap();
                if &live.owner.path().0.package != package {
                    continue;
                }
                covered = true;
                let binding = (accepted.artifact, accepted.policy, accepted.receipt);
                let issued = &accepted.original.receipt;
                if live.binding != binding
                    || (issued.artifact, issued.policy, issued.action) != binding
                    || !live.issuer.ptr_eq(&Arc::downgrade(&self.2))
                    || !live.types.ptr_eq(&Arc::downgrade(types))
                    || live.environment.strong_count() == 0
                {
                    return Ok(false);
                }
                let overlay;
                let inputs = match &live.overlay {
                    Some(signature) => {
                        let Some(signature) = signature.upgrade() else {
                            return Ok(false);
                        };
                        let Some(views) = environment
                            .checked_overlay(package, signature.clone())
                            .map_err(FactError::View)?
                        else {
                            return Ok(false);
                        };
                        if views.origin_input() != *digest
                            || !views
                                .replacement(signature.definition)
                                .is_some_and(|view| view.matches(types, &signature))
                        {
                            return Ok(false);
                        }
                        overlay = views;
                        &*overlay as &dyn crate::CheckedEnvironment
                    }
                    None => &*base,
                };
                if !Arc::ptr_eq(&inputs.stamp().types, types)
                    || !live
                        .environment
                        .ptr_eq(&Arc::downgrade(&inputs.stamp().identity))
                    || live.anchors.iter().any(|(site, owner, occurrence)| {
                        anchor(owner.path(), *occurrence) != Some(*site)
                    })
                {
                    return Ok(false);
                }
            }
            if !covered {
                return Ok(false);
            }
        }
        Ok(original.artifacts.iter().all(|accepted| {
            let package = &accepted.live.as_ref().unwrap().owner.path().0.package;
            original.contexts.iter().any(|(owner, _)| owner == package)
        }))
    }
}
