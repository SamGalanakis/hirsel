//! The CoreIR data types. Extracted from `hirsel-lir/src/ir.rs` (u32 ID arenas,
//! typed pools, immutable values plus mutable slots, block parameters, edges with
//! arguments, source anchors); WHY-NEW-FORM.md lists, per LIR opcode, what was kept,
//! rewritten or deleted.
//!
//! Values are defined exactly once and hold a scalar, pointer or fixed vector; aggregates live in
//! slots or behind pointers and are reached through addresses. Slots are ordinary
//! memory: a read names no "reaching version", so a producer need not build SSA for
//! slots or prove liveness before emitting.

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
    /// limit tried (MEM-22), and nothing is allocated; on success `available` is 0. Under a finalizer's grant a
    /// refusal is a failure with cause `Capacity` instead (MEM-27).
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
        fail: Option<BlockId>,
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
    /// its elements are unchanged; under a finalizer's grant a refusal is the failure
    /// `Capacity` (MEM-27). On success the first `moved` bytes (the carrier's
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
        fail: Option<BlockId>,
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
    /// Installs a node and returns the old root's node, which the caller releases.
    LatestPublish,
    /// The root's node, which the caller shares before any other event.
    LatestRead,
    DestId,
    /// Bytewise order of two byte ranges (SEM-7): `String`, `Text` and `Bytes`.
    CompareBytes,
    /// The fixed byte hash a `Map` key's text components fold (crate docs, "Maps").
    HashBytes,
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
    /// `tape.open` (CAP-9): open a Run tape in a Dir.
    TapeOpen,
    /// `tape.end`: end a tape with its domain's outcome, consuming it.
    TapeEnd,
    /// `tape.clock`, `tape.dir`, `tape.stdout`, `tape.stderr`, `tape.args`:
    /// a capability's substitute on a tape.
    TapeGrant,
    /// `tape.across`: whether an Across open's revision differs.
    TapeAcross,
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
}

impl HostOp {
    /// Native entry for rows that use the common operand signature.
    pub fn io_symbol(self) -> &'static str {
        match self {
            Self::WallMs | Self::MonoNs => "hirsel_core_host_clock",
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
        HostOp::Sync,
        HostOp::SyncDir,
        HostOp::Random,
        HostOp::Connect,
        HostOp::Listen,
        HostOp::Accept,
        HostOp::RecvBounded,
        HostOp::SendPair,
        HostOp::NetNarrow,
        HostOp::NetSplit,
        HostOp::Stopped,
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
            HostOp::Sync => "host_sync",
            HostOp::SyncDir => "host_sync_dir",
            HostOp::Random => "host_random",
            HostOp::Connect => "host_connect",
            HostOp::Listen => "host_listen",
            HostOp::Accept => "host_accept",
            HostOp::RecvBounded => "host_recv_bounded",
            HostOp::SendPair => "host_send_pair",
            HostOp::NetNarrow => "net_narrow",
            HostOp::NetSplit => "net_split",
            HostOp::Stopped => "host_stopped",
        }
    }
    /// C entry of a socket or Net ownership/authority adapter.
    pub const fn net_symbol(self) -> &'static str {
        match self {
            Self::Connect => "hirsel_core_host_connect",
            Self::Listen => "hirsel_core_host_listen",
            Self::Accept => "hirsel_core_host_accept",
            Self::RecvBounded => "hirsel_core_host_recv_bounded",
            Self::SendPair => "hirsel_core_host_send_pair",
            Self::NetNarrow => "hirsel_core_net_narrow",
            Self::NetSplit => "hirsel_core_net_split",
            _ => unreachable!(),
        }
    }
    /// Whether the operation waits (a cancellation point, so it can fail).
    pub const fn waits(self) -> bool {
        !matches!(
            self,
            HostOp::WallMs | HostOp::MonoNs | HostOp::Random | HostOp::NetNarrow | HostOp::NetSplit
        )
    }
}

/// The primary cause a `Raise` records (FAIL-2). Overflow, Bounds and Division are
/// recorded by the instructions that raise them; Cancelled by the engine operations
/// that observe it; Capacity by an allocation over a grant.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RaiseCause {
    Unwrap,
    Bounds,
    /// `fail(msg)`: takes the `String` at the address.
    Fail(AddressId),
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
    /// | `hash_bytes` | `data: ptr`, `len: u64` | `u64`: the fixed byte hash (crate docs, "Maps") |
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
    /// | `tape_open` | `Dir: u64`, name `ptr`, `mode: u8`, identity `ptr`, arg `ptr`, `limit: u64`, `id0: u64`, `id1: u64` | `Tape: u64` (0 unless opened), `code: u64` |
    /// | `tape_end` | `Tape: u64`, `kind: u8`, operand `ptr` | `code: u64` |
    /// | `tape_grant` | `Tape: u64`, `kind: u8`, capability `u64` | substitute `u64` |
    /// | `tape_across` | `Tape: u64` | `bool` |
    /// | `isolate_enter` | | `bool`: reserved; `available: u64` (0 when reserved) |
    /// | `isolate_leave` | | |
    /// | `isolate_catch` | | `Causes` `ptr`: the caught chain's head |
    ///
    /// `f` is a plain function `(&X) -> () dest R`. `channel_new` with `cap` 0 fails
    /// with `Bounds`; `send`, `receive`, `join` and `check_cancel` fail with
    /// `Cancelled` when they observe it (CONC-7). `isolate_catch` fails, adding no
    /// cause, when the chain it would catch has the primary cause `Cancelled`. Every
    /// provider row and `tape_grant` can fail with `Tape` (CAP-9). These are the rows
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
            RtOp::TryReceive { item } => (vec![V(p), out(item)], vec![u8_]),
            RtOp::LatestNew { .. } => (vec![V(p)], vec![p, u64_]),
            RtOp::LatestHandle | RtOp::LatestRead => (vec![V(p)], vec![p]),
            RtOp::LatestPublish => (vec![V(p), V(p)], vec![]),
            RtOp::DestId => (vec![], vec![u64_]),
            RtOp::CompareBytes => (
                vec![V(p), V(u64_), V(p), V(u64_)],
                vec![Ty::Scalar(Scalar::I8)],
            ),
            RtOp::HashBytes => (vec![V(p), V(u64_)], vec![u64_]),
            RtOp::CauseNext | RtOp::CauseValue => (vec![V(p)], vec![p]),
            RtOp::ArgsLen => (vec![V(u64_)], vec![u64_]),
            RtOp::ArgsGet => (vec![V(u64_), V(u64_)], vec![p]),
            RtOp::LaunchWord => (vec![V(u64_), V(u64_)], vec![u64_]),
            RtOp::LaunchTarget { view } => (vec![out(view)], vec![]),
            RtOp::DescribeEmit => (vec![V(p)], vec![bool_]),
            RtOp::Flow(_) => (vec![V(p), V(p), V(p)], vec![]),
            RtOp::TapeOpen => (
                vec![V(u64_), V(p), V(u8_), V(p), V(p), V(u64_), V(u64_), V(u64_)],
                vec![u64_, u64_],
            ),
            RtOp::TapeEnd => (vec![V(u64_), V(u8_), V(p)], vec![u64_]),
            RtOp::TapeGrant => (vec![V(u64_), V(u8_), V(u64_)], vec![u64_]),
            RtOp::TapeAcross => (vec![V(u64_)], vec![bool_]),
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
                    HostOp::NetSplit => (vec![V(u64_)], outcome),
                    HostOp::Open => (vec![V(u64_), V(p), V(u8_)], outcome),
                    HostOp::Rename => (vec![V(u64_), V(p), V(p)], outcome),
                    HostOp::SleepUntil => (vec![V(u64_), V(u64_)], outcome),
                    HostOp::Close
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
                | RtOp::IsolateCatch
                | RtOp::TapeGrant
                | RtOp::Flow(_)
        ) || matches!(self, RtOp::Host(_))
    }

    pub fn types(self) -> Vec<TypeId> {
        match self {
            RtOp::Spawn { x, r } => vec![x, r],
            RtOp::Join { r } => vec![r],
            RtOp::ChannelNew { item }
            | RtOp::Send { item }
            | RtOp::TrySend { item }
            | RtOp::Receive { item }
            | RtOp::TryReceive { item }
            | RtOp::LatestNew { item } => vec![item],
            RtOp::LaunchTarget { view } => vec![view],
            RtOp::Cancel
            | RtOp::CheckCancel
            | RtOp::ChannelSender
            | RtOp::LatestHandle
            | RtOp::LatestPublish
            | RtOp::LatestRead
            | RtOp::DestId
            | RtOp::CompareBytes
            | RtOp::HashBytes
            | RtOp::CauseNext
            | RtOp::CauseValue
            | RtOp::ArgsLen
            | RtOp::ArgsGet
            | RtOp::LaunchWord
            | RtOp::DescribeEmit
            | RtOp::Flow(_)
            | RtOp::Host(_)
            | RtOp::TapeOpen
            | RtOp::TapeEnd
            | RtOp::TapeGrant
            | RtOp::TapeAcross
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
            Inst::Step { .. } | Inst::Alloc { .. } | Inst::Grow { .. } | Inst::Drop { .. } => true,
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
            | Inst::Alloc { fail, .. }
            | Inst::Grow { fail, .. }
            | Inst::Drop { fail, .. } => fail,
            Inst::Call { call } => b.calls.get(call.index()).and_then(|c| c.fail),
            _ => None,
        }
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
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct Identity {
    pub definition: DefPath,
    pub dictionaries: Vec<String>,
    pub closures: Vec<ContentHash>,
}
impl Identity {
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
/// `revision(root)` (`hirsel.code-revision.v1`) over the instances `row`
/// describes: each one's body hash and the instances it names. None when an
/// instance the walk reaches has no row.
pub fn code_revision(
    root: &Identity,
    row: impl FnMut(&Identity) -> Option<(ContentHash, Vec<Identity>)>,
) -> Option<ContentHash> {
    let reached = revision_rows(root, row)?;
    let mut rows: Vec<[u8; 64]> = reached
        .iter()
        .map(|(identity, body)| {
            let mut row = [0; 64];
            row[..32].copy_from_slice(&identity.digest().0);
            row[32..].copy_from_slice(&body.0);
            row
        })
        .collect();
    rows.sort_unstable();
    let count = (rows.len() as u64).to_le_bytes();
    Some(ContentHash::of_parts(
        "hirsel.code-revision.v1",
        [&root.digest().0[..], &count[..]]
            .into_iter()
            .chain(rows.iter().map(|r| &r[..])),
    ))
}

fn revision_rows<T>(
    root: &Identity,
    mut row: impl FnMut(&Identity) -> Option<(T, Vec<Identity>)>,
) -> Option<std::collections::BTreeMap<Identity, T>> {
    let mut reached = std::collections::BTreeMap::new();
    let mut pending = vec![root.clone()];
    while let Some(identity) = pending.pop() {
        if reached.contains_key(&identity) {
            continue;
        }
        let (body, children) = row(&identity)?;
        pending.extend(children.into_iter().filter(|c| !reached.contains_key(c)));
        reached.insert(identity, body);
    }
    Some(reached)
}
/// The code revision of an intrinsic: its one row, with the zero body.
pub fn intrinsic_revision(root: &Identity) -> ContentHash {
    code_revision(root, |_| Some((ContentHash::default(), vec![]))).expect("one row")
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
    /// Callable code variants' identities, separate from PROG-5 semantic instances:
    /// an optimiser may emit several code bodies for one instance without changing
    /// the identity of its function values.
    pub code_variants: std::collections::BTreeMap<FuncId, ContentHash>,
}
impl Module {
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
        let hash = ContentHash::of_encoded("hirsel.core-closure.v1", &rows);
        Self { rows, hash }
    }
}

impl Module {
    /// The closure of row `root` ([`Closure`]).
    pub fn closure(&self, root: TypeId) -> Closure {
        let mut order = HashMap::<u32, u32>::new();
        let mut rows: Vec<Option<ClosureRow>> = vec![];
        let mut pending = vec![root.0];
        order.insert(root.0, 0);
        rows.push(None);
        let visit = |r: u32,
                     order: &mut HashMap<u32, u32>,
                     rows: &mut Vec<Option<ClosureRow>>,
                     pending: &mut Vec<u32>| {
            let next = order.len() as u32;
            *order.entry(r).or_insert_with(|| {
                rows.push(None);
                pending.push(r);
                next
            })
        };
        while let Some(t) = pending.pop() {
            let mut shape = self.types[t as usize].clone();
            shape.map_refs(&mut |r| TypeRef(visit(r.0, &mut order, &mut rows, &mut pending)));
            let hook = |f: Option<FuncRef>,
                        order: &mut HashMap<u32, u32>,
                        rows: &mut Vec<Option<ClosureRow>>,
                        pending: &mut Vec<u32>| {
                let f = &self.functions[f?.0 as usize];
                let key = &self.instances[f.instance.index()];
                let arguments = key
                    .arguments
                    .iter()
                    .map(|a| visit(a.0, order, rows, pending))
                    .collect();
                let mut signature = self.signatures[f.signature.index()].clone();
                for param in &mut signature.params {
                    if let Param::Address {
                        place: PlaceTy::Declared(id),
                        ..
                    } = param
                    {
                        *id = TypeId(visit(id.0, order, rows, pending));
                    }
                }
                signature.dest = signature
                    .dest
                    .map(|id| TypeId(visit(id.0, order, rows, pending)));
                Some(ClosureHook {
                    definition: key.definition.clone(),
                    dictionaries: key.dictionaries.clone(),
                    arguments,
                    signature,
                })
            };
            let hooks = [
                hook(shape.finalizer.take(), &mut order, &mut rows, &mut pending),
                hook(shape.clone.take(), &mut order, &mut rows, &mut pending),
                hook(shape.codec.take(), &mut order, &mut rows, &mut pending),
            ];
            rows[order[&t] as usize] = Some(ClosureRow { shape, hooks });
        }
        Closure::new(
            rows.into_iter()
                .map(|r| r.expect("every visited row is filled"))
                .collect(),
        )
    }
}

/// One step of the `Map` key hash (crate docs, "Maps"): `rotl((h ^ w) × K, 26)`.
pub fn hash_mix(h: u64, w: u64) -> u64 {
    (h ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(26)
}

/// `rt.hash_bytes` (crate docs, "Maps"): from `h = len`, [`hash_mix`] over the
/// little-endian 8-byte words, the last zero-padded, then `h ^ (h >> 32)`. The
/// producer uses it for static map literals; the run time's export is the same.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = bytes.len() as u64;
    for chunk in bytes.chunks(8) {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        h = hash_mix(h, u64::from_le_bytes(word));
    }
    h ^ (h >> 32)
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
