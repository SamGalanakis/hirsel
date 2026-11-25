//! The evaluator's block table: block records indexed by a monotonically
//! assigned identity, with only live blocks occupying storage.

use super::{Cleanup, Counted, Limit, Res};
use std::collections::BTreeMap;

pub(super) struct Blk {
    pub(super) bytes: Vec<u8>,
    pub(super) align: u64,
    pub(super) live: bool,
    /// The limit the block is charged to; None for frame slots and engine records.
    pub(super) limit: Option<Limit>,
    pub(super) charge: u64,
    pub(super) lent_size: Option<u64>,
    pub(super) res: Option<Res>,
    /// A counted node's kind, fixed by its `alloc`; its operations must name it.
    pub(super) counted: Option<Counted>,
    /// The cleanup class its `alloc` named (crate docs, "Cleanup classes"): `None`
    /// for a block no `alloc` row made; `Some(None)` for region storage.
    pub(super) class: Option<Option<Cleanup>>,
    /// Static data (`static dN`): read, never written or freed.
    pub(super) fixed: bool,
    /// The liveness shadow of `mark` (crate docs, "Slot kernels"): each marked
    /// slot's offset, whether it holds a value, and its size.
    pub(super) marks: BTreeMap<usize, (bool, usize)>,
}
// A pointer's upper word is a monotonically assigned block identity, not a
// storage slot. Only live blocks occupy records. Absence is the tombstone.
// Completed empty index chunks are discarded; the top directory is bounded by
// the 32-bit identity space. One live block can pin one 4096-entry u32 chunk.
const BLOCK_CHUNK: usize = 4096;
pub(super) struct BlockChunk {
    pub(super) slots: Box<[u32; BLOCK_CHUNK]>,
    pub(super) live: usize,
}
#[derive(Default)]
pub(super) struct Blocks {
    pub(super) next: usize,
    pub(super) chunks: Vec<Option<Box<BlockChunk>>>,
    pub(super) entries: Vec<(usize, Blk)>,
}
impl Blocks {
    pub(super) fn insert(&mut self, block: Blk) -> usize {
        assert!(
            self.next < u32::MAX as usize,
            "evaluator block identity space exhausted (a run stop, not Capacity)"
        );
        let id = self.next;
        self.next += 1;
        let c = id / BLOCK_CHUNK;
        if c == self.chunks.len() {
            self.chunks.push(Some(Box::new(BlockChunk {
                slots: Box::new([0; BLOCK_CHUNK]),
                live: 0,
            })));
        }
        let chunk = self.chunks[c].as_mut().unwrap();
        chunk.slots[id % BLOCK_CHUNK] = (self.entries.len() + 1) as u32;
        chunk.live += 1;
        self.entries.push((id, block));
        id
    }
    pub(super) fn slot(&self, id: usize) -> Option<usize> {
        let chunk = self.chunks.get(id / BLOCK_CHUNK)?.as_ref()?;
        let slot = chunk.slots[id % BLOCK_CHUNK];
        (slot != 0).then_some(slot.wrapping_sub(1) as usize)
    }
    pub(super) fn remove(&mut self, id: usize) -> Blk {
        let slot = self.slot(id).expect("free of a non-block");
        let chunk = self.chunks[id / BLOCK_CHUNK].as_mut().unwrap();
        chunk.slots[id % BLOCK_CHUNK] = 0;
        chunk.live -= 1;
        if chunk.live == 0 && id / BLOCK_CHUNK < self.next / BLOCK_CHUNK {
            self.chunks[id / BLOCK_CHUNK] = None;
        }
        let (_, block) = self.entries.swap_remove(slot);
        if let Some(&(moved, _)) = self.entries.get(slot) {
            self.chunks[moved / BLOCK_CHUNK].as_mut().unwrap().slots[moved % BLOCK_CHUNK] =
                (slot + 1) as u32;
        }
        // Bound record storage by current liveness, not the historical peak.
        // Hysteresis keeps repeated allocation/free at the boundary amortized.
        if self.entries.capacity() > 256 && self.entries.len() < self.entries.capacity() / 4 {
            self.entries.shrink_to((2 * self.entries.len()).max(256));
        }
        block
    }
}
// Absent blocks retain only their liveness fact. No diagnostic depends on the
// retired bytes, size, limit or origin. Mutable indexing always follows a live
// check, and cannot create an absent entry.
static DEAD_BLOCK: Blk = Blk {
    bytes: Vec::new(),
    align: 8,
    live: false,
    limit: None,
    charge: 0,
    lent_size: None,
    res: None,
    counted: None,
    class: None,
    fixed: false,
    marks: BTreeMap::new(),
};
impl std::ops::Index<usize> for Blocks {
    type Output = Blk;
    fn index(&self, id: usize) -> &Blk {
        self.slot(id)
            .map_or(&DEAD_BLOCK, |slot| &self.entries[slot].1)
    }
}
impl std::ops::IndexMut<usize> for Blocks {
    fn index_mut(&mut self, id: usize) -> &mut Blk {
        let slot = self.slot(id).expect("use of a freed block");
        &mut self.entries[slot].1
    }
}

#[cfg(test)]
#[path = "block_table_tests.rs"]
mod block_table_tests;
