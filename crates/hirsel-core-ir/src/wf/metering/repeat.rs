use super::*;

// A scalar repeat has one entry from the paying block, a counted test and a
// straight-line latch. Only the proved latch edge can be omitted. Every other
// incoming edge, including a failure edge, invalidates this proof.
pub(super) fn paid_repeats(
    body: &Body,
    facts: &bulk::Facts<'_>,
    budget: &Debit<'_>,
) -> Result<Vec<bool>, Error> {
    let mut paid = Vec::new();
    budget.reserve(&mut paid, body.blocks.len())?;
    paid.resize(body.blocks.len(), false);
    for (bi, term) in body.terminators.iter().enumerate() {
        budget.check()?;
        if let Terminator::Jump(edge) = term
            && let Some(latch) = repeat(body, facts, bi, &body.edges[edge.index()], budget)?
        {
            paid[latch] = true;
        }
    }
    Ok(paid)
}

fn repeat(
    body: &Body,
    facts: &bulk::Facts<'_>,
    pre: usize,
    entry: &Edge,
    budget: &Debit<'_>,
) -> Result<Option<usize>, Error> {
    let head = entry.target.index();
    let params = |b: usize| {
        body.blocks[b]
            .params
            .get(&body.pools.values)
            .unwrap_or_default()
    };
    let instructions = |b: usize| body.blocks[b].insts.get(&body.insts).unwrap_or_default();
    let [initial] = entry.args.get(&body.pools.values).unwrap_or_default() else {
        return Ok(None);
    };
    let [index] = params(head) else {
        return Ok(None);
    };
    let [
        Inst::Compare {
            result,
            op: CompareOp::Lt,
            left,
            right: bound,
        },
    ] = instructions(head)
    else {
        return Ok(None);
    };
    let Terminator::Branch { condition, yes, no } = body.terminators[head] else {
        return Ok(None);
    };
    let latch = body.edges[yes.index()].target.index();
    if head == latch
        || pre == head
        || pre == latch
        || facts.incoming(head) != 2
        || facts.incoming(latch) != 1
        || condition != *result
        || left != index
        || body.edges[no.index()].target.index() == latch
        || body.values[index.index()] != Ty::Scalar(Scalar::U64)
        || !params(latch).is_empty()
    {
        return Ok(None);
    }
    if facts.constant(*initial) != Some(0) {
        return Ok(None);
    }
    let Terminator::Jump(back) = body.terminators[latch] else {
        return Ok(None);
    };
    let back = &body.edges[back.index()];
    let (address, next, increment, scalar_repeat) = match instructions(latch) {
        [
            Inst::Address { address, .. },
            Inst::Store { address: dest, .. },
            Inst::Binary {
                result: next,
                op: BinaryOp::Add,
                left,
                right,
                ..
            }
            | Inst::IntArith {
                result: next,
                op: ArithOp::Add,
                mode: ArithMode::Wrapping,
                left,
                right,
                ..
            },
        ] if address == dest
            && left == index
            && matches!(body.addresses[address.index()].steps.get(&body.pools.steps),Some([Step::Field(0),Step::Element(v)]) if v == index) =>
        {
            (*address, *next, *right, true)
        }
        [
            Inst::IntArith {
                result: offset,
                op: ArithOp::Mul,
                mode: ArithMode::Wrapping,
                left: pos,
                right: stride,
                ..
            },
            Inst::Address { address, .. },
            Inst::Store { address: dest, .. },
            Inst::IntArith {
                result: next,
                op: ArithOp::Add,
                mode: ArithMode::Wrapping,
                left,
                right,
                ..
            },
        ] if address == dest
            && pos == index
            && left == index
            && matches!(body.addresses[address.index()].steps.get(&body.pools.steps), Some([Step::Byte(v)]) if v==offset)
            && matches!(body.addresses[address.index()].ty,PlaceTy::Value(Ty::Scalar(s)) if facts.constant(*stride)==Some(s.size())) =>
        {
            (*address, *next, *right, false)
        }
        _ => return Ok(None),
    };
    let PlaceTy::Value(Ty::Scalar(s)) = body.addresses[address.index()].ty else {
        return Ok(None);
    };
    if facts.constant(increment) != Some(1)
        || back.target.index() != head
        || back.args.get(&body.pools.values) != Some(std::slice::from_ref(&next))
    {
        return Ok(None);
    }
    let needed = if let Some(n) = facts.constant(*bound) {
        let Some(bytes) = n.checked_mul(s.size()) else {
            return Ok(None);
        };
        bulk::Bytes::Fixed(bytes)
    } else {
        // One byte per item makes the required count exact even at u64::MAX.
        // Only entry SSA parameters are eligible; an untyped capacity read is
        // never evidence of funded storage.
        if scalar_repeat || s.size() != 1 {
            return Ok(None);
        }
        let mut parameter = false;
        for v in params(0) {
            budget.check()?;
            parameter |= v == bound;
        }
        if !parameter {
            return Ok(None);
        }
        bulk::Bytes::Value(*bound, 1)
    };
    if !facts.prepaid(
        body,
        pre,
        instructions(pre).len(),
        needed,
        hirsel_core_ids::work_steps::table::COPY,
        budget,
    )? {
        if scalar_repeat {
            return Err(Error::Invalid(format!(
                "b{head}: scalar repeat lacks matching MEM-30 precharge"
            )));
        }
        return Ok(None);
    }
    Ok(Some(latch))
}
