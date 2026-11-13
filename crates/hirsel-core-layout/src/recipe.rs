//! Layout recipes: computed once per declared shape (ARCH "Runtime and layout").
//!
//! Own, resource, stable and scoped types get a native fixed layout: declaration
//! order, natural alignment, a variant tag of the smallest of u8/u16/u32 at offset 0
//! and a payload union after it (MEM-13 leaves this private; both engines use this
//! one recipe so they agree). Compact types get the byte-aligned blob layout of
//! MEM-9..12: positions are either constant or "after variable field j, plus k".

use crate::shape::{Body, Field, FieldTy, FuncRef, Handle, Repr, Shape, TypeRef, Width};

/// Native size and alignment of a field slot of this type in a fixed layout.
pub const POINTER: u64 = 8;
/// `List`, `String`, `Bytes` carrier: data pointer, length, capacity (u64 each).
pub const SEQ_CARRIER: u64 = 24;
/// `own fn` and `scoped fn` carrier: code pointer, environment pointer.
pub const CLOSURE_CARRIER: u64 = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Layout {
    Fixed(Fixed),
    Compact(CompactSet),
}

/// A compact type's encodings. A compact value held inline or as a link target is
/// encoded with its holder's width (MEM-9), so a type has one encoding per width.
#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tag {
    pub offset: u64,
    /// 1, 2 or 4 bytes: the smallest that holds the case count.
    pub width: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fixed {
    pub size: u64,
    pub align: u64,
    pub tag: Option<Tag>,
    /// Field byte offsets from the value's start: one row for a record, one per case.
    pub offsets: Vec<Vec<u64>>,
}

/// Where a compact field starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pos {
    /// A constant byte offset from the value's start.
    At(u64),
    /// `plus` bytes after the end of variable field `field` of the same case.
    After { field: u32, plus: u64 },
}

/// How many bytes a compact field occupies.
#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CField {
    pub pos: Pos,
    pub size: Size,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Compact {
    pub width: Width,
    pub tag: Option<Tag>,
    /// One row for a record, one per case (each case's first field follows the tag).
    pub fields: Vec<Vec<CField>>,
    /// The extent when every field of every case is fixed and all cases are equal.
    pub fixed_extent: Option<u64>,
}

/// One owned field a value's cleanup visits, in cleanup order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldDrop {
    pub field: u32,
    pub offset: u64,
    pub ty: FieldTy,
}

/// FAIL-8: the finalizer runs first, then fields in reverse declaration order,
/// then the block is freed and refunded. A `List` (`Seq`) and an inline array
/// (`FieldTy::Array`, at the stride `field_size` gives) clean their completed
/// elements from index 0 up (SEM-9, core.md); a `Map` cleans entry by entry in
/// insertion order, each key before its value. Those loops are in the glue built
/// from this recipe.
#[derive(Clone, Debug, Eq, PartialEq)]
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

fn align_up(n: u64, a: u64) -> u64 {
    n.div_ceil(a) * a
}

/// Recipes for every shape of a module, indexed like `shapes`.
pub fn compute(shapes: &[Shape]) -> Result<Vec<Recipe>, LayoutError> {
    let mut state = vec![State::Todo; shapes.len()];
    let mut out: Vec<Option<Recipe>> = vec![None; shapes.len()];
    for i in 0..shapes.len() {
        visit(shapes, TypeRef(i as u32), &mut state, &mut out)?;
    }
    Ok(out.into_iter().map(|r| r.expect("visited")).collect())
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
    out: &mut [Option<Recipe>],
) -> Result<(), LayoutError> {
    let mut stack = vec![root];
    while let Some(&t) = stack.last() {
        match state[t.index()] {
            State::Done => {
                stack.pop();
            }
            State::Visiting | State::Todo => {
                state[t.index()] = State::Visiting;
                let pending: Vec<TypeRef> = inline_deps(&shapes[t.index()])
                    .filter(|d| state[d.index()] != State::Done)
                    .collect();
                if let Some(&d) = pending.first() {
                    if state[d.index()] == State::Visiting {
                        return Err(LayoutError::Cycle {
                            ty: shapes[t.index()].name.clone(),
                        });
                    }
                    stack.push(d);
                    continue;
                }
                out[t.index()] = Some(recipe(shapes, &shapes[t.index()], out)?);
                state[t.index()] = State::Done;
                stack.pop();
            }
        }
    }
    Ok(())
}

fn inline_deps(shape: &Shape) -> impl Iterator<Item = TypeRef> + '_ {
    let fields: Box<dyn Iterator<Item = &Field>> = match &shape.body {
        Body::Record(f) => Box::new(f.iter()),
        Body::Variant(c) => Box::new(c.iter().flat_map(|c| c.fields.iter())),
    };
    fields.filter_map(|f| inline_target(&f.ty))
}

fn inline_target(ty: &FieldTy) -> Option<TypeRef> {
    match ty {
        FieldTy::Inline(t) => Some(*t),
        FieldTy::Array(e, _) => inline_target(e),
        FieldTy::FixedSpan(e) | FieldTy::SeqSpan(e) | FieldTy::IndexedSpan(e) => inline_target(e),
        _ => None,
    }
}

fn cases(shape: &Shape) -> Vec<&[Field]> {
    match &shape.body {
        Body::Record(f) => vec![&f[..]],
        Body::Variant(c) => c.iter().map(|c| &c.fields[..]).collect(),
    }
}

fn done(out: &[Option<Recipe>], t: TypeRef) -> &Recipe {
    out[t.index()]
        .as_ref()
        .expect("inline dependency computed first")
}

fn recipe(shapes: &[Shape], shape: &Shape, out: &[Option<Recipe>]) -> Result<Recipe, LayoutError> {
    match shape.repr {
        Repr::Compact(w) => compact(shapes, shape, w, out),
        _ => fixed(shapes, shape, out),
    }
}

fn err(shape: &Shape, f: &Field, why: &'static str) -> LayoutError {
    LayoutError::Field {
        ty: shape.name.clone(),
        field: f.name.clone(),
        why,
    }
}

fn fixed(shapes: &[Shape], shape: &Shape, out: &[Option<Recipe>]) -> Result<Recipe, LayoutError> {
    let rows = cases(shape);
    let tag = matches!(shape.body, Body::Variant(_)).then(|| tag_width(rows.len()));
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
    let align = measured
        .iter()
        .flatten()
        .map(|&(_, a, _)| a)
        .chain(tag)
        .max()
        .unwrap_or(1);
    let start = tag.map_or(0, |t| align_up(t, align));
    let mut offsets = Vec::with_capacity(rows.len());
    let mut end = start;
    let mut logical = 0;
    for m in &measured {
        let mut off = start;
        let mut row = Vec::with_capacity(m.len());
        for &(size, a, _) in m {
            off = align_up(off, a);
            row.push(off);
            off += size;
        }
        end = end.max(off);
        logical = logical.max(m.iter().map(|&(_, _, l)| l).sum::<u64>());
        offsets.push(row);
    }
    let size = align_up(end, align);
    // MEM-24: an own variant is 8 plus its largest payload, whatever its tag width.
    let logical = logical + if tag.is_some() { 8 } else { 0 };
    let drop = drop_recipe(shapes, shape, &rows, &offsets, out);
    Ok(Recipe {
        plain: drop.is_none() && shape.repr != Repr::Resource,
        drop,
        logical_size: Some(logical),
        layout: Layout::Fixed(Fixed {
            size,
            align,
            tag: tag.map(|width| Tag { offset: 0, width }),
            offsets,
        }),
    })
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
        FieldTy::Locator => (2 * POINTER, POINTER, 16),
        FieldTy::Fn => (POINTER, POINTER, 8),
        FieldTy::Borrow if holder != Repr::Scoped => {
            return Err("views and scoped callables live only in scoped types");
        }
        FieldTy::Borrow => (CLOSURE_CARRIER, POINTER, 16),
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
        | FieldTy::Link(_)
        | FieldTy::OptLink(_) => return Err("compact field types need a compact declaration"),
    })
}

/// SEM-9: an inline array's element is plain or own, never a resource or scoped.
fn array_element(shapes: &[Shape], ty: &FieldTy) -> bool {
    match ty {
        FieldTy::Inline(t) => matches!(shapes[t.index()].repr, Repr::Own | Repr::Stable),
        FieldTy::Array(e, _) => array_element(shapes, e),
        FieldTy::Borrow
        | FieldTy::Handle(Handle::Task | Handle::Sender | Handle::Receiver | Handle::HostFile) => {
            false
        }
        _ => true,
    }
}

/// Native size, native alignment and logical size of an own-held field of type `ty`
/// whose shape is already accepted: what drop glue needs for an array's stride.
pub fn field_size(shapes: &[Shape], recipes: &[Recipe], ty: &FieldTy) -> (u64, u64, u64) {
    measure(shapes, Repr::Own, ty, &|t| &recipes[t.index()]).expect("an accepted field type")
}

fn needs_drop(shapes: &[Shape], ty: &FieldTy, out: &[Option<Recipe>]) -> bool {
    match ty {
        FieldTy::Inline(t) => {
            !matches!(shapes[t.index()].repr, Repr::Compact(_)) && done(out, *t).drop.is_some()
        }
        FieldTy::Box(_)
        | FieldTy::Seq(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Map(..)
        | FieldTy::OwnFn
        | FieldTy::Handle(
            Handle::Task | Handle::Sender | Handle::Receiver | Handle::Causes | Handle::HostFile,
        ) => true,
        FieldTy::Array(e, n) => *n > 0 && needs_drop(shapes, e, out),
        _ => false,
    }
}

fn drop_recipe(
    shapes: &[Shape],
    shape: &Shape,
    rows: &[&[Field]],
    offsets: &[Vec<u64>],
    out: &[Option<Recipe>],
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
    out: &[Option<Recipe>],
) -> Result<Recipe, LayoutError> {
    let rows = cases(shape);
    if matches!(shape.body, Body::Record(ref f) if f.is_empty()) {
        return Err(LayoutError::Empty {
            ty: shape.name.clone(),
        });
    }
    let encode = |w: Width| -> Result<Compact, LayoutError> {
        let tag = matches!(shape.body, Body::Variant(_)).then(|| tag_width(rows.len()));
        let start = tag.unwrap_or(0);
        let mut fields = Vec::with_capacity(rows.len());
        let mut extents = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut pos = Pos::At(start);
            let mut cfs = Vec::with_capacity(row.len());
            for (i, f) in row.iter().enumerate() {
                let size = compact_size(shapes, shape, f, &f.ty, w, false, out)?;
                let next = match (&size, pos) {
                    (Size::Fixed(n), Pos::At(o)) => Pos::At(o + n),
                    (Size::Fixed(n), Pos::After { field, plus }) => Pos::After {
                        field,
                        plus: plus + n,
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
            tag: tag.map(|width| Tag { offset: 0, width }),
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
    out: &[Option<Recipe>],
) -> Result<Size, LayoutError> {
    Ok(match ty {
        FieldTy::Scalar(s) => Size::Fixed(s.size()),
        FieldTy::Link(_) | FieldTy::OptLink(_) if element => {
            return Err(err(
                shape,
                f,
                "a link is a field, never an element (MEM-10)",
            ));
        }
        FieldTy::Link(t) | FieldTy::OptLink(t) => {
            if !matches!(shapes[t.index()].repr, Repr::Compact(_)) {
                return Err(err(
                    shape,
                    f,
                    "a link targets a compact type in the same blob",
                ));
            }
            Size::Fixed(w.bytes())
        }
        FieldTy::Inline(t) => match &done(out, *t).layout {
            Layout::Compact(set) => set.at(w).fixed_extent.map_or(Size::Inline(*t), Size::Fixed),
            Layout::Fixed(_) => match packed_plain(shapes, *t) {
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
fn packed_plain(shapes: &[Shape], t: TypeRef) -> Option<u64> {
    let s = &shapes[t.index()];
    match &s.body {
        Body::Record(fs) if s.repr == Repr::Own && !fs.is_empty() => {
            fs.iter().try_fold(0, |n, f| match &f.ty {
                FieldTy::Scalar(x) => Some(n + x.size()),
                FieldTy::Inline(u) => packed_plain(shapes, *u).map(|m| n + m),
                _ => None,
            })
        }
        _ => None,
    }
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
    let case = c.tag.map_or(0, |t| read_tag(base + t.offset, t.width)) as usize;
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
