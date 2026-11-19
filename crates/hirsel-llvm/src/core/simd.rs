use super::sink::Cast as LlvmCast;
use super::*;

pub(super) fn vector_type(lanes: u64, bits: u32) -> Type {
    Type::Vector(lanes as u32, Box::new(Type::Int(bits)))
}

pub(super) fn static_vectors(body: &Body) -> Box<[(ValueId, StaticId)]> {
    let pointers = body
        .insts
        .iter()
        .filter_map(|i| match *i {
            Inst::ReadStatic { result, data } => Some((result, data)),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    body.insts
        .iter()
        .filter_map(|i| match *i {
            Inst::Simd {
                result,
                op: SimdOp::From,
                first,
                ..
            } => pointers.get(&first).copied().map(|data| (result, data)),
            _ => None,
        })
        .collect()
}

impl FunctionEmitter<'_> {
    pub(super) fn simd(
        &mut self,
        result: ValueId,
        op: SimdOp,
        first: ValueId,
        second: Option<ValueId>,
        fail: Option<BlockId>,
    ) -> Result<()> {
        use SimdOp::*;

        let first_ty = self.vty(first);
        let result_ty = self.vty(result);
        let shape = match (result_ty, first_ty) {
            (Ty::Vector(v), _) | (_, Ty::Vector(v)) => v,
            _ => unreachable!(),
        };
        let ty = ty(Ty::Vector(shape));
        let lane = Ty::Scalar(shape.lane());
        let (signed, bits) = lane.int().unwrap();
        let lane_ty = Type::Int(bits);
        let count = shape.lanes();
        let a = self.numeric(first);
        let b = second.map(|v| self.numeric(v));
        let value = match op {
            From | Lanes => self.load(&ty, &a, 1),
            Cast => {
                let from = super::ty(first_ty);
                if from == ty {
                    a
                } else {
                    self.cast(LlvmCast::Bitcast, &from, &a, &ty)
                }
            }
            Splat => self.splat(&ty, &lane_ty, count, &a),
            Swizzle => {
                let Ty::Vector(table) = first_ty else {
                    unreachable!()
                };
                self.swizzle(table, shape, &a, &b.unwrap(), second.unwrap())
            }
            Eq | Lt => {
                let predicate = match op {
                    Eq => Predicate::Eq,
                    Lt if signed => Predicate::Slt,
                    Lt => Predicate::Ult,
                    _ => unreachable!(),
                };
                let mask = self.cmp(predicate, &ty, &a, &b.unwrap());
                self.cast(LlvmCast::SExt, &vector_type(count, 1), &mask, &ty)
            }
            WrappingAdd | WrappingSub | BitAnd | BitOr | BitXor => {
                let binary = match op {
                    WrappingAdd => Binary::Add,
                    WrappingSub => Binary::Sub,
                    BitAnd => Binary::And,
                    BitOr => Binary::Or,
                    BitXor => Binary::Xor,
                    _ => unreachable!(),
                };
                self.bin(binary, &ty, &a, &b.unwrap())
            }
            BitNot => {
                let all = self.splat(&ty, &lane_ty, count, &int(-1));
                self.bin(Binary::Xor, &ty, &a, &all)
            }
            Shl | Shr => {
                let b = b.unwrap();
                let bad = self.cmp(Predicate::Uge, &lane_ty, &b, &int(bits));
                self.fail_if(&bad, fail, 0)?;
                let count_v = self.splat(&ty, &lane_ty, count, &b);
                let op = match op {
                    Shl => Binary::Shl,
                    Shr if signed => Binary::AShr,
                    Shr => Binary::LShr,
                    _ => unreachable!(),
                };
                self.bin(op, &ty, &a, &count_v)
            }
            SaturatingAdd | SaturatingSub => {
                let sign = if signed { "s" } else { "u" };
                let op = if op == SaturatingAdd { "add" } else { "sub" };
                self.intrinsic(
                    &format!("llvm.{sign}{op}.sat.v{count}i{bits}"),
                    ty.clone(),
                    &[arg(ty.clone(), a), arg(ty, b.unwrap())],
                )
            }
            Any => {
                let max = self.intrinsic(
                    &format!("llvm.vector.reduce.umax.v{count}i{bits}"),
                    lane_ty.clone(),
                    &[arg(ty, a)],
                );
                let any = self.cmp(Predicate::Ne, &lane_ty, &max, &int(0));
                self.zext(&Type::I1, &any, &Type::I8)
            }
            Bitmask => {
                let top = self.cmp(Predicate::Slt, &ty, &a, &Value::Zero);
                let compact = Type::Int(count as u32);
                let mask = self.cast(LlvmCast::Bitcast, &vector_type(count, 1), &top, &compact);
                self.zext(&compact, &mask, &Type::I64)
            }
        };
        self.put(result, &value);
        Ok(())
    }

    fn splat(&mut self, ty: &Type, lane: &Type, count: u64, value: &Value) -> Value {
        let one = self.emit(Operation::InsertElement(ty, &Value::Poison, lane, value, 0));
        self.emit(Operation::Shuffle(
            ty,
            &one,
            &Value::Poison,
            &[0; 32][..count as usize],
        ))
    }

    fn swizzle(
        &mut self,
        table: Vector,
        output: Vector,
        bytes: &Value,
        indices: &Value,
        index_value: ValueId,
    ) -> Value {
        let table_ty = ty(Ty::Vector(table));
        let static_indices = self
            .static_vectors
            .as_deref()
            .and_then(|vectors| vectors.iter().find(|&&(value, _)| value == index_value));
        if let Some(&(_, data)) = static_indices {
            let (_, data) =
                self.module.statics[data.index()].native(&self.module.types, self.recipes);
            // The second operand is zeros. Out-of-range indices select its lane 0.
            let mask: Vec<_> = data
                .iter()
                .map(|&i| i32::from(i).min(table.lanes() as i32))
                .collect();
            return self.emit(Operation::Shuffle(&table_ty, bytes, &Value::Zero, &mask));
        }
        if output.lanes() == 32 && self.level.is_some_and(|level| level.number >= 3) {
            let wide = vector_type(32, 8);
            let broadcast = std::array::from_fn::<_, 32, _>(|lane| (lane % 16) as i32);
            let low = self.emit(Operation::Shuffle(
                &table_ty,
                bytes,
                &Value::Poison,
                &broadcast,
            ));
            let high = (table.lanes() == 32).then(|| {
                let broadcast = broadcast.map(|lane| lane + 16);
                self.emit(Operation::Shuffle(
                    &table_ty,
                    bytes,
                    &Value::Poison,
                    &broadcast,
                ))
            });
            return self.swizzle_pshufb(&wide, 32, &low, high.as_ref(), indices);
        }
        const LOW: [i32; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        const HIGH: [i32; 16] = [
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        ];
        let half = vector_type(16, 8);
        let (low, high) = if table.lanes() == 16 {
            (bytes.clone(), None)
        } else {
            (
                self.emit(Operation::Shuffle(&table_ty, bytes, &Value::Poison, &LOW)),
                Some(self.emit(Operation::Shuffle(&table_ty, bytes, &Value::Poison, &HIGH))),
            )
        };
        if output.lanes() == 16 {
            return self.swizzle_half(&half, &low, high.as_ref(), indices);
        }
        let wide = vector_type(32, 8);
        let il = self.emit(Operation::Shuffle(&wide, indices, &Value::Poison, &LOW));
        let ih = self.emit(Operation::Shuffle(&wide, indices, &Value::Poison, &HIGH));
        let lo = self.swizzle_half(&half, &low, high.as_ref(), &il);
        let hi = self.swizzle_half(&half, &low, high.as_ref(), &ih);
        let concatenate = std::array::from_fn::<_, 32, _>(|i| i as i32);
        self.emit(Operation::Shuffle(&half, &lo, &hi, &concatenate))
    }

    fn swizzle_half(
        &mut self,
        ty: &Type,
        low: &Value,
        high: Option<&Value>,
        indices: &Value,
    ) -> Value {
        if self.abi.target == Target::Aarch64Linux {
            let mut args = vec![arg(ty.clone(), low.clone())];
            if let Some(high) = high {
                args.push(arg(ty.clone(), high.clone()));
            }
            args.push(arg(ty.clone(), indices.clone()));
            let name = if high.is_some() {
                "llvm.aarch64.neon.tbl2.v16i8"
            } else {
                "llvm.aarch64.neon.tbl1.v16i8"
            };
            return self.intrinsic(name, ty.clone(), &args);
        }
        if self.level.is_some_and(|level| level.number >= 2) {
            return self.swizzle_pshufb(ty, 16, low, high, indices);
        }
        let mask_ty = vector_type(16, 1);
        let mut result = Value::Zero;
        for (half, table) in std::iter::once(low).chain(high).enumerate() {
            for lane in 0..16 {
                let index = self.splat(ty, &Type::I8, 16, &int(16 * half as i32 + lane));
                let found = self.cmp(Predicate::Eq, ty, indices, &index);
                let byte = self.emit(Operation::ExtractElement(ty, table, lane as u32));
                let byte = self.splat(ty, &Type::I8, 16, &byte);
                result = self.emit(Operation::VectorSelect(
                    ty, &mask_ty, &found, &byte, &result,
                ));
            }
        }
        result
    }

    fn swizzle_pshufb(
        &mut self,
        ty: &Type,
        width: u64,
        low: &Value,
        high: Option<&Value>,
        indices: &Value,
    ) -> Value {
        let shuffle = if width == 32 {
            "llvm.x86.avx2.pshuf.b"
        } else {
            "llvm.x86.ssse3.pshuf.b.128"
        };
        let Some(high) = high else {
            // 0..15 retains its low nibble. Every larger byte saturates into
            // 0x80..0xff, whose top bit makes PSHUFB zero that lane.
            let clamp = self.splat(ty, &Type::I8, width, &int(0x70));
            let indices = self.intrinsic(
                &format!("llvm.uadd.sat.v{width}i8"),
                ty.clone(),
                &[arg(ty.clone(), indices.clone()), arg(ty.clone(), clamp)],
            );
            return self.intrinsic(
                shuffle,
                ty.clone(),
                &[arg(ty.clone(), low.clone()), arg(ty.clone(), indices)],
            );
        };
        let nibble = self.splat(ty, &Type::I8, width, &int(15));
        let within = self.bin(Binary::And, ty, indices, &nibble);
        let lo = self.intrinsic(
            shuffle,
            ty.clone(),
            &[
                arg(ty.clone(), low.clone()),
                arg(ty.clone(), within.clone()),
            ],
        );
        let hi = self.intrinsic(
            shuffle,
            ty.clone(),
            &[arg(ty.clone(), high.clone()), arg(ty.clone(), within)],
        );
        let upper = self.cmp(Predicate::Ugt, ty, indices, &nibble);
        let mask = vector_type(width, 1);
        let selected = self.emit(Operation::VectorSelect(ty, &mask, &upper, &hi, &lo));
        let limit = self.splat(ty, &Type::I8, width, &int(32));
        let valid = self.cmp(Predicate::Ult, ty, indices, &limit);
        self.emit(Operation::VectorSelect(
            ty,
            &mask,
            &valid,
            &selected,
            &Value::Zero,
        ))
    }

    pub(super) fn put_lanes(&mut self, to: AddressId, vector: ValueId, count: ValueId) {
        let vector_ty = self.vty(vector);
        let ty = ty(vector_ty);
        let dest = self.address(to);
        let vector = self.numeric(vector);
        let count = self.numeric(count);
        let full = self.cmp(Predicate::Eq, &Type::I64, &count, &int(size(vector_ty)));
        let id = self.counter();
        let wide = Label::new("put_lanes_full", id);
        let prefix = Label::new("put_lanes_prefix", id);
        let done = Label::new("put_lanes_done", id);
        self.cond(&full, wide, prefix);
        self.block(wide);
        self.store(&ty, &vector, &dest, 1);
        self.br(done);
        self.block(prefix);
        let scratch = self.temp(ty.clone());
        self.store(&ty, &vector, &scratch, 8);
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &super::abi::memcpy(&dest, &scratch, count),
        );
        self.br(done);
        self.block(done);
    }
}
