//! The declared-state record (OPTION-L v3.2): one record per shape, the
//! program's declared tables free of their parse ([`hirsel_core_front::Declared`]).
//! A store marks the shape of each successful complete request; the second
//! such request with one shape writes the record and selects it. A refused,
//! incomplete, resource-stopped or reachable-only request, and a request served
//! by snapshot, write nothing. A request whose shape the store selects installs
//! the record in place of declaration, and counts it
//! ([`crate::meter::Counters::declared_installs`]).
use crate::store::Store;
use hirsel_core_ids::ContentHash;
use std::io;

/// The selection naming the published record.
pub(crate) const SELECTION: &str = "core-layers-v1";
/// The selection holding the last successful request's shape mark.
const MARKER: &str = "core-layer-marker-v1";

/// The store's record state for one shape, read once per request.
pub(crate) struct Layer {
    pub(crate) key: ContentHash,
    mark: ContentHash,
    /// The store selects this shape's record.
    pub(crate) selected: bool,
    /// The last successful request had this shape.
    pub(crate) marked: bool,
}
impl Layer {
    pub(crate) fn read(store: &Store, shape: ContentHash) -> io::Result<Self> {
        let key = ContentHash::of_parts("hirsel.core-layer.v1", [shape.0.as_slice()]);
        let mark = ContentHash::of_parts("hirsel.core-layer-mark.v1", [shape.0.as_slice()]);
        Ok(Self {
            key,
            mark,
            selected: store.selection(SELECTION)? == Some(key),
            marked: store.selection(MARKER)? == Some(mark),
        })
    }
    /// The record to write once the request succeeds: declared when the shape
    /// is marked and not yet selected.
    pub(crate) fn wanted(&self) -> bool {
        self.marked && !self.selected
    }
    /// After a successful complete request: publish `record` when given, else
    /// mark the shape.
    pub(crate) fn finish(&self, store: &Store, record: Option<&[u8]>) -> io::Result<()> {
        match record {
            Some(bytes) => {
                store.put_keyed(self.key, bytes)?;
                store.select(SELECTION, self.key)
            }
            None if !self.selected && !self.marked => store.select(MARKER, self.mark),
            None => Ok(()),
        }
    }
}

/// The request-sequence law's view of one request: its shape when it
/// declared, and, when `law` is set, its declared tables.
#[cfg(test)]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Probe {
    /// The law reads this request's tables. Reading them decodes every library
    /// row, so other tests leave it off.
    pub(crate) law: bool,
    /// None when the request declared nothing (served by snapshot).
    pub(crate) shape: Option<ContentHash>,
    /// The store's record stood in for declaration. The meter's install
    /// count cannot say: it counts every request running in the process.
    pub(crate) installed: bool,
    pub(crate) tables: Vec<(&'static str, Vec<String>)>,
}
