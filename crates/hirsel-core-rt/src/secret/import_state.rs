use super::*;
use hirsel_core_net::{ImportKind, ImportRefusal, ImportUse};

impl Secret {
    pub(crate) fn new(
        origin: Origin,
        policy: SecretPolicy,
        bytes: Option<Arc<SecretBytes>>,
        seal: Option<Arc<ChargeCell>>,
    ) -> Self {
        let import = ImportSpend::new(policy.uses).map(Arc::new);
        Self {
            origin,
            policy,
            bytes,
            seal,
            import,
        }
    }

    pub(crate) fn begin_import<T, E>(
        &self,
        kind: ImportKind,
        prepare: impl FnOnce() -> Result<T, E>,
    ) -> Result<(ImportUse, T), ImportRefusal<E>> {
        if self
            .policy
            .uses
            .import()
            .is_none_or(|use_| use_.kind() != kind)
        {
            return Err(ImportRefusal::Denied);
        }
        self.import
            .as_ref()
            .ok_or(ImportRefusal::Denied)?
            .issue(kind, prepare)
    }
}
