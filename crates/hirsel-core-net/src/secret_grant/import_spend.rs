use super::{ImportUse, SecretPolicy};
use std::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Srtp,
    Traffic,
    Sframe,
}

impl ImportUse {
    pub fn kind(self) -> ImportKind {
        match self {
            Self::SrtpSend | Self::SrtpReceive => ImportKind::Srtp,
            Self::TrafficSend | Self::TrafficReceive => ImportKind::Traffic,
            Self::Sframe(_) => ImportKind::Sframe,
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
///
/// The mark also holds the run-owned bytes of every alias (TLS-8, imports):
/// no other operation can read an import secret's bytes, and the import that
/// spends them zeroes them in place before it returns.
pub struct ImportSpend {
    authority: ImportUse,
    spent: Mutex<bool>,
    bytes: Mutex<Option<Zeroizing<Vec<u8>>>>,
}

/// The spent bytes, lent to the import that spent them. Dropping it zeroes
/// them, on every path out of the import.
pub struct Spending<'a>(&'a ImportSpend);

impl Spending<'_> {
    /// The bytes, or `None` for an opaque designation.
    pub fn bytes<R>(&self, f: impl FnOnce(Option<&[u8]>) -> R) -> R {
        f(self.0.bytes.lock().unwrap().as_deref().map(Vec::as_slice))
    }
}

impl Drop for Spending<'_> {
    fn drop(&mut self) {
        if let Some(bytes) = self.0.bytes.lock().unwrap().as_mut() {
            // Zeroes the whole allocation, which stays owned until the mark drops.
            Vec::zeroize(bytes);
        }
    }
}

impl ImportSpend {
    pub fn new(policy: &SecretPolicy, bytes: Option<Zeroizing<Vec<u8>>>) -> Option<Self> {
        Some(Self {
            authority: policy.import()?,
            spent: Mutex::new(false),
            bytes: Mutex::new(bytes),
        })
    }

    /// `prepare` checks the profile and admits the schedule. It runs once
    /// at most on a successful issue. Validation of key bytes follows issue,
    /// through the returned `Spending`.
    pub fn issue<T, E>(
        &self,
        kind: ImportKind,
        prepare: impl FnOnce() -> Result<T, E>,
    ) -> Result<(ImportUse, T, Spending<'_>), ImportRefusal<E>> {
        if self.authority.kind() != kind {
            return Err(ImportRefusal::Denied);
        }
        let mut spent = self.spent.lock().unwrap();
        if *spent {
            return Err(ImportRefusal::Spent);
        }
        let admitted = prepare().map_err(ImportRefusal::BeforeIssue)?;
        *spent = true;
        Ok((self.authority, admitted, Spending(self)))
    }

    /// The allocation behind the bytes, which lives as long as the mark: the
    /// erasure laws read it after an import (TLS-8 PC11).
    pub fn allocation(&self) -> Option<(*const u8, usize)> {
        self.bytes
            .lock()
            .unwrap()
            .as_ref()
            .map(|bytes| (bytes.as_ptr(), bytes.capacity()))
    }
}
