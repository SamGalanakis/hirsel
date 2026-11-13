//! The CoreIR data types. Extracted from `hirsel-lir/src/ir.rs` (u32 ID arenas,
//! typed pools, immutable values plus mutable slots, block parameters, edges with
//! arguments, source anchors); WHY-NEW-FORM.md lists, per LIR opcode, what was kept,
//! rewritten or deleted.
//!
//! Values are defined exactly once and hold a scalar or a pointer; aggregates live in
//! slots or behind pointers and are reached through addresses. Slots are ordinary
//! memory: a read names no "reaching version", so a producer need not build SSA for
//! slots or prove liveness before emitting.

use hirsel_core_ids::{ContentHash, DefPath};
pub use hirsel_core_layout::{Scalar, Shape};
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

/// The type of a value: a scalar or an opaque target pointer.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Ty {
    Scalar(Scalar),
    Ptr,
}
impl Ty {
    pub fn int(self) -> Option<(bool, u32)> {
        match self {
            Ty::Scalar(s) => match s {
                Scalar::I8 | Scalar::I16 | Scalar::I32 | Scalar::I64 => {
                    Some((true, s.size() as u32 * 8))
                }
                Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64 => {
                    Some((false, s.size() as u32 * 8))
                }
                _ => None,
            },
            Ty::Ptr => None,
        }
    }
    pub fn is_float(self) -> bool {
        matches!(self, Ty::Scalar(Scalar::F32 | Scalar::F64))
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

/// LIR `ConvertMode`, unchanged; see its documentation for each mode's exact result.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ConvertMode {
    Widen,
    Truncate,
    /// `flag` (bool) is true exactly when the operand is representable.
    TryNarrow,
    /// `float.to_<int>` (SEM-2) to any integer type: the value truncated toward
    /// zero, and `flag` (u8): 0 in range, 1 NaN, 2 out of range (infinities
    /// included). The value is 0 unless the flag is 0.
    FloatToInt,
    IntToFloat,
    Reinterpret,
    FloatResize,
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

/// LIR `ScalarUnaryOp`, unchanged.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ScalarUnaryOp {
    Sqrt,
    Floor,
    Ceil,
    Trunc,
    Round,
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
            Self::Sqrt | Self::Floor | Self::Ceil | Self::Trunc | Self::Round | Self::Abs
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
        kind: AllocKind,
        place: AllocPlace,
        charge: ValueId,
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
    /// Copy `bytes` bytes from the place `from` to the place `to` (they do not
    /// overlap). A move of a non-plain value is this copy; the source is then moved
    /// out and its drop flag cleared by the producer. Growth copies the old storage.
    Copy {
        to: AddressId,
        from: AddressId,
        bytes: ValueId,
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
    /// finalizer under its grant, then fields in reverse declaration order, then
    /// the value's own block. A finalizer failure still cleans the fields and then
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
    /// `try_unwrap`: true exactly when this is the only holder; the payload is then
    /// the caller's (moved out with `Copy`) and the node is freed with `Free`, which
    /// refunds the node only. False leaves the holder unchanged.
    TryUnwrap {
        result: ValueId,
        node: ValueId,
        kind: Counted,
    },
}

/// What an `Alloc` makes. The block's bookkeeping (its limit record,
/// and for a resource its cause cell and grant) is the engine's.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AllocKind {
    Own,
    /// A counted node: the count (u64, starting at 1) at offset 0, the payload at 8.
    Counted,
    /// A resource block: `charge` covers its storage, its cause cell and `grant`.
    Resource {
        grant: u64,
    },
}

/// Which limit an `Alloc` is charged to.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AllocPlace {
    /// The current limit (MEM-19).
    Current,
    /// The limit the block's record leads to: growth keeps the limit (MEM-26).
    Like(ValueId),
    /// FORM v1b: in this region, charged to the current limit until it closes (MEM-1).
    Region(ValueId),
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
    Spawn { x: TypeId, r: TypeId },
    Join { r: TypeId },
    Cancel,
    CheckCancel,
    ChannelNew { item: TypeId },
    ChannelSender,
    Send { item: TypeId },
    TrySend { item: TypeId },
    Receive { item: TypeId },
    DestId,
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
    /// The signature row a call of this operation names. A `take` input is `&T` and
    /// stays the caller's when the operation refuses before its commit point.
    ///
    /// | op | params | results |
    /// |---|---|---|
    /// | `spawn[X, R]` | `n: u64`, `&X`, `f: ptr` | task `ptr` (null on `Capacity`), `available: u64` |
    /// | `join[R]` | task `ptr`, `&R` | status `u8`: 0 Ok (R written), 1 Failed, 2 Cancelled; `Causes` `ptr` (null when Ok) |
    /// | `cancel` | task `ptr` | |
    /// | `check_cancel` | | |
    /// | `channel_new[T]` | `cap: u64` | sender `ptr`, receiver `ptr` (null on `Capacity`), `available: u64` |
    /// | `channel_sender` | sender `ptr` | sender `ptr` |
    /// | `send[T]` | sender `ptr`, `&T` | `bool`: published (false: `Closed`, the item stays) |
    /// | `try_send[T]` | sender `ptr`, `&T` | status `u8`: 0 published, 1 `Full`, 2 `Closed` |
    /// | `receive[T]` | receiver `ptr`, `&T` | `bool`: received (false: `Closed`) |
    /// | `dest_id` | | `u64`: a destination identity no other call of the run returns (MEM-4) |
    ///
    /// `f` is a plain function `(&X) -> () dest R`. `channel_new` with `cap` 0 fails
    /// with `Bounds`; `send`, `receive`, `join` and `check_cancel` fail with
    /// `Cancelled` when they observe it (CONC-7). These five are the rows that can
    /// fail ([`RtOp::fails`]); the others never do.
    pub fn signature(self) -> Signature {
        use Param::{Address as A, Value as V};
        let (u8_, u64_, bool_) = (
            Ty::Scalar(Scalar::U8),
            Ty::Scalar(Scalar::U64),
            Ty::Scalar(Scalar::Bool),
        );
        let p = Ty::Ptr;
        let (params, results) = match self {
            RtOp::Spawn { x, .. } => (vec![V(u64_), A(x), V(p)], vec![p, u64_]),
            RtOp::Join { r } => (vec![V(p), A(r)], vec![u8_, p]),
            RtOp::Cancel => (vec![V(p)], vec![]),
            RtOp::CheckCancel => (vec![], vec![]),
            RtOp::ChannelNew { .. } => (vec![V(u64_)], vec![p, p, u64_]),
            RtOp::ChannelSender => (vec![V(p)], vec![p]),
            RtOp::Send { item } => (vec![V(p), A(item)], vec![bool_]),
            RtOp::TrySend { item } => (vec![V(p), A(item)], vec![u8_]),
            RtOp::Receive { item } => (vec![V(p), A(item)], vec![bool_]),
            RtOp::DestId => (vec![], vec![u64_]),
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
                | RtOp::CheckCancel
                | RtOp::ChannelNew { .. }
                | RtOp::Send { .. }
                | RtOp::Receive { .. }
        )
    }

    pub fn types(self) -> Vec<TypeId> {
        match self {
            RtOp::Spawn { x, r } => vec![x, r],
            RtOp::Join { r } => vec![r],
            RtOp::ChannelNew { item }
            | RtOp::Send { item }
            | RtOp::TrySend { item }
            | RtOp::Receive { item } => vec![item],
            RtOp::Cancel | RtOp::CheckCancel | RtOp::ChannelSender | RtOp::DestId => vec![],
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
            Inst::Alloc { .. } | Inst::Drop { .. } => true,
            _ => false,
        }
    }

    /// The failure block the instruction names, if any.
    pub fn fail(&self, b: &Body) -> Option<BlockId> {
        match *self {
            Inst::Unary { fail, .. }
            | Inst::Binary { fail, .. }
            | Inst::Address { fail, .. }
            | Inst::Alloc { fail, .. }
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
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Edge {
    pub target: BlockId,
    pub args: PoolRange<ValueId>,
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
    /// block's last instruction. Nothing reaches it: such a call only fails.
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
    /// A pointer to a place of the declared type, owned by the caller.
    Address(TypeId),
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

/// The identity of one instantiation (see the crate docs, "Instances").
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct InstanceKey {
    pub definition: DefPath,
    /// Canonical text of each closed type argument, in parameter order.
    pub types: Vec<String>,
    /// Canonical text of the implementation chosen for each constraint, in order.
    pub dictionaries: Vec<String>,
}
impl InstanceKey {
    /// The content identity the link name and caches use.
    pub fn digest(&self) -> ContentHash {
        let definition = self.definition.to_string();
        let counts =
            [self.types.len() as u64, self.dictionaries.len() as u64].map(u64::to_le_bytes);
        let parts = [definition.as_bytes(), &counts[0], &counts[1]]
            .into_iter()
            .chain(
                self.types
                    .iter()
                    .chain(&self.dictionaries)
                    .map(String::as_bytes),
            );
        ContentHash::of_parts("hirsel.core-instance.v0", parts)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Function {
    pub instance: InstanceId,
    pub signature: SigId,
    /// None for a function defined in another module (linked by name).
    pub body: Option<Body>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Static {
    pub align: u32,
    pub bytes: Vec<u8>,
}

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
}
