//! Scalar lowering adapted from this backend's scalar.rs, with CoreIR types and
//! SEM-2's all-width conversion result/status contract.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{
    MemFlagsData as MemFlags, Value,
    condcodes::{FloatCC, IntCC},
    immediates::{Ieee32, Ieee64},
};

impl Function<'_, '_, '_> {
    pub fn resize(&mut self, v: Value, to: ir::Type, signed: bool) -> Value {
        let from = self.b.func.dfg.value_type(v);
        if from.bits() > to.bits() {
            self.b.ins().ireduce(to, v)
        } else if from.bits() < to.bits() {
            if signed {
                self.b.ins().sextend(to, v)
            } else {
                self.b.ins().uextend(to, v)
            }
        } else {
            v
        }
    }
    fn float_const(&mut self, t: ir::Type, x: f64) -> Value {
        if t == types::F32 {
            self.b
                .ins()
                .f32const(Ieee32::with_bits((x as f32).to_bits()))
        } else {
            self.b.ins().f64const(Ieee64::with_bits(x.to_bits()))
        }
    }
    pub fn canonical(&mut self, v: Value) -> Value {
        let t = self.b.func.dfg.value_type(v);
        let nan = self.b.ins().fcmp(FloatCC::Unordered, v, v);
        let value = self.float_const(t, f64::NAN);
        self.b.ins().select(nan, value, v)
    }
    fn libcall(&mut self, name: &'static str, args: &[Value]) -> Result<Value> {
        let t = Ty::Scalar(if self.b.func.dfg.value_type(args[0]) == types::F32 {
            Scalar::F32
        } else {
            Scalar::F64
        });
        let params = [t; 2];
        Ok(self.c_call(name, &params[..args.len()], &[t], args)?[0])
    }
    pub fn binary(&mut self, op: BinaryOp, t: Ty, l: Value, r: Value) -> Result<Value> {
        use BinaryOp::*;
        if t.is_float() {
            let v = match op {
                Add => self.b.ins().fadd(l, r),
                Sub => self.b.ins().fsub(l, r),
                Mul => self.b.ins().fmul(l, r),
                Div => self.b.ins().fdiv(l, r),
                Rem => self.libcall(if ty(t) == types::F32 { "fmodf" } else { "fmod" }, &[l, r])?,
                Min | Max | MinNumber | MaxNumber => {
                    let ordered = if matches!(op, Min | MinNumber) {
                        self.b.ins().fmin(l, r)
                    } else {
                        self.b.ins().fmax(l, r)
                    };
                    if matches!(op, MinNumber | MaxNumber) {
                        let ln = self.b.ins().fcmp(FloatCC::Unordered, l, l);
                        let rn = self.b.ins().fcmp(FloatCC::Unordered, r, r);
                        let v = self.b.ins().select(rn, l, ordered);
                        self.b.ins().select(ln, r, v)
                    } else {
                        ordered
                    }
                }
                _ => unreachable!("typed float operation"),
            };
            return Ok(v);
        }
        Ok(match op {
            BitAnd => self.b.ins().band(l, r),
            BitOr => self.b.ins().bor(l, r),
            BitXor => self.b.ins().bxor(l, r),
            Min | Max | MinNumber | MaxNumber => {
                let signed = t.int().is_some_and(|(s, _)| s);
                let cc = if signed {
                    IntCC::SignedLessThan
                } else {
                    IntCC::UnsignedLessThan
                };
                let lt = self.b.ins().icmp(cc, l, r);
                if matches!(op, Min | MinNumber) {
                    self.b.ins().select(lt, l, r)
                } else {
                    self.b.ins().select(lt, r, l)
                }
            }
            _ => {
                return Err(unsupported(format!(
                    "checked {op:?} requires the failure ABI"
                )));
            }
        })
    }
    pub fn compare(&mut self, op: CompareOp, t: Ty, l: Value, r: Value) -> Value {
        use CompareOp::*;
        if t.is_float() {
            let cc = match op {
                Eq => FloatCC::Equal,
                Ne => FloatCC::NotEqual,
                Lt => FloatCC::LessThan,
                Le => FloatCC::LessThanOrEqual,
                Gt => FloatCC::GreaterThan,
                Ge => FloatCC::GreaterThanOrEqual,
            };
            self.b.ins().fcmp(cc, l, r)
        } else {
            let signed = t.int().is_some_and(|(s, _)| s);
            let cc = match (op, signed) {
                (Eq, _) => IntCC::Equal,
                (Ne, _) => IntCC::NotEqual,
                (Lt, true) => IntCC::SignedLessThan,
                (Lt, false) => IntCC::UnsignedLessThan,
                (Le, true) => IntCC::SignedLessThanOrEqual,
                (Le, false) => IntCC::UnsignedLessThanOrEqual,
                (Gt, true) => IntCC::SignedGreaterThan,
                (Gt, false) => IntCC::UnsignedGreaterThan,
                (Ge, true) => IntCC::SignedGreaterThanOrEqual,
                (Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
            };
            self.b.ins().icmp(cc, l, r)
        }
    }
    pub fn arith(
        &mut self,
        op: ArithOp,
        mode: ArithMode,
        t: Ty,
        l: Value,
        r: Value,
    ) -> (Value, Value) {
        let (signed, width) = t.int().unwrap();
        let native = ty(t);
        if mode == ArithMode::Wrapping {
            let value = match op {
                ArithOp::Add => self.b.ins().iadd(l, r),
                ArithOp::Sub => self.b.ins().isub(l, r),
                ArithOp::Mul => self.b.ins().imul(l, r),
                ArithOp::Div => unreachable!("checked division only"),
            };
            let unused = self.b.ins().iconst(types::I8, 0);
            return (value, unused);
        }
        if op == ArithOp::Div {
            let zero = self.b.ins().icmp_imm_s(IntCC::Equal, r, 0);
            let fail = if signed {
                let min = self.b.ins().iconst(native, (1u64 << (width - 1)) as i64);
                let lm = self.b.ins().icmp(IntCC::Equal, l, min);
                let rm = self.b.ins().icmp_imm_s(IntCC::Equal, r, -1);
                let overflow = self.b.ins().band(lm, rm);
                self.b.ins().bor(zero, overflow)
            } else {
                zero
            };
            let one = self.b.ins().iconst(native, 1);
            let divisor = self.b.ins().select(fail, one, r);
            // The x86 division instructions operate on at least 32 bits.
            let wide = if width < 32 { types::I32 } else { native };
            let l = self.resize(l, wide, signed);
            let divisor = self.resize(divisor, wide, signed);
            let q = if signed {
                self.b.ins().sdiv(l, divisor)
            } else {
                self.b.ins().udiv(l, divisor)
            };
            let q = self.resize(q, native, false);
            let z = self.b.ins().iconst(native, 0);
            return (self.b.ins().select(fail, z, q), fail);
        }
        let (v, over) = match (op, signed) {
            (ArithOp::Add, true) => self.b.ins().sadd_overflow(l, r),
            (ArithOp::Add, false) => self.b.ins().uadd_overflow(l, r),
            (ArithOp::Sub, true) => self.b.ins().ssub_overflow(l, r),
            (ArithOp::Sub, false) => self.b.ins().usub_overflow(l, r),
            (ArithOp::Mul, true) => self.b.ins().smul_overflow(l, r),
            (ArithOp::Mul, false) => self.b.ins().umul_overflow(l, r),
            (ArithOp::Div, _) => unreachable!(),
        };
        if mode != ArithMode::Saturating {
            return (v, over);
        }
        let bound = if signed {
            let negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, l, 0);
            let negative = if op == ArithOp::Mul {
                let rn = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, r, 0);
                self.b.ins().bxor(negative, rn)
            } else {
                negative
            };
            let min = self.b.ins().iconst(native, (1u64 << (width - 1)) as i64);
            let max = self
                .b
                .ins()
                .iconst(native, ((1u64 << (width - 1)) - 1) as i64);
            self.b.ins().select(negative, min, max)
        } else {
            self.b
                .ins()
                .iconst(native, if op == ArithOp::Sub { 0 } else { -1 })
        };
        (self.b.ins().select(over, bound, v), over)
    }
    pub fn convert(
        &mut self,
        mode: ConvertMode,
        from: Ty,
        to: Ty,
        v: Value,
    ) -> (Value, Option<Value>) {
        let target = ty(to);
        match mode {
            ConvertMode::Widen | ConvertMode::Truncate | ConvertMode::TryNarrow => {
                let (si, wi) = from.int().unwrap();
                let (sj, wj) = to.int().unwrap();
                let value = self.resize(v, target, si);
                if mode != ConvertMode::TryNarrow {
                    return (value, None);
                }
                let mut fits = self.b.ins().iconst(types::I8, 1);
                let max = |signed: bool, width: u32| {
                    if width == 64 && !signed {
                        u64::MAX
                    } else {
                        (1u64 << (width - u32::from(signed))) - 1
                    }
                };
                if max(sj, wj) < max(si, wi) {
                    let bound = self.b.ins().iconst(ty(from), max(sj, wj) as i64);
                    let cmp = self.b.ins().icmp(
                        if si {
                            IntCC::SignedLessThanOrEqual
                        } else {
                            IntCC::UnsignedLessThanOrEqual
                        },
                        v,
                        bound,
                    );
                    fits = self.b.ins().band(fits, cmp);
                }
                if si && (!sj || wj < wi) {
                    let low = if sj {
                        (1u64 << (wj - 1)).wrapping_neg()
                    } else {
                        0
                    };
                    let bound = self.b.ins().iconst(ty(from), low as i64);
                    let cmp = self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, v, bound);
                    fits = self.b.ins().band(fits, cmp);
                }
                (value, Some(fits))
            }
            ConvertMode::IntToFloat => {
                let signed = from.int().unwrap().0;
                let wide = if ty(from).bits() < 32 {
                    types::I32
                } else {
                    ty(from)
                };
                let v = self.resize(v, wide, signed);
                (
                    if signed {
                        self.b.ins().fcvt_from_sint(target, v)
                    } else {
                        self.b.ins().fcvt_from_uint(target, v)
                    },
                    None,
                )
            }
            ConvertMode::FloatResize => (
                if target == types::F64 {
                    self.b.ins().fpromote(target, v)
                } else {
                    self.b.ins().fdemote(target, v)
                },
                None,
            ),
            ConvertMode::Reinterpret => {
                let v = if from.is_float() {
                    self.canonical(v)
                } else {
                    v
                };
                let v = self.b.ins().bitcast(target, MemFlags::trusted(), v);
                (if to.is_float() { self.canonical(v) } else { v }, None)
            }
            ConvertMode::FloatToInt => {
                let (signed, width) = to.int().unwrap();
                let x = if ty(from) == types::F32 {
                    self.b.ins().fpromote(types::F64, v)
                } else {
                    v
                };
                let x = self.b.ins().trunc(x);
                let lower = self.float_const(
                    types::F64,
                    if signed {
                        -(2f64).powi(width as i32 - 1)
                    } else {
                        0.0
                    },
                );
                let upper =
                    self.float_const(types::F64, (2f64).powi(width as i32 - i32::from(signed)));
                let low = self.b.ins().fcmp(FloatCC::LessThan, x, lower);
                let high = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, x, upper);
                let outside = self.b.ins().bor(low, high);
                let nan = self.b.ins().fcmp(FloatCC::Unordered, x, x);
                let zero = self.b.ins().iconst(types::I8, 0);
                let one = self.b.ins().iconst(types::I8, 1);
                let two = self.b.ins().iconst(types::I8, 2);
                let status = self.b.ins().select(outside, two, zero);
                let status = self.b.ins().select(nan, one, status);
                let wide = if width < 32 { types::I32 } else { target };
                let converted = if signed {
                    self.b.ins().fcvt_to_sint_sat(wide, x)
                } else {
                    self.b.ins().fcvt_to_uint_sat(wide, x)
                };
                let converted = self.resize(converted, target, false);
                let z = self.b.ins().iconst(target, 0);
                let ok = self.b.ins().icmp_imm_s(IntCC::Equal, status, 0);
                (self.b.ins().select(ok, converted, z), Some(status))
            }
        }
    }
    pub fn scalar_unary(&mut self, op: ScalarUnaryOp, v: Value) -> Result<Value> {
        use ScalarUnaryOp::*;
        Ok(match op {
            Sqrt => self.b.ins().sqrt(v),
            Floor => self.b.ins().floor(v),
            Ceil => self.b.ins().ceil(v),
            Trunc => self.b.ins().trunc(v),
            Abs => self.b.ins().fabs(v),
            Round => self.libcall(
                if self.b.func.dfg.value_type(v) == types::F32 {
                    "roundf"
                } else {
                    "round"
                },
                &[v],
            )?,
            LeadingZeros => self.b.ins().clz(v),
            TrailingZeros => self.b.ins().ctz(v),
            CountOnes => self.b.ins().popcnt(v),
            SwapBytes if self.b.func.dfg.value_type(v) == types::I8 => v,
            SwapBytes => self.b.ins().bswap(v),
            ReverseBits => self.b.ins().bitrev(v),
        })
    }
}
