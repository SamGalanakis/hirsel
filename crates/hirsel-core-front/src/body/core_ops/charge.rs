use super::*;
use hirsel_core_layout::charge::{
    CAUSE_CELL, CHANNEL_BASE, COUNTED_EXTRA, FLOW_PORT_BASE, FLOW_SLOT_EXTRA, LATEST_ROOT,
    MAP_ENTRY_EXTRA, ROUND_MASK, SHARED_NODE, TASK_RECORD,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Price {
    Round,
    Counted,
    Shared,
    Seq,
    Map,
    Channel,
    Task,
    Block,
    Port,
    Latest,
}

impl Cx<'_, '_, '_> {
    pub(super) fn charge_value(
        &mut self,
        price: Price,
        args: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let word = Ty::Scalar(Scalar::U64);
        let bool_ = Ty::Scalar(Scalar::Bool);
        let mut overflow = None;
        let mut checked = |cx: &mut Self, op, left, right| {
            let result = cx.value(word);
            let flag = cx.value(bool_);
            cx.inst(
                Inst::IntArith {
                    result,
                    flag: Some(flag),
                    op,
                    mode: ArithMode::Checked,
                    left,
                    right,
                },
                n,
            );
            overflow = Some(match overflow {
                Some(prior) => cx.charge_bitwise(bool_, BinaryOp::BitOr, prior, flag, n),
                None => flag,
            });
            result
        };
        let first = || args.first().copied().flatten();
        let (value, round) = match price {
            Price::Round => (first()?, true),
            Price::Counted | Price::Shared => {
                let header = if price == Price::Counted {
                    COUNTED_EXTRA
                } else {
                    SHARED_NODE
                };
                let header = self.konst(word, header, n)?;
                (checked(self, ArithOp::Add, first()?, header), true)
            }
            Price::Seq => (checked(self, ArithOp::Mul, first()?, args[1]?), true),
            Price::Map => {
                let extra = self.konst(word, MAP_ENTRY_EXTRA, n)?;
                let pair = checked(self, ArithOp::Add, first()?, args[1]?);
                let entry = checked(self, ArithOp::Add, pair, extra);
                (checked(self, ArithOp::Mul, entry, args[2]?), true)
            }
            Price::Channel | Price::Port => {
                let (item, base) = if price == Price::Port {
                    let extra = self.konst(word, FLOW_SLOT_EXTRA, n)?;
                    (checked(self, ArithOp::Add, first()?, extra), FLOW_PORT_BASE)
                } else {
                    (first()?, CHANNEL_BASE)
                };
                let base = self.konst(word, base, n)?;
                let slots = checked(self, ArithOp::Mul, item, args[1]?);
                (checked(self, ArithOp::Add, slots, base), false)
            }
            Price::Task | Price::Block => {
                let header = if price == Price::Task {
                    TASK_RECORD + CAUSE_CELL
                } else {
                    CAUSE_CELL
                };
                let header = self.konst(word, header, n)?;
                let fixed = checked(self, ArithOp::Add, first()?, header);
                (checked(self, ArithOp::Add, fixed, args[1]?), false)
            }
            Price::Latest => (self.konst(word, LATEST_ROOT, n)?, false),
        };
        let value = if round {
            let extra = self.konst(word, ROUND_MASK, n)?;
            let mask = self.konst(word, !ROUND_MASK, n)?;
            let rounded = checked(self, ArithOp::Add, value, extra);
            self.charge_bitwise(word, BinaryOp::BitAnd, rounded, mask, n)
        } else {
            value
        };
        let ok = match overflow {
            Some(flag) => self.emit_unary(UnaryOp::Not, BOOL, Some(flag), n)?,
            None => self.konst(bool_, 1, n)?,
        };
        self.numeric_option(result, scalar(Scalar::U64), value, ok, n)
    }

    fn charge_bitwise(
        &mut self,
        ty: Ty,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> ValueId {
        let result = self.value(ty);
        self.inst(
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail: None,
            },
            n,
        );
        result
    }
}

impl Cx<'_, '_, '_> {
    /// Saturated exact charge. Overflow is a recoverable refusal by the runtime;
    /// it is not a source Overflow failure while building the Capacity record.
    pub(super) fn runtime_charge(
        &mut self,
        count: ValueId,
        size: ValueId,
        base: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        self.runtime_charge_parts(count, size, base, false, n)
            .map(|p| p.0)
    }

    pub(super) fn runtime_charge_parts(
        &mut self,
        count: ValueId,
        size: ValueId,
        base: ValueId,
        round: bool,
        n: NodeId,
    ) -> Option<(ValueId, ValueId)> {
        let u = Ty::Scalar(Scalar::U64);
        let product = self.value(u);
        let mul_overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: product,
                flag: Some(mul_overflow),
                op: ArithOp::Mul,
                mode: ArithMode::Checked,
                left: count,
                right: size,
            },
            n,
        );
        let total = self.value(u);
        let add_overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::IntArith {
                result: total,
                flag: Some(add_overflow),
                op: ArithOp::Add,
                mode: ArithMode::Checked,
                left: product,
                right: base,
            },
            n,
        );
        let overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Binary {
                result: overflow,
                op: BinaryOp::BitOr,
                left: mul_overflow,
                right: add_overflow,
                fail: None,
            },
            n,
        );
        let total = if round {
            let mask = self.konst(u, !ROUND_MASK, n)?;
            let aligned = self.value(u);
            self.inst(
                Inst::Binary {
                    result: aligned,
                    op: BinaryOp::BitAnd,
                    left: total,
                    right: mask,
                    fail: None,
                },
                n,
            );
            aligned
        } else {
            total
        };
        let max = self.konst(u, u64::MAX, n)?;
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[u]);
        self.runtime_branch(overflow, yes, no, n);
        self.start(yes);
        self.jump(join, &[max], n);
        self.start(no);
        self.jump(join, &[total], n);
        self.start(join);
        Some((
            self.b.blocks[join.index()]
                .params
                .get(&self.b.pools.values)
                .expect("the join block just built holds its parameter")[0],
            overflow,
        ))
    }
}
