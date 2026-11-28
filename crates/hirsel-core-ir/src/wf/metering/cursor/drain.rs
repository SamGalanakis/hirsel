//! A local sequence allocation with a paid, nonwrapping size. Its success
//! arm initializes the buffer, and a failure transfers its elements to one
//! drain. The SSA allocation result identifies the storage throughout.
use super::*;
use hirsel_core_layout::FieldTy;

pub(super) fn paid(
    m: &Module,
    b: &Body,
    f: &bulk::Facts<'_>,
    layouts: &mut repeat::Layouts<'_, '_>,
    c: &Cursor,
    pre: usize,
    budget: &Debit<'_>,
) -> Result<bool, Error> {
    let [
        Inst::Address {
            address,
            fail: None,
        },
        Inst::Const {
            result: one,
            bits: 1,
        },
        Inst::IntArith {
            result: next,
            op: ArithOp::Add,
            mode: ArithMode::Wrapping,
            left,
            right,
            ..
        },
        Inst::Store {
            address: slot,
            value,
        },
        Inst::Drop {
            address: dropped,
            ty,
            fail: None,
        },
    ] = rows(b, c.yes)
    else {
        return Ok(false);
    };
    let place = &b.addresses[address.index()];
    let AddressBase::Value(data) = place.base else {
        return Ok(false);
    };
    // A loaded carrier word has no allocation provenance.
    if f.definition(data).is_some()
        || address != dropped
        || slot != &c.address
        || left != &c.index
        || right != one
        || value != next
        || place.steps.get(&b.pools.steps) != Some(&[Step::Index(c.index)][..])
        || place.ty != PlaceTy::Declared(*ty)
        || f.incoming(c.yes) != 1
        || f.incoming(c.head) != 2
        || f.incoming(pre) != 1
        || !rows(b, pre).is_empty()
        || !matches!(b.terminators[c.yes], Terminator::Jump(e) if b.edges[e.index()].target.index() == c.head)
        || !matches!(b.terminators[c.no], Terminator::Propagate)
    {
        return Ok(false);
    }
    for (success, term) in b.terminators.iter().enumerate() {
        budget.check()?;
        let init = rows(b, success);
        // Initialization precedes the failure. A failed move or charge cannot
        // enter this cleanup with an uninitialized suffix.
        let (before, transferred) = match (term, init) {
            (
                Terminator::Raise {
                    cleanup: Some(t), ..
                },
                [before @ .., transfer],
            ) if t.index() == pre => (before, transfer),
            (_, [before @ .., transfer, Inst::Step { fail: Some(t) }]) if t.index() == pre => {
                (before, transfer)
            }
            _ => continue,
        };
        let Inst::Move { to, bytes, .. } = *transferred else {
            return Ok(false);
        };
        let dst = &b.addresses[to.index()];
        if dst.base != AddressBase::Value(data) || dst.steps.len != 0 || dst.ty != place.ty {
            return Ok(false);
        }
        for row in before {
            budget.check()?;
            match row {
                Inst::Const { .. } | Inst::Address { .. } => {}
                Inst::Call { call }
                    if b.calls[call.index()].callee == Callee::Runtime(RtOp::WorkSteps) => {}
                _ => return Ok(false),
            }
        }
        let Some(alloc) = f.predecessor(success) else {
            return Ok(false);
        };
        let Terminator::Branch { condition, yes, no } = b.terminators[alloc] else {
            return Ok(false);
        };
        let [
            prefix @ ..,
            Inst::Alloc {
                result,
                size,
                charge,
                kind: AllocKind::Own { edge },
                ..
            },
            Inst::Compare {
                result: ok,
                op,
                left: pointer,
                right: null,
            },
        ] = rows(b, alloc)
        else {
            return Ok(false);
        };
        let nonnull = match op {
            CompareOp::Ne => yes,
            CompareOp::Eq => no,
            _ => return Ok(false),
        };
        if *result != data
            || *size != bytes
            || *pointer != data
            || *ok != condition
            || f.constant(*null) != Some(0)
            || b.edges[nonnull.index()].target.index() != success
            || !matches!(edge.as_ref(), FieldTy::Seq(item) if matches!(item.as_ref(), FieldTy::Inline(t) if t.0 == ty.0))
            || !matches!(rows(b,c.no), [Inst::Free {block, charge: refund}] if *block == data && refund == charge)
            || layouts.logical(m, *ty)?.unwrap_or(0) == 0
        {
            return Ok(false);
        }
        let mut sized = false;
        for row in prefix {
            budget.check()?;
            if let Inst::Binary {
                result,
                op: BinaryOp::Mul,
                left,
                right,
                ..
            } = *row
                && result == bytes
                && left == c.bound
                && matches!(f.definition(right), Some(Inst::LayoutConst {query: LayoutQuery::HeldSize(t), ..}) if t == ty)
            {
                sized = true;
            }
        }
        return Ok(sized
            && f.prepaid(
                b,
                alloc,
                prefix.len(),
                f.bytes(bytes),
                None,
                table::COPY,
                budget,
            )?
            && c.private(b, budget)?);
    }
    Ok(false)
}
