//! The CoreIR data types. Extracted from `hirsel-lir/src/ir.rs` (u32 ID arenas,
//! typed pools, immutable values plus mutable slots, block parameters, edges with
//! arguments, source anchors); WHY-NEW-FORM.md lists, per LIR opcode, what was kept,
//! rewritten or deleted.
//!
//! Values are defined exactly once and hold a scalar, pointer or fixed vector; aggregates live in
//! slots or behind pointers and are reached through addresses. Slots are ordinary
//! memory: a read names no "reaching version", so a producer need not build SSA for
//! slots or prove liveness before emitting.

use crate::decode::{DecodeBudget, DecodeError, DecodeLimit, DecodeResult};
use hirsel_core_ids::{ContentHash, DefPath};
pub use hirsel_core_layout::{Carried, Scalar, Shape, Ty, Vector};
use hirsel_core_layout::{Cleanup, FieldTy, FuncRef, Recipe, TypeRef, cleanup_class};
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
/// The vector value operations of SEM-11.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SimdOp {
    WrappingAdd,
    WrappingSub,
    SaturatingAdd,
    SaturatingSub,
    Eq,
    Lt,
    Splat,
    From,
    Swizzle,
    Cast,
    Any,
    Bitmask,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    Shl,
    Shr,
    Lanes,
}
impl SimdOp {
    pub const ALL: [Self; 19] = [
        Self::WrappingAdd,
        Self::WrappingSub,
        Self::SaturatingAdd,
        Self::SaturatingSub,
        Self::Eq,
        Self::Lt,
        Self::Splat,
        Self::From,
        Self::Swizzle,
        Self::Cast,
        Self::Any,
        Self::Bitmask,
        Self::BitAnd,
        Self::BitOr,
        Self::BitXor,
        Self::BitNot,
        Self::Shl,
        Self::Shr,
        Self::Lanes,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::WrappingAdd => "wrapping_add",
            Self::WrappingSub => "wrapping_sub",
            Self::SaturatingAdd => "saturating_add",
            Self::SaturatingSub => "saturating_sub",
            Self::Eq => "eq",
            Self::Lt => "lt",
            Self::Splat => "splat",
            Self::From => "from",
            Self::Swizzle => "swizzle",
            Self::Cast => "cast",
            Self::Any => "any",
            Self::Bitmask => "bitmask",
            Self::BitAnd => "bit_and",
            Self::BitOr => "bit_or",
            Self::BitXor => "bit_xor",
            Self::BitNot => "bit_not",
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::Lanes => "lanes",
        }
    }

    pub const fn binary(self) -> bool {
        matches!(
            self,
            Self::WrappingAdd
                | Self::WrappingSub
                | Self::SaturatingAdd
                | Self::SaturatingSub
                | Self::Eq
                | Self::Lt
                | Self::Swizzle
                | Self::BitAnd
                | Self::BitOr
                | Self::BitXor
                | Self::Shl
                | Self::Shr
        )
    }

    pub const fn can_fail(self) -> bool {
        matches!(self, Self::Shl | Self::Shr)
    }
}

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
    /// Refund `charge`, which an `alloc .. frame` admitted, to the current limit; the
    /// frame bytes stay where they are. Never fails.
    FrameRefund {
        charge: ValueId,
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
    /// Store the first count lanes of a byte vector at to, after the source
    /// operation's bounds check and reservation for those bytes.
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
}

/// What an `Alloc` makes. The block's bookkeeping (its limit record,
/// and for a resource its cause cell and grant) is the engine's.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AllocKind {
    /// Own storage for the owning edge that will hold it (crate docs, "Cleanup
    /// classes"): `box tN`, `seq E`, `map K V`, or
    /// `rc tN` / `arc tN`, a counted node: the count (u64, starting at 1) at offset 0,
    /// the payload at 8. `arc` makes the node with the atomic count every later
    /// `share`, `release` and `try_unwrap` of it uses (MEM-15); its operations name the
    /// same kind. The edge's [`hirsel_core_layout::cleanup_class`] fixes the block's
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
    pub fn cleanup(&self, shapes: &[Shape], recipes: &[Recipe]) -> Option<Cleanup> {
        match self {
            AllocKind::Own { edge } => cleanup_class(shapes, recipes, edge),
            AllocKind::Environment { .. } => Some(Cleanup::Fixed),
            AllocKind::Region => None,
            AllocKind::Resource { .. } => Some(Cleanup::Resource),
        }
    }
    /// Whether the block is terminal: the 8-byte payer prefix, freed with nothing
    /// to drop (crate docs, "Cleanup classes").
    pub fn terminal(&self, shapes: &[Shape], recipes: &[Recipe]) -> bool {
        self.cleanup(shapes, recipes) == Some(Cleanup::Terminal)
    }
}

/// The owning `seq E` edge of a sequence carrier place, which `grow` replaces the
/// storage of (crate docs, "Growth"): a row whose only field is that edge, as
/// `List[T]` (`items`) and `String` (`bytes`) are. None for any other place.
pub fn sequence_edge(shapes: &[Shape], place: PlaceTy) -> Option<&FieldTy> {
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
    /// Charged like `Current`, with the bytes at this pointer, the caller's frame, as
    /// the block: nothing is allocated, and `FrameRefund` later returns the charge.
    Frame(ValueId),
    /// Frame storage with a live allocation record understood by lent callees.
    /// The aligned slot includes FRAME_LIST_LINK_BYTES after its payload.
    LentFrame(ValueId),
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
    /// Verify a pure Ed25519 signature over a message.
    Ed25519Verify,
    /// Verify a fixed-width P-256 ECDSA signature over a message with SHA-256.
    P256Verify,
    /// Verify a DER P-256 ECDSA signature over a message with SHA-256.
    P256VerifyDer,
    /// The running task's frame record (`crate::returnable`): `size` zeroed
    /// bytes allocated at the first call and the same bytes at every later
    /// one, freed when the task ends.
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
}

/// The waits with a returnable row ([`RtOp::Returnable`]), each the stackful
/// row [`Wait::row`] with the child word in front.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Wait {
    /// An owned network wait (`RecvBounded`, `SendPair`).
    Host(HostOp),
    Join {
        r: TypeId,
    },
    ConnSend {
        item: TypeId,
    },
    ConnReceive {
        item: TypeId,
    },
}

impl Wait {
    /// The stackful row this wait wraps.
    pub fn row(self) -> RtOp {
        match self {
            Wait::Host(op) => RtOp::Host(op),
            Wait::Join { r } => RtOp::Join { r },
            Wait::ConnSend { item } => RtOp::ConnSend { item },
            Wait::ConnReceive { item } => RtOp::ConnReceive { item },
        }
    }

    /// The returnable wait of a stackful row, if it has one.
    pub fn of(row: RtOp) -> Option<Wait> {
        Some(match row {
            RtOp::Host(op @ (HostOp::RecvBounded | HostOp::SendPair)) => Wait::Host(op),
            RtOp::Join { r } => Wait::Join { r },
            RtOp::ConnSend { item } => Wait::ConnSend { item },
            RtOp::ConnReceive { item } => Wait::ConnReceive { item },
            _ => return None,
        })
    }
}

/// The provider operations, numbered as the run time's `HirselCoreHostRequest`
/// operation codes ([`HostOp::code`]).
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HostOp {
    Open,
    Read,
    Write,
    Close,
    TryLock,
    Unlock,
    Mkdir,
    Mkdtemp,
    Remove,
    RemoveTree,
    WallMs,
    MonoNs,
    Stdout,
    Stderr,
    Rename,
    SleepUntil,
    /// Flush a file's data and read-visible metadata with fdatasync.
    Sync,
    /// Flush directory entries with fsync.
    SyncDir,
    /// Fill a Bytes carrier's spare capacity from the Random capability.
    Random,
    Connect,
    Listen,
    Accept,
    RecvBounded = 27,
    SendPair = 26,
    NetNarrow = 24,
    NetSplit = 25,
    Stopped = 28,
    TlsConnect = 22,
    NetNarrowTls = 29,
    SendSecret = 23,
    SecretSeal = 30,
    SecretNarrow = 31,
    OpenDir = hirsel_core_ids::host_op::OPEN_DIR as isize,
    Entries = hirsel_core_ids::host_op::ENTRIES as isize,
    NextEntries = hirsel_core_ids::host_op::NEXT_ENTRIES as isize,
    Stat = hirsel_core_ids::host_op::STAT as isize,
    StatFile = hirsel_core_ids::host_op::STAT_FILE as isize,
    ReadAt = 37,
    WriteAt = 38,
    SecretHmac = hirsel_core_ids::host_op::SECRET_MAC as isize,
    SecretVerifyHmac = 42,
    Resolve = hirsel_core_ids::host_op::RESOLVE as isize,
    NetNarrowResolve = 41,
    StdinRead = hirsel_core_ids::host_op::STDIN_READ as isize,
    EnvGet = 33,
    EnvLen = 34,
    EnvName = 35,
    EnvNarrow = 36,
    ProvideClock = hirsel_core_ids::host_op::PROVIDE_CLOCK as isize,
    ProvideRandom = hirsel_core_ids::host_op::PROVIDE_RANDOM as isize,
    ProvideProcess = hirsel_core_ids::host_op::PROVIDE_PROCESS as isize,
    ClockAdvance = hirsel_core_ids::host_op::CLOCK_ADVANCE as isize,
    ClockSetWall = hirsel_core_ids::host_op::CLOCK_SET_WALL as isize,
    ClockIdle = hirsel_core_ids::host_op::CLOCK_IDLE as isize,
    ProcessTrigger = hirsel_core_ids::host_op::PROCESS_TRIGGER as isize,
    ProvideStdout = hirsel_core_ids::host_op::PROVIDE_STDOUT as isize,
    ProvideStderr = hirsel_core_ids::host_op::PROVIDE_STDERR as isize,
    PortNext = hirsel_core_ids::host_op::PORT_NEXT as isize,
    CallWord = hirsel_core_ids::host_op::CALL_WORD as isize,
    CallSecret = hirsel_core_ids::host_op::CALL_SECRET as isize,
    ExecSpawn = hirsel_core_ids::host_op::EXEC_SPAWN as isize,
    ExecWait = hirsel_core_ids::host_op::EXEC_WAIT as isize,
    ExecSignal = hirsel_core_ids::host_op::EXEC_SIGNAL as isize,
    CallPayload = hirsel_core_ids::host_op::CALL_PAYLOAD as isize,
    PortReply = hirsel_core_ids::host_op::PORT_REPLY as isize,
    ProvideDir = hirsel_core_ids::host_op::PROVIDE_DIR as isize,
    ProvideNet = hirsel_core_ids::host_op::PROVIDE_NET as isize,
    ProvideExec = hirsel_core_ids::host_op::PROVIDE_EXEC as isize,
    PortFill = hirsel_core_ids::host_op::PORT_FILL as isize,
    ScramSha256 = hirsel_core_ids::host_op::SCRAM as isize,
    ScramVerify = hirsel_core_ids::host_op::SCRAM_VERIFY as isize,
    StreamTerminal = 96,
    NetJoin = 97,
    ScramFinalLen = 98,
    ExecNarrow = hirsel_core_ids::host_op::EXEC_NARROW as isize,
    UnixConnect = hirsel_core_ids::host_op::UNIX_CONNECT as isize,
    UnixListen = hirsel_core_ids::host_op::UNIX_LISTEN as isize,
    UnixAccept = hirsel_core_ids::host_op::UNIX_ACCEPT as isize,
    DatagramOpen = hirsel_core_ids::host_op::DATAGRAM_OPEN as isize,
    DatagramBind = hirsel_core_ids::host_op::DATAGRAM_BIND as isize,
    DatagramConnect = hirsel_core_ids::host_op::DATAGRAM_CONNECT as isize,
    DatagramSend = hirsel_core_ids::host_op::DATAGRAM_SEND as isize,
    DatagramSendTo = hirsel_core_ids::host_op::DATAGRAM_SEND_TO as isize,
    DatagramRecv = hirsel_core_ids::host_op::DATAGRAM_RECV as isize,
    NetNarrowUnix = hirsel_core_ids::host_op::NET_NARROW_UNIX as isize,
    /// Wait until the clock's wall time reaches an absolute millisecond reading.
    SleepUntilWall = hirsel_core_ids::host_op::SLEEP_UNTIL_WALL as isize,
}

impl HostOp {
    /// Native entry for rows that use the common operand signature.
    pub fn io_symbol(self) -> &'static str {
        match self {
            Self::WallMs | Self::MonoNs => "hirsel_core_host_clock",
            Self::Stat | Self::StatFile => "hirsel_core_host_stat",
            Self::Random | Self::Stdout | Self::Stderr | Self::Stopped => "hirsel_core_host_io",
            _ => "hirsel_core_host_op",
        }
    }

    pub const ALL: &[HostOp] = &[
        HostOp::Open,
        HostOp::Read,
        HostOp::Write,
        HostOp::Close,
        HostOp::TryLock,
        HostOp::Unlock,
        HostOp::Mkdir,
        HostOp::Mkdtemp,
        HostOp::Remove,
        HostOp::RemoveTree,
        HostOp::WallMs,
        HostOp::MonoNs,
        HostOp::Stdout,
        HostOp::Stderr,
        HostOp::Rename,
        HostOp::SleepUntil,
        HostOp::SleepUntilWall,
        HostOp::Sync,
        HostOp::SyncDir,
        HostOp::Random,
        HostOp::Connect,
        HostOp::Listen,
        HostOp::Accept,
        HostOp::TlsConnect,
        HostOp::SendSecret,
        HostOp::SecretSeal,
        HostOp::SecretNarrow,
        HostOp::SecretHmac,
        HostOp::SecretVerifyHmac,
        HostOp::RecvBounded,
        HostOp::SendPair,
        HostOp::NetNarrow,
        HostOp::NetNarrowTls,
        HostOp::NetSplit,
        HostOp::Stopped,
        HostOp::OpenDir,
        HostOp::Entries,
        HostOp::NextEntries,
        HostOp::Stat,
        HostOp::StatFile,
        HostOp::ReadAt,
        HostOp::WriteAt,
        HostOp::Resolve,
        HostOp::NetNarrowResolve,
        HostOp::UnixConnect,
        HostOp::UnixListen,
        HostOp::UnixAccept,
        HostOp::DatagramOpen,
        HostOp::DatagramBind,
        HostOp::DatagramConnect,
        HostOp::DatagramSend,
        HostOp::DatagramSendTo,
        HostOp::DatagramRecv,
        HostOp::NetNarrowUnix,
        HostOp::StdinRead,
        HostOp::EnvGet,
        HostOp::EnvLen,
        HostOp::EnvName,
        HostOp::EnvNarrow,
        HostOp::ProvideClock,
        HostOp::ProvideRandom,
        HostOp::ProvideProcess,
        HostOp::ClockAdvance,
        HostOp::ClockSetWall,
        HostOp::ClockIdle,
        HostOp::ProcessTrigger,
        HostOp::ProvideStdout,
        HostOp::ProvideStderr,
        HostOp::PortNext,
        HostOp::CallWord,
        HostOp::CallSecret,
        HostOp::ExecSpawn,
        HostOp::ExecWait,
        HostOp::ExecSignal,
        HostOp::CallPayload,
        HostOp::PortReply,
        HostOp::ProvideDir,
        HostOp::PortFill,
        HostOp::StreamTerminal,
        HostOp::NetJoin,
        HostOp::ScramSha256,
        HostOp::ScramFinalLen,
        HostOp::ScramVerify,
        HostOp::ProvideNet,
        HostOp::ProvideExec,
        HostOp::ExecNarrow,
    ];
    /// The run time's operation code (`hirsel_core_rt_host.h`).
    pub const fn code(self) -> u8 {
        self as u8
    }
    /// The row's name after `rt.`.
    pub const fn name(self) -> &'static str {
        match self {
            HostOp::Open => "host_open",
            HostOp::Read => "host_read",
            HostOp::OpenDir => "host_open_dir",
            HostOp::Entries => "host_entries",
            HostOp::NextEntries => "host_next_entries",
            HostOp::Stat => "host_stat",
            HostOp::StatFile => "host_stat_file",
            HostOp::ReadAt => "host_read_at",
            HostOp::WriteAt => "host_write_at",
            HostOp::Write => "host_write",
            HostOp::Close => "host_close",
            HostOp::TryLock => "host_try_lock",
            HostOp::Unlock => "host_unlock",
            HostOp::Mkdir => "host_mkdir",
            HostOp::Mkdtemp => "host_mkdtemp",
            HostOp::Remove => "host_remove",
            HostOp::RemoveTree => "host_remove_tree",
            HostOp::WallMs => "host_wall_ms",
            HostOp::MonoNs => "host_mono_ns",
            HostOp::Stdout => "stdout_write",
            HostOp::Stderr => "stderr_write",
            HostOp::Rename => "host_rename",
            HostOp::SleepUntil => "host_sleep_until",
            HostOp::SleepUntilWall => "host_sleep_until_wall",
            HostOp::Sync => "host_sync",
            HostOp::SyncDir => "host_sync_dir",
            HostOp::Random => "host_random",
            HostOp::Connect => "host_connect",
            HostOp::Resolve => "host_resolve",
            HostOp::Listen => "host_listen",
            HostOp::Accept => "host_accept",
            HostOp::RecvBounded => "host_recv_bounded",
            HostOp::SendPair => "host_send_pair",
            HostOp::NetNarrow => "net_narrow",
            HostOp::ExecNarrow => "exec_narrow",
            HostOp::NetNarrowTls => "net_narrow_tls",
            HostOp::NetNarrowResolve => "net_narrow_resolve",
            HostOp::UnixConnect => "host_connect_unix",
            HostOp::UnixListen => "host_listen_unix",
            HostOp::UnixAccept => "host_accept_unix",
            HostOp::DatagramOpen => "host_open_datagram",
            HostOp::DatagramBind => "host_bind_datagram",
            HostOp::DatagramConnect => "host_connect_datagram",
            HostOp::DatagramSend => "host_send_datagram",
            HostOp::DatagramSendTo => "host_send_to",
            HostOp::DatagramRecv => "host_recv_datagram",
            HostOp::NetNarrowUnix => "net_narrow_unix",
            HostOp::TlsConnect => "tls_connect",
            HostOp::SendSecret => "send_secret",
            HostOp::SecretSeal => "secret_seal",
            HostOp::SecretNarrow => "secret_narrow",
            HostOp::SecretHmac => "secret_hmac_sha256",
            HostOp::SecretVerifyHmac => "secret_verify_hmac_sha256",
            HostOp::NetSplit => "net_split",
            HostOp::NetJoin => "net_join",
            HostOp::ScramSha256 => "scram_sha256",
            HostOp::ScramFinalLen => "scram_final_len",
            HostOp::ScramVerify => "scram_verify",
            HostOp::Stopped => "host_stopped",
            HostOp::StdinRead => "stdin_read",
            HostOp::EnvGet => "env_get",
            HostOp::EnvLen => "env_len",
            HostOp::EnvName => "env_name",
            HostOp::EnvNarrow => "env_narrow",
            HostOp::ProvideClock => "provide_clock",
            HostOp::ProvideRandom => "provide_random",
            HostOp::ProvideProcess => "provide_process",
            HostOp::ClockAdvance => "clock_advance_to",
            HostOp::ClockSetWall => "clock_set_wall",
            HostOp::ClockIdle => "clock_idle",
            HostOp::ProcessTrigger => "process_trigger",
            HostOp::ProvideStdout => "provide_stdout",
            HostOp::ProvideStderr => "provide_stderr",
            HostOp::PortNext => "port_next",
            HostOp::CallWord => "call_word",
            HostOp::CallSecret => "call_secret",
            HostOp::ExecSpawn => "exec_spawn",
            HostOp::ExecWait => "exec_wait",
            HostOp::ExecSignal => "exec_signal",
            HostOp::CallPayload => "call_payload",
            HostOp::PortReply => "port_reply",
            HostOp::ProvideDir => "provide_dir",
            HostOp::ProvideNet => "provide_net",
            HostOp::ProvideExec => "provide_exec",
            HostOp::PortFill => "port_fill",
            HostOp::StreamTerminal => "stream_is_terminal",
        }
    }
    /// C entry of a host operation that has its own adapter.
    pub const fn adapter_symbol(self) -> &'static str {
        match self {
            Self::Connect => "hirsel_core_host_connect",
            Self::Resolve => "hirsel_core_host_resolve",
            Self::Listen => "hirsel_core_host_listen",
            Self::Accept => "hirsel_core_host_accept",
            Self::RecvBounded => "hirsel_core_host_recv_bounded",
            Self::SendPair => "hirsel_core_host_send_pair",
            Self::NetNarrow => "hirsel_core_net_narrow",
            Self::ExecNarrow => "hirsel_core_exec_narrow",
            Self::NetNarrowTls => "hirsel_core_net_narrow_tls",
            Self::NetNarrowResolve => "hirsel_core_net_narrow_resolve",
            Self::UnixConnect => "hirsel_core_host_connect_unix",
            Self::UnixListen => "hirsel_core_host_listen_unix",
            Self::UnixAccept => "hirsel_core_host_accept_unix",
            Self::DatagramOpen => "hirsel_core_host_open_datagram",
            Self::DatagramBind => "hirsel_core_host_bind_datagram",
            Self::DatagramConnect => "hirsel_core_host_connect_datagram",
            Self::DatagramSend => "hirsel_core_host_send_datagram",
            Self::DatagramSendTo => "hirsel_core_host_send_to",
            Self::DatagramRecv => "hirsel_core_host_recv_datagram",
            Self::NetNarrowUnix => "hirsel_core_net_narrow_unix",
            Self::TlsConnect => "hirsel_core_tls_connect",
            Self::SendSecret => "hirsel_core_host_send_secret",
            Self::SecretSeal => "hirsel_core_secret_seal",
            Self::SecretNarrow => "hirsel_core_secret_narrow",
            Self::SecretHmac => "hirsel_core_secret_hmac_sha256",
            Self::SecretVerifyHmac => "hirsel_core_secret_verify_hmac_sha256",
            Self::NetSplit => "hirsel_core_net_split",
            Self::NetJoin => "hirsel_core_net_join",
            Self::ScramSha256 => "hirsel_core_scram_sha256",
            Self::ScramFinalLen => "hirsel_core_scram_final_len",
            Self::ScramVerify => "hirsel_core_scram_verify",
            Self::StdinRead => "hirsel_core_stdin_read",
            Self::EnvGet => "hirsel_core_env_get",
            Self::EnvLen => "hirsel_core_env_len",
            Self::EnvName => "hirsel_core_env_name",
            Self::EnvNarrow => "hirsel_core_env_narrow",
            Self::ProvideClock => "hirsel_core_provide_clock",
            Self::ProvideRandom => "hirsel_core_provide_random",
            Self::ProvideProcess => "hirsel_core_provide_process",
            Self::ClockAdvance => "hirsel_core_clock_advance_to",
            Self::ClockSetWall => "hirsel_core_clock_set_wall",
            Self::ClockIdle => "hirsel_core_clock_idle",
            Self::ProcessTrigger => "hirsel_core_process_trigger",
            Self::ProvideStdout => "hirsel_core_provide_stdout",
            Self::ProvideStderr => "hirsel_core_provide_stderr",
            Self::PortNext => "hirsel_core_port_next",
            Self::CallWord => "hirsel_core_call_word",
            Self::CallSecret => "hirsel_core_call_secret",
            Self::ExecSpawn => "hirsel_core_exec_spawn",
            Self::ExecWait => "hirsel_core_exec_wait",
            Self::ExecSignal => "hirsel_core_exec_signal",
            Self::CallPayload => "hirsel_core_call_payload",
            Self::PortReply => "hirsel_core_port_reply",
            Self::ProvideDir => "hirsel_core_provide_dir",
            Self::ProvideNet => "hirsel_core_provide_net",
            Self::ProvideExec => "hirsel_core_provide_exec",
            Self::PortFill => "hirsel_core_port_fill",
            Self::StreamTerminal => "hirsel_core_stream_is_terminal",
            _ => unreachable!(),
        }
    }
    /// Whether the operation waits (a cancellation point, so it can fail).
    pub const fn waits(self) -> bool {
        !matches!(
            self,
            HostOp::WallMs
                | HostOp::MonoNs
                | HostOp::Random
                | HostOp::NetNarrow
                | HostOp::ExecNarrow
                | HostOp::NetNarrowTls
                | HostOp::NetNarrowResolve
                | HostOp::NetNarrowUnix
                | HostOp::SecretSeal
                | HostOp::SecretNarrow
                | HostOp::SecretHmac
                | HostOp::SecretVerifyHmac
                | HostOp::NetSplit
                | HostOp::NetJoin
                | HostOp::ScramFinalLen
                | HostOp::ScramVerify
                | HostOp::EnvGet
                | HostOp::EnvLen
                | HostOp::EnvName
                | HostOp::EnvNarrow
                | HostOp::ProvideClock
                | HostOp::ProvideRandom
                | HostOp::ProvideProcess
                | HostOp::ClockAdvance
                | HostOp::ClockSetWall
                | HostOp::ProcessTrigger
                | HostOp::ProvideStdout
                | HostOp::ProvideStderr
                | HostOp::CallWord
                | HostOp::CallSecret
                | HostOp::PortReply
                | HostOp::ProvideDir
                | HostOp::ProvideNet
                | HostOp::ProvideExec
                | HostOp::PortFill
                | HostOp::StreamTerminal
        )
    }
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
    /// | `zero_bytes` | `block: ptr`, `at: ptr`, `len: u64` | (crate docs, "Zero fill") |
    /// | `sha256_blocks` | state `ptr`, `data: ptr`, `blocks: u64` | (crate docs, "SHA-256") |
    /// | `ed25519_verify`, `p256_verify`, `p256_verify_der` | key, message and signature pointer/length pairs (lengths `u64`) | `u64`: 1 if valid, 0 otherwise |
    /// | `cause_next` | cell `ptr` | `ptr`: the next cell, null after the last |
    /// | `cause_value` | cell `ptr` | `ptr`: the cell's `Cause` |
    /// | `args_len` | `Args: u64` | `u64` |
    /// | `args_get` | `Args: u64`, `i: u64` | `ptr`: a `String` carrier, null past the end |
    /// | `host_open` | `Dir: u64`, path `ptr`, `mode: u8` | outcome |
    /// | `host_read` | `HostFile: u64`, buffer `ptr` | outcome |
    /// | `host_write` | `HostFile: u64`, bytes `ptr` | outcome |
    /// | `host_close`, `host_try_lock`, `host_unlock` | `HostFile: u64` | outcome |
    /// | `host_sync` | `HostFile: u64` | outcome |
    /// | `host_sync_dir` | `Dir: u64` | outcome |
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
            RtOp::LatestHandle | RtOp::LatestRead => (vec![V(p)], vec![p]),
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
            RtOp::Sha1Blocks => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::ZeroBytes => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::Sha256Blocks => (vec![V(p), V(p), V(u64_)], vec![]),
            RtOp::Ed25519Verify | RtOp::P256Verify | RtOp::P256VerifyDer => (
                vec![V(p), V(u64_), V(p), V(u64_), V(p), V(u64_)],
                vec![u64_],
            ),
            RtOp::CauseNext | RtOp::CauseValue => (vec![V(p)], vec![p]),
            RtOp::ArgsLen => (vec![V(u64_)], vec![u64_]),
            RtOp::ArgsGet => (vec![V(u64_), V(u64_)], vec![p]),
            RtOp::LaunchWord => (vec![V(u64_), V(u64_)], vec![u64_]),
            RtOp::LaunchHandle => (vec![V(u64_)], vec![u64_]),
            RtOp::FrameRecord => (vec![V(u64_)], vec![p]),
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
            RtOp::StepLeave => (vec![], vec![]),
            RtOp::StepCatch => (vec![], vec![bool_]),
            RtOp::IsolateEnter => (vec![], vec![bool_, u64_]),
            RtOp::IsolateLeave => (vec![], vec![]),
            RtOp::IsolateCatch => (vec![], vec![p]),
            RtOp::Host(op) => {
                let outcome = vec![u64_, u8_, Ty::Scalar(Scalar::U32), u64_];
                match op {
                    HostOp::Connect => (
                        vec![
                            V(u64_),
                            V(u8_),
                            V(u64_),
                            V(u64_),
                            V(Ty::Scalar(Scalar::U16)),
                            V(u8_),
                            V(u64_),
                            V(u64_),
                        ],
                        outcome,
                    ),
                    HostOp::Listen => (
                        vec![
                            V(u64_),
                            V(u8_),
                            V(u64_),
                            V(u64_),
                            V(Ty::Scalar(Scalar::U16)),
                            V(Ty::Scalar(Scalar::U16)),
                        ],
                        vec![
                            u64_,
                            u8_,
                            Ty::Scalar(Scalar::U32),
                            u64_,
                            u8_,
                            u64_,
                            u64_,
                            Ty::Scalar(Scalar::U16),
                        ],
                    ),
                    HostOp::Accept => (
                        vec![V(u64_), V(u8_), V(u64_), V(u64_)],
                        vec![
                            u64_,
                            u8_,
                            Ty::Scalar(Scalar::U32),
                            u64_,
                            u8_,
                            u64_,
                            u64_,
                            Ty::Scalar(Scalar::U16),
                        ],
                    ),
                    HostOp::RecvBounded => (
                        vec![V(u64_), V(p), V(u64_), V(u8_), V(u64_), V(u64_)],
                        outcome,
                    ),
                    HostOp::Resolve => {
                        (vec![V(u64_), V(p), V(p), V(u8_), V(u64_), V(u64_)], outcome)
                    }
                    HostOp::NetNarrowResolve => (vec![V(u64_), V(p)], outcome),
                    HostOp::ExecNarrow => (
                        vec![V(u64_), V(Ty::Scalar(Scalar::U32)), V(p), V(p)],
                        outcome,
                    ),
                    HostOp::UnixConnect => {
                        (vec![V(u64_), V(p), V(p), V(u8_), V(u64_), V(u64_)], outcome)
                    }
                    HostOp::UnixListen => (
                        vec![V(u64_), V(p), V(p), V(Ty::Scalar(Scalar::U16))],
                        outcome,
                    ),
                    HostOp::UnixAccept => (vec![V(u64_), V(u8_), V(u64_), V(u64_)], outcome),
                    HostOp::DatagramOpen => (vec![V(u64_)], outcome),
                    HostOp::DatagramBind => (vec![V(u64_), V(p), V(p)], outcome),
                    HostOp::DatagramConnect => (vec![V(u64_), V(p), V(p)], outcome),
                    HostOp::DatagramSend => {
                        (vec![V(u64_), V(p), V(u8_), V(u64_), V(u64_)], outcome)
                    }
                    HostOp::DatagramSendTo => (
                        vec![V(u64_), V(p), V(p), V(p), V(u8_), V(u64_), V(u64_)],
                        outcome,
                    ),
                    HostOp::DatagramRecv => {
                        (vec![V(u64_), V(p), V(u8_), V(u64_), V(u64_)], outcome)
                    }
                    HostOp::NetNarrowUnix => (vec![V(u64_), V(u8_), V(p), V(p)], outcome),
                    HostOp::SendPair => {
                        (vec![V(u64_), V(p), V(p), V(u8_), V(u64_), V(u64_)], outcome)
                    }
                    HostOp::NetNarrow => (
                        vec![
                            V(u64_),
                            V(u8_),
                            V(u8_),
                            V(u64_),
                            V(u64_),
                            V(u8_),
                            V(Ty::Scalar(Scalar::U16)),
                            V(Ty::Scalar(Scalar::U16)),
                        ],
                        outcome,
                    ),
                    HostOp::TlsConnect => (
                        vec![
                            V(u64_),
                            V(u64_),
                            V(p),
                            V(u8_),
                            V(p),
                            V(p),
                            V(u8_),
                            V(u64_),
                            V(u64_),
                        ],
                        vec![
                            u64_,
                            u8_,
                            Ty::Scalar(Scalar::U32),
                            u64_,
                            u64_,
                            u64_,
                            u64_,
                            u64_,
                        ],
                    ),
                    HostOp::SendSecret => (
                        vec![V(u64_), V(p), V(u64_), V(p), V(u8_), V(u64_), V(u64_)],
                        outcome,
                    ),
                    HostOp::ScramSha256 => (
                        vec![
                            V(u64_),
                            V(u64_),
                            V(p),
                            V(p),
                            V(p),
                            V(p),
                            V(u8_),
                            V(u64_),
                            V(u64_),
                        ],
                        outcome,
                    ),
                    HostOp::ScramFinalLen => (vec![V(u64_), V(p), V(p)], outcome),
                    HostOp::ScramVerify => (vec![V(u64_), V(p)], outcome),
                    // The source carrier stays with the caller. Success wipes its
                    // owned bytes. Tags 1 (invalid) and 2 (capacity) leave it intact.
                    HostOp::SecretSeal => (vec![V(p), V(p)], outcome),
                    HostOp::SecretNarrow => (vec![V(u64_), V(p)], outcome),
                    HostOp::SecretHmac | HostOp::SecretVerifyHmac => {
                        (vec![V(u64_), V(p), V(p)], outcome)
                    }
                    HostOp::NetNarrowTls => (
                        vec![
                            V(u64_),
                            V(p),
                            V(Ty::Scalar(Scalar::U16)),
                            V(Ty::Scalar(Scalar::U16)),
                            V(u8_),
                            V(p),
                        ],
                        outcome,
                    ),
                    HostOp::NetSplit
                    | HostOp::EnvLen
                    | HostOp::ProvideRandom
                    | HostOp::ClockIdle
                    | HostOp::CallSecret
                    | HostOp::ProcessTrigger => (vec![V(u64_)], outcome),
                    HostOp::ProvideProcess => (vec![], outcome),
                    HostOp::ProvideDir
                    | HostOp::ProvideNet
                    | HostOp::ProvideExec
                    | HostOp::PortNext => (vec![V(u64_)], outcome),
                    HostOp::ProvideStdout | HostOp::ProvideStderr => {
                        (vec![V(u64_), V(Ty::Scalar(Scalar::Bool))], outcome)
                    }
                    HostOp::StreamTerminal => (vec![V(u64_), V(u8_)], outcome),
                    HostOp::CallWord | HostOp::NetJoin => (vec![V(u64_), V(u64_)], outcome),
                    HostOp::CallPayload => (vec![V(u64_), V(u64_), V(p)], outcome),
                    HostOp::PortReply => (vec![V(u64_); 9], outcome),
                    HostOp::PortFill => (vec![V(u64_), V(u64_), V(p)], outcome),
                    HostOp::ProvideClock | HostOp::ClockAdvance | HostOp::ClockSetWall => {
                        (vec![V(u64_), V(u64_)], outcome)
                    }
                    HostOp::EnvGet | HostOp::EnvName => {
                        let input = if op == HostOp::EnvGet { p } else { u64_ };
                        (
                            vec![V(u64_), V(input)],
                            vec![p, u8_, Ty::Scalar(Scalar::U32), u64_],
                        )
                    }
                    HostOp::ExecSpawn => {
                        let mut results = outcome;
                        results.extend([u64_; 3]);
                        (
                            vec![
                                V(u64_),
                                V(p),
                                V(p),
                                V(p),
                                V(p),
                                V(p),
                                V(u8_),
                                V(u8_),
                                V(u8_),
                            ],
                            results,
                        )
                    }
                    HostOp::ExecWait => (vec![V(u64_), V(u8_), V(u64_), V(u64_)], outcome),
                    HostOp::ExecSignal => (vec![V(u64_), V(u8_)], outcome),
                    HostOp::StdinRead => (vec![V(u64_), V(p), V(u8_), V(u64_), V(u64_)], outcome),
                    HostOp::Open => (vec![V(u64_), V(p), V(u8_)], outcome),
                    HostOp::ReadAt | HostOp::WriteAt => (vec![V(u64_), V(u64_), V(p)], outcome),
                    HostOp::Rename => (vec![V(u64_), V(p), V(p)], outcome),
                    HostOp::SleepUntil | HostOp::SleepUntilWall => {
                        (vec![V(u64_), V(u64_)], outcome)
                    }
                    HostOp::Stat | HostOp::StatFile => {
                        let mut metadata = outcome;
                        metadata[3] = Ty::Scalar(Scalar::I64);
                        metadata.push(u8_);
                        let params = if op == HostOp::Stat {
                            vec![V(u64_), V(p)]
                        } else {
                            vec![V(u64_)]
                        };
                        (params, metadata)
                    }
                    HostOp::Close
                    | HostOp::Entries
                    | HostOp::TryLock
                    | HostOp::Unlock
                    | HostOp::Sync
                    | HostOp::SyncDir => (vec![V(u64_)], outcome),
                    HostOp::Random => (vec![V(u64_), V(p)], vec![]),
                    HostOp::Stopped => (vec![V(u64_)], vec![]),
                    HostOp::WallMs | HostOp::MonoNs => (vec![V(u64_)], vec![u64_]),
                    _ => (vec![V(u64_), V(p)], outcome),
                }
            }
        };
        Signature {
            params,
            results,
            dest: None,
            never: false,
        }
    }

    /// Whether a call of this row can fail, and so returns the failure status
    /// (crate docs, "Native ABI").
    pub fn fails(self) -> bool {
        matches!(
            self,
            RtOp::Join { .. }
                | RtOp::Spawn { .. }
                | RtOp::CheckCancel
                | RtOp::ChannelNew { .. }
                | RtOp::Send { .. }
                | RtOp::Receive { .. }
                | RtOp::ReceiveRec { .. }
                | RtOp::ConnNew { .. }
                | RtOp::ConnSend { .. }
                | RtOp::ConnTrySend { .. }
                | RtOp::ConnReceive { .. }
                | RtOp::SpawnUnit { .. }
                | RtOp::IsolateCatch
                | RtOp::Flow(_)
        ) || matches!(self, RtOp::Host(_) | RtOp::Returnable(_))
    }

    pub fn types(self) -> Vec<TypeId> {
        match self {
            RtOp::Returnable(wait) => wait.row().types(),
            RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } => vec![x, r],
            RtOp::Join { r } => vec![r],
            RtOp::ConnNew { item }
            | RtOp::ConnSend { item }
            | RtOp::ConnTrySend { item }
            | RtOp::ConnReceive { item }
            | RtOp::ConnTryReceive { item }
            | RtOp::ChannelNew { item }
            | RtOp::Send { item }
            | RtOp::TrySend { item }
            | RtOp::Receive { item }
            | RtOp::ReceiveRec { item }
            | RtOp::TryReceive { item }
            | RtOp::LatestNew { item } => vec![item],
            RtOp::LaunchTarget { view } => vec![view],
            RtOp::Cancel
            | RtOp::CheckCancel
            | RtOp::ChannelSender
            | RtOp::ConnClient
            | RtOp::LatestHandle
            | RtOp::LatestPublish
            | RtOp::LatestRead
            | RtOp::DestId
            | RtOp::CompareBytes
            | RtOp::MapHashBytes
            | RtOp::MapHashWord
            | RtOp::MapHashStart
            | RtOp::MapHashAddWord
            | RtOp::MapHashAddBytes
            | RtOp::MapHashFinish
            | RtOp::MapPlaceWord
            | RtOp::MapPlaceBytes
            | RtOp::MapStrongWord
            | RtOp::MapStrongBytes
            | RtOp::MapStrongStart
            | RtOp::Xxh3Short
            | RtOp::Xxh3Stripes
            | RtOp::Sha1Blocks
            | RtOp::ZeroBytes
            | RtOp::Sha256Blocks
            | RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::CauseNext
            | RtOp::CauseValue
            | RtOp::ArgsLen
            | RtOp::ArgsGet
            | RtOp::LaunchWord
            | RtOp::LaunchHandle
            | RtOp::DescribeEmit
            | RtOp::Flow(_)
            | RtOp::Host(_)
            | RtOp::FrameRecord
            | RtOp::IsolateEnter
            | RtOp::IsolateLeave
            | RtOp::IsolateCatch
            | RtOp::StepEnter
            | RtOp::StepLeave
            | RtOp::StepCatch => vec![],
        }
    }
}

impl Inst {
    /// Whether the instruction can fail; it may name a failure block exactly then.
    pub fn can_fail(&self, b: &Body) -> bool {
        let ty = |v: ValueId| b.values.get(v.index()).copied();
        match *self {
            Inst::Unary { op, operand, .. } => {
                op == UnaryOp::Neg && ty(operand).is_some_and(|t| t.int().is_some())
            }
            Inst::Binary { op, left, .. } => ty(left).is_some_and(|t| op.can_fail(t)),
            Inst::Simd { op, .. } => op.can_fail(),
            Inst::Address { address, .. } => b.addresses.get(address.index()).is_some_and(|a| {
                a.steps
                    .get(&b.pools.steps)
                    .unwrap_or_default()
                    .iter()
                    .any(|s| matches!(s, Step::Element(_)))
            }),
            Inst::Call { call } => b.calls.get(call.index()).is_none_or(|c| match c.callee {
                Callee::Runtime(op) => op.fails(),
                Callee::Direct(_) | Callee::Indirect(_) => true,
            }),
            Inst::Step { .. } | Inst::Drop { .. } => true,
            _ => false,
        }
    }

    /// The failure block the instruction names, if any.
    pub fn fail(&self, b: &Body) -> Option<BlockId> {
        match *self {
            Inst::Step { fail }
            | Inst::Unary { fail, .. }
            | Inst::Binary { fail, .. }
            | Inst::Simd { fail, .. }
            | Inst::Address { fail, .. }
            | Inst::Drop { fail, .. } => fail,
            Inst::Call { call } => b.calls.get(call.index()).and_then(|c| c.fail),
            _ => None,
        }
    }
}

/// The blocks control leaves `bi` for: its instructions' failure blocks and its
/// terminator's targets.
#[cfg(any(test, feature = "wf"))]
pub(crate) fn successors(b: &Body, bi: usize) -> Vec<BlockId> {
    let mut successors = Vec::new();
    for_each_successor(b, bi, |next| successors.push(next));
    successors
}

pub(crate) fn for_each_successor(b: &Body, bi: usize, mut visit: impl FnMut(BlockId)) {
    b.blocks[bi]
        .insts
        .get(&b.insts)
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i.fail(b))
        .for_each(&mut visit);
    let edge = |e: &EdgeId| b.edges[e.index()].target;
    match &b.terminators[bi] {
        Terminator::Jump(e) => visit(edge(e)),
        Terminator::Branch { yes, no, .. } => {
            visit(edge(yes));
            visit(edge(no));
        }
        Terminator::Switch {
            arms, otherwise, ..
        } => {
            arms.get(&b.pools.arms)
                .unwrap_or_default()
                .iter()
                .map(|a| edge(&a.edge))
                .for_each(&mut visit);
            visit(edge(otherwise));
        }
        Terminator::Raise { cleanup, .. } => cleanup.iter().copied().for_each(visit),
        Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => {}
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
    /// FORM v1a: the point after a call whose signature is `never`, which is its
    /// block's last instruction; such a call only fails. FORM #31a: also the point
    /// after a read of a binder whose type has no values (`Never`). Nothing reaches it.
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
    pub never: bool,
}

impl Signature {
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
/// The link name of the revision carrier of the root whose identity digest is
/// `digest` ([`Module::revision_root`]): a `Bytes` carrier `{bytes, 32, 0}` of
/// its code revision. Each unit that describes an item's function defines it
/// weak as `{0, 0, 0}` (no revision), and a link that knows the program's
/// reach defines it; an intrinsic's carrier holds [`intrinsic_revision`].
pub fn revision_symbol(digest: ContentHash) -> String {
    symbol(digest) + "$revision"
}
/// Whose code revision a described function has (PROG-9 `code.revision`):
/// the code a call through the `StaticFn` runs.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RevisionRoot {
    /// A reached instance: the described item, which a `$callable` adapter
    /// calls.
    Item(Identity),
    /// A core item without a body, which the adapter computes itself: no
    /// checked IR reaches out of it ([`intrinsic_revision`]).
    Intrinsic(Identity),
}
impl RevisionRoot {
    pub fn identity(&self) -> &Identity {
        match self {
            Self::Item(identity) | Self::Intrinsic(identity) => identity,
        }
    }
}
impl Module {
    /// The revision root of described function `function`: itself, or for a
    /// `$callable` adapter the item it calls. `code.describe` names a
    /// non-generic item, so an item without a callee is a plain intrinsic.
    pub fn revision_root(&self, function: FuncId) -> RevisionRoot {
        let f = &self.functions[function.index()];
        let identity = self.identity(f.instance);
        let Some((source, _)) = identity.definition.0.name.split_once("$callable[") else {
            return RevisionRoot::Item(identity);
        };
        let mut item = identity.definition.clone();
        item.0.name = source.to_owned();
        let callee = f
            .body
            .iter()
            .flat_map(|b| &b.calls)
            .find_map(|c| match c.callee {
                Callee::Direct(g) => {
                    let callee = self.identity(self.functions[g.index()].instance);
                    (callee.definition == item).then_some(callee)
                }
                _ => None,
            });
        match callee {
            Some(callee) => RevisionRoot::Item(callee),
            None => RevisionRoot::Intrinsic(Identity::plain(item)),
        }
    }
}
/// The code revision of a compiler-implemented core definition.
pub fn intrinsic_revision(root: &Identity) -> ContentHash {
    crate::code_revision(crate::builtin_address(root).expect("intrinsic has a semantics number"))
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

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Function {
    pub instance: InstanceId,
    pub signature: SigId,
    /// None for a function defined in another module (linked by name).
    pub body: Option<Body>,
    /// This definition has a vector-typed parameter, result, binding or expression.
    pub versioned: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Static {
    Bytes { align: u32, bytes: Vec<u8> },
    Aggregate { ty: TypeId, value: StaticValue },
}

/// A plain constant in declaration order. The enclosing type supplies each
/// field's type, and the engine supplies its native layout.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StaticValue {
    Bits(u64),
    Fields {
        case: Option<u32>,
        fields: Vec<StaticValue>,
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
    pub types: Vec<Shape>,
    pub statics: Vec<Static>,
    pub signatures: Vec<Signature>,
    pub instances: Vec<InstanceKey>,
    pub functions: Vec<Function>,
    /// A native assembly with foreign grants passes caller sites to bodyless
    /// functions. Ordinary bodies keep their own sites. This is a link-time
    /// condition derived from the launch, outside the checked body encoding.
    #[serde(skip)]
    pub external_call_sites: bool,
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
    pub hooks: [Option<ClosureHook>; 3],
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

impl Module {
    /// The closure of row `root` ([`Closure`]).
    pub fn closure(&self, root: TypeId) -> Closure {
        self.closure_impl(root, ClosureKind::Full, None)
            .expect("unmetered closure")
    }

    pub(crate) fn decoded_closure(
        &self,
        root: TypeId,
        budget: &mut DecodeBudget,
    ) -> DecodeResult<Closure> {
        self.closure_impl(root, ClosureKind::Full, Some(budget))
    }

    pub(crate) fn closure_impl(
        &self,
        root: TypeId,
        kind: ClosureKind,
        mut budget: Option<&mut DecodeBudget>,
    ) -> DecodeResult<Closure> {
        use std::collections::hash_map::Entry;
        let mut order = HashMap::<u32, u32>::new();
        let mut rows: Vec<Option<ClosureRow>> = Vec::new();
        let mut pending = Vec::new();
        let visit = |r: u32,
                     order: &mut HashMap<u32, u32>,
                     rows: &mut Vec<Option<ClosureRow>>,
                     pending: &mut Vec<u32>,
                     budget: &mut Option<&mut DecodeBudget>| {
            if let Some(&number) = order.get(&r) {
                return Ok(number);
            }
            if let Some(budget) = budget.as_deref_mut() {
                budget.reserve(rows, 1)?;
                budget.reserve(pending, 1)?;
                if order.len() == order.capacity() {
                    // This accounts for the growing hash table, including
                    // bucket control bytes and unused load-factor capacity.
                    budget.charge_bytes(order.capacity().max(4).saturating_mul(32))?;
                    order
                        .try_reserve(1)
                        .map_err(|_| DecodeError::Limit(DecodeLimit::Bytes))?;
                }
            }
            let next = order.len() as u32;
            let Entry::Vacant(entry) = order.entry(r) else {
                unreachable!()
            };
            entry.insert(next);
            rows.push(None);
            pending.push(r);
            Ok(next)
        };
        visit(root.0, &mut order, &mut rows, &mut pending, &mut budget)?;
        while let Some(t) = pending.pop() {
            let original = &self.types[t as usize];
            if let Some(budget) = budget.as_deref_mut() {
                crate::normal::charge_shape(original, budget)?;
            }
            let mut shape = original.clone();
            let mut error = None;
            shape.map_refs(&mut |r| match visit(
                r.0,
                &mut order,
                &mut rows,
                &mut pending,
                &mut budget,
            ) {
                Ok(number) => TypeRef(number),
                Err(refused) => {
                    error = Some(refused);
                    r
                }
            });
            if let Some(error) = error {
                return Err(error);
            }
            let mut hook = |f: Option<FuncRef>| -> DecodeResult<Option<ClosureHook>> {
                let Some(f) = f else { return Ok(None) };
                let f = &self.functions[f.0 as usize];
                let key = &self.instances[f.instance.index()];
                let original = &self.signatures[f.signature.index()];
                if let Some(budget) = budget.as_deref_mut() {
                    crate::normal::charge_identity(&key.definition, &key.dictionaries, budget)?;
                    budget.allocation::<u32>(key.arguments.len())?;
                    budget.allocation::<Param>(original.params.len())?;
                    budget.allocation::<Ty>(original.results.len())?;
                }
                let mut arguments = Vec::with_capacity(key.arguments.len());
                for argument in &key.arguments {
                    arguments.push(visit(
                        argument.0,
                        &mut order,
                        &mut rows,
                        &mut pending,
                        &mut budget,
                    )?);
                }
                let mut signature = original.clone();
                for param in &mut signature.params {
                    if let Param::Address {
                        place: PlaceTy::Declared(id),
                        ..
                    } = param
                    {
                        *id = TypeId(visit(
                            id.0,
                            &mut order,
                            &mut rows,
                            &mut pending,
                            &mut budget,
                        )?);
                    }
                }
                if let Some(id) = &mut signature.dest {
                    *id = TypeId(visit(
                        id.0,
                        &mut order,
                        &mut rows,
                        &mut pending,
                        &mut budget,
                    )?);
                }
                Ok(Some(ClosureHook {
                    definition: key.definition.clone(),
                    dictionaries: key.dictionaries.clone(),
                    arguments,
                    signature,
                }))
            };
            // A codec maker is selected by the checking context. It is type
            // metadata and contributes no layout or calling-convention facts.
            let codec = shape.codec.take();
            let hooks = [
                hook(shape.finalizer.take())?,
                hook(shape.clone.take())?,
                match kind {
                    ClosureKind::Full => hook(codec)?,
                    ClosureKind::Abi => None,
                },
            ];
            rows[order[&t] as usize] = Some(ClosureRow { shape, hooks });
        }
        if let Some(budget) = budget.as_deref_mut() {
            budget.allocation::<ClosureRow>(rows.len())?;
        }
        let rows: Vec<_> = rows
            .into_iter()
            .map(|r| r.expect("every visited row is filled"))
            .collect();
        let hash = match budget {
            Some(budget) => crate::normal::decoded_closure_hash(&rows, budget)?,
            None => crate::normal::closure_hash(&rows),
        };
        Ok(Closure { rows, hash })
    }
}

/// `m` with every `mark` and `unmark` removed (FORM v1c). Neither has a run-time
/// effect, so an engine must emit the same code for both; engines' tests compare them.
pub fn without_marks(m: &Module) -> Module {
    let mut m = m.clone();
    for b in m.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        let (mut insts, mut anchors) = (Vec::new(), Vec::new());
        for block in &mut b.blocks {
            let start = insts.len() as u32;
            for i in block.insts.start..block.insts.start + block.insts.len {
                if !matches!(b.insts[i as usize], Inst::Mark { .. } | Inst::Unmark { .. }) {
                    insts.push(b.insts[i as usize].clone());
                    anchors.push(b.inst_anchors[i as usize]);
                }
            }
            block.insts = PoolRange::new(start, insts.len() as u32 - start);
        }
        (b.insts, b.inst_anchors) = (insts, anchors);
    }
    m
}
