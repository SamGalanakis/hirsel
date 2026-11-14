use super::*;

#[derive(Default)]
pub(super) struct CallScratch {
    pub(super) found: Vec<Option<TyId>>,
    pub(super) checked: Vec<bool>,
    pub(super) vals: Vec<Option<ValueId>>,
    pub(super) later: Vec<(usize, NodeId)>,
    pub(super) tys: Vec<TyId>,
    pub(super) params: Vec<(Mode, TyId)>,
    pub(super) roots: Vec<Option<TyId>>,
    pub(super) ir_params: Vec<Param>,
    pub(super) ir_results: Vec<Ty>,
    pub(super) ir_args: Vec<Arg>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
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
                    return self.call_value(n, callee, args, want, scratch);
                }
                other => other.map(|u| &u.target),
            },
            _ => None,
        };
        let f = match target {
            Some(Target::Definition(id)) => match p.by_definition[id.0 as usize].as_ref() {
                Some(Def::Fn(f)) => *f as usize,
                Some(Def::Type(d)) => return self.construct_record(n, *d, want),
                // A constant is a value, so use the ordinary callable type check.
                None => return self.call_value(n, callee, args, want, scratch),
            },
            Some(Target::Missing) if self.kind(callee) == NodeKind::Path => {
                self.halt();
                return (ERROR, None);
            }
            Some(&Target::ContextualConstructor { name }) => {
                return self.construct_variant(n, name, args, want);
            }
            _ => return self.call_value(n, callee, args, want, scratch),
        };
        // MEM-4: `emit(take w, x)` writes the literal `x` in place; it is a form.
        if self.is_emit(f) {
            return self.emit_form(n, f, args, want);
        }
        if self.is_put(f) {
            return self.put_form(n, f, args, want);
        }
        let g = &p.fns[f];
        // A result rooted in a Region parameter is rooted in the region its argument
        // names (MEM-6); `rooted` lists those parameters.
        let rooted: Vec<usize> = g
            .region_anchors
            .iter()
            .filter_map(|&r| match self.t.kind(r) {
                TyKind::Region(Root::Parameter(i))
                    if matches!(self.t.kind(g.params[*i as usize].ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region)) =>
                {
                    Some(*i as usize)
                }
                _ => None,
            })
            .collect();
        if rooted
            .iter()
            .any(|&i| args.get(i).is_none_or(|&e| self.binder_path(e).is_none()))
        {
            return self.gap_ty(
                n,
                "a call result rooted in a Region expression is not built",
            );
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
        // `)`, an extra one at the first extra argument. A missing capability is CAP-1:
        // authority is reached only through a capability argument.
        if args.len() < g.params.len() {
            let missing = g.params[args.len()..]
                .iter()
                .find_map(|q| match self.t.kind(q.ty) {
                    TyKind::Adt(d, _) if crate::layout::handle(&self.p.decls[d.index()]) => {
                        Some(self.p.decls[d.index()].path.0.name.clone())
                    }
                    _ => None,
                });
            if let Some(name) = missing {
                self.refuse_at(
                    "CAP-1",
                    self.close_brace(n),
                    format!(
                        "no `{name}` argument: authority comes only from a capability argument"
                    ),
                );
                return (ERROR, None);
            }
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
        if g.has_region_parameters {
            self.call_region_roots(g.params.iter().map(|p| p.ty), args, &mut scratch.roots);
        } else {
            scratch.roots.clear();
        }
        let CallScratch {
            roots,
            found,
            checked,
            vals,
            later,
            tys,
            ..
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
            if let Some(result) = self.expected_part(g.result, w) {
                self.unify_rooted(result, w.ty, found, roots);
            }
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
                // A missing or extra mark is refused at the argument (OWN-7); the
                // argument is still typed against its parameter.
                (NodeKind::BytesArgument, false) => {
                    self.refuse("OWN-7", a, "this parameter takes no `bytes` mark");
                    self.kids(a)[0]
                }
                (_, true) => {
                    self.refuse("OWN-7", a, "this parameter needs a `bytes` argument");
                    a
                }
                _ => a,
            };
            if self.argument_rooted(
                g.params[i].mode,
                g.params[i].ty,
                call,
                i,
                e,
                found,
                &mut vals[i],
                roots,
            ) {
                if g.anchors.contains(&(i as u32)) {
                    let fact = self.anchor_argument(e);
                    if fact != 0 {
                        self.anchors.expressions.insert(e, fact);
                    }
                }
                self.activate(vals[i], call, e);
                self.determined(f, found, checked, e);
                continue;
            }
            // A literal operand waits for the other arguments and the result (LANG-21).
            if !self.synthesises(e) {
                later.push((i, e));
                continue;
            }
            let pt = g.params[i].ty;
            vals[i] = match self.known_rooted(pt, found, roots) {
                Some(t) => {
                    let view = g.params[i].mode == Mode::View
                        && !matches!(self.t.kind(t), TyKind::View(_));
                    let w = Want {
                        ty: t,
                        view,
                        payload: 0,
                    };
                    self.expr(e, Some(w)).1
                }
                None => {
                    // A call takes the partly known parameter type as its expected type.
                    let (at, v) = if let Some(hint) =
                        want.and_then(|w| self.payload_argument(g.result, pt, w))
                    {
                        self.expr_hinted(e, Some(hint))
                    } else if matches!(self.kind(e), NodeKind::Closure | NodeKind::ScopedClosure) {
                        let w = Want {
                            ty: self.partial_rooted(pt, found, roots),
                            view: false,
                            payload: 0,
                        };
                        self.expr_hinted(e, Some(w))
                    } else if self.kind(e) == NodeKind::Call {
                        self.out.counts.nodes += 1;
                        let w = Want {
                            ty: self.partial_rooted(pt, found, roots),
                            view: false,
                            payload: 0,
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
                    if !self.unify_rooted(pt, inferred, found, roots) {
                        self.refuse(
                            "LANG-11",
                            e,
                            if self.holder_payload(inferred).is_some()
                                || self.result_parts(inferred).is_some_and(|(_,ok,_)|self.holder_payload(ok).is_some()) {
                                "a type parameter is determined twice differently; bind the holder with `let` first"
                            } else { "a type parameter is determined twice differently" },
                        );
                    } else if let Some(t) = self.known_rooted(pt, found, roots) {
                        let view = g.params[i].mode == Mode::View
                            && !matches!(self.t.kind(t), TyKind::View(_));
                        self.coerce(
                            at,
                            Want {
                                ty: t,
                                view,
                                payload: 0,
                            },
                            e,
                        );
                    }
                    self.determined(f, found, checked, e);
                    v
                }
            };
            if g.anchors.contains(&(i as u32)) {
                let fact = self.anchor_argument(e);
                if fact != 0 {
                    self.anchors.expressions.insert(e, fact);
                }
            }
            self.activate(vals[i], call, e);
        }
        for &(i, e) in later.iter() {
            match self.known_rooted(g.params[i].ty, found, roots) {
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
            self.partial_rooted(g.result, found, roots)
        };
        if result == NEVER {
            self.dead = true;
        }
        if !undetermined
            && g.path.package_is("core", "1")
            && g.path.0.modules == ["core"]
            && matches!(g.path.0.name.as_str(), "clone" | "clone_in")
            && let Some(why) = self.clone_refusal(tys[0], tys[1])
        {
            self.refuse("MEM-17", n, why);
            return (result, None);
        }
        let builtin = g.path.package_is("core", "1")
            && g.body.is_none()
            && (g.path.0.modules == ["int"]
                || g.path.0.modules == ["char"]
                || g.path.0.modules == ["float"]
                || g.path.0.modules == ["span"]
                || g.path.0.modules == ["seq"]
                || g.path.0.modules == ["indexed"]
                || g.path.0.modules == ["writer"]
                || g.path.0.modules == ["text"]
                || (g.path.0.modules == ["core"]
                    && matches!(
                        g.path.0.name.as_str(),
                        "build_in"
                            | "build_stream_in"
                            | "finish"
                            | "finish_trim"
                            | "fail"
                            | "clone"
                            | "clone_in"
                    )));
        let v = if builtin {
            self.intrinsic(f, tys, vals, n)
        } else {
            self.emit_call(f, tys, vals, result, n)
        };
        if self.type_kind(result) == Kind::Scoped {
            let mut fact = 0;
            for &i in &g.anchors {
                let argument = self.anchor_argument(args[i as usize]);
                fact = self.anchors.union(fact, argument);
            }
            if fact != 0 {
                self.anchors.expressions.insert(n, fact);
            }
        }
        (result, v)
    }

    pub(super) fn call_region_roots(
        &mut self,
        params: impl IntoIterator<Item = TyId>,
        args: &[NodeId],
        roots: &mut Vec<Option<TyId>>,
    ) {
        roots.clear();
        for (i, ty) in params.into_iter().enumerate() {
            if matches!(self.t.kind(ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region))
                && let Some(&arg) = args.get(i)
                && self.binder_path(arg).is_some()
            {
                roots.resize(i + 1, None);
                roots[i] = Some(self.body_root(arg, true));
            }
        }
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
        self.argument_rooted(mode, pt, call, i, e, found, val, &[])
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn argument_rooted(
        &mut self,
        mode: Mode,
        pt: TyId,
        call: u32,
        i: usize,
        e: NodeId,
        found: &mut [Option<TyId>],
        val: &mut Option<ValueId>,
        roots: &[Option<TyId>],
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
                    Ok(Some(mut at)) => {
                        if let Some(payload) = self.payload_argument_at(at, pt, found, true) {
                            at = payload;
                        }
                        if at.shared {
                            self.refuse("OWN-8", e, "nothing is written through a view");
                            return true;
                        }
                        self.use_place(at, Use::Mut(Hold::CallMut(call)), e);
                        self.arg_ty_rooted(pt, at.ty, e, found, roots);
                        if self.contains_callback(at.ty)
                            || self.live() && self.contains_callback(pt)
                        {
                            self.gap(
                                e,
                                "capture descriptor updates through mut arguments are not built",
                            );
                            return true;
                        }
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
                let (t, v) = self.expr(e, None);
                let t = match self.t.kind(t) {
                    TyKind::View(x)
                        if mode == Mode::View && !matches!(self.t.kind(pt), TyKind::View(_)) =>
                    {
                        *x
                    }
                    _ => t,
                };
                // A plain parameter takes the value the view reads (OWN-7).
                *val = match self.rep(t) {
                    Ok(Rep::Scalar(_)) => self.read_plain_view(t, v, e),
                    _ => v,
                };
                self.arg_ty_rooted(pt, t, e, found, roots);
                true
            }
            Mode::View | Mode::Take if unary.is_none() => {
                let mut at = match self.place(e) {
                    Ok(Some(at)) => at,
                    Ok(None) => return false,
                    Err(()) => return true,
                };
                let projected = mode == Mode::View
                    && self
                        .payload_argument_at(at, pt, found, false)
                        .map(|payload| {
                            at = payload;
                        })
                        .is_some();
                let ty = at.ty;
                if (projected && (self.is_view(ty) || !self.copies(ty)))
                    || (at.place.len > 0
                        && self.is_view(ty)
                        && kind(
                            &self.p.decls,
                            &self.t,
                            self.var(at.place.slot).ty,
                            &self.params,
                        ) != Kind::Scoped)
                {
                    self.use_place(at, Use::View(Hold::CallView(call, i as u32)), e);
                    *val = if matches!(self.rep(ty), Ok(Rep::Place(_))) {
                        self.place_ptr(at, e)
                    } else {
                        self.emit_place(at, e)
                    };
                } else if self.copies(ty) {
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
                    TyKind::View(x)
                        if mode == Mode::View && !matches!(self.t.kind(pt), TyKind::View(_)) =>
                    {
                        *x
                    }
                    _ => ty,
                };
                if mode == Mode::View && t != ty && self.copies(t) {
                    *val = self.read_plain_view(t, *val, e);
                }
                self.arg_ty_rooted(pt, t, e, found, roots);
                true
            }
            _ => false,
        }
    }
    /// An argument of type `at` for a parameter of type `pt` (LANG-11).
    pub(super) fn arg_ty(&mut self, pt: TyId, at: TyId, e: NodeId, found: &mut [Option<TyId>]) {
        self.arg_ty_rooted(pt, at, e, found, &[])
    }
    fn arg_ty_rooted(
        &mut self,
        pt: TyId,
        at: TyId,
        e: NodeId,
        found: &mut [Option<TyId>],
        roots: &[Option<TyId>],
    ) {
        match self.known_rooted(pt, found, roots) {
            Some(t) => {
                let view = self.type_kind(t) == Kind::Stable;
                self.coerce(
                    at,
                    Want {
                        ty: t,
                        view,
                        payload: 0,
                    },
                    e,
                );
            }
            None => {
                if !self.unify_rooted(pt, at, found, roots) {
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
        self.partial_rooted(pt, s, &[])
    }
    // Substituted caller types and roots are atoms. Walking into them again would
    // confuse a caller's Parameter/Generic index with a callee's index.
    pub(super) fn partial_rooted(
        &mut self,
        pt: TyId,
        s: &[Option<TyId>],
        roots: &[Option<TyId>],
    ) -> TyId {
        fn fill(
            t: &mut impl TyRead,
            ty: TyId,
            found: &[Option<TyId>],
            roots: &[Option<TyId>],
        ) -> TyId {
            match t.kind(ty).clone() {
                TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => {
                    found[i as usize].unwrap_or(HOLE)
                }
                TyKind::Region(Root::Parameter(i)) => {
                    roots.get(i as usize).copied().flatten().unwrap_or(ty)
                }
                TyKind::Anchored(x, root) => {
                    let x = fill(t, x, found, roots);
                    let root = fill(t, root, found, roots);
                    t.mk(TyKind::Anchored(x, root))
                }
                TyKind::Array(x, n) => {
                    let x = fill(t, x, found, roots);
                    t.mk(TyKind::Array(x, n))
                }
                TyKind::Tuple(xs) => {
                    let xs = xs.into_iter().map(|x| fill(t, x, found, roots)).collect();
                    t.mk(TyKind::Tuple(xs))
                }
                TyKind::Adt(d, xs) => {
                    let xs = xs.into_iter().map(|x| fill(t, x, found, roots)).collect();
                    t.mk(TyKind::Adt(d, xs))
                }
                TyKind::View(x) => {
                    let x = fill(t, x, found, roots);
                    t.mk(TyKind::View(x))
                }
                TyKind::Fn(ps, r, w, k) => {
                    let ps = ps
                        .into_iter()
                        .map(|(m, p)| (m, fill(t, p, found, roots)))
                        .collect();
                    let r = fill(t, r, found, roots);
                    t.mk(TyKind::Fn(ps, r, w, k))
                }
                _ => ty,
            }
        }
        if roots.is_empty() && !has_param(&self.t, pt) {
            return pt;
        }
        fill(&mut self.t, pt, s, roots)
    }
    /// `pt` with the type arguments found so far, when they determine it.
    pub(super) fn known(&mut self, pt: TyId, s: &[Option<TyId>]) -> Option<TyId> {
        self.known_rooted(pt, s, &[])
    }
    pub(super) fn known_rooted(
        &mut self,
        pt: TyId,
        s: &[Option<TyId>],
        roots: &[Option<TyId>],
    ) -> Option<TyId> {
        fn ready(t: &impl TyRead, ty: TyId, found: &[Option<TyId>]) -> bool {
            match t.kind(ty) {
                TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => found[*i as usize].is_some(),
                TyKind::Anchored(x, root) => ready(t, *x, found) && ready(t, *root, found),
                TyKind::Array(x, _) => ready(t, *x, found),
                TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().all(|&x| ready(t, x, found)),
                TyKind::View(x) => ready(t, *x, found),
                TyKind::Fn(ps, r, _, _) => {
                    ps.iter().all(|&(_, p)| ready(t, p, found)) && ready(t, *r, found)
                }
                _ => true,
            }
        }
        ready(&self.t, pt, s).then(|| self.partial_rooted(pt, s, roots))
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
            if !p.fns[f].generics[i].region
                && kind(&p.decls, &self.t, t, &self.params) == Kind::Scoped
            {
                // OWN-19: only a user-declared generic refuses a scoped type argument. A
                // core operation takes one, unless its signature would make the scoped
                // value an element of a collection or a field of a non-scoped type.
                let g = &p.fns[f];
                let core = g.path.package_is("core", "1");
                if !core
                    || g.params
                        .iter()
                        .map(|q| q.ty)
                        .chain([g.result])
                        .any(|ty| contained(&p.decls, &self.t, ty, i as u32, false))
                {
                    self.refuse("OWN-19", n, "a type parameter cannot be a scoped type");
                    return;
                }
            }
            let g = &p.fns[f].generics[i];
            let missing = g.bounds & !has_bounds(&p.decls, &self.t, t, &self.params);
            if missing != 0 {
                let (a, b) = (self.name(t), crate::decls::bound_names(missing));
                let transfer = p.fns[f].path.package_is("core", "1")
                    && matches!(p.fns[f].path.0.modules.as_slice(), [module] if module == "task" || module == "channel");
                let rule = if transfer && missing & crate::decls::SEND != 0 {
                    "CONC-2"
                } else {
                    "LANG-12"
                };
                self.refuse(rule, n, format!("`{a}` is not {b}"));
                return;
            }
        }
    }
    pub(super) fn unify(&mut self, pt: TyId, at: TyId, s: &mut [Option<TyId>]) -> bool {
        self.unify_rooted(pt, at, s, &[])
    }
    fn unify_rooted(
        &mut self,
        pt: TyId,
        at: TyId,
        s: &mut [Option<TyId>],
        roots: &[Option<TyId>],
    ) -> bool {
        // A diverging operand constrains nothing (LANG-11); a `Never` written inside a
        // type, as in an expected `Option[Never]`, is a type like any other.
        if at == NEVER {
            return true;
        }
        self.unify_part(pt, at, s, roots)
    }
    fn unify_part(
        &mut self,
        pt: TyId,
        at: TyId,
        s: &mut [Option<TyId>],
        roots: &[Option<TyId>],
    ) -> bool {
        self.out.counts.types += 1;
        if at == ERROR || at == HOLE {
            return true;
        }
        match (self.t.kind(pt).clone(), self.t.kind(at).clone()) {
            (TyKind::Region(Root::Parameter(i)), _)
                if let Some(root) = roots.get(i as usize).copied().flatten() =>
            {
                root == at
            }
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
            (TyKind::Tuple(a), TyKind::Tuple(b)) if a.len() == b.len() => a
                .iter()
                .zip(&b)
                .all(|(&x, &y)| self.unify_part(x, y, s, roots)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e && a.len() == b.len() => a
                .iter()
                .zip(&b)
                .all(|(&x, &y)| self.unify_part(x, y, s, roots)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"])
                    && a.len() == b.len() =>
            {
                a.iter()
                    .zip(&b)
                    .all(|(&x, &y)| self.unify_part(x, y, s, roots))
            }
            (TyKind::Array(a, n), TyKind::Array(b, m)) if n == m => self.unify_part(a, b, s, roots),
            (TyKind::View(a), TyKind::View(b)) => self.unify_part(a, b, s, roots),
            (TyKind::Array(a, n), TyKind::Array(b, m)) if n == m => self.unify_part(a, b, s, roots),
            (TyKind::Anchored(a, ar), TyKind::Anchored(b, br)) => {
                self.unify_part(a, b, s, roots) && self.unify_part(ar, br, s, roots)
            }
            (_, TyKind::Anchored(b, _)) if self.type_kind(b) == Kind::Stable => {
                self.unify_part(pt, b, s, roots)
            }
            (TyKind::Fn(a, ar, aw, ak), TyKind::Fn(b, br, bw, bk))
                if ak == bk && a.len() == b.len() && (!bw || aw) =>
            {
                a.iter()
                    .zip(&b)
                    .all(|(&(am, at), &(bm, bt))| am == bm && self.unify_part(at, bt, s, roots))
                    && self.unify_part(ar, br, s, roots)
            }
            _ => pt == at || pt == ERROR || pt == HOLE,
        }
    }

    pub(super) fn instance_row(
        &mut self,
        f: usize,
        types: &[TyId],
    ) -> Result<(FuncId, SigId), &'static str> {
        use std::hash::{Hash, Hasher};
        if types.is_empty() {
            return self.e.row(self.p, f);
        }
        let mut hash = std::hash::DefaultHasher::new();
        f.hash(&mut hash);
        types.hash(&mut hash);
        let hash = hash.finish();
        let mut next = self.instance_heads.get(&hash).copied();
        while let Some(i) = next {
            let row = &self.instance_rows[i];
            if row.definition == f && row.arguments.get(&self.instance_args) == Some(types) {
                return Ok(row.row);
            }
            next = row.next;
        }
        let row = self.e.instance(self.p, &mut self.t, f, types)?;
        let arguments = PoolRange::push(&mut self.instance_args, types.iter().copied());
        let next = self.instance_heads.insert(hash, self.instance_rows.len());
        self.instance_rows.push(BodyInstance {
            definition: f,
            arguments,
            next,
            row,
        });
        Ok(row)
    }

    pub(super) fn emit_call(
        &mut self,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        if let Some(value) = self.core_operation(f, types, vals, result, n) {
            return value;
        }
        if let Some(value) = self.core_value(f, types, vals, result, n) {
            return value;
        }
        let (func, signature) = match self.instance_row(f, types) {
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
        if !g.generics.is_empty() && g.body.is_none() && g.path.package_is("core", "1") {
            self.gap(n, "this generic core operation is not built");
            return None;
        }
        let sig = &self.e.module.signatures[signature.index()];
        let (dest, result_ty) = (sig.dest, sig.results.first().copied());
        // The destination is the first argument: a fresh place the result is written to.
        let dest = dest.map(|id| self.new_slot(PlaceTy::Declared(id), n));
        let start = self.b.pools.args.len();
        self.b.pools.args.extend(dest.map(Arg::Address));
        for (i, p) in g.params.iter().enumerate() {
            if p.mode == Mode::Take {
                // A1 cleanup: the callee owns a taken argument from entry (T2.3).
                self.consume(vals[i], n);
            }
            let ty = subst(&mut self.t, p.ty, types);
            let r = match self.rep(ty) {
                Ok(Rep::None) if p.mode != Mode::Mut => {
                    continue;
                }
                Ok(r) => r,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            // Unit parameters were skipped above. An emission failure halts the
            // body before this live-call path, so every carried argument has a value.
            let v = vals[i].expect("a live call has every carried argument value");
            let arg = match r {
                _ if p.mode == Mode::Mut => {
                    let id = self.e.type_row(self.p, &mut self.t, ty)?;
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
        let fail = self.fail_to(n);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(func),
            signature,
            args,
            results,
            fail,
        });
        let call = CallId(self.b.calls.len() as u32 - 1);
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            // The callee never returns normally; nothing follows the call (FORM v1a).
            self.end(Terminator::Unreachable, Some(n));
            return None;
        }
        match dest {
            Some(a) => {
                // A1 cleanup: an own result is a temporary until something takes it.
                let p = self.pointer(a, n);
                self.temp(result, Some(p));
                Some(p)
            }
            None => first,
        }
    }

    /// MEM-17: why `clone` or `clone_in` of an `x: T` expecting `U` is refused, if it is.
    /// A value holding a resource, a scoped value or an opaque owner is never cloned;
    /// otherwise `U` (or its twin) is `T` or a payload through `Box`, `Rc` and `Arc`,
    /// outermost first.
    fn clone_refusal(&self, x: TyId, u: TyId) -> Option<&'static str> {
        let unanchored = |t: TyId| match self.t.kind(t) {
            TyKind::Anchored(x, _) => *x,
            _ => t,
        };
        if self.capture_holds(x) & (crate::H_RESOURCE | crate::H_SCOPED | crate::H_OPAQUE) != 0 {
            return Some(
                "a value holding a resource, a scoped value or an opaque owner is never cloned",
            );
        }
        let u = unanchored(u);
        let twin = match self.t.kind(u) {
            TyKind::Adt(d, xs) if xs.is_empty() => {
                crate::decls::twin_of(&self.p.decls, &self.t, *d)
            }
            _ => None,
        };
        let mut part = unanchored(x);
        loop {
            match self.t.kind(part) {
                TyKind::Error | TyKind::Hole => return None,
                _ if part == u => return None,
                TyKind::Adt(d, xs) if Some(*d) == twin && xs.is_empty() => return None,
                TyKind::Adt(d, xs)
                    if matches!(
                        self.p.decls[d.index()].role,
                        Some(crate::Role::Box | crate::Role::Rc | crate::Role::Arc)
                    ) =>
                {
                    part = unanchored(xs[0]);
                }
                _ => return Some("no part of this value is the expected type or its twin"),
            }
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
        if g.path.0.modules == ["core"] && g.path.0.name == "fail" {
            // `fail(msg)` raises with the message it takes (FORM `raise fail`).
            let string = self.p.core_decl("String")?;
            let string = self.t.mk(TyKind::Adt(string, vec![]));
            let Ok(Rep::Place(id)) = self.rep(string) else {
                return None;
            };
            let msg = vals[0]?;
            if !self.live() {
                return None;
            }
            let address = self.address(AddressBase::Value(msg), &[], PlaceTy::Declared(id), n);
            let cleanup = self.fail_to(n);
            self.end(
                Terminator::Raise {
                    cause: hirsel_core_ir::RaiseCause::Fail(address),
                    cleanup,
                },
                Some(n),
            );
            return None;
        }
        if g.path.0.modules == ["char"] {
            return self.char_convert(g.path.0.name == "from_u32", g.result, vals[0]?, n);
        }
        if matches!(g.path.0.modules[0].as_str(), "writer" | "text" | "core") {
            return self.writer_op(f, tys, vals, n);
        }
        if g.path.0.modules == ["seq"] || g.path.0.modules == ["indexed"] {
            if tys.contains(&ERROR) || !self.live() {
                return None;
            }
            return match (g.path.0.modules[0].as_str(), g.path.0.name.as_str()) {
                ("seq", "len") => self.span_parts(vals[0]?, n).map(|p| p.3),
                ("seq", "index_in") => {
                    let result = subst(&mut self.t, g.result, tys);
                    self.seq_index_in(vals[0]?, vals[1]?, tys[0], result, n)
                }
                ("seq", "get") => {
                    let result = subst(&mut self.t, g.result, tys);
                    self.seq_index_get(vals[0]?, vals[1]?, tys[0], result, n)
                }
                (module, "nth" | "get") => {
                    let result = subst(&mut self.t, g.result, tys);
                    self.packed_get(vals[0]?, vals[1]?, tys[0], module == "indexed", result, n)
                }
                _ => {
                    self.gap(n, "this sequence operation is not built");
                    None
                }
            };
        }
        if g.path.0.modules == ["span"] {
            if tys.contains(&ERROR) || !self.live() {
                return None;
            }
            return match g.path.0.name.as_str() {
                "len" => self.span_parts(vals[0]?, n).map(|p| p.3),
                "get" => {
                    let result = subst(&mut self.t, g.result, tys);
                    self.fixed_span_get(vals[0]?, vals[1]?, tys[0], result, n)
                }
                name => {
                    let result = subst(&mut self.t, g.result, tys);
                    self.span_builder_op(name, tys[0], vals, result, n)
                }
            };
        }
        if (g.path.0.modules == ["int"] || g.path.0.modules == ["float"])
            && tys.iter().any(|&ty| has_param(&self.t, ty))
        {
            return if g.path.0.modules == ["int"] {
                self.generic_numeric(f, tys, vals, n)
            } else {
                if self.live() {
                    self.gap(n, "a float operation on a type parameter is not built");
                }
                None
            };
        }
        if g.path.0.modules == ["float"] {
            if tys.contains(&ERROR) {
                return None;
            }
            return self.float_intrinsic(f, tys, vals, n);
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
        if matches!(name, "leading_zeros" | "trailing_zeros" | "count_ones") {
            let operand = vals[0]?;
            if !self.live() {
                return None;
            }
            let result = self.value(ty);
            self.inst(
                Inst::ScalarUnary {
                    result,
                    op: match name {
                        "leading_zeros" => hirsel_core_ir::ScalarUnaryOp::LeadingZeros,
                        "trailing_zeros" => hirsel_core_ir::ScalarUnaryOp::TrailingZeros,
                        _ => hirsel_core_ir::ScalarUnaryOp::CountOnes,
                    },
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
    /// `char.to_u32` widens a char to its code point; `char.from_u32` narrows a u32 to
    /// a char, flagged exactly when it is a Unicode scalar value (FORM #57).
    fn char_convert(
        &mut self,
        from_u32: bool,
        result: TyId,
        operand: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !from_u32 {
            return self.numeric_convert(Ty::Scalar(Scalar::U32), ConvertMode::Widen, operand, n);
        }
        if !self.live() {
            return None;
        }
        let value = self.value(Ty::Scalar(Scalar::Char));
        let ok = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Convert {
                result: value,
                flag: Some(ok),
                operand,
                mode: ConvertMode::TryNarrow,
            },
            n,
        );
        let payload = self.t.mk(TyKind::Scalar(Scalar::Char));
        self.numeric_option(result, payload, value, ok, n)
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
        let fail = if op.can_fail(ty) {
            self.fail_to(n)
        } else {
            None
        };
        let result = self.value(ty);
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
        if !self.live() {
            return;
        }
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
            "sqrt" | "abs" | "floor" | "ceil" | "trunc" | "round" | "round_even" => {
                use hirsel_core_ir::ScalarUnaryOp as U;
                let op = match name {
                    "sqrt" => U::Sqrt,
                    "abs" => U::Abs,
                    "floor" => U::Floor,
                    "ceil" => U::Ceil,
                    "trunc" => U::Trunc,
                    "round" => U::Round,
                    _ => U::RoundEven,
                };
                let result = self.value(result_ir?);
                self.inst(
                    Inst::ScalarUnary {
                        result,
                        op,
                        operand,
                    },
                    n,
                );
                return Some(result);
            }
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

/// Whether type parameter `i` occurs in `ty` as, or inside, a type argument of a generic
/// other than Option and Result, or an array element (OWN-19). A callable's parameters
/// and result are not containers.
fn contained(
    decls: &crate::Rows<crate::decls::TypeDecl>,
    t: &impl TyRead,
    ty: TyId,
    i: u32,
    inside: bool,
) -> bool {
    match t.kind(ty) {
        TyKind::Param(j) => inside && *j == i,
        TyKind::Adt(d, args) => {
            let inside = inside || !decls[d.index()].structural;
            args.iter().any(|&a| contained(decls, t, a, i, inside))
        }
        TyKind::Tuple(xs) => xs.iter().any(|&x| contained(decls, t, x, i, inside)),
        TyKind::Array(x, _) => contained(decls, t, *x, i, true),
        TyKind::View(x) | TyKind::Anchored(x, _) => contained(decls, t, *x, i, inside),
        _ => false,
    }
}
