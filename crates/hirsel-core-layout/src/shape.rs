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

/// A SEM-11 integer vector. Each variant occupies exactly 16 or 32 bytes in a
/// frame. Keeping both properties in one byte also keeps scalar CoreIR rows small.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub enum Vector {
    I8x16,
    I16x8,
    I32x4,
    I64x2,
    U8x16,
    U16x8,
    U32x4,
    U64x2,
    I8x32,
    I16x16,
    I32x8,
    I64x4,
    U8x32,
    U16x16,
    U32x8,
    U64x4,
}
impl Vector {
    pub fn new(lane: Scalar, lanes: u64) -> Option<Self> {
        use Scalar::*;
        Some(match (lane, lanes) {
            (I8, 16) => Self::I8x16,
            (I16, 8) => Self::I16x8,
            (I32, 4) => Self::I32x4,
            (I64, 2) => Self::I64x2,
            (U8, 16) => Self::U8x16,
            (U16, 8) => Self::U16x8,
            (U32, 4) => Self::U32x4,
            (U64, 2) => Self::U64x2,
            (I8, 32) => Self::I8x32,
            (I16, 16) => Self::I16x16,
            (I32, 8) => Self::I32x8,
            (I64, 4) => Self::I64x4,
            (U8, 32) => Self::U8x32,
            (U16, 16) => Self::U16x16,
            (U32, 8) => Self::U32x8,
            (U64, 4) => Self::U64x4,
            _ => return None,
        })
    }

    pub const fn lane(self) -> Scalar {
        match self {
            Self::I8x16 | Self::I8x32 => Scalar::I8,
            Self::I16x8 | Self::I16x16 => Scalar::I16,
            Self::I32x4 | Self::I32x8 => Scalar::I32,
            Self::I64x2 | Self::I64x4 => Scalar::I64,
            Self::U8x16 | Self::U8x32 => Scalar::U8,
            Self::U16x8 | Self::U16x16 => Scalar::U16,
            Self::U32x4 | Self::U32x8 => Scalar::U32,
            Self::U64x2 | Self::U64x4 => Scalar::U64,
        }
    }

    pub const fn size(self) -> u64 {
        match self {
            Self::I8x16
            | Self::I16x8
            | Self::I32x4
            | Self::I64x2
            | Self::U8x16
            | Self::U16x8
            | Self::U32x4
            | Self::U64x2 => 16,
            _ => 32,
        }
    }

    pub const fn lanes(self) -> u64 {
        self.size() / self.lane().size()
    }
}

/// The type of a CoreIR value.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Ty {
    Scalar(Scalar),
    Ptr,
    Vector(Vector),
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
            Ty::Ptr | Ty::Vector(_) => None,
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
    Latest,
    /// A connection's client (CONC-17): cross, cloned by `conn.client`.
    Client,
    /// A connection's inbox (CONC-17): dropping it seals the connection.
    Inbox,
    /// A capability (`Dir`, `Tasks`, ...).
    Capability,
    /// A delivered cause chain (`Causes`, CONC-6): cleaning it refunds its used cells.
    Causes,
    /// A host file (CAP-5): a mechanical resource with no cause cell; cleaning it
    /// closes the file through its provider and ignores the outcome (MEM-24).
    HostFile,
    /// Socket, directional half, listener, datagram endpoint or child process.
    /// The key retains its provider object kind.
    Socket,
    /// Owner-only graph, mechanically discarding buffers on cleanup.
    Flow,
    /// One holder of a `Secret` designation (TLS-5): cleaning it releases the holder.
    Secret,
    /// A move-only media resource (TLS-8).
    Media,
    /// A counted handle to one typed entry of a code image (`Code[F]`, PACK-14).
    Code,
    /// A versioned activation slot (`CodeSlot[F]`, PACK-14).
    CodeSlot,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Repr {
    /// `type`: one owner, mutated in place, capacity-based (MEM-13).
    Own,
    /// `resource type`: own, cleaned exactly once, finalizer optional (FAIL-11).
    Resource,
    /// Fixed-layout region storage for a sequence index.
    Region,
    /// `@compact(W) type`: one byte-aligned blob (MEM-9..12).
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
    /// `Columns[T]`: a `Seq` carrier (ptr/len/cap) whose one block holds a column
    /// per field of the row `T` ([`crate::columns`]), each at its own stride. It is
    /// charged and cleaned row by row as `Seq(T)` is (MEM-24, SEM-6).
    Columns(Box<FieldTy>),
    Rc(TypeRef),
    Arc(TypeRef),
    /// A reference to a published region value of this type (logically 16, MEM-24).
    RegionRef(TypeRef),
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
    /// Read-only Bytes storage. The third word is capacity, with the usual
    /// sequence niches. The second word is a doubled length or an odd payer token.
    FrozenBytes,
    /// `Shared[T]` (CONC-15): a pointer to a node read by many units. The
    /// runtime shares and releases it with an atomic count; its payload sits at
    /// the node's word 1, as an `Arc`'s does.
    Shared(TypeRef),
}

impl FieldTy {
    /// Whether this field names a row.
    pub fn names_row(&self) -> bool {
        match self {
            FieldTy::Inline(_)
            | FieldTy::Box(_)
            | FieldTy::Rc(_)
            | FieldTy::Arc(_)
            | FieldTy::Shared(_)
            | FieldTy::RegionRef(_)
            | FieldTy::Link(_) => true,
            FieldTy::Seq(ty)
            | FieldTy::Columns(ty)
            | FieldTy::Array(ty, _)
            | FieldTy::View(ty)
            | FieldTy::Locator(ty)
            | FieldTy::FixedSpan(ty)
            | FieldTy::SeqSpan(ty)
            | FieldTy::IndexedSpan(ty) => ty.names_row(),
            FieldTy::Map(k, v) => k.names_row() || v.names_row(),
            FieldTy::Scalar(_)
            | FieldTy::Fn
            | FieldTy::OwnFn
            | FieldTy::Borrow
            | FieldTy::Handle(_)
            | FieldTy::Text
            | FieldTy::FrozenBytes => false,
        }
    }
    /// Rewrite every row this field names, in field order.
    pub fn map_refs(&mut self, f: &mut impl FnMut(TypeRef) -> TypeRef) {
        match self {
            FieldTy::Inline(id)
            | FieldTy::Box(id)
            | FieldTy::Rc(id)
            | FieldTy::Arc(id)
            | FieldTy::Shared(id)
            | FieldTy::RegionRef(id)
            | FieldTy::Link(id) => *id = f(*id),
            FieldTy::Seq(ty)
            | FieldTy::Columns(ty)
            | FieldTy::Array(ty, _)
            | FieldTy::View(ty)
            | FieldTy::Locator(ty)
            | FieldTy::FixedSpan(ty)
            | FieldTy::SeqSpan(ty)
            | FieldTy::IndexedSpan(ty) => ty.map_refs(f),
            FieldTy::Map(k, v) => {
                k.map_refs(f);
                v.map_refs(f);
            }
            FieldTy::Scalar(_)
            | FieldTy::Fn
            | FieldTy::OwnFn
            | FieldTy::Borrow
            | FieldTy::Handle(_)
            | FieldTy::Text
            | FieldTy::FrozenBytes => {}
        }
    }
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
    /// A codec type's maker (SEM-10), `() -> Codec[T]` at this row: the function
    /// `std.codec.of` calls at it. Unlike the finalizer it is no dependency of a unit
    /// that only holds the row; only a call reaches it.
    pub codec: Option<FuncRef>,
    /// The ordered key maker that `std.codec.key_of` selects for this row.
    pub key_codec: Option<FuncRef>,
    /// `grant bytes n` (MEM-27).
    pub grant: u64,
    /// The row has the `send` bound (CONC-2): a value of it may arrive from
    /// another task, so the blocks it owns may be charged to that task's limit
    /// and their refunds cross tasks (MEM-20).
    pub send: bool,
    /// How a holder writes a field of this row's type when that is not
    /// `Inline(this row)` (`Scalar` for a scalar, `Seq` for a list, `Box` for a box,
    /// `RegionRef` for a region value, ..): the one encoding a substituted generic row
    /// writes at its arguments, so one name is one closure (FORM #109). A compact
    /// context reads it without its `Locator`, and a `RegionRef` as `Inline`.
    pub held: Option<FieldTy>,
}

/// A row of a type table. Tables share rows: a row closure copied into a body's
/// module (and every later copy of it) holds the closure's own rows wherever
/// renumbering leaves them as they are, and a write copies a shared row first
/// (`Arc::make_mut`), so it never reaches another table.
pub type Row = std::sync::Arc<Shape>;

impl Shape {
    /// Whether the row names another row or a function: renumbering a table leaves a
    /// row that names neither as it is.
    pub fn numbered(&self) -> bool {
        self.finalizer.is_some()
            || self.clone.is_some()
            || self.codec.is_some()
            || self.key_codec.is_some()
            || self.held.as_ref().is_some_and(FieldTy::names_row)
            || match &self.body {
                Body::Record(fields) => fields.iter().any(|x| x.ty.names_row()),
                Body::Variant(cases) => cases
                    .iter()
                    .any(|c| c.fields.iter().any(|x| x.ty.names_row())),
            }
    }
    /// Rewrite every row the body names, in field order, then the rows `held` names.
    pub fn map_refs(&mut self, f: &mut impl FnMut(TypeRef) -> TypeRef) {
        match &mut self.body {
            Body::Record(fields) => fields.iter_mut().for_each(|x| x.ty.map_refs(f)),
            Body::Variant(cases) => cases
                .iter_mut()
                .flat_map(|c| &mut c.fields)
                .for_each(|x| x.ty.map_refs(f)),
        }
        if let Some(held) = &mut self.held {
            held.map_refs(f);
        }
    }
    /// The fields of case `case`, or of the record when `case` is `None`.
    pub fn fields(&self, case: Option<u32>) -> Option<&[Field]> {
        match (&self.body, case) {
            (Body::Record(fields), None) => Some(fields),
            (Body::Variant(cases), Some(c)) => cases.get(c as usize).map(|c| &c.fields[..]),
            _ => None,
        }
    }
}
