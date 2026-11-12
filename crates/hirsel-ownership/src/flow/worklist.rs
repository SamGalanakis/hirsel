//! The minimum dirty reverse-postorder rank, as in rustc 19833af7
//! compiler/rustc_mir_dataflow/src/framework/mod.rs:290-377. Insertion
//! revisits an earlier rank immediately; duplicate insertions have no effect.
pub(super) struct Pending {
    words: Vec<u64>,
    next: usize,
}
impl Pending {
    pub(super) fn new(count: usize) -> Self {
        Self {
            words: vec![0; count.div_ceil(64)],
            next: count,
        }
    }
    pub(super) fn insert(&mut self, rank: usize) {
        self.words[rank / 64] |= 1 << (rank % 64);
        self.next = self.next.min(rank);
    }
    pub(super) fn pop_first(&mut self) -> Option<usize> {
        while let Some(word) = self.words.get_mut(self.next / 64) {
            if *word != 0 {
                let rank = self.next / 64 * 64 + word.trailing_zeros() as usize;
                *word &= *word - 1;
                self.next = rank;
                return Some(rank);
            }
            self.next = (self.next / 64 + 1) * 64;
        }
        None
    }
}
