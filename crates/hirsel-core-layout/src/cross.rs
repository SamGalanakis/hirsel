//! What a `cross` value owns as it moves between units (CONC-15, CONC-17): each
//! sequence buffer, client, socket, listener and `Shared` holder, by byte offset
//! in the value. The runtime copies sequence contents and moves client handles
//! and `Shared` holders. Sockets and listeners move with their host state,
//! detached from the poller until taken. Capability, function and `Secret`
//! words copy as bytes; their rows let a recorder write each one as its
//! snapshot or identity (docs/recording.md, "Values"). A media key object
//! also moves by its owner word. A plain variant's
//! words and padding are listed per case, each case's behind a `CASE` row.
use crate::{
    Body, Encoding, Field, FieldTy, Fixed, Handle, Layout, RecipeRow, Row, Tag, TypeRef, field_size,
};

/// A sequence carrier (pointer, length, capacity).
pub const SEQ: u64 = 0;
/// A `Client[T]` handle word.
pub const CLIENT: u64 = 1;
/// A whole socket or listener handle word.
pub const SOCKET: u64 = 2;
/// One holder of a `Shared` node: the pointer moves, the payload stays put.
pub const SHARED: u64 = 3;
/// A capability word: plain, copied with the value's bytes.
pub const CAPABILITY: u64 = 4;
/// A plain `fn` code word: plain, copied with the value's bytes.
pub const FUNCTION: u64 = 5;
/// A `Secret` holder word: it moves with the value's bytes, the holder
/// staying in the run's table; a recorder writes its designation.
pub const SECRET: u64 = 6;
/// Set on a row after a `SEQ` carrier: its offset is within each element.
/// A crossing list's plain elements can have capability words, function
/// words, padding and case guards.
pub const ELEMENT: u64 = 8;
/// A guard: the rows it covers apply only while a plain variant
/// is in one case. Its row is `[tag offset, CASE | OTHER? | width << 8 |
/// covered << 16, low, count]`: the case holds when the tag word (`width`
/// bytes, zero-extended) less `low`, wrapping at the width, is below `count`,
/// or, with `OTHER`, when it is not (a niche's dataful case). The covered rows
/// follow it, and a guard counts as one of its outer guard's rows with its own
/// covered rows. Inside a sequence's elements it is flagged `ELEMENT` too.
pub const CASE: u64 = 16;
/// Set on a `CASE` row whose case is the words outside its range.
pub const OTHER: u64 = 32;
/// A range to zero in the crossing copy: `[offset, PADDING, length, 0]`.
pub const PADDING: u64 = 64;
/// An owned media key object, written as snapshot tag 8 through 12.
pub const MEDIA: u64 = 128;

/// Owner and padding rows of a value of row `ty`. Owner rows are in field order
/// as `[offset, kind, element bytes, element logical size]`. Each `SEQ` row is
/// followed by its element's rows, flagged `ELEMENT`.
pub fn owners(shapes: &[Row], recipes: &[RecipeRow], ty: TypeRef) -> Vec<[u64; 4]> {
    let mut out = Vec::new();
    row(shapes, recipes, ty, 0, &mut out);
    out
}

/// The rows of `rows` that hold for a value, with each `CASE` row resolved
/// by `tag`, which reads `width` bytes at an offset: a guard and its covered
/// rows go, and the covered rows stay when its case holds. Element rows
/// (offsets inside an element) pass through for the caller to resolve per
/// element.
pub fn live(rows: &[[u64; 4]], tag: &dyn Fn(u64, u64) -> u64) -> Vec<[u64; 4]> {
    let mut out = Vec::with_capacity(rows.len());
    visit_live(rows, 0, tag, &mut |row| out.push(row));
    out
}

/// The padding ranges of a value or of one sequence element.
pub fn padding(
    rows: &[[u64; 4]],
    element: bool,
    tag: &dyn Fn(u64, u64) -> u64,
    mut clear: impl FnMut(u64, u64),
) {
    let flag = if element { ELEMENT } else { 0 };
    visit_live(rows, flag, tag, &mut |[at, kind, len, _]| {
        if kind == (PADDING | flag) {
            clear(at, len);
        }
    });
}

/// Rows for a sequence's elements, starting just after the carrier row.
pub fn elements(rows: &[[u64; 4]]) -> &[[u64; 4]] {
    let n = rows.iter().take_while(|r| r[1] & ELEMENT != 0).count();
    &rows[..n]
}

/// Each sequence carrier and the rows for its elements.
pub fn sequences(rows: &[[u64; 4]]) -> impl Iterator<Item = (&[u64; 4], &[[u64; 4]])> + Clone {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row[1] == SEQ)
        .map(|(i, row)| (row, elements(&rows[i + 1..])))
}

fn visit_live(
    rows: &[[u64; 4]],
    flag: u64,
    tag: &dyn Fn(u64, u64) -> u64,
    visit: &mut impl FnMut([u64; 4]),
) {
    let mut i = 0;
    while i < rows.len() {
        let r = rows[i];
        i += 1;
        if r[1] & CASE == 0 || r[1] & ELEMENT != flag {
            visit(r);
            continue;
        }
        let covered = (r[1] >> 16) as usize;
        let width = (r[1] >> 8) & 0xff;
        let mask = if width >= 8 {
            u64::MAX
        } else {
            (1 << (8 * width)) - 1
        };
        let inside = tag(r[0], width).wrapping_sub(r[2]) & mask < r[3];
        if inside != (r[1] & OTHER != 0) {
            visit_live(&rows[i..i + covered], flag, tag, visit);
        }
        i += covered;
    }
}

fn row(shapes: &[Row], recipes: &[RecipeRow], ty: TypeRef, at: u64, out: &mut Vec<[u64; 4]>) {
    let Layout::Fixed(fixed) = &recipes[ty.index()].layout else {
        return;
    };
    match &shapes[ty.index()].body {
        Body::Record(fields) => {
            fields_at(shapes, recipes, fields, fixed, 0, at, out);
        }
        // A variant is cross only when plain, so its only owner words are
        // capabilities and functions.
        Body::Variant(cases) => {
            for (c, case) in cases.iter().enumerate() {
                let mut inner = Vec::new();
                fields_at(shapes, recipes, &case.fields, fixed, c, at, &mut inner);
                match fixed.tag {
                    _ if inner.is_empty() => {}
                    None => out.extend(inner),
                    Some(tag) => {
                        out.push(guard(&tag, c as u32, at, inner.len() as u64));
                        out.extend(inner);
                    }
                }
            }
        }
    }
}

/// Record the fields' rows, then the gaps around the fields and tag. A niche
/// tag can overlap a field, so use the union of their ranges.
fn fields_at(
    shapes: &[Row],
    recipes: &[RecipeRow],
    fields: &[Field],
    fixed: &Fixed,
    case: usize,
    at: u64,
    out: &mut Vec<[u64; 4]>,
) {
    let mut occupied = Vec::with_capacity(fields.len() + usize::from(fixed.tag.is_some()));
    if let Some(tag) = fixed.tag {
        occupied.push((tag.offset, tag.offset + tag.width));
    }
    for (field, &offset) in fields.iter().zip(&fixed.offsets[case]) {
        held(shapes, recipes, &field.ty, at + offset, out);
        let bytes = field_size(shapes, recipes, &field.ty).0;
        if bytes != 0 {
            occupied.push((offset, offset + bytes));
        }
    }
    occupied.sort_unstable();
    let mut end = 0;
    for (start, next_end) in occupied {
        if start > end {
            out.push([at + end, PADDING, start - end, 0]);
        }
        end = end.max(next_end);
    }
    if end < fixed.size {
        out.push([at + end, PADDING, fixed.size - end, 0]);
    }
}

/// The `CASE` row of case `c` of a variant at `at` whose tag is `tag`.
fn guard(tag: &Tag, c: u32, at: u64, covered: u64) -> [u64; 4] {
    let head = CASE | tag.width << 8 | covered << 16;
    match tag.encoding {
        Encoding::Niche {
            dataful,
            first,
            last,
            start,
        } if c == dataful => [
            at + tag.offset,
            head | OTHER,
            start,
            u64::from(last - first) + 1,
        ],
        _ => [
            at + tag.offset,
            head,
            crate::case_word(tag, c).unwrap_or(0),
            1,
        ],
    }
}

fn held(shapes: &[Row], recipes: &[RecipeRow], ty: &FieldTy, at: u64, out: &mut Vec<[u64; 4]>) {
    match ty {
        FieldTy::Inline(t) => row(shapes, recipes, *t, at, out),
        FieldTy::Seq(e) => {
            let (bytes, _, logical) = field_size(shapes, recipes, e);
            out.push([at, SEQ, bytes, logical]);
            let mut inner = Vec::new();
            held(shapes, recipes, e, 0, &mut inner);
            out.extend(
                inner
                    .into_iter()
                    .filter(|o| matches!(o[1], CAPABILITY | FUNCTION | PADDING) || o[1] & CASE != 0)
                    .map(|[offset, kind, low, count]| [offset, kind | ELEMENT, low, count]),
            );
        }
        FieldTy::Handle(Handle::Client) => out.push([at, CLIENT, 0, 0]),
        FieldTy::Handle(Handle::Socket) => out.push([at, SOCKET, 0, 0]),
        FieldTy::Handle(Handle::Capability) => out.push([at, CAPABILITY, 0, 0]),
        FieldTy::Handle(Handle::Secret) => out.push([at, SECRET, 0, 0]),
        FieldTy::Handle(Handle::Media) => out.push([at, MEDIA, 0, 0]),
        FieldTy::Fn => out.push([at, FUNCTION, 0, 0]),
        FieldTy::Shared(_) => out.push([at, SHARED, 0, 0]),
        FieldTy::Array(e, n) => {
            let (stride, _, _) = field_size(shapes, recipes, e);
            for i in 0..*n {
                held(shapes, recipes, e, at + i * stride, out);
            }
        }
        _ => {}
    }
}

/// Where a task entry's `x` sits in its wrapped input `(f, x)` of row `ty`:
/// its offset and its bytes. A recorder writes those bytes as the entry's
/// value (docs/recording.md, "Helper stream header").
pub fn entry_span(shapes: &[Row], recipes: &[RecipeRow], ty: TypeRef) -> [u64; 2] {
    match (&shapes[ty.index()].body, &recipes[ty.index()].layout) {
        (Body::Record(fields), Layout::Fixed(fixed)) if fields.len() == 2 => [
            fixed.offsets[0][1],
            field_size(shapes, recipes, &fields[1].ty).0,
        ],
        _ => [0, 0],
    }
}
