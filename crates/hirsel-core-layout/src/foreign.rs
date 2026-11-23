//! Fixed-layout paths from a resource's payload to its owned foreign ordinals.
use crate::{
    Body, Carried, Encoding, FieldTy, Layout, Recipe, Repr, Row, Scalar, Shape, Tag, Ty, TypeRef,
};

/// A raw handle has an eight-byte carrier and a nominal kind encoded in its
/// shape name. It is inline in a resource so the cleanup can identify it.
pub fn foreign_kind(shape: &Shape) -> Option<&str> {
    let kind = shape
        .name
        .strip_prefix("core@1/foreign/Ptr[")?
        .strip_suffix(']')?;
    (shape.repr == Repr::Own
        && shape.carried == Carried::Value(Ty::Scalar(Scalar::U64))
        && shape.finalizer.is_none()
        && matches!(&shape.body, Body::Record(fields) if matches!(fields.as_slice(),
            [field] if field.ty == FieldTy::Scalar(Scalar::U64))))
    .then_some(kind)
}

/// A record per handle slot: offset, condition count, then its enclosing
/// variant tests from outside in. Each test gives a tag and the required case.
/// A nested owning resource releases its own fields.
pub fn foreign_cleanup(shapes: &[Row], recipes: &[Recipe], resource: TypeRef) -> Vec<u64> {
    let mut words = Vec::new();
    if shapes.iter().any(|shape| foreign_kind(shape).is_some()) {
        paths(shapes, recipes, resource, 0, &mut Vec::new(), &mut words);
    }
    words
}

fn paths(
    shapes: &[Row],
    recipes: &[Recipe],
    row: TypeRef,
    base: u64,
    conditions: &mut Vec<[u64; 7]>,
    words: &mut Vec<u64>,
) {
    let shape = &shapes[row.index()];
    let Layout::Fixed(layout) = &recipes[row.index()].layout else {
        return;
    };
    if foreign_kind(shape).is_some() {
        words.extend([base + layout.offsets[0][0], conditions.len() as u64]);
        words.extend(conditions.iter().flatten().copied());
        return;
    }
    let mut fields = |case: usize, fields: &[crate::Field], conditions: &mut Vec<[u64; 7]>| {
        for (i, field) in fields.iter().enumerate() {
            if let FieldTy::Inline(child) = field.ty
                && shapes[child.index()].repr != Repr::Resource
            {
                paths(
                    shapes,
                    recipes,
                    child,
                    base + layout.offsets[case][i],
                    conditions,
                    words,
                );
            }
        }
    };
    match &shape.body {
        Body::Record(row) => fields(0, row, conditions),
        Body::Variant(cases) => {
            let tag = layout.tag.expect("a fixed variant has a tag");
            for (case, row) in cases.iter().enumerate() {
                let tail = match tag.encoding {
                    Encoding::Direct => [u64::MAX, 0, 0, 0],
                    Encoding::Niche {
                        dataful,
                        first,
                        last,
                        start,
                    } => [u64::from(dataful), u64::from(first), u64::from(last), start],
                };
                conditions.push([
                    base + tag.offset,
                    tag.width,
                    case as u64,
                    tail[0],
                    tail[1],
                    tail[2],
                    tail[3],
                ]);
                fields(case, &row.fields, conditions);
                conditions.pop();
            }
        }
    }
}

/// Get the greatest active ordinal below `before`; None means no previous
/// release. The supplied load respects the current resource and reads only
/// an active case once all enclosing tags match.
pub fn next_foreign_handle(
    mut words: &[u64],
    before: Option<u64>,
    mut load: impl FnMut(u64, u64) -> u64,
) -> u64 {
    let mut best = 0;
    while !words.is_empty() {
        let offset = words[0];
        let length = words[1] as usize * 7;
        let (conditions, remaining) = words[2..].split_at(length);
        let active = conditions.chunks_exact(7).all(|condition| {
            let [offset, width, case, dataful, first, last, start] = condition else {
                unreachable!()
            };
            let tag = Tag {
                offset: *offset,
                width: *width,
                encoding: if *dataful == u64::MAX {
                    Encoding::Direct
                } else {
                    Encoding::Niche {
                        dataful: *dataful as u32,
                        first: *first as u32,
                        last: *last as u32,
                        start: *start,
                    }
                },
            };
            u64::from(crate::case_of(&tag, load(*offset, *width))) == *case
        });
        if active {
            let ordinal = load(offset, 8);
            if before.is_none_or(|before| ordinal < before) {
                best = best.max(ordinal);
            }
        }
        words = remaining;
    }
    best
}
