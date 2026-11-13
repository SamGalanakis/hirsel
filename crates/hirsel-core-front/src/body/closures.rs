//! Callable values, using FORM's code pointers and ordinary call rows (LANG-13).
use super::calls::CallScratch;
use super::*;

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
        if g.body.is_none() && g.path.package_is("core", "1") {
            return self.gap_ty(n, "core function values need callable wrappers");
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
        // A direct rigid T uses an address ABI. Exposing its scalar/unit closed
        // instance as a normal callable requires an adapter, which is not emitted.
        let direct_result = matches!(self.t.kind(g.result), TyKind::Param(_));
        let direct_params = g
            .params
            .iter()
            .any(|p| p.mode != Mode::Mut && matches!(self.t.kind(p.ty), TyKind::Param(_)));
        if direct_result || direct_params {
            return self.gap_ty(n, "generic function-value ABI adapters are not built");
        }
        let (function, _) = match self.instance_row(f, &scratch.tys) {
            Ok(row) => row,
            Err(what) => return self.gap_ty(n, what),
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
        if matches!(self.t.kind(result), TyKind::Param(_))
            || scratch.params.iter().any(|(mode, ty)| {
                *mode != Mode::Mut && matches!(self.t.kind(*ty), TyKind::Param(_))
            })
        {
            return self.gap_ty(n, "indirect calls with opaque generic values are not built");
        }
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
        let (code, environment) = if callable == Callable::Plain {
            (value, None)
        } else {
            (
                self.callable_word(value, 0, n),
                self.callable_word(value, 8, n),
            )
        };
        let v = self.emit_value_call(code, environment, result, n, scratch);
        if result == NEVER {
            self.dead = true;
        }
        (result, v)
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

    fn emit_value_call(
        &mut self,
        code: Option<ValueId>,
        environment: Option<ValueId>,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let code = code?;
        scratch.ir_params.clear();
        scratch.ir_results.clear();
        scratch
            .ir_params
            .extend(environment.map(|_| Param::Value(Ty::Ptr)));
        for &(mode, ty) in &scratch.params {
            let rep = match self.rep(ty) {
                Ok(r) => r,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let param = if mode == Mode::Mut {
                Param::Address(self.e.type_row(self.p, &mut self.t, ty)?)
            } else {
                match rep {
                    Rep::None => continue,
                    Rep::Scalar(t) => Param::Value(t),
                    Rep::View(_) => Param::Value(Ty::Ptr),
                    Rep::Place(id) => Param::Address(id),
                }
            };
            scratch.ir_params.push(param);
        }
        let dest = match self.rep(result) {
            Ok(Rep::None) => None,
            Ok(Rep::Scalar(t)) => {
                scratch.ir_results.push(t);
                None
            }
            Ok(Rep::View(_)) => {
                scratch.ir_results.push(Ty::Ptr);
                None
            }
            Ok(Rep::Place(id)) => Some(id),
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let signature = Signature {
            params: std::mem::take(&mut scratch.ir_params),
            results: std::mem::take(&mut scratch.ir_results),
            dest,
            never: result == NEVER,
        };
        let signature = if let Some(&id) = self.e.sigs.get(&signature) {
            scratch.ir_params = signature.params;
            scratch.ir_results = signature.results;
            id
        } else {
            self.e.sig(signature)
        };
        let dest = dest.map(|id| self.new_slot(PlaceTy::Declared(id), n));
        let start = self.b.pools.args.len();
        self.b.pools.args.extend(dest.map(Arg::Address));
        self.b.pools.args.extend(environment.map(Arg::Value));
        for (i, &(mode, ty)) in scratch.params.iter().enumerate() {
            if mode == Mode::Take {
                self.consume(scratch.vals[i], n);
            }
            let rep = self.rep(ty).ok()?;
            if rep == Rep::None && mode != Mode::Mut {
                continue;
            }
            let v = scratch.vals[i]?;
            let arg = if mode == Mode::Mut {
                let id = self.e.type_row(self.p, &mut self.t, ty)?;
                Arg::Address(self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n))
            } else if let Rep::Place(id) = rep {
                Arg::Address(self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n))
            } else {
                Arg::Value(v)
            };
            self.b.pools.args.push(arg);
        }
        let args = PoolRange::new(start as u32, (self.b.pools.args.len() - start) as u32);
        let first = self.e.module.signatures[signature.index()]
            .results
            .first()
            .copied()
            .map(|ty| self.value(ty));
        let results = PoolRange::push(&mut self.b.pools.values, first);
        let fail = self.fail_to(n);
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
        match dest {
            Some(a) => {
                let ptr = self.pointer(a, n);
                self.temp(result, Some(ptr));
                Some(ptr)
            }
            None => first,
        }
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn closure(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        if !self.fd.generics.is_empty() {
            return self.gap_ty(n, "closure bodies inside generic definitions are not built");
        }
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
        if !written_result && let Some(w) = want.filter(|w| w.payload == 0) {
            if let TyKind::Fn(_, result, _, _) = self.t.kind(w.ty)
                && *result != HOLE
            {
                fd.result = *result;
            }
        }
        let scoped = self.kind(n) == NodeKind::ScopedClosure
            || want.is_some_and(|w| {
                matches!(self.t.kind(w.ty), TyKind::Fn(_, _, _, Callable::Scoped))
            });
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
        let mut child = Cx::new(self.p, self.definition, self.e);
        child.fd = std::borrow::Cow::Owned(fd);
        child.header = n;
        child.entry = false;
        child.capture_frame = Some(CaptureFrame {
            outer: self.vars.clone(),
            by_slot: vec![None; self.vars.len()],
            captures: vec![],
            environment: None,
            has_environment: scoped,
        });
        std::mem::swap(&mut child.t, &mut self.t);
        let signature = child.callable_signature(ty, scoped);
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
        let captures = child.capture_frame.take().unwrap().captures;
        let out = child.finish();
        let function = out.function;
        self.out.refusals.extend(out.refusals);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        if function.is_none() {
            self.halt();
        }
        if !scoped && !captures.is_empty() {
            if want
                .is_some_and(|w| matches!(self.t.kind(w.ty), TyKind::Fn(_, _, _, Callable::Plain)))
            {
                self.refuse("LANG-13", n, "a plain callable captures nothing");
                return (ERROR, None);
            }
            return self.gap_ty(n, "owning closure admission is not built");
        }
        let value = if self.live() {
            let function = function.unwrap();
            let result = self.value(Ty::Ptr);
            self.inst(Inst::FunctionRef { result, function }, n);
            if scoped {
                self.scoped_value(result, &captures, n)
            } else {
                Some(result)
            }
        } else {
            None
        };
        (ty, value)
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
                    Rep::View(_) => Param::Value(Ty::Ptr),
                    Rep::Place(id) => Param::Address(id),
                }
            });
        }
        let (results, dest) = match self.rep(r)? {
            Rep::None => (vec![], None),
            Rep::Scalar(t) => (vec![t], None),
            Rep::View(_) => (vec![Ty::Ptr], None),
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
}

pub(super) struct CaptureFrame {
    outer: Vec<Option<Var>>,
    by_slot: Vec<Option<usize>>,
    captures: Vec<Capture>,
    pub(super) environment: Option<ValueId>,
    pub(super) has_environment: bool,
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
    pub(super) fn of(&self, value: Option<ValueId>) -> Option<u32> {
        self.values.get(&value?).copied()
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// Discover an outer slot exactly when its first source use is visited.
    pub(super) fn capture_slot(&mut self, slot: u32, n: NodeId) {
        let Some(frame) = &mut self.capture_frame else {
            return;
        };
        let Some(original) = frame.outer[slot as usize] else {
            return;
        };
        if frame.by_slot[slot as usize].is_some() {
            return;
        }
        if matches!(
            self.t.kind(original.ty),
            TyKind::Fn(_, _, _, Callable::Scoped)
        ) {
            self.gap(n, "captured callback descriptor substitution is not built");
            return;
        }
        if matches!(self.t.kind(original.ty),TyKind::Adt(d,_) if self.p.decls[d.index()].role==Some(Role::Writer))
        {
            self.refuse("MEM-4", n, "a Writer is never captured");
            return;
        }
        let frame = self.capture_frame.as_mut().unwrap();
        let index = frame.captures.len();
        frame.by_slot[slot as usize] = Some(index);
        frame.captures.push(Capture {
            slot,
            mode: Mode::View,
        });
        let environment = frame.environment;
        let binding = if let Some(environment) = environment.filter(|_| self.live()) {
            let offset = self
                .konst(Ty::Scalar(Scalar::U64), index as u64 * 8, n)
                .unwrap();
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
            let address = self
                .rep(original.ty)
                .ok()
                .and_then(Rep::stored)
                .map(|ty| self.address(AddressBase::Value(pointer), &[], ty, n));
            Binding::Slot(address)
        } else {
            Binding::Value(None)
        };
        self.vars[slot as usize] = Some(Var {
            binding,
            origin: if original.origin.writable() {
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
    pub(super) fn capture_write(&mut self, slot: u32) {
        if let Some(frame) = &mut self.capture_frame
            && let Some(i) = frame.by_slot[slot as usize]
        {
            frame.captures[i].mode = Mode::Mut;
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
            self.refuse("LANG-14", n, "a scoped closure never moves a capture");
            return false;
        }
        true
    }
    fn capture_address(&mut self, slot: u32, n: NodeId) -> Option<ValueId> {
        self.capture_slot(slot, n);
        let var = self.var(slot);
        match (var.binding, self.rep(var.ty).ok()?) {
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
