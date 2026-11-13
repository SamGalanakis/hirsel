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
        let (size, align) = self.e.block(rid);
        let grant = self.e.module.types[rid.index()].grant;
        self.admit_with_handback(
            ty,
            admission::OwnedAllocation {
                layout: Some(rid),
                kind: AllocKind::Resource { grant },
                charge,
                size,
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
    /// `lit in r` (MEM-2): the literal of a stable type that is not compact, placed in
    /// region `r`. Its expected type is the placed one's: in `Result[T in r, Capacity]`
    /// it is `T`. Anything but such a literal is refused.
    pub(super) fn placement(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let (lit, at) = (self.kids(n)[0], self.kids(n)[1]);
        let root = self.body_root(at, true);
        if root == ERROR {
            self.halt();
            return (ERROR, None);
        }
        let region = self.expr(at, None).1;
        let inner = want.and_then(|w| match self.t.kind(w.ty) {
            TyKind::Adt(d, args) if Some(*d) == self.p.core_decl("Result") && !args.is_empty() => {
                let ty = match self.t.kind(args[0]) {
                    TyKind::Anchored(x, _) => *x,
                    _ => args[0],
                };
                Some(Want {
                    ty,
                    view: false,
                    payload: 0,
                })
            }
            _ => None,
        });
        let outer = self.placing.replace(Placing {
            lit,
            region,
            root,
            want: inner,
        });
        let r = self.expr(lit, None);
        let unplaced = self.placing.take().is_some();
        self.placing = outer;
        if unplaced && r.0 != ERROR {
            self.refuse(
                "MEM-2",
                n,
                "only the literal of a stable type is placed with `in r`",
            );
            return (ERROR, None);
        }
        r
    }
    /// The literal of `own`, a stable type that is not compact, placed in a region
    /// (MEM-2): its fields, evaluated already, are written into one block admitted in
    /// the region and charged its logical size rounded up to 8 (MEM-24). The value is
    /// `Result[own in r, Capacity]`; on refusal nothing was admitted, and the fields,
    /// plain or stable, need no cleaning.
    #[allow(clippy::too_many_arguments)]
    fn place_literal(
        &mut self,
        own: TyId,
        pl: Placing,
        case: Option<u32>,
        fields: &[crate::decls::FieldDecl],
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> (TyId, Option<ValueId>) {
        let placed = self.t.mk(TyKind::Anchored(own, pl.root));
        let cap = self.capacity_ty(n);
        let Some(result) = self.p.core_decl("Result") else {
            return self.gap_ty(n, "Result is not declared by core");
        };
        let ty = self.t.mk(TyKind::Adt(result, vec![placed, cap]));
        let (id, rid) = match (self.rep(own), self.rep(ty)) {
            (Ok(Rep::Place(id)), Ok(Rep::Place(rid))) => (id, rid),
            (Err(what), _) | (_, Err(what)) => {
                self.gap(n, what);
                return (ty, None);
            }
            _ => return (ty, None),
        };
        let mut reps = vec![];
        for f in fields {
            let fty = subst(&mut self.t, f.ty, tys);
            match self.rep(fty) {
                Ok(r) => reps.push(r),
                Err(what) => {
                    self.gap(n, what);
                    return (ty, None);
                }
            }
        }
        if reps
            .iter()
            .zip(vals)
            .any(|(r, v)| r.stored().is_some() && v.is_none())
        {
            self.gap(n, "this field value is not built");
            return (ty, None);
        }
        let (Some(region), Some(logical)) = (pl.region, self.e.recipes[id.index()].logical_size)
        else {
            return (ty, None);
        };
        if !self.live() {
            return (ty, None);
        }
        let u64t = Ty::Scalar(Scalar::U64);
        let (size, align) = self.e.block(id);
        let Some(charge) = self.konst(u64t, logical.div_ceil(8) * 8, n) else {
            return (ty, None);
        };
        let Some(sz) = self.konst(u64t, size, n) else {
            return (ty, None);
        };
        let to = self.new_slot(PlaceTy::Declared(rid), n);
        let block = self.value(Ty::Ptr);
        let available = self.value(u64t);
        let fail = self.fail_to(n);
        let overflow = self.allocation_layout(id, block, n);
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: AllocKind::Plain,
                place: AllocPlace::Region(region),
                charge,
                overflow,
                size: sz,
                align,
                fail,
            },
            n,
        );
        let Some(null) = self.konst(Ty::Ptr, 0, n) else {
            return (ty, None);
        };
        let admitted = self.test(CompareOp::Ne, block, null, n);
        let (yes, no, join) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.branch(admitted, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let holder = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        self.inst(
            Inst::Store {
                address: holder,
                value: block,
            },
            n,
        );
        let at = self.address(AddressBase::Value(block), &[], PlaceTy::Declared(id), n);
        if let Some(case) = case {
            self.inst(Inst::SetCase { address: at, case }, n);
        }
        for (i, r) in reps.iter().enumerate() {
            let Some(place) = r.stored() else {
                continue;
            };
            let step = match case {
                Some(case) => Step::CaseField {
                    case,
                    field: i as u32,
                },
                None => Step::Field(i as u32),
            };
            let f = self.address(AddressBase::Address(at), &[step], place, n);
            self.install(f, *r, vals[i], n);
        }
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        if self
            .capacity_at(
                AddressBase::Address(to),
                &[Step::CaseField { case: 1, field: 0 }],
                charge,
                available,
                n,
            )
            .is_none()
        {
            return (ty, None);
        }
        self.jump(join, &[], n);
        self.start(join);
        let p = self.pointer(to, n);
        self.temp(ty, Some(p));
        (ty, Some(p))
    }
    /// A byte-string literal is not static (core.md "Literals", c8e2e795): an own `List`
    /// with elements owns its storage, because a `List` has in-place element writes. It
    /// has type `Result[Bytes, Capacity]`, admits its length as `List[u8]` storage and
    /// copies its bytes. The empty literal is `{null, 0, 0}`: capacity 0 means length 0,
    /// so it admits nothing and cannot fail, like `list.with_capacity(0)`.
    fn byte_string(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let p = self.p;
        let (Some(bytes), Some(result), Some(list), Some(cap)) = (
            quoted_bytes(self.token_text(n), true),
            p.core_decl("Result"),
            p.core_decl("List"),
            p.core_decl("Capacity"),
        ) else {
            return self.gap_ty(n, "byte string literal form is not built");
        };
        let list = self.t.mk(TyKind::Adt(list, vec![scalar(Scalar::U8)]));
        let cap = self.t.mk(TyKind::Adt(cap, vec![]));
        let ty = self.t.mk(TyKind::Adt(result, vec![list, cap]));
        let (id, list_id, cap_id) = match (self.rep(ty), self.rep(list), self.rep(cap)) {
            (Ok(Rep::Place(a)), Ok(Rep::Place(b)), Ok(Rep::Place(c))) => (a, b, c),
            (Err(what), _, _) | (_, Err(what), _) | (_, _, Err(what)) => {
                return self.gap_ty(n, what);
            }
            _ => return (ty, None),
        };
        if !self.live() {
            return (ty, None);
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let u64t = Ty::Scalar(Scalar::U64);
        let len = bytes.len() as u64;
        let (Some(length), Some(null)) = (self.konst(u64t, len, n), self.konst(Ty::Ptr, 0, n))
        else {
            return (ty, None);
        };
        // Ok: the carrier {storage, length, capacity = length}.
        let ok = |cx: &mut Self, block: ValueId| {
            cx.inst(
                Inst::SetCase {
                    address: to,
                    case: 0,
                },
                n,
            );
            let carrier = cx.address(
                AddressBase::Address(to),
                &[Step::CaseField { case: 0, field: 0 }],
                PlaceTy::Declared(list_id),
                n,
            );
            for (k, value) in [block, length, length].into_iter().enumerate() {
                let Some(offset) = cx.konst(u64t, 8 * k as u64, n) else {
                    return;
                };
                let at = cx.address(
                    AddressBase::Address(carrier),
                    &[Step::Byte(offset)],
                    PlaceTy::Value(if k == 0 { Ty::Ptr } else { u64t }),
                    n,
                );
                cx.inst(Inst::Store { address: at, value }, n);
            }
        };
        if bytes.is_empty() {
            ok(self, null);
        } else {
            let Some(charge) = self.konst(u64t, len.next_multiple_of(8), n) else {
                return (ty, None);
            };
            // MEM-24: `List[u8]` storage of capacity `len`, charged rounded up to 8; the
            // block is that rounded size so the bytes go in as whole words.
            let block = self.value(Ty::Ptr);
            let available = self.value(u64t);
            self.inst(
                Inst::Alloc {
                    result: block,
                    available,
                    region_entry: None,
                    kind: AllocKind::Plain,
                    place: AllocPlace::Current,
                    charge,
                    overflow: None,
                    size: charge,
                    align: 8,
                    fail: None,
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
            let (yes, no, join) = (
                self.new_block(&[]),
                self.new_block(&[]),
                self.new_block(&[]),
            );
            let (ye, ne) = (self.edge(yes, &[]), self.edge(no, &[]));
            self.end(
                Terminator::Branch {
                    condition: admitted,
                    yes: ye,
                    no: ne,
                },
                Some(n),
            );
            self.start(yes);
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
            ok(self, block);
            self.jump(join, &[], n);
            // Error: `Capacity { requested, available }`.
            self.start(no);
            self.inst(
                Inst::SetCase {
                    address: to,
                    case: 1,
                },
                n,
            );
            let err = self.address(
                AddressBase::Address(to),
                &[Step::CaseField { case: 1, field: 0 }],
                PlaceTy::Declared(cap_id),
                n,
            );
            for (k, value) in [charge, available].into_iter().enumerate() {
                let at = self.address(
                    AddressBase::Address(err),
                    &[Step::Field(k as u32)],
                    PlaceTy::Value(u64t),
                    n,
                );
                self.inst(Inst::Store { address: at, value }, n);
            }
            self.jump(join, &[], n);
            self.start(join);
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
        let viewed = want.is_some_and(|w| matches!(self.t.kind(w.ty), TyKind::View(x) if *x == ty));
        let shown = if viewed {
            self.t.mk(TyKind::View(ty))
        } else {
            ty
        };
        let id = match self.rep(ty) {
            Ok(Rep::Place(id)) => id,
            Err(what) => return self.gap_ty(n, what),
            _ => return (shown, None),
        };
        if !self.live() {
            return (shown, None);
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let u64t = Ty::Scalar(Scalar::U64);
        let start = if data.is_empty() {
            self.konst(Ty::Ptr, 0, n)
        } else {
            let d = self.e.static_bytes(&data);
            let v = self.value(Ty::Ptr);
            self.inst(Inst::ReadStatic { result: v, data: d }, n);
            Some(v)
        };
        let (Some(start), Some(length), Some(zero)) = (
            start,
            self.konst(u64t, data.len() as u64, n),
            self.konst(u64t, 0, n),
        ) else {
            return (shown, None);
        };
        for (k, (value, t)) in [(start, Ty::Ptr), (length, u64t), (zero, u64t)]
            .into_iter()
            .enumerate()
        {
            let Some(offset) = self.konst(u64t, 8 * k as u64, n) else {
                return (shown, None);
            };
            let at = self.address(
                AddressBase::Address(to),
                &[Step::Byte(offset)],
                PlaceTy::Value(t),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        let v = self.pointer(to, n);
        if !viewed {
            self.temp(ty, Some(v));
        }
        (shown, Some(v))
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
            Some(TokenKind::Bytes) => return self.byte_string(n),
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
            Target::Binder(_) => self.place_value(n, want),
            Target::Missing => {
                self.halt();
                (ERROR, None)
            }
            &Target::Definition(id) if self.p.constant_ids[id.0 as usize].is_some() => {
                let constant = self.p.constant_ids[id.0 as usize].unwrap();
                let Some(value) = self.p.constants[constant as usize].value else {
                    self.halt();
                    return (ERROR, None);
                };
                let mut value = value;
                for token in
                    &self.item.tree.tokens[u.projections.start as usize..u.projections.end as usize]
                {
                    if token.kind == TokenKind::Ident {
                        let field = match self.p.constant_field(
                            self.p.modules[self.at.module as usize].id,
                            value,
                            self.p.index.symbols.text(token.symbol),
                        ) {
                            Ok(field) => field,
                            Err(rule) => {
                                self.refuse(rule, n, "constant field is absent or opaque");
                                return (ERROR, None);
                            }
                        };
                        value = field;
                    }
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
        for (i, &field) in fields.iter().enumerate() {
            let expected = want.and_then(|w| match self.t.kind(w.ty) {
                TyKind::Tuple(xs) if w.payload == 0 => xs.get(i).copied(),
                _ => None,
            });
            let (ty, value) = self.expr(field, expected.and_then(expect));
            scratch.tys.push(ty);
            scratch.vals.push(value);
            self.anchor_field(n, anchors::Component::Field(i as u32), field);
        }
        let ty = self.t.mk(TyKind::Tuple(scratch.tys.clone()));
        let value = match self.rep(ty) {
            Ok(Rep::Place(id)) if self.live() => {
                let slot = self.new_slot(PlaceTy::Declared(id), n);
                for (i, (&ty, &value)) in scratch.tys.iter().zip(&scratch.vals).enumerate() {
                    match self.rep(ty) {
                        Ok(Rep::None) => {}
                        Ok(rep) => {
                            let place = rep.stored().unwrap();
                            let address = self.address(
                                AddressBase::Address(slot),
                                &[Step::Field(i as u32)],
                                place,
                                n,
                            );
                            self.install(address, rep, value, n);
                        }
                        Err(what) => self.gap(n, what),
                    }
                }
                let value = self.pointer(slot, n);
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
        // MEM-2: a stable value is made by a builder, a call, or its literal placed in
        // a region; a compact one only by a builder or a clone (MEM-9).
        let placed = match self.placing {
            Some(pl) if pl.lit == n && matches!(decl.repr, Repr::Stable) => self.placing.take(),
            _ => None,
        };
        if matches!(decl.repr, Repr::Stable) {
            if decl.compact.is_some() {
                self.refuse(
                    "MEM-9",
                    n,
                    "a compact value is built only by a builder or a clone",
                );
                return (ERROR, None);
            }
            if placed.is_none() {
                self.refuse(
                    "MEM-2",
                    n,
                    "a stable value is made by its literal placed `in r`, a builder or a call",
                );
                return (ERROR, None);
            }
        }
        let want = match placed {
            Some(pl) => pl.want,
            None => want,
        };
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
        // A placed literal's stable fields are in its region unless they name another
        // (MEM-6).
        let ftys: Vec<TyId> = fields
            .iter()
            .map(|f| match placed {
                Some(pl) => reroot(&mut self.t, f.ty, Root::Enclosing, pl.root),
                None => f.ty,
            })
            .collect();
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
                // A payload is a `take` parameter, never a `bytes` one (OWN-7).
                (NodeKind::BytesArgument, _) => {
                    self.refuse("OWN-7", a, "a payload takes no `bytes` mark");
                    self.kids(a)[0]
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
            vals[i] = match self.known(ftys[i], &found) {
                Some(t) => self.expr(e, expect(t)).1,
                None => {
                    let (at, v) = if let Some(hint) =
                        want.and_then(|w| self.payload_argument(lit.unwrap_or(own), ftys[i], w))
                    {
                        self.expr_hinted(e, Some(hint))
                    } else if self.kind(e) == NodeKind::Call {
                        self.out.counts.nodes += 1;
                        let w = self.partial(ftys[i], &found);
                        self.call(e, expect(w))
                    } else {
                        self.expr(e, None)
                    };
                    if !self.unify(ftys[i], at, &mut found) {
                        self.refuse(
                            "LANG-11",
                            e,
                            "a type parameter is determined twice differently",
                        );
                    } else if let Some(t) = self.known(ftys[i], &found) {
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
            match self.known(ftys[i], &found) {
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
                NodeKind::LabelledArgument | NodeKind::BytesArgument
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
        if let Some(pl) = placed {
            let own = self.t.mk(TyKind::Adt(d, tys.clone()));
            return self.place_literal(own, pl, case, fields, &tys, &vals, n);
        }
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
            let Some(place) = r.stored() else {
                continue;
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

/// The bytes a quoted literal denotes (syntax.md): `".."`, `r#".."#`, and with `bytes`
/// `b".."`, `br#".."#`. Escapes are `\n \t \r \0 \\ \" \'` and `\xNN` (any byte in
/// byte strings), and in strings `\u{..}` as UTF-8; a raw body is its source bytes. The
/// lexer has refused every malformed spelling; None marks one anyway.
fn quoted_bytes(spelling: &str, bytes: bool) -> Option<Vec<u8>> {
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
