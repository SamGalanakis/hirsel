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
