//! Callable values, using FORM's code pointers and ordinary call rows (LANG-13).
use super::calls::CallScratch;
use super::*;

#[derive(Clone, Copy)]
pub(super) struct CallableAbi {
    signature: SigId,
    params: PoolRange<Rep>,
    result: Rep,
    regions: PoolRange<u32>,
    scoped_result: bool,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    #[inline(always)]
    pub(super) fn isolate_descriptor_forbidden(&mut self, value: Option<ValueId>) -> bool {
        let Some(id) = self.descriptors.of(value) else {
            return false;
        };
        self.isolate_descriptor_forbidden_at(id, None)
    }

    /// A projected capture only carries the projected type's descriptor.  Keep a
    /// callback sibling in a tuple from poisoning an isolate that reads another
    /// field (FAIL-16); a projected callback field still refuses.
    fn isolate_descriptor_forbidden_at(&mut self, id: u32, projected: Option<TyId>) -> bool {
        let range = self.descriptors.ranges[id as usize];
        let captures = self
            .descriptors
            .projected
            .get(&id)
            .cloned()
            .filter(|captures| !captures.is_empty())
            .unwrap_or_else(|| range.get(&self.descriptors.captures).unwrap().to_vec());
        if captures.iter().any(|capture| capture.mode == Mode::Mut) {
            return true;
        }
        let fields = self.descriptors.fields[id as usize]
            .get(&self.descriptors.parts)
            .unwrap()
            .to_vec();
        if fields.is_empty() {
            let projected_path = captures.iter().any(|capture| capture.path.is_some());
            return captures.iter().any(|capture| {
                if projected_path && capture.path.is_none() {
                    return false;
                }
                let var = self.var(capture.slot);
                let ty = projected
                    .or_else(|| self.descriptor_capture_type(*capture, var.ty))
                    .unwrap_or(var.ty);
                matches!(var.origin, Origin::ViewParam | Origin::MutParam)
                    && self.contains_callback(ty)
            });
        }
        fields.iter().any(|(component, child)| {
            let ty = projected.and_then(|ty| match component {
                anchors::Component::Field(i) => self.field_ty(ty, *i),
                _ => None,
            });
            self.isolate_descriptor_forbidden_at(*child, ty)
        })
    }

    fn descriptor_capture_type(&mut self, capture: Capture, root: TyId) -> Option<TyId> {
        let mut path = Vec::new();
        let mut at = capture.path?;
        while at != 0 {
            let step = self.descriptors.paths[at as usize - 1];
            path.push(step.field);
            at = step.parent;
        }
        let mut ty = root;
        for field in path.into_iter().rev() {
            ty = self.field_ty(ty, field)?;
        }
        Some(ty)
    }
    pub(super) fn scoped_callable_view(&self, from: TyId, to: TyId) -> bool {
        if from.0 <= HOLE.0 || to.0 <= HOLE.0 {
            return false;
        }
        if !matches!(
            self.t.kind(to),
            TyKind::Fn(..) | TyKind::View(_) | TyKind::Anchored(..)
        ) {
            return false;
        }
        let TyKind::Fn(b, br, bw, Callable::Scoped) = self.t.kind(self.bare(to)) else {
            return false;
        };
        matches!(self.t.kind(self.bare(from)),
            TyKind::Fn(a, ar, aw, kind)
                if a == b && ar == br && (!aw || *bw)
                    && (matches!(kind, Callable::Own | Callable::OwnSend)
                        || *kind == Callable::Scoped && self.is_view(from)))
    }

    #[inline(always)]
    pub(super) fn plain_callable_fit(&self, from: TyId, to: TyId) -> bool {
        if from.0 <= HOLE.0 || to.0 <= HOLE.0 {
            return false;
        }
        if !matches!(
            self.t.kind(to),
            TyKind::Fn(
                _,
                _,
                _,
                Callable::Own | Callable::OwnSend | Callable::Scoped
            )
        ) {
            return false;
        }
        matches!((self.t.kind(self.bare(from)), self.t.kind(to)),
            (TyKind::Fn(a, ar, aw, Callable::Plain), TyKind::Fn(b, br, bw, Callable::Own | Callable::OwnSend | Callable::Scoped))
                if a == b && ar == br && (!aw || *bw))
    }

    pub(super) fn fit_plain_callable(
        &mut self,
        value: Option<ValueId>,
        to: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let value = value.and_then(|code| self.captureless_callable(code, n));
        if matches!(
            self.t.kind(to),
            TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend)
        ) {
            self.temp(to, value);
        }
        value
    }

    pub(super) fn function_value(
        &mut self,
        f: usize,
        n: NodeId,
        want: Option<Want>,
    ) -> (TyId, Option<ValueId>) {
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        let result = self.function_value_inner(f, n, want, &mut scratch);
        self.call_scratch.push(scratch);
        result
    }

    fn function_value_inner(
        &mut self,
        f: usize,
        n: NodeId,
        want: Option<Want>,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        if want.is_some_and(|w| w.ty == ERROR) {
            return (ERROR, None);
        }
        let g = &self.p.fns[f];
        if g.anchors
            .iter()
            .any(|&i| !self.region_handle(g.params[i as usize].ty))
        {
            self.refuse(
                "LANG-13",
                n,
                "a result anchored to a view parameter fits no callable type",
            );
            return (ERROR, None);
        }
        let ty = g.callable;
        if let TyKind::Fn(_, result, _, _) = self.t.kind(ty)
            && !crate::decls::scoped_result_rooted(&self.p.decls, &self.t, *result, false)
        {
            return self.gap_ty(
                n,
                "scoped callable result region anchors are not represented",
            );
        }
        scratch.found.clear();
        scratch.found.resize(g.generics.len(), None);
        scratch.checked.clear();
        scratch.checked.resize(g.generics.len(), false);
        if let Some(w) = want.filter(|w| w.payload == 0) {
            if let TyKind::Fn(ps, r, _, _) = self.t.kind(w.ty) {
                let (len, r) = (ps.len(), *r);
                for (i, parameter) in g.params.iter().enumerate().take(len) {
                    let TyKind::Fn(ps, _, _, _) = self.t.kind(w.ty) else {
                        unreachable!()
                    };
                    let expected = ps[i].1;
                    self.unify(parameter.ty, expected, &mut scratch.found);
                }
                self.unify(g.result, r, &mut scratch.found);
            }
            self.determined(f, &scratch.found, &mut scratch.checked, n);
        }
        if scratch.found.iter().any(Option::is_none) {
            self.refuse(
                "LANG-12",
                n,
                "a function value's type parameters need an expected callable type",
            );
            return (ERROR, None);
        }
        scratch.tys.clear();
        scratch
            .tys
            .extend(scratch.found.iter().map(|t| t.unwrap_or(ERROR)));
        let ty = subst(&mut self.t, ty, &scratch.tys);
        let Some(function) = self.item_adapter(f, &scratch.tys, ty, n) else {
            return (ty, None);
        };
        let value = if self.live() {
            let result = self.value(Ty::Ptr);
            self.inst(Inst::FunctionRef { result, function }, n);
            Some(result)
        } else {
            None
        };
        (ty, value)
    }

    pub(super) fn call_value(
        &mut self,
        n: NodeId,
        callee: NodeId,
        args: &[NodeId],
        _want: Option<Want>,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        self.call_seq += 1;
        let call = self.call_seq;
        let holds = self.freezes.len();
        let (ty, value) = match self.place(callee) {
            Ok(Some(at)) => {
                self.use_place(at, Use::View(Hold::CallView(call, u32::MAX)), callee);
                (at.ty, self.emit_place(at, callee))
            }
            Ok(None) => self.expr(callee, None),
            Err(()) => return (ERROR, None),
        };
        // Views carry the referent's bytes (FORM #56), including a callable's
        // code/environment pair; invoking one borrows the same callable.
        let ty = self.bare(ty);
        let (result, waits, callable) = match self.t.kind(ty) {
            TyKind::Fn(params, result, waits, callable) => {
                scratch.params.clear();
                scratch.params.extend_from_slice(params);
                (*result, *waits, *callable)
            }
            TyKind::Error => return (ERROR, None),
            _ => {
                let a = self.shown(ty);
                let message = "the value being called is not callable";
                self.refuse_compared("LANG-13", callee, a, "a callable type", message);
                return (ERROR, None);
            }
        };
        if args.len() < scratch.params.len() {
            let message = "a callable needs one argument per parameter".to_owned();
            self.refuse_arity_at(
                self.close_brace(n),
                args.len(),
                scratch.params.len(),
                message,
            );
            return (ERROR, None);
        }
        if args.len() > scratch.params.len() {
            let at = self.span(args[scratch.params.len()]);
            self.refuse_arity_at(
                at,
                args.len(),
                scratch.params.len(),
                "extra callable argument".to_owned(),
            );
            return (ERROR, None);
        }
        if self.patterns.guard
            && scratch
                .params
                .iter()
                .any(|p| matches!(p.0, Mode::Take | Mode::Mut))
        {
            self.refuse(
                "LANG-17",
                n,
                "a match guard cannot call a take or mut parameter",
            );
            return (ERROR, None);
        }
        if waits {
            self.waited = true;
            if !self.fd.waits && !self.entry {
                let message = "invokes a waits callable but is not declared waits";
                self.refuse_compared("LANG-8", n, "not waits", "waits", message);
            }
        }
        let abi = self.value_call_abi(ty, result, n, scratch);
        if let Some(abi) = abi {
            for j in abi.regions.start..abi.regions.start + abi.regions.len {
                let i = self.callable_regions[j as usize] as usize;
                if self.region_argument_path(args[i]).is_none() {
                    return self.gap_ty(
                        n,
                        "a call result rooted in a Region expression is not built",
                    );
                }
            }
        }
        let rooted = abi.is_some_and(|a| a.regions.len != 0);
        let result = if rooted {
            self.call_region_roots(scratch.params.iter().map(|p| p.1), args, &mut scratch.roots);
            for (_, ty) in &mut scratch.params {
                *ty = crate::types::open_callable_roots(&mut self.t, *ty, &scratch.roots);
            }
            crate::types::open_callable_roots(&mut self.t, result, &scratch.roots)
        } else {
            scratch.roots.clear();
            result
        };
        self.activate(value, call, n);
        scratch.vals.clear();
        scratch.vals.resize(args.len(), None);
        scratch.found.clear();
        scratch
            .found
            .extend(self.fd.generics.iter().enumerate().map(|(i, g)| {
                Some(self.t.mk(if g.region {
                    TyKind::Region(Root::Generic(i as u32))
                } else {
                    TyKind::Param(i as u32)
                }))
            }));
        for (i, &arg) in args.iter().enumerate() {
            let (mode, pt) = scratch.params[i];
            let bytes = mode == Mode::Bytes;
            let arg = match (self.kind(arg), bytes) {
                (NodeKind::LabelledArgument, _) => {
                    // LANG-9 is reported by resolution, as on a direct call.
                    self.halt();
                    continue;
                }
                (NodeKind::BytesArgument, true) => self.kids(arg)[0],
                (NodeKind::BytesArgument, false) => {
                    let message = "this parameter takes no `bytes` mark";
                    self.refuse_compared("OWN-7", arg, "bytes", mode.keyword(), message);
                    self.kids(arg)[0]
                }
                (_, true) => {
                    let message = "this parameter needs a `bytes` argument";
                    self.refuse_compared("OWN-7", arg, self.written_mode(arg), "bytes", message);
                    arg
                }
                _ => arg,
            };
            let mode = if bytes { Mode::View } else { mode };
            if !self.argument_rooted(
                mode,
                pt,
                call,
                i,
                arg,
                &mut scratch.found,
                &mut scratch.vals[i],
                &[],
            ) {
                let expected = self.partial(pt, &scratch.found);
                scratch.vals[i] = self
                    .expr(
                        arg,
                        Some(Want {
                            ty: expected,
                            view: mode == Mode::View,
                            payload: 0,
                        }),
                    )
                    .1;
            }
            self.activate(scratch.vals[i], call, arg);
        }
        self.unfreeze(holds, |hold| match hold {
            Hold::CallView(c, _) | Hold::CallMut(c, _) if c == call => None,
            other => Some(other),
        });
        let result = self.partial(result, &scratch.found);
        let result = if abi.is_some_and(|a| a.scoped_result) {
            let fact = self.anchor_regions(result);
            if fact != 0 {
                self.anchors.expressions.insert(n, fact);
            }
            crate::decls::erase_scoped_result(&self.p.decls, &mut self.t, result)
        } else {
            result
        };
        let (code, environment) = if callable == Callable::Plain {
            (value, self.konst(Ty::Ptr, 0, n))
        } else {
            (
                self.callable_word(value, 0, n),
                self.callable_word(value, 8, n),
            )
        };
        let fail = self.fail_to(n);
        let v = self.emit_value_call(code, environment, abi, result, n, scratch, fail);
        if result == NEVER {
            self.dead = true;
        }
        (result, v)
    }

    /// Call the callable `value` of type `ty` from a built-in form with argument values
    /// already lowered (MEM-4 writers); its result value.
    pub(super) fn call_lowered(
        &mut self,
        ty: TyId,
        value: ValueId,
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let fail = self.fail_to(n);
        self.call_lowered_with_fail(ty, value, vals, n, fail)
    }

    /// Call a lowered callable with an explicit failure successor.  Isolation uses
    /// this to send the body's failure directly to its handler, before ordinary
    /// enclosing cleanup runs.
    pub(super) fn call_lowered_fail(
        &mut self,
        ty: TyId,
        value: ValueId,
        n: NodeId,
        fail: BlockId,
    ) -> Option<ValueId> {
        self.call_lowered_with_fail(ty, value, &[], n, Some(fail))
    }

    fn call_lowered_with_fail(
        &mut self,
        ty: TyId,
        value: ValueId,
        vals: &[Option<ValueId>],
        n: NodeId,
        fail: Option<BlockId>,
    ) -> Option<ValueId> {
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        let TyKind::Fn(params, result, _, callable) = self.t.kind(ty).clone() else {
            self.call_scratch.push(scratch);
            self.gap(n, "invocation of this lowered callable type is not built");
            return None;
        };
        scratch.params.clear();
        scratch.params.extend_from_slice(&params);
        scratch.vals.clear();
        scratch.vals.extend_from_slice(vals);
        let abi = self.value_call_abi(ty, result, n, &mut scratch);
        let (code, environment) = if callable == Callable::Plain {
            (Some(value), self.konst(Ty::Ptr, 0, n))
        } else {
            (
                self.callable_word(Some(value), 0, n),
                self.callable_word(Some(value), 8, n),
            )
        };
        let v = self.emit_value_call(code, environment, abi, result, n, &mut scratch, fail);
        self.call_scratch.push(scratch);
        v
    }

    fn callable_word(&mut self, value: Option<ValueId>, offset: u64, n: NodeId) -> Option<ValueId> {
        let base = value?;
        if !self.live() {
            return None;
        }
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
        let address = self.address(
            AddressBase::Value(base),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let result = self.value(Ty::Ptr);
        self.inst(
            Inst::Read {
                result,
                address,
                mode: ReadMode::Bits,
            },
            n,
        );
        Some(result)
    }

    fn value_call_abi(
        &mut self,
        callable: TyId,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
    ) -> Option<CallableAbi> {
        let abi = match self.callable_signatures.get(&callable).copied() {
            Some(abi) => abi,
            None => {
                let signature = match self.indirect_signature(
                    &scratch.params,
                    result,
                    &mut scratch.ir_params,
                ) {
                    Ok(signature) => signature,
                    Err(what) => {
                        self.gap(n, what);
                        return None;
                    }
                };
                let start = self.callable_reps.len();
                for &(_, ty) in &scratch.params {
                    let rep = match self.rep(ty) {
                        Ok(rep) => rep,
                        Err(what) => {
                            self.gap(n, what);
                            return None;
                        }
                    };
                    self.callable_reps.push(rep);
                }
                let result_ty = result;
                let result = match self.rep(result) {
                    Ok(rep) => rep,
                    Err(what) => {
                        self.gap(n, what);
                        return None;
                    }
                };
                let roots = self.callable_regions.len();
                for (i, &(_, ty)) in scratch.params.iter().enumerate() {
                    if self.region_handle(ty)
                        && (self.parameter_region_result(result_ty, i as u32)
                            || scratch
                                .params
                                .iter()
                                .any(|&(_, ty)| self.parameter_region_result(ty, i as u32)))
                    {
                        self.callable_regions.push(i as u32);
                    }
                }
                let abi = CallableAbi {
                    signature,
                    params: PoolRange::new(start as u32, scratch.params.len() as u32),
                    result,
                    scoped_result: matches!(
                        self.type_kind(result_ty),
                        Kind::Scoped | Kind::Generic
                    ),
                    regions: PoolRange::new(
                        roots as u32,
                        (self.callable_regions.len() - roots) as u32,
                    ),
                };
                self.callable_signatures.insert(callable, abi);
                abi
            }
        };
        Some(abi)
    }
    fn scoped_result_regions_only(&self, ty: TyId, params: &[crate::decls::ParamDecl]) -> bool {
        match self.t.kind(ty) {
            TyKind::Region(Root::Parameter(i)) => self
                .fd
                .params
                .get(*i as usize)
                .is_some_and(|p| self.region_handle(p.ty)),
            TyKind::Region(Root::CallableParameter { depth: 0, index: i }) => params
                .get(*i as usize)
                .is_some_and(|p| self.region_handle(p.ty)),
            TyKind::Region(Root::Local(slot)) => self.vars[*slot as usize]
                .map(|v| v.ty)
                .or_else(|| self.captured_root_ty(*slot))
                .is_some_and(|ty| self.region_handle(ty)),
            TyKind::Anchored(x, root) => {
                self.scoped_result_regions_only(*x, params)
                    && self.scoped_result_regions_only(*root, params)
            }
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs
                .iter()
                .all(|&x| self.scoped_result_regions_only(x, params)),
            TyKind::View(x) | TyKind::Array(x, _) => self.scoped_result_regions_only(*x, params),
            _ => true,
        }
    }

    fn parameter_region_result(&self, ty: TyId, parameter: u32) -> bool {
        self.parameter_region_result_at(ty, parameter, 0)
    }
    fn parameter_region_result_at(&self, ty: TyId, parameter: u32, depth: u32) -> bool {
        match self.t.kind(ty) {
            TyKind::Region(Root::CallableParameter {
                depth: bound,
                index: i,
            }) => *i == parameter && *bound == depth,
            TyKind::Anchored(inner, root) => {
                matches!(self.t.kind(*root), TyKind::Region(Root::CallableParameter {depth: bound, index: i}) if *i == parameter && *bound == depth)
                    || self.parameter_region_result_at(*inner, parameter, depth)
            }
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs
                .iter()
                .any(|&x| self.parameter_region_result_at(x, parameter, depth)),
            TyKind::Array(x, _) | TyKind::View(x) => {
                self.parameter_region_result_at(*x, parameter, depth)
            }
            TyKind::Fn(ps, r, _, _) => {
                ps.iter()
                    .any(|&(_, p)| self.parameter_region_result_at(p, parameter, depth + 1))
                    || self.parameter_region_result_at(*r, parameter, depth + 1)
            }
            _ => false,
        }
    }

    fn emit_value_call(
        &mut self,
        code: Option<ValueId>,
        environment: Option<ValueId>,
        abi: Option<CallableAbi>,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
        fail: Option<BlockId>,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let code = code?;
        let abi = abi?;
        let signature = abi.signature;
        let dest = self.e.module.signatures[signature.index()]
            .dest
            .map(|id| self.new_slot(PlaceTy::Declared(id), n));
        // Arguments follow their concrete signature; mutable places retain addresses.
        scratch.ir_args.clear();
        scratch.ir_args.extend(dest.map(Arg::Address));
        scratch.ir_args.extend(environment.map(Arg::Value));
        let mut parameter = usize::from(environment.is_some());
        for (i, &(mode, _)) in scratch.params.iter().enumerate() {
            if mode == Mode::Take {
                self.consume(scratch.vals[i], n);
            }
            let rep = self.callable_reps[abi.params.start as usize + i];
            if mode != Mode::Mut && matches!(rep, Rep::None) {
                continue;
            }
            let param = self.e.module.signatures[signature.index()].params[parameter];
            parameter += 1;
            let place = match param {
                Param::Address { place, .. } => place,
                Param::Value(_) => {
                    scratch.ir_args.push(Arg::Value(scratch.vals[i]?));
                    continue;
                }
            };
            let pointer = if mode == Mode::Mut {
                scratch.vals[i]
            } else if matches!(rep, Rep::Place(_)) {
                scratch.vals[i]
            } else {
                let at = self.new_slot(rep.stored().unwrap_or(place), n);
                self.install(at, rep, scratch.vals[i], n);
                Some(self.pointer(at, n))
            }?;
            let at = self.address(AddressBase::Value(pointer), &[], place, n);
            scratch.ir_args.push(Arg::Address(at));
        }
        let args = PoolRange::push(&mut self.b.pools.args, scratch.ir_args.iter().copied());
        let value = self.e.module.signatures[signature.index()]
            .results
            .first()
            .copied()
            .map(|t| self.value(t));
        let results = PoolRange::push(&mut self.b.pools.values, value);
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Indirect(code),
            signature,
            args,
            results,
            fail,
        });
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            self.end(Terminator::Unreachable, Some(n));
            return None;
        }
        let value = if let Some(dest) = dest {
            self.read_carried_rep(dest, abi.result, n)
        } else {
            value
        };
        self.temp(result, value);
        value
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn closure(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let mut fd = self.fd.as_ref().clone();
        fd.drop_of = None;
        fd.clone_of = None;
        fd.params.clear();
        fd.body = None;
        fd.result = UNIT;
        fd.waits = false;
        fd.anchors.clear();
        fd.region_anchors.clear();
        fd.outlives.clear();
        fd.path.0.name = format!(
            "{}$closure{}",
            fd.path.0.name,
            n.0 - self.item.range.nodes.start
        );
        fd.site = self.p.site(self.at, n);
        self.closure_header_params.clear();
        let mut written_result = false;
        let mut callable_result = UNIT;
        for &child in self.kids(n) {
            match self.kind(child) {
                NodeKind::Parameters => {
                    for &param in self.kids(child) {
                        let Some(token) = self.item.tree.node(param).token else {
                            return self.gap_ty(
                                param,
                                "closure parameters require written names and types",
                            );
                        };
                        let ty = self.ty(self.kids(param)[0]);
                        if self.kind(param) == NodeKind::BytesParameter
                            && ty != scalar(Scalar::U64)
                            && ty != ERROR
                        {
                            self.refuse("LANG-5", param, "a `bytes` parameter has type `u64`");
                        }
                        if let Some(slot) = self.binder(param) {
                            self.closure_header_params.push((slot, ty));
                        }
                        fd.params.push(crate::decls::ParamDecl {
                            bytes: self.kind(param) == NodeKind::BytesParameter,
                            name: self.item.symbol(token),
                            mode: crate::decls::mode_of(self.kind(param)),
                            ty,
                            site: self.p.site(self.at, param),
                        });
                    }
                }
                NodeKind::ResultType => {
                    written_result = true;
                    let node = self.kids(child)[0];
                    callable_result = self.ty_result(node, true);
                    if crate::decls::scoped_payload(&self.p.decls, &self.t, callable_result)
                        && crate::decls::written_result_missing_anchor(
                            self.p,
                            self.at,
                            node,
                            &|_| true,
                        )
                    {
                        self.refuse("OWN-20", node, "a scoped result names its anchor");
                        return (ERROR, None);
                    }
                    fd.result = crate::decls::erase_scoped_result(
                        &self.p.decls,
                        &mut self.t,
                        callable_result,
                    );
                    fd.result_site = self.p.site(self.at, child);
                }
                NodeKind::Waits => fd.waits = true,
                NodeKind::Block => fd.body = Some(child),
                _ => {}
            }
        }
        self.closure_header_params.clear();
        if !self.scoped_result_regions_only(callable_result, &fd.params) {
            self.refuse(
                "LANG-13",
                n,
                "a result anchored to a view parameter fits no callable type",
            );
            return (ERROR, None);
        }
        let expected = want
            .filter(|w| w.payload == 0)
            .map(|w| w.ty)
            .and_then(|ty| {
                if matches!(self.t.kind(ty), TyKind::Fn(..)) {
                    Some(ty)
                } else {
                    self.result_parts(ty).map(|(_, ok, _)| ok)
                }
            });
        if !written_result
            && let Some(ty) = expected
            && let TyKind::Fn(_, result, _, _) = self.t.kind(ty)
            && *result != HOLE
        {
            callable_result = *result;
            fd.result =
                crate::decls::erase_scoped_result(&self.p.decls, &mut self.t, callable_result);
        }
        if !crate::decls::scoped_result_rooted(&self.p.decls, &self.t, callable_result, false) {
            return self.gap_ty(
                n,
                "scoped callable result region anchors are not represented",
            );
        }
        let expected_kind = expected.and_then(|ty| match self.t.kind(ty) {
            TyKind::Fn(_, _, _, k) => Some(*k),
            _ => None,
        });
        let scoped =
            self.kind(n) == NodeKind::ScopedClosure || expected_kind == Some(Callable::Scoped);
        let environment =
            scoped || matches!(expected_kind, Some(Callable::Own | Callable::OwnSend));
        let ty = self.t.mk(TyKind::Fn(
            fd.params
                .iter()
                .map(|p| (if p.bytes { Mode::Bytes } else { p.mode }, p.ty))
                .collect(),
            callable_result,
            fd.waits,
            if scoped {
                Callable::Scoped
            } else {
                Callable::Plain
            },
        ));
        fd.callable = ty;
        // The public closure type already distinguishes invocation roots from
        // lexical roots. Its child body uses ordinary formal/outer-slot roots.
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        for (i, pd) in self.fd.params.iter().enumerate() {
            if let Some(&slot) = self.binder_at.get(&(pd.site.span.start, pd.site.span.end)) {
                let root = self.t.mk(TyKind::Region(Root::Local(slot)));
                for pd in &mut fd.params {
                    pd.ty = reroot(&mut self.t, pd.ty, Root::Parameter(i as u32), root);
                }
                fd.result = reroot(&mut self.t, fd.result, Root::Parameter(i as u32), root);
            }
        }
        scratch.roots.clear();
        for (i, pd) in fd.params.iter().enumerate() {
            let root = self
                .region_handle(pd.ty)
                .then(|| self.t.mk(TyKind::Region(Root::Parameter(i as u32))));
            scratch.roots.push(root);
        }
        for pd in &mut fd.params {
            pd.ty = crate::types::open_callable_roots(&mut self.t, pd.ty, &scratch.roots);
        }
        fd.result = crate::types::open_callable_roots(&mut self.t, fd.result, &scratch.roots);
        self.call_scratch.push(scratch);
        let body = fd.body.unwrap();
        self.capture_nested(body);
        let written_anchors: Vec<_> = (0..self.vars.len())
            .map(|slot| {
                self.anchors.declared_for(slot as u32).or_else(|| {
                    self.capture_frame
                        .as_ref()
                        .and_then(|f| f.written_anchors[slot])
                })
            })
            .collect();
        let mut child = Cx::new(self.p, self.definition, self.e);
        child.fd = std::borrow::Cow::Owned(fd);
        child.header = n;
        child.frame_level = self.frame_level + 1;
        child.entry = false;
        child.capture_frame = Some(CaptureFrame {
            outer: self.vars.clone(),
            outer_descriptors: std::mem::take(&mut self.descriptors),
            written_anchors: vec![None; self.vars.len()],
            by_slot: vec![None; self.vars.len()],
            captures: vec![],
            callbacks: vec![],
            projected: vec![],
            callback_paths: vec![],
            activating: false,
            words: 0,
            environments: vec![],
            environment: None,
            owning: !scoped,
            physical: 8,
            logical: 0,
            send: true,
        });
        for (slot, fact) in written_anchors.into_iter().enumerate() {
            child.capture_frame.as_mut().unwrap().written_anchors[slot] =
                fact.map(|fact| child.anchors.import_fact(&self.anchors, fact));
        }
        std::mem::swap(&mut child.t, &mut self.t);
        let signature = child.callable_signature(ty);
        let row = signature.map(|signature| {
            let f = child.e.function(
                &child.fd.path,
                child.e.module.signatures[signature.index()].clone(),
            );
            child.e.module.functions[f.index()].signature = signature;
            (f, signature)
        });
        child.run_row(row, body);
        std::mem::swap(&mut child.t, &mut self.t);
        let mut frame = child.capture_frame.take().unwrap();
        frame.callback_paths = std::mem::take(&mut child.descriptors.paths);
        self.descriptors = std::mem::take(&mut frame.outer_descriptors);
        let out = child.finish();
        let function = out.function;
        if let (Some(function), Some(template)) = (function, out.template) {
            self.nested.insert(function, template);
        }
        let refused = !out.refusals.is_empty();
        self.out.refusals.extend(out.refusals);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        if function.is_none() {
            self.halt();
        }
        if function.is_none() {
            return (if refused { ERROR } else { ty }, None);
        }
        let raw = function.unwrap();
        let Some(function) = self.row_adapter(raw, ty, n) else {
            return (ty, None);
        };
        let code = if self.live() {
            let v = self.value(Ty::Ptr);
            self.inst(
                Inst::FunctionRef {
                    result: v,
                    function,
                },
                n,
            );
            Some(v)
        } else {
            None
        };
        if scoped {
            return (ty, code.and_then(|code| self.scoped_value(code, &frame, n)));
        }
        if frame.captures.is_empty() {
            if environment {
                let kind = expected_kind.unwrap_or(Callable::OwnSend);
                let TyKind::Fn(ps, r, w, _) = self.t.kind(ty).clone() else {
                    unreachable!()
                };
                let ty = self.t.mk(TyKind::Fn(ps, r, w, kind));
                let v = code.and_then(|code| self.captureless_callable(code, n));
                self.temp(ty, v);
                return (ty, v);
            }
            return (ty, code);
        }
        if expected_kind == Some(Callable::Plain) {
            let message = "a plain callable captures nothing";
            self.refuse_compared("LANG-13", n, "own fn", "fn", message);
            return (ERROR, None);
        }
        let kind = if frame.send {
            Callable::OwnSend
        } else {
            Callable::Own
        };
        if expected_kind == Some(Callable::OwnSend) && !frame.send {
            let message = "own[send] requires send captures";
            self.refuse_compared("LANG-13", n, "own fn", "own[send] fn", message);
            return (ERROR, None);
        }
        let kind = if expected_kind == Some(Callable::Own) {
            Callable::Own
        } else {
            kind
        };
        let TyKind::Fn(ps, r, w, _) = self.t.kind(ty).clone() else {
            unreachable!()
        };
        let callable = self.t.mk(TyKind::Fn(ps, r, w, kind));
        self.owning_value(callable, code, &frame, n)
    }

    fn callable_signature(&mut self, ty: TyId) -> Result<SigId, &'static str> {
        let TyKind::Fn(ps, result, _, _) = self.t.kind(ty).clone() else {
            unreachable!()
        };
        self.indirect_signature(&ps, result, &mut vec![])
    }
}

/// One whole outer slot in first-naming order (LANG-14, OWN-21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Capture {
    slot: u32,
    mode: Mode,
    offset: u64,
    site: NodeId,
    /// Some(path) reads the callback currently stored in this outer place.
    path: Option<u32>,
    /// Runtime address needed only while checking a callback's known snapshot.
    synthetic: bool,
}

#[derive(Clone, Copy)]
struct CapturePath {
    parent: u32,
    field: u32,
}

#[derive(Clone, Copy)]
struct CapturedEnvironment {
    literal: NodeId,
    owner: u32,
    words: usize,
    offset: u64,
}

pub(super) struct CaptureFrame {
    outer: Vec<Option<Var>>,
    outer_descriptors: Descriptors,
    written_anchors: Vec<Option<anchors::Fact>>,
    by_slot: Vec<Option<usize>>,
    captures: Vec<Capture>,
    callbacks: Vec<Capture>,
    projected: Vec<Capture>,
    callback_paths: Vec<CapturePath>,
    activating: bool,
    words: u32,
    environments: Vec<CapturedEnvironment>,
    pub(super) environment: Option<ValueId>,
    owning: bool,
    physical: u64,
    logical: u64,
    send: bool,
}

#[derive(Clone, Copy)]
struct EnvironmentOrigin {
    site: NodeId,
    // An imported descriptor names an environment from an enclosing activation.
    local: bool,
}

#[derive(Default)]
pub(super) struct Descriptors {
    pub(super) captures: Vec<Capture>,
    pub(super) ranges: Vec<PoolRange<Capture>>,
    pub(super) values: HashMap<ValueId, u32>,
    pub(super) slots: HashMap<u32, u32>,
    fields: Vec<PoolRange<(anchors::Component, u32)>>,
    environments: Vec<EnvironmentOrigin>,
    parts: Vec<(anchors::Component, u32)>,
    imports: HashMap<u32, u32>,
    paths: Vec<CapturePath>,
    activation: Vec<Capture>,
    activation_path: Vec<u32>,
    references: HashMap<(u32, u32), u32>,
    projections: HashMap<(u32, u32), u32>,
    projected: HashMap<u32, Vec<Capture>>,
}
impl Descriptors {
    fn path_field(&mut self, parent: u32, field: u32) -> u32 {
        self.paths.push(CapturePath { parent, field });
        self.paths.len() as u32
    }
    fn import_path(&mut self, paths: &[CapturePath], path: u32) -> u32 {
        if path == 0 {
            return 0;
        }
        let p = paths[path as usize - 1];
        let parent = self.import_path(paths, p.parent);
        self.path_field(parent, p.field)
    }
    fn same_path(&self, mut a: u32, mut b: u32) -> bool {
        while a != b {
            if a == 0 || b == 0 {
                return false;
            }
            let (x, y) = (self.paths[a as usize - 1], self.paths[b as usize - 1]);
            if x.field != y.field {
                return false;
            }
            a = x.parent;
            b = y.parent;
        }
        true
    }
    fn same_capture(&self, a: Capture, b: Capture) -> bool {
        a.slot == b.slot
            && match (a.path, b.path) {
                (None, None) => true,
                (Some(a), Some(b)) => self.same_path(a, b),
                _ => false,
            }
    }
    fn reference(&mut self, slot: u32, path: u32, site: NodeId) -> u32 {
        if let Some(&id) = self.references.get(&(slot, path)) {
            return id;
        }
        let id = self.ranges.len() as u32;
        self.ranges.push(PoolRange::push(
            &mut self.captures,
            [Capture {
                slot,
                path: Some(path),
                synthetic: false,
                mode: Mode::View,
                offset: 0,
                site,
            }],
        ));
        self.environments
            .push(EnvironmentOrigin { site, local: false });
        self.fields.push(PoolRange::EMPTY);
        self.references.insert((slot, path), id);
        id
    }

    fn opaque(&mut self, id: Option<u32>) -> Option<u32> {
        let old = id?;
        if self.fields[old as usize].len == 0 {
            return id;
        }
        let id = self.ranges.len() as u32;
        self.ranges.push(self.ranges[old as usize]);
        self.environments.push(self.environments[old as usize]);
        self.fields.push(PoolRange::EMPTY);
        Some(id)
    }
    fn add(&mut self, frame: &CaptureFrame, site: NodeId) -> Option<u32> {
        let start = self.captures.len();
        self.captures.extend(
            frame
                .captures
                .iter()
                .filter(|c| !c.synthetic || c.mode == Mode::Mut)
                .copied(),
        );
        for c in &frame.callbacks {
            let mut c = *c;
            c.path = c.path.map(|p| self.import_path(&frame.callback_paths, p));
            if !(start..self.captures.len()).any(|i| self.same_capture(self.captures[i], c)) {
                self.captures.push(c);
            }
        }
        if self.captures.len() == start {
            return None;
        }
        let id = self.ranges.len() as u32;
        self.ranges.push(PoolRange::new(
            start as u32,
            (self.captures.len() - start) as u32,
        ));
        self.environments
            .push(EnvironmentOrigin { site, local: true });
        self.fields.push(PoolRange::EMPTY);
        Some(id)
    }
    /// Capture identity is a set of (place, mode), independent of closure code,
    /// source site, first-naming order and environment offsets (OWN-17/OWN-21).
    pub(super) fn same(&mut self, a: Option<u32>, b: Option<u32>) -> bool {
        if a == b {
            return true;
        }
        let captures = |id: Option<u32>| {
            id.map_or(&[][..], |id| {
                self.ranges[id as usize].get(&self.captures).unwrap()
            })
        };
        let (left, right) = (captures(a), captures(b));
        if left.len() != right.len()
            || !left.iter().all(|x| {
                right
                    .iter()
                    .any(|y| self.same_capture(*x, *y) && x.mode == y.mode)
            })
        {
            return false;
        }
        for source in [a, b].into_iter().flatten() {
            let range = self.fields[source as usize];
            for i in range.start..range.start + range.len {
                let field = self.parts[i as usize].0;
                let left = self.project(a, field);
                let right = self.project(b, field);
                if !self.same(left, right) {
                    return false;
                }
            }
        }
        true
    }
    fn union(&mut self, a: Option<u32>, b: Option<u32>) -> Option<u32> {
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) if a != b => (a, b),
            _ => return a.or(b),
        };
        let a_env = self.environments[a as usize];
        let b_env = self.environments[b as usize];
        self.environments
            .push(if a_env.local { a_env } else { b_env });
        let start = self.captures.len();
        let ra = self.ranges[a as usize];
        for i in ra.start..ra.start + ra.len {
            let c = self.captures[i as usize];
            self.captures.push(c);
        }
        let rb = self.ranges[b as usize];
        for i in rb.start..rb.start + rb.len {
            let c = self.captures[i as usize];
            if let Some(j) =
                (start..self.captures.len()).find(|&j| self.same_capture(self.captures[j], c))
            {
                if c.mode == Mode::Mut {
                    self.captures[j].mode = Mode::Mut
                }
            } else {
                self.captures.push(c);
            }
        }
        let id = self.ranges.len() as u32;
        self.ranges.push(PoolRange::new(
            start as u32,
            (self.captures.len() - start) as u32,
        ));
        if self.fields[a as usize].len == 0 || self.fields[b as usize].len == 0 {
            self.fields.push(PoolRange::EMPTY);
            return Some(id);
        }
        let start = self.parts.len();
        for source in [a, b] {
            let range = self.fields[source as usize];
            for i in range.start..range.start + range.len {
                self.parts.push(self.parts[i as usize]);
            }
        }
        self.fields.push(PoolRange::new(
            start as u32,
            (self.parts.len() - start) as u32,
        ));
        Some(id)
    }
    fn component(&mut self, component: anchors::Component, child: Option<u32>) -> Option<u32> {
        let child = child?;
        let id = self.ranges.len() as u32;
        self.ranges.push(self.ranges[child as usize]);
        self.environments.push(self.environments[child as usize]);
        self.fields
            .push(PoolRange::push(&mut self.parts, [(component, child)]));
        Some(id)
    }
    fn project(&mut self, id: Option<u32>, component: anchors::Component) -> Option<u32> {
        let range = self.fields[id? as usize];
        if range.len == 0 {
            let captures = self.ranges[id.unwrap() as usize];
            if captures.len == 1 {
                let c = self.captures[captures.start as usize];
                if let Some(path) = c.path {
                    let anchors::Component::Field(field) = component else {
                        return id;
                    };
                    let key = (id.unwrap(), field);
                    if let Some(&id) = self.projections.get(&key) {
                        return Some(id);
                    }
                    let path = self.path_field(path, field);
                    let projected = self.reference(c.slot, path, c.site);
                    self.projections.insert(key, projected);
                    return Some(projected);
                }
            }
            return id;
        }
        let mut result = None;
        for i in range.start..range.start + range.len {
            let (field, child) = self.parts[i as usize];
            if field == component {
                result = self.union(result, Some(child));
            }
        }
        result
    }
    fn replace(
        &mut self,
        old: Option<u32>,
        path: &[anchors::Component],
        new: Option<u32>,
    ) -> Option<u32> {
        let Some((&component, rest)) = path.split_first() else {
            return new;
        };
        if old.is_some_and(|id| self.fields[id as usize].len == 0) {
            return self.union(old, new);
        }
        let child = self.project(old, component);
        let child = self.replace(child, rest, new);
        let mut result = None;
        if let Some(old) = old {
            let range = self.fields[old as usize];
            for i in range.start..range.start + range.len {
                let (field, value) = self.parts[i as usize];
                if field != component {
                    let value = self.component(field, Some(value));
                    result = self.union(result, value);
                }
            }
        }
        let child = self.component(component, child);
        self.union(result, child)
    }
    fn import(&mut self, outer: &Self, source: u32) -> u32 {
        if let Some(&id) = self.imports.get(&source) {
            return id;
        }
        let range = outer.ranges[source as usize];
        let start = self.captures.len();
        for i in range.start..range.start + range.len {
            let mut c = outer.captures[i as usize];
            c.path = c.path.map(|p| self.import_path(&outer.paths, p));
            self.captures.push(c);
        }
        let captures = PoolRange::new(start as u32, range.len);
        let id = self.ranges.len() as u32;
        self.ranges.push(captures);
        self.environments.push(EnvironmentOrigin {
            local: false,
            ..outer.environments[source as usize]
        });
        self.fields.push(PoolRange::EMPTY);
        self.imports.insert(source, id);
        let fields = outer.fields[source as usize];
        let start = self.parts.len();
        self.parts
            .extend_from_slice(fields.get(&outer.parts).unwrap());
        for i in start..start + fields.len as usize {
            let child = self.import(outer, self.parts[i].1);
            self.parts[i].1 = child;
        }
        self.fields[id as usize] = PoolRange::new(start as u32, fields.len);
        id
    }
    pub(super) fn of(&self, value: Option<ValueId>) -> Option<u32> {
        self.values.get(&value?).copied()
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// A closure nested in a closure reaches an outer slot through its enclosing
    /// closure, which captures that slot before the nested one is built.
    fn capture_nested(&mut self, body: NodeId) {
        if self.capture_frame.is_none() {
            return;
        }
        let p: &'c Program<'a> = self.p;
        let mut stack = vec![body];
        while let Some(n) = stack.pop() {
            stack.extend(self.kids(n).iter().copied());
            if self.kind(n) != NodeKind::Path {
                continue;
            }
            let Some(u) = p.resolved(self.at, n) else {
                continue;
            };
            let Target::Binder(b) = u.target else {
                continue;
            };
            if self.vars.get(b as usize).is_some_and(Option::is_none) {
                self.capture_slot(b, n);
            }
        }
    }
    /// Type-only root mentions need the enclosing declaration's type, without a
    /// runtime capture. An expression use still discovers its capture normally.
    pub(super) fn captured_root_ty(&self, slot: u32) -> Option<TyId> {
        self.capture_frame
            .as_ref()?
            .outer
            .get(slot as usize)?
            .map(|v| v.ty)
    }
    /// Discover an outer slot exactly when its first source use is visited.
    pub(super) fn capture_slot(&mut self, slot: u32, n: NodeId) {
        self.capture_slot_source(slot, n, false);
    }
    fn capture_slot_source(&mut self, slot: u32, n: NodeId, synthetic: bool) {
        let Some(frame) = &self.capture_frame else {
            return;
        };
        let Some(original) = frame.outer[slot as usize] else {
            return;
        };
        if let Some(i) = frame.by_slot[slot as usize] {
            if !synthetic {
                self.capture_frame.as_mut().unwrap().captures[i].synthetic = false
            }
            return;
        }
        let owning = frame.owning;
        let index = frame.captures.len();
        if matches!(self.t.kind(original.ty),TyKind::Adt(d,_) if self.p.decls[d.index()].role==Some(Role::Writer))
        {
            self.refuse("MEM-4", n, "a Writer is never captured");
        } else if owning {
            if self.capture_holds(original.ty)
                & (crate::H_RESOURCE | crate::H_CAPABILITY | crate::H_SCOPED | crate::H_STABLE)
                != 0
            {
                self.refuse("LANG-14",n,"an owning closure capture holds a resource, capability, scoped value or stable reference");
            } else if !self.copies(original.ty)
                && matches!(
                    original.origin,
                    Origin::ViewParam | Origin::MutParam | Origin::PatternView
                )
            {
                self.refuse(
                    "LANG-14",
                    n,
                    "an owning closure cannot move a borrowed capture",
                );
            }
        }
        let (size, align, logical) = match self.rep(original.ty) {
            Ok(Rep::None) => (0, 1, 0),
            Ok(Rep::Scalar(ty)) => {
                let size = match ty {
                    Ty::Ptr => 8,
                    Ty::Scalar(s) => s.size(),
                };
                (size, size, size)
            }
            Ok(Rep::Place(id)) => {
                let Layout::Fixed(layout) = &self.e.recipes[id.index()].layout else {
                    unreachable!()
                };
                (
                    layout.size,
                    layout.align,
                    self.e.recipes[id.index()]
                        .logical_size
                        .unwrap_or(layout.size),
                )
            }
            Err(what) => {
                self.gap(n, what);
                (0, 1, 0)
            }
        };
        let send = has_bounds(&self.p.decls, &self.t, original.ty, &self.params) & crate::SEND != 0;
        let frame = self.capture_frame.as_mut().unwrap();
        let offset = if owning {
            (frame.physical + align - 1) & !(align - 1)
        } else {
            let offset = frame.words as u64 * 8;
            frame.words += 1;
            offset
        };
        frame.physical = offset + size;
        frame.logical += logical;
        frame.send &= send;
        frame.by_slot[slot as usize] = Some(index);
        frame.captures.push(Capture {
            slot,
            mode: Mode::View,
            offset,
            site: n,
            path: None,
            synthetic,
        });
        let environment = frame.environment;
        let binding = if let Some(environment) = environment.filter(|_| self.live()) {
            let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n).unwrap();
            let address = if owning {
                self.rep(original.ty).ok().and_then(Rep::stored).map(|ty| {
                    self.address(
                        AddressBase::Value(environment),
                        &[Step::Byte(offset)],
                        ty,
                        n,
                    )
                })
            } else {
                let address = self.address(
                    AddressBase::Value(environment),
                    &[Step::Byte(offset)],
                    PlaceTy::Value(Ty::Ptr),
                    n,
                );
                let pointer = self.value(Ty::Ptr);
                self.inst(
                    Inst::Read {
                        result: pointer,
                        address,
                        mode: ReadMode::Bits,
                    },
                    n,
                );
                self.rep(original.ty)
                    .ok()
                    .and_then(Rep::stored)
                    .map(|ty| self.address(AddressBase::Value(pointer), &[], ty, n))
            };
            Binding::Slot(address)
        } else {
            Binding::Value(None)
        };
        self.vars[slot as usize] = Some(Var {
            binding,
            origin: if !owning && original.origin.writable() {
                Origin::MutParam
            } else {
                Origin::ViewParam
            },
            avail: Avail::Live,
            holds: 0,
            own: None,
            frame_level: if owning {
                self.frame_level
            } else {
                original.frame_level
            },
            depth: if owning { 0 } else { original.depth },
            ..original
        });
        if kind(&self.p.decls, &self.t, original.ty, &self.params) == Kind::Stable {
            self.parameter_anchors(slot, original.ty, Mode::View, slot);
        } else if self.anchor_tracked(original.ty) {
            let fact = self.anchor_root(Place::whole(slot));
            self.replace_slot_anchors(slot, fact);
        }
        if original.mut_anchor {
            self.parameter_anchors(slot, original.ty, Mode::Mut, slot);
        }
        // Import only this callback's roots. Marking its environment slot above
        // makes repeated and transitive captures reuse the same slot.
        let outer = &self.capture_frame.as_ref().unwrap().outer_descriptors;
        if let Some(&id) = outer.slots.get(&slot) {
            let id = self.descriptors.import(outer, id);
            let range = self.descriptors.ranges[id as usize];
            self.set_slot_descriptor(slot, Some(id));
            for i in range.start..range.start + range.len {
                let capture = self.descriptors.captures[i as usize];
                // A captured aggregate can contain several callbacks. Import their
                // root metadata now, but capture/activate only the selected value
                // when its invocation or argument is visited.
                if self.vars[capture.slot as usize].is_none() {
                    let original =
                        self.capture_frame.as_ref().unwrap().outer[capture.slot as usize].unwrap();
                    self.vars[capture.slot as usize] = Some(Var {
                        binding: Binding::Value(None),
                        avail: Avail::Live,
                        holds: 0,
                        own: None,
                        ..original
                    });
                }
            }
        }
    }
    pub(super) fn capture_write(&mut self, slot: u32, site: Span) {
        if let Some(frame) = &mut self.capture_frame
            && let Some(i) = frame.by_slot[slot as usize]
        {
            if frame.activating {
                return;
            }
            frame.captures[i].mode = Mode::Mut;
            if frame.owning && !self.broken {
                self.refuse_at("LANG-14", site, "an owning closure never mutates a capture");
            }
        }
    }
    pub(super) fn capture_move(&mut self, slot: u32, how: Use, ty: TyId, n: NodeId) -> bool {
        if self
            .capture_frame
            .as_ref()
            .is_some_and(|f| f.outer[slot as usize].is_some())
            && matches!(how, Use::Take | Use::Value)
            && !self.copies(ty)
        {
            if !self.broken {
                self.refuse(
                    "LANG-14",
                    n,
                    "a closure never moves a capture during invocation",
                );
            }
            return false;
        }
        true
    }
    pub(super) fn capture_holds(&self, ty: TyId) -> u8 {
        match self.t.kind(ty) {
            // MEM-17: a clone body runs only for a value whose clone passed this check,
            // so its parameters hold no resource, scoped value or opaque value.
            TyKind::Param(i) if self.fd.clone_of.is_some() => {
                crate::param_holds(self.params[*i as usize])
                    & !(crate::H_RESOURCE | crate::H_SCOPED | crate::H_OPAQUE)
            }
            TyKind::Param(i) => crate::param_holds(self.params[*i as usize]),
            TyKind::View(_) => crate::H_SCOPED,
            TyKind::Region(_) => crate::H_SCOPED,
            TyKind::Anchored(x, _) => self.capture_holds(*x) | crate::H_STABLE,
            TyKind::Tuple(xs) => xs.iter().fold(0, |h, &x| h | self.capture_holds(x)),
            TyKind::Array(x, _) => self.capture_holds(*x),
            TyKind::Adt(d, args) => {
                let summary = &self.p.decls[d.index()].summary;
                args.iter()
                    .enumerate()
                    .filter(|(i, _)| summary.holds_params & (1 << i) != 0)
                    .fold(summary.holds, |h, (_, &x)| h | self.capture_holds(x))
            }
            TyKind::Fn(_, _, _, Callable::Scoped) => crate::H_SCOPED,
            TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend) => crate::H_OPAQUE,
            _ => 0,
        }
    }
    fn capture_address(&mut self, slot: u32, n: NodeId) -> Option<ValueId> {
        let var = self.var(slot);
        let rep = match self.rep(var.ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        match (var.binding, rep) {
            (Binding::Slot(Some(a)), _) => Some(self.pointer(a, n)),
            (Binding::Value(Some(p)), Rep::Place(_)) => Some(p),
            (Binding::Value(value), _) => {
                let address = self.slot(var.ty, value, n)?;
                self.vars[slot as usize].as_mut().unwrap().binding = Binding::Slot(Some(address));
                Some(self.pointer(address, n))
            }
            _ => None,
        }
    }
    fn raw_frame(&mut self, words: usize, n: NodeId) -> Option<ValueId> {
        let ty = self.t.mk(TyKind::Array(scalar(Scalar::U64), words as u64));
        let address = self.slot(ty, None, n)?;
        Some(self.pointer(address, n))
    }
    fn store_word(&mut self, base: ValueId, offset: u64, value: ValueId, n: NodeId) {
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n).unwrap();
        let address = self.address(
            AddressBase::Value(base),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        self.inst(Inst::Store { address, value }, n);
    }
    /// FORM87: a literal's environment belongs to the innermost activation
    /// owning a capture. Intervening closures carry that slot's address.
    fn environment_slot(
        &mut self,
        literal: NodeId,
        owner: u32,
        words: usize,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        if owner == self.frame_level {
            let ty = self.t.mk(TyKind::Array(scalar(Scalar::U64), words as u64));
            let place = self.rep(ty).ok()?.stored()?;
            self.out.counts.places += 1;
            let address = self.entry_slot(place, n);
            return Some(self.pointer(address, n));
        }
        let frame = self
            .capture_frame
            .as_mut()
            .expect("a nested activation has an environment");
        // The one source visit forwards each literal along its unique enclosing path.
        let offset = frame.words as u64 * 8;
        frame.words += 1;
        frame.environments.push(CapturedEnvironment {
            literal,
            owner,
            words,
            offset,
        });
        let environment = frame.environment;
        self.callable_word(environment, offset, n)
    }

    fn scoped_value(&mut self, code: ValueId, frame: &CaptureFrame, n: NodeId) -> Option<ValueId> {
        let captures = &frame.captures;
        if captures.is_empty() && frame.environments.is_empty() {
            return self.captureless_callable(code, n);
        }
        for capture in captures {
            self.capture_slot_source(capture.slot, n, capture.synthetic);
        }
        if !self.live() {
            return None;
        }
        let owner = captures
            .iter()
            .map(|c| self.var(c.slot).frame_level)
            .chain(frame.environments.iter().map(|e| e.owner))
            .max()
            .unwrap_or(self.frame_level);
        let environment = self.environment_slot(n, owner, frame.words as usize, n)?;
        for capture in captures {
            let pointer = self.capture_address(capture.slot, n)?;
            self.store_word(environment, capture.offset, pointer, n);
        }
        for captured in &frame.environments {
            let pointer =
                self.environment_slot(captured.literal, captured.owner, captured.words, n)?;
            self.store_word(environment, captured.offset, pointer, n);
        }
        let carrier = self.raw_frame(2, n)?;
        self.store_word(carrier, 0, code, n);
        self.store_word(carrier, 8, environment, n);
        let mut projected = frame.projected.clone();
        for capture in &mut projected {
            capture.path = capture
                .path
                .map(|path| self.descriptors.import_path(&frame.callback_paths, path));
        }
        if let Some(id) = self.descriptors.add(frame, n) {
            self.descriptors.values.insert(carrier, id);
            if !projected.is_empty() {
                self.descriptors.projected.insert(id, projected);
            }
        }
        Some(carrier)
    }
    pub(super) fn bind_descriptor(&mut self, slot: u32, value: Option<ValueId>) {
        self.set_slot_descriptor(slot, self.descriptors.of(value));
    }
    pub(super) fn descriptor_component(
        &mut self,
        value: Option<ValueId>,
        component: anchors::Component,
        child: Option<ValueId>,
    ) {
        let Some(value) = value else {
            return;
        };
        let child = self.descriptors.of(child);
        let component = self.descriptors.component(component, child);
        let id = self
            .descriptors
            .union(self.descriptors.of(Some(value)), component);
        if let Some(id) = id {
            self.descriptors.values.insert(value, id);
        }
    }
    pub(super) fn project_descriptor(
        &mut self,
        from: Option<ValueId>,
        value: Option<ValueId>,
        component: anchors::Component,
        ty: TyId,
    ) {
        let id = self.descriptors.of(from);
        if id.is_none() || !self.contains_callback(ty) {
            return;
        }
        let id = self.descriptors.project(id, component);
        if let (Some(value), Some(id)) = (value, id) {
            self.descriptors.values.insert(value, id);
        }
    }
    pub(super) fn read_descriptor(&mut self, at: At, value: Option<ValueId>, n: NodeId) {
        if self
            .capture_frame
            .as_ref()
            .is_some_and(|f| !f.owning && f.outer[at.place.slot as usize].is_some())
            && self.contains_callback(at.ty)
        {
            if let Some(value) = value {
                let mut id = self.descriptors.reference(at.place.slot, 0, n);
                for i in at.place.start..at.place.start + at.place.len {
                    let PStep::Field(field) = self.steps[i as usize] else {
                        self.gap(
                            n,
                            "capture descriptors through this projection are not built",
                        );
                        return;
                    };
                    id = self
                        .descriptors
                        .project(Some(id), anchors::Component::Field(field))
                        .unwrap();
                }
                self.descriptors.values.insert(value, id);
            }
            return;
        }
        if self
            .capture_frame
            .as_ref()
            .is_some_and(|f| !f.owning && f.outer[at.place.slot as usize].is_some())
            && at.place.len > 0
            && self.contains_callback(self.var(at.place.slot).ty)
            && !self.contains_callback(at.ty)
        {
            let mut path = 0;
            for i in at.place.start..at.place.start + at.place.len {
                let PStep::Field(field) = self.steps[i as usize] else {
                    self.gap(
                        n,
                        "capture descriptors through this projection are not built",
                    );
                    return;
                };
                path = self.descriptors.path_field(path, field);
            }
            let projected = Capture {
                slot: at.place.slot,
                mode: Mode::View,
                offset: 0,
                site: n,
                path: Some(path),
                synthetic: false,
            };
            if let Some(frame) = &mut self.capture_frame
                && !frame
                    .projected
                    .iter()
                    .any(|old| self.descriptors.same_capture(*old, projected))
            {
                frame.projected.push(projected);
            }
            return;
        }
        let mut id = self.descriptors.slots.get(&at.place.slot).copied();
        if id.is_none() || !self.contains_callback(at.ty) {
            return;
        }
        for i in at.place.start..at.place.start + at.place.len {
            if id.is_none() {
                break;
            }
            match self.steps[i as usize] {
                PStep::Field(field) => {
                    id = self
                        .descriptors
                        .project(id, anchors::Component::Field(field))
                }
                _ => {
                    self.gap(
                        n,
                        "capture descriptors through this projection are not built",
                    );
                    return;
                }
            }
        }
        if let (Some(value), Some(id)) = (value, id) {
            self.descriptors.values.insert(value, id);
        }
    }

    pub(super) fn activate(&mut self, value: Option<ValueId>, call: u32, n: NodeId) {
        let Some(id) = self.descriptors.of(value) else {
            return;
        };
        let mut work = std::mem::take(&mut self.descriptors.activation);
        let mut path = std::mem::take(&mut self.descriptors.activation_path);
        work.clear();
        let range = self.descriptors.ranges[id as usize];
        work.extend_from_slice(range.get(&self.descriptors.captures).unwrap());
        // Direct roots of a locally built callback are real uses in this body.
        // Only roots reached by expanding an outer callback are snapshot-only.
        for &c in &work {
            if c.path.is_none() {
                self.capture_slot_source(c.slot, n, false);
                if c.mode == Mode::Mut {
                    self.capture_write(c.slot, self.span(n))
                }
            }
        }
        // A mut activation of a callback-valued slot also activates the
        // descriptor currently installed in that slot.  Carry its nested
        // capture modes into this loan set so an installed mut capture is
        // checked against the slot's written-anchor freezes at this call.
        let roots = work.clone();
        for capture in roots {
            if capture.path.is_some() {
                continue;
            }
            let written = self.anchors.declared_for(capture.slot).or_else(|| {
                self.capture_frame
                    .as_ref()
                    .and_then(|f| f.written_anchors[capture.slot as usize])
            });
            if written.is_none() {
                continue;
            }
            let Some(&id) = self.descriptors.slots.get(&capture.slot) else {
                continue;
            };
            let nested = self.descriptors.ranges[id as usize]
                .get(&self.descriptors.captures)
                .unwrap()
                .to_vec();
            for nested in nested {
                if !work
                    .iter()
                    .any(|existing| self.descriptors.same_capture(*existing, nested))
                {
                    work.push(nested);
                }
            }
        }
        if let Some(frame) = &mut self.capture_frame {
            for &c in &work {
                if c.path.is_some()
                    && frame.outer[c.slot as usize].is_some()
                    && !frame
                        .callbacks
                        .iter()
                        .any(|old| self.descriptors.same_capture(*old, c))
                {
                    frame.callbacks.push(c)
                }
            }
        }
        let mut i = 0;
        while i < work.len() {
            let c = work[i];
            i += 1;
            let Some(mut at) = c.path else { continue };
            self.capture_slot_source(c.slot, n, true);
            path.clear();
            while at != 0 {
                let p = self.descriptors.paths[at as usize - 1];
                path.push(p.field);
                at = p.parent;
            }
            let mut current = self.descriptors.slots.get(&c.slot).copied();
            for &field in path.iter().rev() {
                current = self
                    .descriptors
                    .project(current, anchors::Component::Field(field));
            }
            let root = Capture {
                path: None,
                mode: Mode::View,
                ..c
            };
            if !work.iter().any(|x| self.descriptors.same_capture(*x, root)) {
                work.push(root)
            }
            if let Some(current) = current {
                let range = self.descriptors.ranges[current as usize];
                for j in range.start..range.start + range.len {
                    let c = self.descriptors.captures[j as usize];
                    if let Some(k) = work
                        .iter()
                        .position(|x| self.descriptors.same_capture(*x, c))
                    {
                        if c.mode == Mode::Mut {
                            work[k].mode = Mode::Mut
                        }
                    } else {
                        work.push(c)
                    }
                }
            }
        }
        if let Some(frame) = &mut self.capture_frame {
            frame.activating = true
        }
        for &capture in &work {
            if capture.path.is_some() {
                continue;
            }
            self.capture_slot_source(capture.slot, n, true);
            let at = At {
                place: Place::whole(capture.slot),
                ty: self.var(capture.slot).ty,
                shared: false,
            };
            self.use_place(
                at,
                if capture.mode == Mode::Mut {
                    Use::Mut(Hold::CallMut(call, u32::MAX))
                } else {
                    Use::View(Hold::CallView(call, u32::MAX))
                },
                n,
            );
            if capture.mode == Mode::Mut {
                self.mut_descriptor(at)
            }
        }
        if let Some(frame) = &mut self.capture_frame {
            frame.activating = false
        }
        work.clear();
        path.clear();
        self.descriptors.activation = work;
        self.descriptors.activation_path = path;
    }
    pub(super) fn returned_descriptor(
        &mut self,
        value: Option<ValueId>,
        anchors: &[u32],
        args: &[Option<ValueId>],
    ) {
        let Some(value) = value else { return };
        let mut id = None;
        for &i in anchors {
            id = self
                .descriptors
                .union(id, self.descriptors.of(args[i as usize]));
        }
        // A declaration states which arguments, not which component permutation.
        let id = self.descriptors.opaque(id);
        if let Some(id) = id {
            self.descriptors.values.insert(value, id);
        }
    }
    pub(super) fn return_descriptor(&mut self, value: Option<ValueId>) -> bool {
        if !self.live() || self.descriptors.values.is_empty() {
            return false;
        }
        if let Some(id) = self.descriptors.of(value) {
            let environment = self.descriptors.environments[id as usize];
            if environment.local {
                self.refuse(
                    "OWN-21",
                    environment.site,
                    "a scoped callback that captures a callee slot cannot be returned",
                );
                self.dead = true;
                return true;
            }
        }
        false
    }
    pub(super) fn descriptor_scope(&mut self, scope: usize, value: Option<ValueId>, n: NodeId) {
        let Some(id) = self.descriptors.of(value) else {
            return;
        };
        let range = self.descriptors.ranges[id as usize];
        if let Some(c) = range
            .get(&self.descriptors.captures)
            .unwrap()
            .iter()
            .find(|c| self.scope_slots[scope..].contains(&c.slot))
        {
            let cause = self.declared_at(c.slot);
            self.refuse_from(
                "OWN-21",
                self.close_brace(n),
                vec![cause],
                "a captured slot must outlive its scoped callback",
            );
        }
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn join_descriptor(&mut self, j: usize, value: Option<ValueId>) {
        self.joins[j].descriptor =
            self.union_descriptor(self.joins[j].descriptor, self.descriptors.of(value));
    }
    pub(super) fn union_descriptor(&mut self, a: Option<u32>, b: Option<u32>) -> Option<u32> {
        self.descriptors.union(a, b)
    }
    pub(super) fn set_slot_descriptor(&mut self, slot: u32, id: Option<u32>) {
        let before = self.descriptors.slots.get(&slot).copied();
        if before != id {
            let v = self.var(slot);
            self.log
                .push((slot, v.avail, v.at, before, self.slot_anchors(slot)));
            self.descriptors.replace_slot(slot, id);
        }
    }
    /// A mut callee can install any old part. Keep its capture union but forget
    /// component refinements; the caller cannot know which part was installed.
    pub(super) fn mut_descriptor(&mut self, at: At) {
        let root = self.descriptors.slots.get(&at.place.slot).copied();
        let Some(_) = root else {
            return;
        };
        if !self.contains_callback(at.ty) {
            return;
        }
        let mut selected = root;
        let mut path = std::mem::take(&mut self.anchor_path);
        path.clear();
        for i in at.place.start..at.place.start + at.place.len {
            if selected.is_none() {
                break;
            }
            let PStep::Field(field) = self.steps[i as usize] else {
                unreachable!("a callback component is reached only through fields");
            };
            let part = anchors::Component::Field(field);
            path.push(part);
            selected = self.descriptors.project(selected, part);
        }
        let opaque = self.descriptors.opaque(selected);
        if opaque != selected {
            let id = self.descriptors.replace(root, &path, opaque);
            self.set_slot_descriptor(at.place.slot, id);
        }
        self.anchor_path = path;
    }
    /// Walk only inline scoped structure; ordinary type arguments cannot be scoped.
    pub(super) fn contains_callback(&self, ty: TyId) -> bool {
        match self.t.kind(ty) {
            TyKind::Fn(_, _, _, Callable::Scoped) => true,
            TyKind::Tuple(xs) => xs.iter().any(|&x| self.contains_callback(x)),
            TyKind::View(x) | TyKind::Anchored(x, _) => self.contains_callback(*x),
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                if decl.structural {
                    return args.iter().any(|&x| self.contains_callback(x));
                }
                if decl.repr != Repr::Scoped || decl.summary.cycle {
                    return false;
                }
                match &decl.body {
                    DeclBody::Record(fs) => fs.iter().any(|f| self.contains_callback(f.ty)),
                    DeclBody::Variant(cs) => cs
                        .iter()
                        .any(|c| c.fields.iter().any(|f| self.contains_callback(f.ty))),
                    DeclBody::Alias(x) => self.contains_callback(*x),
                }
            }
            _ => false,
        }
    }
    pub(super) fn descriptor_store(
        &mut self,
        at: At,
        value: Option<ValueId>,
        source: NodeId,
        n: NodeId,
    ) -> bool {
        // A sibling may hold a callback, but this place cannot change its descriptor.
        // In particular a list, array or Box payload cannot contain scoped values.
        if !self.contains_callback(at.ty) {
            return true;
        }
        // OWN-11 governs true mut formals and aliases. The language has not yet
        // chosen invariance or carried store effects for an ordinary upvar.
        if self.capture_frame.as_ref().is_some_and(|f| {
            !f.owning
                && f.outer[at.place.slot as usize].is_some()
                && !self.var(at.place.slot).mut_anchor
                && f.outer[at.place.slot as usize]
                    .is_none_or(|outer| outer.origin != Origin::MutParam)
        }) {
            // OWN-21: activation of a mut capture is a mut loan of the slot.
            // The installed callback may read only the slot's written anchors,
            // their projections and regions named by its type.  `anchor_store`
            // applies the same fact construction for ordinary stores; do this
            // check before it so a captured-slot violation is reported as
            // OWN-21 at the assignment rather than as a generic OWN-19 gap.
            let declared = self.anchors.declared_for(at.place.slot).or_else(|| {
                self.capture_frame
                    .as_ref()
                    .and_then(|f| f.written_anchors[at.place.slot as usize])
            });
            let new = self.anchor_expression(source);
            let captures = self.capture_anchors(value);
            let actual = self.anchors.union(new, captures);
            if let Some(allowed) = declared {
                let permits = self.anchors.permits_fact(allowed, actual, &self.steps);
                if !permits {
                    let cause = self.declared_at(at.place.slot);
                    self.refuse_from(
                        "OWN-21",
                        self.span(n),
                        vec![cause],
                        "a value installed through a captured slot reads an undeclared anchor",
                    );
                    return false;
                }
            } else if actual != 0 {
                self.refuse(
                    "OWN-21",
                    n,
                    "a captured slot without written anchors may receive only an anchor-free value",
                );
                return false;
            }
            // A callback installed through this captured slot contributes its
            // roots transitively to the enclosing callback descriptor. Preserve
            // their modes so a later activation loans a nested mut capture too.
            if let Some(id) = self.descriptors.of(value) {
                let captures = self.descriptors.ranges[id as usize]
                    .get(&self.descriptors.captures)
                    .unwrap()
                    .to_vec();
                if let Some(frame) = &mut self.capture_frame {
                    for capture in captures {
                        if let Some(existing) = frame
                            .captures
                            .iter_mut()
                            .find(|existing| self.descriptors.same_capture(**existing, capture))
                        {
                            if capture.mode == Mode::Mut {
                                existing.mode = Mode::Mut;
                                existing.synthetic = false;
                            }
                        } else {
                            frame.captures.push(Capture {
                                path: None,
                                synthetic: false,
                                ..capture
                            });
                        }
                    }
                }
            }
        }
        if self.var(at.place.slot).origin == Origin::MutParam {
            if let Some(id) = self.descriptors.of(value) {
                let range = self.descriptors.ranges[id as usize];
                if let Some(c) = range
                    .get(&self.descriptors.captures)
                    .unwrap()
                    .iter()
                    .find(|c| {
                        c.path.is_none()
                            && (c.slot == at.place.slot
                                || self
                                    .capture_frame
                                    .as_ref()
                                    .is_none_or(|f| f.outer[c.slot as usize].is_none()))
                    })
                {
                    let cause = self.declared_at(c.slot);
                    self.refuse_from(
                        "OWN-21",
                        self.span(n),
                        vec![cause],
                        "a stored callback cannot capture a callee slot",
                    );
                    return false;
                }
            }
            // OWN-11 checks the value's anchors next. An allowed replacement
            // leaves this borrow's capture upper bound, and the caller's, intact.
            return true;
        }
        if let Some(id) = self.descriptors.of(value) {
            let target = self.var(at.place.slot);
            let range = self.descriptors.ranges[id as usize];
            if let Some(c) = range
                .get(&self.descriptors.captures)
                .unwrap()
                .iter()
                .find(|c| {
                    let captured = self.var(c.slot);
                    (captured.frame_level, captured.depth) > (target.frame_level, target.depth)
                })
            {
                let cause = self.declared_at(c.slot);
                self.refuse_from(
                    "OWN-21",
                    self.span(n),
                    vec![cause],
                    "a scoped callback cannot outlive a captured slot",
                );
                return false;
            }
        }
        if at.place.len == 0 {
            self.bind_descriptor(at.place.slot, value);
        } else {
            let mut path = std::mem::take(&mut self.anchor_path);
            path.clear();
            for i in at.place.start..at.place.start + at.place.len {
                match self.steps[i as usize] {
                    PStep::Field(field) => path.push(anchors::Component::Field(field)),
                    _ => {
                        if self.descriptors.of(value).is_some()
                            || self.descriptors.slots.contains_key(&at.place.slot)
                        {
                            self.gap(
                                n,
                                "capture descriptors through this projection are not built",
                            );
                        }
                        self.anchor_path = path;
                        return false;
                    }
                }
            }
            let old = self.descriptors.slots.get(&at.place.slot).copied();
            let new = self.descriptors.of(value);
            let id = self.descriptors.replace(old, &path, new);
            self.anchor_path = path;
            self.set_slot_descriptor(at.place.slot, id);
        }
        true
    }
    pub(super) fn capture_store(&mut self, at: At, ty: TyId, n: NodeId) {
        let Some(frame) = &self.capture_frame else {
            return;
        };
        if frame.outer[at.place.slot as usize].is_none() {
            return;
        }
        if kind(&self.p.decls, &self.t, ty, &self.params) == Kind::Scoped
            && let Some(f) = self.freezes.iter().find(|f| {
                // A view parameter, or a `mut` one a result is anchored to (OWN-20).
                frame.outer[f.place.slot as usize].is_none()
                    && matches!(
                        self.var(f.place.slot).origin,
                        Origin::ViewParam | Origin::MutParam
                    )
            })
        {
            let cause = self.cause(f.site);
            self.refuse_from(
                "OWN-21",
                self.span(n),
                vec![cause],
                "a callback invocation parameter cannot escape into a capture",
            );
        }
    }
}

impl Descriptors {
    pub(super) fn replace_slot(&mut self, slot: u32, id: Option<u32>) {
        if let Some(id) = id {
            self.slots.insert(slot, id);
        } else {
            self.slots.remove(&slot);
        }
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn captureless_callable(&mut self, code: ValueId, n: NodeId) -> Option<ValueId> {
        let carrier = self.raw_frame(2, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        self.store_word(carrier, 0, code, n);
        self.store_word(carrier, 8, null, n);
        Some(carrier)
    }
    fn owning_value(
        &mut self,
        callable: TyId,
        code: Option<ValueId>,
        frame: &CaptureFrame,
        n: NodeId,
    ) -> (TyId, Option<ValueId>) {
        let (Some(result), Some(cap)) = (self.p.core_decl("Result"), self.p.core_decl("Capacity"))
        else {
            return self.gap_ty(n, "owning closures need Result and Capacity declarations");
        };
        let cap = self.t.mk(TyKind::Adt(cap, vec![]));
        let types = frame
            .captures
            .iter()
            .map(|c| self.var(c.slot).ty)
            .collect::<Vec<_>>();
        let fields = if types.len() == 1 {
            types[0]
        } else {
            self.t.mk(TyKind::Tuple(types.clone()))
        };
        let back = self.t.mk(TyKind::Tuple(vec![cap, fields]));
        let result = self.t.mk(TyKind::Adt(result, vec![callable, back]));
        let mut values = vec![];
        let mut reps = vec![];
        for (capture, &ty) in frame.captures.iter().zip(&types) {
            let at = At {
                place: Place::whole(capture.slot),
                ty,
                shared: false,
            };
            self.use_place(at, Use::Value, capture.site);
            let value = self.emit_place(at, capture.site);
            self.temp(ty, value);
            values.push(value);
            match self.rep(ty) {
                Ok(rep) => reps.push(rep),
                Err(what) => {
                    self.gap(n, what);
                    return (result, None);
                }
            }
        }
        let Some(charge) = hirsel_core_layout::charge::closure_env(frame.logical) else {
            self.refuse("OWN-2", n, "closure charge exceeds u64");
            return (ERROR, None);
        };
        if !self.live() {
            return (result, None);
        }
        let code = code.unwrap();
        let Some(glue) = self.environment_drop(frame, &types, charge, n) else {
            return (result, None);
        };
        let glue_value = self.value(Ty::Ptr);
        self.inst(
            Inst::FunctionRef {
                result: glue_value,
                function: glue,
            },
            n,
        );
        let id = match self.rep(callable) {
            Ok(Rep::Place(id)) => id,
            Ok(_) => unreachable!("an owning callable has a carrier"),
            Err(what) => {
                self.gap(n, what);
                return (result, None);
            }
        };
        let allocation = admission::OwnedAllocation {
            layout: None,
            kind: AllocKind::Own {
                edge: FieldTy::OwnFn,
            },
            charge,
            size: (frame.physical + 7) & !7,
            align: 8,
        };
        let value =
            self.admit_with_handback(result, allocation, &reps, &values, n, |cx, to, block| {
                cx.store_word(block, 0, glue_value, n);
                for (i, capture) in frame.captures.iter().enumerate() {
                    if let Some(place) = reps[i].stored() {
                        let offset = cx
                            .konst(Ty::Scalar(Scalar::U64), capture.offset, n)
                            .unwrap();
                        let at =
                            cx.address(AddressBase::Value(block), &[Step::Byte(offset)], place, n);
                        cx.install(at, reps[i], values[i], n);
                    }
                }
                let ok = cx.address(
                    AddressBase::Address(to),
                    &[Step::CaseField { case: 0, field: 0 }],
                    PlaceTy::Declared(id),
                    n,
                );
                let carrier = cx.pointer(ok, n);
                cx.store_word(carrier, 0, code, n);
                cx.store_word(carrier, 8, block, n);
            });
        (result, value)
    }
    fn environment_drop(
        &mut self,
        frame: &CaptureFrame,
        types: &[TyId],
        charge: u64,
        n: NodeId,
    ) -> Option<FuncId> {
        let mut child = Cx::new(self.p, self.definition, self.e);
        let mut fd = self.fd.as_ref().clone();
        fd.params.clear();
        fd.result = UNIT;
        fd.generics.clear();
        fd.drop_of = None;
        fd.clone_of = None;
        fd.waits = false;
        fd.body = None;
        fd.path.0.name = format!(
            "{}$environment{}",
            fd.path.0.name,
            n.0 - self.item.range.nodes.start
        );
        child.fd = std::borrow::Cow::Owned(fd);
        child.entry = false;
        std::mem::swap(&mut child.t, &mut self.t);
        let function = child.e.function(
            &child.fd.path,
            Signature {
                params: vec![Param::Value(Ty::Ptr)],
                results: vec![],
                dest: None,
                never: false,
            },
        );
        child.func = Some(function);
        let entry = child.new_block(&[Ty::Ptr]);
        child.start(entry);
        let environment = child.block_params(entry)[0];
        for (capture, &ty) in frame.captures.iter().zip(types).rev() {
            let place = match child.rep(ty) {
                Ok(rep) => rep.stored(),
                Err(what) => {
                    child.gap(n, what);
                    None
                }
            };
            if let Some(place) = place {
                let offset = child
                    .konst(Ty::Scalar(Scalar::U64), capture.offset, n)
                    .unwrap();
                let at = child.address(
                    AddressBase::Value(environment),
                    &[Step::Byte(offset)],
                    place,
                    n,
                );
                child.drop_at(at, ty, n);
            }
        }
        let charge = child.konst(Ty::Scalar(Scalar::U64), charge, n).unwrap();
        child.inst(
            Inst::Free {
                block: environment,
                charge,
            },
            n,
        );
        child.end(Terminator::Return(PoolRange::EMPTY), Some(n));
        std::mem::swap(&mut child.t, &mut self.t);
        let out = child.finish();
        self.out.refusals.extend(out.refusals);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        out.function
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn capture_assignment(&mut self, slot: u32, n: NodeId) -> bool {
        if self
            .capture_frame
            .as_ref()
            .is_some_and(|f| f.owning && f.by_slot[slot as usize].is_some())
        {
            if !self.broken {
                self.refuse("LANG-14", n, "an owning closure never mutates a capture");
            }
            return false;
        }
        true
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn capture_anchors(&mut self, value: Option<ValueId>) -> anchors::Fact {
        let Some(id) = self.descriptors.of(value) else {
            return 0;
        };
        let range = self.descriptors.ranges[id as usize];
        let mut fact = 0;
        for i in range.start..range.start + range.len {
            let slot = self.descriptors.captures[i as usize].slot;
            let ty = self.var(slot).ty;
            let captured = if self.region_handle(ty) {
                self.anchors.named_region(slot)
            } else {
                self.anchor_read(At {
                    place: Place::whole(slot),
                    ty,
                    shared: false,
                })
            };
            fact = self.anchors.union(fact, captured);
        }
        fact
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn callable_abi_cache_occupies_only_mentioned_types() {
        let (index, modules) = crate::tests::program_inputs("fn only() -> u64 { 42 }\n");
        let p = Program::declare(&index, &modules);
        let f = p.fns.iter().position(|f| f.path.0.name == "only").unwrap();
        let mut emitter = Emitter::new();
        let mut cx = Cx::new(&p, f, &mut emitter);
        for n in 1..65 {
            cx.t.mk(TyKind::Array(UNIT, n));
        }
        let value = scalar(Scalar::U64);
        let callable = cx.t.mk(TyKind::Fn(
            vec![(Mode::View, value)],
            value,
            false,
            Callable::Plain,
        ));
        assert!(callable.0 >= 64);
        let mut scratch = CallScratch {
            params: vec![(Mode::View, value)],
            ..Default::default()
        };
        let n = cx.item.root;
        let first = cx.value_call_abi(callable, value, n, &mut scratch).unwrap();
        assert_eq!(
            cx.callable_signatures.len(),
            1,
            "unused types took callable-cache slots"
        );
        let again = cx.value_call_abi(callable, value, n, &mut scratch).unwrap();
        assert_eq!(again.signature, first.signature);
        assert_eq!(cx.callable_reps.len(), 1);
        assert_eq!(cx.callable_signatures.len(), 1);
    }
}
