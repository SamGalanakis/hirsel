//! PACK-12 fused scope rows: the first row of a group makes one runtime call
//! for the whole group, and the group's later rows take their results from it.
use super::{function::Function, *};
use cranelift_codegen::ir::{StackSlot, StackSlotData, StackSlotKind, Value, condcodes::IntCC};
use hirsel_core_ir::contain::{Operand, Plan};
use std::collections::HashMap;

const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);

/// One function's fused scope rows and what lowering them has made so far.
pub(super) struct State {
    plan: Plan,
    /// The instruction being lowered, as its index in `Body::insts`.
    pub row: usize,
    /// By a fused enter's first row: the enters made and the byte bound's room.
    entered: HashMap<usize, (Value, Value)>,
    /// The leaves a fused leave has made ahead of their own rows.
    leave_credit: Option<StackSlot>,
}

impl State {
    pub fn new(body: &Body, observe_charge: bool) -> Self {
        // An empty bound makes no runtime call, so it has nothing to fuse.
        let plan = if !observe_charge && super::fast::empty_bound(body) {
            Plan::default()
        } else {
            hirsel_core_ir::contain::plan(body, &|_| false, &|_| false)
        };
        Self {
            plan,
            row: 0,
            entered: HashMap::new(),
            leave_credit: None,
        }
    }
}

impl Function<'_, '_, '_> {
    /// The leave credit starts at zero: no row has left ahead of another.
    pub fn open_leave_credit(&mut self) {
        if self.contain.plan.left.is_empty() {
            return;
        }
        let slot =
            self.b
                .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().stack_store(types::I64, zero, slot, 0);
        self.contain.leave_credit = Some(slot);
    }
    /// For a fused enter's later row: what its first row made.
    pub fn entered_by_first(&self) -> Option<(Value, Value)> {
        let first = self.contain.plan.entered.get(&self.contain.row)?;
        Some(self.contain.entered[first])
    }
    pub fn contain_operand(&mut self, operand: Operand) -> Value {
        match operand {
            Operand::Value(value) => self.get(value),
            Operand::Const(bits) => self.b.ins().iconst(types::I64, bits as i64),
        }
    }
    /// Make both enters of the group this row starts. `step_row` is the step
    /// bound's row, whose source position the runtime reports.
    pub fn contain_enter(
        &mut self,
        order: i64,
        bytes: Value,
        steps: Value,
        step_row: usize,
    ) -> Result<(Value, Value)> {
        let anchor = self.anchor;
        self.anchor = self.body.inst_anchors.get(step_row).copied().flatten();
        let site = self.site();
        self.anchor = anchor;
        let order = self.b.ins().iconst(types::I64, order);
        let values = self.c_call(
            "hirsel_core_contain_enter",
            &[P, U, U, U, U],
            &[U, U],
            &[self.context, order, bytes, steps, site?],
        )?;
        let made = (values[0], values[1]);
        self.contain.entered.insert(self.contain.row, made);
        Ok(made)
    }
    /// Lower a byte bound's enter that a fused enter makes, or say it is not one.
    pub fn fused_limit_enter(
        &mut self,
        admitted: ValueId,
        available: ValueId,
        bytes: Value,
    ) -> Result<bool> {
        let (ok, room) = if let Some((made, room)) = self.entered_by_first() {
            (self.b.ins().icmp_imm_s(IntCC::Equal, made, 2), room)
        } else if let Some(enter) = self.contain.plan.enters.get(&self.contain.row).copied() {
            let steps = self.contain_operand(enter.operand);
            let (made, room) = self.contain_enter(0, bytes, steps, enter.second)?;
            (self.b.ins().icmp_imm_s(IntCC::NotEqual, made, 0), room)
        } else {
            return Ok(false);
        };
        self.set(admitted, ok);
        self.set(available, room);
        Ok(true)
    }
    /// The same for a step bound's enter.
    pub fn fused_step_enter(&mut self, call: &CallSite, steps: Value) -> Result<bool> {
        let ok = if let Some((made, _)) = self.entered_by_first() {
            self.b.ins().icmp_imm_s(IntCC::Equal, made, 2)
        } else if let Some(enter) = self.contain.plan.enters.get(&self.contain.row).copied() {
            let bytes = self.contain_operand(enter.operand);
            let (made, _) = self.contain_enter(1, bytes, steps, self.contain.row)?;
            self.b.ins().icmp_imm_s(IntCC::NotEqual, made, 0)
        } else {
            return Ok(false);
        };
        let &[admitted] = call.results.get(&self.body.pools.values).unwrap() else {
            unreachable!("a step enter has one result")
        };
        self.set(admitted, ok);
        Ok(true)
    }
    /// Lower a leave row whose own runtime entry is `name`. A group's first
    /// row leaves for every row of the group and records how many; each later
    /// row then spends one, and calls for itself only when reached another way.
    pub fn leave(&mut self, name: &'static str) -> Result<()> {
        if let Some(&layers) = self.contain.plan.leaves.get(&self.contain.row) {
            let slot = self.contain.leave_credit.unwrap();
            let word = self.b.ins().iconst(types::I64, layers as i64);
            self.c_call(
                "hirsel_core_contain_leave",
                &[P, U],
                &[],
                &[self.context, word],
            )?;
            let covered = self
                .b
                .ins()
                .iconst(types::I64, Plan::covered(layers) as i64);
            self.b.ins().stack_store(types::I64, covered, slot, 0);
            return Ok(());
        }
        if !self.contain.plan.left.contains(&self.contain.row) {
            self.c_call(name, &[P], &[], &[self.context])?;
            return Ok(());
        }
        let slot = self.contain.leave_credit.unwrap();
        let credit = self.b.ins().stack_load(types::I64, types::I64, slot, 0);
        let (covered, own, done) = (
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
        );
        self.b.ins().brif(credit, covered, &[], own, &[]);
        self.b.switch_to_block(covered);
        let rest = self.b.ins().iadd_imm_s(credit, -1);
        self.b.ins().stack_store(types::I64, rest, slot, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(own);
        self.c_call(name, &[P], &[], &[self.context])?;
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }
}
