use super::{ImportUse, SecretUse};
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Srtp,
    Traffic,
}

impl ImportUse {
    pub fn kind(self) -> ImportKind {
        match self {
            Self::SrtpSend | Self::SrtpReceive => ImportKind::Srtp,
            Self::TrafficSend | Self::TrafficReceive => ImportKind::Traffic,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImportRefusal<E> {
    Denied,
    Spent,
    BeforeIssue(E),
}

/// One mark belongs to the backing of all aliases, including opaque replay
/// aliases. Admission and spending are one transition across those aliases.
pub struct ImportSpend {
    authority: ImportUse,
    spent: Mutex<bool>,
}

impl ImportSpend {
    pub fn new(uses: SecretUse) -> Option<Self> {
        Some(Self {
            authority: uses.import()?,
            spent: Mutex::new(false),
        })
    }

    /// `prepare` checks the profile and admits the schedule. It runs once
    /// at most on a successful issue. Validation of key bytes follows issue.
    pub fn issue<T, E>(
        &self,
        kind: ImportKind,
        prepare: impl FnOnce() -> Result<T, E>,
    ) -> Result<(ImportUse, T), ImportRefusal<E>> {
        if self.authority.kind() != kind {
            return Err(ImportRefusal::Denied);
        }
        let mut spent = self.spent.lock().unwrap();
        if *spent {
            return Err(ImportRefusal::Spent);
        }
        let admitted = prepare().map_err(ImportRefusal::BeforeIssue)?;
        *spent = true;
        Ok((self.authority, admitted))
    }
}
