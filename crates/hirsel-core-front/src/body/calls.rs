use super::*;

#[derive(Default)]
pub(super) struct CallScratch {
    found: Vec<Option<TyId>>,
    checked: Vec<bool>,
    vals: Vec<Option<ValueId>>,
    later: Vec<(usize, NodeId)>,
    tys: Vec<TyId>,
}

impl<'c, 'a> Cx<'c, 'a> {
    // ---- calls
    pub(super) fn call(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        let result = self.call_inner(n, want, &mut scratch);
        self.call_scratch.push(scratch);
        result
    }

    fn call_inner(
        &mut self,
        n: NodeId,
        want: Option<Want>,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        self.out.counts.calls += 1;
        let c = self.kids(n);
        let (callee, args) = (c[0], &c[1..]);
        self.out.counts.uses += 1;
        let p: &'c Program<'a> = self.p;
        let target = match self.kind(callee) {
            NodeKind::Path => match p.resolved(self.at, callee) {
                Some(u)
                    if u.projections.start != u.projections.end
                        && matches!(u.target, Target::Definition(_)) =>
                {
                    return self
                        .gap_ty(callee, "function, constant and module values are not built");
                }
                other => other.map(|u| &u.target),
            },
            _ => None,
        };
        let f = match target {
            Some(Target::Definition(id)) => match p.by_definition[id.0 as usize].as_ref() {
                Some(Def::Fn(f)) => *f as usize,
                Some(Def::Type(d)) => return self.construct_record(n, *d, want),
                None => return self.gap_ty(callee, "construction is not built"),
            },
            Some(Target::Missing) | None if self.kind(callee) == NodeKind::Path => {
                self.halt();
                return (ERROR, None);
            }
            Some(&Target::ContextualConstructor { name }) => {
                return self.construct_variant(n, name, args, want);
            }
            _ => return self.gap_ty(callee, "calls through values are not built"),
        };
        let g = &self.p.fns[f];
        if g.region_anchors.iter().any(|&r| {
            matches!(self.t.kind(r), TyKind::Region(Root::Parameter(i))
                if matches!(self.t.kind(g.params[*i as usize].ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region)))
        }) {
            return self.gap_ty(n, "call result region anchors are not built");
        }
        if self.patterns.guard && g.params.iter().any(|p| p.mode != Mode::View) {
            self.refuse(
                "LANG-17",
                n,
                "a match guard cannot call a take or mut parameter",
            );
            return (ERROR, None);
        }
        // LANG-11: one argument per parameter; a missing one is refused at the call's
        // `)`, an extra one at the first extra argument.
        if args.len() < g.params.len() {
            let k = g.params.len();
            self.refuse_at(
                "LANG-11",
                self.close_brace(n),
                format!("expected {k} arguments"),
            );
            return (ERROR, None);
        }
        if args.len() > g.params.len() {
            let k = g.params.len();
            self.refuse("LANG-11", args[k], format!("expected {k} arguments"));
            return (ERROR, None);
        }
        if g.waits {
            self.waited = true;
            if !self.fd.waits && !self.entry {
                self.refuse(
                    "LANG-8",
                    n,
                    "calls a `waits` function but is not declared `waits`",
                );
            }
        }
        let CallScratch {
            found,
            checked,
            vals,
            later,
            tys,
        } = scratch;
        found.clear();
        found.resize(g.generics.len(), None);
        checked.clear();
        checked.resize(g.generics.len(), false);
        vals.clear();
        vals.resize(args.len(), None);
        later.clear();
        tys.clear();
        self.call_seq += 1;
        let call = self.call_seq;
        let holds = self.freezes.len();
        // One forward visit (LANG-11): the expected result fixes what it can, then each
        // argument left to right, then the unsuffixed literal arguments.
        if let Some(w) = want {
            self.unify(g.result, w.ty, found);
            self.determined(f, found, checked, n);
        }
        for (i, &a) in args.iter().enumerate() {
            let e = match (self.kind(a), g.params[i].bytes) {
                (NodeKind::LabelledArgument, _) => {
                    // The resolver already refuses labels on ordinary calls (LANG-9).
                    self.halt();
                    continue;
                }
                (NodeKind::BytesArgument, true) => self.kids(a)[0],
                (NodeKind::BytesArgument, false) | (_, true) => {
                    self.gap(
                        a,
                        "bytes argument mark does not match its parameter (rule unspecified)",
                    );
                    continue;
                }
                _ => a,
            };
            if self.argument(
                g.params[i].mode,
                g.params[i].ty,
                call,
                i,
                e,
                found,
                &mut vals[i],
            ) {
                self.determined(f, found, checked, e);
                continue;
            }
            // A literal operand waits for the other arguments and the result (LANG-21).
            if !self.synthesises(e) {
                later.push((i, e));
                continue;
            }
            let pt = g.params[i].ty;
            vals[i] = match self.known(pt, found) {
                Some(t) => {
                    let view = g.params[i].mode == Mode::View
                        && !matches!(self.t.kind(t), TyKind::View(_));
                    let w = Want { ty: t, view };
                    self.expr(e, Some(w)).1
                }
                None => {
                    // A call takes the partly known parameter type as its expected type.
                    let (at, v) = if self.kind(e) == NodeKind::Call {
                        self.out.counts.nodes += 1;
                        let w = Want {
                            ty: self.partial(pt, found),
                            view: false,
                        };
                        self.call(e, Some(w))
                    } else {
                        self.expr(e, None)
                    };
                    let inferred = match self.t.kind(at) {
                        TyKind::View(x)
                            if g.params[i].mode == Mode::View
                                && !matches!(self.t.kind(pt), TyKind::View(_)) =>
                        {
                            *x
                        }
                        _ => at,
                    };
                    if !self.unify(pt, inferred, found) {
                        self.refuse(
                            "LANG-11",
                            e,
                            "a type parameter is determined twice differently",
                        );
                    } else if let Some(t) = self.known(pt, found) {
                        let view = g.params[i].mode == Mode::View
                            && !matches!(self.t.kind(t), TyKind::View(_));
                        self.coerce(at, Want { ty: t, view }, e);
                    }
                    self.determined(f, found, checked, e);
                    v
                }
            };
        }
        for &(i, e) in later.iter() {
            match self.known(g.params[i].ty, found) {
                Some(t) => vals[i] = self.expr(e, expect(t)).1,
                None => {
                    self.refuse("LANG-11", e, "an unsuffixed number needs an expected type");
                }
            }
        }
        tys.extend(found.iter().map(|s| s.unwrap_or(ERROR)));
        // An undetermined type argument is refused once; the call's type is then the
        // refused type, so nothing built from it is refused again.
        let undetermined = found.iter().any(Option::is_none);
        if undetermined && !self.broken {
            self.refuse("LANG-12", n, "a type parameter of the callee is still open");
        }
        // The call completes: its arguments' holds end, except a view the scoped result
        // is anchored to, which lasts for the result's extent (OWN-11, OWN-20).
        let anchors = &g.anchors;
        self.unfreeze(holds, |h| match h {
            Hold::CallView(c, i) if c == call => {
                anchors.contains(&i).then_some(Hold::View(Extent::Temp))
            }
            Hold::CallMut(c) if c == call => None,
            h => Some(h),
        });
        let result = if undetermined {
            ERROR
        } else {
            subst(&mut self.t, g.result, tys)
        };
        if result == NEVER {
            self.dead = true;
        }
        let numeric = g.path.package_is("core", "1")
            && (g.path.0.modules == ["int"] || g.path.0.modules == ["float"]);
        let v = if g.generics.is_empty() && !numeric {
            self.emit_call(f, vals, result, n)
        } else {
            self.intrinsic(f, tys, vals, n)
        };
        (result, v)
    }

    /// Check argument `e` for a parameter of mode `mode` and type `pt` when its mode
    /// decides how (OWN-7): a `mut` loan, a place viewed or moved. False when `e` is an
    /// ordinary value, checked by the caller.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn argument(
        &mut self,
        mode: Mode,
        pt: TyId,
        call: u32,
        i: usize,
        e: NodeId,
        found: &mut [Option<TyId>],
        val: &mut Option<ValueId>,
    ) -> bool {
        let unary = (self.kind(e) == NodeKind::Unary).then(|| self.token_text(e));
        match mode {
            Mode::Mut => {
                if unary != Some("mut") {
                    // Refused as written; a place here is neither read nor moved.
                    if !matches!(self.place(e), Ok(Some(_)) | Err(())) {
                        let _ = self.expr(e, None);
                    }
                    self.refuse("OWN-7", e, "an argument for a `mut` parameter is `mut p`");
                    return true;
                }
                let p = self.kids(e)[0];
                match self.place(p) {
                    Ok(Some(at)) => {
                        if at.shared {
                            self.refuse("OWN-8", e, "nothing is written through a view");
                            return true;
                        }
                        self.use_place(at, Use::Mut(Hold::CallMut(call)), e);
                        self.arg_ty(pt, at.ty, e, found);
                        *val = self.place_ptr(at, e);
                    }
                    Ok(None) => {
                        let _ = self.expr(p, None);
                        self.refuse("OWN-7", e, "`mut` of an expression that is not a place");
                    }
                    Err(()) => {}
                }
                true
            }
            _ if unary == Some("mut") => {
                let _ = self.expr(self.kids(e)[0], None);
                self.refuse("OWN-7", e, "`mut` is written only for a `mut` parameter");
                true
            }
            Mode::View if unary == Some("view") => {
                // An explicit view where the parameter views anyway.
                let (t, _) = self.expr(e, None);
                let t = match self.t.kind(t) {
                    TyKind::View(x) if !matches!(self.t.kind(pt), TyKind::View(_)) => *x,
                    _ => t,
                };
                self.arg_ty(pt, t, e, found);
                true
            }
            Mode::View | Mode::Take if unary.is_none() => {
                let at = match self.place(e) {
                    Ok(Some(at)) => at,
                    Ok(None) => return false,
                    Err(()) => return true,
                };
                let ty = at.ty;
                if self.copies(ty) {
                    // A copyable value is copied (OWN-7).
                    self.use_place(at, Use::Value, e);
                    *val = self.emit_place(at, e);
                } else if mode == Mode::Take {
                    let name = self.slot_name(at.place.slot);
                    self.refuse(
                        "OWN-7",
                        e,
                        format!("a moving place is passed as `take {name}`"),
                    );
                    return true;
                } else {
                    // A bare place for an unmarked parameter is a view for the call.
                    self.use_place(at, Use::View(Hold::CallView(call, i as u32)), e);
                    *val = self.place_ptr(at, e);
                }
                let t = match self.t.kind(ty) {
                    TyKind::View(x) if !matches!(self.t.kind(pt), TyKind::View(_)) => *x,
                    _ => ty,
                };
                self.arg_ty(pt, t, e, found);
                true
            }
            _ => false,
        }
    }
    /// An argument of type `at` for a parameter of type `pt` (LANG-11).
    pub(super) fn arg_ty(&mut self, pt: TyId, at: TyId, e: NodeId, found: &mut [Option<TyId>]) {
        match self.known(pt, found) {
            Some(t) => self.coerce(
                at,
                Want {
                    ty: t,
                    view: kind(&self.p.decls, &self.t, t, &self.params) == Kind::Stable,
                },
                e,
            ),
            None => {
                if !self.unify(pt, at, found) {
                    self.refuse(
                        "LANG-11",
                        e,
                        "a type parameter is determined twice differently",
                    );
                }
            }
        }
    }

    /// `pt` with the type arguments found so far, a hole for each one still open.
    pub(super) fn partial(&mut self, pt: TyId, s: &[Option<TyId>]) -> TyId {
        fn fill(t: &mut impl TyRead, ty: TyId, found: &[Option<TyId>]) -> TyId {
            match t.kind(ty).clone() {
                TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => {
                    found[i as usize].unwrap_or(HOLE)
                }
                TyKind::Anchored(x, root) => {
                    let x = fill(t, x, found);
                    let root = fill(t, root, found);
                    t.mk(TyKind::Anchored(x, root))
                }
                TyKind::Array(x, n) => {
                    let x = fill(t, x, found);
                    t.mk(TyKind::Array(x, n))
                }
                TyKind::Tuple(xs) => {
                    let xs = xs.into_iter().map(|x| fill(t, x, found)).collect();
                    t.mk(TyKind::Tuple(xs))
                }
                TyKind::Adt(d, xs) => {
                    let xs = xs.into_iter().map(|x| fill(t, x, found)).collect();
                    t.mk(TyKind::Adt(d, xs))
                }
                TyKind::View(x) => {
                    let x = fill(t, x, found);
                    t.mk(TyKind::View(x))
                }
                TyKind::Fn(ps, r, w) => {
                    let ps = ps
                        .into_iter()
                        .map(|(m, p)| (m, fill(t, p, found)))
                        .collect();
                    let r = fill(t, r, found);
                    t.mk(TyKind::Fn(ps, r, w))
                }
                _ => ty,
            }
        }
        if !has_param(&self.t, pt) {
            return pt;
        }
        fill(&mut self.t, pt, s)
    }
    /// `pt` with the type arguments found so far, when they determine it.
    pub(super) fn known(&mut self, pt: TyId, s: &[Option<TyId>]) -> Option<TyId> {
        fn ready(t: &impl TyRead, ty: TyId, found: &[Option<TyId>]) -> bool {
            match t.kind(ty) {
                TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => found[*i as usize].is_some(),
                TyKind::Anchored(x, root) => ready(t, *x, found) && ready(t, *root, found),
                TyKind::Array(x, _) => ready(t, *x, found),
                TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().all(|&x| ready(t, x, found)),
                TyKind::View(x) => ready(t, *x, found),
                TyKind::Fn(ps, r, _) => {
                    ps.iter().all(|&(_, p)| ready(t, p, found)) && ready(t, *r, found)
                }
                _ => true,
            }
        }
        ready(&self.t, pt, s).then(|| self.partial(pt, s))
    }

    /// Find type arguments from one argument type; false when one is found twice
    /// differently (LANG-11). A mismatch elsewhere is the argument's own refusal.
    /// LANG-12: a type argument that lacks a bound its parameter declares is refused at
    /// the argument or result that determined it; `checked` marks those already seen.
    pub(super) fn determined(
        &mut self,
        f: usize,
        found: &[Option<TyId>],
        checked: &mut [bool],
        n: NodeId,
    ) {
        let p: &'c Program<'a> = self.p;
        for (i, t) in found.iter().enumerate() {
            let Some(t) = *t else { continue };
            if checked[i] {
                continue;
            }
            checked[i] = true;
            let g = &p.fns[f].generics[i];
            let missing = g.bounds & !has_bounds(&p.decls, &self.t, t, &self.params);
            if missing != 0 {
                let (a, b) = (self.name(t), crate::decls::bound_names(missing));
                self.refuse("LANG-12", n, format!("`{a}` is not {b}"));
                return;
            }
        }
    }
    pub(super) fn unify(&mut self, pt: TyId, at: TyId, s: &mut [Option<TyId>]) -> bool {
        self.out.counts.types += 1;
        if at == NEVER || at == ERROR || at == HOLE {
            return true;
        }
        match (self.t.kind(pt).clone(), self.t.kind(at).clone()) {
            // A partly known type fixes nothing whole (LANG-11).
            (TyKind::Param(_) | TyKind::Region(Root::Generic(_)), _) if has_hole(&self.t, at) => {
                true
            }
            (TyKind::Param(i), _) | (TyKind::Region(Root::Generic(i)), TyKind::Region(_)) => {
                match s[i as usize] {
                    None => {
                        s[i as usize] = Some(at);
                        true
                    }
                    Some(x) => x == at || x == ERROR,
                }
            }
            (TyKind::Tuple(a), TyKind::Tuple(b)) if a.len() == b.len() => {
                a.iter().zip(&b).all(|(&x, &y)| self.unify(x, y, s))
            }
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e && a.len() == b.len() => {
                a.iter().zip(&b).all(|(&x, &y)| self.unify(x, y, s))
            }
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"])
                    && a.len() == b.len() =>
            {
                a.iter().zip(&b).all(|(&x, &y)| self.unify(x, y, s))
            }
            (TyKind::View(a), TyKind::View(b)) => self.unify(a, b, s),
            (TyKind::Array(a, n), TyKind::Array(b, m)) if n == m => self.unify(a, b, s),
            (TyKind::Anchored(a, ar), TyKind::Anchored(b, br)) => {
                self.unify(a, b, s) && self.unify(ar, br, s)
            }
            (_, TyKind::Anchored(b, _))
                if kind(&self.p.decls, &self.t, b, &self.params) == Kind::Stable =>
            {
                self.unify(pt, b, s)
            }
            (TyKind::Fn(a, ar, aw), TyKind::Fn(b, br, bw)) if a.len() == b.len() && (!bw || aw) => {
                a.iter()
                    .zip(&b)
                    .all(|(&(am, at), &(bm, bt))| am == bm && self.unify(at, bt, s))
                    && self.unify(ar, br, s)
            }
            _ => pt == at || pt == ERROR || pt == HOLE,
        }
    }

    pub(super) fn emit_call(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (func, signature) = match self.e.row(self.p, f) {
            Ok(row) => row,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        if !self.live() {
            return None;
        }
        let g = &self.p.fns[f];
        let sig = &self.e.module.signatures[signature.index()];
        let (dest, result_ty) = (sig.dest, sig.results.first().copied());
        // The destination is the first argument: a fresh place the result is written to.
        let dest = dest.map(|id| self.new_slot(PlaceTy::Declared(id), n));
        let start = self.b.pools.args.len();
        self.b.pools.args.extend(dest.map(Arg::Address));
        for (i, p) in g.params.iter().enumerate() {
            let r = match self.rep(p.ty) {
                Ok(Rep::None) if p.mode != Mode::Mut => continue,
                Ok(r) => r,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let Some(v) = vals[i] else {
                self.gap(n, "this argument is not built");
                return None;
            };
            let arg = match r {
                _ if p.mode == Mode::Mut => {
                    let id = self.e.type_row(self.p, &mut self.t, p.ty)?;
                    let a = self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n);
                    Arg::Address(a)
                }
                Rep::Place(id) => {
                    let a = self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n);
                    Arg::Address(a)
                }
                _ => Arg::Value(v),
            };
            self.b.pools.args.push(arg);
        }
        let args = PoolRange::new(start as u32, (self.b.pools.args.len() - start) as u32);
        let first = result_ty.map(|t| self.value(t));
        let results = PoolRange::push(&mut self.b.pools.values, first);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(func),
            signature,
            args,
            results,
            fail: None,
        });
        let call = CallId(self.b.calls.len() as u32 - 1);
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            // The callee never returns normally; nothing follows the call (FORM v1a).
            self.end(Terminator::Unreachable, Some(n));
            return None;
        }
        match dest {
            Some(a) => Some(self.pointer(a, n)),
            None => first,
        }
    }

    /// SEM-1/2 numeric operations use the scalar FORM instructions. Wrapping and
    /// checked arithmetic never use an ordinary failing arithmetic instruction.
    pub(super) fn intrinsic(
        &mut self,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let g = &self.p.fns[f];
        if !g.path.package_is("core", "1") {
            self.gap(n, "calls to generic functions are not built");
            return None;
        }
        if g.path.0.modules == ["float"] {
            if tys.contains(&ERROR) {
                return None;
            }
            return self.float_intrinsic(f, tys, vals, n);
        }
        if g.path.0.modules != ["int"] {
            self.gap(n, "calls to generic functions are not built");
            return None;
        }
        if tys.contains(&ERROR) {
            return None;
        }
        let name = g.path.0.name.as_str();
        let input = if tys.is_empty() {
            g.params[0].ty
        } else {
            tys[0]
        };
        let (signed, width) = int_of(&self.t, input)?;
        let ty = value_ty(&self.t, input).ok().flatten()?;
        let op = match name {
            "wrapping_add" => Some((ArithMode::Wrapping, ArithOp::Add)),
            "wrapping_sub" => Some((ArithMode::Wrapping, ArithOp::Sub)),
            "wrapping_mul" => Some((ArithMode::Wrapping, ArithOp::Mul)),
            "saturating_add" => Some((ArithMode::Saturating, ArithOp::Add)),
            "saturating_sub" => Some((ArithMode::Saturating, ArithOp::Sub)),
            "saturating_mul" => Some((ArithMode::Saturating, ArithOp::Mul)),
            "checked_add" => Some((ArithMode::Checked, ArithOp::Add)),
            "checked_sub" => Some((ArithMode::Checked, ArithOp::Sub)),
            "checked_mul" => Some((ArithMode::Checked, ArithOp::Mul)),
            "checked_div" => Some((ArithMode::Checked, ArithOp::Div)),
            _ => None,
        };
        if let Some((mode, op)) = op {
            let (left, right) = (vals[0]?, vals[1]?);
            if !self.live() {
                return None;
            }
            let result = self.value(ty);
            let flag = (mode == ArithMode::Checked).then(|| self.value(Ty::Scalar(Scalar::Bool)));
            self.inst(
                Inst::IntArith {
                    result,
                    flag,
                    op,
                    mode,
                    left,
                    right,
                },
                n,
            );
            return if let Some(flag) = flag {
                let ok = self.emit_unary(UnaryOp::Not, BOOL, Some(flag), n)?;
                let result_ty = subst(&mut self.t, g.result, tys);
                self.numeric_option(result_ty, input, result, ok, n)
            } else {
                Some(result)
            };
        }
        if name == "wrapping_neg" {
            let zero = self.konst(ty, 0, n)?;
            return self.numeric_arith(ty, ArithOp::Sub, zero, vals[0]?, n);
        }
        if name == "wrapping_shl" {
            let mask = self.konst(Ty::Scalar(Scalar::U32), u64::from(width - 1), n)?;
            let count =
                self.numeric_binary(Ty::Scalar(Scalar::U32), BinaryOp::BitAnd, vals[1]?, mask, n)?;
            let mode = if width <= 32 {
                ConvertMode::Truncate
            } else {
                ConvertMode::Widen
            };
            let count = self.numeric_convert(ty, mode, count, n)?;
            return self.numeric_binary(ty, BinaryOp::Shl, vals[0]?, count, n);
        }
        if name == "leading_zeros" {
            let operand = vals[0]?;
            if !self.live() {
                return None;
            }
            let result = self.value(ty);
            self.inst(
                Inst::ScalarUnary {
                    result,
                    op: hirsel_core_ir::ScalarUnaryOp::LeadingZeros,
                    operand,
                },
                n,
            );
            let unsigned = Ty::Scalar(match width {
                8 => Scalar::U8,
                16 => Scalar::U16,
                32 => Scalar::U32,
                _ => Scalar::U64,
            });
            let result = if signed {
                self.numeric_convert(unsigned, ConvertMode::Truncate, result, n)?
            } else {
                result
            };
            let mode = if width > 32 {
                ConvertMode::Truncate
            } else {
                ConvertMode::Widen
            };
            return self.numeric_convert(Ty::Scalar(Scalar::U32), mode, result, n);
        }
        if name == "mul_wide" {
            return self.mul_wide(vals[0]?, vals[1]?, g.result, n);
        }
        if name == "parse" {
            self.gap(n, "core integer parsing is not built");
            return None;
        }
        let target = *tys.get(1)?;
        let target_ty = value_ty(&self.t, target).ok().flatten()?;
        let mode = match name {
            "widen" => {
                let (ts, b) = int_of(&self.t, target)?;
                if !((signed == ts && width <= b) || (!signed && ts && width < b)) {
                    let (x, y) = (self.name(input), self.name(target));
                    self.refuse("SEM-1", n, format!("not every `{x}` fits `{y}`"));
                    return None;
                }
                ConvertMode::Widen
            }
            "truncate" if int_of(&self.t, target)?.1 <= width => ConvertMode::Truncate,
            "truncate" => {
                let (x, y) = (self.name(input), self.name(target));
                self.refuse("SEM-1", n, format!("`{y}` is wider than `{x}`"));
                return None;
            }
            "narrow" | "try_narrow" => ConvertMode::TryNarrow,
            _ => {
                self.gap(n, "this core integer operation is not built");
                return None;
            }
        };
        let operand = vals[0]?;
        if !self.live() {
            return None;
        }
        let result = self.value(target_ty);
        let flag = (mode == ConvertMode::TryNarrow).then(|| self.value(Ty::Scalar(Scalar::Bool)));
        self.inst(
            Inst::Convert {
                result,
                flag,
                operand,
                mode,
            },
            n,
        );
        if let Some(ok) = flag {
            if name == "try_narrow" {
                let result_ty = subst(&mut self.t, g.result, tys);
                return self.numeric_option(result_ty, target, result, ok, n);
            }
            self.require_numeric(ok, n);
        }
        Some(result)
    }
    fn numeric_convert(
        &mut self,
        ty: Ty,
        mode: ConvertMode,
        operand: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        self.inst(
            Inst::Convert {
                result,
                flag: None,
                operand,
                mode,
            },
            n,
        );
        Some(result)
    }
    fn numeric_binary(
        &mut self,
        ty: Ty,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        self.inst(
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail: None,
            },
            n,
        );
        Some(result)
    }
    fn numeric_arith(
        &mut self,
        ty: Ty,
        op: ArithOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        self.inst(
            Inst::IntArith {
                result,
                flag: None,
                op,
                mode: ArithMode::Wrapping,
                left,
                right,
            },
            n,
        );
        Some(result)
    }
    fn numeric_compare(
        &mut self,
        op: CompareOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result,
                op,
                left,
                right,
            },
            n,
        );
        result
    }
    fn numeric_branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
        let (yes, no) = (self.edge(yes, &[]), self.edge(no, &[]));
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }
    fn require_numeric(&mut self, ok: ValueId, n: NodeId) {
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        self.numeric_branch(ok, yes, no, n);
        self.start(no);
        // FORM records Overflow on scalar arithmetic. This failing arithmetic is
        // the narrow operation's failure edge, with the narrow call's source anchor.
        let ty = Ty::Scalar(Scalar::U8);
        let max = self.konst(ty, 255, n).unwrap();
        let one = self.konst(ty, 1, n).unwrap();
        let _ = self.numeric_binary(ty, BinaryOp::Add, max, one, n);
        self.end(Terminator::Propagate, Some(n));
        self.start(yes);
    }
    /// The overflow flag decides which ordinary Option variant is constructed.
    fn numeric_option(
        &mut self,
        ty: TyId,
        payload: TyId,
        value: ValueId,
        ok: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(ty).ok()? {
            Rep::Place(id) => id,
            _ => return None,
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.numeric_branch(ok, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let place = self.place_ty(payload, n)?;
        let at = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            place,
            n,
        );
        let rep = self.rep(payload).ok()?;
        self.install(at, rep, Some(value), n);
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }
    /// Base-2^32 long multiplication. Each limb product fits u64; the two carry
    /// sums are bounded by 2^64-2^32, and high is at most u64::MAX-1.
    fn mul_wide(&mut self, a: ValueId, b: ValueId, tuple: TyId, n: NodeId) -> Option<ValueId> {
        let ty = Ty::Scalar(Scalar::U64);
        let mask = self.konst(ty, 0xffff_ffff, n)?;
        let shift = self.konst(ty, 32, n)?;
        let a0 = self.numeric_binary(ty, BinaryOp::BitAnd, a, mask, n)?;
        let b0 = self.numeric_binary(ty, BinaryOp::BitAnd, b, mask, n)?;
        let a1 = self.numeric_binary(ty, BinaryOp::Shr, a, shift, n)?;
        let b1 = self.numeric_binary(ty, BinaryOp::Shr, b, shift, n)?;
        let w0 = self.numeric_binary(ty, BinaryOp::Mul, a0, b0, n)?;
        let carry = self.numeric_binary(ty, BinaryOp::Shr, w0, shift, n)?;
        let cross = self.numeric_binary(ty, BinaryOp::Mul, a1, b0, n)?;
        let t = self.numeric_binary(ty, BinaryOp::Add, cross, carry, n)?;
        let w1 = self.numeric_binary(ty, BinaryOp::BitAnd, t, mask, n)?;
        let w2 = self.numeric_binary(ty, BinaryOp::Shr, t, shift, n)?;
        let cross = self.numeric_binary(ty, BinaryOp::Mul, a0, b1, n)?;
        let w1 = self.numeric_binary(ty, BinaryOp::Add, w1, cross, n)?;
        let carry = self.numeric_binary(ty, BinaryOp::Shr, w1, shift, n)?;
        let high = self.numeric_binary(ty, BinaryOp::Mul, a1, b1, n)?;
        let high = self.numeric_binary(ty, BinaryOp::Add, high, w2, n)?;
        let high = self.numeric_binary(ty, BinaryOp::Add, high, carry, n)?;
        let low = self.numeric_binary(ty, BinaryOp::Shl, w1, shift, n)?;
        let bottom = self.numeric_binary(ty, BinaryOp::BitAnd, w0, mask, n)?;
        let low = self.numeric_binary(ty, BinaryOp::BitOr, low, bottom, n)?;
        let place = self.place_ty(tuple, n)?;
        let to = self.new_slot(place, n);
        for (i, value) in [high, low].into_iter().enumerate() {
            let at = self.address(
                AddressBase::Address(to),
                &[Step::Field(i as u32)],
                PlaceTy::Value(ty),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        Some(self.pointer(to, n))
    }
    fn float_intrinsic(
        &mut self,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let g = &self.p.fns[f];
        let name = g.path.0.name.as_str();
        let result_ty = subst(&mut self.t, g.result, tys);
        let result_ir = value_ty(&self.t, result_ty).ok().flatten();
        let operand = vals[0]?;
        if !self.live() {
            return None;
        }
        let mode = match name {
            "to_bits32" | "to_bits64" | "from_bits32" | "from_bits64" => ConvertMode::Reinterpret,
            "widen" | "narrow" => ConvertMode::FloatResize,
            "from_int" => ConvertMode::IntToFloat,
            "mul_add" => {
                let result = self.value(result_ir?);
                self.inst(
                    Inst::MulAdd {
                        result,
                        left: operand,
                        right: vals[1]?,
                        addend: vals[2]?,
                    },
                    n,
                );
                return Some(result);
            }
            "to_int" => return self.float_to_int(result_ty, tys[1], operand, n),
            "total_order" => return self.float_total_order(result_ty, operand, vals[1]?, n),
            _ => {
                self.gap(n, "this core float operation is not built");
                return None;
            }
        };
        self.numeric_convert(result_ir?, mode, operand, n)
    }
    fn float_to_int(
        &mut self,
        ty: TyId,
        integer: TyId,
        operand: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let result = self.value(value_ty(&self.t, integer).ok().flatten()?);
        let flag = self.value(Ty::Scalar(Scalar::U8));
        self.inst(
            Inst::Convert {
                result,
                flag: Some(flag),
                operand,
                mode: ConvertMode::FloatToInt,
            },
            n,
        );
        let zero = self.konst(Ty::Scalar(Scalar::U8), 0, n)?;
        let ok = self.numeric_compare(CompareOp::Eq, flag, zero, n);
        let place = self.place_ty(ty, n)?;
        let to = self.new_slot(place, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.numeric_branch(ok, yes, no, n);
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let place = self.place_ty(integer, n)?;
        let at = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            place,
            n,
        );
        self.inst(
            Inst::Store {
                address: at,
                value: result,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let error = self
            .t
            .mk(TyKind::Adt(self.p.core_decl("FloatError")?, vec![]));
        let place = self.place_ty(error, n)?;
        let at = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            place,
            n,
        );
        let one = self.konst(Ty::Scalar(Scalar::U8), 1, n)?;
        let nan = self.numeric_compare(CompareOp::Eq, flag, one, n);
        let nan_block = self.new_block(&[]);
        let range_block = self.new_block(&[]);
        self.numeric_branch(nan, nan_block, range_block, n);
        self.start(nan_block);
        self.inst(
            Inst::SetCase {
                address: at,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(range_block);
        self.inst(
            Inst::SetCase {
                address: at,
                case: 1,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }
    /// IEEE totalOrder becomes unsigned comparison after complementing negative
    /// bits and flipping positive sign bits. SEM-2 canonicalizes every NaN.
    fn float_order_key(&mut self, operand: ValueId, n: NodeId) -> Option<ValueId> {
        let width = if self.b.values[operand.index()] == Ty::Scalar(Scalar::F32) {
            32
        } else {
            64
        };
        let ty = Ty::Scalar(if width == 32 {
            Scalar::U32
        } else {
            Scalar::U64
        });
        let bits = self.numeric_convert(ty, ConvertMode::Reinterpret, operand, n)?;
        let sign = self.konst(ty, 1u64 << (width - 1), n)?;
        let negative = self.numeric_compare(CompareOp::Ge, bits, sign, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[ty]);
        self.numeric_branch(negative, yes, no, n);
        self.start(yes);
        let result = self.value(ty);
        self.inst(
            Inst::Unary {
                result,
                op: UnaryOp::BitNot,
                operand: bits,
                fail: None,
            },
            n,
        );
        self.jump(join, &[result], n);
        self.start(no);
        let result = self.numeric_binary(ty, BinaryOp::BitXor, bits, sign, n)?;
        self.jump(join, &[result], n);
        self.start(join);
        Some(
            self.b.blocks[join.index()]
                .params
                .get(&self.b.pools.values)?[0],
        )
    }
    fn float_total_order(
        &mut self,
        ty: TyId,
        a: ValueId,
        b: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let a = self.float_order_key(a, n)?;
        let b = self.float_order_key(b, n)?;
        let place = self.place_ty(ty, n)?;
        let to = self.new_slot(place, n);
        let less = self.numeric_compare(CompareOp::Lt, a, b, n);
        let lt = self.new_block(&[]);
        let other = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.numeric_branch(less, lt, other, n);
        self.start(lt);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(other);
        let equal = self.numeric_compare(CompareOp::Eq, a, b, n);
        let eq = self.new_block(&[]);
        let gt = self.new_block(&[]);
        self.numeric_branch(equal, eq, gt, n);
        self.start(eq);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(gt);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 2,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }
}
