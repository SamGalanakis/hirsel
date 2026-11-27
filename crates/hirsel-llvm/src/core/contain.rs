//! PACK-12 fused scope rows: the first row of a group makes one runtime call
//! for the whole group, and the group's later rows take their results from it.
//!
//! A body's blocks can be emitted more than once (a step loop's or region's
//! copies), so what a first row made is kept in three entry allocas, which
//! LLVM promotes to registers.
use super::*;
use hirsel_core_ir::contain::{Layer, Operand, Plan};

const MADE: Local = Local::Named("hirsel.contain.made");
const ROOM: Local = Local::Named("hirsel.contain.room");
const CREDIT: Local = Local::Named("hirsel.contain.credit");

impl FunctionEmitter<'_> {
    /// Plan the body's fused rows. A byte row inside a step loop or region,
    /// which pay steps ahead and refund on the way out, starts no group: the
    /// refund must reach the step bound current when the steps were paid.
    pub(super) fn open_contain(&mut self, loops: &[steps::Loop], regions: &[steps::Region]) {
        let body = self.body;
        let mut blocks = vec![0; body.insts.len()];
        for (block, row) in body.blocks.iter().enumerate() {
            let start = row.insts.start as usize;
            blocks[start..start + row.insts.len as usize].fill(block);
        }
        let bounds = &self.empty_bounds;
        self.contain = hirsel_core_ir::contain::plan(
            body,
            &|row| match body.insts[row] {
                Inst::LimitEnter { admitted, .. } => bounds.enters.contains(&admitted),
                Inst::LimitExit => bounds.exits.contains(&row),
                _ => false,
            },
            &|row| {
                loops.iter().any(|plan| plan.covers(blocks[row]))
                    || regions.iter().any(|plan| plan.covers(blocks[row]))
            },
        );
        if !self.contain.enters.is_empty() {
            self.sink.alloca(MADE, &Type::I64, 8);
            self.sink.alloca(ROOM, &Type::I64, 8);
        }
        if !self.contain.left.is_empty() {
            self.sink.alloca(CREDIT, &Type::I64, 8);
            // No row has left ahead of another.
            self.store(&Type::I64, &int(0), &CREDIT.into(), 8);
        }
    }
    /// For a fused enter's later row: what its first row made.
    pub(super) fn entered_by_first(&mut self) -> (Value, Value) {
        (
            self.load(&Type::I64, &MADE.into(), 8),
            self.load(&Type::I64, &ROOM.into(), 8),
        )
    }
    /// A row's `admitted` result from the count of enters made.
    pub(super) fn made(&mut self, test: Predicate, made: &Value, count: u64) -> Value {
        let yes = self.cmp(test, &Type::I64, made, &int(count));
        self.zext(&Type::I1, &yes, &Type::I8)
    }
    pub(super) fn contain_operand(&mut self, operand: Operand) -> Value {
        match operand {
            Operand::Value(value) => self.numeric(value),
            Operand::Const(bits) => int(bits),
        }
    }
    /// Make both enters of the group this row starts. `step_row` is the step
    /// bound's row, whose source position the runtime reports.
    pub(super) fn contain_enter(
        &mut self,
        order: u64,
        bytes: &Value,
        steps: &Value,
        step_row: usize,
    ) -> (Value, Value) {
        let anchor = std::mem::replace(&mut self.anchor, self.body.inst_anchors[step_row]);
        let site = self.site_word();
        self.anchor = anchor;
        // The call opens a step bound whichever row makes it.
        self.neutral_step_call = false;
        let values = self.native_call(
            "hirsel_core_contain_enter",
            &[Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)],
            &[
                ptr(CONTEXT),
                word(int(order)),
                word(bytes.clone()),
                word(steps.clone()),
                word(site),
            ],
        );
        self.store(&Type::I64, &values[0], &MADE.into(), 8);
        self.store(&Type::I64, &values[1], &ROOM.into(), 8);
        (values[0].clone(), values[1].clone())
    }
    /// Lower a leave row whose own runtime entry is `name`. A group's first
    /// row leaves for every row of the group and records how many; each later
    /// row then spends one, and calls for itself only when reached another way.
    pub(super) fn leave(&mut self, name: &str) {
        if let Some(&layers) = self.contain.leaves.get(&self.row) {
            let steps = (0..16).any(|n| (layers >> (4 * n)) & 15 == Layer::Steps as u64);
            self.neutral_step_call &= !steps;
            self.runtime_void(
                "hirsel_core_contain_leave",
                &[ptr(CONTEXT), word(int(layers))],
            );
            self.store(&Type::I64, &int(Plan::covered(layers)), &CREDIT.into(), 8);
            return;
        }
        if !self.contain.left.contains(&self.row) {
            self.runtime_void(name, &[ptr(CONTEXT)]);
            return;
        }
        let credit = self.load(&Type::I64, &CREDIT.into(), 8);
        let covered = self.cmp(Predicate::Ne, &Type::I64, &credit, &int(0));
        let n = self.counter();
        let (spent, own, done) = (
            Label::new("leave_covered", n),
            Label::new("leave_own", n),
            Label::new("leave_done", n),
        );
        self.cond(&covered, spent, own);
        self.block(spent);
        let rest = self.bin(Binary::Sub, &Type::I64, &credit, &int(1));
        self.store(&Type::I64, &rest, &CREDIT.into(), 8);
        self.br(done);
        self.block(own);
        self.runtime_void(name, &[ptr(CONTEXT)]);
        self.br(done);
        self.block(done);
    }
}
