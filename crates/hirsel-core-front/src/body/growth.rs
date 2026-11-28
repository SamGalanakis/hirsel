//! The growth path is cold. A sequence or map grows only when it is full, and
//! the front lowers both words a full test reads (`len`/`capacity` of a List,
//! `slots`/`capacity` of a Map): a source `==` between the used word and the
//! capacity word of one place is a full test, and every call lowered in the
//! true arm of an `if` on it is marked `cold` on its CallSite. `list.push` is
//! expanded in place as the same full test and true arm. The flag is a hint with
//! no behaviour (core-ir CallSite::cold).
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
    pub(super) root: AddressBase,
    pub(super) steps: Vec<Step>,
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
    /// The most recently emitted u64 constant with value one.
    pub(super) recent_one: Option<ValueId>,
    /// Lowering is inside the true arm of a full test.
    pub(super) cold: bool,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn note_pointer(&mut self, result: ValueId, address: AddressId) {
        self.growth.pointers.insert(result, address);
    }
    pub(super) fn pointer_address(&self, value: ValueId) -> Option<AddressId> {
        self.growth.pointers.get(&value).copied()
    }
    /// A word of an own `List` or `String` carrier, addressed through the carrier's
    /// row so its type names the element (FORM #113): the evaluator checks
    /// `len <= capacity <= charge::seq_len_bound(size(T))` at it, and LLVM ranges it.
    pub(super) fn carrier_word(
        &mut self,
        header: ValueId,
        carrier: TypeId,
        word: SeqWord,
        n: NodeId,
    ) -> ValueId {
        let ty = word.ty();
        if !self.live() {
            return self.value(ty);
        }
        let address = self.carrier_word_at(header, carrier, word, n);
        let result = self.value(ty);
        self.inst(
            Inst::Read {
                result,
                address,
                mode: ReadMode::Bits,
            },
            n,
        );
        // A generic body's carrier is a byte carrier in its `u8` instance.
        if word == SeqWord::Cap
            && (self.e.generic_names.contains_key(&carrier)
                || hirsel_core_ir::byte_carrier(&self.e.module.types, carrier))
        {
            return self
                .runtime_row(
                    hirsel_core_ir::RtOp::ByteCapacity { carrier },
                    &[Arg::Value(result)],
                    n,
                )
                .expect("live capacity read")[0];
        }
        result
    }
    pub(super) fn set_carrier_word(
        &mut self,
        header: ValueId,
        carrier: TypeId,
        word: SeqWord,
        value: ValueId,
        n: NodeId,
    ) {
        if !self.live() {
            return;
        }
        let address = self.carrier_word_at(header, carrier, word, n);
        self.inst(Inst::Store { address, value }, n);
    }
    fn carrier_word_at(
        &mut self,
        header: ValueId,
        carrier: TypeId,
        word: SeqWord,
        n: NodeId,
    ) -> AddressId {
        let row = self.address(
            AddressBase::Value(header),
            &[],
            PlaceTy::Declared(carrier),
            n,
        );
        self.address(
            AddressBase::Address(row),
            &[Step::Field(0), Step::Seq(word)],
            PlaceTy::Value(word.ty()),
            n,
        )
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
    pub(super) fn place_chain(&self, mut value: ValueId) -> PlaceChain {
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
    /// The full test of the own List `header` points at (its row `carrier`), as
    /// the source `list.len(xs) == list.capacity(xs)` lowers it.
    pub(super) fn full_test(&mut self, header: ValueId, carrier: TypeId, n: NodeId) -> ValueId {
        let len = self.fill_word(header, Some(carrier), FillWord::Used, n);
        let cap = self.fill_word(header, Some(carrier), FillWord::Capacity, n);
        let result = self.list_compare(CompareOp::Eq, len, cap, n);
        self.note_equal(result, len, cap);
        result
    }
    /// Lower `arm` as the true arm of an `if` whose condition is a full test when
    /// `full`: every call in it is then cold.
    pub(super) fn true_arm<R>(&mut self, full: bool, arm: impl FnOnce(&mut Self) -> R) -> R {
        let outer = self.growth.cold;
        self.growth.cold |= full;
        let r = arm(self);
        self.growth.cold = outer;
        r
    }
}
