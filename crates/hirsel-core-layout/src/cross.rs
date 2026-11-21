//! What a `cross` value owns as it moves between units (CONC-15, CONC-17): each
//! sequence buffer and each client handle, by byte offset in the value. The
//! runtime copies a buffer's content into the receiving unit and moves a client;
//! the checker refuses every other owner in a `cross` type.
use crate::{Body, FieldTy, Handle, Layout, Recipe, Shape, TypeRef, field_size};

/// A sequence carrier (pointer, length, capacity).
pub const SEQ: u64 = 0;
/// A `Client[T]` handle word.
pub const CLIENT: u64 = 1;

/// Each owner of a value of row `ty`, as `[offset, kind, element bytes, element
/// logical size]`, in field order.
pub fn owners(shapes: &[Shape], recipes: &[Recipe], ty: TypeRef) -> Vec<[u64; 4]> {
    let mut out = Vec::new();
    row(shapes, recipes, ty, 0, &mut out);
    out
}

fn row(shapes: &[Shape], recipes: &[Recipe], ty: TypeRef, at: u64, out: &mut Vec<[u64; 4]>) {
    // A variant is cross only when plain, so it owns nothing.
    let (Body::Record(fields), Layout::Fixed(fixed)) =
        (&shapes[ty.index()].body, &recipes[ty.index()].layout)
    else {
        return;
    };
    for (field, offset) in fields.iter().zip(&fixed.offsets[0]) {
        held(shapes, recipes, &field.ty, at + offset, out);
    }
}

fn held(shapes: &[Shape], recipes: &[Recipe], ty: &FieldTy, at: u64, out: &mut Vec<[u64; 4]>) {
    match ty {
        FieldTy::Inline(t) => row(shapes, recipes, *t, at, out),
        FieldTy::Seq(e) => {
            let (bytes, _, logical) = field_size(shapes, recipes, e);
            out.push([at, SEQ, bytes, logical]);
        }
        FieldTy::Handle(Handle::Client) => out.push([at, CLIENT, 0, 0]),
        FieldTy::Array(e, n) => {
            let (stride, _, _) = field_size(shapes, recipes, e);
            for i in 0..*n {
                held(shapes, recipes, e, at + i * stride, out);
            }
        }
        _ => {}
    }
}
