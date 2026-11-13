//! Explicit failure edges and FORM #38 status returns. A callee has already
//! recorded its cause; only the instruction that raises one records a new cell.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{
    ArgumentPurpose, MemFlagsData, StackSlotData, StackSlotKind, Value, condcodes::IntCC,
};
impl Function<'_, '_, '_> {
    pub fn fail_to(&mut self, target: Option<BlockId>) {
        if let Some(target) = target {
            self.b.ins().jump(self.blocks[target.index()], &[]);
        } else {
            let mut results = Vec::new();
            for &t in &self.signature.results[..self.signature.results.len() - 1] {
                let zero = match ty(t) {
                    types::F32 => self.b.ins().f32const(0.0),
                    types::F64 => self.b.ins().f64const(0.0),
                    t => self.b.ins().iconst(t, 0),
                };
                results.push(zero);
            }
            results.push(self.b.ins().iconst(types::I8, 1));
            self.signature
                .return_(&mut self.b, &results, self.result_storage);
        }
    }
    pub fn record_failure(&mut self, kind: u8) -> Result<()> {
        // HirselCoreCause in the runtime header, passed by value under the C ABI.
        // Its pointer fields are empty for primitive causes. Source-site table
        // rebinding is reserved for the driver/debug integration.
        let slot =
            self.b
                .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 32, 3));
        let addr = self.b.ins().stack_addr(types::I64, slot, 0);
        let zero = self.b.ins().iconst(types::I64, 0);
        for offset in [0, 8, 16, 24] {
            self.b
                .ins()
                .store(MemFlagsData::trusted(), zero, addr, offset);
        }
        let kind = self.b.ins().iconst(types::I8, i64::from(kind));
        self.b.ins().store(MemFlagsData::trusted(), kind, addr, 0);
        let mut sig = self.object.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::special(
            types::I64,
            ArgumentPurpose::StructArgument(32),
        ));
        let func = self
            .object
            .declare_function("hirsel_core_raise", Linkage::Import, &sig)
            .map_err(error)?;
        let func = self.object.declare_func_in_func(func, self.b.func);
        self.b.ins().call(func, &[self.context, addr]);
        Ok(())
    }
    pub fn guard(&mut self, condition: Value, kind: u8, target: Option<BlockId>) -> Result<()> {
        let failed = self.b.create_block();
        let next = self.b.create_block();
        self.b.ins().brif(condition, failed, &[], next, &[]);
        self.b.switch_to_block(failed);
        self.record_failure(kind)?;
        self.fail_to(target);
        self.b.switch_to_block(next);
        Ok(())
    }
    pub fn call_failure(&mut self, status: Value, target: Option<BlockId>) -> Result<()> {
        let failed = self.b.create_block();
        let next = self.b.create_block();
        self.b.ins().brif(status, failed, &[], next, &[]);
        self.b.switch_to_block(failed);
        self.fail_to(target);
        self.b.switch_to_block(next);
        Ok(())
    }
    pub fn checked_binary(
        &mut self,
        op: BinaryOp,
        t: Ty,
        l: Value,
        r: Value,
        fail: Option<BlockId>,
    ) -> Result<Value> {
        use BinaryOp::*;
        let (signed, width) = t.int().unwrap();
        let native = ty(t);
        if let Some(arith) = match op {
            Add => Some(ArithOp::Add),
            Sub => Some(ArithOp::Sub),
            Mul => Some(ArithOp::Mul),
            _ => None,
        } {
            let (value, overflow) = self.arith(arith, ArithMode::Checked, t, l, r);
            self.guard(overflow, 0, fail)?;
            return Ok(value);
        }
        if matches!(op, Shl | Shr) {
            let outside =
                self.b
                    .ins()
                    .icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, r, i64::from(width));
            self.guard(outside, 0, fail)?;
            return Ok(if op == Shl {
                self.b.ins().ishl(l, r)
            } else if signed {
                self.b.ins().sshr(l, r)
            } else {
                self.b.ins().ushr(l, r)
            });
        }
        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, r, 0);
        self.guard(zero, 2, fail)?;
        let minus_one = self.b.ins().icmp_imm_s(IntCC::Equal, r, -1);
        if signed && !matches!(op, Rem | Mod) {
            let min = self.b.ins().iconst(native, (1u64 << (width - 1)) as i64);
            let is_min = self.b.ins().icmp(IntCC::Equal, l, min);
            let overflow = self.b.ins().band(is_min, minus_one);
            self.guard(overflow, 0, fail)?;
        }
        let divisor = if signed && matches!(op, Rem | Mod) {
            let one = self.b.ins().iconst(native, 1);
            self.b.ins().select(minus_one, one, r)
        } else {
            r
        };
        let wide = if width < 32 { types::I32 } else { native };
        let a = self.resize(l, wide, signed);
        let b = self.resize(divisor, wide, signed);
        let rem = if signed {
            self.b.ins().srem(a, b)
        } else {
            self.b.ins().urem(a, b)
        };
        let rem = self.resize(rem, native, false);
        if matches!(op, Rem) {
            return Ok(rem);
        }
        let rn = self.b.ins().icmp_imm_s(IntCC::NotEqual, rem, 0);
        if op == Mod {
            if !signed {
                return Ok(rem);
            }
            let different = self.b.ins().bxor(rem, r);
            let different = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, different, 0);
            let adjust = self.b.ins().band(different, rn);
            let sum = self.b.ins().iadd(rem, r);
            return Ok(self.b.ins().select(adjust, sum, rem));
        }
        let q = if signed {
            self.b.ins().sdiv(a, b)
        } else {
            self.b.ins().udiv(a, b)
        };
        let q = self.resize(q, native, false);
        if op == DivExact {
            self.guard(rn, 0, fail)?;
            return Ok(q);
        }
        if op == DivFloor && signed {
            // Follow the reference evaluator while FORM settles its rounding:
            // div_euclid plus one for a negative divisor and nonzero remainder.
            let neg_rem = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, rem, 0);
            let neg_div = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, r, 0);
            let one = self.b.ins().iconst(native, 1);
            let minus = self.b.ins().iconst(native, -1);
            let z = self.b.ins().iconst(native, 0);
            let direction = self.b.ins().select(neg_div, one, minus);
            let adjustment = self.b.ins().select(neg_rem, direction, z);
            let q = self.b.ins().iadd(q, adjustment);
            let extra = self.b.ins().band(neg_div, rn);
            let extra = self.resize(extra, native, false);
            return Ok(self.b.ins().iadd(q, extra));
        }
        Ok(q)
    }
}
