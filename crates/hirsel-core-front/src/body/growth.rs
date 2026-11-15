//! The growth path is cold. A sequence or map grows only when it is full, and
//! the front lowers both words a full test reads (`len`/`capacity` of a List,
//! `slots`/`capacity` of a Map): a source `==` between the used word and the
//! capacity word of one place is a full test, and every call lowered in the
//! true arm of an `if` on it is marked `cold` on its CallSite. The flag is a
//! hint with no behaviour (core-ir CallSite::cold).
//!
//! A cold call keeps the caller's places out of memory: a `mut` argument of a
//! fixed layout is passed as a temporary copy and copied back on both edges of
//! the call. A `mut` place is exclusive for the call, so the round trip cannot
//! be observed; without it the place's address escapes into the cold call and
//! the hot path keeps it in memory.
use super::*;
use rustc_hash::FxHashSet;

/// Which header word of a sequence or map a read loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FillWord {
    /// List `len`, Map `slots`.
    Used,
    Capacity,
}

/// A place as the chain of addresses that reach it, outermost step last.
/// Two reads of one place build separate pointer values; equal chains are one
/// place. A root that is not a pointer this body built is its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PlaceChain {
    root: AddressBase,
    steps: Vec<Step>,
}

/// What the growth rule knows of this body's values.
#[derive(Default)]
pub(super) struct Growth {
    /// The address each pointer value of this body was made from.
    pointers: HashMap<ValueId, AddressId>,
    /// Header words read through `len`/`slots`/`capacity`, by result.
    words: HashMap<ValueId, (FillWord, PlaceChain)>,
    /// Results of a full test.
    full: FxHashSet<ValueId>,
    /// Lowering is inside the true arm of a full test.
    pub(super) cold: bool,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn note_pointer(&mut self, result: ValueId, address: AddressId) {
        self.growth.pointers.insert(result, address);
    }
    /// Read the used or capacity word of the sequence or map whose header
    /// `header` points at; an own List's (its row `carrier`) is a typed carrier
    /// word (FORM #113), a Map's two raw words.
    pub(super) fn fill_word(
        &mut self,
        header: ValueId,
        carrier: Option<TypeId>,
        word: FillWord,
        n: NodeId,
    ) -> ValueId {
        let result = match (carrier, word) {
            (Some(c), FillWord::Used) => self.carrier_word(header, c, SeqWord::Len, n),
            (Some(c), FillWord::Capacity) => self.carrier_word(header, c, SeqWord::Cap, n),
            (None, FillWord::Used) => {
                self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n)
            }
            (None, FillWord::Capacity) => {
                self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n)
            }
        };
        if self.live() {
            let place = self.place_chain(header);
            self.growth.words.insert(result, (word, place));
        }
        result
    }
    fn place_chain(&self, mut value: ValueId) -> PlaceChain {
        let mut steps = Vec::new();
        let root = 'root: loop {
            let Some(&first) = self.growth.pointers.get(&value) else {
                break AddressBase::Value(value);
            };
            let mut address = first;
            loop {
                let info = &self.b.addresses[address.index()];
                let own = info.steps.get(&self.b.pools.steps).unwrap_or_default();
                steps.extend(own.iter().rev().copied());
                match info.base {
                    AddressBase::Address(inner) => address = inner,
                    AddressBase::Value(v) => {
                        value = v;
                        continue 'root;
                    }
                    slot @ AddressBase::Slot(_) => break 'root slot,
                }
            }
        };
        steps.reverse();
        PlaceChain { root, steps }
    }
    /// A source `==` produced `result` from `left` and `right`.
    pub(super) fn note_equal(&mut self, result: ValueId, left: ValueId, right: ValueId) {
        let g = &self.growth;
        if let (Some((l, lp)), Some((r, rp))) = (g.words.get(&left), g.words.get(&right))
            && l != r
            && lp == rp
        {
            self.growth.full.insert(result);
        }
    }
    /// Whether an `if` on `condition` takes its true arm only when full.
    pub(super) fn is_full_test(&self, condition: Option<ValueId>) -> bool {
        condition.is_some_and(|c| self.growth.full.contains(&c))
    }
    /// Pass the `mut` place `v` (row `id`) to a cold call through a temporary
    /// copy; `None` when the call is not cold or the place is a block holder.
    pub(super) fn cold_place(
        &mut self,
        v: ValueId,
        id: TypeId,
        n: NodeId,
    ) -> Option<(AddressId, Staged)> {
        if !self.growth.cold || self.e.by_block(id) {
            return None;
        }
        let tmp = self.new_slot(PlaceTy::Declared(id), n);
        self.copy_bytes(tmp, v, id, n);
        let copy = self.pointer(tmp, n);
        Some((tmp, Staged { place: v, copy, id }))
    }
    /// Copy every staged temporary back into its place.
    pub(super) fn unstage(&mut self, staged: &[Staged], n: NodeId) {
        for s in staged {
            let to = self.address(AddressBase::Value(s.place), &[], PlaceTy::Declared(s.id), n);
            self.copy_bytes(to, s.copy, s.id, n);
        }
    }
    /// The failure edge of a call with staged places: copy them back, then
    /// continue to `fail`, or propagate when nothing here needs cleanup.
    pub(super) fn staged_fail(
        &mut self,
        staged: &[Staged],
        fail: Option<BlockId>,
        n: NodeId,
    ) -> Option<BlockId> {
        if staged.is_empty() {
            return fail;
        }
        let cur = self.cur.take();
        let open = std::mem::take(&mut self.open);
        let anchors = std::mem::take(&mut self.open_anchors);
        let blk = self.new_block(&[]);
        self.start(blk);
        self.unstage(staged, n);
        match fail {
            Some(f) => self.jump(f, &[], n),
            None => self.end(Terminator::Propagate, Some(n)),
        }
        self.cur = cur;
        self.open = open;
        self.open_anchors = anchors;
        Some(blk)
    }
}

/// A `mut` place passed to a cold call as `copy`.
#[derive(Clone, Copy)]
pub(super) struct Staged {
    place: ValueId,
    copy: ValueId,
    id: TypeId,
}
