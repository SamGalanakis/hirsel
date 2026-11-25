use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    // ---- operators
    pub(super) fn unary(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let e = self.kids(n)[0];
        match self.token_text(n) {
            "-" => {
                if let Some(lit) = self.bare_number(e) {
                    let s = match int_suffix(self.token_text(lit)).1 {
                        Some(s) => Some(s),
                        None => want.and_then(|w| match self.t.kind(w.ty) {
                            TyKind::Scalar(s) => Some(*s),
                            _ => None,
                        }),
                    };
                    // A signed integer's MIN magnitude is accepted under negation (LANG-3).
                    if s.is_some_and(|s| {
                        matches!(Ty::Scalar(s).int(), Some((true, _)))
                            || matches!(s, Scalar::F32 | Scalar::F64)
                    }) {
                        return self.literal(lit, want, true);
                    }
                }
                let (t, v) = self.expr(e, want.filter(|w| self.is_numeric(w.ty)));
                if t == ERROR || t == NEVER {
                    return (t, None);
                }
                // Unary minus is for signed integers and floats (LANG-21).
                let r = match int_of(t) {
                    Some((true, _)) => self.emit_unary(UnaryOp::Neg, t, v, n),
                    None if is_float(t) => self.emit_unary(UnaryOp::Neg, t, v, n),
                    _ => return self.no_operator(n, t),
                };
                (t, r)
            }
            "!" => {
                let (t, v) = self.expr(e, expect(BOOL));
                if t != BOOL {
                    return self.no_operator(n, t);
                }
                (t, self.emit_unary(UnaryOp::Not, t, v, n))
            }
            "~" => {
                let hint =
                    want.filter(|w| int_of(w.ty).is_some() || self.vector_shape(w.ty).is_some());
                let (t, v) = self.expr(e, hint);
                if self.vector_shape(t).is_some() {
                    return (t, self.emit_simd(SimdOp::BitNot, t, v, None, n));
                }
                if int_of(t).is_none() {
                    return self.no_operator(n, t);
                }
                (t, self.emit_unary(UnaryOp::BitNot, t, v, n))
            }
            "copy" => {
                // OWN-6: `copy x` of a whole slot reads `x` with its declared type. One
                // that is not plain is refused here and only viewed, so it is not also
                // moved. Other operands keep their value path, which may give a view.
                if self.kind(e) == NodeKind::Path {
                    match self.place(e) {
                        Ok(Some(at))
                            if at.place.len == 0
                                && !copyable(&self.p.decls, &self.t, at.ty, &self.params) =>
                        {
                            self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                            let a = self.shown(at.ty);
                            self.refuse("OWN-6", n, format!("`{a}` is not plain"));
                            return (at.ty, None);
                        }
                        Ok(Some(at)) => self.resolved_operand = Some((e, at)),
                        Ok(None) => {}
                        Err(()) => return (ERROR, None),
                    }
                }
                let (t, v) = self.expr(e, None);
                if !copyable(&self.p.decls, &self.t, t, &self.params) {
                    let a = self.shown(t);
                    self.refuse("OWN-6", n, format!("`{a}` is not plain"));
                }
                (t, v)
            }
            "take" => match self.place(e) {
                // OWN-6: `take` moves a place.
                Ok(Some(at)) => {
                    // OWN-15 refuses taking a binding that views the scrutinee; the
                    // refused `take` gives no value, so it is not also mistyped.
                    let views = self.var(at.place.slot).origin == Origin::PatternView;
                    self.use_place(at, Use::Take, n);
                    if views {
                        return (ERROR, None);
                    }
                    let v = self.emit_place(at, n);
                    if !self.copies(at.ty) {
                        self.temp(at.ty, v);
                    }
                    (at.ty, v)
                }
                Ok(None) => {
                    let _ = self.expr(e, None);
                    self.refuse("OWN-6", n, "`take` of an expression that is not a place");
                    (ERROR, None)
                }
                Err(()) => (ERROR, None),
            },
            "view" => match self.place(e) {
                // OWN-8: a temporary view, to the end of its statement unless bound.
                Ok(Some(at)) => {
                    if self.outside_vector(e, at.ty, "the target of a view") == ERROR
                        && at.ty != ERROR
                    {
                        return (ERROR, None);
                    }
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    let t = match self.t.kind(at.ty) {
                        TyKind::View(_) => at.ty,
                        _ => self.t.mk(TyKind::View(at.ty)),
                    };
                    let v = self.view_of(at, t, n);
                    (t, v)
                }
                Ok(None) => {
                    let _ = self.expr(e, None);
                    self.refuse("OWN-8", n, "`view` of an expression that is not a place");
                    (ERROR, None)
                }
                Err(()) => (ERROR, None),
            },
            _ => {
                self.refuse("OWN-7", n, "`mut p` is written only as an argument");
                (ERROR, None)
            }
        }
    }
    /// The value `view p` gives, carried as `p`'s type (FORM #56): a scalar is read,
    /// anything else is the address of `p`'s place, frozen while the view lives
    /// (OWN-8), or `p` itself when it already is a view.
    fn view_of(&mut self, at: At, t: TyId, n: NodeId) -> Option<ValueId> {
        match self.rep(t) {
            Err(what) => {
                self.gap(n, what);
                None
            }
            _ if t == at.ty => self.emit_place(at, n),
            Ok(Rep::Scalar(_) | Rep::None) => self.emit_place(at, n),
            Ok(Rep::Place(_)) => self.place_ptr(at, n),
        }
    }
    /// LANG-21: an operator applies only to the operand types its table lists; anything
    /// else is refused at the operator.
    pub(super) fn no_operator(&mut self, n: NodeId, t: TyId) -> (TyId, Option<ValueId>) {
        if t == ERROR || t == NEVER {
            return (t, None);
        }
        let (op, a) = (self.token_text(n), self.shown(t));
        let message = match (op.trim_end_matches('='), self.vector_shape(t)) {
            ("+", Some(_)) => format!("`{op}` does not apply to `{a}`. Use simd.wrapping_add"),
            ("-", Some(_)) if self.kind(n) == NodeKind::Binary || op == "-=" => {
                format!("`{op}` does not apply to `{a}`. Use simd.wrapping_sub")
            }
            _ => format!("`{op}` does not apply to `{a}`"),
        };
        self.refuse_compared_at(
            "LANG-21",
            self.op_span(n),
            a,
            format!("an operand type of `{op}`"),
            message,
        );
        (ERROR, None)
    }
    /// LANG-21's key / ordered operand requirement, shared by operators and literal
    /// patterns (SEM-7 defines what equality and order mean).
    pub(super) fn comparison_allowed(&self, ty: TyId, ordered: bool) -> bool {
        ty == ERROR
            || has_bounds(&self.p.decls, &self.t, ty, &self.params)
                & if ordered { ORDERED } else { KEY }
                != 0
    }
    pub(super) fn is_numeric(&self, t: TyId) -> bool {
        matches!(self.t.kind(t), TyKind::Scalar(s) if numeric(*s))
    }
    pub(super) fn emit_unary(
        &mut self,
        op: UnaryOp,
        t: TyId,
        v: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let (operand, ty) = (v?, value_ty(&self.t, t).ok().flatten()?);
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        let fail = if op == UnaryOp::Neg && ty.int().is_some() {
            self.fail_to(n)
        } else {
            None
        };
        self.inst(
            Inst::Unary {
                result,
                op,
                operand,
                fail,
            },
            n,
        );
        Some(result)
    }
    pub(super) fn emit_binary(
        &mut self,
        op: BinaryOp,
        t: TyId,
        l: Option<ValueId>,
        r: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let (left, right, ty) = (l?, r?, value_ty(&self.t, t).ok().flatten()?);
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        let fail = if op.can_fail(ty) {
            self.fail_to(n)
        } else {
            None
        };
        self.inst(
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail,
            },
            n,
        );
        Some(result)
    }

    pub(super) fn binary(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let op = self.token_text(n);
        match op {
            "&&" | "||" => self.short_circuit(n, op == "&&"),
            ".." | "..=" => self.gap_ty(n, "ranges are not built"),
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let (t, lv, rv) = self.operands(n, None);
                if t == ERROR || t == NEVER {
                    return (BOOL, None);
                }
                let need = if matches!(op, "==" | "!=") {
                    KEY
                } else {
                    ORDERED
                };
                if !self.comparison_allowed(t, need == ORDERED) {
                    let a = self.shown(t);
                    let b = if need == KEY { "key" } else { "ordered" };
                    let message = format!("`{a}` is not {b}");
                    self.refuse_compared("LANG-21", n, a, b, message);
                    return (BOOL, None);
                }
                let cop = match op {
                    "==" => CompareOp::Eq,
                    "!=" => CompareOp::Ne,
                    "<" => CompareOp::Lt,
                    "<=" => CompareOp::Le,
                    ">" => CompareOp::Gt,
                    _ => CompareOp::Ge,
                };
                if t == UNIT {
                    let eq = matches!(cop, CompareOp::Eq | CompareOp::Le | CompareOp::Ge);
                    return (BOOL, self.konst(Ty::Scalar(Scalar::Bool), u64::from(eq), n));
                }
                if value_ty(&self.t, t).ok().flatten().is_none() && !self.word_like(t) {
                    return match self.compare_places(t, cop, lv, rv, n) {
                        Ok(v) => (BOOL, v),
                        Err(what) => self.gap_ty(n, what),
                    };
                }
                let v = match (lv, rv) {
                    (Some(left), Some(right)) if self.live() => {
                        let result = self.value(Ty::Scalar(Scalar::Bool));
                        self.inst(
                            Inst::Compare {
                                result,
                                op: cop,
                                left,
                                right,
                            },
                            n,
                        );
                        if cop == CompareOp::Eq {
                            self.note_equal(result, left, right);
                        }
                        Some(result)
                    }
                    _ => None,
                };
                (BOOL, v)
            }
            _ => {
                let hint =
                    want.filter(|w| self.is_numeric(w.ty) || self.vector_shape(w.ty).is_some());
                let (t, lv, rv) = self.operands(n, hint);
                if t == ERROR || t == NEVER {
                    return (t, None);
                }
                (t, self.arith(op, t, lv, rv, n))
            }
        }
    }

    /// Type binary operator `n`'s two operands, which have one type (LANG-21): an
    /// operand that is not a literal operand fixes it, else the expected type does
    /// (LANG-11). A literal operand on the left is emitted after the right operand fixes
    /// its type.
    pub(super) fn operands(
        &mut self,
        n: NodeId,
        hint: Option<Want>,
    ) -> (TyId, Option<ValueId>, Option<ValueId>) {
        let before = self.out.refusals.len();
        let (t, lv, rv) = self.operand_pair(n, hint);
        // A refused operand is the program's one refusal here; the operator's own
        // checks and its result do not add another.
        if self.out.refusals.len() > before && t != NEVER {
            return (ERROR, None, None);
        }
        (t, lv, rv)
    }
    pub(super) fn operand_pair(
        &mut self,
        n: NodeId,
        hint: Option<Want>,
    ) -> (TyId, Option<ValueId>, Option<ValueId>) {
        let c = self.kids(n);
        let (l, r) = (c[0], c[1]);
        if self.synthesises(l) {
            let vector_hint = hint.filter(|w| self.vector_shape(w.ty).is_some());
            let (lt, lv) = self.operand(l, vector_hint);
            if lt == NEVER || lt == ERROR {
                if self.synthesises(r) {
                    self.operand(r, None);
                }
                return (lt, None, None);
            }
            let rv = self.second_operand(n, r, lt);
            return (lt, lv, rv);
        }
        if self.synthesises(r) {
            let (rt, rv) = self.operand(r, None);
            if rt == NEVER || rt == ERROR {
                return (rt, None, None);
            }
            let (_, lv) = self.expr(l, expect(rt));
            return (rt, lv, rv);
        }
        let Some(h) = hint else {
            self.refuse("LANG-11", l, "a literal operand needs an expected type");
            return (ERROR, None, None);
        };
        let (lt, lv) = self.expr(l, Some(h));
        let (_, rv) = self.expr(r, expect(lt));
        (lt, lv, rv)
    }
    /// The operand after one of type `t`: a literal operand takes `t`; any other must
    /// have it, or operator `n` is refused (LANG-21, no mixed widths or kinds).
    /// An operand that synthesises its type. An operator reads its operands: a place of
    /// a moving type is viewed for the statement, not moved, since `take` is written
    /// exactly where such a place is moved (OWN-6), and a view operand is its payload
    /// (SEM-7).
    pub(super) fn operand(&mut self, e: NodeId, hint: Option<Want>) -> (TyId, Option<ValueId>) {
        let at = match self.place(e) {
            Ok(Some(at)) if !self.copies(at.ty) => at,
            Ok(Some(at)) => {
                // `place` already evaluated every index operand. Do not visit it again.
                self.out.counts.nodes += 1;
                self.use_place(at, Use::Value, e);
                let value = self.emit_place(at, e);
                self.synth_ty = at.ty;
                self.diverge_if_never(at.ty, e);
                return match self.t.kind(at.ty) {
                    TyKind::View(x) => {
                        let x = *x;
                        (
                            x,
                            if self.copies(x) {
                                self.read_plain_view(x, value, e)
                            } else {
                                value
                            },
                        )
                    }
                    _ => (at.ty, value),
                };
            }
            Ok(None) => {
                let (t, v) = self.expr_hinted(e, hint);
                return match self.t.kind(t) {
                    TyKind::View(x) => {
                        let x = *x;
                        (
                            x,
                            if self.copies(x) {
                                self.read_plain_view(x, v, e)
                            } else {
                                v
                            },
                        )
                    }
                    _ => (t, v),
                };
            }
            Err(()) => return (ERROR, None),
        };
        // A moving operand is read through a pointer to its payload (SEM-7).
        self.use_place(at, Use::View(Hold::View(Extent::Temp)), e);
        let (t, view) = match self.t.kind(at.ty) {
            TyKind::View(x) => (*x, at.ty),
            _ => (at.ty, self.t.mk(TyKind::View(at.ty))),
        };
        (t, self.view_of(at, view, e))
    }
    pub(super) fn second_operand(&mut self, n: NodeId, r: NodeId, t: TyId) -> Option<ValueId> {
        let t = match self.vector_shape(t) {
            Some(shape) if matches!(self.token_text(n), "<<" | ">>" | "<<=" | ">>=") => {
                scalar(shape.lane())
            }
            _ => t,
        };
        if !self.synthesises(r) {
            return self.expr(r, expect(t)).1;
        }
        // LANG-21: the left operand gives the right its expected type; a mismatch is
        // still the operator's refusal, so the right is not coerced to it.
        let (rt, rv) = self.operand(r, (t != ERROR).then(|| expect(t)).flatten());
        if rt != t && ![NEVER, ERROR].contains(&rt) && t != ERROR {
            let (op, a, b) = (self.token_text(n), self.shown(t), self.shown(rt));
            let message = if matches!(op, "<<" | ">>" | "<<=" | ">>=") {
                format!("`{a}` {op} `{b}`; a shift count has the shifted value's type")
            } else {
                format!("`{a}` {op} `{b}`")
            };
            self.refuse_compared_at("LANG-21", self.op_span(n), b, a, message);
            return None;
        }
        rv
    }

    /// An arithmetic, bitwise or shift operator on two values of `t` (LANG-21, SEM-1).
    pub(super) fn arith(
        &mut self,
        op: &str,
        t: TyId,
        l: Option<ValueId>,
        r: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        if self.vector_shape(t).is_some() {
            let op = match op {
                "&" => SimdOp::BitAnd,
                "|" => SimdOp::BitOr,
                "^" => SimdOp::BitXor,
                "<<" => SimdOp::Shl,
                ">>" => SimdOp::Shr,
                _ => return self.no_operator(n, t).1,
            };
            return self.emit_simd(op, t, l, r, n);
        }
        let int = int_of(t).is_some();
        let float = is_float(t);
        let bop = match op {
            "+" => BinaryOp::Add,
            "-" => BinaryOp::Sub,
            "*" => BinaryOp::Mul,
            "/" => BinaryOp::Div,
            "%" => BinaryOp::Rem,
            "&" => BinaryOp::BitAnd,
            "|" => BinaryOp::BitOr,
            "^" => BinaryOp::BitXor,
            "<<" => BinaryOp::Shl,
            _ => BinaryOp::Shr,
        };
        let ok = match bop {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => int || float,
            _ => int,
        };
        if t == ERROR || t == NEVER {
            return None;
        }
        if !ok {
            self.no_operator(n, t);
            return None;
        }
        self.emit_binary(bop, t, l, r, n)
    }

    pub(super) fn short_circuit(&mut self, n: NodeId, and: bool) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        // LANG-6, OWN-8: an operand's temporaries and temporary views end once its bool
        // is produced.
        let (freezes, owners) = (self.freezes.len(), self.owner_mark());
        let (lt, lv) = self.expr(c[0], None);
        self.unfreeze(freezes, |h| (h != Hold::View(Extent::Temp)).then_some(h));
        self.end_owners(owners, None, true, c[0]);
        if lt != BOOL {
            return self.no_operator(n, lt);
        }
        let join = self.new_join();
        let mut right = None;
        if self.live()
            && let Some(lv) = lv
        {
            // A bounded loop's `&&` skips only on false. An explicit zero lets
            // the optimizer relate entry to the right operand's comparison.
            let loop_test = FrameKind::While { condition: Some(n) };
            let short = if self
                .frames
                .last()
                .is_some_and(|frame| frame.kind == loop_test)
            {
                self.konst(Ty::Scalar(Scalar::Bool), 0, c[0]).unwrap()
            } else {
                lv
            };
            let rb = self.new_block(&[]);
            let jb = self.new_block(&[Ty::Scalar(Scalar::Bool)]);
            self.joins[join].block = Some(jb);
            let (to_r, to_j) = (self.edge(rb, &[]), self.edge(jb, &[short]));
            let (yes, no) = if and { (to_r, to_j) } else { (to_j, to_r) };
            self.end(
                Terminator::Branch {
                    condition: lv,
                    yes,
                    no,
                },
                Some(c[0]),
            );
            self.start(rb);
            right = Some(rb);
        }
        // The right side runs on one path only: its effects join with none (OWN-16).
        let (was_dead, mark) = (self.dead, self.log.len());
        let (freezes, owners) = (self.freezes.len(), self.owner_mark());
        let (rt, rv) = self.expr(c[1], None);
        // Its temporaries end with it, on the one path that ran it.
        self.unfreeze(freezes, |h| (h != Hold::View(Extent::Temp)).then_some(h));
        self.end_owners(owners, None, true, c[1]);
        let ran = self.arm_edge(mark);
        let skipped = Some((self.diffs.len() as u32, 0));
        self.merge_arms(mark, [ran, skipped], was_dead, n);
        if rt != BOOL && rt != NEVER {
            return self.no_operator(n, rt);
        }
        if right.is_some() {
            self.goto(join, BOOL, rv, c[1]);
        }
        (BOOL, self.land(join))
    }
}
