//! Checked execution's declared facts over one immutable signature snapshot.
//! Reading these facts neither checks a body nor lowers a finalizer.
use super::*;
impl SignatureSnapshot {
    /// The checked facts of `closure`, the definitions a lowered program
    /// reaches: only those can be called or dropped while it runs. A
    /// definition this snapshot does not declare contributes nothing.
    pub fn checked_runtime<'a>(
        &self,
        closure: impl IntoIterator<Item = &'a DefPath>,
        forget: Option<&DefPath>,
    ) -> Result<hirsel_interp::checked::CheckedRuntime, QueryError> {
        let mut checked = hirsel_interp::checked::CheckedRuntime {
            arena_with: Some(DefPath::core("arena.with")),
            ..Default::default()
        };
        for path in closure {
            let signature = match self.signature(DefId::intern(path)) {
                Some(signature) => signature?,
                None => match self.lookup.registry.get(path) {
                    Some(signature) => signature.clone(),
                    None => continue,
                },
            };
            if let hirsel_types::SignatureKind::Type(declaration) = &signature.kind {
                if declaration.layout.flat {
                    checked.flat.insert(signature.definition.path().clone());
                }
                if declaration.loop_source.is_some() {
                    checked
                        .operations
                        .insert(signature.definition.path().clone(), signature.clone());
                }
                if declaration.destructor_allocation == hirsel_types::Allocation::NoAlloc {
                    checked
                        .noalloc_drop
                        .insert(signature.definition.path().clone());
                    checked.noalloc.insert(signature.definition.path().clone());
                }
            }
            let hirsel_types::SignatureKind::Function(function) = &signature.kind else {
                continue;
            };
            if function.allocation == hirsel_types::Allocation::NoAlloc {
                checked.noalloc.insert(signature.definition.path().clone());
            }
            checked
                .operations
                .insert(signature.definition.path().clone(), signature.clone());
        }
        if let Some(forget) = forget {
            checked.noalloc.insert(forget.clone());
        }
        Ok(checked)
    }
}
