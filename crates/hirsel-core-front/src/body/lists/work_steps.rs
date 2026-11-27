//! MEM-30's work steps (docs/spec/budgets.md, "Work steps"): the one emitter of the
//! charge that `hirsel_core_ids::work_steps` defines, so every engine counts it alike.
use super::*;
use crate::body::key_glue::Code;
use hirsel_core_ids::work_steps::Work;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// MEM-30 work charge: `work`'s weight and one step for each of its quanta,
    /// or part of one, of `units`, taken before any work. `units` counts a live
    /// range (bytes, or elements or leaves of a held sequence), so it fits a
    /// word, and with every quantum at least 4 the count cannot wrap.
    pub(in crate::body) fn charge_work(
        &mut self,
        work: Work,
        units: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let shift = self.konst(u64_, u64::from(work.shift), n)?;
        let mask = self.konst(u64_, work.quantum() - 1, n)?;
        let whole = self.value(u64_);
        let fail = self.fail_to(n);
        self.inst(
            Inst::Binary {
                result: whole,
                op: BinaryOp::Shr,
                left: units,
                right: shift,
                fail,
            },
            n,
        );
        let part = self.value(u64_);
        self.inst(
            Inst::Binary {
                result: part,
                op: BinaryOp::BitAnd,
                left: units,
                right: mask,
                fail: None,
            },
            n,
        );
        // `part + mask` is below two quanta, so it shifts to 1 exactly when `part` is not 0.
        let (padded, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, part, mask, n);
        let up = self.value(u64_);
        let fail = self.fail_to(n);
        self.inst(
            Inst::Binary {
                result: up,
                op: BinaryOp::Shr,
                left: padded,
                right: shift,
                fail,
            },
            n,
        );
        let (bulk, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, whole, up, n);
        let weight = self.konst(u64_, work.weight, n)?;
        let (count, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, bulk, weight, n);
        self.take_steps(count, n)
    }

    /// `charge_work` for the bytes of `count` elements of `item`, each counted
    /// at its logical size (MEM-24), so every engine charges the same count
    /// whatever layout it holds. `count` elements are a live range's.
    pub(in crate::body) fn charge_elements(
        &mut self,
        work: Work,
        item: TyId,
        count: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let size = self.layout_size(item, true, n)?;
        let (units, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, count, size, n);
        self.charge_work(work, units, n)
    }

    /// Takes `count` steps, one MEM-30 step instruction at a time.
    fn take_steps(&mut self, count: ValueId, n: NodeId) -> Option<()> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let zero = self.konst(u64_, 0, n)?;
        let one = self.konst(u64_, 1, n)?;
        let head = self.new_block(&[u64_]);
        let step = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.jump(head, &[zero], n);
        self.start(head);
        let taken = self.block_params(head)[0];
        let more = self.list_compare(CompareOp::Lt, taken, count, n);
        self.list_branch(more, step, done, n);
        self.start(step);
        let fail = self.fail_to(n);
        self.inst(Inst::Step { fail }, n);
        let (next, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, taken, one, n);
        self.jump(head, &[next], n);
        self.start(done);
        Some(())
    }
}

/// `charge_work` in derived glue: the same count, each step failing to the
/// glue's cleanup `fail`.
pub(in crate::body) fn take_work(c: &mut Code, work: Work, units: ValueId, fail: BlockId) {
    let shift = c.constant(u64::from(work.shift));
    let mask = c.constant(work.quantum() - 1);
    let whole = c.binary(BinaryOp::Shr, units, shift);
    let part = c.binary(BinaryOp::BitAnd, units, mask);
    let padded = c.binary(BinaryOp::Add, part, mask);
    let up = c.binary(BinaryOp::Shr, padded, shift);
    let bulk = c.binary(BinaryOp::Add, whole, up);
    let weight = c.constant(work.weight);
    let count = c.binary(BinaryOp::Add, bulk, weight);
    let zero = c.constant(0);
    let one = c.constant(1);
    let head = c.block(&[Ty::Scalar(Scalar::U64)]);
    let (step, done) = (c.block(&[]), c.block(&[]));
    c.jump(head, &[zero]);
    c.start(head);
    let taken = c.params(head)[0];
    let more = c.compare(CompareOp::Lt, taken, count);
    c.branch(more, step, done);
    c.start(step);
    c.inst(Inst::Step { fail: Some(fail) });
    let next = c.binary(BinaryOp::Add, taken, one);
    c.jump(head, &[next]);
    c.start(done);
}
