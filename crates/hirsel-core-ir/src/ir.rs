//! The CoreIR data types. Extracted from `hirsel-lir/src/ir.rs` (u32 ID arenas,
//! typed pools, immutable values plus mutable slots, block parameters, edges with
//! arguments, source anchors); WHY-NEW-FORM.md lists, per LIR opcode, what was kept,
//! rewritten or deleted.
//!
//! Values are defined exactly once and hold a scalar, pointer or fixed vector; aggregates live in
//! slots or behind pointers and are reached through addresses. Slots are ordinary
//! memory: a read names no "reaching version", so a producer need not build SSA for
//! slots or prove liveness before emitting.

mod flow;
pub use crate::static_data::StaticValue;
pub(crate) use flow::for_each_successor;
#[cfg(any(test, feature = "wf"))]
pub(crate) use flow::successors;
mod closed;
mod closure;
mod host_op;
mod marks;
mod runtime_types;
mod simd;
pub use closed::CLOSED;
pub use host_op::{HostOp, host_drop_symbol};
pub use marks::without_marks;
pub use runtime_types::Wait;
pub use simd::SimdOp;

use crate::decode::{Debit, DecodeResult};
use hirsel_core_ids::{ContentHash, DefPath};
pub use hirsel_core_layout::{Carried, Row, Scalar, Shape, Ty, Vector};
use hirsel_core_layout::{Cleanup, FieldTy, FuncRef, RecipeRow, TypeRef, cleanup_class};
use std::collections::HashMap;
use std::marker::PhantomData;

macro_rules! ids { ($($(#[$doc:meta])* $name:ident),* $(,)?) => {$(
    $(#[$doc])*
    #[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
    pub struct $name(pub u32);
    impl $name {
        pub const fn index(self) -> usize {
            self.0 as usize
        }
    }
)*}; }
ids!(
    /// An immutable value of one body.
    ValueId,
    /// A mutable storage slot of one body (a local, a temporary, a destination).
    SlotId,
    /// A computed address: base plus projection steps.
    AddressId,
    BlockId,
    /// A control edge with its block arguments, in `Body::edges`.
    EdgeId,
    /// A call site in `Body::calls`.
    CallId,
    /// A function (defined or external) in `Module::functions`.
    FuncId,
    /// Read-only static data in `Module::statics`.
    StaticId,
    /// A signature row in `Module::signatures`.
    SigId,
    /// An instance row in `Module::instances`.
    InstanceId,
    /// A source item in `Module::items` (debug identity).
    ItemId,
    /// A declared type in `Module::types`: its shape, from which
    /// `hirsel_core_layout::compute` derives the layout and drop recipe.
    TypeId,
);

/// A typed range in one of a body's pools (LIR `PoolRange`, unchanged).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct PoolRange<T> {
    pub start: u32,
    pub len: u32,
    _element: PhantomData<fn() -> T>,
}
impl<T> PoolRange<T> {
    pub const EMPTY: Self = Self::new(0, 0);
    pub const fn new(start: u32, len: u32) -> Self {
        Self {
            start,
            len,
            _element: PhantomData,
        }
    }
    /// The range's elements, or None if it reaches outside `pool`.
    pub fn get<'a>(&self, pool: &'a [T]) -> Option<&'a [T]> {
        let start = self.start as usize;
        pool.get(start..start.checked_add(self.len as usize)?)
    }
    /// Append `items` to `pool` and return their range.
    pub fn push(pool: &mut Vec<T>, items: impl IntoIterator<Item = T>) -> Self {
        let start = pool.len() as u32;
        pool.extend(items);
        Self::new(start, pool.len() as u32 - start)
    }
}
impl<T> Clone for PoolRange<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for PoolRange<T> {}
impl<T> PartialEq for PoolRange<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.start, self.len) == (other.start, other.len)
    }
}
impl<T> Eq for PoolRange<T> {}
impl<T> std::hash::Hash for PoolRange<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.start, self.len).hash(state);
    }
}
impl<T> std::fmt::Debug for PoolRange<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..+{}", self.start, self.len)
    }
}

/// The type of a slot or of the place an address names.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PlaceTy {
    Value(Ty),
    /// A declared type laid out by its recipe (FORM v1).
    Declared(TypeId),
}

/// One projection step; indices are already-evaluated values (LIR `ProjectionStep`
/// without `Slot` and `FlatAt`, which were library-unit and old-flat steps).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Step {
    /// Follow the pointer stored at this place.
    Deref,
    Field(u32),
    /// A field of one case of a variant (FORM v1b): the payload projection. The
    /// producer tests the case first (`case`); the step itself checks nothing.
    CaseField {
        case: u32,
        field: u32,
    },
    /// Raw storage: the element at this index, at the stride of the address's place
    /// type. Unchecked; a collection's producer tests its length first.
    Index(ValueId),
    /// An element of an inline array place (`Inline[T, n]`, SEM-9, FORM v1b): fails
    /// with `Bounds` when the index is not below `n`, at the `Address` instruction.
    Element(ValueId),
    /// Compact data (FORM v1b): the place this many bytes further on. Blob bytes have
    /// no alignment (MEM-9), so a read, store or copy through an address with a
    /// `byte` step is unaligned. Unchecked.
    Byte(ValueId),
    /// One word of the `seq` field this place is (FORM #113): last in its address,
    /// whose type is the word's (`ptr` data, `u64` len and capacity). An own `List`
    /// or `String` carrier's words are reached this way, so the evaluator checks
    /// `len <= capacity <= charge::seq_len_bound(size(T))` (logical size, SEM-4 and
    /// MEM-24) and an engine may assume it.
    Seq(SeqWord),
}

/// A word of the 24-byte `seq` carrier: data, len, capacity at bytes 0, 8, 16.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SeqWord {
    Data,
    Len,
    Cap,
}

impl SeqWord {
    pub const fn offset(self) -> u64 {
        match self {
            Self::Data => 0,
            Self::Len => 8,
            Self::Cap => 16,
        }
    }
    pub const fn ty(self) -> Ty {
        match self {
            Self::Data => Ty::Ptr,
            Self::Len | Self::Cap => Ty::Scalar(Scalar::U64),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AddressBase {
    Slot(SlotId),
    /// The place a pointer value points at.
    Value(ValueId),
    Address(AddressId),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct AddressInfo {
    pub base: AddressBase,
    pub steps: PoolRange<Step>,
    pub ty: PlaceTy,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum UnaryOp {
    /// Integer negation fails on signed MIN and is refused on unsigned kinds;
    /// float negation flips the sign bit.
    Neg,
    /// Boolean not.
    Not,
    BitNot,
}

/// Scalar binary operations (LIR `BinaryOp` without the comparisons, which are
/// `CompareOp`). Integer `Add`..`Shr` fail as SEM-1 says: `Overflow` on an
/// out-of-range result, on signed `MIN / -1` and on a shift count outside
/// `0..width`; `Division` on a zero divisor. `Div` truncates toward zero and `Rem`
/// takes the dividend's sign; `MIN rem -1` is 0 and does not fail. Float forms are
/// exact IEEE.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    /// IEEE 754-2019 minimum/maximum/minimumNumber/maximumNumber on floats;
    /// the ordinary minimum/maximum on integers.
    Min,
    Max,
    MinNumber,
    MaxNumber,
    /// Integers only: the operand's bits rotated left by the count modulo the width,
    /// the count read as unsigned; never fails (LLVM `fshl` with both inputs equal,
    /// Cranelift `rotl`). Last, so no stored variant index moves (FORM #112).
    Rotl,
}
impl BinaryOp {
    /// Whether the op on operands of `ty` can fail and so names a failure block.
    pub fn can_fail(self, ty: Ty) -> bool {
        ty.int().is_some()
            && matches!(
                self,
                Self::Add | Self::Sub | Self::Mul | Self::Div | Self::Rem | Self::Shl | Self::Shr
            )
    }
}

/// Comparison of two scalars of one kind, or two pointers (Eq/Ne only). Floats
/// compare by IEEE: an unordered pair makes every op but `Ne` false.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// LIR `ConvertMode`; see its documentation for each mode's exact result. A `char`
/// converts to and from `u32` only (`char.to_u32`, `char.from_u32`), by `Widen` and
/// `TryNarrow`; no other mode takes a `char`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ConvertMode {
    /// The same value at a type that holds every value of the operand; `char` to
    /// `u32` is its code point.
    Widen,
    Truncate,
    /// `flag` (bool) is true exactly when the operand is representable. `u32` to
    /// `char`: true exactly for a Unicode scalar value (0..=0xD7FF, 0xE000..=0x10FFFF),
    /// and the value is then the operand, else 0, so no invalid `char` exists.
    TryNarrow,
    /// `float.to_<int>` (SEM-2) to any integer type: the value truncated toward
    /// zero, and `flag` (u8): 0 in range, 1 NaN, 2 out of range (infinities
    /// included). The value is 0 unless the flag is 0.
    FloatToInt,
    IntToFloat,
    Reinterpret,
    FloatResize,
}

/// The lower-bound test of `FloatToInt` on the operand itself, so a backend needs no
/// trunc: for an integer `low`, trunc(x) >= low iff x > low - 1, and where low - 1 is
/// not representable no float lies strictly between it and `low`, so iff x >= low.
/// Returns the bound and whether the test is strict. `single`: an f32 operand.
pub fn float_to_int_floor(low: f64, single: bool) -> (f64, bool) {
    let below = low - 1.0;
    if below != low && (!single || f64::from(below as f32) == below) {
        (below, true)
    } else {
        (low, false)
    }
}

/// Overflow behaviour of `IntArith` (LIR `ArithMode`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ArithMode {
    Wrapping,
    Saturating,
    /// The wrapped result and a bool flag, true exactly on overflow.
    Checked,
}

/// The ops `IntArith` admits (LIR used `BinaryOp` and a validator rule).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    /// Checked only (`int.checked_div`): the flag is set on a zero divisor or
    /// `MIN / -1`, and the value is then 0.
    Div,
}

/// LIR `ScalarUnaryOp`, plus `RoundEven`: IEEE 754 roundToIntegralTiesToEven
/// (`float.round_even`); `Round` rounds ties away from zero (`float.round`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ScalarUnaryOp {
    Sqrt,
    Floor,
    Ceil,
    Trunc,
    Round,
    RoundEven,
    Abs,
    LeadingZeros,
    TrailingZeros,
    CountOnes,
    SwapBytes,
    ReverseBits,
}
impl ScalarUnaryOp {
    pub const fn is_float(self) -> bool {
        matches!(
            self,
            Self::Sqrt
                | Self::Floor
                | Self::Ceil
                | Self::Trunc
                | Self::Round
                | Self::RoundEven
                | Self::Abs
        )
    }
}

/// How a read uses its place.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ReadMode {
    /// Plain bits; the place is unchanged.
    Bits,
    /// Move out: the place is uninitialized afterwards (its owner moved).
    Take,
}

/// An instruction. A `fail` block is the failure successor of an operation that
/// can fail; the block takes no parameters in FORM v0 and what it does (cause
/// storage, cleanup, propagation) is FORM v1.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Inst {
    /// The bits of a scalar of the result's type (floats by their IEEE bits,
    /// signed integers two's complement in the low bits, bool 0 or 1, char its
    /// scalar value).
    Const {
        result: ValueId,
        bits: u64,
    },
    /// A pointer to read-only static data.
    ReadStatic {
        result: ValueId,
        data: StaticId,
    },
    /// A function's code pointer.
    FunctionRef {
        result: ValueId,
        function: FuncId,
    },
    /// The address of `function`'s static descriptor (PROG-9): read-only words
    /// `{code, length, revision}` then the `length` bytes of `identity`, where
    /// `code` is `function`'s code pointer, `identity` is its identity bytes and
    /// `revision` addresses its revision carrier ([`revision_symbol`]). A program
    /// has one descriptor per function: every unit that refers to it defines it
    /// weak beside the function's code identity, and the linker keeps one, so
    /// two descriptors' addresses are equal exactly when their functions are.
    DescriptorRef {
        result: ValueId,
        function: FuncId,
        identity: StaticId,
    },
    Unary {
        result: ValueId,
        op: UnaryOp,
        operand: ValueId,
        fail: Option<BlockId>,
    },
    Binary {
        result: ValueId,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        fail: Option<BlockId>,
    },
    Compare {
        result: ValueId,
        op: CompareOp,
        left: ValueId,
        right: ValueId,
    },
    Convert {
        result: ValueId,
        flag: Option<ValueId>,
        operand: ValueId,
        mode: ConvertMode,
    },
    IntArith {
        result: ValueId,
        flag: Option<ValueId>,
        op: ArithOp,
        mode: ArithMode,
        left: ValueId,
        right: ValueId,
    },
    /// `left * right + addend`, rounded once.
    MulAdd {
        result: ValueId,
        left: ValueId,
        right: ValueId,
        addend: ValueId,
    },
    ScalarUnary {
        result: ValueId,
        op: ScalarUnaryOp,
        operand: ValueId,
    },
    /// Evaluate an address. An `element` step's bounds check runs here (failing
    /// to `fail`); `index` steps are raw and never fail.
    Address {
        address: AddressId,
        fail: Option<BlockId>,
    },
    Read {
        result: ValueId,
        address: AddressId,
        mode: ReadMode,
    },
    Store {
        address: AddressId,
        value: ValueId,
    },
    /// The slot's storage begins; its content is uninitialized.
    StorageLive {
        slot: SlotId,
    },
    StorageDead {
        slot: SlotId,
    },
    /// Set or clear bit `field` of an initialization mask (an integer value).
    UpdateMask {
        result: ValueId,
        mask: ValueId,
        field: u32,
        initialized: bool,
    },
    Call {
        call: CallId,
    },
    /// FORM v1b: the address of a place, as a pointer value (a view's bytes, a
    /// locator's blob).
    Pointer {
        result: ValueId,
        address: AddressId,
    },

    // ---- FORM v1a ----
    /// Admit `charge` and allocate a block of `size` bytes (a `u64` value: a
    /// sized destination's `n` and a text's length are known only at run time)
    /// aligned to `align`
    /// (MEM-21). On refusal `result` is null and `available` is the room left in the
    /// limit tried (MEM-22), and nothing is allocated; on success `available` is 0. It
    /// never fails: the producer raises `Capacity` on a null result where the source
    /// operation raises (MEM-21).
    Alloc {
        result: ValueId,
        available: ValueId,
        /// Optional opaque payer-entry token, for a region writer only.
        region_entry: Option<ValueId>,
        kind: AllocKind,
        place: AllocPlace,
        charge: ValueId,
        /// Exact logical charge exceeds u64; admission refuses even at MAX room.
        /// Ordinary/counted and region storage only; a resource has a fixed total.
        overflow: Option<ValueId>,
        size: ValueId,
        align: u64,
    },
    /// Free an own, counted or resource block and refund `charge` to the limit its
    /// record leads to (MEM-20, MEM-23). Never fails. A resource's refund leaves out
    /// a cause cell that holds a cause and a grant its finalizer already settled.
    Free {
        block: ValueId,
        charge: ValueId,
    },
    /// Admit room for a lexical frame slot. On success the slot owes the
    /// admitted charge. With `replace`, success also refunds the previous
    /// charge, after admitting the whole new charge. Refusal leaves it alone.
    /// The block branches directly on `admitted`.
    FrameAdmit {
        storage: AddressId,
        admitted: ValueId,
        available: ValueId,
        charge: ValueId,
        overflow: Option<ValueId>,
        replace: bool,
    },
    /// Test whether the frame slot owes a refund. The block branches directly
    /// on this result, so the verifier can refine the obligation on each edge.
    FrameActive {
        storage: AddressId,
        result: ValueId,
    },
    /// Return the charge admitted for this frame slot to the lexical limit.
    /// Consumes the obligation and leaves the storage in place.
    FrameRefund {
        storage: AddressId,
    },
    /// Copy `bytes` bytes from the place `from` to the place `to`. The two ranges
    /// are the same or do not overlap (the evaluator stops the run otherwise). A move of a non-plain value is this copy; the source is then moved
    /// out and its drop flag cleared by the producer. Growth copies the old storage.
    Copy {
        to: AddressId,
        from: AddressId,
        bytes: ValueId,
    },
    /// `grow` (crate docs, "Growth"): replace the storage the sequence carrier at
    /// `carrier` owns (its capacity is not 0) with a block of `size` bytes for
    /// `capacity` elements, consuming the old one. It admits `charge` as `alloc ..
    /// like` the old block does, the whole charge before any refund. On refusal
    /// `result` is null, `available` the room left, and the carrier, its block and
    /// its elements are unchanged; it never fails, as `alloc`. On success the first `moved` bytes (the carrier's
    /// length times its element's stride) are the new block's, the carrier holds the
    /// new data and `capacity`, and then the old block is dead, even at an unchanged
    /// address, and `refund` (its charge) goes back to the limit its record leads to.
    Grow {
        result: ValueId,
        available: ValueId,
        carrier: AddressId,
        charge: ValueId,
        overflow: Option<ValueId>,
        size: ValueId,
        align: u64,
        capacity: ValueId,
        moved: ValueId,
        refund: ValueId,
    },
    // ---- FORM v1b (frozen) ----
    /// The case number (`u32`, declaration order from 0) of the variant value at
    /// `address`, a place of an own, resource, stable or scoped variant type. Its
    /// layout is private (MEM-13); a producer never reads the tag bytes itself.
    Case {
        result: ValueId,
        address: AddressId,
    },
    /// Make the variant place at `address` hold case `case`; its payload fields are
    /// then stored through `CaseField` addresses. Constructing writes the case first
    /// or last, as the producer likes: nothing reads the place until it is complete.
    /// Under a niche the data case writes nothing and an empty case's word overlaps
    /// the payload (crate docs, "Variants").
    SetCase {
        address: AddressId,
        case: u32,
    },
    /// Open a region (MEM-1); `result` is its handle, a value never stored.
    RegionOpen {
        result: ValueId,
    },
    /// Free and refund everything allocated in the region at once (MEM-8).
    RegionClose {
        region: ValueId,
    },
    /// `finish_trim` (MEM-4): lower the retained charge of the region block `block`
    /// to `charge`, refunding the difference to the limit its record leads to
    /// (MEM-20), whatever limit is current. `charge` is at most the block's charge.
    /// Moves nothing and never fails; the region's close refunds the rest.
    Trim {
        block: ValueId,
        region: ValueId,
        entry: ValueId,
        previous: ValueId,
        charge: ValueId,
    },
    // ---- FORM v1a ----
    /// Enter `bound bytes n`: admit `bytes` against the current limit and make a new
    /// limit of that size current (MEM-25). `admitted` is false, `available` the
    /// room, and no limit is entered when the admission is refused; on success
    /// `available` is 0.
    LimitEnter {
        admitted: ValueId,
        available: ValueId,
        bytes: ValueId,
    },
    /// Leave the innermost bound scope: refund its unused part to the parent and
    /// hand its record over (MEM-20). Never fails.
    LimitExit,
    /// Clean the value at `address` by its type's drop recipe (FAIL-8): the
    /// finalizer under its grant, then fields in the recipe's order (reverse
    /// declaration order wherever cleanup can observe it), then the value's own
    /// block. A finalizer failure still cleans the fields and then
    /// takes `fail` (FAIL-12); the cause is already in the chain.
    Drop {
        address: AddressId,
        ty: TypeId,
        fail: Option<BlockId>,
    },
    /// Add a holder to a counted node (MEM-16); past `2^64 - 1` holders is a run stop.
    Share {
        node: ValueId,
        kind: Counted,
    },
    /// Drop a holder; at zero clean the payload of type `ty` with a constant-stack
    /// worklist and refund each node (MEM-16). Never fails and runs no user code: a
    /// shareable payload holds no resource.
    Release {
        node: ValueId,
        kind: Counted,
        ty: TypeId,
    },
    /// Uniqueness test shared by `try_unwrap` and `with_unique`: true exactly
    /// when this is the only holder. It leaves the node and payload in place.
    /// `try_unwrap` separately moves the payload out and frees the node;
    /// `with_unique` lends the payload mutably while keeping the holder.
    TryUnwrap {
        result: ValueId,
        node: ValueId,
        kind: Counted,
    },
    // ---- FORM v1c ----
    /// `mark a, vN, live|empty : ty`: the `count` consecutive slots from `address`,
    /// each a place of `ty` at its size's stride, now hold a value (`live`, after a
    /// put or a move in) or none (`empty`, after a take or a move out). Only core's
    /// slot kernels emit it. It has no run-time effect: engines emit nothing for
    /// it, and the evaluator keeps it as a liveness shadow that stops the run on a
    /// take of an empty slot, a put over a live one, a read of an empty one and a
    /// free of a block with a live one (crate docs, "Slot kernels").
    Mark {
        address: AddressId,
        count: ValueId,
        live: bool,
        ty: PlaceTy,
    },
    /// `unmark a, vN, live|empty : ty`: the inverse of `mark`. The `count` slots from
    /// `address` leave the liveness shadow and is ordinary storage again, and the kernel
    /// states which way it leaves: `live` (a value the storage now holds) or `empty`.
    /// Like `mark` it has no run-time effect; the evaluator stops the run when the
    /// shadow shows the other state, and a slot never marked passes.
    Unmark {
        address: AddressId,
        count: ValueId,
        live: bool,
        ty: PlaceTy,
    },
    /// One MEM-30 source call, loop latch or entry from core into user code.
    /// The attempted step fails before the source operation when exhausted.
    Step {
        fail: Option<BlockId>,
    },
    /// Relocate frame-backed sequence storage to the heap on a move-out,
    /// transferring its existing payer and charge. Other layouts and storage
    /// are already movable. The typed place may be closed from a generic body.
    PromoteFrame {
        carrier: AddressId,
    },
    /// An engine-private byte count. Resolve it from the shared layout recipe
    /// before running code or selecting machine instructions.
    LayoutConst {
        result: ValueId,
        query: LayoutQuery,
    },
    /// The `W` of a compact blob holding an encoding of `ty` (a compact type's
    /// place row, MEM-9): `word`, the blob's header word the body read.
    CompactWidth {
        result: ValueId,
        word: ValueId,
        ty: TypeId,
    },
    /// One vector value operation. The two loads take the address of the first
    /// lane, after the source operation's bounds check for Lanes.
    Simd {
        result: ValueId,
        op: SimdOp,
        first: ValueId,
        second: Option<ValueId>,
        fail: Option<BlockId>,
    },
    /// Store the first count bytes of a vector at the byte place to, unaligned, after
    /// the source operation's bounds check and reservation for those bytes.
    PutLanes {
        to: AddressId,
        vector: ValueId,
        count: ValueId,
    },
    /// A symbolic dependency on a compiler-implemented definition lowered in
    /// this block. Its instructions do the work; this row carries its identity.
    BuiltinUse {
        definition: Box<DefPath>,
    },
    /// Copy `bytes` bytes from the place `from` to the place `to` as if through a
    /// temporary buffer, so the two ranges may overlap (memmove). Plain bytes only;
    /// it moves no ownership. Last, so no stored variant index moves (FORM #112).
    Move {
        to: AddressId,
        from: AddressId,
        bytes: ValueId,
    },
    /// A read hint for the byte `offset` (wrapping) past the pointer `data`. The
    /// address need not lie in any storage: it is not an access, it never fails and
    /// it has no observable effect, so an engine may emit nothing for it.
    Prefetch {
        data: ValueId,
        offset: ValueId,
    },
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LayoutQuery {
    /// The native stride of the field as held by an own value.
    FieldSize(FieldTy),
    /// The native stride of a declared value as carried in a slot.
    HeldSize(TypeId),
    /// The bytes of a fixed row's body, plus its fixed prefix.
    BlockSize { ty: TypeId, prefix: u64 },
    /// Number of elements that fit in the frame-backed List buffer. A row is
    /// eligible only if the frame holds the first growth of four elements.
    FrameCapacity(FieldTy),
    /// The index of the build's target in [`hirsel_core_layout::Target::ALL`]. Only a targeted
    /// selection reads it, and retention replaces it before any engine runs (PROG-6).
    Target,
}

/// What an `Alloc` makes. The block's bookkeeping (its limit record,
/// and for a resource its cause cell and grant) is the engine's.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AllocKind {
    /// Own storage for the owning edge that will hold it (crate docs, "Cleanup
    /// classes"): `box tN`, `seq E`, `map K V`, or
    /// `rc tN` / `arc tN`, a counted node: the count (u64, starting at 1) at offset 0,
    /// the payload at 8. `arc` makes a node whose every later `share`, `release`
    /// and `try_unwrap` is an event (MEM-15); its operations name the same kind. The edge's [`hirsel_core_layout::cleanup_class`] fixes the block's
    /// native prefix.
    Own { edge: FieldTy },
    /// An `own fn` environment (`alloc env tN`, crate docs "Callable
    /// environments"): word 0 names row `tN`'s fixed environment descriptor and
    /// the captures follow from offset 8, laid out as row `tN`. The alloc writes
    /// word 0 (an engine writes the descriptor's address); it is never charged
    /// (MEM-24). Its class is Fixed: the drain cleans it from that descriptor.
    Environment { ty: TypeRef },
    /// Region storage (MEM-1): freed when its region closes, never by an edge. Only
    /// `in` a region, and every region allocation is stable.
    Region,
    /// A resource block: `charge` covers its storage, its cause cell and `grant`.
    Resource { grant: u64 },
}

impl AllocKind {
    /// The count kind of a counted node (an `rc` or `arc` edge).
    pub fn counted(&self) -> Option<Counted> {
        match self {
            AllocKind::Own {
                edge: FieldTy::Rc(_),
            } => Some(Counted::Rc),
            AllocKind::Own {
                edge: FieldTy::Arc(_),
            } => Some(Counted::Arc),
            _ => None,
        }
    }
    /// The cleanup class of the block (crate docs, "Cleanup classes"); `None` for
    /// region storage, which no edge cleans.
    pub fn cleanup(&self, shapes: &[Row], recipes: &[RecipeRow]) -> Option<Cleanup> {
        match self {
            AllocKind::Own { edge } => cleanup_class(shapes, recipes, edge),
            AllocKind::Environment { .. } => Some(Cleanup::Fixed),
            AllocKind::Region => None,
            AllocKind::Resource { .. } => Some(Cleanup::Resource),
        }
    }
    /// Whether the block is terminal: the 8-byte payer prefix, freed with nothing
    /// to drop (crate docs, "Cleanup classes").
    pub fn terminal(&self, shapes: &[Row], recipes: &[RecipeRow]) -> bool {
        self.cleanup(shapes, recipes) == Some(Cleanup::Terminal)
    }
}

/// The owning `seq E` edge of a sequence carrier place, which `grow` replaces the
/// storage of (crate docs, "Growth"): a row whose only field is that edge, as
/// `List[T]` (`items`) and `String` (`bytes`) are. None for any other place.
pub fn sequence_edge(shapes: &[Row], place: PlaceTy) -> Option<&FieldTy> {
    let PlaceTy::Declared(t) = place else {
        return None;
    };
    match &shapes.get(t.index())?.body {
        hirsel_core_layout::Body::Record(fields) => match fields.as_slice() {
            [f] if matches!(f.ty, FieldTy::Seq(_)) => Some(&f.ty),
            _ => None,
        },
        hirsel_core_layout::Body::Variant(_) => None,
    }
}

/// Which limit an `Alloc` is charged to.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AllocPlace {
    /// The current limit (MEM-19).
    Current,
    /// Like allocation uses the current limit, as does growth (MEM-26).
    Like(ValueId),
    /// FORM v1b: in this region, charged to the current limit until it closes (MEM-1).
    Region(ValueId),
    /// Frame storage with a live allocation record understood by lent callees.
    /// The aligned slot includes FRAME_LIST_LINK_BYTES after its payload.
    LentFrame(ValueId),
    /// The current limit. A short byte buffer may reserve disjoint tail backing.
    ByteHead,
    /// The current limit. A nonnull pointer is data exclusively lent from a
    /// nonempty owning Bytes carrier, which may have an unused tail.
    ByteTail(ValueId),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Counted {
    Rc,
    /// Every operation is an event in the run's order (MEM-15).
    Arc,
}

/// The smallest `list.fill` span, in bytes, that lowers to `rt.zero_bytes`
/// (crate docs, "Zero fill"); shorter fills keep their store loop.
pub const ZERO_FILL_MIN: u64 = 1 << 16;

/// Engine operations a call can name. Rows are in the crate docs;
/// a `take` argument is passed by address and is the callee's only after the
/// operation's commit point (FAIL-3), so a refused operation leaves it in place.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RtOp {
    Spawn {
        x: TypeId,
        r: TypeId,
    },
    Join {
        r: TypeId,
    },
    Cancel,
    CheckCancel,
    ChannelNew {
        item: TypeId,
    },
    ChannelSender,
    Send {
        item: TypeId,
    },
    TrySend {
        item: TypeId,
    },
    Receive {
        item: TypeId,
    },
    TryReceive {
        item: TypeId,
    },
    /// `latest.new`: `item` is `Arc[T]`, whose drop the root keeps for its last handle.
    LatestNew {
        item: TypeId,
    },
    LatestHandle,
    /// `conn.new` (CONC-17): `(cap, b) -> (client, inbox, available)`; a null
    /// client is `Capacity`, and `cap` 0 fails with `Bounds`.
    ConnNew {
        item: TypeId,
    },
    /// `conn.client`: one more client of the same connection.
    ConnClient,
    /// `secret.share` (TLS-5): one more holder of the designation, as its key.
    SecretShare,
    /// `conn.send`: 0 accepted, 1 closed (the item stays in its place).
    ConnSend {
        item: TypeId,
    },
    /// `conn.try_send`: 0 accepted, 1 closed, 2 full (the item stays in its place).
    ConnTrySend {
        item: TypeId,
    },
    /// `conn.receive`: `(outcome, requested, available)`, outcome 0 an item,
    /// 1 closed, 2 `Capacity` (the item stays queued).
    ConnReceive {
        item: TypeId,
    },
    /// `conn.try_receive`: as `ConnReceive`, and outcome 3 empty.
    ConnTryReceive {
        item: TypeId,
    },
    /// `task.spawn_unit` (CONC-15): `Spawn`'s operands, and
    /// `(task, requested, available)`; a null task is `Capacity`.
    SpawnUnit {
        x: TypeId,
        r: TypeId,
    },
    /// Installs a node and returns the old root's node, which the caller releases.
    LatestPublish,
    /// The root's node, which the caller shares before any other event.
    LatestRead,
    /// `shared.new` (CONC-15): `(x, charge) -> (node, available)`; the runtime
    /// admits `charge` and moves `x` into a new node. A null node is `Capacity`,
    /// with `x` still in its place.
    SharedNew {
        item: TypeId,
    },
    /// `shared.share`: one more holder of the node, counted atomically and not
    /// an event (CONC-15).
    SharedShare,
    /// One holder of a `Shared[item]` node fewer; the last one cleans the payload
    /// and refunds the node through its own record (CONC-15).
    SharedRelease {
        item: TypeId,
    },
    DestId,
    /// Bytewise order of two byte ranges (SEM-7): `String`, `Text` and `Bytes`.
    CompareBytes,
    /// Keyed SipHash-1-3 over a byte sequence or one canonical scalar word.
    MapHashBytes,
    MapHashWord,
    /// Composite Hash: initialize, append to, and finish the caller's scratch.
    MapHashStart,
    MapHashAddWord,
    MapHashAddBytes,
    MapHashFinish,
    MapPlaceWord,
    MapPlaceBytes,
    MapStrongWord,
    MapStrongBytes,
    MapStrongStart,
    /// The SHA-256 compression of whole blocks into an eight-word state (crate docs, "SHA-256").
    Sha256Blocks,
    Sha512Blocks,
    /// The next cell of a `Causes` chain, or null (crate docs, "Causes").
    CauseNext,
    /// A cause cell's `Cause`, in core's layout (crate docs, "Causes").
    CauseValue,
    /// `args.len` (CAP-5): the run's argument count.
    ArgsLen,
    /// `args.get`: a run-owned `String` carrier of capacity 0, or null.
    ArgsGet,
    /// A provider operation of `host`, `stdout` or `stderr` (CAP-5).
    Host(HostOp),
    /// `isolate`'s entry: reserve a cause cell (crate docs, "Isolation").
    IsolateEnter,
    /// `isolate`'s body returned: refund the innermost reservation.
    IsolateLeave,
    /// `isolate`'s handler: take the caught chain as a `Causes`, or fail on with
    /// it when its primary cause is `Cancelled`.
    IsolateCatch,
    /// Reserve steps from the current bound; false leaves the stack unchanged.
    StepEnter,
    /// Close the current lexical step scope and refund unused steps.
    StepLeave,
    /// Close a failed step scope, catching only its own OutOfTime causes.
    StepCatch,
    /// The current task's original allowance.
    RootLimit,
    /// Private immutable launch metadata: `(field, index) -> word`.
    LaunchWord,
    /// Private launch target as a borrowed String carrier, anchored to this run.
    LaunchTarget {
        view: TypeId,
    },
    /// Write a Bytes carrier through the private describe transport.
    DescribeEmit,
    /// Owner flow operation with pointers to its fixed operand and result words.
    Flow(hirsel_core_layout::flow::Op),
    /// SHA-1 compression of a byte span and its RFC 3174 padding.
    Sha1Blocks,
    /// Zero a byte span of a list's storage (crate docs, "Zero fill").
    ZeroBytes,
    /// XXH3-64 over at most 240 bytes with seed zero and the default secret.
    Xxh3Short,
    /// Accumulate complete XXH3 stripes in eight words with the given secret.
    Xxh3Stripes,
    /// Take the socket or read the scratch grant designated by the place host.
    LaunchHandle,
    /// Extend a finalized Castagnoli CRC over a readable byte range.
    Crc32c,
    /// Extend a finalized NVME CRC over a readable byte range.
    Crc64Nvme,
    /// Concatenate finalized CRC-32C checksums.
    Crc32cCombine,
    /// Concatenate finalized CRC-64/NVME checksums.
    Crc64NvmeCombine,
    /// Verify a pure Ed25519 signature over a message.
    Ed25519Verify,
    /// Verify a fixed-width P-256 ECDSA signature over a message with SHA-256.
    P256Verify,
    /// Verify a DER P-256 ECDSA signature over a message with SHA-256.
    P256VerifyDer,
    /// Verify a fixed-width P-384 ECDSA signature over a message with SHA-384.
    P384Verify,
    /// Verify a DER P-384 ECDSA signature over a message with SHA-384.
    P384VerifyDer,
    /// Verify an RSA PKCS#1 v1.5 signature over a message with SHA-256.
    RsaVerifyPkcs1Sha256,
    /// Verify an RSA-PSS signature with SHA-256 and a 32-byte salt.
    RsaVerifyPssSha256,
    /// Argon2id v0x13 with block steps and temporary working-memory admission.
    Argon2id,
    /// The running task's frame record (`crate::returnable`). Takes the area
    /// size and `u8` flags for network and connection-send state. Allocated
    /// on first entry, reused on resume, and freed when the root ends.
    FrameRecord,
    /// A wait that may park with the stack given back (`crate::returnable`):
    /// a leading pointer to the caller's child word, then the wrapped row's
    /// operands and results. The word is 1 when the task parked and the
    /// result is not yet delivered, else 0 with the row's result.
    Returnable(Wait),
    /// `Receive` that may park with the stack given back (`crate::returnable`):
    /// a leading pointer to the caller's child word, then `Receive`'s operands.
    /// The word is 1 when the task parked and nothing is received yet, else 0
    /// with `Receive`'s outcome.
    ReceiveRec {
        item: TypeId,
    },
    /// Hand the running task to a fresh root (`crate::returnable`): copy the
    /// `x` input the leading address takes into new zero-charge task input,
    /// with the code word after it, and make the task's body enter that code
    /// through the transfer glue of `(x, r)` once its current frames have
    /// returned; a nonzero `u8` says the root reads none of it once parked,
    /// so it is freed then. True when taken; false (no frame record, replay).
    Transfer {
        x: TypeId,
        r: TypeId,
    },
    /// SHA-1 compression of complete blocks, without padding.
    Sha1Compress,
    /// A function on LANG-20's closed list is reached; [`CLOSED`] names it by
    /// index. It does nothing at run time. A constant's initialiser that
    /// reaches it stops there, and the constant is refused.
    Closed(u8),
}

/// The primary cause a `Raise` records (FAIL-2). Overflow, Bounds and Division are
/// recorded by the instructions that raise them; Cancelled by the engine operations
/// that observe it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RaiseCause {
    Unwrap,
    Bounds,
    /// `fail(msg)`: takes the `String` at the address.
    Fail(AddressId),
    /// A refused admission (MEM-21): `Capacity(c)` with `c`'s two `u64` words, the
    /// refused charge and the room left in the limit tried (MEM-22).
    Capacity {
        requested: ValueId,
        available: ValueId,
    },
}

impl RtOp {
    /// The signature row a call of this operation names. A `take` input is `&take T`
    /// and stays the caller's when the operation refuses before its commit point; an
    /// output the operation writes is `&mut T`.
    ///
    /// | op | params | results |
    /// |---|---|---|
    /// | `spawn[X, R]` | `n: u64`, `&X`, `f: ptr`, `tasks: u64` | task `ptr` (null on `Capacity`), `available: u64` |
    /// | `join[R]` | task `ptr`, `&R` | status `u8`: 0 Ok (R written), 1 Failed, 2 Cancelled; `Causes` `ptr` (null when Ok) |
    /// | `cancel` | task `ptr` | |
    /// | `check_cancel` | | |
    /// | `channel_new[T]` | `cap: u64` | sender `ptr`, receiver `ptr` (null on `Capacity`), `available: u64` |
    /// | `channel_sender` | sender `ptr` | sender `ptr` |
    /// | `send[T]` | sender `ptr`, `&T` | `bool`: published (false: `Closed`, the item stays) |
    /// | `try_send[T]` | sender `ptr`, `&T` | status `u8`: 0 published, 1 `Full`, 2 `Closed` |
    /// | `receive[T]` | receiver `ptr`, `&T` | `bool`: received (false: `Closed`) |
    /// | `try_receive[T]` | receiver `ptr`, `&T` | status `u8`: 0 received, 1 `Empty`, 2 `Closed` |
    /// | `dest_id` | | `u64`: a destination identity no other call of the run returns (MEM-4) |
    /// | `compare_bytes` | `a: ptr`, `a_len: u64`, `b: ptr`, `b_len: u64` | `i8`: -1, 0 or 1 as `a` orders before, equal to or after `b` |
    /// | `map_hash_bytes` | `data: ptr`, `len: u64` | `u64`: keyed byte sequence hash |
    /// | `map_hash_word` | `word: u64` | `u64`: keyed scalar hash |
    /// | `map_hash_start` | `state: ptr` | |
    /// | `map_hash_add_word` | `state: ptr`, `word: u64` | |
    /// | `map_hash_add_bytes` | `state: ptr`, `data: ptr`, `len: u64` | |
    /// | `map_hash_finish` | `state: ptr` | `u64`: keyed hash of the stream |
    /// | `xxh3_short` | `data: ptr`, `len: u64` | `u64`: XXH3-64 (crate docs, "XXH3") |
    /// | `xxh3_stripes` | `acc: ptr`, `data: ptr`, `secret: ptr`, `count: u64` | (crate docs, "XXH3") |
    /// | `sha1_blocks` | state `ptr`, `data: ptr`, `len: u64` | (crate docs, "SHA-1") |
    /// | `sha1_compress` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-1") |
    /// | `zero_bytes` | `block: ptr`, `at: ptr`, `len: u64` | (crate docs, "Zero fill") |
    /// | `sha256_blocks` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-256") |
    /// | `sha512_blocks` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-512") |
    /// | `crc32c` | `crc: u32`, `data: ptr`, `len: u64` | `u32`: updated CRC-32C |
    /// | `crc64nvme` | `crc: u64`, `data: ptr`, `len: u64` | `u64`: updated CRC-64/NVME |
    /// | `crc32c_combine` | `a: u32`, `b: u32`, `len_b: u64` | `u32`: concatenated CRC-32C |
    /// | `crc64nvme_combine` | `a: u64`, `b: u64`, `len_b: u64` | `u64`: concatenated CRC-64/NVME |
    /// | `ed25519_verify`, `p256_verify`, `p256_verify_der`, `p384_verify`, `p384_verify_der` | key, message and signature pointer/length pairs (lengths `u64`) | `u64`: 1 if valid, 0 otherwise |
    /// | `rsa_verify_pkcs1_sha256`, `rsa_verify_pss_sha256` | modulus, exponent, message and signature pointer/length pairs (lengths `u64`) | `u64`: 1 if valid, 0 otherwise |
    /// | `cause_next` | cell `ptr` | `ptr`: the next cell, null after the last |
    /// | `cause_value` | cell `ptr` | `ptr`: the cell's `Cause` |
    /// | `args_len` | `Args: u64` | `u64` |
    /// | `args_get` | `Args: u64`, `i: u64` | `ptr`: a `String` carrier, null past the end |
    /// | `host_open` | `Dir: u64`, path `ptr`, `mode: u8` | outcome |
    /// | `host_read` | `HostFile: u64`, buffer `ptr` | outcome |
    /// | `host_write` | `HostFile: u64`, bytes `ptr` | outcome |
    /// | `host_close`, `host_try_lock`, `host_unlock`, `host_sync` | `HostFile: u64` | outcome |
    /// | `host_sync_dir`, `host_zero_extension` | `Dir: u64` | outcome |
    /// | `host_random` | `Random: u64`, buffer `ptr` | |
    /// | `host_mkdir`, `host_remove`, `host_remove_tree` | `Dir: u64`, path `ptr` | outcome |
    /// | `host_mkdtemp` | `Dir: u64`, name `ptr` | outcome |
    /// | `host_wall_ms`, `host_mono_ns` | `Clock: u64` | `u64` |
    /// | `stdout_write`, `stderr_write` | stream `u64`, string `ptr` | outcome |
    /// | `isolate_enter` | | `bool`: reserved; `available: u64` (0 when reserved) |
    /// | `isolate_leave` | | |
    /// | `isolate_catch` | | `Causes` `ptr`: the caught chain's head |
    ///
    /// `f` is a plain function `(&X) -> () dest R`. `channel_new` with `cap` 0 fails
    /// with `Bounds`; `send`, `receive`, `join` and `check_cancel` fail with
    /// `Cancelled` when they observe it (CONC-7). `isolate_catch` fails, adding no
    /// cause, when the chain it would catch has the primary cause `Cancelled`. Every
    /// provider row can fail (for example `Cancelled` or `Bounds`). These are the rows
    /// that can fail
    /// ([`RtOp::fails`]); the others never do.
    ///
    /// `compare_bytes` orders unsigned bytes from the first, a proper prefix first
    /// (SEM-7); it reads both ranges, writes nothing, admits nothing and takes no
    /// context. A pointer with length 0 is never read, so it may be null.
    ///
    /// The provider rows (crate docs, "Providers") take the capability or file
    /// handle, and each `String` or `Bytes` operand as a pointer to its carrier.
    /// An outcome is `value: u64, error: u8, code: u32, written: u64`: `error` is
    /// 0 for success, else `IoError`'s case + 1 (`code` is `Other`'s payload),
    /// `value` is the opened `HostFile`, the bytes read, the lock taken (1) or the
    /// new `Dir`, and `written` the bytes written.
    pub fn signature(self) -> Signature {
        use Param::Value as V;
        let take = |ty| Param::Address {
            place: PlaceTy::Declared(ty),
            loan: Loan::Take,
        };
        let out = |ty| Param::Address {
            place: PlaceTy::Declared(ty),
            loan: Loan::Mut,
        };
        let (u8_, u64_, bool_) = (
            Ty::Scalar(Scalar::U8),
            Ty::Scalar(Scalar::U64),
            Ty::Scalar(Scalar::Bool),
        );
        let p = Ty::Ptr;
        let (params, results) = match self {
            RtOp::Spawn { x, .. } => (vec![V(u64_), take(x), V(p), V(u64_)], vec![p, u64_]),
            RtOp::Join { r } => (vec![V(p), out(r)], vec![u8_, p]),
            RtOp::Cancel => (vec![V(p)], vec![]),
            RtOp::CheckCancel => (vec![], vec![]),
            RtOp::ChannelNew { .. } => (vec![V(u64_)], vec![p, p, u64_]),
            RtOp::ChannelSender => (vec![V(p)], vec![p]),
            RtOp::Send { item } => (vec![V(p), take(item)], vec![bool_]),
            RtOp::TrySend { item } => (vec![V(p), take(item)], vec![u8_]),
            RtOp::Receive { item } => (vec![V(p), out(item)], vec![bool_]),
            RtOp::ReceiveRec { item } => (vec![V(p), V(p), out(item)], vec![bool_]),
            RtOp::TryReceive { item } => (vec![V(p), out(item)], vec![u8_]),
            RtOp::LatestNew { .. } => (vec![V(p)], vec![p, u64_]),
            RtOp::ConnNew { .. } => (vec![V(u64_), V(u64_)], vec![p, p, u64_]),
            RtOp::ConnClient => (vec![V(p)], vec![p]),
            RtOp::ConnSend { item } | RtOp::ConnTrySend { item } => {
                (vec![V(p), take(item)], vec![u8_])
            }
            RtOp::ConnReceive { item } | RtOp::ConnTryReceive { item } => {
                (vec![V(p), out(item)], vec![u8_, u64_, u64_])
            }
            RtOp::SpawnUnit { x, .. } => {
                (vec![V(u64_), take(x), V(p), V(u64_)], vec![p, u64_, u64_])
            }
            RtOp::LatestHandle | RtOp::LatestRead | RtOp::SharedShare => (vec![V(p)], vec![p]),
            RtOp::SharedNew { item } => (vec![take(item), V(u64_)], vec![p, u64_]),
            RtOp::SharedRelease { .. } => (vec![V(p)], vec![]),
            RtOp::LatestPublish => (vec![V(p), V(p)], vec![]),
            RtOp::DestId => (vec![], vec![u64_]),
            RtOp::CompareBytes => (
                vec![V(p), V(u64_), V(p), V(u64_)],
                vec![Ty::Scalar(Scalar::I8)],
            ),
            RtOp::MapHashBytes | RtOp::MapPlaceBytes | RtOp::MapStrongBytes => {
                (vec![V(p), V(u64_)], vec![u64_])
            }
            RtOp::MapHashWord | RtOp::MapPlaceWord | RtOp::MapStrongWord => {
                (vec![V(u64_)], vec![u64_])
            }
            RtOp::MapHashStart | RtOp::MapStrongStart => (vec![V(p)], vec![]),
            RtOp::MapHashAddWord => (vec![V(p), V(u64_)], vec![]),
            RtOp::MapHashAddBytes => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::MapHashFinish => (vec![V(p)], vec![u64_]),
            RtOp::Xxh3Short => (vec![V(p), V(u64_)], vec![u64_]),
            RtOp::Xxh3Stripes => (vec![V(p), V(p), V(p), V(u64_)], vec![]),
            RtOp::Sha1Blocks | RtOp::Sha1Compress => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::ZeroBytes => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::Sha256Blocks | RtOp::Sha512Blocks => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::Crc32c => {
                let u32_ = Ty::Scalar(Scalar::U32);
                (vec![V(u32_), V(p), V(u64_)], vec![u32_])
            }
            RtOp::Crc64Nvme => (vec![V(u64_), V(p), V(u64_)], vec![u64_]),
            RtOp::Crc32cCombine => {
                let u32_ = Ty::Scalar(Scalar::U32);
                (vec![V(u32_), V(u32_), V(u64_)], vec![u32_])
            }
            RtOp::Crc64NvmeCombine => (vec![V(u64_), V(u64_), V(u64_)], vec![u64_]),
            RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer => (
                vec![V(p), V(u64_), V(p), V(u64_), V(p), V(u64_)],
                vec![u64_],
            ),
            RtOp::RsaVerifyPkcs1Sha256 | RtOp::RsaVerifyPssSha256 => (
                vec![V(p), V(u64_), V(p), V(u64_), V(p), V(u64_), V(p), V(u64_)],
                vec![u64_],
            ),
            RtOp::Argon2id => {
                let u32_ = Ty::Scalar(Scalar::U32);
                (
                    vec![
                        V(p),
                        V(u64_),
                        V(p),
                        V(u64_),
                        V(p),
                        V(u64_),
                        V(p),
                        V(u64_),
                        V(u32_),
                        V(u32_),
                        V(u32_),
                        V(u64_),
                        V(p),
                    ],
                    vec![u64_, u64_, u8_],
                )
            }
            RtOp::CauseNext | RtOp::CauseValue => (vec![V(p)], vec![p]),
            RtOp::ArgsLen => (vec![V(u64_)], vec![u64_]),
            RtOp::ArgsGet => (vec![V(u64_), V(u64_)], vec![p]),
            RtOp::RootLimit => (vec![], vec![u64_]),
            RtOp::LaunchWord => (vec![V(u64_), V(u64_)], vec![u64_]),
            RtOp::LaunchHandle | RtOp::SecretShare => (vec![V(u64_)], vec![u64_]),
            RtOp::FrameRecord => (vec![V(u64_), V(u8_), V(u8_)], vec![p]),
            RtOp::Transfer { x, .. } => (vec![take(x), V(p), V(u8_)], vec![bool_]),
            RtOp::Returnable(wait) => {
                let row = wait.row().signature();
                let mut params = vec![V(p)];
                params.extend(row.params);
                (params, row.results)
            }
            RtOp::LaunchTarget { view } => (vec![out(view)], vec![]),
            RtOp::DescribeEmit => (vec![V(p)], vec![bool_]),
            RtOp::Flow(_) => (vec![V(p), V(p), V(p)], vec![]),
            RtOp::StepEnter => (vec![V(u64_)], vec![bool_]),
            RtOp::StepLeave | RtOp::IsolateLeave | RtOp::Closed(_) => (vec![], vec![]),
            RtOp::StepCatch => (vec![], vec![bool_]),
            RtOp::IsolateEnter => (vec![], vec![bool_, u64_]),
            RtOp::IsolateCatch => (vec![], vec![p]),
            RtOp::Host(op) => op.operand_signature(),
        };
        Signature::new(params, results, None)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Callee {
    Direct(FuncId),
    /// A code pointer value, called with the call site's signature.
    Indirect(ValueId),
    /// An engine operation.
    Runtime(RtOp),
}

/// A call argument: a value, or the address of a place passed by address.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Arg {
    Value(ValueId),
    Address(AddressId),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct CallSite {
    pub callee: Callee,
    pub signature: SigId,
    pub args: PoolRange<Arg>,
    pub results: PoolRange<ValueId>,
    /// Where a failure of the callee continues; None propagates it
    /// out of this function with no cleanup here.
    pub fail: Option<BlockId>,
    /// The call is on a path the front knows is rarely taken: the arm of a
    /// growth check that runs only when a sequence or map is full. A hint
    /// with no behaviour; an engine may ignore it.
    pub cold: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Edge {
    pub target: BlockId,
    pub args: PoolRange<ValueId>,
    /// The front knows this edge is rarely taken: it enters the growth path of
    /// a list reserve, which runs only when the list lacks room. A hint with no
    /// behaviour; an engine may ignore it.
    pub cold: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct SwitchArm {
    pub value: u64,
    pub edge: EdgeId,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Terminator {
    Jump(EdgeId),
    Branch {
        condition: ValueId,
        yes: EdgeId,
        no: EdgeId,
    },
    /// Arms in order, values distinct; `otherwise` takes every other value.
    Switch {
        value: ValueId,
        arms: PoolRange<SwitchArm>,
        otherwise: EdgeId,
    },
    /// The signature's results, in order.
    Return(PoolRange<ValueId>),
    /// FORM v1a: the point after a call whose signature is `never` (its block's last
    /// instruction), a read of a `Never` binder (FORM #31a), or an instruction that
    /// always fails (a failing narrow's overflowing add). Nothing reaches it.
    Unreachable,
    /// Record a cause in the chain (FAIL-10), then continue at `cleanup`, or
    /// propagate when there is none.
    Raise {
        cause: RaiseCause,
        cleanup: Option<BlockId>,
    },
    /// Leave the function failing; the caller continues at its call's `fail`.
    Propagate,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Block {
    pub params: PoolRange<ValueId>,
    pub insts: PoolRange<Inst>,
    /// The terminator's source, None for generated code.
    pub anchor: Option<Anchor>,
    /// The terminator is a source decision (`docs/coverage.md`, "Sites"): an
    /// `if`, `while` or `for` test, a `?`, a pattern test, a guard, or the
    /// left side of `&&` or `||`. A terminator the front generates inside an
    /// expression keeps its anchor for debug lines and law rows but is not a
    /// decision. Only an anchored branch or switch decides.
    pub decision: bool,
}

/// Where code came from: a byte range relative to the start of an item, so an
/// item moved in its file keeps its body; the driver rebinds items to files at
/// link time (as `hirsel-lir/src/debug.rs` already persists).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Anchor {
    pub item: ItemId,
    pub start: u32,
    pub end: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Pools {
    pub values: Vec<ValueId>,
    pub steps: Vec<Step>,
    pub args: Vec<Arg>,
    pub arms: Vec<SwitchArm>,
}

/// One function body. Block 0 is the entry; its parameters are the signature's
/// parameters (a by-address parameter arrives as a pointer value).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Body {
    /// Entry values whose source parameter is an immutable callable. Optional
    /// optimisation information; execution and semantic instance identity ignore it.
    pub callable_parameters: Vec<ValueId>,
    pub values: Vec<Ty>,
    pub slots: Vec<PlaceTy>,
    pub addresses: Vec<AddressInfo>,
    pub blocks: Vec<Block>,
    /// Parallel to `blocks`.
    pub terminators: Vec<Terminator>,
    /// Returns to the dispatcher with the result destination still uninitialized.
    /// Added by returnable lowering after the checked body is assembled.
    #[serde(skip)]
    pub parked_returns: std::collections::BTreeSet<BlockId>,
    pub insts: Vec<Inst>,
    /// Parallel to `insts`; None for generated code (no source line).
    pub inst_anchors: Vec<Option<Anchor>>,
    pub calls: Vec<CallSite>,
    pub edges: Vec<Edge>,
    pub pools: Pools,
    /// The blobs this body roots (MEM-4): each root's type, as a place row, and
    /// the `W` its blob records.
    pub compact_roots: Vec<CompactRoot>,
}

impl Body {
    /// The storage addresses with a lexical frame obligation in this body.
    pub fn frame_obligations(&self) -> std::collections::BTreeSet<AddressId> {
        self.insts
            .iter()
            .filter_map(|inst| match *inst {
                Inst::FrameAdmit { storage, .. }
                | Inst::FrameActive { storage, .. }
                | Inst::FrameRefund { storage } => Some(storage),
                _ => None,
            })
            .collect()
    }
}

/// One blob root a body creates: `build_in` or `build_stream_in` of `ty`, whose
/// blob records `width` (2, 4 or 8) as its `W`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CompactRoot {
    pub ty: TypeId,
    pub width: u8,
}

/// How a parameter is passed.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Param {
    Value(Ty),
    /// A pointer to a place of the declared type, owned by the caller, lent for
    /// the whole call under `loan` (crate docs, "Loans").
    Address {
        place: PlaceTy,
        loan: Loan,
    },
}

/// How a by-address parameter's place is lent to the callee (OWN-7..11). The extent
/// is the call: entry to every exit, failure cleanup included.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Loan {
    /// Exclusive: during the call no other argument's place, and no other path the
    /// callee can reach, overlaps it. Its bytes may change.
    Mut,
    /// Frozen: no write anywhere touches its bytes during the call. Other views of
    /// the same place may be passed alongside it.
    View,
    /// The callee owns the value: it consumes the place by reading it into its own
    /// slot or by dropping or moving out of it in place, and neither it nor
    /// anything else writes the place during the call. A copyable value may be
    /// lent from a place other arguments still view. A refusal before the callee's
    /// commit point leaves it the caller's. See the crate docs, "Loans", for what
    /// `noalias` on it claims.
    Take,
}

/// An instantiated signature. `dest` is the declared type of a result returned
/// through a caller-provided destination, passed as a leading pointer parameter.
/// `never` (FORM v1a) is a function whose result type is `Never`: it has no results
/// and no `dest`, its body never returns, and a call of it ends its block, followed
/// by `Unreachable`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Signature {
    pub params: Vec<Param>,
    pub results: Vec<Ty>,
    pub dest: Option<TypeId>,
    /// The parameters (indices into `params`, ascending) a `dest` result is a view
    /// anchored on (OWN-20): the destination borrows their places, so it dangles
    /// once one of them is dropped. Empty for an owned result or no `dest`.
    pub anchors: Vec<u32>,
    pub never: bool,
}

impl Signature {
    /// A signature that returns, with no dest view.
    pub fn new(params: Vec<Param>, results: Vec<Ty>, dest: Option<TypeId>) -> Self {
        let (anchors, never) = (vec![], false);
        Signature {
            params,
            results,
            dest,
            anchors,
            never,
        }
    }
    pub fn has_vectors(&self) -> bool {
        self.results.iter().any(|t| matches!(t, Ty::Vector(_)))
            || self.params.iter().any(|p| {
                matches!(
                    p,
                    Param::Value(Ty::Vector(_))
                        | Param::Address {
                            place: PlaceTy::Value(Ty::Vector(_)),
                            ..
                        },
                )
            })
    }
}

/// One instantiation as a row of its module (see the crate docs, "Instances"): its
/// type arguments are rows of the same module, so the row means nothing apart from
/// the module; [`Module::identity`] is its module-independent identity.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct InstanceKey {
    pub definition: DefPath,
    /// Each closed type argument as a row of this module, in parameter order.
    pub arguments: Vec<TypeId>,
    /// Canonical text of the implementation chosen for each constraint, in order.
    pub dictionaries: Vec<String>,
}

/// An instantiation apart from any module: its definition, its ordered dictionaries
/// and the closure hash of each type argument ([`Closure`]).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Identity {
    pub definition: DefPath,
    pub dictionaries: Vec<String>,
    pub closures: Vec<ContentHash>,
}
impl Ord for Identity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order_fields().cmp(&other.order_fields())
    }
}

impl PartialOrd for Identity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Identity {
    fn order_fields(&self) -> (&str, &str, &[String], &str, &[String], &[ContentHash]) {
        let path = &self.definition.0;
        (
            &path.package.name,
            &path.package.version,
            &path.modules,
            &path.name,
            &self.dictionaries,
            &self.closures,
        )
    }

    /// The non-generic definition `definition`.
    pub fn plain(definition: DefPath) -> Self {
        Self {
            definition,
            dictionaries: vec![],
            closures: vec![],
        }
    }
    /// The content identity the link name and caches use: a versioned, length-framed
    /// SHA-256 of the identity's encoding.
    pub fn digest(&self) -> ContentHash {
        let definition = self.definition.to_string();
        let counts =
            [self.dictionaries.len() as u64, self.closures.len() as u64].map(u64::to_le_bytes);
        let parts = [definition.as_bytes(), &counts[0], &counts[1]]
            .into_iter()
            .chain(self.dictionaries.iter().map(String::as_bytes))
            .chain(self.closures.iter().map(|c| &c.0[..]));
        ContentHash::of_parts("hirsel.core-symbol.v1", parts)
    }
    /// The link name: `h` and the 64 hex digits of [`Self::digest`].
    pub fn symbol(&self) -> String {
        symbol(self.digest())
    }
}
/// `h` and the 64 hex digits of `digest`.
pub fn symbol(digest: ContentHash) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(65);
    name.push('h');
    for byte in digest.0 {
        name.push(HEX[(byte >> 4) as usize] as char);
        name.push(HEX[(byte & 15) as usize] as char);
    }
    name
}

mod revision;
pub use revision::{RevisionRoot, intrinsic_revision, revision_symbol};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Function {
    pub instance: InstanceId,
    pub signature: SigId,
    /// None for a function defined in another module (linked by name).
    pub body: Option<Body>,
    /// This definition has a vector-typed parameter, result, binding or expression.
    pub versioned: bool,
    /// LANG-8 `cold fn`: kept out of line; calls to it are cold sites. A hint only.
    #[serde(default)]
    pub cold: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Static {
    Bytes {
        align: u32,
        bytes: Vec<u8>,
    },
    Aggregate {
        ty: TypeId,
        value: StaticValue,
    },
    /// A computed constant's slot (LANG-20, PROG-10): the reader names the
    /// constant and its type, never its value. A build binds it to the value
    /// ([`Static::bind`]) before anything reads its bytes.
    Computed {
        ty: FieldTy,
        constant: DefPath,
    },
}

/// Compiler-owned writer bookkeeping, never reachable as source payload storage.
pub const WRITER_STATE_SHAPE: &str = "$hirsel.writer.state";
/// The same bookkeeping with the destination identity in its sixth word.
pub const WRITER_DEST_STATE_SHAPE: &str = "$hirsel.writer.destination-state";

/// One unit of emission.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Module {
    pub items: Vec<DefPath>,
    /// Each declared type's shape (its `name` is its canonical text). `Shape`'s
    /// `TypeRef` and `FuncRef` index this table and `functions`.
    pub types: Vec<Row>,
    pub statics: Vec<Static>,
    pub signatures: Vec<Signature>,
    pub instances: Vec<InstanceKey>,
    pub functions: Vec<Function>,
    /// A native assembly with foreign grants passes caller sites to bodyless
    /// functions. Ordinary bodies keep their own sites. This is a link-time
    /// condition derived from the launch, outside the checked body encoding.
    #[serde(skip)]
    pub external_call_sites: bool,
    /// Insert engine observations only while emitting a recording build.
    #[serde(skip)]
    pub law_observation: bool,
    /// Flag the sites of bodies from these packages while emitting a coverage
    /// build ([`crate::coverage`]). Like `law_observation`, a condition of the
    /// emission, outside the checked body encoding.
    #[serde(skip)]
    pub coverage: Option<crate::coverage::Scope>,
    /// In a coverage build of units whose anchors name their unit's sites
    /// (release units), each unit's decisions as its source gives them
    /// ([`crate::coverage::Rebound`]).
    #[serde(skip)]
    pub coverage_rebound: Option<std::sync::Arc<crate::coverage::Rebound>>,
    /// In a coverage build, each body a pass after binding rebuilt with blocks
    /// of its own (a returnable variant or root), by function: for each block
    /// of the body it was built from, the block that ends it
    /// ([`crate::coverage::Moved`]).
    #[serde(skip)]
    pub coverage_moved: crate::coverage::Moved,
    /// Callable code variants' identities, separate from PROG-5 semantic instances:
    /// an optimiser may emit several code bodies for one instance without changing
    /// the identity of its function values.
    pub code_variants: std::collections::BTreeMap<FuncId, ContentHash>,
    /// MEM-30 step-free bodies ([`crate::step_contexts`]): `step_free[f]` runs
    /// `f` while no step bound is current, a time no step can be observed.
    pub step_free: std::collections::BTreeMap<FuncId, StepFree>,
}
/// The body that runs a function while no step bound is current.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct StepFree {
    /// A private code variant of the same instance and signature.
    pub body: FuncId,
    /// Code pointers, finalizers or the launcher may enter the function under a
    /// bound or without one: its symbol selects this body or the checked one by
    /// the current step window, and both bodies share one calling convention.
    pub dispatch: bool,
}
impl Module {
    pub fn passes_caller_site(&self, function: FuncId) -> bool {
        let function = &self.functions[function.index()];
        crate::source::inherits_site(&self.instances[function.instance.index()])
            || (self.law_observation
                && crate::law::callee(&self.instances[function.instance.index()].definition)
                    .is_some())
            || (self.external_call_sites && function.body.is_none())
    }

    /// The identity of function `function`'s code: a code variant's, or its
    /// instance's [`Identity::digest`].
    pub fn code_identity(&self, function: FuncId) -> ContentHash {
        self.code_variants
            .get(&function)
            .copied()
            .unwrap_or_else(|| {
                self.identity(self.functions[function.index()].instance)
                    .digest()
            })
    }
    /// The link name of every function, in function order: `h` and the hex of its
    /// [`Self::code_identity`]. Each row's closure is taken once.
    pub fn link_symbols(&self) -> Vec<String> {
        let mut closures = std::collections::HashMap::<TypeId, ContentHash>::new();
        self.functions
            .iter()
            .enumerate()
            .map(|(i, f)| {
                if let Some(&code) = self.code_variants.get(&FuncId(i as u32)) {
                    return symbol(code);
                }
                let key = &self.instances[f.instance.index()];
                symbol(
                    Identity {
                        definition: key.definition.clone(),
                        dictionaries: key.dictionaries.clone(),
                        closures: key
                            .arguments
                            .iter()
                            .map(|&t| *closures.entry(t).or_insert_with(|| self.closure(t).hash))
                            .collect(),
                    }
                    .digest(),
                )
            })
            .collect()
    }
    /// The module-independent identity of instance row `instance`.
    pub fn identity(&self, instance: InstanceId) -> Identity {
        let key = &self.instances[instance.index()];
        Identity {
            definition: key.definition.clone(),
            dictionaries: key.dictionaries.clone(),
            closures: key
                .arguments
                .iter()
                .map(|&t| self.closure(t).hash)
                .collect(),
        }
    }
    /// The canonical text of row `ty`, for diagnostics only: identities never read it.
    pub fn type_name(&self, ty: TypeId) -> &str {
        &self.types[ty.index()].name
    }
    /// The row names of `key`'s type arguments, for diagnostics and tests.
    pub fn argument_names(&self, key: &InstanceKey) -> Vec<&str> {
        key.arguments.iter().map(|&t| self.type_name(t)).collect()
    }
    /// Instance row `instance` rendered with its arguments' row names, for diagnostics.
    pub fn render_instance(&self, instance: InstanceId) -> String {
        let key = &self.instances[instance.index()];
        let mut out = key.definition.to_string();
        if !key.arguments.is_empty() {
            out.push('[');
            for (i, &t) in key.arguments.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(self.type_name(t));
            }
            out.push(']');
        }
        out
    }
}

/// A hook of a closure row: its definition and dictionaries, each type argument as a
/// closure position, and its signature with every declared row and `dest` as a
/// position.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClosureHook {
    pub definition: DefPath,
    pub dictionaries: Vec<String>,
    pub arguments: Vec<u32>,
    pub signature: Signature,
}

/// A row of a [`Closure`]: the shape with every row it names replaced by that row's
/// closure position and its hooks unset, and its finalizer, clone body and codec maker
/// as hooks.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClosureRow {
    pub shape: Shape,
    pub hooks: [Option<ClosureHook>; 4],
}

/// The rows a root row reaches, independent of any module's numbering: positions are
/// first-visit order from the root (position 0), over fields, then `held`, then hook
/// type arguments, then hook signature rows. Two rows of two modules are one type
/// exactly when their closures are equal; `hash` names it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Closure {
    pub rows: Vec<ClosureRow>,
    pub hash: ContentHash,
}
impl Closure {
    /// The closure of `rows`, hashed over their encoding (the encoding the store
    /// hashes IR with).
    pub fn new(rows: Vec<ClosureRow>) -> Self {
        let hash = crate::normal::closure_hash(&rows);
        Self { rows, hash }
    }
}

pub(crate) enum ClosureKind {
    Full,
    Abi,
}
