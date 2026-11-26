//! What a `cross` value owns as it moves between units (CONC-15, CONC-17): each
//! sequence buffer, client, socket, listener and `Shared` holder, by byte offset
//! in the value. The runtime copies sequence contents and moves client handles
//! and `Shared` holders. Sockets and listeners move with their host state,
//! detached from the poller until taken. Capability, function and `Secret`
//! words copy as bytes; their rows let a recorder write each one as its
//! snapshot or identity (docs/recording.md, "Values"). A plain variant's
//! words are listed per case, each case's behind a `CASE` row.
use crate::{Body, Encoding, FieldTy, Handle, Layout, RecipeRow, Row, Tag, TypeRef, field_size};

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
/// Set on a row that follows a `SEQ` row: an owner word inside each of that
/// sequence's elements, at its offset in the element. A crossing list's
/// elements are plain, so the base kind is `CAPABILITY` or `FUNCTION`.
pub const ELEMENT: u64 = 8;
/// A guard: the rows it covers name owner words only while a plain variant
/// is in one case. Its row is `[tag offset, CASE | OTHER? | width << 8 |
/// covered << 16, low, count]`: the case holds when the tag word (`width`
/// bytes, zero-extended) less `low`, wrapping at the width, is below `count`,
/// or, with `OTHER`, when it is not (a niche's dataful case). The covered rows
/// follow it, and a guard counts as one of its outer guard's rows with its own
/// covered rows. Inside a sequence's elements it is flagged `ELEMENT` too.
pub const CASE: u64 = 16;
/// Set on a `CASE` row whose case is the words outside its range.
pub const OTHER: u64 = 32;

/// Each owner of a value of row `ty`, as `[offset, kind, element bytes, element
/// logical size]`, in field order; each `SEQ` row is followed at once by its
/// element's owner rows, flagged `ELEMENT`.
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
    let mut i = 0;
    while i < rows.len() {
        let r = rows[i];
        i += 1;
        if r[1] & CASE == 0 || r[1] & ELEMENT != 0 {
            out.push(r);
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
            out.extend(live(&rows[i..i + covered], tag));
        }
        i += covered;
    }
    out
}

fn row(shapes: &[Row], recipes: &[RecipeRow], ty: TypeRef, at: u64, out: &mut Vec<[u64; 4]>) {
    let Layout::Fixed(fixed) = &recipes[ty.index()].layout else {
        return;
    };
    match &shapes[ty.index()].body {
        Body::Record(fields) => {
            for (field, offset) in fields.iter().zip(&fixed.offsets[0]) {
                held(shapes, recipes, &field.ty, at + offset, out);
            }
        }
        // A variant is cross only when plain, so its cases hold only
        // capability and function words, each case's behind its guard.
        Body::Variant(cases) => {
            for (c, case) in cases.iter().enumerate() {
                let mut inner = Vec::new();
                for (field, offset) in case.fields.iter().zip(&fixed.offsets[c]) {
                    held(shapes, recipes, &field.ty, at + offset, &mut inner);
                }
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
                    .filter(|o| matches!(o[1], CAPABILITY | FUNCTION) || o[1] & CASE != 0)
                    .map(|[offset, kind, low, count]| [offset, kind | ELEMENT, low, count]),
            );
        }
        FieldTy::Handle(Handle::Client) => out.push([at, CLIENT, 0, 0]),
        FieldTy::Handle(Handle::Socket) => out.push([at, SOCKET, 0, 0]),
        FieldTy::Handle(Handle::Capability) => out.push([at, CAPABILITY, 0, 0]),
        FieldTy::Handle(Handle::Secret) => out.push([at, SECRET, 0, 0]),
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
