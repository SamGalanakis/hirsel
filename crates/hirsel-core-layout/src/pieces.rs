//! The integer pieces, as (offset, width), that a native whole-value copy of a
//! fixed own value moves in place of all its bytes. Both native emitters read
//! these; the evaluator copies bytes.
//!
//! Padding is private (MEM-13): no read observes it, so a copy may skip it.
//! A load forwards from an earlier store only when that store covers it, so a
//! load that spans two narrower stores waits for both to retire.

use crate::{FieldTy, Layout, RecipeRow, Repr, Row, TypeRef, held_size};

/// Pieces for a copy whose source this body may have just written: occupied
/// bytes only, and only for a value whose occupied bytes are at most two
/// naturally aligned pieces and leave a hole. Copying the gap after a narrow
/// tag reads a word that the tag's narrow store only partly wrote. Like rustc,
/// which copies Scalar and ScalarPair values as typed scalars and everything
/// else with memcpy, a wider split costs more moves than the whole copy and
/// hides the copy from memcpy forwarding. None keeps the whole copy.
pub fn holed_pieces(shapes: &[Row], recipes: &[RecipeRow], t: TypeRef) -> Option<Vec<(u64, u64)>> {
    let mut ranges = Vec::new();
    occupied(shapes, recipes, t, 0, &mut ranges)?;
    let merged = merge(ranges, |start, end| start <= end);
    if merged == [(0, fixed_size(recipes, t)?)] {
        return None;
    }
    let pieces = split(&merged);
    (pieces.len() <= 2).then_some(pieces)
}

/// Pieces for a copy out of a place the caller wrote just before the call (a
/// `take` parameter): one piece per scalar leaf at its own width, so each load
/// lies inside the caller's store of that leaf, whatever the callee's backend
/// sees. Only the bytes where cases overlap are joined. None when the value has
/// more than [`LEAF_PIECES`] such pieces, or only whole words: the whole copy
/// is then cheaper, and words cost more moves split than the backend's wide
/// copy (LangArena LZW rose 2% in instructions with three-word strings split).
pub fn leaf_pieces(shapes: &[Row], recipes: &[RecipeRow], t: TypeRef) -> Option<Vec<(u64, u64)>> {
    let mut ranges = Vec::new();
    occupied(shapes, recipes, t, 0, &mut ranges)?;
    let pieces = split(&merge(ranges, |start, end| start < end));
    (pieces.len() <= LEAF_PIECES && pieces.iter().any(|&(_, width)| width < 8)).then_some(pieces)
}

/// The most leaf pieces a by-leaf copy moves.
pub const LEAF_PIECES: usize = 8;

fn fixed_size(recipes: &[RecipeRow], t: TypeRef) -> Option<u64> {
    match &recipes[t.index()].layout {
        Layout::Fixed(f) => Some(f.size),
        Layout::Compact(_) => None,
    }
}

fn merge(mut ranges: Vec<(u64, u64)>, joins: impl Fn(u64, u64) -> bool) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if joins(start, last.1) => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Naturally aligned integer pieces of at most 8 bytes covering each range.
fn split(ranges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut pieces = Vec::new();
    for &(start, end) in ranges {
        let mut at = start;
        while at < end {
            let width = [8, 4, 2, 1]
                .into_iter()
                .find(|w| at % w == 0 && at + w <= end)
                .expect("a one-byte piece fits");
            pieces.push((at, width));
            at += width;
        }
    }
    pieces
}

/// A resource or region value is held as a pointer to its block (MEM-5, MEM-24).
fn by_block(shapes: &[Row], t: TypeRef) -> bool {
    matches!(shapes[t.index()].repr, Repr::Resource | Repr::Region)
}

/// Push the byte range of each leaf that the tag or some case's data of fixed
/// own type `t` at offset `at` occupies. None when the value is too
/// fragmented to be worth splitting.
fn occupied(
    shapes: &[Row],
    recipes: &[RecipeRow],
    t: TypeRef,
    at: u64,
    out: &mut Vec<(u64, u64)>,
) -> Option<()> {
    let Layout::Fixed(f) = &recipes[t.index()].layout else {
        return None;
    };
    if by_block(shapes, t) {
        out.push((at, at + 8));
        return Some(());
    }
    if let Some(tag) = f.tag {
        out.push((at + tag.offset, at + tag.offset + tag.width));
    }
    let shape = &shapes[t.index()];
    for (case, offsets) in f.offsets.iter().enumerate() {
        let case = f.tag.map(|_| case as u32);
        for (field, &offset) in shape.fields(case)?.iter().zip(offsets) {
            occupied_field(shapes, recipes, shape.repr, &field.ty, at + offset, out)?;
        }
    }
    (out.len() <= 64).then_some(())
}

fn occupied_field(
    shapes: &[Row],
    recipes: &[RecipeRow],
    holder: Repr,
    ty: &FieldTy,
    at: u64,
    out: &mut Vec<(u64, u64)>,
) -> Option<()> {
    match ty {
        FieldTy::Inline(t) if matches!(recipes[t.index()].layout, Layout::Fixed(_)) => {
            occupied(shapes, recipes, *t, at, out)
        }
        FieldTy::Array(element, n) if *n <= 4 => {
            let stride = held_size(shapes, recipes, holder, element).0;
            (0..*n).try_for_each(|i| {
                occupied_field(shapes, recipes, holder, element, at + i * stride, out)
            })
        }
        _ => {
            let size = held_size(shapes, recipes, holder, ty).0;
            out.push((at, at + size));
            Some(())
        }
    }
}
