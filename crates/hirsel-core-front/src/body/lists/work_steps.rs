//! Emit the MEM-30 work charge before the operation it covers.
use super::*;
use hirsel_core_ids::work_steps::Work;
use hirsel_core_ir::RtOp;

impl Cx<'_, '_, '_> {
    pub(in crate::body) fn complete_iteration(&mut self, n: NodeId) {
        self.source_step(n);
        if self.steps_in_source {
            self.inst(Inst::LoopIteration, n);
        }
    }
    pub(in crate::body) fn charge_work(
        &mut self,
        work: Work,
        units: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let shift = self.konst(u64_, u64::from(work.shift), n)?;
        let weight = self.konst(u64_, work.weight, n)?;
        self.runtime_row(
            RtOp::WorkSteps,
            &[Arg::Value(units), Arg::Value(shift), Arg::Value(weight)],
            n,
        )?;
        Some(())
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
}
