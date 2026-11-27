//! The free page runs of one band.

use std::collections::BTreeMap;

/// Free runs by start offset; neighbours are always merged.
pub(crate) struct Runs(BTreeMap<usize, usize>);

impl Runs {
    pub(crate) fn new(length: usize) -> Self {
        Self(BTreeMap::from([(0, length)]))
    }

    /// The lowest run of at least `least` bytes, cut to at most `most`.
    pub(crate) fn take(&mut self, least: usize, most: usize) -> Option<(usize, usize)> {
        let (&start, &length) = self.0.iter().find(|(_, length)| **length >= least.max(1))?;
        let taken = length.min(most);
        self.0.remove(&start);
        if taken < length {
            self.0.insert(start + taken, length - taken);
        }
        Some((start, taken))
    }

    pub(crate) fn give(&mut self, mut start: usize, mut length: usize) {
        if length == 0 {
            return;
        }
        if let Some(after) = self.0.remove(&(start + length)) {
            length += after;
        }
        if let Some((&before, &held)) = self.0.range(..start).next_back()
            && before + held == start
        {
            start = before;
            length += held;
        }
        self.0.insert(start, length);
    }
}
