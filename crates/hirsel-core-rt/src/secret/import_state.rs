use super::*;
use hirsel_core_net::{ImportKind, ImportRefusal, ImportUse, Spending};

impl Secret {
    /// An import secret's bytes move into its spent mark, which every
    /// designation shares and the import zeroes (TLS-8, imports), so the
    /// secret itself holds none.
    pub(crate) fn new(
        origin: Origin,
        policy: SecretPolicy,
        bytes: Option<Arc<SecretBytes>>,
        seal: Option<Arc<ChargeCell>>,
    ) -> Self {
        let (bytes, import) = if policy.import().is_some() {
            let owned = bytes.map(|bytes| {
                Arc::try_unwrap(bytes)
                    .unwrap_or_else(|_| unreachable!("an import secret is made from fresh bytes"))
                    .into_bytes()
            });
            (None, ImportSpend::new(&policy, owned).map(Arc::new))
        } else {
            (bytes, None)
        };
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
    ) -> Result<(ImportUse, T, Spending<'_>), ImportRefusal<E>> {
        self.import
            .as_ref()
            .ok_or(ImportRefusal::Denied)?
            .issue(kind, prepare)
    }
}
