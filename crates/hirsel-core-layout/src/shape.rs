//! Declared shapes: what a type declaration says about its representation. The
//! checker publishes one `Shape` per declared type; `compute` turns the table into
//! recipes once. Shapes name other types by `TypeRef`, an index into the same table.

/// Index of a declared type in its module's type table.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct TypeRef(pub u32);
impl TypeRef {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a function in the module's function table (finalizers, glue).
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct FuncRef(pub u32);

/// A scalar as the language sees it (SEM-1, SEM-2): its logical width is its
/// written width; `bool` is 1 byte and `char` 4 (MEM-24).
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash,
)]
pub enum Scalar {
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
}
impl Scalar {
    pub const ALL: [Scalar; 12] = [
        Scalar::Bool,
        Scalar::Char,
        Scalar::I8,
        Scalar::I16,
        Scalar::I32,
        Scalar::I64,
        Scalar::U8,
        Scalar::U16,
        Scalar::U32,
        Scalar::U64,
        Scalar::F32,
        Scalar::F64,
    ];
    pub const fn size(self) -> u64 {
        match self {
            Scalar::Bool | Scalar::I8 | Scalar::U8 => 1,
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::Char | Scalar::I32 | Scalar::U32 | Scalar::F32 => 4,
            Scalar::I64 | Scalar::U64 | Scalar::F64 => 8,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Scalar::Bool => "bool",
            Scalar::Char => "char",
            Scalar::I8 => "i8",
            Scalar::I16 => "i16",
            Scalar::I32 => "i32",
            Scalar::I64 => "i64",
            Scalar::U8 => "u8",
            Scalar::U16 => "u16",
            Scalar::U32 => "u32",
            Scalar::U64 => "u64",
            Scalar::F32 => "f32",
            Scalar::F64 => "f64",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// The type of a CoreIR value: a scalar or an opaque target pointer.
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

/// How a value of a row is carried in a signature, published from the closed rule
/// that lowers every signature (crate `hirsel-core-ir`, "Values"): `Value(t)` as the
/// one CoreIR value `t` (a scalar, a plain `fn` or a `Writer` as `ptr`, a capability,
/// `Args`, `Unique` or `Site` as its `u64` word); `Nothing` for unit; `Address` for
/// every other row (passed by address, returned through `dest`). A generic instance's
/// signature reads it for each substituted row, so it never classifies a row itself.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Carried {
    Address,
    Value(Ty),
    Nothing,
}

/// The width `W` of a compact declaration (MEM-9): the blob cap is `2^W - 1`
/// bytes and every stored length, extent, offset and link uses `W` bytes.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Width {
    U16,
    U32,
    U64,
}
impl Width {
    pub const ALL: [Width; 3] = [Width::U16, Width::U32, Width::U64];
    pub const fn index(self) -> usize {
        match self {
            Width::U16 => 0,
            Width::U32 => 1,
            Width::U64 => 2,
        }
    }
    pub const fn bytes(self) -> u64 {
        match self {
            Width::U16 => 2,
            Width::U32 => 4,
            Width::U64 => 8,
        }
    }
    /// The largest blob a compact declaration of this width admits.
    pub const fn max_blob(self) -> u64 {
        match self {
            Width::U16 => u16::MAX as u64,
            Width::U32 => u32::MAX as u64,
            Width::U64 => u64::MAX,
        }
    }
}

/// A runtime-owned handle kind: an opaque pointer whose cleanup is a run-time call.
/// Logical sizes per core's built-in types: a task handle, a capability, a
/// `Causes` chain and a `HostFile` 8, a sender or receiver 24.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Handle {
    Task,
    Sender,
    Receiver,
    /// A capability (`Dir`, `Tasks`, ...).
    Capability,
    /// A delivered cause chain (`Causes`, CONC-6): cleaning it refunds its used cells.
    Causes,
    /// A host file (CAP-5): a mechanical resource with no cause cell; cleaning it
    /// closes the file through its provider and ignores the outcome (MEM-24).
    HostFile,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Repr {
    /// `type`: one owner, mutated in place, capacity-based (MEM-13).
    Own,
    /// `resource type`: own, cleaned exactly once, finalizer optional (FAIL-11).
    Resource,
    /// `stable type`: published in a region, immutable, natively laid out like own.
    Stable,
    /// `@compact(W) stable type`: one byte-aligned blob (MEM-9..12).
    Compact(Width),
    /// A scoped record (OWN-19): lives in a frame, never stored or returned.
    Scoped,
}

/// The type of one field.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FieldTy {
    Scalar(Scalar),
    /// The declared type stored by value (own/stable/scoped) or inline (compact).
    Inline(TypeRef),
    /// `Box[T]`: one own block.
    Box(TypeRef),
    /// `List[T]`, `String` (`Seq(u8)` of UTF-8), `Bytes` (`Seq(u8)`): ptr/len/cap.
    Seq(Box<FieldTy>),
    Rc(TypeRef),
    Arc(TypeRef),
    /// A reference to a published stable value of this type (logically 16, MEM-24).
    Stable(TypeRef),
    /// `Inline[T, n]` (SEM-9): `n` values of a plain or own `T` held inline, with no
    /// block. Its size is `n` times `T`'s; it charges nothing of its own; it is
    /// cleaned from index 0 up. (`Region` is never a field: it is scoped and never
    /// stored, MEM-1.)
    Array(Box<FieldTy>, u64),
    /// A plain `fn`: a code pointer, captures nothing (LANG-13).
    Fn,
    /// An `own fn`: code pointer and an owned environment block.
    OwnFn,
    /// A `scoped fn`, a `Dest` or a `Writer`: borrowed, cleaned by nobody, only in
    /// scoped types.
    Borrow,
    /// `View[T]`: T's own bytes, laid out exactly as a field of type T, cleaned by
    /// nobody, only in scoped types (crate `hirsel-core-ir`, "Views").
    View(Box<FieldTy>),
    Handle(Handle),
    /// `Map[K, V]`: a header like a `List`'s over its entry storage.
    Map(Box<FieldTy>, Box<FieldTy>),
    // Compact-only field types (MEM-9..12).
    /// `W` byte count, then that many bytes of UTF-8 (MEM-12).
    Text,
    /// `W` count, then the elements at the element's fixed stride (MEM-12).
    FixedSpan(Box<FieldTy>),
    /// Elements of a packed type, each its own size: `W` count, `W` byte length,
    /// then the elements (MEM-12).
    SeqSpan(Box<FieldTy>),
    /// As `SeqSpan`, then a trailer of `count` `W` offsets (MEM-12).
    IndexedSpan(Box<FieldTy>),
    /// `Link[U]` (MEM-10); `Option[Link[U]]` is an inline compact variant whose
    /// empty case is the zero distance (the recipe's niche tag).
    Link(TypeRef),
    /// A reference to compact data held outside a blob: the blob pointer, then the
    /// value's offset from it (crate `hirsel-core-ir`, "Compact data"). Plain,
    /// logically 16 (MEM-24). It names what it locates, as a compact field type:
    /// `Text`, `FixedSpan(e)`, `SeqSpan(e)`, `IndexedSpan(e)`, or `Inline(c)` of a
    /// compact `c`; that target is the recipe hash, equality and order read.
    Locator(Box<FieldTy>),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Field {
    pub name: String,
    pub ty: FieldTy,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Case {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Body {
    Record(Vec<Field>),
    /// Cases in declaration order; the tag of case `i` is `i`.
    Variant(Vec<Case>),
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Shape {
    pub name: String,
    pub repr: Repr,
    /// How a signature carries a value of this row (see [`Carried`]).
    pub carried: Carried,
    pub body: Body,
    /// A resource's finalizer (FAIL-11), or a core slot kernel's on an own row
    /// (FORM v1c), called with the value's address before its fields are cleaned.
    pub finalizer: Option<FuncRef>,
    /// A core own row's declared clone body (MEM-17, `clone[..](x: T) { .. }`), the
    /// finalizer's counterpart: `(ptr from, ptr to) -> (u64 requested, u64 available)`.
    /// The derived clone of any value holding this row calls it, after the byte copy,
    /// instead of walking the row's fields; it writes the copy to `to` and returns
    /// `(0, 0)`, or leaves `to` as it was and returns the refused request.
    pub clone: Option<FuncRef>,
    /// `grant bytes n` (MEM-27).
    pub grant: u64,
}

impl Shape {
    /// The fields of case `case`, or of the record when `case` is `None`.
    pub fn fields(&self, case: Option<u32>) -> Option<&[Field]> {
        match (&self.body, case) {
            (Body::Record(fields), None) => Some(fields),
            (Body::Variant(cases), Some(c)) => cases.get(c as usize).map(|c| &c.fields[..]),
            _ => None,
        }
    }
}
