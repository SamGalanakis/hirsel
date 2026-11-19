//! A compact blob's `W` as a whole-program constant (MEM-9).
//!
//! A compact value is encoded with its blob's `W`, and a blob's `W` is written
//! once, by the root that creates it: `build_in` of a compact type writes that
//! type's declared `W`, and `build_stream_in` writes the root type's, or 8 for a
//! `Text` or sequence root. Every other blob is a copy that keeps its source's `W`
//! (`clone_in`, a region's closed copy). So an encoding of a compact type `T`
//! only ever sits in a blob whose `W` was written by a root whose type reaches
//! `T` through its rows. When a unit holds every body that can run, and every
//! root that reaches `T` writes the same `W`, a read of `T`'s `W` is that
//! constant; otherwise it stays the header word the body read.
use crate::{Body, Inst, Module, TypeId};
use hirsel_core_layout::{Body as Shaped, FieldTy, Repr};

/// Replace each [`Inst::CompactWidth`] in `module` whose type every root reaching
/// it writes with one `W` by that constant. Sound only when `module` holds every
/// body the program can run (a release unit).
pub fn resolve(module: &mut Module) {
    let bits = written(module);
    let Module {
        types, functions, ..
    } = module;
    for body in functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        for inst in &mut body.insts {
            let Inst::CompactWidth { result, ty, .. } = *inst else {
                continue;
            };
            let seen = located(types, ty)
                .into_iter()
                .fold(0u8, |seen, row| seen | bits[row]);
            if seen.count_ones() == 1 {
                *inst = Inst::Const {
                    result,
                    bits: u64::from(seen),
                };
            }
        }
    }
}

/// For each row, the `W`s (as the bits 2, 4 and 8) the roots that reach it write.
fn written(module: &Module) -> Vec<u8> {
    let types = &module.types;
    let mut bits = vec![0u8; types.len()];
    let roots = module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .flat_map(|b: &Body| b.compact_roots.iter().copied());
    for root in roots {
        let mut seen = vec![false; types.len()];
        let mut stack = vec![root.ty.index()];
        while let Some(row) = stack.pop() {
            if std::mem::replace(&mut seen[row], true) {
                continue;
            }
            bits[row] |= root.width;
            let shape = &types[row];
            let mut push = |field: &FieldTy| {
                field.clone().map_refs(&mut |r| {
                    stack.push(r.index());
                    r
                });
            };
            match &shape.body {
                Shaped::Record(fields) => fields.iter().for_each(|f| push(&f.ty)),
                Shaped::Variant(cases) => cases
                    .iter()
                    .flat_map(|c| &c.fields)
                    .for_each(|f| push(&f.ty)),
            }
            if let Some(held) = &shape.held {
                push(held);
            }
        }
    }
    bits
}
/// The compact rows a read of `ty`'s `W` locates: `ty` itself when it is a compact
/// row, else the compact rows its fields locate (a place row's `Locator`).
fn located(types: &[hirsel_core_layout::Shape], ty: TypeId) -> Vec<usize> {
    let shape = &types[ty.index()];
    if matches!(shape.repr, Repr::Compact(_)) {
        return vec![ty.index()];
    }
    let mut out = vec![];
    let fields: Vec<&FieldTy> = match &shape.body {
        Shaped::Record(fields) => fields.iter().map(|f| &f.ty).collect(),
        Shaped::Variant(cases) => cases
            .iter()
            .flat_map(|c| &c.fields)
            .map(|f| &f.ty)
            .collect(),
    };
    for field in fields {
        if let FieldTy::Locator(x) = field
            && let FieldTy::Inline(r) = &**x
        {
            out.push(r.index());
        }
    }
    out
}
