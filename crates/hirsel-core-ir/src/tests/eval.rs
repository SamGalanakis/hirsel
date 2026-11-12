//! A test-only evaluator for the FORM v0 subset, written from the op documentation
//! (SEM-1, SEM-2, LIR's op docs) so the samples' hand-derived run lines can be
//! checked. Slots without steps are its only memory; everything FORM v1 panics.

use crate::*;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Returned(Vec<u64>),
    /// Control entered a failure block (or a callee failed).
    Failed,
}

const FN_BASE: u64 = 0x1000;

/// A run-line literal of type `t`: decimal floats or the constant syntax.
pub fn scalar(t: Ty, s: &str) -> u64 {
    if t.is_float() && !s.starts_with("0x") {
        let f: f64 = s.parse().unwrap_or_else(|_| panic!("bad float `{s}`"));
        return if t == Ty::Scalar(Scalar::F32) {
            u64::from((f as f32).to_bits())
        } else {
            f.to_bits()
        };
    }
    crate::text::parse_constant(t, s).unwrap_or_else(|e| panic!("{e}"))
}

fn width(t: Ty) -> u32 {
    match t {
        Ty::Scalar(s) => s.size() as u32 * 8,
        Ty::Ptr => 64,
    }
}
fn mask(t: Ty, x: u64) -> u64 {
    let w = width(t);
    if w == 64 { x } else { x & ((1 << w) - 1) }
}
/// The integer value of `x` read at `t`.
fn int(t: Ty, x: u64) -> i128 {
    match t.int() {
        Some((true, w)) => i128::from(((x << (64 - w)) as i64) >> (64 - w)),
        _ => i128::from(x),
    }
}
/// `v` as bits of `t`, or None when it does not fit.
fn fit(t: Ty, v: i128) -> Option<u64> {
    let (signed, w) = t.int()?;
    let (lo, hi) = if signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    };
    (lo..=hi).contains(&v).then(|| mask(t, v as u64))
}
fn is_f32(t: Ty) -> bool {
    t == Ty::Scalar(Scalar::F32)
}
fn float(t: Ty, x: u64) -> f64 {
    if is_f32(t) {
        f64::from(f32::from_bits(x as u32))
    } else {
        f64::from_bits(x)
    }
}
/// A float result as bits, a NaN as the kind's one observable NaN (SEM-2).
fn bits(t: Ty, f: f64) -> u64 {
    match (is_f32(t), f.is_nan()) {
        (true, true) => 0x7fc0_0000,
        (true, false) => u64::from((f as f32).to_bits()),
        (false, true) => 0x7ff8_0000_0000_0000,
        (false, false) => f.to_bits(),
    }
}

pub fn run(m: &Module, f: FuncId, args: &[u64]) -> Outcome {
    let func = &m.functions[f.index()];
    let b = func
        .body
        .as_ref()
        .expect("external functions do not run here");
    let mut vals = vec![0u64; b.values.len()];
    let mut slots: Vec<Option<u64>> = vec![None; b.slots.len()];
    let mut live = vec![false; b.slots.len()];
    let mut places: Vec<Option<SlotId>> = vec![None; b.addresses.len()];
    let pool = |r: PoolRange<ValueId>| r.get(&b.pools.values).unwrap().to_vec();
    let mut block = BlockId(0);
    let mut incoming = args.to_vec();
    loop {
        let blk = &b.blocks[block.index()];
        for (p, a) in pool(blk.params).into_iter().zip(&incoming) {
            vals[p.index()] = *a;
        }
        for inst in blk.insts.get(&b.insts).unwrap() {
            let ty = |v: ValueId| b.values[v.index()];
            match *inst {
                Inst::Const { result, bits } => vals[result.index()] = bits,
                Inst::FunctionRef { result, function } => {
                    vals[result.index()] = FN_BASE + u64::from(function.0)
                }
                Inst::ReadStatic { .. } => panic!("static data needs a layout (FORM v1)"),
                Inst::Unary {
                    result,
                    op,
                    operand,
                    ..
                } => {
                    let (t, x) = (ty(operand), vals[operand.index()]);
                    vals[result.index()] = match op {
                        UnaryOp::Not => x ^ 1,
                        UnaryOp::BitNot => mask(t, !x),
                        UnaryOp::Neg if t.is_float() => bits(t, -float(t, x)),
                        UnaryOp::Neg => match fit(t, -int(t, x)) {
                            Some(v) => v,
                            None => return Outcome::Failed,
                        },
                    };
                }
                Inst::Binary {
                    result,
                    op,
                    left,
                    right,
                    ..
                } => {
                    let t = ty(left);
                    let (a, c) = (vals[left.index()], vals[right.index()]);
                    let v = if t.is_float() {
                        Some(float_binary(t, op, a, c))
                    } else {
                        int_binary(t, op, a, c)
                    };
                    match v {
                        Some(v) => vals[result.index()] = v,
                        None => return Outcome::Failed,
                    }
                }
                Inst::Compare {
                    result,
                    op,
                    left,
                    right,
                } => {
                    let t = ty(left);
                    let (a, c) = (vals[left.index()], vals[right.index()]);
                    let ord = if t.is_float() {
                        float(t, a).partial_cmp(&float(t, c))
                    } else {
                        Some(int(t, a).cmp(&int(t, c)))
                    };
                    use std::cmp::Ordering::*;
                    let r = match op {
                        CompareOp::Eq => ord == Some(Equal),
                        CompareOp::Ne => ord != Some(Equal),
                        CompareOp::Lt => ord == Some(Less),
                        CompareOp::Le => matches!(ord, Some(Less | Equal)),
                        CompareOp::Gt => ord == Some(Greater),
                        CompareOp::Ge => matches!(ord, Some(Greater | Equal)),
                    };
                    vals[result.index()] = u64::from(r);
                }
                Inst::Convert {
                    result,
                    flag,
                    operand,
                    mode,
                } => {
                    let (from, to, x) = (ty(operand), ty(result), vals[operand.index()]);
                    let (r, fl) = match mode {
                        ConvertMode::Widen => (fit(to, int(from, x)).expect("widen fits"), 0),
                        ConvertMode::Truncate => (mask(to, x), 0),
                        ConvertMode::TryNarrow => {
                            (mask(to, x), u64::from(fit(to, int(from, x)).is_some()))
                        }
                        ConvertMode::FloatToI32 => {
                            let f = float(from, x);
                            let t = f.trunc();
                            if f.is_nan() {
                                (0, 1)
                            } else if f.is_infinite() {
                                (
                                    mask(to, (if f > 0.0 { i32::MAX } else { i32::MIN }) as u64),
                                    1,
                                )
                            } else if t < f64::from(i32::MIN) || t > f64::from(i32::MAX) {
                                (
                                    mask(to, (if t > 0.0 { i32::MAX } else { i32::MIN }) as u64),
                                    2,
                                )
                            } else {
                                (mask(to, t as i32 as u64), 0)
                            }
                        }
                        ConvertMode::IntToFloat => {
                            let v = int(from, x);
                            (
                                if is_f32(to) {
                                    u64::from((v as f32).to_bits())
                                } else {
                                    (v as f64).to_bits()
                                },
                                0,
                            )
                        }
                        ConvertMode::Reinterpret if from.is_float() => {
                            (bits(from, float(from, x)), 0)
                        }
                        ConvertMode::Reinterpret => (bits(to, float(to, x)), 0),
                        ConvertMode::FloatResize => (bits(to, float(from, x)), 0),
                    };
                    vals[result.index()] = r;
                    if let Some(f) = flag {
                        vals[f.index()] = fl;
                    }
                }
                Inst::IntArith {
                    result,
                    flag,
                    op,
                    mode,
                    left,
                    right,
                } => {
                    let t = ty(left);
                    let (a, c) = (int(t, vals[left.index()]), int(t, vals[right.index()]));
                    let exact = match op {
                        ArithOp::Add => a + c,
                        ArithOp::Sub => a - c,
                        ArithOp::Mul => a * c,
                    };
                    let wrapped = mask(t, exact as u64);
                    vals[result.index()] = match mode {
                        ArithMode::Wrapping | ArithMode::Checked => wrapped,
                        ArithMode::Saturating => {
                            let (signed, w) = t.int().unwrap();
                            let (lo, hi) = if signed {
                                (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
                            } else {
                                (0, (1i128 << w) - 1)
                            };
                            fit(t, exact.clamp(lo, hi)).unwrap()
                        }
                    };
                    if let Some(f) = flag {
                        vals[f.index()] = u64::from(fit(t, exact).is_none());
                    }
                }
                Inst::MulAdd {
                    result,
                    left,
                    right,
                    addend,
                } => {
                    let t = ty(result);
                    let [a, c, d] = [left, right, addend].map(|v| vals[v.index()]);
                    vals[result.index()] = if is_f32(t) {
                        let f = |x: u64| f32::from_bits(x as u32);
                        bits(t, f64::from(f(a).mul_add(f(c), f(d))))
                    } else {
                        bits(
                            t,
                            f64::from_bits(a).mul_add(f64::from_bits(c), f64::from_bits(d)),
                        )
                    };
                }
                Inst::ScalarUnary {
                    result,
                    op,
                    operand,
                } => {
                    let (t, x) = (ty(operand), vals[operand.index()]);
                    let w = width(t);
                    let f = float(t, x);
                    vals[result.index()] = match op {
                        ScalarUnaryOp::Sqrt => bits(t, f.sqrt()),
                        ScalarUnaryOp::Floor => bits(t, f.floor()),
                        ScalarUnaryOp::Ceil => bits(t, f.ceil()),
                        ScalarUnaryOp::Trunc => bits(t, f.trunc()),
                        ScalarUnaryOp::Round => bits(t, f.round()),
                        ScalarUnaryOp::Abs => bits(t, f.abs()),
                        ScalarUnaryOp::LeadingZeros => u64::from(x.leading_zeros() - (64 - w)),
                        ScalarUnaryOp::TrailingZeros => u64::from(x.trailing_zeros().min(w)),
                        ScalarUnaryOp::CountOnes => u64::from(x.count_ones()),
                        ScalarUnaryOp::SwapBytes => x.swap_bytes() >> (64 - w),
                        ScalarUnaryOp::ReverseBits => x.reverse_bits() >> (64 - w),
                    };
                }
                Inst::Address { address, .. } => {
                    let a = &b.addresses[address.index()];
                    let AddressBase::Slot(s) = a.base else {
                        panic!("pointer bases need layouts (FORM v1)")
                    };
                    assert_eq!(a.steps.len, 0, "projection steps need layouts (FORM v1)");
                    places[address.index()] = Some(s);
                }
                Inst::Read {
                    result,
                    address,
                    mode,
                } => {
                    let s = places[address.index()]
                        .expect("read of an unevaluated address")
                        .index();
                    assert!(live[s], "read of a dead slot");
                    vals[result.index()] = slots[s].expect("read of an uninitialized slot");
                    if mode == ReadMode::Take {
                        slots[s] = None;
                    }
                }
                Inst::Store { address, value } => {
                    let s = places[address.index()]
                        .expect("store to an unevaluated address")
                        .index();
                    assert!(live[s], "store to a dead slot");
                    slots[s] = Some(vals[value.index()]);
                }
                Inst::StorageLive { slot } => {
                    (live[slot.index()], slots[slot.index()]) = (true, None)
                }
                Inst::StorageDead { slot } => {
                    (live[slot.index()], slots[slot.index()]) = (false, None)
                }
                Inst::UpdateMask {
                    result,
                    mask: mv,
                    field,
                    initialized,
                } => {
                    let x = vals[mv.index()];
                    vals[result.index()] = if initialized {
                        x | (1 << field)
                    } else {
                        x & !(1 << field)
                    };
                }
                Inst::Call { call } => {
                    let site = &b.calls[call.index()];
                    let callee = match site.callee {
                        Callee::Direct(f) => f,
                        Callee::Indirect(v) => FuncId((vals[v.index()] - FN_BASE) as u32),
                    };
                    let args: Vec<u64> = site
                        .args
                        .get(&b.pools.args)
                        .unwrap()
                        .iter()
                        .map(|a| match a {
                            Arg::Value(v) => vals[v.index()],
                            Arg::Address(_) => panic!("by-address arguments are FORM v1"),
                        })
                        .collect();
                    match run(m, callee, &args) {
                        Outcome::Returned(r) => {
                            for (v, x) in pool(site.results).into_iter().zip(r) {
                                vals[v.index()] = x;
                            }
                        }
                        Outcome::Failed => return Outcome::Failed,
                    }
                }
            }
        }
        let take = |e: EdgeId, vals: &[u64]| {
            let e = &b.edges[e.index()];
            (
                e.target,
                pool(e.args)
                    .iter()
                    .map(|v| vals[v.index()])
                    .collect::<Vec<_>>(),
            )
        };
        (block, incoming) = match &b.terminators[block.index()] {
            Terminator::Jump(e) => take(*e, &vals),
            Terminator::Branch { condition, yes, no } => take(
                if vals[condition.index()] != 0 {
                    *yes
                } else {
                    *no
                },
                &vals,
            ),
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                let x = vals[value.index()];
                let arm = arms
                    .get(&b.pools.arms)
                    .unwrap()
                    .iter()
                    .find(|a| a.value == x);
                take(arm.map_or(*otherwise, |a| a.edge), &vals)
            }
            Terminator::Return(r) => {
                return Outcome::Returned(pool(*r).iter().map(|v| vals[v.index()]).collect());
            }
        };
    }
}

fn int_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> Option<u64> {
    let (x, y) = (int(t, a), int(t, c));
    let w = width(t);
    match op {
        BinaryOp::Add => fit(t, x + y),
        BinaryOp::Sub => fit(t, x - y),
        BinaryOp::Mul => fit(t, x.checked_mul(y)?),
        BinaryOp::Div => fit(t, x.checked_div(y)?),
        BinaryOp::Rem => (y != 0).then(|| mask(t, (x % y) as u64)),
        BinaryOp::DivFloor => fit(
            t,
            x.checked_div_euclid(y)
                .map(|q| if y < 0 && x % y != 0 { q + 1 } else { q })?,
        ),
        BinaryOp::Mod => (y != 0).then(|| mask(t, (((x % y) + y) % y) as u64)),
        BinaryOp::DivExact => {
            if y != 0 && x % y == 0 {
                fit(t, x / y)
            } else {
                None
            }
        }
        BinaryOp::Shl | BinaryOp::Shr if !(0..i128::from(w)).contains(&y) => None,
        BinaryOp::Shl => Some(mask(t, a << y)),
        BinaryOp::Shr => Some(mask(t, (x >> y) as u64)),
        BinaryOp::BitAnd => Some(a & c),
        BinaryOp::BitOr => Some(a | c),
        BinaryOp::BitXor => Some(a ^ c),
        BinaryOp::Min => Some(if x <= y { a } else { c }),
        BinaryOp::Max => Some(if x >= y { a } else { c }),
        BinaryOp::MinNumber | BinaryOp::MaxNumber => unreachable!("float only"),
    }
}

fn float_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> u64 {
    let (x, y) = (float(t, a), float(t, c));
    let r = match op {
        BinaryOp::Add => x + y,
        BinaryOp::Sub => x - y,
        BinaryOp::Mul => x * y,
        BinaryOp::Div => x / y,
        BinaryOp::Min | BinaryOp::Max if x.is_nan() || y.is_nan() => f64::NAN,
        BinaryOp::Min | BinaryOp::MinNumber => x.minimum_or_number(y, op == BinaryOp::Min),
        BinaryOp::Max | BinaryOp::MaxNumber => -(-x).minimum_or_number(-y, op == BinaryOp::Max),
        _ => unreachable!("integer only"),
    };
    // f32 arithmetic done in f64 is exact before the one rounding in `bits` for
    // + - * (24-bit significands); `/` is not used on f32 by any sample.
    bits(t, r)
}

trait MinimumOrNumber {
    fn minimum_or_number(self, y: f64, propagate: bool) -> f64;
}
impl MinimumOrNumber for f64 {
    /// IEEE 754-2019 minimum (`propagate`) or minimumNumber; -0 < +0.
    fn minimum_or_number(self, y: f64, propagate: bool) -> f64 {
        let x = self;
        if x.is_nan() || y.is_nan() {
            return if propagate {
                f64::NAN
            } else if x.is_nan() {
                y
            } else {
                x
            };
        }
        if x == y {
            return if x.is_sign_negative() { x } else { y };
        }
        x.min(y)
    }
}
