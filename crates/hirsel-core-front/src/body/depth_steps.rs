//! MEM-30 depth steps: which rows a generated walk may visit at a depth the value
//! chooses.

use super::key_glue::{Code, Derive, KeyOp};
use super::*;
use hirsel_core_layout::{Body as ShapeBody, FieldTy, Row};

impl Derive<'_> {
    /// At a row that reaches itself, each call of its equality, order or hash walk
    /// takes one step on entry, so a bound stops a walk whose depth the value chooses
    /// before the stack does. Read-only glue holds nothing, so the failure leaves it
    /// with no cleanup.
    pub(super) fn depth_step(&self, c: &mut Code, op: KeyOp, ty: TypeId) {
        if matches!(
            op,
            KeyOp::Equal | KeyOp::Order | KeyOp::Ordered(_) | KeyOp::Fold
        ) && reaches_itself(&self.module.types, ty)
        {
            c.inst(Inst::Step { fail: None });
        }
    }
}

impl Cx<'_, '_, '_> {
    /// Where a call of key glue `op` fails to: the walk of a recursive row takes
    /// steps (map hashing reaches `Fold`), and the caller's cleanup runs.
    pub(super) fn key_fail(&mut self, op: KeyOp, n: NodeId) -> Option<BlockId> {
        let walks = matches!(
            op,
            KeyOp::Equal
                | KeyOp::Order
                | KeyOp::Ordered(_)
                | KeyOp::Hash
                | KeyOp::Place
                | KeyOp::Strong
        );
        if walks { self.fail_to(n) } else { None }
    }
}

/// Whether a value of row `ty` can hold another of `ty`, through any field.
fn reaches_itself(types: &[Row], ty: TypeId) -> bool {
    let mut seen = Vec::new();
    let mut rows = vec![ty.0];
    while let Some(id) = rows.pop() {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let mut fields: Vec<&FieldTy> = match &types[id as usize].body {
            ShapeBody::Record(fs) => fs.iter().map(|f| &f.ty).collect(),
            ShapeBody::Variant(cs) => cs
                .iter()
                .flat_map(|c| c.fields.iter().map(|f| &f.ty))
                .collect(),
        };
        while let Some(f) = fields.pop() {
            match f {
                FieldTy::Inline(t)
                | FieldTy::Box(t)
                | FieldTy::Rc(t)
                | FieldTy::Arc(t)
                | FieldTy::Shared(t)
                | FieldTy::RegionRef(t)
                | FieldTy::Link(t) => {
                    if t.0 == ty.0 {
                        return true;
                    }
                    rows.push(t.0);
                }
                FieldTy::Seq(x)
                | FieldTy::Columns(x)
                | FieldTy::Array(x, _)
                | FieldTy::View(x)
                | FieldTy::FixedSpan(x)
                | FieldTy::SeqSpan(x)
                | FieldTy::IndexedSpan(x)
                | FieldTy::Locator(x) => fields.push(x),
                FieldTy::Map(k, v) => {
                    fields.push(k);
                    fields.push(v);
                }
                _ => {}
            }
        }
    }
    false
}
