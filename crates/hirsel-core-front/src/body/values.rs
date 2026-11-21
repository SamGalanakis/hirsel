use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
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

    /// T1.2: a resource literal admits the block (storage, cause cell, grant) and is
    /// `Ok` of it with the fields written into the block, or on `Capacity` the error
    /// `(Capacity, F)` owning the fields: F the one field or the tuple of them, or
    /// bare `Capacity` with none (FAIL-11).
    fn resource_literal(
        &mut self,
        ty: TyId,
        own: TyId,
        fields: &[crate::decls::FieldDecl],
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let rid = match (self.rep(own), self.rep(ty)) {
            (Ok(Rep::Place(rid)), Ok(Rep::Place(_))) => rid,
            (Err(what), _) | (_, Err(what)) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let mut reps = vec![];
        for f in fields {
            let fty = subst(&mut self.t, f.ty, tys);
            match self.rep(fty) {
                Ok(r) => reps.push(r),
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            }
        }
        if reps
            .iter()
            .zip(vals)
            .any(|(r, v)| r.stored().is_some() && v.is_none())
        {
            self.gap(n, "this field value is not built");
            return None;
        }
        let Some(charge) = self.e.resource_charge(rid) else {
            self.gap(n, "resource literals are not built");
            return None;
        };
        if !self.live() {
            return None;
        }
        let (_, align) = self.e.block(rid);
        let grant = self.e.module.types[rid.index()].grant;
        self.admit_with_handback(
            ty,
            admission::OwnedAllocation {
                row: rid,
                prefix: 0,
                kind: AllocKind::Resource { grant },
                charge,
                align,
            },
            &reps,
            vals,
            n,
            |cx, to, block| {
                let holder = cx.address(
                    AddressBase::Address(to),
                    &[Step::CaseField { case: 0, field: 0 }],
                    PlaceTy::Value(Ty::Ptr),
                    n,
                );
                cx.inst(
                    Inst::Store {
                        address: holder,
                        value: block,
                    },
                    n,
                );
                let fields_at =
                    cx.address(AddressBase::Value(block), &[], PlaceTy::Declared(rid), n);
                for (i, r) in reps.iter().enumerate() {
                    if let Some(place) = r.stored() {
                        let at = cx.address(
                            AddressBase::Address(fields_at),
                            &[Step::Field(i as u32)],
                            place,
                            n,
                        );
                        cx.install(at, *r, vals[i], n);
                    }
                }
            },
        )
    }
    fn byte_string(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let p = self.p;
        let (Some(bytes), Some(list)) =
            (quoted_bytes(self.token_text(n), true), p.core_decl("List"))
        else {
            return self.gap_ty(n, "byte string literal form is not built");
        };
        let frozen = want
            .filter(|w| w.payload == 0)
            .and_then(|w| match self.t.kind(w.ty) {
                TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::FrozenBytes) => {
                    Some(w.ty)
                }
                _ => None,
            });
        let ty = frozen.unwrap_or_else(|| self.t.mk(TyKind::Adt(list, vec![scalar(Scalar::U8)])));
        let frozen = frozen.is_some();
        let list_id = match self.rep(ty) {
            Ok(Rep::Place(a)) => a,
            Err(what) => return self.gap_ty(n, what),
            _ => return (ty, None),
        };
        if !self.live() {
            return (ty, None);
        }
        let to = self.new_slot(PlaceTy::Declared(list_id), n);
        let u64t = Ty::Scalar(Scalar::U64);
        let len = bytes.len() as u64;
        let (Some(length), Some(null)) = (self.konst(u64t, len, n), self.konst(Ty::Ptr, 0, n))
        else {
            return (ty, None);
        };
        // Both carriers have the same capacity and occupy three words.
        let ok = |cx: &mut Self, data: ValueId, payer: Option<ValueId>| {
            let carrier = to;
            let fields = [
                (data, Ty::Ptr),
                payer.map_or((length, u64t), |payer| (payer, Ty::Ptr)),
                (length, u64t),
            ];
            for (k, (value, word_ty)) in fields.into_iter().enumerate() {
                let Some(offset) = cx.konst(u64t, 8 * k as u64, n) else {
                    return;
                };
                let at = cx.address(
                    AddressBase::Address(carrier),
                    &[Step::Byte(offset)],
                    PlaceTy::Value(word_ty),
                    n,
                );
                cx.inst(Inst::Store { address: at, value }, n);
            }
        };
        if bytes.is_empty() {
            ok(self, null, None);
        } else {
            let Some(charge) = self.konst(u64t, len.next_multiple_of(8), n) else {
                return (ty, None);
            };
            // MEM-24: both carriers admit a List[u8] of capacity len, rounded
            // up to eight. The heap block uses the rounded size for word stores.
            let block = self.value(Ty::Ptr);
            let Some(size) = (if frozen {
                self.konst(u64t, 0, n)
            } else {
                Some(charge)
            }) else {
                return (ty, None);
            };
            let available = self.value(u64t);
            self.inst(
                Inst::Alloc {
                    result: block,
                    available,
                    region_entry: None,
                    kind: AllocKind::Own {
                        edge: if frozen {
                            FieldTy::FrozenBytes
                        } else {
                            FieldTy::Seq(Box::new(FieldTy::Scalar(Scalar::U8)))
                        },
                    },
                    place: AllocPlace::Current,
                    charge,
                    overflow: None,
                    size,
                    align: 8,
                },
                n,
            );
            let admitted = self.value(Ty::Scalar(Scalar::Bool));
            self.inst(
                Inst::Compare {
                    result: admitted,
                    op: CompareOp::Ne,
                    left: block,
                    right: null,
                },
                n,
            );
            self.raise_unless(admitted, charge, available, n);
            if frozen {
                let data = self.e.static_bytes(&bytes);
                let value = self.value(Ty::Ptr);
                self.inst(
                    Inst::ReadStatic {
                        result: value,
                        data,
                    },
                    n,
                );
                ok(self, value, Some(block));
            } else {
                // The bytes as little-endian words (memory.md: scalars are little-endian),
                // the last one zero-padded inside the rounded block.
                for (k, word) in bytes.chunks(8).enumerate() {
                    let mut w = [0u8; 8];
                    w[..word.len()].copy_from_slice(word);
                    let (Some(offset), Some(value)) = (
                        self.konst(u64t, 8 * k as u64, n),
                        self.konst(u64t, u64::from_le_bytes(w), n),
                    ) else {
                        return (ty, None);
                    };
                    let at = self.address(
                        AddressBase::Value(block),
                        &[Step::Byte(offset)],
                        PlaceTy::Value(u64t),
                        n,
                    );
                    self.inst(Inst::Store { address: at, value }, n);
                }
                ok(self, block, None);
            }
        }
        let v = self.pointer(to, n);
        self.temp(ty, Some(v));
        (ty, Some(v))
    }
    /// A string literal is static (core.md "Literals"): a `String` whose carrier is
    /// `{static data, byte length, capacity 0}`. It admits nothing, is never charged and
    /// cannot fail; capacity 0 owns no storage, so its cleanup frees nothing (FORM
    /// "Strings and literals"), and no operation writes a `String`'s bytes in place. `""`
    /// has a null data pointer. Where a view of `String` is expected, the literal is the
    /// view of its frame slot.
    fn static_literal(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let p = self.p;
        let (Some(data), Some(decl)) = (
            quoted_bytes(self.token_text(n), false),
            p.core_decl("String"),
        ) else {
            return self.gap_ty(n, "string literal form is not built");
        };
        let ty = self.t.mk(TyKind::Adt(decl, vec![]));
        self.static_string_value(&data, ty, n, want)
    }

    fn static_string_value(
        &mut self,
        data: &[u8],
        ty: TyId,
        n: NodeId,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let viewed = want.is_some_and(|w| matches!(self.t.kind(w.ty), TyKind::View(x) if *x == ty));
        let shown = if viewed {
            self.t.mk(TyKind::View(ty))
        } else {
            ty
        };
        let v = self.static_string(data, ty, n);
        if !viewed {
            self.temp(ty, v);
        }
        (shown, v)
    }

    pub(super) fn static_string(&mut self, data: &[u8], ty: TyId, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let u64t = Ty::Scalar(Scalar::U64);
        let start = if data.is_empty() {
            self.konst(Ty::Ptr, 0, n)
        } else {
            let d = self.e.static_bytes(data);
            let v = self.value(Ty::Ptr);
            self.inst(Inst::ReadStatic { result: v, data: d }, n);
            Some(v)
        };
        let (Some(start), Some(length), Some(zero)) = (
            start,
            self.konst(u64t, data.len() as u64, n),
            self.konst(u64t, 0, n),
        ) else {
            return None;
        };
        for (k, (value, t)) in [(start, Ty::Ptr), (length, u64t), (zero, u64t)]
            .into_iter()
            .enumerate()
        {
            let Some(offset) = self.konst(u64t, 8 * k as u64, n) else {
                return None;
            };
            let at = self.address(
                AddressBase::Address(to),
                &[Step::Byte(offset)],
                PlaceTy::Value(t),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        Some(self.pointer(to, n))
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
                let Some((signed, w)) = Ty::Scalar(s).int() else {
                    let a = self.shown(scalar(s));
                    let message = format!("an integer literal is not `{a}`");
                    self.refuse_literal(n, negate, a, message);
                    return (ERROR, None);
                };
                let Some(m) = int_magnitude(digits) else {
                    let a = self.shown(scalar(s));
                    let message = format!("the literal does not fit `{a}`");
                    self.refuse_literal(n, negate, a, message);
                    return (ERROR, None);
                };
                let limit: u128 = if signed {
                    (1u128 << (w - 1)) - u128::from(!negate)
                } else {
                    (1u128 << w) - 1
                };
                if m > limit {
                    let a = self.shown(scalar(s));
                    let message = format!("the literal does not fit `{a}`");
                    self.refuse_literal(n, negate, a, message);
                    return (ERROR, None);
                }
                let v = if negate {
                    (m as i128).wrapping_neg() as u64
                } else {
                    m as u64
                };
                (s, v & (u64::MAX >> (64 - w)))
            }
            Some(TokenKind::Float) => {
                let (digits, suffix) = float_suffix(text);
                let Some(s) = suffix.or(scalar_want.filter(|s| numeric(*s))) else {
                    return self.untyped_number(n, want);
                };
                if !matches!(s, Scalar::F32 | Scalar::F64) {
                    // LANG-3: a float token takes only a float type.
                    let a = self.shown(scalar(s));
                    let message = format!("a fractional literal is not `{a}`");
                    self.refuse_literal(n, negate, a, message);
                    return (ERROR, None);
                }
                let Some(bits) = parse_float_bits(s, digits, negate) else {
                    return self.gap_ty(n, "float literal form is not built");
                };
                (s, bits)
            }
            Some(TokenKind::Bytes) => return self.byte_string(n, want),
            Some(TokenKind::String) => return self.static_literal(n, want),
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
                    Num::Float(bits) => bits,
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
                        Num::Float(parse_float_bits(s, float_suffix(text).0, false)?)
                    }
                    (Some(TokenKind::Integer), None) => {
                        let a = self.shown(scalar(s));
                        let message = format!("an integer literal is not `{a}`");
                        self.refuse_literal(n, false, a, message);
                        return None;
                    }
                    _ => {
                        let a = self.shown(scalar(s));
                        let message = format!("a fractional literal is not `{a}`");
                        self.refuse_literal(n, false, a, message);
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
                    ("-", Num::Float(bits)) => Num::Float(float_negate_bits(s, bits)),
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
                    (Num::Float(a), Num::Float(b)) => Num::Float(float_binary_bits(s, op, a, b)),
                    (Num::Int(a), Num::Int(b)) => {
                        let (signed, w) = int.unwrap_or((false, 64));
                        let bad = |cx: &mut Self, why: &str, found: String, required: String| {
                            cx.refuse_compared("LANG-3", n, found, required, why.to_owned());
                            None
                        };
                        Num::Int(match op {
                            "+" => a.saturating_add(b),
                            "-" => a.saturating_sub(b),
                            "*" => a.saturating_mul(b),
                            "/" | "%" if b == 0 => {
                                return bad(
                                    self,
                                    "division by zero",
                                    "0".into(),
                                    "a nonzero divisor".into(),
                                );
                            }
                            "/" => a / b,
                            "%" => a % b,
                            "&" => a & b,
                            "|" => a | b,
                            "^" => a ^ b,
                            _ if b < 0 || b >= i128::from(w) => {
                                let required = format!("a shift count from 0 to {}", w - 1);
                                return bad(
                                    self,
                                    "shift count out of range",
                                    b.to_string(),
                                    required,
                                );
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
        if let (Num::Int(v), Some((signed, w)), true) = (x, int, check) {
            let (lo, hi) = if signed {
                (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
            } else {
                (0, (1i128 << w) - 1)
            };
            if v < lo || v > hi {
                let a = self.shown(scalar(s));
                let message = format!("the value does not fit `{a}`");
                self.refuse_compared("LANG-3", n, v.to_string(), a, message);
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
                let a = self.shown(w.ty);
                let message = format!("expected `{a}`, found a number");
                self.refuse_literal_as("LANG-11", n, false, a, message);
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
            Target::Binder(_) => self.place_value(n, want),
            Target::Missing => {
                self.halt();
                (ERROR, None)
            }
            &Target::Definition(id) if self.p.constant_ids[id.0 as usize].is_some() => {
                let p = self.p;
                let constant = p.constant_ids[id.0 as usize].unwrap();
                let Some(value) = p.constants[constant as usize].value else {
                    self.halt();
                    return (ERROR, None);
                };
                let mut value = value;
                for token in
                    &self.item.tree.tokens[u.projections.start as usize..u.projections.end as usize]
                {
                    if token.kind == TokenKind::Ident {
                        let name = p.index.symbols.text(token.symbol);
                        let field = match p.constant_field(
                            p.modules[self.at.module as usize].id,
                            value,
                            name,
                        ) {
                            Ok(field) => field,
                            Err(rule) => {
                                self.push_refusal(p.constant_field_refusal(
                                    rule,
                                    self.cause(n),
                                    value,
                                    name,
                                ));
                                return (ERROR, None);
                            }
                        };
                        value = field;
                    }
                }
                if let crate::ConstData::StaticString(id) = value.data {
                    return self.static_string_value(
                        &p.constant_strings[id as usize],
                        value.ty,
                        n,
                        want,
                    );
                }
                let emitted = self.constant_value(value, n);
                (value.ty, emitted)
            }
            &Target::ContextualConstructor { name } => self.construct_variant(n, name, &[], want),
            &Target::Definition(id) if u.projections.is_empty() => {
                match self.p.by_definition[id.0 as usize] {
                    Some(Def::Fn(f)) => self.function_value(f as usize, n, want),
                    _ => self.gap_ty(n, "function, constant and module values are not built"),
                }
            }
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
    /// Tuples use the same place representation as records. Retain their argument
    /// workspace with the other call workspaces across expressions.
    pub(super) fn tuple(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        scratch.vals.clear();
        scratch.tys.clear();
        let fields = self.kids(n);
        let mut slot = None;
        let mut field_places = Vec::new();
        let can_place_fields = self.expression_destination(n).is_none() || !self.may_return(n);
        for (i, &field) in fields.iter().enumerate() {
            let expected = want.and_then(|w| match self.t.kind(w.ty) {
                TyKind::Tuple(xs) if w.payload == 0 => xs.get(i).copied(),
                _ => None,
            });
            let place = if can_place_fields
                && self.live()
                && let Some(ty) = expected
                && let Ok(Rep::Place(field_ty)) = self.rep(ty)
                && let Some(want) = want
                && matches!(self.t.kind(want.ty), TyKind::Tuple(xs) if xs.len() == fields.len())
                && let Ok(Rep::Place(id)) = self.rep(want.ty)
            {
                let to = *slot.get_or_insert_with(|| self.result_slot(PlaceTy::Declared(id), n));
                let address = self.field_destination(to, &[Step::Field(i as u32)], field_ty, field);
                field_places.push((i, address));
                Some(address)
            } else {
                None
            };
            let fresh = self.expression_destination(n).is_none();
            let (ty, value) = self.expr_into_field(field, expected.and_then(expect), place, fresh);
            let ty = self.outside_vector(field, ty, "a tuple element");
            scratch.tys.push(ty);
            scratch.vals.push(value);
            self.anchor_field(n, anchors::Component::Field(i as u32), field);
        }
        // A refused component has no aggregate layout. Propagate the refusal
        // without publishing an error-shaped tuple or an emission gap.
        if scratch.tys.contains(&ERROR) {
            self.call_scratch.push(scratch);
            return (ERROR, None);
        }
        let ty = self.t.mk(TyKind::Tuple(scratch.tys.clone()));
        let value = match self.rep(ty) {
            Ok(Rep::Place(id)) if self.live() => {
                let slot = slot.unwrap_or_else(|| self.result_slot(PlaceTy::Declared(id), n));
                let mut field_places = field_places.into_iter().peekable();
                for (i, (&ty, &value)) in scratch.tys.iter().zip(&scratch.vals).enumerate() {
                    match self.rep(ty) {
                        Ok(Rep::None) => {}
                        Ok(rep) => {
                            let place = rep.stored().unwrap();
                            let address =
                                if field_places.peek().is_some_and(|(field, _)| *field == i) {
                                    field_places.next().unwrap().1
                                } else {
                                    self.address(
                                        AddressBase::Address(slot),
                                        &[Step::Field(i as u32)],
                                        place,
                                        n,
                                    )
                                };
                            self.install(address, rep, value, n);
                        }
                        Err(what) => self.gap(n, what),
                    }
                }
                let value = self.pointer(slot, n);
                for (i, &child) in scratch.vals.iter().enumerate() {
                    self.descriptor_component(
                        Some(value),
                        anchors::Component::Field(i as u32),
                        child,
                    );
                }
                self.temp(ty, Some(value));
                Some(value)
            }
            Err(what) => {
                self.gap(n, what);
                None
            }
            _ => None,
        };
        self.call_scratch.push(scratch);
        (ty, value)
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
                    let (a, b) = (self.shown(w.ty), p.index.symbols.text(name));
                    let message = format!("expected `{a}`, found variant `{b}`");
                    self.refuse_compared("LANG-11", n, b, a, message);
                    (ERROR, None)
                }
                Some(w) if w.ty == ERROR => {
                    self.halt();
                    (ERROR, None)
                }
                // LANG-7: with no expected type a record the current module declares
                // is built by its name, so a case added to core or a dependency never
                // takes it over; otherwise a variant names the one visible type that
                // declares it; its type arguments come from its payloads alone
                // (LANG-12, Q33).
                _ => {
                    let from = p.modules[self.at.module as usize].id;
                    let text = p.index.symbols.text(name);
                    let own = p.index.constructor_candidates(from, name).find_map(|id| {
                        let Some(Def::Type(d)) = p.by_definition[id.0 as usize] else {
                            return None;
                        };
                        let decl = &p.decls[d.index()];
                        match &decl.body {
                            DeclBody::Record(fs) if decl.site.module == from => Some((d, fs)),
                            _ => None,
                        }
                    });
                    if let Some((d, fields)) = own {
                        let want = want.filter(|w| w.payload != 0);
                        return self.construct(n, d, None, fields, true, args, want);
                    }
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
        // FAIL-11, T1.2: a resource literal is itself the `Result`, so under an expected
        // `Result` its name is the resource type's, as with no expected type.
        if found.is_none() && is_core_prelude(&decl.path, &["Result"]) {
            let resource = p.index.constructor_candidates(from, name).find_map(|id| {
                let Some(Def::Type(r)) = p.by_definition[id.0 as usize] else {
                    return None;
                };
                let rd = &p.decls[r.index()];
                match &rd.body {
                    DeclBody::Record(fs) if matches!(rd.repr, Repr::Resource) => Some((r, fs)),
                    _ => None,
                }
            });
            if let Some((r, fields)) = resource {
                return self.construct(n, r, None, fields, true, args, want);
            }
        }
        let Some((case, fields, labelled)) = found else {
            let a = self.shown(want.map_or(ERROR, |w| w.ty));
            let message = format!("expected `{a}`, found variant `{text}`");
            self.push_refusal(
                refusal("LANG-11", self.cause(n), message)
                    .with_comparison(text, a)
                    .with_unresolved(text, NameScope::Variant(decl.path.clone())),
            );
            return (ERROR, None);
        };
        self.construct(n, d, case, fields, labelled, args, want)
    }
    /// Resolve the label against the field and return the ordinary operand.
    pub(super) fn constructor_argument(
        &mut self,
        arg: NodeId,
        field: &crate::decls::FieldDecl,
        labelled: bool,
    ) -> Option<NodeId> {
        let kind = self.kind(arg);
        if !labelled {
            if kind == NodeKind::LabelledArgument {
                self.refuse_label(arg, None, "this payload is not labelled".to_owned());
                return None;
            }
            return Some(arg);
        }
        let name = &field.name;
        if kind == NodeKind::LabelledArgument {
            if self.token_text(arg) == name {
                return Some(self.kids(arg)[0]);
            }
            self.refuse_label(arg, Some(name), format!("expected the field `{name}` here"));
        } else if kind == NodeKind::Path
            && self.item.tree.node(arg).origin.tokens.len() == 1
            && matches!(self.p.target(self.at, arg), Some(Target::Binder(_)))
        {
            let written = self.token_text(arg);
            if written == name {
                return Some(arg);
            }
            self.refuse_compared(
                "LANG-9",
                arg,
                format!("{written}:"),
                format!("{name}:"),
                format!("expected the field `{name}` here"),
            );
        } else {
            self.refuse_label(arg, Some(name), format!("the field `{name}` is labelled"));
        }
        None
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
        if decl.repr == Repr::Region {
            self.refuse(
                "MEM-2",
                n,
                "region literals are written in dest.put or emit",
            );
            return (ERROR, None);
        }
        let k = fields.len();
        if args.len() < k {
            let message = format!("expected {k} arguments");
            self.refuse_arity_at(self.close_brace(n), args.len(), k, message);
            return (ERROR, None);
        }
        if args.len() > k {
            let message = format!("expected {k} arguments");
            self.refuse_arity_at(self.span(args[k]), args.len(), k, message);
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
        let mut destination = None;
        let mut field_places = Vec::new();
        let can_place_fields = self.expression_destination(n).is_none() || !self.may_return(n);
        for (i, (&a, f)) in args.iter().zip(fields).enumerate() {
            let Some(e) = self.constructor_argument(a, f, labelled) else {
                return (ERROR, None);
            };
            let e = match self.kind(e) {
                NodeKind::StepsArgument if !labelled => {
                    self.refuse_compared(
                        "OWN-7",
                        e,
                        "steps",
                        "take",
                        "a payload takes no `steps` mark",
                    );
                    self.kids(e)[0]
                }
                NodeKind::BytesArgument if !labelled => {
                    self.refuse_compared(
                        "OWN-7",
                        e,
                        "bytes",
                        "take",
                        "a payload takes no `bytes` mark",
                    );
                    self.kids(e)[0]
                }
                _ => e,
            };
            if !self.synthesises(e) {
                later.push((i, e));
                continue;
            }
            vals[i] = match self.known(f.ty, &found) {
                Some(t) => {
                    let field = if can_place_fields
                        && lit.is_none()
                        && self.live()
                        && !self.is_view(t)
                        && let Ok(Rep::Place(field)) = self.rep(t)
                        && let Some(ty) = self.known(own, &found)
                        && let Ok(Rep::Place(id)) = self.rep(ty)
                    {
                        let to = *destination
                            .get_or_insert_with(|| self.result_slot(PlaceTy::Declared(id), n));
                        let address = self.field_destination(to, &[field_step(case, i)], field, e);
                        field_places.push((i, address));
                        Some(address)
                    } else {
                        None
                    };
                    let fresh = self.expression_destination(n).is_none();
                    self.expr_into_field(e, expect(t), field, fresh).1
                }
                None => {
                    let (at, v) = if let Some(hint) =
                        want.and_then(|w| self.payload_argument(lit.unwrap_or(own), f.ty, w))
                    {
                        self.expr_hinted(e, Some(hint))
                    } else if self.structured_argument_type(f.ty) {
                        let ty = self.partial(f.ty, &found);
                        self.expr_hinted(e, expect(ty))
                    } else if self.kind(e) == NodeKind::Call {
                        self.out.counts.nodes += 1;
                        let w = self.partial(f.ty, &found);
                        self.call(e, expect(w))
                    } else {
                        self.expr(e, None)
                    };
                    let at = self.outside_vector(e, at, "a field");
                    if at == ERROR {
                        return (ERROR, None);
                    }
                    if !self.unify(f.ty, at, &mut found) {
                        let before = self.partial_rooted(f.ty, &found, &[]);
                        self.refuse_twice(
                            e,
                            at,
                            before,
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
        for (i, &arg) in args.iter().enumerate() {
            let child = if matches!(
                self.kind(arg),
                NodeKind::LabelledArgument | NodeKind::BytesArgument | NodeKind::StepsArgument
            ) {
                self.kids(arg)[0]
            } else {
                arg
            };
            let component = case.map_or(anchors::Component::Field(i as u32), |case| {
                anchors::Component::Case(case, i as u32)
            });
            self.anchor_field(n, component, child);
        }
        let tys: Vec<TyId> = found.into_iter().flatten().collect();
        if let Some(lit) = lit {
            let ty = subst(&mut self.t, lit, &tys);
            let own = self.t.mk(TyKind::Adt(d, tys.clone()));
            let v = self.resource_literal(ty, own, fields, &tys, &vals, n);
            return (ty, v);
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
        let to = destination.unwrap_or_else(|| self.result_slot(PlaceTy::Declared(id), n));
        let mut field_places = field_places.into_iter().peekable();
        if let Some(case) = case {
            self.inst(Inst::SetCase { address: to, case }, n);
        }
        for (i, f) in fields.iter().enumerate() {
            let fty = subst(&mut self.t, f.ty, &tys);
            let r = match self.rep(fty) {
                Ok(r) => r,
                Err(what) => return self.gap_ty(n, what),
            };
            let Some(place) = r.stored() else {
                continue;
            };
            if vals[i].is_none() {
                return self.gap_ty(n, "this field value is not built");
            }
            let at = if field_places.peek().is_some_and(|(field, _)| *field == i) {
                field_places.next().unwrap().1
            } else {
                self.address(AddressBase::Address(to), &[field_step(case, i)], place, n)
            };
            self.install(at, r, vals[i], n);
        }
        let p = self.pointer(to, n);
        for (i, &child) in vals.iter().enumerate() {
            let component = case.map_or(anchors::Component::Field(i as u32), |case| {
                anchors::Component::Case(case, i as u32)
            });
            self.descriptor_component(Some(p), component, child);
        }
        self.temp(ty, Some(p));
        (ty, Some(p))
    }
}

fn field_step(case: Option<u32>, field: usize) -> Step {
    let field = field as u32;
    match case {
        Some(case) => Step::CaseField { case, field },
        None => Step::Field(field),
    }
}

/// The bytes a quoted literal denotes (syntax.md): `".."`, `r#".."#`, and with `bytes`
/// `b".."`, `br#".."#`. Escapes are `\n \t \r \0 \\ \" \'` and `\xNN` (any byte in
/// byte strings), and in strings `\u{..}` as UTF-8; a raw body is its source bytes. The
/// lexer has refused every malformed spelling; None marks one anyway.
pub(crate) fn quoted_bytes(spelling: &str, bytes: bool) -> Option<Vec<u8>> {
    let spelling = if bytes {
        spelling.strip_prefix('b')?
    } else {
        spelling
    };
    if let Some(raw) = spelling.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let body = raw[hashes..].strip_prefix('"')?;
        let body = body.strip_suffix(&raw[..hashes])?.strip_suffix('"')?;
        return Some(body.as_bytes().to_vec());
    }
    let body = spelling.strip_prefix('"')?.strip_suffix('"')?.as_bytes();
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'\\' {
            out.push(body[i]);
            i += 1;
            continue;
        }
        let b = match *body.get(i + 1)? {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'0' => 0,
            c @ (b'\\' | b'"' | b'\'') => c,
            b'x' => {
                let hex = std::str::from_utf8(body.get(i + 2..i + 4)?).ok()?;
                i += 2;
                u8::from_str_radix(hex, 16).ok()?
            }
            b'u' if !bytes => {
                let close = i + body[i..].iter().position(|&c| c == b'}')?;
                let hex = std::str::from_utf8(body.get(i + 3..close)?).ok()?;
                let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                i = close + 1;
                continue;
            }
            _ => return None,
        };
        out.push(b);
        i += 2;
    }
    Some(out)
}
