//! Scalar lowering uses LLVM overflow and numeric intrinsics like the retained
//! backend, with CoreIR types and explicit failure successors.
use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn overflow(&mut self, t: Ty, op: ArithOp, a: &Value, b: &Value) -> (Value, Value) {
        let tt = ty(t);
        if op == ArithOp::Add && !t.int().unwrap().0 {
            // Keep the value and its overflow condition scalar. A PHI of the
            // intrinsic's aggregate can hide end = index + width from the late
            // constraint pass, even on the edge where overflow was ruled out.
            // Wrapping add is defined on both paths, including checked_add's
            // observable wrapped result. No no-wrap promise is needed here.
            let value = self.bin(Binary::Add, &tt, a, b);
            let flag = self.cmp(Predicate::Ult, &tt, &value, a);
            return (value, flag);
        }
        if op == ArithOp::Sub && !t.int().unwrap().0 {
            let value = self.bin(Binary::Sub, &tt, a, b);
            let flag = self.cmp(Predicate::Ult, &tt, a, b);
            return (value, flag);
        }
        let op = match op {
            ArithOp::Add => "add",
            ArithOp::Sub => "sub",
            ArithOp::Mul => "mul",
            ArithOp::Div => unreachable!(),
        };
        let sign = if t.int().unwrap().0 { "s" } else { "u" };
        let rt = Type::structure([tt.clone(), Type::I1]);
        let x = self.intrinsic(
            &format!("llvm.{sign}{op}.with.overflow.{tt}"),
            rt.clone(),
            &[arg(tt.clone(), a.clone()), arg(tt, b.clone())],
        );
        let v = self.extract(&rt, &x, 0);
        let flag = self.extract(&rt, &x, 1);
        (v, flag)
    }
    fn div_overflow(&mut self, t: Ty, a: &Value, b: &Value) -> Value {
        let tt = ty(t);
        let zero = self.cmp(Predicate::Eq, &tt, b, &int(0));
        if !t.int().unwrap().0 {
            return zero;
        }
        let min = 1u64 << (size(t) * 8 - 1);
        let x = self.cmp(Predicate::Eq, &tt, a, &int(min));
        let y = self.cmp(Predicate::Eq, &tt, b, &int(-1));
        let over = self.bin(Binary::And, &Type::I1, &x, &y);
        self.bin(Binary::Or, &Type::I1, &zero, &over)
    }
    pub(super) fn arith(
        &mut self,
        t: Ty,
        op: ArithOp,
        mode: ArithMode,
        a: &Value,
        b: &Value,
    ) -> (Value, Value) {
        let tt = ty(t);
        if op == ArithOp::Div {
            let over = self.div_overflow(t, a, b);
            let safe = self.select(&tt, &over, &int(1), b);
            let div = if t.int().unwrap().0 {
                Binary::SDiv
            } else {
                Binary::UDiv
            };
            let x = self.bin(div, &tt, a, &safe);
            let x = self.select(&tt, &over, &int(0), &x);
            return (x, over);
        }
        let binary = match op {
            ArithOp::Add => Binary::Add,
            ArithOp::Sub => Binary::Sub,
            ArithOp::Mul => Binary::Mul,
            _ => unreachable!(),
        };
        match mode {
            ArithMode::Checked => self.overflow(t, op, a, b),
            ArithMode::Wrapping => (self.bin(binary, &tt, a, b), FALSE),
            ArithMode::Saturating => {
                let (x, over) = self.overflow(t, op, a, b);
                let (signed, w) = t.int().unwrap();
                let limit = if !signed {
                    if op == ArithOp::Sub {
                        int(0)
                    } else {
                        int(-1)
                    }
                } else {
                    let negative = if op == ArithOp::Mul {
                        let n = self.bin(Binary::Xor, &tt, a, b);
                        self.cmp(Predicate::Slt, &tt, &n, &int(0))
                    } else {
                        self.cmp(Predicate::Slt, &tt, a, &int(0))
                    };
                    self.select(
                        &tt,
                        &negative,
                        &int(1u64 << (w - 1)),
                        &int((1u64 << (w - 1)) - 1),
                    )
                };
                (self.select(&tt, &over, &limit, &x), over)
            }
        }
    }
    pub(super) fn binary(
        &mut self,
        t: Ty,
        op: BinaryOp,
        a: &Value,
        right: ValueId,
        fail: Option<BlockId>,
    ) -> Result<Value> {
        let b = &self.numeric(right);
        let tt = ty(t);
        if t.is_float() {
            let binary = match op {
                BinaryOp::Add => Some(Binary::FAdd),
                BinaryOp::Sub => Some(Binary::FSub),
                BinaryOp::Mul => Some(Binary::FMul),
                BinaryOp::Div => Some(Binary::FDiv),
                _ => None,
            };
            if let Some(binary) = binary {
                return Ok(self.bin(binary, &tt, a, b));
            }
            let name = match op {
                BinaryOp::Min | BinaryOp::MinNumber => "minimum",
                BinaryOp::Max | BinaryOp::MaxNumber => "maximum",
                _ => unreachable!(),
            };
            let ordered = self.intrinsic(
                &format!("llvm.{name}.{}", if size(t) == 4 { "f32" } else { "f64" }),
                tt.clone(),
                &[arg(tt.clone(), a.clone()), arg(tt.clone(), b.clone())],
            );
            if matches!(op, BinaryOp::MinNumber | BinaryOp::MaxNumber) {
                // Retain the existing engine's signed-zero behavior. Its minimumNumber
                // intrinsic had a +0/-0 lowering defect at O0 on x86-64.
                let left_nan = self.cmp(Predicate::Uno, &tt, a, a);
                let right_nan = self.cmp(Predicate::Uno, &tt, b, b);
                let number = self.select(&tt, &right_nan, a, &ordered);
                return Ok(self.select(&tt, &left_nan, b, &number));
            }
            return Ok(ordered);
        }
        let (signed, w) = t.int().unwrap_or((false, (size(t) * 8) as u32));
        Ok(match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let op = match op {
                    BinaryOp::Add => ArithOp::Add,
                    BinaryOp::Sub => ArithOp::Sub,
                    _ => ArithOp::Mul,
                };
                let (x, flag) = self.overflow(t, op, a, b);
                self.fail_if(&flag, fail, 0)?;
                x
            }
            BinaryOp::Rem => {
                let zero = self.cmp(Predicate::Eq, &tt, b, &int(0));
                self.fail_if(&zero, fail, 2)?;
                let safe = if signed {
                    let minus_one = self.cmp(Predicate::Eq, &tt, b, &int(-1));
                    self.select(&tt, &minus_one, &int(1), b)
                } else {
                    b.clone()
                };
                let rem = if signed { Binary::SRem } else { Binary::URem };
                self.bin(rem, &tt, a, &safe)
            }
            BinaryOp::Div => {
                let zero = self.cmp(Predicate::Eq, &tt, b, &int(0));
                self.fail_if(&zero, fail, 2)?;
                let over = self.div_overflow(t, a, b);
                self.fail_if(&over, fail, 0)?;
                let div = if signed { Binary::SDiv } else { Binary::UDiv };
                self.bin(div, &tt, a, b)
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                if !failure::bounded_shift(t, self.constants[right.index()]) {
                    let outside = self.cmp(Predicate::Uge, &tt, b, &int(w));
                    self.fail_if(&outside, fail, 0)?;
                }
                let shift = if op == BinaryOp::Shl {
                    Binary::Shl
                } else if signed {
                    Binary::AShr
                } else {
                    Binary::LShr
                };
                self.bin(shift, &tt, a, b)
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                let bit = match op {
                    BinaryOp::BitAnd => Binary::And,
                    BinaryOp::BitOr => Binary::Or,
                    _ => Binary::Xor,
                };
                self.bin(bit, &tt, a, b)
            }
            BinaryOp::Min | BinaryOp::Max => {
                let predicate = match (op == BinaryOp::Min, signed) {
                    (true, true) => Predicate::Slt,
                    (true, false) => Predicate::Ult,
                    (false, true) => Predicate::Sgt,
                    (false, false) => Predicate::Ugt,
                };
                let cmp = self.cmp(predicate, &tt, a, b);
                self.select(&tt, &cmp, a, b)
            }
            _ => unreachable!(),
        })
    }
    pub(super) fn scalar_unary(&mut self, t: Ty, op: ScalarUnaryOp, x: &Value) -> Value {
        let name = match op {
            ScalarUnaryOp::Sqrt => "sqrt",
            ScalarUnaryOp::Floor => "floor",
            ScalarUnaryOp::Ceil => "ceil",
            ScalarUnaryOp::Trunc => "trunc",
            ScalarUnaryOp::Round => "round",
            ScalarUnaryOp::RoundEven => "roundeven",
            ScalarUnaryOp::Abs => "fabs",
            ScalarUnaryOp::LeadingZeros => "ctlz",
            ScalarUnaryOp::TrailingZeros => "cttz",
            ScalarUnaryOp::CountOnes => "ctpop",
            ScalarUnaryOp::SwapBytes => "bswap",
            ScalarUnaryOp::ReverseBits => "bitreverse",
        };
        if op == ScalarUnaryOp::SwapBytes && size(t) == 1 {
            return x.clone();
        }
        let tt = ty(t);
        let suffix = if t.is_float() {
            format!("f{}", size(t) * 8)
        } else {
            tt.to_string()
        };
        let mut args = vec![arg(tt.clone(), x.clone())];
        if matches!(
            op,
            ScalarUnaryOp::LeadingZeros | ScalarUnaryOp::TrailingZeros
        ) {
            args.push(arg(Type::I1, FALSE));
        }
        self.intrinsic(&format!("llvm.{name}.{suffix}"), tt, &args)
    }
    pub(super) fn convert(
        &mut self,
        result: ValueId,
        flag: Option<ValueId>,
        operand: ValueId,
        mode: ConvertMode,
    ) {
        let from = self.vty(operand);
        let to = self.vty(result);
        let x = self.numeric(operand);
        let x = if mode == ConvertMode::Reinterpret {
            self.canonical(from, &x)
        } else {
            x
        };
        let ft = ty(from);
        let tt = ty(to);
        let signed = from.int().is_some_and(|x| x.0);
        let r = match mode {
            ConvertMode::Reinterpret => {
                if from == to {
                    x.clone()
                } else if from == Ty::Ptr {
                    self.cast(Cast::PtrToInt, &ft, &x, &tt)
                } else if to == Ty::Ptr {
                    self.cast(Cast::IntToPtr, &ft, &x, &tt)
                } else {
                    self.cast(Cast::Bitcast, &ft, &x, &tt)
                }
            }
            ConvertMode::Widen => {
                if size(from) == size(to) {
                    x.clone()
                } else {
                    let op = if signed { Cast::SExt } else { Cast::ZExt };
                    self.cast(op, &ft, &x, &tt)
                }
            }
            ConvertMode::Truncate => {
                if size(from) == size(to) {
                    x.clone()
                } else {
                    self.cast(Cast::Trunc, &ft, &x, &tt)
                }
            }
            ConvertMode::IntToFloat => {
                let op = if signed { Cast::SIToFP } else { Cast::UIToFP };
                self.cast(op, &ft, &x, &tt)
            }
            ConvertMode::FloatResize => {
                if from == to {
                    x.clone()
                } else {
                    let op = if size(from) < size(to) {
                        Cast::FPExt
                    } else {
                        Cast::FPTrunc
                    };
                    self.cast(op, &ft, &x, &tt)
                }
            }
            ConvertMode::TryNarrow if to == Ty::Scalar(Scalar::Char) => {
                // A Unicode scalar value, else 0: no invalid char exists.
                let i32 = Type::I32;
                let a = self.cmp(Predicate::Ult, &i32, &x, &int(55296));
                let b = self.cmp(Predicate::Uge, &i32, &x, &int(57344));
                let c = self.cmp(Predicate::Ule, &i32, &x, &int(1114111));
                let high = self.bin(Binary::And, &Type::I1, &b, &c);
                let ok = self.bin(Binary::Or, &Type::I1, &a, &high);
                let status = self.zext(&Type::I1, &ok, &Type::I8);
                self.put(flag.unwrap(), &status);
                self.select(&i32, &ok, &x, &int(0))
            }
            ConvertMode::TryNarrow => {
                // Extend to i128 before range comparisons, preserving signedness.
                let i128 = Type::Int(128);
                let op = if signed { Cast::SExt } else { Cast::ZExt };
                let wide = self.cast(op, &ft, &x, &i128);
                let (sign, w) = to.int().unwrap();
                let lo = if sign { -(1i128 << (w - 1)) } else { 0 };
                let hi = if sign {
                    (1i128 << (w - 1)) - 1
                } else {
                    (1i128 << w) - 1
                };
                let a = self.cmp(Predicate::Sge, &i128, &wide, &int(lo));
                let b = self.cmp(Predicate::Sle, &i128, &wide, &int(hi));
                let ok = self.bin(Binary::And, &Type::I1, &a, &b);
                let status = self.zext(&Type::I1, &ok, &Type::I8);
                self.put(flag.unwrap(), &status);
                let raw = self.zext(&ft, &x, &i128);
                self.cast(Cast::Trunc, &i128, &raw, &tt)
            }
            ConvertMode::FloatToInt => {
                let (sign, w) = to.int().unwrap();
                let truncated = self.intrinsic(
                    &format!("llvm.trunc.f{}", size(from) * 8),
                    ft.clone(),
                    &[arg(ft.clone(), x.clone())],
                );
                let low = if sign {
                    -(2f64).powi(w as i32 - 1)
                } else {
                    0.0
                };
                let high = (2f64).powi(w as i32 - i32::from(sign));
                let single = size(from) == 4;
                let constant = |f: f64| {
                    if single {
                        Value::Bitcast {
                            from: Type::I32,
                            bits: u64::from((f as f32).to_bits()),
                            to: Type::Float,
                        }
                    } else {
                        Value::Bitcast {
                            from: Type::I64,
                            bits: f.to_bits(),
                            to: Type::Double,
                        }
                    }
                };
                let a = self.cmp(Predicate::OGe, &ft, &truncated, &constant(low));
                let b = self.cmp(Predicate::OLt, &ft, &truncated, &constant(high));
                let ok = self.bin(Binary::And, &Type::I1, &a, &b);
                let safe = self.select(&ft, &ok, &truncated, &constant(0.0));
                let op = if sign { Cast::FPToSI } else { Cast::FPToUI };
                let r = self.cast(op, &ft, &safe, &tt);
                let nan = self.cmp(Predicate::Uno, &ft, &x, &x);
                let bad = self.select(&Type::I8, &nan, &int(1), &int(2));
                let status = self.select(&Type::I8, &ok, &int(0), &bad);
                self.put(flag.unwrap(), &status);
                r
            }
        };
        self.put(result, &r);
    }
}
