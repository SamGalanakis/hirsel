use super::function::{Address, Function};
use super::*;
use cranelift_codegen::ir::{
    MemFlagsData as MemFlags, StackSlotData, StackSlotKind, Value, condcodes::IntCC,
};

pub(super) fn two_halves(ty: Ty) -> bool {
    matches!(ty, Ty::Vector(v) if v.size() == 32)
}

impl Function<'_, '_, '_> {
    pub(super) fn load_vector(&mut self, result: ValueId, at: Address) {
        let low = self
            .b
            .ins()
            .load(types::I8X16, MemFlags::new(), at.base, at.offset);
        self.set(result, low);
        if two_halves(self.body.values[result.index()]) {
            let upper = self.offset_address(at, 16);
            let high = self
                .b
                .ins()
                .load(types::I8X16, MemFlags::new(), upper.base, upper.offset);
            self.high[result.index()] = high;
        }
    }

    pub(super) fn store_vector(&mut self, value: ValueId, at: Address) {
        let low = self.get(value);
        self.b.ins().store(MemFlags::new(), low, at.base, at.offset);
        if two_halves(self.body.values[value.index()]) {
            let upper = self.offset_address(at, 16);
            self.b.ins().store(
                MemFlags::new(),
                self.high[value.index()],
                upper.base,
                upper.offset,
            );
        }
    }

    fn vector_slot(&mut self, shape: Vector) -> ir::StackSlot {
        self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            shape.size() as u32,
            4,
        ))
    }

    fn spill_vector(&mut self, v: ValueId) -> ir::StackSlot {
        let Ty::Vector(shape) = self.body.values[v.index()] else {
            unreachable!()
        };
        let slot = self.vector_slot(shape);
        let low = self.get(v);
        self.b.ins().stack_store(types::I64, low, slot, 0);
        if shape.size() == 32 {
            self.b
                .ins()
                .stack_store(types::I64, self.high[v.index()], slot, 16);
        }
        slot
    }

    fn fill_vector(&mut self, result: ValueId, slot: ir::StackSlot) {
        let low = self.b.ins().stack_load(types::I64, types::I8X16, slot, 0);
        self.set(result, low);
        if two_halves(self.body.values[result.index()]) {
            self.high[result.index()] = self.b.ins().stack_load(types::I64, types::I8X16, slot, 16);
        }
    }

    pub(super) fn simd(
        &mut self,
        result: ValueId,
        op: SimdOp,
        first: ValueId,
        second: Option<ValueId>,
        fail: Option<BlockId>,
    ) -> Result<()> {
        use SimdOp::*;
        let first_ty = self.body.values[first.index()];
        let result_ty = self.body.values[result.index()];
        let shape = match (result_ty, first_ty) {
            (Ty::Vector(shape), _) | (_, Ty::Vector(shape)) => shape,
            _ => unreachable!(),
        };
        match op {
            From | Lanes => {
                self.load_vector(result, self.get(first).into());
                return Ok(());
            }
            Cast => {
                self.set(result, self.get(first));
                if shape.size() == 32 {
                    self.high[result.index()] = self.high[first.index()];
                }
                return Ok(());
            }
            Swizzle => {
                self.swizzle(result, shape, first, second.unwrap());
                return Ok(());
            }
            _ => {}
        }

        let lane = Ty::Scalar(shape.lane());
        let lane_ty = ty(lane);
        let width = shape.lane().size() as i32;
        let (signed, bits) = lane.int().unwrap();
        let count = if matches!(op, Shl | Shr) {
            let count = self.get(second.unwrap());
            let invalid =
                self.b
                    .ins()
                    .icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, count, i64::from(bits));
            self.guard(invalid, 0, fail)?;
            Some(count)
        } else {
            None
        };
        let a = (op != Splat).then(|| self.spill_vector(first));
        let b = second
            .filter(|_| count.is_none())
            .map(|v| self.spill_vector(v));
        let output = (!matches!(op, Any | Bitmask)).then(|| self.vector_slot(shape));
        let reduce_ty = ty(result_ty);
        let mut reduction = if output.is_none() {
            Some(self.b.ins().iconst(reduce_ty, 0))
        } else {
            None
        };
        for i in 0..shape.lanes() as i32 {
            let offset = i * width;
            let a = match a {
                Some(slot) => self.b.ins().stack_load(types::I64, lane_ty, slot, offset),
                None => self.get(first),
            };
            let b = b.map(|slot| self.b.ins().stack_load(types::I64, lane_ty, slot, offset));
            let value = match op {
                Splat => a,
                WrappingAdd => self.b.ins().iadd(a, b.unwrap()),
                WrappingSub => self.b.ins().isub(a, b.unwrap()),
                SaturatingAdd | SaturatingSub => {
                    let arith = if op == SaturatingAdd {
                        ArithOp::Add
                    } else {
                        ArithOp::Sub
                    };
                    self.arith(arith, ArithMode::Saturating, lane, a, b.unwrap())
                        .0
                }
                Eq | Lt => {
                    let compare = if op == Eq {
                        CompareOp::Eq
                    } else {
                        CompareOp::Lt
                    };
                    let condition = self.compare(compare, lane, a, b.unwrap());
                    let all = self.b.ins().iconst(lane_ty, -1);
                    let zero = self.b.ins().iconst(lane_ty, 0);
                    self.b.ins().select(condition, all, zero)
                }
                BitAnd => self.b.ins().band(a, b.unwrap()),
                BitOr => self.b.ins().bor(a, b.unwrap()),
                BitXor => self.b.ins().bxor(a, b.unwrap()),
                BitNot => self.b.ins().bnot(a),
                Shl => self.b.ins().ishl(a, count.unwrap()),
                Shr if signed => self.b.ins().sshr(a, count.unwrap()),
                Shr => self.b.ins().ushr(a, count.unwrap()),
                Any => self.b.ins().icmp_imm_s(IntCC::NotEqual, a, 0),
                Bitmask => {
                    let top = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, a, 0);
                    let bit = self.b.ins().iconst(types::I64, 1_i64 << i);
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.b.ins().select(top, bit, zero)
                }
                _ => unreachable!(),
            };
            if let Some(output) = output {
                self.b.ins().stack_store(types::I64, value, output, offset);
            } else {
                reduction = Some(self.b.ins().bor(reduction.unwrap(), value));
            }
        }
        if let Some(output) = output {
            self.fill_vector(result, output);
        } else {
            self.set(result, reduction.unwrap());
        }
        Ok(())
    }

    fn swizzle(&mut self, result: ValueId, shape: Vector, table: ValueId, indices: ValueId) {
        let Ty::Vector(table_shape) = self.body.values[table.index()] else {
            unreachable!()
        };
        let table = self.spill_vector(table);
        let indices = self.spill_vector(indices);
        let output = self.vector_slot(shape);
        let base = self.b.ins().stack_addr(types::I64, table, 0);
        let zero = self.b.ins().iconst(types::I8, 0);
        for i in 0..shape.lanes() as i32 {
            let index = self.b.ins().stack_load(types::I64, types::I8, indices, i);
            let valid =
                self.b
                    .ins()
                    .icmp_imm_u(IntCC::UnsignedLessThan, index, table_shape.lanes() as i64);
            let safe = self.b.ins().select(valid, index, zero);
            let offset = self.b.ins().uextend(types::I64, safe);
            let addr = self.b.ins().iadd(base, offset);
            let lane = self.b.ins().load(types::I8, MemFlags::new(), addr, 0);
            let lane = self.b.ins().select(valid, lane, zero);
            self.b.ins().stack_store(types::I64, lane, output, i);
        }
        self.fill_vector(result, output);
    }

    pub(super) fn put_lanes(
        &mut self,
        to: AddressId,
        vector: ValueId,
        count: ValueId,
    ) -> Result<()> {
        let count = self.get(count);
        let nonzero = self.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
        let write = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(nonzero, write, &[], done, &[]);
        self.b.switch_to_block(write);
        let to = self.addr(to);
        let from = self.spill_vector(vector);
        let from = self.b.ins().stack_addr(types::I64, from, 0);
        self.c_call(
            "memcpy",
            &[Ty::Ptr, Ty::Ptr, Ty::Scalar(Scalar::U64)],
            &[Ty::Ptr],
            &[to, from, count],
        )?;
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        Ok(())
    }
}
