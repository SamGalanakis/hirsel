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

    pub(super) fn path(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
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
            &Target::ContextualConstructor { name } => self.construct_variant(n, name, &[], want),
            _ => self.gap_ty(n, "function, constant and module values are not built"),
        }
    }

    // ---- construction (LANG-6, LANG-7, LANG-9)
    /// `P(x: 1, y: 2)`: a record of declaration `d`.
    pub(super) fn construct_record(
        &mut self,
        n: NodeId,
        d: DeclId,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let p: &'c Program<'a> = self.p;
        match &p.decls[d.index()].body {
            DeclBody::Record(fields) => {
                let args = &self.kids(n)[1..];
                self.construct(n, d, None, fields, true, args, want)
            }
            _ => self.gap_ty(n, "construction of this type is not built"),
        }
    }
    /// A variant, written bare and selected by the expected type (LANG-7): `Ok(x)`,
    /// `None`. `args` is empty for a bare name.
    pub(super) fn construct_variant(
        &mut self,
        n: NodeId,
        name: hirsel_core_syntax::Symbol,
        args: &[NodeId],
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let p: &'c Program<'a> = self.p;
        let expected = want
            .filter(|w| w.payload == 0)
            .and_then(|w| match self.t.kind(w.ty) {
                TyKind::Adt(d, _) => Some(*d),
                _ => None,
            });
        let Some(d) = expected else {
            return match want.filter(|w| w.payload == 0) {
                Some(w) if w.ty != HOLE && w.ty != ERROR => {
                    let (a, b) = (self.name(w.ty), p.index.symbols.text(name));
                    self.refuse("LANG-11", n, format!("expected `{a}`, found variant `{b}`"));
                    (ERROR, None)
                }
                Some(w) if w.ty == ERROR => {
                    self.halt();
                    (ERROR, None)
                }
                // LANG-7: with no expected type a variant names the one visible type
                // that declares it; its type arguments come from its payloads alone
                // (LANG-12, Q33).
                _ => {
                    let from = p.modules[self.at.module as usize].id;
                    let text = p.index.symbols.text(name);
                    let mut found = None;
                    for id in p.index.constructor_candidates(from, name) {
                        let Some(Def::Type(d)) = p.by_definition[id.0 as usize] else {
                            continue;
                        };
                        let DeclBody::Variant(cs) = &p.decls[d.index()].body else {
                            continue;
                        };
                        let Some(i) = cs.iter().position(|c| c.name == text) else {
                            continue;
                        };
                        if found.is_some_and(|(e, _, _)| e != d) {
                            self.refuse(
                                "LANG-7",
                                n,
                                format!("two visible types declare the variant `{text}`"),
                            );
                            return (ERROR, None);
                        }
                        found = Some((d, i, &cs[i]));
                    }
                    let Some((d, i, c)) = found else {
                        self.refuse("LANG-7", n, format!("no visible type declares `{text}`"));
                        return (ERROR, None);
                    };
                    self.construct(
                        n,
                        d,
                        Some(i as u32),
                        &c.fields,
                        c.labelled,
                        args,
                        want.filter(|w| w.payload != 0),
                    )
                }
            };
        };
        let from = p.modules[self.at.module as usize].id;
        let visible = p
            .index
            .constructor_candidates(from, name)
            .any(|id| matches!(p.by_definition[id.0 as usize], Some(Def::Type(x)) if x == d));
        let text = p.index.symbols.text(name);
        let decl = &p.decls[d.index()];
        let found = match &decl.body {
            DeclBody::Variant(cs) if visible => cs
                .iter()
                .position(|c| c.name == text)
                .map(|i| (Some(i as u32), &cs[i].fields, cs[i].labelled)),
            DeclBody::Record(fs) if visible && decl.path.0.name == text => Some((None, fs, true)),
            _ => None,
        };
        let Some((case, fields, labelled)) = found else {
            let a = self.name(want.map_or(ERROR, |w| w.ty));
            self.refuse(
                "LANG-11",
                n,
                format!("expected `{a}`, found variant `{text}`"),
            );
            return (ERROR, None);
        };
        self.construct(n, d, case, fields, labelled, args, want)
    }
    /// Check and build a value of declaration `d` (case `case` of a variant) from
    /// `args`, one per field in declaration order, labelled exactly when `labelled`
    /// (LANG-9). Type parameters are found as a call's are (LANG-11): the expected
    /// type, then each field left to right, then the unsuffixed literals. Fields are
    /// evaluated left to right (LANG-6) and the value is built in a new place.
    #[allow(clippy::too_many_arguments)]
    fn construct(
        &mut self,
        n: NodeId,
        d: DeclId,
        case: Option<u32>,
        fields: &'c [crate::decls::FieldDecl],
        labelled: bool,
        args: &[NodeId],
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        self.out.counts.calls += 1;
        let p: &'c Program<'a> = self.p;
        let decl = &p.decls[d.index()];
        // PROG-2: an opaque type is built only inside its own module.
        if decl.opaque && decl.site.module != p.modules[self.at.module as usize].id {
            let a = &decl.path.0.name;
            self.refuse("PROG-2", n, format!("`{a}` is opaque outside its module"));
            return (ERROR, None);
        }
        // A stable value is written through a writer (MEM-4, `emit(f)` payloads).
        if matches!(decl.repr, Repr::Stable) {
            return self.gap_ty(n, "stable values are not built");
        }
        let k = fields.len();
        if args.len() < k {
            self.refuse_at(
                "LANG-11",
                self.close_brace(n),
                format!("expected {k} arguments"),
            );
            return (ERROR, None);
        }
        if args.len() > k {
            self.refuse("LANG-11", args[k], format!("expected {k} arguments"));
            return (ERROR, None);
        }
        let mut found: Vec<Option<TyId>> = vec![None; decl.generics.len()];
        let params: Vec<TyId> = (0..decl.generics.len() as u32)
            .map(|i| self.t.mk(TyKind::Param(i)))
            .collect();
        let own = self.t.mk(TyKind::Adt(d, params));
        // A resource literal is `Result[R, (Capacity, F)]` (T1.2, LANG-14): F the one
        // field, or the tuple of them in declaration order, or bare `Capacity` for none.
        let lit = if matches!(decl.repr, Repr::Resource) {
            let (Some(result), Some(cap)) = (p.core_decl("Result"), p.core_decl("Capacity")) else {
                return self.gap_ty(n, "resource literals are not built");
            };
            let cap = self.t.mk(TyKind::Adt(cap, vec![]));
            let back = match fields {
                [] => cap,
                [f] => self.t.mk(TyKind::Tuple(vec![cap, f.ty])),
                fs => {
                    let f = self.t.mk(TyKind::Tuple(fs.iter().map(|f| f.ty).collect()));
                    self.t.mk(TyKind::Tuple(vec![cap, f]))
                }
            };
            Some(self.t.mk(TyKind::Adt(result, vec![own, back])))
        } else {
            None
        };
        if let Some(w) = want {
            if let Some(result) = self.expected_part(lit.unwrap_or(own), w) {
                self.unify(result, w.ty, &mut found);
            }
        }
        let mut vals: Vec<Option<ValueId>> = vec![None; k];
        let mut later = vec![];
        for (i, (&a, f)) in args.iter().zip(fields).enumerate() {
            let e = match (self.kind(a), labelled) {
                (NodeKind::LabelledArgument, true) if self.token_text(a) == f.name => {
                    self.kids(a)[0]
                }
                (NodeKind::LabelledArgument, true) => {
                    let name = &f.name;
                    self.refuse("LANG-9", a, format!("expected the field `{name}` here"));
                    return (ERROR, None);
                }
                (NodeKind::LabelledArgument, false) => {
                    self.refuse("LANG-9", a, "this payload is not labelled");
                    return (ERROR, None);
                }
                (NodeKind::BytesArgument, _) => {
                    return self.gap_ty(a, "labelled and bytes arguments are not built");
                }
                (_, true) => {
                    let name = &f.name;
                    self.refuse("LANG-9", a, format!("the field `{name}` is labelled"));
                    return (ERROR, None);
                }
                (_, false) => a,
            };
            if !self.synthesises(e) {
                later.push((i, e));
                continue;
            }
            vals[i] = match self.known(f.ty, &found) {
                Some(t) => self.expr(e, expect(t)).1,
                None => {
                    let (at, v) = if let Some(hint) =
                        want.and_then(|w| self.payload_argument(lit.unwrap_or(own), f.ty, w))
                    {
                        self.expr_hinted(e, Some(hint))
                    } else if self.kind(e) == NodeKind::Call {
                        self.out.counts.nodes += 1;
                        let w = self.partial(f.ty, &found);
                        self.call(e, expect(w))
                    } else {
                        self.expr(e, None)
                    };
                    if !self.unify(f.ty, at, &mut found) {
                        self.refuse(
                            "LANG-11",
                            e,
                            "a type parameter is determined twice differently",
                        );
                    } else if let Some(t) = self.known(f.ty, &found) {
                        self.coerce(
                            at,
                            Want {
                                ty: t,
                                view: false,
                                payload: 0,
                            },
                            e,
                        );
                    }
                    v
                }
            };
        }
        for (i, e) in later {
            match self.known(fields[i].ty, &found) {
                Some(t) => vals[i] = self.expr(e, expect(t)).1,
                None => {
                    self.refuse("LANG-11", e, "an unsuffixed number needs an expected type");
                }
            }
        }
        if found.iter().any(Option::is_none) {
            if !self.broken {
                self.refuse("LANG-12", n, "a type parameter of the type is still open");
            }
            return (ERROR, None);
        }
        let tys: Vec<TyId> = found.into_iter().flatten().collect();
        if let Some(lit) = lit {
            let ty = subst(&mut self.t, lit, &tys);
            self.gap(n, "resource literals are not built");
            return (ty, None);
        }
        let ty = self.t.mk(TyKind::Adt(d, tys.clone()));
        let id = match self.rep(ty) {
            Ok(Rep::Place(id)) => id,
            Ok(_) => return (ty, None),
            Err(what) => {
                self.gap(n, what);
                return (ty, None);
            }
        };
        if !self.live() {
            return (ty, None);
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        if let Some(case) = case {
            self.inst(Inst::SetCase { address: to, case }, n);
        }
        for (i, f) in fields.iter().enumerate() {
            let fty = subst(&mut self.t, f.ty, &tys);
            let r = match self.rep(fty) {
                Ok(r) => r,
                Err(what) => return self.gap_ty(n, what),
            };
            let step = match case {
                Some(case) => Step::CaseField {
                    case,
                    field: i as u32,
                },
                None => Step::Field(i as u32),
            };
            let place = match r {
                Rep::None => continue,
                Rep::Scalar(t) => PlaceTy::Value(t),
                Rep::Place(fid) => PlaceTy::Declared(fid),
            };
            if vals[i].is_none() {
                return self.gap_ty(n, "this field value is not built");
            }
            let at = self.address(AddressBase::Address(to), &[step], place, n);
            self.install(at, r, vals[i], n);
        }
        let p = self.pointer(to, n);
        self.temp(ty, Some(p));
        (ty, Some(p))
    }
}
