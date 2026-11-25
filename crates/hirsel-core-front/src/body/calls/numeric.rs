use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// SEM-1 `int.ilog2(x)` on an unsigned `x` of `width` bits is
    /// `Some(width - 1 - leading_zeros(x))`, and `None` for 0.
    pub(super) fn ilog2(
        &mut self,
        input: TyId,
        (signed, width, ty): (bool, u32, Ty),
        result_ty: TyId,
        operand: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        if signed {
            let x = self.shown(input);
            self.refuse(
                "SEM-1",
                n,
                format!("`int.ilog2` takes an unsigned integer, not `{x}`"),
            );
            return None;
        }
        let operand = operand?;
        if !self.live() {
            return None;
        }
        let leading = self.value(ty);
        self.inst(
            Inst::ScalarUnary {
                result: leading,
                op: hirsel_core_ir::ScalarUnaryOp::LeadingZeros,
                operand,
            },
            n,
        );
        let u32_ = Ty::Scalar(Scalar::U32);
        let mode = if width > 32 {
            ConvertMode::Truncate
        } else {
            ConvertMode::Widen
        };
        let leading = self.numeric_convert(u32_, mode, leading, n)?;
        let top = self.konst(u32_, u64::from(width - 1), n)?;
        // At 0 the count is the width and this wraps; the Option discards it.
        let log = self.numeric_arith(u32_, ArithOp::Sub, top, leading, n)?;
        let zero = self.konst(ty, 0, n)?;
        let nonzero = self.numeric_compare(CompareOp::Ne, operand, zero, n);
        self.numeric_option(result_ty, scalar(Scalar::U32), log, nonzero, n)
    }

    pub(super) fn require_numeric(&mut self, ok: ValueId, n: NodeId) {
        if !self.live() {
            return;
        }
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        self.numeric_branch(ok, yes, no, n);
        self.start(no);
        // FORM records Overflow on scalar arithmetic. This failing arithmetic is
        // the narrow operation's failure edge, with the narrow call's source anchor.
        // It always fails, through the cleanup of its `fail` block, so nothing
        // reaches the end of this block; an exit here would skip that cleanup.
        let ty = Ty::Scalar(Scalar::U8);
        let max = self.konst(ty, 255, n).unwrap();
        let one = self.konst(ty, 1, n).unwrap();
        let _ = self.numeric_binary(ty, BinaryOp::Add, max, one, n);
        self.end(Terminator::Unreachable, Some(n));
        self.start(yes);
    }
}
