//! Bound a slot cursor by the units of a successful precharge. Only the
//! verified jump back to the header leaves the unmetered graph. In particular
//! a zero-unit payment never excuses a cycle through the exit.
use super::*;
mod drain;
use hirsel_core_ids::work_steps::table;

fn rows(body: &Body, block: usize) -> &[Inst] {
    body.blocks[block]
        .insts
        .get(&body.insts)
        .unwrap_or_default()
}

struct Cursor {
    address: AddressId,
    slot: SlotId,
    index: ValueId,
    bound: ValueId,
    head: usize,
    yes: usize,
    no: usize,
}

impl Cursor {
    fn header(b: &Body, head: usize) -> Option<Self> {
        let [
            Inst::Read {
                result: index,
                address,
                mode: ReadMode::Bits,
            },
            Inst::Compare {
                result,
                op: CompareOp::Lt,
                left,
                right: bound,
            },
        ] = rows(b, head)
        else {
            return None;
        };
        let Terminator::Branch { condition, yes, no } = b.terminators[head] else {
            return None;
        };
        let place = &b.addresses[address.index()];
        let AddressBase::Slot(slot) = place.base else {
            return None;
        };
        let u64_place = PlaceTy::Value(Ty::Scalar(Scalar::U64));
        if result != &condition
            || index != left
            || place.steps.len != 0
            || place.ty != u64_place
            || b.slots[slot.index()] != u64_place
            || b.blocks[head].params.len != 0
        {
            return None;
        }
        Some(Self {
            address: *address,
            slot,
            index: *index,
            bound: *bound,
            head,
            yes: b.edges[yes.index()].target.index(),
            no: b.edges[no.index()].target.index(),
        })
    }

    // One direct address for this slot, never projected, borrowed or exported.
    fn private(&self, b: &Body, budget: &Debit<'_>) -> Result<bool, Error> {
        for (i, a) in b.addresses.iter().enumerate() {
            budget.check()?;
            if i != self.address.index() && a.base == AddressBase::Slot(self.slot)
                || a.base == AddressBase::Address(self.address)
            {
                return Ok(false);
            }
        }
        let name = b.values.len() + self.address.index();
        for inst in &b.insts {
            budget.check()?;
            if matches!(
                inst,
                Inst::Read { .. } | Inst::Store { .. } | Inst::Address { .. }
            ) {
                continue;
            }
            let mut used = false;
            crate::extent::visit_operands(b, inst, |v| used |= v == name, |_| {});
            if used {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn precharged(
        &self,
        b: &Body,
        f: &bulk::Facts<'_>,
        pre: usize,
        budget: &Debit<'_>,
    ) -> Result<bool, Error> {
        let instructions = rows(b, pre);
        let mut initial = None;
        let mut end = None;
        let mut payment = None;
        for (i, inst) in instructions.iter().enumerate() {
            budget.check()?;
            match *inst {
                Inst::Store { address, value } if address == self.address => initial = Some(value),
                Inst::StorageLive { slot } if slot == self.slot => initial = None,
                Inst::Binary {
                    result,
                    op: BinaryOp::Add,
                    left,
                    right,
                    ..
                }
                | Inst::IntArith {
                    result,
                    op: ArithOp::Add,
                    mode: ArithMode::Wrapping,
                    left,
                    right,
                    ..
                } if result == self.bound => end = Some((left, right)),
                Inst::Call { call } => {
                    let call = &b.calls[call.index()];
                    payment = None;
                    if call.callee != Callee::Runtime(RtOp::WorkSteps) {
                        initial = None;
                        continue;
                    }
                    if let Some([Arg::Value(n), Arg::Value(shift), Arg::Value(weight)]) =
                        call.args.get(&b.pools.args)
                        && f.constant(*weight) == Some(0)
                        && let Some(work) =
                            [table::ASCII_CASE, table::UTF8, table::WALK, table::COPY]
                                .into_iter()
                                .find(|row| f.constant(*shift) == Some(row.shift.into()))
                    {
                        payment = Some((i, *n, work));
                    }
                }
                // Moving a fixed-size carrier may prepare an output. A COPY
                // payment spent on this transfer is no longer available.
                Inst::Copy { bytes, .. }
                    if matches!(
                        f.definition(bytes),
                        Some(Inst::LayoutConst {
                            query: LayoutQuery::HeldSize(_),
                            ..
                        })
                    ) =>
                {
                    if payment.is_some_and(|(_, _, row)| row == table::COPY) {
                        payment = None;
                    }
                    initial = None;
                }
                Inst::Store { .. } => initial = None,
                Inst::Address { .. }
                | Inst::Read {
                    mode: ReadMode::Bits,
                    ..
                }
                | Inst::Pointer { .. }
                | Inst::Const { .. }
                | Inst::LayoutConst { .. }
                | Inst::Binary { .. }
                | Inst::IntArith { .. }
                | Inst::StorageLive { .. }
                | Inst::BuiltinUse { .. } => {}
                _ => {
                    payment = None;
                    initial = None;
                }
            }
        }
        let Some((at, units, _)) = payment else {
            return Ok(false);
        };
        let covers = |v| v == units || f.constant(v).is_some_and(|k| f.constant(units) == Some(k));
        // A wrapped end lies below its initial cursor and runs no iterations.
        let count = end
            .filter(|(base, _)| Some(*base) == initial)
            .map(|(_, count)| count);
        if covers(self.bound) || count.is_some_and(covers) {
            return Ok(true);
        }
        // The multiplication and payment must see the same activation of the
        // count. A product retained from a previous traversal cannot pay here.
        for inst in &instructions[..at] {
            budget.check()?;
            if let Inst::IntArith {
                result,
                op: ArithOp::Mul,
                mode: ArithMode::Saturating,
                left,
                right,
                ..
            } = *inst
                && result == units
                && (left == self.bound || Some(left) == count)
                && f.constant(right).is_some_and(|k| k > 0)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn advance(
        &self,
        b: &Body,
        f: &bulk::Facts<'_>,
        instructions: &[Inst],
        budget: &Debit<'_>,
    ) -> Result<bool, Error> {
        let Some((Inst::Store { address, value }, before)) = instructions.split_last() else {
            return Ok(false);
        };
        if *address != self.address {
            return Ok(false);
        }
        // Either use the comparison's index or capture it on the true edge
        // before any instruction can write. Later reads cannot prove progress.
        let compared = |v| {
            v == self.index
                || matches!(rows(b, self.yes).first(),
            Some(Inst::Read {result, address, mode: ReadMode::Bits}) if *result == v && *address == self.address)
        };
        let mut next = *value;
        let mut advanced = false;
        for row in before.iter().rev() {
            budget.check()?;
            match *row {
                Inst::Binary {
                    op: BinaryOp::Add,
                    result,
                    left,
                    right,
                    ..
                } if result == next => {
                    // Checked unsigned sums either fail or preserve progress.
                    advanced |= f.constant(right).is_some_and(|k| k > 0);
                    next = left;
                }
                Inst::IntArith {
                    op: ArithOp::Add,
                    mode: ArithMode::Wrapping,
                    result,
                    left,
                    right,
                    ..
                } if result == next => {
                    // The head bounds the compared value, not an intermediate sum.
                    return Ok(compared(left)
                        && f.constant(right).is_some_and(|k| {
                            k > 0
                                && (k == 1
                                    || f.constant(self.bound)
                                        .is_some_and(|end| end <= u64::MAX - (k - 1)))
                        }));
                }
                _ => {}
            }
        }
        Ok(advanced && compared(next))
    }

    fn prove(
        &self,
        b: &Body,
        f: &bulk::Facts<'_>,
        pre: usize,
        paid: &mut [bool],
        budget: &Debit<'_>,
    ) -> Result<(), Error> {
        if f.incoming(self.yes) != 1
            || self.yes == self.no
            || self.head == self.no
            || !self.precharged(b, f, pre, budget)?
            || !self.private(b, budget)?
        {
            return Ok(());
        }
        let mut work = Vec::new();
        let mut incoming = Vec::new();
        budget.reserve(&mut incoming, b.blocks.len())?;
        incoming.resize(b.blocks.len(), 0usize);
        let mut latches = Vec::new();
        budget.reserve(&mut work, 1)?;
        work.push(self.yes);
        while let Some(block) = work.pop() {
            budget.check()?;
            if block == self.no {
                continue;
            }
            if block == self.head || block == pre || block == 0 {
                return Ok(());
            }
            let rows = rows(b, block);
            incoming[block] += 1;
            if incoming[block] > 1 {
                continue;
            }
            let mut back = false;
            match b.terminators[block] {
                Terminator::Jump(e) if b.edges[e.index()].target.index() == self.head => {
                    back = true
                }
                Terminator::Jump(e) => {
                    budget.reserve(&mut work, 1)?;
                    work.push(b.edges[e.index()].target.index());
                }
                Terminator::Branch { yes, no, .. } => {
                    budget.reserve(&mut work, 2)?;
                    work.extend([yes, no].map(|e| b.edges[e.index()].target.index()));
                }
                Terminator::Raise { .. }
                | Terminator::Propagate
                | Terminator::Return(_)
                | Terminator::Unreachable => continue,
                _ => return Ok(()),
            }
            if back {
                if !self.advance(b, f, rows, budget)? {
                    return Ok(());
                }
                budget.reserve(&mut latches, 1)?;
                latches.push(block);
            }
            for inst in &rows[..rows.len() - usize::from(back)] {
                budget.check()?;
                if matches!(*inst, Inst::Store { address, .. } if address == self.address)
                    || matches!(*inst, Inst::StorageLive {slot} | Inst::StorageDead {slot} if slot == self.slot)
                    || matches!(*inst, Inst::Read {address, mode: ReadMode::Take, ..} if address == self.address)
                {
                    return Ok(());
                }
            }
        }
        if f.incoming(self.head) != latches.len() + 1 {
            return Ok(());
        }
        for (block, count) in incoming.into_iter().enumerate() {
            budget.check()?;
            if count != 0 && f.incoming(block) != count {
                return Ok(());
            }
        }
        for block in latches {
            paid[block] = true;
        }
        Ok(())
    }
}

pub(super) fn paid_latches(
    m: &Module,
    layouts: &mut repeat::Layouts<'_, '_>,
    b: &Body,
    facts: &bulk::Facts<'_>,
    paid: &mut [bool],
    budget: &Debit<'_>,
) -> Result<(), Error> {
    for (pre, term) in b.terminators.iter().enumerate() {
        budget.check()?;
        if let Terminator::Jump(e) = term
            && let Some(cursor) = Cursor::header(b, b.edges[e.index()].target.index())
        {
            let local = budget.scratch();
            if drain::paid(m, b, facts, layouts, &cursor, pre, &local)? {
                paid[cursor.yes] = true;
            } else {
                cursor.prove(b, facts, pre, paid, &local)?;
            }
        }
    }
    Ok(())
}
