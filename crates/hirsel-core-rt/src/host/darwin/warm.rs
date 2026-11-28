//! Warm coverage feedback's host mechanisms: Darwin has neither yet, so a
//! warm worker refuses before it greets and the driver runs fresh children.

pub(crate) fn main_entry() -> Option<usize> {
    None
}

pub(crate) fn counters() -> [u64; 6] {
    [0; 6]
}
