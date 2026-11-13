//! Scalar lowering uses LLVM overflow and numeric intrinsics like the retained
//! backend, with CoreIR types and explicit failure successors.
use super::*;

impl FunctionEmitter<'_> {
    pub(super) fn overflow(&mut self, t: Ty, op: ArithOp, a: &str, b: &str) -> (String, String) {
        let op = match op {
            ArithOp::Add => "add",
            ArithOp::Sub => "sub",
            ArithOp::Mul => "mul",
            ArithOp::Div => unreachable!(),
        };
        let sign = if t.int().unwrap().0 { "s" } else { "u" };
        let rt = format!("{{ {}, i1 }}", ty(t));
        let x = self.intrinsic(
            &format!("llvm.{sign}{op}.with.overflow.{}", ty(t)),
            &rt,
            &[(ty(t), a.into()), (ty(t), b.into())],
        );
        let v = self.calc(format!("extractvalue {rt} {x}, 0"));
        let flag = self.calc(format!("extractvalue {rt} {x}, 1"));
        (v, flag)
    }
    fn div_overflow(&mut self, t: Ty, a: &str, b: &str) -> String {
        let zero = self.calc(format!("icmp eq {} {b}, 0", ty(t)));
        if !t.int().unwrap().0 {
            return zero;
        }
        let min = 1u64 << (size(t) * 8 - 1);
        let x = self.calc(format!("icmp eq {} {a}, {min}", ty(t)));
        let y = self.calc(format!("icmp eq {} {b}, -1", ty(t)));
        let over = self.calc(format!("and i1 {x}, {y}"));
        self.calc(format!("or i1 {zero}, {over}"))
    }
    pub(super) fn arith(
        &mut self,
        t: Ty,
        op: ArithOp,
        mode: ArithMode,
        a: &str,
        b: &str,
    ) -> (String, String) {
        let tt = ty(t);
        if op == ArithOp::Div {
            let over = self.div_overflow(t, a, b);
            let safe = self.calc(format!("select i1 {over}, {tt} 1, {tt} {b}"));
            let x = self.calc(format!(
                "{}div {tt} {a}, {safe}",
                if t.int().unwrap().0 { "s" } else { "u" }
            ));
            let x = self.calc(format!("select i1 {over}, {tt} 0, {tt} {x}"));
            return (x, over);
        }
        let op_name = match op {
            ArithOp::Add => "add",
            ArithOp::Sub => "sub",
            ArithOp::Mul => "mul",
            _ => unreachable!(),
        };
        match mode {
            ArithMode::Checked => self.overflow(t, op, a, b),
            ArithMode::Wrapping => (
                self.calc(format!("{op_name} {tt} {a}, {b}")),
                "false".into(),
            ),
            ArithMode::Saturating => {
                let (x, over) = self.overflow(t, op, a, b);
                let (signed, w) = t.int().unwrap();
                let limit = if !signed {
                    if op == ArithOp::Sub {
                        "0".into()
                    } else {
                        "-1".into()
                    }
                } else {
                    let negative = if op == ArithOp::Mul {
                        let n = self.calc(format!("xor {tt} {a}, {b}"));
                        self.calc(format!("icmp slt {tt} {n}, 0"))
                    } else {
                        self.calc(format!("icmp slt {tt} {a}, 0"))
                    };
                    self.calc(format!(
                        "select i1 {negative}, {tt} {}, {tt} {}",
                        1u64 << (w - 1),
                        (1u64 << (w - 1)) - 1
                    ))
                };
                (
                    self.calc(format!("select i1 {over}, {tt} {limit}, {tt} {x}")),
                    over,
                )
            }
        }
    }
    pub(super) fn binary(
        &mut self,
        t: Ty,
        op: BinaryOp,
        a: &str,
        b: &str,
        fail: Option<BlockId>,
    ) -> Result<String> {
        let tt = ty(t);
        if t.is_float() {
            let name = match op {
                BinaryOp::Add => "fadd",
                BinaryOp::Sub => "fsub",
                BinaryOp::Mul => "fmul",
                BinaryOp::Div => "fdiv",
                _ => "",
            };
            if !name.is_empty() {
                return Ok(self.calc(format!("{name} {tt} {a}, {b}")));
            }
            let name = match op {
                BinaryOp::Min | BinaryOp::MinNumber => "minimum",
                BinaryOp::Max | BinaryOp::MaxNumber => "maximum",
                _ => unreachable!(),
            };
            let ordered = self.intrinsic(
                &format!("llvm.{name}.{}", if size(t) == 4 { "f32" } else { "f64" }),
                &tt,
                &[(tt.clone(), a.into()), (tt.clone(), b.into())],
            );
            if matches!(op, BinaryOp::MinNumber | BinaryOp::MaxNumber) {
                // Retain the existing engine's signed-zero behavior. Its minimumNumber
                // intrinsic had a +0/-0 lowering defect at O0 on x86-64.
                let left_nan = self.calc(format!("fcmp uno {tt} {a}, {a}"));
                let right_nan = self.calc(format!("fcmp uno {tt} {b}, {b}"));
                let number = self.calc(format!("select i1 {right_nan}, {tt} {a}, {tt} {ordered}"));
                return Ok(self.calc(format!("select i1 {left_nan}, {tt} {b}, {tt} {number}")));
            }
            return Ok(ordered);
        }
        let (signed, w) = t.int().unwrap_or((false, (size(t) * 8) as u32));
        let sign = if signed { "s" } else { "u" };
        Ok(match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                let op = match op {
                    BinaryOp::Add => ArithOp::Add,
                    BinaryOp::Sub => ArithOp::Sub,
                    _ => ArithOp::Mul,
                };
                let (x, flag) = self.overflow(t, op, a, b);
                self.fail_if(&flag, fail)?;
                x
            }
            BinaryOp::Rem => {
                let zero = self.calc(format!("icmp eq {tt} {b}, 0"));
                self.fail_if(&zero, fail)?;
                // SEM-1: signed MIN % -1 is 0 and does not fail; a native signed
                // remainder traps there, and x % 1 is 0 for every x.
                let safe = if signed {
                    let minus_one = self.calc(format!("icmp eq {tt} {b}, -1"));
                    self.calc(format!("select i1 {minus_one}, {tt} 1, {tt} {b}"))
                } else {
                    b.into()
                };
                self.calc(format!("{sign}rem {tt} {a}, {safe}"))
            }
            BinaryOp::Div => {
                let over = self.div_overflow(t, a, b);
                self.fail_if(&over, fail)?;
                self.calc(format!("{sign}div {tt} {a}, {b}"))
            }
            BinaryOp::Shl | BinaryOp::Shr => {
                let outside = self.calc(format!("icmp uge {tt} {b}, {w}"));
                self.fail_if(&outside, fail)?;
                self.calc(format!(
                    "{} {tt} {a}, {b}",
                    if op == BinaryOp::Shl {
                        "shl"
                    } else if signed {
                        "ashr"
                    } else {
                        "lshr"
                    }
                ))
            }
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => self.calc(format!(
                "{} {tt} {a}, {b}",
                match op {
                    BinaryOp::BitAnd => "and",
                    BinaryOp::BitOr => "or",
                    _ => "xor",
                }
            )),
            BinaryOp::Min | BinaryOp::Max => {
                let cmp = self.calc(format!(
                    "icmp {sign}{} {tt} {a}, {b}",
                    if op == BinaryOp::Min { "lt" } else { "gt" }
                ));
                self.calc(format!("select i1 {cmp}, {tt} {a}, {tt} {b}"))
            }
            _ => unreachable!(),
        })
    }
    pub(super) fn scalar_unary(&mut self, t: Ty, op: ScalarUnaryOp, x: &str) -> String {
        let name = match op {
            ScalarUnaryOp::Sqrt => "sqrt",
            ScalarUnaryOp::Floor => "floor",
            ScalarUnaryOp::Ceil => "ceil",
            ScalarUnaryOp::Trunc => "trunc",
            ScalarUnaryOp::Round => "round",
            ScalarUnaryOp::Abs => "fabs",
            ScalarUnaryOp::LeadingZeros => "ctlz",
            ScalarUnaryOp::TrailingZeros => "cttz",
            ScalarUnaryOp::CountOnes => "ctpop",
            ScalarUnaryOp::SwapBytes => "bswap",
            ScalarUnaryOp::ReverseBits => "bitreverse",
        };
        if op == ScalarUnaryOp::SwapBytes && size(t) == 1 {
            return x.into();
        }
        let suffix = if t.is_float() {
            format!("f{}", size(t) * 8)
        } else {
            ty(t)
        };
        let mut args = vec![(ty(t), x.into())];
        if matches!(
            op,
            ScalarUnaryOp::LeadingZeros | ScalarUnaryOp::TrailingZeros
        ) {
            args.push(("i1".into(), "false".into()));
        }
        self.intrinsic(&format!("llvm.{name}.{suffix}"), &ty(t), &args)
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
        let x = self.get(operand);
        let ft = ty(from);
        let tt = ty(to);
        let signed = from.int().is_some_and(|x| x.0);
        let cast = |op: &str| format!("{op} {ft} {x} to {tt}");
        let r = match mode {
            ConvertMode::Reinterpret => {
                if from == to {
                    x.clone()
                } else if from == Ty::Ptr {
                    self.calc(cast("ptrtoint"))
                } else if to == Ty::Ptr {
                    self.calc(cast("inttoptr"))
                } else {
                    self.calc(cast("bitcast"))
                }
            }
            ConvertMode::Widen => {
                if size(from) == size(to) {
                    x.clone()
                } else {
                    self.calc(cast(if signed { "sext" } else { "zext" }))
                }
            }
            ConvertMode::Truncate => {
                if size(from) == size(to) {
                    x.clone()
                } else {
                    self.calc(cast("trunc"))
                }
            }
            ConvertMode::IntToFloat => self.calc(cast(if signed { "sitofp" } else { "uitofp" })),
            ConvertMode::FloatResize => {
                if from == to {
                    x.clone()
                } else {
                    self.calc(cast(if size(from) < size(to) {
                        "fpext"
                    } else {
                        "fptrunc"
                    }))
                }
            }
            ConvertMode::TryNarrow => {
                // Extend to i128 before range comparisons, preserving signedness.
                let wide = self.calc(format!(
                    "{} {ft} {x} to i128",
                    if signed { "sext" } else { "zext" }
                ));
                let (sign, w) = to.int().unwrap();
                let lo = if sign { -(1i128 << (w - 1)) } else { 0 };
                let hi = if sign {
                    (1i128 << (w - 1)) - 1
                } else {
                    (1i128 << w) - 1
                };
                let a = self.calc(format!("icmp sge i128 {wide}, {lo}"));
                let b = self.calc(format!("icmp sle i128 {wide}, {hi}"));
                let ok = self.calc(format!("and i1 {a}, {b}"));
                let status = self.calc(format!("zext i1 {ok} to i8"));
                self.put(flag.unwrap(), &status);
                let raw = self.calc(format!("zext {ft} {x} to i128"));
                self.calc(format!("trunc i128 {raw} to {tt}"))
            }
            ConvertMode::FloatToInt => {
                let (sign, w) = to.int().unwrap();
                let truncated = self.intrinsic(
                    &format!("llvm.trunc.f{}", size(from) * 8),
                    &ft,
                    &[(ft.clone(), x.clone())],
                );
                let low = if sign {
                    -(2f64).powi(w as i32 - 1)
                } else {
                    0.0
                };
                let high = (2f64).powi(w as i32 - i32::from(sign));
                let constant = |f: f64| {
                    if size(from) == 4 {
                        format!("bitcast (i32 {} to float)", (f as f32).to_bits())
                    } else {
                        format!("bitcast (i64 {} to double)", f.to_bits())
                    }
                };
                let a = self.calc(format!("fcmp oge {ft} {truncated}, {}", constant(low)));
                let b = self.calc(format!("fcmp olt {ft} {truncated}, {}", constant(high)));
                let ok = self.calc(format!("and i1 {a}, {b}"));
                let safe = self.calc(format!(
                    "select i1 {ok}, {ft} {truncated}, {ft} {}",
                    constant(0.0)
                ));
                let r = self.calc(format!(
                    "{} {ft} {safe} to {tt}",
                    if sign { "fptosi" } else { "fptoui" }
                ));
                let nan = self.calc(format!("fcmp uno {ft} {x}, {x}"));
                let bad = self.calc(format!("select i1 {nan}, i8 1, i8 2"));
                let status = self.calc(format!("select i1 {ok}, i8 0, i8 {bad}"));
                self.put(flag.unwrap(), &status);
                r
            }
        };
        self.put(result, &r);
    }
}
