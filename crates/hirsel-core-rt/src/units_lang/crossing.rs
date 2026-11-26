//! A `cross` value's descriptor, and the recording a helper carries.
use crate::tasks;

/// The owner and padding rows of a `cross` type, as the emitter writes them.
/// Owner rows are `[offset, kind, element bytes, element logical size]`. `at` and `len` are
/// the span of the value a recorder writes: a task entry's `x` inside its
/// wrapped input, else the whole value.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Cross {
    pub bytes: u64,
    pub owners: *const [u64; 4],
    pub count: u64,
    pub at: u64,
    pub len: u64,
}

impl Cross {
    pub(crate) fn owners(&self) -> &[[u64; 4]] {
        if self.count == 0 {
            return &[];
        }
        // SAFETY: the emitter's descriptor holds `count` rows for this call.
        unsafe { std::slice::from_raw_parts(self.owners, self.count as usize) }
    }
    pub(super) fn owned(&self) -> Vec<[u64; 4]> {
        self.owners().to_vec()
    }
}

/// The link's static row for one spawnable `(X, R)`: what a spawn passes
/// and what the link's spawn table holds for each spawnable function, so a
/// replay can start a helper from its entry identity alone. `adapter` is the
/// code word the bridge reads from the packed input's first word.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SpawnEntry {
    pub bridge: tasks::Body,
    pub adapter: u64,
    pub layout: tasks::TaskLayout,
    pub x: Cross,
    pub r: Cross,
}

impl Cross {
    /// The same descriptor: equal words and equal owner rows.
    pub(crate) fn same(&self, other: &Cross) -> bool {
        (self.bytes, self.count, self.at, self.len)
            == (other.bytes, other.count, other.at, other.len)
            && self.owners() == other.owners()
    }
}

impl SpawnEntry {
    /// The same bridge, layout and descriptors; the adapter is the spawn
    /// site's and may be any site's for the same `(X, R)`.
    pub(crate) fn same(&self, other: &SpawnEntry) -> bool {
        let l = |v: &tasks::ValueLayout| (v.bytes, v.align, v.logical, v.clean.map(|c| c as usize));
        std::ptr::fn_addr_eq(self.bridge, other.bridge)
            && l(&self.layout.input) == l(&other.layout.input)
            && l(&self.layout.result) == l(&other.layout.result)
            && self.x.same(&other.x)
            && self.r.same(&other.r)
    }
}

/// A helper's own recording, when its spawner's unit records.
#[cfg(feature = "record")]
pub(super) type TapeHelper = Option<crate::tape::helper::Helper>;
#[cfg(not(feature = "record"))]
pub(super) type TapeHelper = ();

#[cfg(feature = "record")]
pub(super) fn tape_group(tape: &TapeHelper) -> tasks::UnitGroup {
    tape.as_ref().and_then(|h| h.group())
}
#[cfg(not(feature = "record"))]
pub(super) fn tape_group(_: &TapeHelper) -> tasks::UnitGroup {}
