//! Layout recipes: computed once per declared shape (ARCH "Runtime and layout").
//!
//! Own, resource, stable and scoped types get a native fixed layout: declaration
//! order, natural alignment, a variant tag of the smallest of u8/u16/u32 at offset 0
//! and a payload union after it (MEM-13 leaves this private; both engines use this
//! one recipe so they agree). Compact types get the byte-aligned blob layout of
//! MEM-9..12: positions are either constant or "after variable field j, plus k".

use crate::shape::{
    Body, Carried, Field, FieldTy, FuncRef, Handle, Repr, Scalar, Shape, Ty, TypeRef, Width,
};

/// Native size and alignment of a field slot of this type in a fixed layout.
pub const POINTER: u64 = 8;
/// `List`, `String`, `Bytes` carrier: data pointer, length, capacity (u64 each).
pub const SEQ_CARRIER: u64 = 24;
/// `own fn` and `scoped fn` carrier: code pointer, environment pointer.
pub const CLOSURE_CARRIER: u64 = 16;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Recipe {
    pub layout: Layout,
    /// charge-v1 logical size (MEM-24): no padding; an own variant is 8 plus its
    /// largest payload. For a compact type, its constant extent at its declared
    /// width when it has one, else `None`.
    pub logical_size: Option<u64>,
    /// No cleanup is needed and the value may be copied bitwise (T2.2).
    pub plain: bool,
    /// Present exactly when cleaning a value of the type does anything.
    pub drop: Option<DropRecipe>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Layout {
    Fixed(Fixed),
    Compact(CompactSet),
}

/// A compact type's encodings. A compact value held inline or as a link target is
/// encoded with its holder's width (MEM-9), so a type has one encoding per width.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompactSet {
    pub declared: Width,
    /// Indexed by `Width::index`.
    pub at: [Compact; 3],
}

impl CompactSet {
    pub fn at(&self, w: Width) -> &Compact {
        &self.at[w.index()]
    }
    pub fn declared(&self) -> &Compact {
        self.at(self.declared)
    }
}

/// Where a variant keeps which case it is, and how the case is encoded there. Every
/// consumer reads a case through [`read_case`] and writes one through [`case_word`];
/// nothing else interprets a tag word.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    pub offset: u64,
    /// The word's width in bytes: 1, 2 or 4 for a direct tag (the smallest that
    /// holds the case count); for a niche, the width of the niche it uses (8 for a
    /// pointer or a capacity).
    pub width: u64,
    pub encoding: Encoding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Encoding {
    /// The word is the case ordinal.
    Direct,
    /// The variant is laid out as case `dataful`'s payload, and every other case is
    /// in `first..=last` and has no data. Case `c != dataful` is the word
    /// `start + (c - first)` (mod 2^(8 × width)), a value no payload of `dataful`
    /// holds; any other word is `dataful`, whose case is written by its payload alone.
    Niche {
        dataful: u32,
        first: u32,
        last: u32,
        start: u64,
    },
}

/// Bit patterns no valid value of a type holds: `count` values from `start` (mod
/// 2^(8 × width)) in the `width`-byte little-endian word at `offset`. Each field type
/// states its own once ([`field_niche`]); a variant with one dataful case spends
/// some on its other cases (MEM-13 keeps this private).
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Niche {
    pub offset: u64,
    pub width: u64,
    pub start: u64,
    pub count: u64,
}

/// The arithmetic a consumer reads a case with: the evaluator on words, an engine on
/// IR values. Values are u64; `below` and `select` treat 0 as false.
pub trait CaseOps {
    type V: Copy;
    fn constant(&mut self, n: u64) -> Self::V;
    /// Wrapping.
    fn add(&mut self, a: Self::V, b: Self::V) -> Self::V;
    /// Wrapping.
    fn sub(&mut self, a: Self::V, b: Self::V) -> Self::V;
    fn and(&mut self, a: Self::V, b: Self::V) -> Self::V;
    /// Unsigned `a < b`, as 1 or 0.
    fn below(&mut self, a: Self::V, b: Self::V) -> Self::V;
    fn select(&mut self, c: Self::V, yes: Self::V, no: Self::V) -> Self::V;
}

/// The case ordinal of a variant whose tag word (`tag.width` bytes at `tag.offset`,
/// zero-extended) is `word`.
pub fn read_case<O: CaseOps>(tag: &Tag, o: &mut O, word: O::V) -> O::V {
    match tag.encoding {
        Encoding::Direct => word,
        Encoding::Niche {
            dataful,
            first,
            last,
            start,
        } => {
            let s = o.constant(start);
            let mut rel = o.sub(word, s);
            if tag.width < 8 {
                let mask = o.constant((1u64 << (8 * tag.width)) - 1);
                rel = o.and(rel, mask);
            }
            let k = o.constant(u64::from(last - first) + 1);
            let empty = o.below(rel, k);
            let f = o.constant(u64::from(first));
            let case = o.add(rel, f);
            let d = o.constant(u64::from(dataful));
            o.select(empty, case, d)
        }
    }
}

/// The tag word that makes a variant case `case`, or `None` when the case is written
/// by its payload alone (the dataful case of a niche).
pub fn case_word(tag: &Tag, case: u32) -> Option<u64> {
    match tag.encoding {
        Encoding::Direct => Some(u64::from(case)),
        Encoding::Niche { dataful, .. } if case == dataful => None,
        Encoding::Niche { first, start, .. } => {
            let word = start.wrapping_add(u64::from(case - first));
            Some(if tag.width < 8 {
                word & ((1u64 << (8 * tag.width)) - 1)
            } else {
                word
            })
        }
    }
}

/// [`read_case`] on a word in hand.
pub fn case_of(tag: &Tag, word: u64) -> u32 {
    struct Words;
    impl CaseOps for Words {
        type V = u64;
        fn constant(&mut self, n: u64) -> u64 {
            n
        }
        fn add(&mut self, a: u64, b: u64) -> u64 {
            a.wrapping_add(b)
        }
        fn sub(&mut self, a: u64, b: u64) -> u64 {
            a.wrapping_sub(b)
        }
        fn and(&mut self, a: u64, b: u64) -> u64 {
            a & b
        }
        fn below(&mut self, a: u64, b: u64) -> u64 {
            u64::from(a < b)
        }
        fn select(&mut self, c: u64, yes: u64, no: u64) -> u64 {
            if c != 0 { yes } else { no }
        }
    }
    read_case(tag, &mut Words, word) as u32
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fixed {
    pub size: u64,
    pub align: u64,
    pub tag: Option<Tag>,
    /// Field byte offsets from the value's start: one row for a record, one per case.
    pub offsets: Vec<Vec<u64>>,
    /// The spare values of the type, for a variant that holds it (the one with the
    /// most, ties to the lowest offset).
    pub niche: Option<Niche>,
}

/// Where a compact field starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Pos {
    /// A constant byte offset from the value's start.
    At(u64),
    /// `plus` bytes after the end of variable field `field` of the same case.
    After { field: u32, plus: u64 },
}

/// How many bytes a compact field occupies.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Size {
    Fixed(u64),
    /// `W` count, then `count * stride` bytes: a `FixedSpan`, or a `Text` (stride 1).
    FixedSpan {
        stride: u64,
    },
    /// `W` count, `W` byte length of the elements, then the elements (MEM-12).
    SeqSpan,
    /// As `SeqSpan`, then a trailer of `count` `W` offsets, each from the first
    /// element byte to its element. The stored byte length excludes the trailer;
    /// the holder's extent includes it.
    IndexedSpan,
    /// An inline compact value whose extent is walked by its recipe at the holder's
    /// width.
    Inline(TypeRef),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CField {
    pub pos: Pos,
    pub size: Size,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Compact {
    pub width: Width,
    pub tag: Option<Tag>,
    /// One row for a record, one per case (each case's first field follows the tag).
    pub fields: Vec<Vec<CField>>,
    /// The extent when every field of every case is fixed and all cases are equal.
    pub fixed_extent: Option<u64>,
}

/// One owned field a value's cleanup visits, in cleanup order.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FieldDrop {
    pub field: u32,
    pub offset: u64,
    pub ty: FieldTy,
}

/// FAIL-8: the finalizer runs first, then fields in reverse declaration order,
/// then the block is freed and refunded. A `List` (`Seq`) and an inline array
/// (`FieldTy::Array`, at the stride `field_size` gives) clean their completed
/// elements from index 0 up (SEM-9, core.md); a `Map` cleans entry by entry in
/// insertion order, each key before its value ([`map_entry`]). Those loops are in
/// the glue built from this recipe. A `Seq` or `Map` with storage then frees it,
/// refunding round8(capacity × element logical size), resp. round8(capacity ×
/// (key + value + 16)) (MEM-24), to its own record; capacity 0 has no storage,
/// whatever its data pointer (a string literal points at static data), so engines
/// test the capacity, never the pointer.
///
/// **Counted cleanup** visits the same order, one step at a time, so a release
/// yields one descendant at a time and resumes by a cursor of steps taken. Steps,
/// counted over the visit order: a `Box`, `Rc` or `Arc` field is one step (it
/// yields its node); a record or `Inline` is its fields' steps in order; a variant
/// is its active case's; an inline array of n is n times its element's. A `Seq`
/// or `Map` whose element (key and value) takes c steps is `slots × c + 1` steps,
/// slot i's steps at `i × c` within it and the last step freeing the storage;
/// `slots` is its length, read from the carrier, which cleanup does not change
/// until that last step; a `Seq` of plain elements, `String` included, is so one
/// step. A value whose recipe has a finalizer (a core slot kernel's own row:
/// a counted payload holds no resource) and any other field that needs cleanup
/// (a `Causes`, an `own fn`, a handle) is cleaned whole as one step. So a cursor names the same point whichever
/// engine built the glue.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DropRecipe {
    pub finalizer: Option<FuncRef>,
    /// One list for a record, one per case.
    pub fields: Vec<Vec<FieldDrop>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutError {
    /// A type contains itself by value (MEM-13).
    Cycle { ty: String },
    /// A field type the declaration's representation does not allow (MEM-9, MEM-17).
    Field {
        ty: String,
        field: String,
        why: &'static str,
    },
    /// A constant extent exceeds the compact width's blob cap (MEM-9).
    TooLarge { ty: String },
    /// A compact type that can encode to zero bytes (MEM-9).
    Empty { ty: String },
    /// A row whose published [`Carried`] contradicts its body.
    Carried { ty: String },
    /// The native layout needs `bytes` at `field`, past the engine's 64-bit size range
    /// (`u64::MAX` bytes): a resource refusal of the implementation (PROG-6), not a rule.
    NativeRange {
        ty: String,
        field: String,
        bytes: u128,
    },
}

/// The smallest tag of u8/u16/u32 that holds `cases` ordinals.
pub fn tag_width(cases: usize) -> u64 {
    if cases <= 1 << 8 {
        1
    } else if cases <= 1 << 16 {
        2
    } else {
        4
    }
}

fn checked_align_up(n: u64, a: u64) -> Option<u64> {
    n.div_ceil(a).checked_mul(a)
}

/// `n` aligned up to `a`, where the result may pass `u64::MAX`.
fn wide_align_up(n: u128, a: u64) -> u128 {
    n.div_ceil(a.into()) * u128::from(a)
}

fn align_up(n: u64, a: u64) -> u64 {
    n.div_ceil(a) * a
}

/// Recipes for every shape of a module, indexed like `shapes`.
pub fn compute(shapes: &[Shape]) -> Result<Vec<Recipe>, LayoutError> {
    let mut recipes = vec![];
    extend(shapes, &mut recipes)?;
    Ok(recipes)
}

/// Compute only the appended shapes. `recipes` describes an unchanged prefix of
/// `shapes`; neither that prefix nor its references may change between extensions.
/// A refused suffix leaves the existing recipes intact.
pub fn extend(shapes: &[Shape], recipes: &mut Vec<Recipe>) -> Result<(), LayoutError> {
    let prefix = recipes.len();
    let mut state = vec![State::Todo; shapes.len()];
    state[..prefix].fill(State::Done);
    let mut order = Vec::with_capacity(shapes.len() - prefix);
    for i in prefix..shapes.len() {
        visit(shapes, TypeRef(i as u32), &mut state, &mut order)?;
    }
    let mut out = Computed {
        prefix: recipes,
        suffix: vec![None; shapes.len() - prefix],
    };
    for t in order {
        out.suffix[t.index() - prefix] = Some(recipe(shapes, &shapes[t.index()], &out)?);
    }
    let suffix = out.suffix;
    recipes.extend(suffix.into_iter().map(|r| r.expect("visited")));
    Ok(())
}

struct Computed<'a> {
    prefix: &'a [Recipe],
    suffix: Vec<Option<Recipe>>,
}
impl Computed<'_> {
    fn get(&self, t: TypeRef) -> Option<&Recipe> {
        if t.index() < self.prefix.len() {
            Some(&self.prefix[t.index()])
        } else {
            self.suffix[t.index() - self.prefix.len()].as_ref()
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Todo,
    Visiting,
    Done,
}

/// Depth is bounded by the number of declared types (each is visited once); an
/// explicit stack keeps it off the native stack.
fn visit(
    shapes: &[Shape],
    root: TypeRef,
    state: &mut [State],
    order: &mut Vec<TypeRef>,
) -> Result<(), LayoutError> {
    let mut stack = vec![root];
    while let Some(&t) = stack.last() {
        match state[t.index()] {
            State::Done => {
                stack.pop();
            }
            State::Visiting | State::Todo => {
                state[t.index()] = State::Visiting;
                let pending =
                    inline_deps(&shapes[t.index()]).find(|d| state[d.index()] != State::Done);
                if let Some(d) = pending {
                    if state[d.index()] == State::Visiting {
                        return Err(LayoutError::Cycle {
                            ty: shapes[t.index()].name.clone(),
                        });
                    }
                    stack.push(d);
                    continue;
                }
                order.push(t);
                state[t.index()] = State::Done;
                stack.pop();
            }
        }
    }
    Ok(())
}

fn inline_deps(shape: &Shape) -> impl Iterator<Item = TypeRef> + '_ {
    let (fields, cases): (&[Field], &[_]) = match &shape.body {
        Body::Record(f) => (f, &[]),
        Body::Variant(c) => (&[], c),
    };
    fields
        .iter()
        .chain(cases.iter().flat_map(|c| c.fields.iter()))
        .filter_map(|f| inline_target(&f.ty))
}

fn inline_target(ty: &FieldTy) -> Option<TypeRef> {
    match ty {
        FieldTy::Inline(t) => Some(*t),
        FieldTy::Array(e, _) => inline_target(e),
        FieldTy::FixedSpan(e) | FieldTy::View(e) => inline_target(e),
        _ => None,
    }
}

fn cases(shape: &Shape) -> Vec<&[Field]> {
    match &shape.body {
        Body::Record(f) => vec![&f[..]],
        Body::Variant(c) => c.iter().map(|c| &c.fields[..]).collect(),
    }
}

fn done<'a>(out: &'a Computed<'_>, t: TypeRef) -> &'a Recipe {
    out.get(t).expect("inline dependency computed first")
}

fn recipe(shapes: &[Shape], shape: &Shape, out: &Computed<'_>) -> Result<Recipe, LayoutError> {
    let refused = || LayoutError::Carried {
        ty: shape.name.clone(),
    };
    if !carries(shapes, shape) {
        return Err(refused());
    }
    let recipe = match shape.repr {
        Repr::Compact(w) => compact(shapes, shape, w, out),
        _ => fixed(shapes, shape, out),
    }?;
    // A variant carried as a word is laid out as exactly that word: its tag.
    if let (Body::Variant(_), Carried::Value(t)) = (&shape.body, shape.carried) {
        let word = |f: &Fixed| {
            f.tag
                .is_some_and(|tag| tag.offset == 0 && tag.width == f.size)
                && matches!(t, Ty::Scalar(s) if s.size() == f.size)
        };
        if !matches!(&recipe.layout, Layout::Fixed(f) if word(f)) {
            return Err(refused());
        }
    }
    Ok(recipe)
}

/// A row's published [`Carried`] agrees with its body: a value record is exactly one
/// field holding that value (a `ptr` is a plain `fn` or a borrow), a row carried as
/// nothing has no fields, a value variant is the word [`word`] gives, and an address
/// row may be anything.
fn carries(shapes: &[Shape], shape: &Shape) -> bool {
    let fields = match &shape.body {
        Body::Record(fields) => fields.as_slice(),
        Body::Variant(_) => {
            return match shape.carried {
                Carried::Address => true,
                Carried::Value(Ty::Scalar(s)) => word(shapes, shape) == Some(s),
                _ => false,
            };
        }
    };
    match (shape.carried, fields) {
        (Carried::Address, _) => true,
        (Carried::Nothing, []) => true,
        (Carried::Value(Ty::Scalar(s)), [f]) => f.ty == FieldTy::Scalar(s),
        (Carried::Value(Ty::Ptr), [f]) => matches!(f.ty, FieldTy::Fn | FieldTy::Borrow),
        _ => false,
    }
}

/// The one scalar word a variant row's bytes are, when they are exactly one (FORM
/// #78): such a row is carried as that word. Own and stable variants only.
///
/// - Every case fieldless: the direct tag, `u8`, `u16` or `u32` by case count.
/// - One case holding one field and every other case fieldless, where that field is a
///   `bool` (the word `u8`), a `char` (`u32`), a one-field record of one, or an inline
///   row that is itself such a variant, with spare values left for the other cases: the niche encoding stores
///   the tag in that word (`Option[char]`'s `None` is 0x110000).
///
/// Everything else is None, including a plain `fn` payload (no niche) and a record.
pub fn word(shapes: &[Shape], shape: &Shape) -> Option<Scalar> {
    spare_word(shapes, shape).map(|(s, _)| s)
}

/// [`word`] and the spare values its word still has.
fn spare_word(shapes: &[Shape], shape: &Shape) -> Option<(Scalar, u64)> {
    use Scalar::{U8, U16, U32};
    if !matches!(shape.repr, Repr::Own | Repr::Stable) {
        return None;
    }
    let Body::Variant(cases) = &shape.body else {
        return None;
    };
    let n = cases.len() as u64;
    let mut dataful = cases.iter().filter(|c| !c.fields.is_empty());
    match (dataful.next(), dataful.next()) {
        (None, _) if n > 0 => {
            let width = tag_width(cases.len());
            let s = match width {
                1 => U8,
                2 => U16,
                _ => U32,
            };
            Some((s, (1u64 << (8 * width)) - n))
        }
        (Some(case), None) if n >= 2 => {
            let [field] = case.fields.as_slice() else {
                return None;
            };
            let (s, spare) = field_word(shapes, &field.ty)?;
            Some((s, spare.checked_sub(n - 1)?))
        }
        _ => None,
    }
}

/// The word a variant's one payload field gives, and its spare values: a `bool` or a
/// `char`, a one-field record of one (the scalar row `char` is `{ 0: char }`), or a
/// word variant.
fn field_word(shapes: &[Shape], ty: &FieldTy) -> Option<(Scalar, u64)> {
    use Scalar::{Bool, Char, U8, U32};
    match ty {
        FieldTy::Scalar(Bool) => Some((U8, 254)),
        FieldTy::Scalar(Char) => Some((U32, (1u64 << 32) - 0x11_0000)),
        FieldTy::Inline(t) => {
            let shape = &shapes[t.index()];
            match &shape.body {
                Body::Record(fields) if matches!(shape.repr, Repr::Own | Repr::Stable) => {
                    match fields.as_slice() {
                        [f] => field_word(shapes, &f.ty),
                        _ => None,
                    }
                }
                Body::Record(_) => None,
                Body::Variant(_) => spare_word(shapes, shape),
            }
        }
        _ => None,
    }
}

fn err(shape: &Shape, f: &Field, why: &'static str) -> LayoutError {
    LayoutError::Field {
        ty: shape.name.clone(),
        field: f.name.clone(),
        why,
    }
}

/// Field offsets of `row` laid out from `start`, and where the row ends.
fn place_row(
    shape: &Shape,
    row: &[Field],
    m: &[(u64, u64, u64)],
    start: u64,
) -> Result<(Vec<u64>, u64), LayoutError> {
    let mut off = start;
    let mut offsets = Vec::with_capacity(m.len());
    for (&(size, a, _), f) in m.iter().zip(row.iter()) {
        let range = |bytes| LayoutError::NativeRange {
            ty: shape.name.clone(),
            field: f.name.clone(),
            bytes,
        };
        off = checked_align_up(off, a).ok_or_else(|| range(wide_align_up(off.into(), a)))?;
        offsets.push(off);
        off = off
            .checked_add(size)
            .ok_or_else(|| range(u128::from(off) + u128::from(size)))?;
    }
    Ok((offsets, off))
}

/// The niche with the most spare values among `fields` at `offsets` (ties to the
/// lowest offset).
fn best_niche<'r>(
    shapes: &[Shape],
    fields: &[Field],
    offsets: &[u64],
    get: &dyn Fn(TypeRef) -> &'r Recipe,
) -> Option<Niche> {
    let mut best: Option<Niche> = None;
    for (f, &at) in fields.iter().zip(offsets) {
        if let Some(n) = field_niche(shapes, &f.ty, get) {
            let n = Niche {
                offset: at + n.offset,
                ..n
            };
            let better = match best {
                None => true,
                Some(b) => n.count > b.count || (n.count == b.count && n.offset < b.offset),
            };
            if better {
                best = Some(n);
            }
        }
    }
    best
}

/// The spare values of a fixed-layout field of type `ty`, from the field's start: the
/// one table of them (MEM-13 keeps it private).
///
/// - `Box`, `Rc`, `Arc` and a stable reference: a null pointer.
/// - `List`, `String`, `Bytes` and `Map`: a capacity of 2^63 to 2^64 - 2. Storage of
///   2^63 elements never fits the address space, and a zero-sized element's list
///   holds 2^64 - 1 (SEM-4), so no carrier holds one. (The data pointer is no niche:
///   an empty string's may be null.)
/// - `bool`: 2 to 255. `char`: 0x110000 to 2^32 - 1.
/// - An inline value: its type's ([`Fixed::niche`]); an inline array: its first
///   element's.
///
/// Nothing else has one: integers and floats use every pattern, and a handle, a
/// code pointer or a resource pointer is not promised to be non-null here.
pub fn field_niche<'r>(
    shapes: &[Shape],
    ty: &FieldTy,
    get: &dyn Fn(TypeRef) -> &'r Recipe,
) -> Option<Niche> {
    let n = |offset, width, start, count| {
        Some(Niche {
            offset,
            width,
            start,
            count,
        })
    };
    match ty {
        FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) => n(0, POINTER, 0, 1),
        FieldTy::Stable(t) if !matches!(shapes[t.index()].repr, Repr::Compact(_)) => {
            n(0, POINTER, 0, 1)
        }
        FieldTy::Seq(_) | FieldTy::Map(..) => n(16, 8, 1 << 63, (1 << 63) - 1),
        FieldTy::Scalar(crate::shape::Scalar::Bool) => n(0, 1, 2, 254),
        FieldTy::Scalar(crate::shape::Scalar::Char) => n(0, 4, 0x11_0000, (1 << 32) - 0x11_0000),
        FieldTy::Inline(t) => match (shapes[t.index()].repr, &get(*t).layout) {
            (Repr::Resource | Repr::Compact(_), _) => None,
            (_, Layout::Fixed(l)) => l.niche,
            (_, Layout::Compact(_)) => None,
        },
        FieldTy::Array(e, count) if *count > 0 => field_niche(shapes, e, get),
        _ => None,
    }
}

/// A variant laid out as its one dataful case's payload: that case's index, offsets
/// from 0, end, and the tag and the variant's own remaining niche. `None` when the
/// variant does not have exactly one case with data, every other case's fields
/// zero-sized and without cleanup, or when that case's payload has too few spare
/// values for the other cases.
#[allow(clippy::type_complexity)]
fn niche_layout(
    shapes: &[Shape],
    rows: &[&[Field]],
    measured: &[Vec<(u64, u64, u64)>],
    shape: &Shape,
    out: &Computed<'_>,
) -> Result<Option<(Vec<Vec<u64>>, u64, Tag, Option<Niche>)>, LayoutError> {
    let get = |t: TypeRef| done(out, t);
    let dataful: Vec<usize> = (0..rows.len())
        .filter(|&r| measured[r].iter().any(|&(size, _, _)| size > 0))
        .collect();
    let &[d] = dataful.as_slice() else {
        return Ok(None);
    };
    if rows.len() < 2
        || rows
            .iter()
            .enumerate()
            .any(|(r, row)| r != d && row.iter().any(|f| needs_drop(shapes, &f.ty, out)))
    {
        return Ok(None);
    }
    let (row, end) = place_row(shape, rows[d], &measured[d], 0)?;
    let Some(niche) = best_niche(shapes, rows[d], &row, &get) else {
        return Ok(None);
    };
    let first = if d == 0 { 1 } else { 0 };
    let last = if d == rows.len() - 1 {
        rows.len() - 2
    } else {
        rows.len() - 1
    };
    let used = (last - first + 1) as u64;
    if niche.count < used {
        return Ok(None);
    }
    let offsets = (0..rows.len())
        .map(|r| {
            if r == d {
                row.clone()
            } else {
                vec![0; rows[r].len()]
            }
        })
        .collect();
    let tag = Tag {
        offset: niche.offset,
        width: niche.width,
        encoding: Encoding::Niche {
            dataful: d as u32,
            first: first as u32,
            last: last as u32,
            start: niche.start,
        },
    };
    let rest = (niche.count > used).then(|| Niche {
        start: niche.start.wrapping_add(used),
        count: niche.count - used,
        ..niche
    });
    Ok(Some((offsets, end, tag, rest)))
}

fn fixed(shapes: &[Shape], shape: &Shape, out: &Computed<'_>) -> Result<Recipe, LayoutError> {
    let rows = cases(shape);
    let variant = matches!(shape.body, Body::Variant(_));
    // (size, align, logical) per field, per case.
    let get = |t: TypeRef| done(out, t);
    let mut measured = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut m = Vec::with_capacity(row.len());
        for f in row.iter() {
            m.push(measure(shapes, shape.repr, &f.ty, &get).map_err(|why| err(shape, f, why))?);
        }
        measured.push(m);
    }
    // charge-v1 (MEM-24): a variant is 8 plus its largest payload, however it is laid
    // out.
    let mut logical = 0;
    for (m, fields) in measured.iter().zip(&rows) {
        let mut row_logical = if variant { 8u64 } else { 0 };
        for (&(_, _, bytes), f) in m.iter().zip(fields.iter()) {
            row_logical = row_logical
                .checked_add(bytes)
                .ok_or_else(|| err(shape, f, "logical size exceeds 2^64 - 1 bytes"))?;
        }
        logical = logical.max(row_logical);
    }
    let fields_align = measured.iter().flatten().map(|&(_, a, _)| a).max();
    let niched = if variant {
        niche_layout(shapes, &rows, &measured, shape, out)?
    } else {
        None
    };
    let (offsets, end, tag, niche, align) = if let Some((offsets, end, tag, rest)) = niched {
        (offsets, end, Some(tag), rest, fields_align.unwrap_or(1))
    } else {
        let width = variant.then(|| tag_width(rows.len()));
        let align = fields_align.into_iter().chain(width).max().unwrap_or(1);
        let start = width.map_or(0, |t| align_up(t, align));
        let mut offsets = Vec::with_capacity(rows.len());
        let mut end = start;
        for (m, fields) in measured.iter().zip(&rows) {
            let (row, row_end) = place_row(shape, fields, m, start)?;
            end = end.max(row_end);
            offsets.push(row);
        }
        let niche = match width {
            Some(w) => {
                let spare = (1u64 << (8 * w)) - rows.len() as u64;
                (spare > 0).then_some(Niche {
                    offset: 0,
                    width: w,
                    start: rows.len() as u64,
                    count: spare,
                })
            }
            None => best_niche(
                shapes,
                rows.first().copied().unwrap_or(&[]),
                offsets.first().map_or(&[][..], |o| &o[..]),
                &get,
            ),
        };
        let tag = width.map(|width| Tag {
            offset: 0,
            width,
            encoding: Encoding::Direct,
        });
        (offsets, end, tag, niche, align)
    };
    let size = checked_align_up(end, align).ok_or_else(|| LayoutError::NativeRange {
        ty: shape.name.clone(),
        field: rows
            .iter()
            .flat_map(|r| r.iter())
            .last()
            .map_or_else(String::new, |f| f.name.clone()),
        bytes: wide_align_up(end.into(), align),
    })?;
    let drop = drop_recipe(shapes, shape, &rows, &offsets, out);
    Ok(Recipe {
        plain: drop.is_none() && shape.repr != Repr::Resource,
        drop,
        logical_size: Some(logical),
        layout: Layout::Fixed(Fixed {
            size,
            align,
            tag,
            offsets,
            niche,
        }),
    })
}

/// Whether `target` is compact data a [`FieldTy::Locator`] may name.
fn locates(shapes: &[Shape], target: &FieldTy) -> bool {
    match target {
        FieldTy::Text | FieldTy::FixedSpan(_) | FieldTy::SeqSpan(_) | FieldTy::IndexedSpan(_) => {
            true
        }
        FieldTy::Inline(c) => matches!(shapes[c.index()].repr, Repr::Compact(_)),
        _ => false,
    }
}

/// Native size, native alignment and logical size (MEM-24) of a field of type `ty`
/// in a fixed layout whose holder has representation `holder`, or why the holder
/// refuses it.
fn measure<'r>(
    shapes: &[Shape],
    holder: Repr,
    ty: &FieldTy,
    get: &dyn Fn(TypeRef) -> &'r Recipe,
) -> Result<(u64, u64, u64), &'static str> {
    let stable = holder == Repr::Stable;
    Ok(match ty {
        FieldTy::Scalar(s) => (s.size(), s.size(), s.size()),
        FieldTy::Inline(t) => {
            let inner = &shapes[t.index()];
            if matches!(inner.repr, Repr::Compact(_)) {
                return Err("a compact value lives only in a blob");
            }
            if stable && inner.repr != Repr::Stable && !get(*t).plain {
                return Err("a stable type's fields are plain or stable (MEM-2)");
            }
            if inner.repr == Repr::Resource {
                // A resource value is a pointer to its block, which carries the
                // cause cell and grant; it counts 8 in its holder (MEM-24).
                return Ok((POINTER, POINTER, 8));
            }
            let Layout::Fixed(l) = &get(*t).layout else {
                unreachable!("non-compact shapes have fixed layouts")
            };
            (l.size, l.align, get(*t).logical_size.expect("fixed"))
        }
        FieldTy::Array(e, n) => {
            if !array_element(shapes, e) {
                return Err("an inline array holds plain or own values (SEM-9)");
            }
            let (size, align, logical) = measure(shapes, holder, e, get)?;
            match (size.checked_mul(*n), logical.checked_mul(*n)) {
                (Some(s), Some(l)) => (s, align, l),
                _ => return Err("an inline array larger than 2^64 - 1 bytes"),
            }
        }
        FieldTy::Stable(t) if matches!(shapes[t.index()].repr, Repr::Compact(_)) => {
            return Err("a reference to compact data is a locator");
        }
        FieldTy::Stable(_) => (POINTER, POINTER, 16),
        FieldTy::Locator(target) if locates(shapes, target) => (2 * POINTER, POINTER, 16),
        FieldTy::Locator(_) => {
            return Err("a locator names compact data: Text, a span or a compact type");
        }
        FieldTy::Fn => (POINTER, POINTER, 8),
        FieldTy::Borrow if holder != Repr::Scoped => {
            return Err("views and scoped callables live only in scoped types");
        }
        FieldTy::Borrow => (CLOSURE_CARRIER, POINTER, 16),
        FieldTy::View(_) if holder != Repr::Scoped => {
            return Err("views and scoped callables live only in scoped types");
        }
        // A compact value has no native field form; its view is its locator. So a
        // template's `View[$i]` is right for every argument by substitution alone.
        FieldTy::View(t) => match **t {
            FieldTy::Inline(c) if matches!(shapes[c.index()].repr, Repr::Compact(_)) => {
                (2 * POINTER, POINTER, 16)
            }
            _ => measure(shapes, Repr::Own, t, get)?,
        },
        _ if stable => return Err("a stable type's fields are plain or stable (MEM-2)"),
        FieldTy::Box(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Handle(Handle::Task | Handle::Capability | Handle::Causes | Handle::HostFile) => {
            (POINTER, POINTER, 8)
        }
        FieldTy::Handle(Handle::Sender | Handle::Receiver) => (POINTER, POINTER, 24),
        FieldTy::Seq(_) | FieldTy::Map(..) => (SEQ_CARRIER, POINTER, 24),
        FieldTy::OwnFn => (CLOSURE_CARRIER, POINTER, 16),
        FieldTy::Text
        | FieldTy::FixedSpan(_)
        | FieldTy::SeqSpan(_)
        | FieldTy::IndexedSpan(_)
        | FieldTy::Link(_) => return Err("compact field types need a compact declaration"),
    })
}

/// SEM-9: an inline array's element is plain or own, never a resource or scoped.
fn array_element(shapes: &[Shape], ty: &FieldTy) -> bool {
    match ty {
        FieldTy::Inline(t) => matches!(shapes[t.index()].repr, Repr::Own | Repr::Stable),
        FieldTy::Array(e, _) => array_element(shapes, e),
        FieldTy::Borrow
        | FieldTy::View(_)
        | FieldTy::Handle(Handle::Task | Handle::Sender | Handle::Receiver | Handle::HostFile) => {
            false
        }
        _ => true,
    }
}

/// One `Map[K, V]` entry slot. The carrier is {entries, slots, capacity} (u64
/// each): `slots` entries were inserted since the storage was made, in insertion
/// order, at `entries + i × stride`. An entry is a u64 hash at 0, a u64 state at 8
/// (1 live, 0 removed: `map.remove` cleans nothing there later and keeps the
/// order of the rest, SEM-5), then the key and the value at their natural
/// alignment. Growth copies the live entries in order, so `slots` restarts at the
/// live count. The live count and the lookup index follow the `capacity` entries
/// in the same block, private to the map operations; cleanup reads only entries
/// and charges never count them (MEM-24: an entry is charged key + value + 16).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapEntry {
    pub stride: u64,
    pub align: u64,
    pub key: u64,
    pub value: u64,
}

pub fn map_entry(shapes: &[Shape], recipes: &[Recipe], k: &FieldTy, v: &FieldTy) -> MapEntry {
    let up = |n: u64, a: u64| n.div_ceil(a) * a;
    let (ks, ka, _) = field_size(shapes, recipes, k);
    let (vs, va, _) = field_size(shapes, recipes, v);
    let key = up(16, ka);
    let value = up(key + ks, va);
    let align = 8.max(ka).max(va);
    MapEntry {
        stride: up(value + vs, align),
        align,
        key,
        value,
    }
}

/// Native size, native alignment and logical size of an own-held field of type `ty`
/// whose shape is already accepted: what drop glue needs for an array's stride.
pub fn field_size(shapes: &[Shape], recipes: &[Recipe], ty: &FieldTy) -> (u64, u64, u64) {
    measure(shapes, Repr::Own, ty, &|t| &recipes[t.index()]).expect("an accepted field type")
}

/// SEM-4: a `List` of a zero-sized element has no element storage (its capacity is
/// 2^64 - 1 from creation and nothing is admitted), so it has nothing to clean.
pub fn storage_free(shapes: &[Shape], ty: &FieldTy) -> bool {
    matches!(ty, FieldTy::Seq(e) if zero_sized(shapes, e, &mut vec![]))
}

/// Whether a field of type `ty` has size 0, read from the shapes alone (a list's
/// element need not be computed before its holder): a record whose fields all have
/// size 0, or an array of none or of such. A variant has a tag; anything else holds
/// a scalar, a pointer or a carrier.
fn zero_sized(shapes: &[Shape], ty: &FieldTy, seen: &mut Vec<TypeRef>) -> bool {
    match ty {
        FieldTy::Inline(t) => {
            let shape = &shapes[t.index()];
            if seen.contains(t) || matches!(shape.repr, Repr::Resource | Repr::Compact(_)) {
                return false;
            }
            let Body::Record(fields) = &shape.body else {
                return false;
            };
            seen.push(*t);
            let zero = fields.iter().all(|f| zero_sized(shapes, &f.ty, seen));
            seen.pop();
            zero
        }
        FieldTy::Array(e, n) => *n == 0 || zero_sized(shapes, e, seen),
        _ => false,
    }
}

fn needs_drop(shapes: &[Shape], ty: &FieldTy, out: &Computed<'_>) -> bool {
    drops(shapes, ty, &|t| done(out, t).drop.is_some())
}

/// How the block an owning edge points at is cleaned, which fixes the native prefix it
/// needs (crate `hirsel-core-ir`, "Cleanup classes").
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Cleanup {
    /// Cleaning the block drops nothing: a free alone, nothing to resume, no frame.
    Terminal,
    /// A non-terminal `Box`, `Rc` or `Arc` payload: one fixed-size root whose recipe,
    /// charge and payload offset the edge's type fixes.
    Fixed,
    /// Non-terminal `Seq` or `Map` storage, or an `own fn` environment: its extent (or,
    /// for an environment, its captures) is not fixed by the edge.
    Dynamic,
    /// A `Box`, `Rc` or `Arc` of a resource type: the resource block, with its own
    /// prefix and cleanup.
    Resource,
}

/// Whether cleaning the block `edge` owns is a free with nothing to drop
/// ([`cleanup_class`] is [`Cleanup::Terminal`]).
pub fn terminal(shapes: &[Shape], recipes: &[Recipe], edge: &FieldTy) -> bool {
    cleanup_class(shapes, recipes, edge) == Some(Cleanup::Terminal)
}

/// The cleanup class of the block an owning edge (`Box`, `Rc`, `Arc`, `Seq`, `Map`,
/// `OwnFn`) points at; `None` for an edge that owns no block. Terminal is a `Box` or
/// counted payload whose type has no drop recipe, `Seq` storage of elements that need no
/// drop (`String` and `Bytes` buffers, a `List` of plain elements), or `Map` storage whose
/// keys and values need none. Derived, never stored.
pub fn cleanup_class(shapes: &[Shape], recipes: &[Recipe], edge: &FieldTy) -> Option<Cleanup> {
    let drop = |t: TypeRef| recipes[t.index()].drop.is_some();
    Some(match edge {
        FieldTy::Box(t) | FieldTy::Rc(t) | FieldTy::Arc(t) => {
            if shapes[t.index()].repr == Repr::Resource {
                Cleanup::Resource
            } else if drop(*t) {
                Cleanup::Fixed
            } else {
                Cleanup::Terminal
            }
        }
        FieldTy::Seq(e) if !drops(shapes, e, &drop) => Cleanup::Terminal,
        FieldTy::Map(k, v) if !drops(shapes, k, &drop) && !drops(shapes, v, &drop) => {
            Cleanup::Terminal
        }
        FieldTy::Seq(_) | FieldTy::Map(..) | FieldTy::OwnFn => Cleanup::Dynamic,
        _ => return None,
    })
}

/// Whether a value of `ty` has anything to drop, given which rows have a recipe.
fn drops(shapes: &[Shape], ty: &FieldTy, drop: &dyn Fn(TypeRef) -> bool) -> bool {
    match ty {
        FieldTy::Seq(e) => !zero_sized(shapes, e, &mut vec![]),
        FieldTy::Inline(t) => !matches!(shapes[t.index()].repr, Repr::Compact(_)) && drop(*t),
        FieldTy::Box(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Map(..)
        | FieldTy::OwnFn
        | FieldTy::Handle(
            Handle::Task | Handle::Sender | Handle::Receiver | Handle::Causes | Handle::HostFile,
        ) => true,
        FieldTy::Array(e, n) => *n > 0 && drops(shapes, e, drop),
        _ => false,
    }
}

fn drop_recipe(
    shapes: &[Shape],
    shape: &Shape,
    rows: &[&[Field]],
    offsets: &[Vec<u64>],
    out: &Computed<'_>,
) -> Option<DropRecipe> {
    let fields: Vec<Vec<FieldDrop>> = rows
        .iter()
        .zip(offsets)
        .map(|(row, offs)| {
            row.iter()
                .enumerate()
                .rev()
                .filter(|(_, f)| needs_drop(shapes, &f.ty, out))
                .map(|(i, f)| FieldDrop {
                    field: i as u32,
                    offset: offs[i],
                    ty: f.ty.clone(),
                })
                .collect()
        })
        .collect();
    // A resource always has cleanup: its block, cell and grant are freed and refunded.
    let any = shape.repr == Repr::Resource || shape.finalizer.is_some();
    (any || fields.iter().any(|r| !r.is_empty())).then_some(DropRecipe {
        finalizer: shape.finalizer,
        fields,
    })
}

fn compact(
    shapes: &[Shape],
    shape: &Shape,
    declared: Width,
    out: &Computed<'_>,
) -> Result<Recipe, LayoutError> {
    let rows = cases(shape);
    if matches!(shape.body, Body::Record(ref f) if f.is_empty()) {
        return Err(LayoutError::Empty {
            ty: shape.name.clone(),
        });
    }
    // The compact encoding is public (MEM-9): a variant's tag is its ordinal, except
    // MEM-10's optional link, two cases of which one is empty and the other one link,
    // whose empty case is the zero distance no link holds.
    let link_option = match rows.as_slice() {
        [a, b] if matches!(shape.body, Body::Variant(_)) => match (a, b) {
            ([], [f]) if matches!(f.ty, FieldTy::Link(_)) => Some(1),
            ([f], []) if matches!(f.ty, FieldTy::Link(_)) => Some(0),
            _ => None,
        },
        _ => None,
    };
    let encode = |w: Width| -> Result<Compact, LayoutError> {
        let tag = match link_option {
            Some(dataful) => Some(Tag {
                offset: 0,
                width: w.bytes(),
                encoding: Encoding::Niche {
                    dataful,
                    first: 1 - dataful,
                    last: 1 - dataful,
                    start: 0,
                },
            }),
            None => matches!(shape.body, Body::Variant(_)).then(|| Tag {
                offset: 0,
                width: tag_width(rows.len()),
                encoding: Encoding::Direct,
            }),
        };
        let start = match tag {
            Some(Tag {
                width,
                encoding: Encoding::Direct,
                ..
            }) => width,
            _ => 0,
        };
        let mut fields = Vec::with_capacity(rows.len());
        let mut extents = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut pos = Pos::At(start);
            let mut cfs = Vec::with_capacity(row.len());
            for (i, f) in row.iter().enumerate() {
                let size = compact_size(shapes, shape, f, &f.ty, w, false, out)?;
                let next = match (&size, pos) {
                    (Size::Fixed(n), Pos::At(o)) => Pos::At(
                        o.checked_add(*n)
                            .ok_or_else(|| err(shape, f, "logical size exceeds 2^64 - 1 bytes"))?,
                    ),
                    (Size::Fixed(n), Pos::After { field, plus }) => Pos::After {
                        field,
                        plus: plus
                            .checked_add(*n)
                            .ok_or_else(|| err(shape, f, "logical size exceeds 2^64 - 1 bytes"))?,
                    },
                    (_, _) => Pos::After {
                        field: i as u32,
                        plus: 0,
                    },
                };
                cfs.push(CField { pos, size });
                pos = next;
            }
            extents.push(match pos {
                // The optional link's empty case is its zero word.
                Pos::At(_) if link_option.is_some() && row.is_empty() => Some(w.bytes()),
                Pos::At(n) => Some(n),
                Pos::After { .. } => None,
            });
            fields.push(cfs);
        }
        let fixed_extent = match extents.first() {
            Some(Some(e)) if extents.iter().all(|x| *x == Some(*e)) => Some(*e),
            _ => None,
        };
        Ok(Compact {
            width: w,
            tag,
            fields,
            fixed_extent,
        })
    };
    let at = [
        encode(Width::U16)?,
        encode(Width::U32)?,
        encode(Width::U64)?,
    ];
    let fixed_extent = at[declared.index()].fixed_extent;
    if fixed_extent.is_some_and(|e| e > declared.max_blob()) {
        return Err(LayoutError::TooLarge {
            ty: shape.name.clone(),
        });
    }
    Ok(Recipe {
        layout: Layout::Compact(CompactSet { declared, at }),
        logical_size: fixed_extent,
        plain: true,
        drop: None,
    })
}

fn compact_size(
    shapes: &[Shape],
    shape: &Shape,
    f: &Field,
    ty: &FieldTy,
    w: Width,
    element: bool,
    out: &Computed<'_>,
) -> Result<Size, LayoutError> {
    Ok(match ty {
        FieldTy::Scalar(s) => Size::Fixed(s.size()),
        FieldTy::Link(_) if element => {
            return Err(err(
                shape,
                f,
                "a link is a field, never an element (MEM-10)",
            ));
        }
        FieldTy::Link(t) => {
            if !matches!(shapes[t.index()].repr, Repr::Compact(_)) {
                return Err(err(
                    shape,
                    f,
                    "a link targets a compact type in the same blob",
                ));
            }
            Size::Fixed(w.bytes())
        }
        FieldTy::Inline(t) if element && shapes[t.index()].repr == Repr::Own => {
            match packed_plain(shapes, *t)
                .map_err(|()| err(shape, f, "logical size exceeds 2^64 - 1 bytes"))?
            {
                Some(n) => Size::Fixed(n),
                None => {
                    return Err(err(
                        shape,
                        f,
                        "a sequence element must have a packed encoding",
                    ));
                }
            }
        }
        FieldTy::Inline(t)
            if element
                && matches!(shapes[t.index()].repr, Repr::Compact(_))
                && out.get(*t).is_none() =>
        {
            Size::Inline(*t)
        }
        FieldTy::Inline(t) => match &done(out, *t).layout {
            Layout::Compact(set) => set.at(w).fixed_extent.map_or(Size::Inline(*t), Size::Fixed),
            Layout::Fixed(_) => match packed_plain(shapes, *t)
                .map_err(|()| err(shape, f, "logical size exceeds 2^64 - 1 bytes"))?
            {
                Some(n) if element => Size::Fixed(n),
                _ => {
                    return Err(err(
                        shape,
                        f,
                        "a compact type holds only compact data inline; an element is packed",
                    ));
                }
            },
        },
        FieldTy::Text => Size::FixedSpan { stride: 1 },
        FieldTy::Array(..) => {
            return Err(err(
                shape,
                f,
                "a compact type holds no inline array (MEM-9)",
            ));
        }
        FieldTy::FixedSpan(e) => match compact_size(shapes, shape, f, e, w, true, out)? {
            Size::Fixed(stride) => Size::FixedSpan { stride },
            _ => return Err(err(shape, f, "FixedSpan elements have one fixed size")),
        },
        FieldTy::SeqSpan(e) | FieldTy::IndexedSpan(e) => {
            compact_size(shapes, shape, f, e, w, true, out)?;
            if matches!(ty, FieldTy::SeqSpan(_)) {
                Size::SeqSpan
            } else {
                Size::IndexedSpan
            }
        }
        _ => {
            return Err(err(
                shape,
                f,
                "a compact type holds no own, counted, resource or locator field",
            ));
        }
    })
}

/// MEM-12: the packed encoding of a plain record or tuple with at least one field,
/// each a scalar or such a record, as its fields back to back (MEM-9); its size.
fn packed_plain(shapes: &[Shape], t: TypeRef) -> Result<Option<u64>, ()> {
    let shape = &shapes[t.index()];
    let Body::Record(fields) = &shape.body else {
        return Ok(None);
    };
    if shape.repr != Repr::Own || fields.is_empty() {
        return Ok(None);
    }
    let mut size = 0u64;
    for field in fields {
        let bytes = match &field.ty {
            FieldTy::Scalar(x) => Some(x.size()),
            FieldTy::Inline(t) => packed_plain(shapes, *t)?,
            _ => None,
        };
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        size = size.checked_add(bytes).ok_or(())?;
    }
    Ok(Some(size))
}

/// Bytes a compact value occupies, read from its blob: the walk the recipe
/// describes. `read_w(offset)` loads a `W`-width unsigned integer at `offset` from
/// the value's start. This is the definition both native engines and the evaluator
/// implement; it is also how tests derive expected extents of hand-built blobs.
pub fn compact_extent(
    recipes: &[Recipe],
    ty: TypeRef,
    w: Width,
    base: u64,
    read_w: &dyn Fn(u64) -> u64,
    read_tag: &dyn Fn(u64, u64) -> u64,
) -> u64 {
    let Layout::Compact(set) = &recipes[ty.index()].layout else {
        panic!("compact_extent of a fixed layout")
    };
    let c = set.at(w);
    if let Some(e) = c.fixed_extent {
        return e;
    }
    let case = c
        .tag
        .map_or(0, |t| case_of(&t, read_tag(base + t.offset, t.width))) as usize;
    let row = &c.fields[case];
    let ends = field_ends(recipes, c, row, base, read_w, read_tag);
    ends.last()
        .copied()
        .unwrap_or(base + c.tag.map_or(0, |t| t.width))
        - base
}

/// Absolute start of every field of `row` for a value at `base`.
pub fn compact_starts(
    recipes: &[Recipe],
    c: &Compact,
    row: &[CField],
    base: u64,
    read_w: &dyn Fn(u64) -> u64,
    read_tag: &dyn Fn(u64, u64) -> u64,
) -> Vec<u64> {
    let ends = field_ends(recipes, c, row, base, read_w, read_tag);
    row.iter()
        .map(|f| match f.pos {
            Pos::At(o) => base + o,
            Pos::After { field, plus } => ends[field as usize] + plus,
        })
        .collect()
}

fn field_ends(
    recipes: &[Recipe],
    c: &Compact,
    row: &[CField],
    base: u64,
    read_w: &dyn Fn(u64) -> u64,
    read_tag: &dyn Fn(u64, u64) -> u64,
) -> Vec<u64> {
    let w = c.width.bytes();
    let mut ends: Vec<u64> = Vec::with_capacity(row.len());
    for f in row {
        let start = match f.pos {
            Pos::At(o) => base + o,
            Pos::After { field, plus } => ends[field as usize] + plus,
        };
        let len = match &f.size {
            Size::Fixed(n) => *n,
            Size::FixedSpan { stride } => w + read_w(start) * stride,
            Size::SeqSpan => 2 * w + read_w(start + w),
            Size::IndexedSpan => 2 * w + read_w(start) * w + read_w(start + w),
            Size::Inline(t) => compact_extent(recipes, *t, c.width, start, read_w, read_tag),
        };
        ends.push(start + len);
    }
    ends
}
