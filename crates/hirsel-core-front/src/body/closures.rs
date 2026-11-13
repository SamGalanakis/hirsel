//! Callable values, using FORM's code pointers and ordinary call rows (LANG-13).
use super::calls::CallScratch;
use super::*;

#[derive(Clone, Copy)]
pub(super) struct CallableAbi {
    signature: SigId,
    params: PoolRange<Rep>,
    result: Rep,
    regions: PoolRange<u32>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
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
        if kind(&self.p.decls, &self.t, g.result, &self.params) == Kind::Scoped {
            return self.gap_ty(
                n,
                "scoped callable result region anchors are not represented",
            );
        }
        let ty = g.callable;
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
        let mut ty = subst(&mut self.t, ty, &scratch.tys);
        // A named item can use an environment-taking adapter without allocating
        // an environment. Its code identity is known at this value's creation.
        if let Some(w) = want.filter(|w| w.payload == 0)
            && matches!((self.t.kind(ty), self.t.kind(w.ty)),
                (TyKind::Fn(a, ar, aw, Callable::Plain), TyKind::Fn(b, br, bw, Callable::Own | Callable::OwnSend | Callable::Scoped))
                    if a == b && ar == br && (!aw || *bw))
        {
            ty = w.ty;
        }
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
        let value = match self.t.kind(ty) {
            TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend) => {
                let value = value.and_then(|code| self.empty_own_callable(code, n));
                self.temp(ty, value);
                value
            }
            TyKind::Fn(_, _, _, Callable::Scoped) => {
                value.and_then(|code| self.scoped_value(code, &[], n))
            }
            _ => value,
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
                self.refuse("LANG-13", callee, "the value being called is not callable");
                return (ERROR, None);
            }
        };
        if args.len() < scratch.params.len() {
            self.refuse_at(
                "LANG-11",
                self.close_brace(n),
                "a callable needs one argument per parameter",
            );
            return (ERROR, None);
        }
        if args.len() > scratch.params.len() {
            self.refuse(
                "LANG-11",
                args[scratch.params.len()],
                "extra callable argument",
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
                self.refuse(
                    "LANG-8",
                    n,
                    "invokes a waits callable but is not declared waits",
                );
            }
        }
        let abi = self.value_call_abi(ty, result, callable != Callable::Plain, n, scratch);
        if let Some(abi) = abi {
            for j in abi.regions.start..abi.regions.start + abi.regions.len {
                let i = self.callable_regions[j as usize] as usize;
                if self.binder_path(args[i]).is_none() {
                    return self.gap_ty(
                        n,
                        "a call result rooted in a Region expression is not built",
                    );
                }
            }
        }
        self.call_region_roots(&scratch.params, args, &mut scratch.roots);
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
                    self.refuse("OWN-7", arg, "this parameter takes no `bytes` mark");
                    self.kids(arg)[0]
                }
                (_, true) => {
                    self.refuse("OWN-7", arg, "this parameter needs a `bytes` argument");
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
                &scratch.roots,
            ) {
                let expected = self.partial_rooted(pt, &scratch.found, &scratch.roots);
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
            Hold::CallView(c, _) | Hold::CallMut(c) if c == call => None,
            other => Some(other),
        });
        let result = self.partial_rooted(result, &scratch.found, &scratch.roots);
        let (code, environment) = if callable == Callable::Plain {
            (value, None)
        } else {
            (
                self.callable_word(value, 0, n),
                self.callable_word(value, 8, n),
            )
        };
        let v = self.emit_value_call(code, environment, abi, result, n, scratch);
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
        let abi = self.value_call_abi(ty, result, callable != Callable::Plain, n, &mut scratch);
        let (code, environment) = if callable == Callable::Plain {
            (Some(value), None)
        } else {
            (
                self.callable_word(Some(value), 0, n),
                self.callable_word(Some(value), 8, n),
            )
        };
        let v = self.emit_value_call(code, environment, abi, result, n, &mut scratch);
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
        environment: bool,
        n: NodeId,
        scratch: &mut CallScratch,
    ) -> Option<CallableAbi> {
        let abi = match self
            .callable_signatures
            .get(callable.0 as usize)
            .copied()
            .flatten()
        {
            Some(abi) => abi,
            None => {
                let signature = match self.indirect_signature(
                    &scratch.params,
                    result,
                    environment,
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
                    if self.region_handle(ty) && self.parameter_region_result(result_ty, i as u32) {
                        self.callable_regions.push(i as u32);
                    }
                }
                let abi = CallableAbi {
                    signature,
                    params: PoolRange::new(start as u32, scratch.params.len() as u32),
                    result,
                    regions: PoolRange::new(
                        roots as u32,
                        (self.callable_regions.len() - roots) as u32,
                    ),
                };
                self.callable_signatures.resize(self.t.len(), None);
                self.callable_signatures[callable.0 as usize] = Some(abi);
                abi
            }
        };
        Some(abi)
    }
    fn parameter_region_result(&self, ty: TyId, parameter: u32) -> bool {
        match self.t.kind(ty) {
            TyKind::Anchored(inner, root) => {
                matches!(self.t.kind(*root), TyKind::Region(Root::Parameter(i)) if *i == parameter)
                    || self.parameter_region_result(*inner, parameter)
            }
            TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs
                .iter()
                .any(|&x| self.parameter_region_result(x, parameter)),
            TyKind::Array(x, _) | TyKind::View(x) => self.parameter_region_result(*x, parameter),
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
            let id = match param {
                Param::Address { ty: id, .. } => id,
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
                let place = rep.stored().unwrap_or(PlaceTy::Declared(id));
                let at = self.new_slot(place, n);
                self.install(at, rep, scratch.vals[i], n);
                Some(self.pointer(at, n))
            }?;
            let at = self.address(AddressBase::Value(pointer), &[], PlaceTy::Declared(id), n);
            scratch.ir_args.push(Arg::Address(at));
        }
        let args = PoolRange::push(&mut self.b.pools.args, scratch.ir_args.iter().copied());
        let fail = self.fail_to(n);
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
        let mut written_result = false;
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
                    fd.result = self.ty(self.kids(child)[0]);
                    fd.result_site = self.p.site(self.at, child);
                }
                NodeKind::Waits => fd.waits = true,
                NodeKind::Block => fd.body = Some(child),
                _ => {}
            }
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
            fd.result = *result;
        }
        if kind(&self.p.decls, &self.t, fd.result, &self.params) == Kind::Scoped {
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
            fd.result,
            fd.waits,
            if scoped {
                Callable::Scoped
            } else {
                Callable::Plain
            },
        ));
        fd.callable = ty;
        let body = fd.body.unwrap();
        self.capture_nested(body);
        let mut child = Cx::new(self.p, self.definition, self.e);
        child.fd = std::borrow::Cow::Owned(fd);
        child.header = n;
        child.entry = false;
        child.capture_frame = Some(CaptureFrame {
            outer: self.vars.clone(),
            outer_descriptors: std::mem::take(&mut self.descriptors),
            by_slot: vec![None; self.vars.len()],
            captures: vec![],
            environment: None,
            has_environment: environment,
            owning: !scoped,
            physical: 8,
            logical: 0,
            send: true,
        });
        std::mem::swap(&mut child.t, &mut self.t);
        let signature = child.callable_signature(ty, environment);
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
        let Some(function) = self.row_adapter(raw, ty, frame.has_environment, n) else {
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
            return (
                ty,
                code.and_then(|code| self.scoped_value(code, &frame.captures, n)),
            );
        }
        if frame.captures.is_empty() {
            if environment {
                let kind = expected_kind.unwrap_or(Callable::OwnSend);
                let TyKind::Fn(ps, r, w, _) = self.t.kind(ty).clone() else {
                    unreachable!()
                };
                let ty = self.t.mk(TyKind::Fn(ps, r, w, kind));
                let v = code.and_then(|code| self.empty_own_callable(code, n));
                self.temp(ty, v);
                return (ty, v);
            }
            return (ty, code);
        }
        if expected_kind == Some(Callable::Plain) {
            self.refuse("LANG-13", n, "a plain callable captures nothing");
            return (ERROR, None);
        }
        let kind = if frame.send {
            Callable::OwnSend
        } else {
            Callable::Own
        };
        if expected_kind == Some(Callable::OwnSend) && !frame.send {
            self.refuse("LANG-13", n, "own[send] requires send captures");
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

    fn callable_signature(&mut self, ty: TyId, environment: bool) -> Result<SigId, &'static str> {
        let TyKind::Fn(ps, result, _, _) = self.t.kind(ty).clone() else {
            unreachable!()
        };
        self.indirect_signature(&ps, result, environment, &mut vec![])
    }
}

/// One whole outer slot in first-naming order (LANG-14, OWN-21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Capture {
    slot: u32,
    mode: Mode,
    offset: u64,
    site: NodeId,
}

pub(super) struct CaptureFrame {
    outer: Vec<Option<Var>>,
    outer_descriptors: Descriptors,
    by_slot: Vec<Option<usize>>,
    captures: Vec<Capture>,
    pub(super) environment: Option<ValueId>,
    pub(super) has_environment: bool,
    owning: bool,
    physical: u64,
    logical: u64,
    send: bool,
}

#[derive(Default)]
pub(super) struct Descriptors {
    pub(super) captures: Vec<Capture>,
    pub(super) ranges: Vec<PoolRange<Capture>>,
    pub(super) values: HashMap<ValueId, u32>,
    pub(super) slots: HashMap<u32, u32>,
    fields: Vec<PoolRange<(anchors::Component, u32)>>,
    parts: Vec<(anchors::Component, u32)>,
    imports: HashMap<u32, u32>,
}
impl Descriptors {
    fn add(&mut self, captures: &[Capture]) -> Option<u32> {
        if captures.is_empty() {
            return None;
        }
        let id = self.ranges.len() as u32;
        self.ranges.push(PoolRange::push(
            &mut self.captures,
            captures.iter().copied(),
        ));
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
            || !left
                .iter()
                .all(|x| right.iter().any(|y| x.slot == y.slot && x.mode == y.mode))
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
        let start = self.captures.len();
        let ra = self.ranges[a as usize];
        for i in ra.start..ra.start + ra.len {
            let c = self.captures[i as usize];
            self.captures.push(c);
        }
        let rb = self.ranges[b as usize];
        for i in rb.start..rb.start + rb.len {
            let c = self.captures[i as usize];
            if let Some(other) = self.captures[start..]
                .iter_mut()
                .find(|other| other.slot == c.slot)
            {
                if c.mode == Mode::Mut {
                    other.mode = Mode::Mut;
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
        self.fields
            .push(PoolRange::push(&mut self.parts, [(component, child)]));
        Some(id)
    }
    fn project(&mut self, id: Option<u32>, component: anchors::Component) -> Option<u32> {
        let range = self.fields[id? as usize];
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
        let captures = PoolRange::push(
            &mut self.captures,
            range.get(&outer.captures).unwrap().iter().copied(),
        );
        let id = self.ranges.len() as u32;
        self.ranges.push(captures);
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
        let Some(frame) = &self.capture_frame else {
            return;
        };
        let Some(original) = frame.outer[slot as usize] else {
            return;
        };
        if frame.by_slot[slot as usize].is_some() {
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
        if owning && !self.capture_frame.as_ref().unwrap().has_environment && self.live() {
            let environment = self.value(Ty::Ptr);
            let f = self.func.unwrap();
            let mut signature = self.e.module.signatures
                [self.e.module.functions[f.index()].signature.index()]
            .clone();
            signature.params.insert(0, Param::Value(Ty::Ptr));
            let old = self.e.module.functions[f.index()].signature;
            let mut abi = self.e.signature_abis.get(&old).cloned().unwrap_or_default();
            abi.params.insert(0, false);
            self.e.module.functions[f.index()].signature = self.e.sig_abi(signature, abi);
            let mut params = self.block_params(BlockId(0));
            params.insert(usize::from(self.dest.is_some()), environment);
            self.b.blocks[0].params = PoolRange::push(&mut self.b.pools.values, params);
            let frame = self.capture_frame.as_mut().unwrap();
            frame.environment = Some(environment);
            frame.has_environment = true;
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
            index as u64 * 8
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
        self.capture_slot(slot, n);
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
    fn scoped_value(&mut self, code: ValueId, captures: &[Capture], n: NodeId) -> Option<ValueId> {
        let environment = self.raw_frame(captures.len(), n)?;
        for (i, capture) in captures.iter().enumerate() {
            let pointer = self.capture_address(capture.slot, n)?;
            self.store_word(environment, i as u64 * 8, pointer, n);
        }
        let carrier = self.raw_frame(2, n)?;
        self.store_word(carrier, 0, code, n);
        self.store_word(carrier, 8, environment, n);
        if let Some(id) = self.descriptors.add(captures) {
            self.descriptors.values.insert(carrier, id);
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
    ) {
        let id = self.descriptors.of(from);
        let id = self.descriptors.project(id, component);
        if let (Some(value), Some(id)) = (value, id) {
            self.descriptors.values.insert(value, id);
        }
    }
    pub(super) fn read_descriptor(&mut self, at: At, value: Option<ValueId>, n: NodeId) {
        let mut id = self.descriptors.slots.get(&at.place.slot).copied();
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
        let range = self.descriptors.ranges[id as usize];
        for i in range.start..range.start + range.len {
            let capture = self.descriptors.captures[i as usize];
            self.capture_slot(capture.slot, n);
            let at = At {
                place: Place::whole(capture.slot),
                ty: self.var(capture.slot).ty,
                shared: false,
            };
            self.use_place(
                at,
                if capture.mode == Mode::Mut {
                    Use::Mut(Hold::CallMut(call))
                } else {
                    Use::View(Hold::CallView(call, u32::MAX))
                },
                n,
            );
        }
    }
    pub(super) fn descriptor_scope(&mut self, scope: usize, value: Option<ValueId>, n: NodeId) {
        let Some(id) = self.descriptors.of(value) else {
            return;
        };
        let range = self.descriptors.ranges[id as usize];
        if range
            .get(&self.descriptors.captures)
            .unwrap()
            .iter()
            .any(|c| self.scope_slots[scope..].contains(&c.slot))
        {
            self.refuse_at(
                "OWN-21",
                self.close_brace(n),
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
    /// A mut signature has no post-call capture descriptor yet. Walk only the
    /// inline scoped structure; ordinary type arguments cannot be scoped (OWN-19).
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
    pub(super) fn descriptor_store(&mut self, at: At, value: Option<ValueId>, n: NodeId) {
        if self.contains_callback(at.ty)
            && (matches!(
                self.var(at.place.slot).origin,
                Origin::MutParam | Origin::ViewParam | Origin::PatternView
            ) || self
                .capture_frame
                .as_ref()
                .is_some_and(|f| f.outer[at.place.slot as usize].is_some()))
        {
            self.gap(
                n,
                "capture descriptor updates through a borrowed place are not built",
            );
            return;
        }
        if let Some(id) = self.descriptors.of(value) {
            let target = self.var(at.place.slot).depth;
            let range = self.descriptors.ranges[id as usize];
            if range
                .get(&self.descriptors.captures)
                .unwrap()
                .iter()
                .any(|c| self.var(c.slot).depth > target)
            {
                self.refuse(
                    "OWN-21",
                    n,
                    "a scoped callback cannot outlive a captured slot",
                );
                return;
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
                        return;
                    }
                }
            }
            let old = self.descriptors.slots.get(&at.place.slot).copied();
            let new = self.descriptors.of(value);
            let id = self.descriptors.replace(old, &path, new);
            self.set_slot_descriptor(at.place.slot, id);
            self.anchor_path = path;
        }
    }
    pub(super) fn capture_store(&mut self, at: At, ty: TyId, n: NodeId) {
        let Some(frame) = &self.capture_frame else {
            return;
        };
        if frame.outer[at.place.slot as usize].is_none() {
            return;
        }
        if kind(&self.p.decls, &self.t, ty, &self.params) == Kind::Scoped
            && self.freezes.iter().any(|f| {
                frame.outer[f.place.slot as usize].is_none()
                    && self.var(f.place.slot).origin == Origin::ViewParam
            })
        {
            self.refuse(
                "OWN-21",
                n,
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
    fn empty_own_callable(&mut self, code: ValueId, n: NodeId) -> Option<ValueId> {
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
            kind: AllocKind::Own,
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
            let captured = self.anchor_read(At {
                place: Place::whole(slot),
                ty,
                shared: false,
            });
            fact = self.anchors.union(fact, captured);
        }
        fact
    }
}
