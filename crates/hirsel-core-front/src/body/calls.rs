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
            NodeKind::Path => p.resolved(self.at, callee).map(|u| &u.target),
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
        let v = if g.generics.is_empty() {
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
                        self.use_place(at, Use::Mut(Hold::CallMut(call)), e);
                        self.arg_ty(pt, at.ty, e, found);
                        self.gap(e, "mut arguments are not built");
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
            Some(t) => self.coerce(at, Want { ty: t, view: false }, e),
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
                TyKind::Param(i) => found[i as usize].unwrap_or(HOLE),
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
                TyKind::Param(i) => found[*i as usize].is_some(),
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
            if missing & ENCODED != 0 {
                self.gap(n, "packed and streamable bounds are not built");
                return;
            }
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
            (TyKind::Param(_), _) if has_hole(&self.t, at) => true,
            (TyKind::Param(i), _) => match s[i as usize] {
                None => {
                    s[i as usize] = Some(at);
                    true
                }
                Some(x) => x == at || x == ERROR,
            },
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
        let mut out: Vec<Arg> = dest.into_iter().map(Arg::Address).collect();
        for (i, p) in g.params.iter().enumerate() {
            let r = match self.rep(p.ty) {
                Ok(Rep::None) => continue,
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
            out.push(match r {
                Rep::Place(id) => {
                    let a = self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n);
                    Arg::Address(a)
                }
                _ => Arg::Value(v),
            });
        }
        let args = PoolRange::push(&mut self.b.pools.args, out);
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

    /// The core integer operations built so far (SEM-1); T and U range over the
    /// integer types (their `integer` bound, checked by LANG-12 at the call).
    pub(super) fn intrinsic(
        &mut self,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let path = &self.p.fns[f].path;
        let name = path.0.name.as_str();
        if !(path.package_is("core", "1") && path.0.modules == ["int"]) {
            self.gap(n, "calls to generic functions are not built");
            return None;
        }
        if tys.contains(&ERROR) {
            return None;
        }
        let ints: Vec<(bool, u32)> = tys.iter().filter_map(|&t| int_of(&self.t, t)).collect();
        if ints.len() != tys.len() {
            // Refused LANG-12 where the type argument was determined.
            return None;
        }
        let arith = |m: ArithMode, op: ArithOp| Some((m, op));
        let op = match name {
            "wrapping_add" => arith(ArithMode::Wrapping, ArithOp::Add),
            "wrapping_sub" => arith(ArithMode::Wrapping, ArithOp::Sub),
            "wrapping_mul" => arith(ArithMode::Wrapping, ArithOp::Mul),
            "saturating_add" => arith(ArithMode::Saturating, ArithOp::Add),
            "saturating_sub" => arith(ArithMode::Saturating, ArithOp::Sub),
            "saturating_mul" => arith(ArithMode::Saturating, ArithOp::Mul),
            _ => None,
        };
        if let Some((mode, op)) = op {
            let ty = value_ty(&self.t, tys[0]).ok().flatten()?;
            let (left, right) = (vals[0]?, vals[1]?);
            if !self.live() {
                return None;
            }
            let result = self.value(ty);
            self.inst(
                Inst::IntArith {
                    result,
                    flag: None,
                    op,
                    mode,
                    left,
                    right,
                },
                n,
            );
            return Some(result);
        }
        let mode = match name {
            "widen" => {
                let ((fs, a), (ts, b)) = (ints[0], ints[1]);
                if !((fs == ts && a <= b) || (!fs && ts && a < b)) {
                    let (x, y) = (self.name(tys[0]), self.name(tys[1]));
                    self.refuse("SEM-1", n, format!("not every `{x}` fits `{y}`"));
                    return None;
                }
                ConvertMode::Widen
            }
            "truncate" if ints[1].1 <= ints[0].1 => ConvertMode::Truncate,
            "truncate" => {
                let (x, y) = (self.name(tys[0]), self.name(tys[1]));
                self.refuse("SEM-1", n, format!("`{y}` is wider than `{x}`"));
                return None;
            }
            _ => {
                self.gap(n, "this core integer operation is not built");
                return None;
            }
        };
        let ty = value_ty(&self.t, tys[1]).ok().flatten()?;
        let operand = vals[0]?;
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
}
