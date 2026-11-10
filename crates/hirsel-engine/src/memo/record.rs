//! Memo identity and records (design §1b). A memo holds its value with the
//! typed reads it observed; a change forgets every memo that read it.
use super::{ReadKey, Reads};
use hirsel_query_products::QueryKind;
use hirsel_syntax::{ContentHash, DefId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// One memo's identity: its query kind and its definition.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MemoKey {
    pub kind: QueryKind,
    pub def: DefId,
}
impl MemoKey {
    pub fn new(kind: QueryKind, def: DefId) -> Self {
        Self { kind, def }
    }
}

/// A memo's value with its record (design §3). The value is shared, and the
/// reads are stored once.
pub struct Memo<T> {
    pub key: MemoKey,
    /// The definition's own content: the text hash the result was computed from.
    pub text: ContentHash,
    pub value: Result<Arc<T>, hirsel_types::QueryError>,
    pub reads: Arc<Reads>,
    /// Set when a change this memo observed forgot it: holders of the value
    /// outside its table see it as stale.
    stale: AtomicBool,
}
impl<T> Memo<T> {
    pub fn new(
        key: MemoKey,
        text: ContentHash,
        value: Result<Arc<T>, hirsel_types::QueryError>,
        reads: Arc<Reads>,
    ) -> Self {
        Self {
            key,
            text,
            value,
            reads,
            stale: AtomicBool::new(false),
        }
    }
    pub fn is_stale(&self) -> bool {
        self.stale.load(Relaxed)
    }
    pub(crate) fn mark_stale(&self) {
        self.stale.store(true, Relaxed);
    }
    /// Project a retained value, keeping its identity and reads.
    pub(crate) fn map_value<U>(&self, map: impl FnOnce(&T) -> U) -> Memo<U> {
        Memo {
            key: self.key,
            text: self.text,
            value: self
                .value
                .as_ref()
                .map(|value| Arc::new(map(value)))
                .map_err(Clone::clone),
            reads: self.reads.clone(),
            stale: AtomicBool::new(self.is_stale()),
        }
    }
}
impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        Self {
            key: self.key,
            text: self.text,
            value: self.value.clone(),
            reads: self.reads.clone(),
            stale: AtomicBool::new(self.is_stale()),
        }
    }
}

/// The inputs whose observed value changed, and the definitions whose own
/// text changed. Every memo that read one of them, or belongs to one of
/// them, is stale.
#[derive(Clone, Debug, Default)]
pub struct Changes {
    pub reads: BTreeSet<ReadKey>,
    /// Settled input values: a reader that already observed these stays current.
    pub values: BTreeMap<ReadKey, ContentHash>,
    pub texts: BTreeSet<DefId>,
    /// A name appeared, disappeared or changed kind or visibility: every
    /// lookup observation is stale.
    pub lookups: bool,
    /// Header, candidate or signature changes can affect every V overlay's
    /// verdict: every broad V input observation is stale.
    pub validation_contexts: bool,
}
impl Changes {
    pub fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.texts.is_empty() && !self.lookups && !self.validation_contexts
    }
    pub fn read(&mut self, key: ReadKey) {
        self.values.remove(&key);
        self.reads.insert(key);
    }
    pub(crate) fn read_value(&mut self, key: ReadKey, value: Option<ContentHash>) {
        self.read(key);
        if let Some(value) = value {
            self.values.insert(key, value);
        }
    }
    pub fn text(&mut self, def: DefId) {
        self.texts.insert(def);
    }
    /// Whether the memo `key` with `reads` observed a change.
    pub fn stales(&self, key: &MemoKey, reads: &Reads) -> bool {
        self.texts.contains(&key.def)
            || reads.iter().any(|(read, value)| {
                (self.reads.contains(read) && self.values.get(read) != Some(value))
                    || (self.lookups && matches!(read, ReadKey::Lookup(_)))
                    || (self.validation_contexts
                        && matches!(
                            read,
                            ReadKey::ValidationUniverse(_)
                                | ReadKey::UniverseErrors
                                | ReadKey::HeapRole
                        ))
            })
    }
}
