//! What a `cross` value owns as it moves between units (CONC-15, CONC-17): each
//! sequence buffer, client, socket, listener and `Shared` holder, by byte offset
//! in the value. The runtime copies sequence contents and moves client handles
//! and `Shared` holders. Sockets and listeners move with their host state,
//! detached from the poller until taken. Capability, function and `Secret`
//! words copy as bytes; their rows let a recorder write each one as its
//! snapshot or identity (docs/recording.md, "Values").
use crate::{Body, FieldTy, Handle, Layout, RecipeRow, Row, TypeRef, field_size};

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

/// Each owner of a value of row `ty`, as `[offset, kind, element bytes, element
/// logical size]`, in field order.
pub fn owners(shapes: &[Row], recipes: &[RecipeRow], ty: TypeRef) -> Vec<[u64; 4]> {
    let mut out = Vec::new();
    row(shapes, recipes, ty, 0, &mut out);
    out
}

fn row(shapes: &[Row], recipes: &[RecipeRow], ty: TypeRef, at: u64, out: &mut Vec<[u64; 4]>) {
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

fn held(shapes: &[Row], recipes: &[RecipeRow], ty: &FieldTy, at: u64, out: &mut Vec<[u64; 4]>) {
    match ty {
        FieldTy::Inline(t) => row(shapes, recipes, *t, at, out),
        FieldTy::Seq(e) => {
            let (bytes, _, logical) = field_size(shapes, recipes, e);
            out.push([at, SEQ, bytes, logical]);
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
