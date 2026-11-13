use super::*;

impl<'c, 'a> Cx<'c, 'a> {
    pub(super) fn unsuffixed_number(&self, n: NodeId) -> bool {
        match self.token_kind(n) {
            Some(TokenKind::Integer) => int_suffix(self.token_text(n)).1.is_none(),
            Some(TokenKind::Float) => float_suffix(self.token_text(n)).1.is_none(),
            _ => false,
        }
    }
    /// A number literal, possibly parenthesised: it has no effect, so it may be
    /// emitted after an operand that follows it.
    pub(super) fn bare_number(&self, n: NodeId) -> Option<NodeId> {
        match self.kind(n) {
            NodeKind::Literal
                if matches!(
                    self.token_kind(n),
                    Some(TokenKind::Integer | TokenKind::Float)
                ) =>
            {
                Some(n)
            }
            NodeKind::Group => self.bare_number(self.kids(n)[0]),
            _ => None,
        }
    }

    pub(super) fn literal(
        &mut self,
        n: NodeId,
        want: Option<Want>,
        negate: bool,
    ) -> (TyId, Option<ValueId>) {
        let text = self.token_text(n);
        let scalar_want = want.and_then(|w| match self.t.kind(w.ty) {
            TyKind::Scalar(s) => Some(*s),
            _ => None,
        });
        let (ty, bits) = match self.token_kind(n) {
            Some(TokenKind::KwTrue) => (Scalar::Bool, 1),
            Some(TokenKind::KwFalse) => (Scalar::Bool, 0),
            Some(TokenKind::Character) => match character_value(text) {
                Some(c) => (Scalar::Char, u64::from(u32::from(c))),
                None => return self.gap_ty(n, "character literal form is not built"),
            },
            Some(TokenKind::Byte) => match byte_value(text) {
                Some(b) => (Scalar::U8, u64::from(b)),
                None => return self.gap_ty(n, "byte literal form is not built"),
            },
            Some(TokenKind::Integer) => {
                let (digits, suffix) = int_suffix(text);
                let Some(s) = suffix.or(scalar_want.filter(|s| numeric(*s))) else {
                    return self.untyped_number(n, want);
                };
                // LANG-3: an integer token takes only an integer type; a suffix names it.
                if suffix.is_none() && matches!(s, Scalar::F32 | Scalar::F64) {
                    let a = self.name(scalar(s));
                    self.refuse("LANG-3", n, format!("an integer literal is not `{a}`"));
                    return (ERROR, None);
                }
                let Some(m) = int_magnitude(digits) else {
                    let a = self.name(scalar(s));
                    self.refuse("LANG-3", n, format!("the literal does not fit `{a}`"));
                    return (ERROR, None);
                };
                if matches!(s, Scalar::F32 | Scalar::F64) {
                    let x = if negate { -(m as f64) } else { m as f64 };
                    (s, float_bits(s, x))
                } else {
                    let (signed, w) = Ty::Scalar(s).int().unwrap_or((false, 64));
                    let limit: u128 = if signed {
                        (1u128 << (w - 1)) - u128::from(!negate)
                    } else {
                        (1u128 << w) - 1
                    };
                    if m > limit {
                        let a = self.name(scalar(s));
                        self.refuse("LANG-3", n, format!("the literal does not fit `{a}`"));
                        return (ERROR, None);
                    }
                    let v = if negate {
                        (m as i128).wrapping_neg() as u64
                    } else {
                        m as u64
                    };
                    (s, v & (u64::MAX >> (64 - w)))
                }
            }
            Some(TokenKind::Float) => {
                let (digits, suffix) = float_suffix(text);
                let Some(s) = suffix.or(scalar_want.filter(|s| numeric(*s))) else {
                    return self.untyped_number(n, want);
                };
                if !matches!(s, Scalar::F32 | Scalar::F64) {
                    // LANG-3: a float token takes only a float type.
                    let a = self.name(scalar(s));
                    self.refuse("LANG-3", n, format!("a fractional literal is not `{a}`"));
                    return (ERROR, None);
                }
                let digits: String = digits.chars().filter(|&c| c != '_').collect();
                let x = match s {
                    Scalar::F32 => digits.parse::<f32>().map(f64::from),
                    _ => digits.parse::<f64>(),
                };
                let Ok(x) = x else {
                    return self.gap_ty(n, "float literal form is not built");
                };
                (s, float_bits(s, if negate { -x } else { x }))
            }
            Some(TokenKind::String | TokenKind::Bytes) => {
                return self.gap_ty(n, "string and bytes literals are not built");
            }
            _ => return self.gap_ty(n, "literal form is not built"),
        };
        let v = self.konst(Ty::Scalar(ty), bits, n);
        (scalar(ty), v)
    }
    /// A literal operand of type `s` (LANG-21) is evaluated here, by SEM-1 and SEM-2,
    /// and emitted as one constant: it has no run-time effect. A failure is refused
    /// LANG-3 at the operation that fails.
    pub(super) fn literal_operand(&mut self, n: NodeId, s: Scalar) -> (TyId, Option<ValueId>) {
        match self.fold(n, s, true) {
            Some(x) => {
                let bits = match x {
                    Num::Int(v) => {
                        let w = Ty::Scalar(s).int().map_or(64, |(_, w)| w);
                        (v as u64) & (u64::MAX >> (64 - w))
                    }
                    Num::Float(f) => float_bits(s, f),
                };
                (scalar(s), self.konst(Ty::Scalar(s), bits, n))
            }
            None => (ERROR, None),
        }
    }
    /// `n`'s value in `s`; `check` is false only for a number directly under unary `-`,
    /// whose magnitude may be one past the type's maximum (LANG-3).
    pub(super) fn fold(&mut self, n: NodeId, s: Scalar, check: bool) -> Option<Num> {
        let int = Ty::Scalar(s).int();
        let c = self.kids(n);
        let x = match self.kind(n) {
            NodeKind::Group => return self.fold(c[0], s, check),
            NodeKind::Literal => {
                let text = self.token_text(n);
                match (self.token_kind(n), int) {
                    (Some(TokenKind::Integer), Some(_)) => Num::Int(
                        int_magnitude(int_suffix(text).0)
                            .map_or(i128::MAX, |m| i128::try_from(m).unwrap_or(i128::MAX)),
                    ),
                    (Some(TokenKind::Float), None) => {
                        let d: String =
                            float_suffix(text).0.chars().filter(|&c| c != '_').collect();
                        Num::Float(d.parse::<f64>().ok()?)
                    }
                    (Some(TokenKind::Integer), None) => {
                        let a = self.name(scalar(s));
                        self.refuse("LANG-3", n, format!("an integer literal is not `{a}`"));
                        return None;
                    }
                    _ => {
                        let a = self.name(scalar(s));
                        self.refuse("LANG-3", n, format!("a fractional literal is not `{a}`"));
                        return None;
                    }
                }
            }
            NodeKind::Unary => {
                let op = self.token_text(n);
                let ok = match op {
                    "-" => matches!(int, Some((true, _)) | None),
                    _ => int.is_some(),
                };
                if !ok {
                    self.no_operator(n, scalar(s));
                    return None;
                }
                let bare = self.kind(c[0]) == NodeKind::Literal;
                match (op, self.fold(c[0], s, !(op == "-" && bare))?) {
                    ("-", Num::Int(v)) => Num::Int(-v),
                    ("-", Num::Float(f)) => Num::Float(-f),
                    (_, Num::Int(v)) => {
                        let (signed, w) = int.unwrap_or((false, 64));
                        Num::Int(wrap(!v, signed, w))
                    }
                    (_, f) => f,
                }
            }
            NodeKind::Binary => {
                let op = self.token_text(n);
                let arith = matches!(op, "+" | "-" | "*" | "/");
                let ints = matches!(op, "%" | "&" | "|" | "^" | "<<" | ">>");
                if !arith && !ints {
                    self.refuse("LANG-11", n, "literal operands have no type here");
                    return None;
                }
                if ints && int.is_none() {
                    self.no_operator(n, scalar(s));
                    return None;
                }
                let a = self.fold(c[0], s, true)?;
                let b = self.fold(c[1], s, true)?;
                match (a, b) {
                    (Num::Float(a), Num::Float(b)) => {
                        let f = match op {
                            "+" => a + b,
                            "-" => a - b,
                            "*" => a * b,
                            _ => a / b,
                        };
                        Num::Float(f)
                    }
                    (Num::Int(a), Num::Int(b)) => {
                        let (signed, w) = int.unwrap_or((false, 64));
                        let bad = |cx: &mut Self, why: &str| {
                            cx.refuse("LANG-3", n, why.to_owned());
                            None
                        };
                        Num::Int(match op {
                            "+" => a.saturating_add(b),
                            "-" => a.saturating_sub(b),
                            "*" => a.saturating_mul(b),
                            "/" | "%" if b == 0 => return bad(self, "division by zero"),
                            "/" => a / b,
                            "%" => a % b,
                            "&" => a & b,
                            "|" => a | b,
                            "^" => a ^ b,
                            _ if b < 0 || b >= i128::from(w) => {
                                return bad(self, "shift count out of range");
                            }
                            "<<" => wrap(a << b, signed, w),
                            _ => a >> b,
                        })
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };
        let x = match (x, s) {
            (Num::Float(f), Scalar::F32) => Num::Float(f64::from(f as f32)),
            (x, _) => x,
        };
        if let (Num::Int(v), Some((signed, w)), true) = (x, int, check) {
            let (lo, hi) = if signed {
                (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
            } else {
                (0, (1i128 << w) - 1)
            };
            if v < lo || v > hi {
                let a = self.name(scalar(s));
                self.refuse("LANG-3", n, format!("the value does not fit `{a}`"));
                return None;
            }
        }
        Some(x)
    }
    pub(super) fn untyped_number(
        &mut self,
        n: NodeId,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        match want {
            Some(w) if w.ty == ERROR => {}
            Some(w) => {
                let a = self.name(w.ty);
                self.refuse("LANG-11", n, format!("expected `{a}`, found a number"));
            }
            None => self.refuse("LANG-11", n, "an unsuffixed number needs an expected type"),
        }
        (ERROR, None)
    }

    pub(super) fn path(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        self.out.counts.uses += 1;
        let Some(u) = self.p.resolved(self.at, n) else {
            self.halt();
            return (ERROR, None);
        };
        match &u.target {
            Target::Binder(_) => self.place_value(n),
            Target::Missing => {
                self.halt();
                (ERROR, None)
            }
            Target::ContextualConstructor { .. } => self.gap_ty(n, "variants are not built"),
            _ => self.gap_ty(n, "function, constant and module values are not built"),
        }
    }
}
