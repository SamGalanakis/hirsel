//! Layout recipes: computed once per declared shape (ARCH "Runtime and layout").
//!
//! Own, resource, stable and scoped types get a native fixed layout: declaration
//! order, natural alignment, a variant tag of the smallest of u8/u16/u32 at offset 0
//! and a payload union after it (MEM-13 leaves this private; both engines use this
//! one recipe so they agree). Compact types get the byte-aligned blob layout of
//! MEM-9..12: positions are either constant or "after variable field j, plus k".

use crate::shape::{Body, Field, FieldTy, FuncRef, Repr, Shape, TypeRef, Width};

/// Native size and alignment of a field slot of this type in a fixed layout.
pub const POINTER: u64 = 8;
/// `List`, `String`, `Bytes` carrier: data pointer, length, capacity (u64 each).
pub const SEQ_CARRIER: u64 = 24;
/// Closure carrier: code pointer, environment block pointer (null when no captures).
pub const CLOSURE_CARRIER: u64 = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recipe {
    pub layout: Layout,
    /// charge-v1 logical size (MEM-26): no padding; references count 8, carriers 24.
    /// For a compact type, its constant extent when it has one, else `None`.
    pub logical_size: Option<u64>,
    /// No cleanup is needed and the value may be copied bitwise (T2.2).
    pub plain: bool,
    /// Present exactly when cleaning a value of the type does anything.
    pub drop: Option<DropRecipe>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Layout {
    Fixed(Fixed),
    Compact(Compact),
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
    /// `W` length, then that many UTF-8 bytes.
    Text,
    /// `W` count, then `count * stride` bytes.
    FixedSpan {
        stride: u64,
    },
    /// `W` count, `W` element-byte extent, then the elements.
    SeqSpan,
    /// `W` count, `W` element-byte extent, a table of `count` `W` offsets (relative to
    /// the first element), then the elements.
    IndexedSpan,
    /// An inline compact value whose own extent is walked by its recipe.
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

/// FAIL-8: the finalizer runs first, then fields in reverse declaration order.
/// Sequences drop elements from the highest index down, then free; that loop is in
/// the glue built from this recipe, not here.
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
    let stable = shape.repr == Repr::Stable;
    let rows = cases(shape);
    let tag = matches!(shape.body, Body::Variant(_)).then(|| tag_width(rows.len()));
    // (size, align, logical) per field, per case.
    let mut measured = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut m = Vec::with_capacity(row.len());
        for f in row.iter() {
            let (size, align, logical) = match &f.ty {
                FieldTy::Scalar(s) => (s.size(), s.size(), s.size()),
                FieldTy::Inline(t) => {
                    if matches!(shapes[t.index()].repr, Repr::Compact(_)) {
                        return Err(err(shape, f, "a compact value lives only in a blob"));
                    }
                    if stable && shapes[t.index()].repr != Repr::Stable {
                        return Err(err(shape, f, "a stable type holds only stable data"));
                    }
                    let Layout::Fixed(l) = &done(out, *t).layout else {
                        unreachable!("non-compact shapes have fixed layouts")
                    };
                    (l.size, l.align, done(out, *t).logical_size.expect("fixed"))
                }
                FieldTy::Box(_) | FieldTy::Rc(_) | FieldTy::Arc(_) | FieldTy::Handle(_)
                    if stable =>
                {
                    return Err(err(
                        shape,
                        f,
                        "a stable type holds no own, counted or resource data",
                    ));
                }
                FieldTy::Seq(_) | FieldTy::Closure if stable => {
                    return Err(err(
                        shape,
                        f,
                        "a stable type holds no own, counted or resource data",
                    ));
                }
                FieldTy::Box(_)
                | FieldTy::Rc(_)
                | FieldTy::Arc(_)
                | FieldTy::Stable(_)
                | FieldTy::Region
                | FieldTy::Handle(_) => (POINTER, POINTER, 8),
                FieldTy::Seq(_) => (SEQ_CARRIER, 8, 24),
                FieldTy::Closure => (CLOSURE_CARRIER, 8, 16),
                FieldTy::Text
                | FieldTy::FixedSpan(_)
                | FieldTy::SeqSpan(_)
                | FieldTy::IndexedSpan(_)
                | FieldTy::Link(_)
                | FieldTy::OptLink(_) => {
                    return Err(err(
                        shape,
                        f,
                        "compact field types need a compact declaration",
                    ));
                }
            };
            m.push((size, align, logical));
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
    let logical = logical + tag.unwrap_or(0);
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

fn needs_drop(shapes: &[Shape], ty: &FieldTy, out: &[Option<Recipe>]) -> bool {
    match ty {
        FieldTy::Inline(t) => {
            !matches!(shapes[t.index()].repr, Repr::Compact(_)) && done(out, *t).drop.is_some()
        }
        FieldTy::Box(_)
        | FieldTy::Seq(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Closure
        | FieldTy::Handle(_) => true,
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
    (shape.finalizer.is_some() || fields.iter().any(|r| !r.is_empty())).then_some(DropRecipe {
        finalizer: shape.finalizer,
        fields,
    })
}

fn compact(
    shapes: &[Shape],
    shape: &Shape,
    w: Width,
    out: &[Option<Recipe>],
) -> Result<Recipe, LayoutError> {
    let rows = cases(shape);
    let tag = matches!(shape.body, Body::Variant(_)).then(|| tag_width(rows.len()));
    let start = tag.unwrap_or(0);
    let mut fields = Vec::with_capacity(rows.len());
    let mut extents = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut pos = Pos::At(start);
        let mut cfs = Vec::with_capacity(row.len());
        for (i, f) in row.iter().enumerate() {
            let size = compact_size(shapes, shape, f, &f.ty, w, out)?;
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
    if fixed_extent.is_some_and(|e| e > w.max_blob()) {
        return Err(LayoutError::TooLarge {
            ty: shape.name.clone(),
        });
    }
    Ok(Recipe {
        layout: Layout::Compact(Compact {
            width: w,
            tag: tag.map(|width| Tag { offset: 0, width }),
            fields,
            fixed_extent,
        }),
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
    out: &[Option<Recipe>],
) -> Result<Size, LayoutError> {
    Ok(match ty {
        FieldTy::Scalar(s) => Size::Fixed(s.size()),
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
        FieldTy::Inline(t) => match (&shapes[t.index()].repr, &done(out, *t).layout) {
            (Repr::Compact(tw), Layout::Compact(c)) => {
                if *tw != w {
                    return Err(err(
                        shape,
                        f,
                        "an inline compact field has its holder's width",
                    ));
                }
                c.fixed_extent.map_or(Size::Inline(*t), Size::Fixed)
            }
            _ => {
                return Err(err(
                    shape,
                    f,
                    "a compact type holds only compact data inline",
                ));
            }
        },
        FieldTy::Text => Size::Text,
        FieldTy::FixedSpan(e) => match compact_size(shapes, shape, f, e, w, out)? {
            Size::Fixed(stride) => Size::FixedSpan { stride },
            _ => return Err(err(shape, f, "FixedSpan elements have one fixed size")),
        },
        FieldTy::SeqSpan(e) | FieldTy::IndexedSpan(e) => {
            compact_size(shapes, shape, f, e, w, out)?;
            if matches!(ty, FieldTy::SeqSpan(_)) {
                Size::SeqSpan
            } else {
                Size::IndexedSpan
            }
        }
        FieldTy::Box(_)
        | FieldTy::Seq(_)
        | FieldTy::Rc(_)
        | FieldTy::Arc(_)
        | FieldTy::Stable(_)
        | FieldTy::Region
        | FieldTy::Closure
        | FieldTy::Handle(_) => {
            return Err(err(
                shape,
                f,
                "a compact type holds no own, counted, resource or locator field",
            ));
        }
    })
}

/// Bytes a compact value occupies, read from its blob: the walk the recipe
/// describes. `read_w(offset)` loads a `W`-width unsigned integer at `offset` from
/// the value's start. This is the definition both native engines and the evaluator
/// implement; it is also how tests derive expected extents of hand-built blobs.
pub fn compact_extent(
    recipes: &[Recipe],
    ty: TypeRef,
    base: u64,
    read_w: &dyn Fn(u64) -> u64,
    read_tag: &dyn Fn(u64, u64) -> u64,
) -> u64 {
    let Layout::Compact(c) = &recipes[ty.index()].layout else {
        panic!("compact_extent of a fixed layout")
    };
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
            Size::Text => w + read_w(start),
            Size::FixedSpan { stride } => w + read_w(start) * stride,
            Size::SeqSpan => 2 * w + read_w(start + w),
            Size::IndexedSpan => 2 * w + read_w(start) * w + read_w(start + w),
            Size::Inline(t) => compact_extent(recipes, *t, start, read_w, read_tag),
        };
        ends.push(start + len);
    }
    ends
}
