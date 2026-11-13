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
        let g = &self.p.fns[f];
        if g.params.iter().any(|p| p.bytes) {
            return self.gap_ty(n, "function values with bytes parameters are not built");
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
        if self.patterns.guard && scratch.params.iter().any(|p| p.0 != Mode::View) {
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
        if kind(&self.p.decls, &self.t, result, &self.params) == Kind::Scoped {
            self.gap(n, "scoped callable result anchors are not built");
        }
        let abi = self.value_call_abi(ty, result, callable != Callable::Plain, n, scratch);
        if let Some(abi) = abi {
            for j in abi.regions.start..abi.regions.start + abi.regions.len {
                let i = self.callable_regions[j as usize] as usize;
                if self.kind(args[i]) != NodeKind::Path
                    || !matches!(self.p.target(self.at, args[i]), Some(Target::Binder(_)))
                {
                    return self.gap_ty(
                        n,
                        "a call result rooted in a Region expression is not built",
                    );
                }
            }
        }
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
            if matches!(
                self.kind(arg),
                NodeKind::LabelledArgument | NodeKind::BytesArgument
            ) {
                self.gap(arg, "marked arguments to callable values are not built");
                continue;
            }
            let (mode, pt) = scratch.params[i];
            if !self.argument(
                mode,
                pt,
                call,
                i,
                arg,
                &mut scratch.found,
                &mut scratch.vals[i],
            ) {
                scratch.vals[i] = self
                    .expr(
                        arg,
                        Some(Want {
                            ty: pt,
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
        let mut result = result;
        if let Some(abi) = abi {
            for j in abi.regions.start..abi.regions.start + abi.regions.len {
                let i = self.callable_regions[j as usize];
                let root = self.body_root(args[i as usize], true);
                result = reroot(&mut self.t, result, Root::Parameter(i), root);
            }
        }
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
        // Scalar arguments are spilled; rigid generic values already travel by address.
        scratch.ir_args.clear();
        scratch.ir_args.extend(dest.map(Arg::Address));
        scratch.ir_args.extend(environment.map(Arg::Value));
        for (i, &(mode, _)) in scratch.params.iter().enumerate() {
            if mode == Mode::Take {
                self.consume(scratch.vals[i], n);
            }
            let id = match self.e.module.signatures[signature.index()].params
                [i + usize::from(environment.is_some())]
            {
                Param::Address(id) => id,
                Param::Value(_) => {
                    scratch.ir_args.push(Arg::Value(scratch.vals[i]?));
                    continue;
                }
            };
            let pointer = if mode == Mode::Mut {
                scratch.vals[i]
            } else {
                let rep = self.callable_reps[abi.params.start as usize + i];
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
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Indirect(code),
            signature,
            args,
            results: PoolRange::EMPTY,
            fail,
        });
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            self.end(Terminator::Unreachable, Some(n));
            return None;
        }
        let value = self.read_carried_rep(dest?, abi.result, n);
        self.temp(result, value);
        value
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn closure(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let mut fd = self.fd.as_ref().clone();
        fd.drop_of = None;
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
        let expected_kind = expected.and_then(|ty| match self.t.kind(ty) {
            TyKind::Fn(_, _, _, k) => Some(*k),
            _ => None,
        });
        let scoped =
            self.kind(n) == NodeKind::ScopedClosure || expected_kind == Some(Callable::Scoped);
        let environment =
            scoped || matches!(expected_kind, Some(Callable::Own | Callable::OwnSend));
        let ty = self.t.mk(TyKind::Fn(
            fd.params.iter().map(|p| (p.mode, p.ty)).collect(),
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
            let signature = child.e.sig(signature);
            let f = child.e.function(
                &child.fd.path,
                child.e.module.signatures[signature.index()].clone(),
            );
            (f, signature)
        });
        child.run_row(row, body);
        std::mem::swap(&mut child.t, &mut self.t);
        let frame = child.capture_frame.take().unwrap();
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

    fn callable_signature(
        &mut self,
        ty: TyId,
        environment: bool,
    ) -> Result<Signature, &'static str> {
        let TyKind::Fn(ps, r, _, _) = self.t.kind(ty) else {
            unreachable!()
        };
        let (len, r) = (ps.len(), *r);
        let mut params = vec![];
        if environment {
            params.push(Param::Value(Ty::Ptr));
        }
        for i in 0..len {
            let TyKind::Fn(ps, _, _, _) = self.t.kind(ty) else {
                unreachable!()
            };
            let (mode, t) = ps[i];
            let rep = self.rep(t)?;
            params.push(if mode == Mode::Mut {
                Param::Address(
                    self.e
                        .type_row(self.p, &mut self.t, t)
                        .ok_or("closure parameter layout is not built")?,
                )
            } else {
                match rep {
                    Rep::None => continue,
                    Rep::Scalar(t) => Param::Value(t),
                    Rep::Place(id) => Param::Address(id),
                }
            });
        }
        let (results, dest) = match self.rep(r)? {
            Rep::None => (vec![], None),
            Rep::Scalar(t) => (vec![t], None),
            Rep::Place(id) => (vec![], Some(id)),
        };
        Ok(Signature {
            params,
            results,
            dest,
            never: r == NEVER,
        })
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
        Some(id)
    }
    /// Capture identity is a set of (place, mode), independent of closure code,
    /// source site, first-naming order and environment offsets (OWN-17/OWN-21).
    pub(super) fn same(&self, a: Option<u32>, b: Option<u32>) -> bool {
        if a == b {
            return true;
        }
        let captures = |id: Option<u32>| {
            id.map_or(&[][..], |id| {
                self.ranges[id as usize].get(&self.captures).unwrap()
            })
        };
        let (a, b) = (captures(a), captures(b));
        a.len() == b.len()
            && a.iter()
                .all(|x| b.iter().any(|y| x.slot == y.slot && x.mode == y.mode))
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
        } else if !owning
            && matches!(
                self.t.kind(original.ty),
                TyKind::Fn(_, _, _, Callable::Scoped)
            )
        {
            self.gap(n, "captured callback descriptor substitution is not built");
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
            self.e.module.functions[f.index()].signature = self.e.sig(signature);
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
    pub(super) fn read_descriptor(&mut self, slot: u32, value: Option<ValueId>) {
        if let (Some(value), Some(&id)) = (value, self.descriptors.slots.get(&slot)) {
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
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) if a != b => (a, b),
            _ => return a.or(b),
        };
        let start = self.descriptors.captures.len();
        let ra = self.descriptors.ranges[a as usize];
        for i in ra.start..ra.start + ra.len {
            let c = self.descriptors.captures[i as usize];
            self.descriptors.captures.push(c);
        }
        let rb = self.descriptors.ranges[b as usize];
        for i in rb.start..rb.start + rb.len {
            let c = self.descriptors.captures[i as usize];
            if let Some(other) = self.descriptors.captures[start..]
                .iter_mut()
                .find(|other| other.slot == c.slot)
            {
                if c.mode == Mode::Mut {
                    other.mode = Mode::Mut;
                }
            } else {
                self.descriptors.captures.push(c);
            }
        }
        let id = self.descriptors.ranges.len() as u32;
        self.descriptors.ranges.push(PoolRange::new(
            start as u32,
            (self.descriptors.captures.len() - start) as u32,
        ));
        Some(id)
    }
    pub(super) fn set_slot_descriptor(&mut self, slot: u32, id: Option<u32>) {
        let before = self.descriptors.slots.get(&slot).copied();
        if before != id {
            let v = self.var(slot);
            self.log.push((slot, v.avail, v.at, before));
            self.descriptors.replace_slot(slot, id);
        }
    }
    pub(super) fn descriptor_store(&mut self, at: At, value: Option<ValueId>, n: NodeId) {
        if let Some(id) = self.descriptors.of(value) {
            if at.place.len > 0 {
                self.gap(
                    n,
                    "capture descriptors in scoped aggregate fields are not built",
                );
                return;
            }
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
