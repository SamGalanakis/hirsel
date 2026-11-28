//! Callable values, using FORM's code pointers and ordinary call rows (LANG-13).
use super::callable_abi::CallableAbi;
use super::calls::CallScratch;
use super::*;

#[derive(Clone, Copy)]
pub(super) enum CallFailure {
    Cleanup,
    Handler(BlockId),
}

/// Whether a value call steps on entering its callee (MEM-30): a callable a core
/// function calls back takes one step on entry when it is user code, and core code
/// takes none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Entry {
    Step,
    Core,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    #[inline(always)]
    pub(super) fn isolate_descriptor_forbidden(&mut self, value: Option<ValueId>) -> bool {
        value.is_some() && self.view_descriptor_offender(value).is_some()
    }

    pub(super) fn step_descriptor_forbidden(&mut self, value: Option<ValueId>) -> bool {
        let Some(&born) = self.step_scopes.last() else {
            return false;
        };
        let Some(id) = self.descriptors.of(value) else {
            return true;
        };
        self.view_descriptor_offender_at(id, &mut Vec::new(), Some(born))
            .is_some()
    }

    fn view_descriptor_offender(&mut self, value: Option<ValueId>) -> Option<NodeId> {
        let value = value?;
        let Some(id) = self.descriptors.of(Some(value)) else {
            return Some(self.node);
        };
        self.view_descriptor_offender_at(id, &mut Vec::new(), None)
    }

    /// Inspect projected types and follow the finite descriptor graph once. A
    /// received unqualified callback is unknown, including through an alias.
    fn view_descriptor_offender_at(
        &mut self,
        id: u32,
        seen: &mut Vec<u32>,
        born: Option<(u32, u32)>,
    ) -> Option<NodeId> {
        if seen.contains(&id) {
            return None;
        }
        seen.push(id);
        let range = self.descriptors.ranges[id as usize];
        let mut captures = range.get(&self.descriptors.captures).unwrap().to_vec();
        if let Some(projected) = self.descriptors.projected.get(&id) {
            // Replace only the viewed root covered by a precise projection.
            // An unrelated capture, or a mut capture of that root, still applies.
            captures.retain(|capture| {
                capture.mode != Mode::View
                    || capture.path.is_some()
                    || !projected.iter().any(|p| p.slot == capture.slot)
            });
            captures.extend_from_slice(projected);
        }
        if let Some(capture) = captures.iter().find(|c| {
            born.map_or(c.mode != Mode::View, |(frame, born)| {
                let var = self.var(c.slot);
                c.mode == Mode::Mut
                    && (var.frame_level < frame || var.frame_level == frame && var.born < born)
            })
        }) {
            return Some(capture.site);
        }
        let fields = self.descriptors.fields[id as usize]
            .get(&self.descriptors.parts)
            .unwrap()
            .to_vec();
        if fields.is_empty() {
            for capture in captures {
                let var = self.var(capture.slot);
                let ty = self
                    .descriptor_capture_type(capture, var.ty)
                    .unwrap_or(var.ty);
                if !self.contains_callback(ty) {
                    continue;
                }
                let unknown = self.contains_callback_kind::<true>(ty);
                if unknown
                    && matches!(
                        var.origin,
                        Origin::ViewParam | Origin::MutParam | Origin::TakeParam
                    )
                {
                    return Some(capture.site);
                }
                if let Some(&child) = self.descriptors.slots.get(&capture.slot) {
                    let mut child = Some(child);
                    let mut path = Vec::new();
                    let mut at = capture.path.unwrap_or(0);
                    while at != 0 {
                        let step = self.descriptors.paths[at as usize - 1];
                        path.push(step.field);
                        at = step.parent;
                    }
                    for field in path.into_iter().rev() {
                        child = self
                            .descriptors
                            .project(child, anchors::Component::Field(field));
                    }
                    if let Some(child) = child
                        && let Some(site) = self.view_descriptor_offender_at(child, seen, born)
                    {
                        return Some(site);
                    }
                } else if unknown {
                    return Some(capture.site);
                }
            }
            return None;
        }
        for (_, child) in fields {
            if let Some(site) = self.view_descriptor_offender_at(child, seen, born) {
                return Some(site);
            }
        }
        None
    }

    fn guarantee_view_descriptor(&mut self, value: Option<ValueId>, site: NodeId) {
        if let Some(value) = value
            && self.descriptors.of(Some(value)).is_none()
        {
            let id = self.descriptors.empty(site);
            self.descriptors.values.insert(value, id);
        }
    }

    pub(super) fn fit_scoped_descriptor(
        &mut self,
        from: TyId,
        value: Option<ValueId>,
        site: NodeId,
    ) {
        if matches!(
            self.t.kind(self.bare(from)),
            TyKind::Fn(
                _,
                _,
                _,
                Callable::Own | Callable::OwnSend | Callable::ScopedView
            )
        ) {
            self.guarantee_view_descriptor(value, site);
        }
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
        let TyKind::Fn(b, br, bw, target @ (Callable::Scoped | Callable::ScopedView)) =
            self.t.kind(self.bare(to))
        else {
            return false;
        };
        matches!(self.t.kind(self.bare(from)),
            TyKind::Fn(a, ar, aw, kind)
                if a == b && ar == br && (!aw || *bw)
                    && (matches!(kind, Callable::Own | Callable::OwnSend)
                        || *kind == Callable::Scoped && *target == Callable::Scoped && self.is_view(from)
                        || *kind == Callable::ScopedView && (*target == Callable::Scoped || self.is_view(from))))
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
                Callable::Own | Callable::OwnSend | Callable::Scoped | Callable::ScopedView
            )
        ) {
            return false;
        }
        matches!((self.t.kind(self.bare(from)), self.t.kind(to)),
            (TyKind::Fn(a, ar, aw, Callable::Plain), TyKind::Fn(b, br, bw, Callable::Own | Callable::OwnSend | Callable::Scoped | Callable::ScopedView))
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
            TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView)
        ) {
            self.guarantee_view_descriptor(value, n);
        }
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
        let Some(ty) = self.checked_function_item(f, n) else {
            return (ERROR, None);
        };
        let g = &self.p.fns[f];
        scratch.found.clear();
        scratch.found.resize(g.generics.len(), None);
        scratch.checked.clear();
        scratch.checked.resize(g.generics.len(), false);
        // PROG-9 permits region-generic function items. Region arguments have no
        // runtime layout and are erased by Emitter::instance; seed their symbolic
        // roots so a StaticFn item does not require an expected callable type merely
        // to close a lifetime binder.
        for (i, generic) in g.generics.iter().enumerate() {
            if generic.region {
                scratch.found[i] = Some(self.t.mk(TyKind::Region(Root::Generic(i as u32))));
            }
        }
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
            self.determined(f, &scratch.found, &mut scratch.checked, n, n);
        }
        if scratch
            .found
            .iter()
            .any(|t| t.is_none_or(|t| has_hole(&self.t, t)))
        {
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
            self.item_value(result, f, &scratch.tys, function, ty, n);
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
        dispatch: bool,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        self.call_seq += 1;
        let call = self.call_seq;
        let holds = self.freezes.len();
        let before = self.out.refusals.len();
        let (ty, value) = match self.place(callee) {
            Ok(Some(at)) => {
                self.use_place(at, Use::View(Hold::CallView(call, u32::MAX)), callee);
                (at.ty, self.emit_place(at, callee))
            }
            Ok(None) => self.expr(callee, None),
            Err(()) => {
                let callee = self.cause(callee);
                let call = self.span(n);
                if let Some(r) = self
                    .out
                    .refusals
                    .get_mut(before..)
                    .and_then(|new| new.last_mut())
                    && r.primary == callee
                    && let Some(RefusalDetail::OpaqueField {
                        receiver_type,
                        receiver,
                        member,
                    }) = r.detail.as_deref()
                    && member.end == callee.span.end
                {
                    r.detail = Some(Box::new(RefusalDetail::OpaqueMethod {
                        receiver_type: receiver_type.clone(),
                        receiver: *receiver,
                        member: *member,
                        call,
                        arity: args.len(),
                    }));
                }
                return (ERROR, None);
            }
        };
        let mut written = callee;
        while self.kind(written) == NodeKind::Group {
            written = self.kids(written)[0];
        }
        if !matches!(
            self.kind(written),
            NodeKind::Closure | NodeKind::ScopedClosure
        ) && value.is_none_or(|v| !self.flow_literals.contains(&v))
        {
            self.flow_callable(n);
        }
        // Views carry the referent's bytes (FORM #56), including a callable's
        // code/environment pair; invoking one borrows the same callable.
        let ty = self.bare(ty);
        let Ok(entry) = self.pack_callee(ty, dispatch, callee) else {
            return (ERROR, None);
        };
        let ty = entry.unwrap_or(ty);
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
        self.diagnostic_callee = Some(calls::DiagnosticCallee::Value(ty));
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
        if !self.mapper_mode(n, scratch.params.first().map(|q| q.0)) {
            return (ERROR, None);
        }
        if self.pack_waits_call_refused(waits, entry, dispatch, n) {
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
        self.step_callback(ty, value, n);
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
            let arg = self.step_argument(arg, mode);
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
            let mode = if bytes || mode == Mode::Steps {
                Mode::View
            } else {
                mode
            };
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
        if let Some(entry) = entry {
            return self.finish_pack_call(value, entry, dispatch, result, n, scratch);
        }
        let (code, environment) = if callable == Callable::Plain {
            (value, self.konst(Ty::Ptr, 0, n))
        } else {
            (
                self.callable_word(value, 0, n),
                self.callable_word(value, 8, n),
            )
        };
        let v = self.emit_value_call(
            code,
            environment,
            abi,
            result,
            n,
            scratch,
            CallFailure::Cleanup,
            Entry::Step,
        );
        if result == NEVER {
            self.dead = true;
        }
        (result, v)
    }

    /// [`Self::call_lowered`] whose callee steps on entry only for `Entry::Step`.
    pub(super) fn call_lowered_entry(
        &mut self,
        ty: TyId,
        value: ValueId,
        vals: &[Option<ValueId>],
        n: NodeId,
        entry: Entry,
    ) -> Option<ValueId> {
        self.call_lowered_with_fail(ty, value, vals, n, CallFailure::Cleanup, entry)
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
        self.call_lowered_with_fail(ty, value, &[], n, CallFailure::Handler(fail), Entry::Step)
    }

    pub(super) fn call_lowered_with_fail(
        &mut self,
        ty: TyId,
        value: ValueId,
        vals: &[Option<ValueId>],
        n: NodeId,
        failure: CallFailure,
        entry: Entry,
    ) -> Option<ValueId> {
        if !self.generated && !self.flow_literals.contains(&value) {
            self.flow_callable(n);
        }
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
        let v = self.emit_value_call(
            code,
            environment,
            abi,
            result,
            n,
            &mut scratch,
            failure,
            entry,
        );
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

    #[allow(clippy::too_many_arguments)]
    fn emit_value_call(
        &mut self,
        code: Option<ValueId>,
        environment: Option<ValueId>,
        abi: Option<CallableAbi>,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
        failure: CallFailure,
        entry: Entry,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let code = code?;
        let abi = abi?;
        if entry == Entry::Step {
            let step_fail = match failure {
                CallFailure::Cleanup => self.fail_to(n),
                CallFailure::Handler(block) => Some(block),
            };
            self.callback_step_to(n, step_fail);
        }
        let signature = abi.signature;
        let dest = self.e.module.signatures[signature.index()]
            .dest
            .map(|id| self.result_slot(PlaceTy::Declared(id), n));
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
        let fail = match failure {
            CallFailure::Cleanup => self.fail_to(n),
            CallFailure::Handler(block) => Some(block),
        };
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::Indirect(code),
            signature,
            args,
            results,
            fail,
            cold: false,
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
    /// Return the semantic Region root for a slot that may cross into a
    /// callback activation.  Resolver slots are storage identities, so a
    /// captured formal would otherwise be mistaken for a local Region root.
    fn region_root_for_capture(&self, slot: usize) -> Option<crate::types::Root> {
        let var = self.vars.get(slot).and_then(Option::as_ref)?;
        if !self.region_handle(var.ty) {
            return None;
        }
        if let Some(root) = self
            .capture_frame
            .as_ref()
            .and_then(|frame| frame.region_roots.get(slot).copied().flatten())
        {
            return Some(root);
        }
        self.fd
            .params
            .iter()
            .enumerate()
            .find_map(|(index, parameter)| {
                (self
                    .binder_at
                    .get(&(parameter.site.span.start, parameter.site.span.end))
                    == Some(&(slot as u32)))
                .then_some(crate::types::Root::Parameter(index as u32))
            })
            .or(Some(crate::types::Root::Local(slot as u32)))
    }

    /// The forward visit has already substituted arguments that are fixed.
    /// A caller's own type parameter is a type, while an unset argument is a hole.
    fn infer_closure_params(
        &mut self,
        closure: NodeId,
        params: &[NodeId],
        expected: Option<TyId>,
    ) -> Result<Option<TyId>, ()> {
        let Some(&first) = params.iter().find(|&&param| self.kids(param).is_empty()) else {
            return Ok(None);
        };
        if params.iter().any(|&param| !self.kids(param).is_empty()) {
            self.refuse(
                "LANG-11",
                first,
                "write every closure parameter type or omit them all",
            );
            return Err(());
        }
        let Some((ty, expected_params)) = expected.and_then(|ty| match self.t.kind(ty) {
            TyKind::Fn(params, ..) => Some((ty, params)),
            _ => None,
        }) else {
            self.refuse(
                "LANG-11",
                first,
                "omitted closure parameter types need an expected callable type",
            );
            return Err(());
        };
        if expected_params.iter().any(|&(_, ty)| has_hole(&self.t, ty)) {
            self.refuse(
                "LANG-11",
                first,
                "the expected callable still has an open parameter type",
            );
            return Err(());
        }
        if expected_params.len() != params.len() {
            let count = |n| crate::decls::counted(n, "parameter");
            self.refuse_compared(
                "LANG-13",
                closure,
                count(params.len()),
                count(expected_params.len()),
                "the closure's parameter count does not fit the expected callable type",
            );
            return Err(());
        }
        Ok(Some(ty))
    }

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
        fd.path.0.name = self.generated_name(GeneratedKind::Closure, n);
        fd.site = self.p.site(self.at, n);
        let expected = want
            .filter(|w| w.payload == 0)
            .map(|w| w.ty)
            .and_then(|ty| {
                if matches!(self.t.kind(ty), TyKind::Fn(..)) {
                    Some(ty)
                } else {
                    let (_, ok, error) = self.result_parts(ty)?;
                    (error.is_some()
                        && matches!(
                            self.t.kind(ok),
                            TyKind::Fn(_, _, _, Callable::Own | Callable::OwnSend)
                        ))
                    .then_some(ok)
                }
            });
        let expected_kind = expected.and_then(|ty| match self.t.kind(ty) {
            TyKind::Fn(_, _, _, k) => Some(*k),
            _ => None,
        });
        let params = self.kids(self.kids(n)[0]);
        let Ok(inferred) = self.infer_closure_params(n, params, expected) else {
            return (ERROR, None);
        };
        self.closure_header_params.clear();
        let mut written_result = false;
        let mut callable_result = UNIT;
        for &child in self.kids(n) {
            match self.kind(child) {
                NodeKind::Parameters => {
                    for (i, &param) in params.iter().enumerate() {
                        let token = self.item.tree.node(param).token.unwrap();
                        let mode = crate::decls::callable_mode_of(self.kind(param));
                        let (ty_node, ty) = if let Some(inferred) = inferred {
                            let TyKind::Fn(types, ..) = self.t.kind(inferred) else {
                                unreachable!("parameter types come from a callable");
                            };
                            let (expected_mode, ty) = types[i];
                            if mode != expected_mode {
                                self.refuse_compared(
                                    "LANG-13", n, mode.keyword(), expected_mode.keyword(),
                                    "the closure's parameter mode does not fit the expected callable type",
                                );
                                self.closure_header_params.clear();
                                return (ERROR, None);
                            }
                            (param, ty)
                        } else {
                            let ty_node = self.kids(param)[0];
                            (ty_node, self.ty(ty_node))
                        };
                        let ty = self.outside_vector(ty_node, ty, "part of a callable type");
                        if mode == Mode::Bytes && ty != scalar(Scalar::U64) && ty != ERROR {
                            self.refuse("LANG-5", param, "a `bytes` parameter has type `u64`");
                        }
                        if let Some(slot) = self.binder(param) {
                            self.closure_header_params.push((slot, ty));
                        }
                        fd.params.push(crate::decls::ParamDecl {
                            bytes: mode == Mode::Bytes,
                            name: self.item.symbol(token),
                            mode: if mode == Mode::Bytes {
                                Mode::View
                            } else {
                                mode
                            },
                            ty,
                            site: self.p.site(self.at, param),
                        });
                    }
                }
                NodeKind::ResultType => {
                    written_result = true;
                    let node = self.kids(child)[0];
                    callable_result = self.ty_result(node, true);
                    callable_result =
                        self.outside_vector(node, callable_result, "part of a callable type");
                    if expected_kind == Some(Callable::ScopedView)
                        && crate::decls::view_result_reserved(
                            &self.p.decls,
                            &self.t,
                            callable_result,
                        )
                    {
                        self.refuse("LANG-13", node, crate::decls::VIEW_RESULT_RESERVED);
                        return (ERROR, None);
                    }
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
        if !written_result
            && let Some(ty) = expected
            && let TyKind::Fn(_, result, _, _) = self.t.kind(ty)
            && !has_hole(&self.t, *result)
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
        let scoped = self.kind(n) == NodeKind::ScopedClosure
            || matches!(expected_kind, Some(Callable::Scoped | Callable::ScopedView));
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
                expected_kind
                    .filter(|k| matches!(k, Callable::Scoped | Callable::ScopedView))
                    .unwrap_or(Callable::Scoped)
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
        // Preserve the semantic identity of Region roots as a callback crosses
        // an activation.  A captured Region parameter is still carried in a
        // local slot, but OWN-21 compares its named parameter root with the
        // written anchor list, rather than with that storage place.
        let region_roots: Vec<_> = (0..self.vars.len())
            .map(|slot| self.region_root_for_capture(slot))
            .collect();
        let enclosing_frame = self.capture_frame.as_ref().map(|frame| EnclosingFrame {
            outer: &frame.outer,
            parent: self.enclosing_frame,
        });
        let mut child = Cx::new(self.p, self.definition, self.e);
        child.fd = std::borrow::Cow::Owned(fd);
        child.header = n;
        child.frame_level = self.frame_level + 1;
        child.entry = false;
        child.inherited_refusal = self.inherited_refusal || !self.out.refusals.is_empty();
        child.enclosing_frame = enclosing_frame.as_ref();
        let view_only = expected_kind == Some(Callable::ScopedView);
        child.capture_frame = Some(CaptureFrame {
            view_only,
            outer: self.vars.clone(),
            outer_descriptors: std::mem::take(&mut self.descriptors),
            written_anchors: vec![None; self.vars.len()],
            region_roots,
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
            own_record: None,
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
            child.e.install_source_signature(f, signature);
            (f, signature)
        });
        child.run_row(row, body);
        let own_row = child.close_capture_record(n);
        std::mem::swap(&mut child.t, &mut self.t);
        let mut frame = child.capture_frame.take().unwrap();
        frame.callback_paths = std::mem::take(&mut child.descriptors.paths);
        self.descriptors = std::mem::take(&mut frame.outer_descriptors);
        let out = child.finish();
        let function = out.function;
        self.out.flow.violations.extend(out.flow.violations);
        self.out.flow.stages.extend(out.flow.stages);
        if out.flow.waits {
            self.flow_waits(n);
        }
        if let (Some(function), Some(template)) = (function, out.template) {
            self.nested.insert(function, template);
        }
        let refused = self.out.merge_refusals(out.refusals, out.resource_refusal);
        self.out.gaps.extend(out.gaps);
        self.out.counts += out.counts;
        self.out.computed.extend(out.computed);
        if function.is_none() {
            self.halt();
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
            let value = code.and_then(|code| self.scoped_value(code, &frame, n));
            if !refused
                && expected_kind == Some(Callable::ScopedView)
                && let Some(site) = self.view_descriptor_offender(value)
            {
                self.refuse(
                    "LANG-13",
                    site,
                    "scoped[view] fn captures only views, transitively",
                );
                return (ERROR, None);
            }
            return (ty, value);
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
        let Some(own_row) = own_row else {
            return (ERROR, None);
        };
        self.owning_value(callable, code, &frame, own_row, n)
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

pub(super) struct EnclosingFrame<'e> {
    outer: &'e [Option<Var>],
    parent: Option<&'e EnclosingFrame<'e>>,
}

pub(super) struct CaptureFrame {
    outer: Vec<Option<Var>>,
    outer_descriptors: Descriptors,
    written_anchors: Vec<Option<anchors::Fact>>,
    /// Semantic Region roots for captured slots.  The slot itself is local to
    /// this activation, while a captured formal keeps its parent's parameter
    /// root for anchor comparison.
    region_roots: Vec<Option<crate::types::Root>>,
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
    view_only: bool,
    own_record: Option<AddressId>,
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
    fn empty(&mut self, site: NodeId) -> u32 {
        let id = self.ranges.len() as u32;
        self.ranges.push(PoolRange::EMPTY);
        self.environments
            .push(EnvironmentOrigin { site, local: false });
        self.fields.push(PoolRange::EMPTY);
        id
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
            return Some(self.empty(site));
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
            let kind = self.kind(n);
            if is_type_node(kind) {
                continue;
            }
            stack.extend(self.kids(n).iter().copied());
            if kind != NodeKind::Path {
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
        let slot = slot as usize;
        let frame = self.capture_frame.as_ref()?;
        frame.outer[slot]
            .or_else(|| {
                std::iter::successors(self.enclosing_frame, |frame| frame.parent)
                    .find_map(|frame| frame.outer[slot])
            })
            .map(|var| var.ty)
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
        if crate::foreign::holds_raw_handle(&self.p.decls, &self.t, original.ty) {
            self.refuse("FFI-6", n, "a raw handle cannot be captured");
            self.vars[slot as usize] = Some(Var {
                ty: ERROR,
                lent: None,
                ..original
            });
            return;
        }
        if self.vector_shape(original.ty).is_some() {
            self.refuse("SEM-11", n, "a vector cannot be captured");
            self.vars[slot as usize] = Some(Var {
                ty: ERROR,
                lent: None,
                ..original
            });
            return;
        }
        if matches!(self.t.kind(original.ty),TyKind::Adt(d,_) if self.p.decls[d.index()].role==Some(Role::Writer))
        {
            self.refuse("MEM-4", n, "a Writer is never captured");
        } else if owning {
            if self.capture_holds(original.ty)
                & (crate::H_RESOURCE | crate::H_CAPABILITY | crate::H_SCOPED | crate::H_REGION)
                != 0
            {
                self.refuse("LANG-14",n,"an owning closure capture holds a resource, capability, scoped value or region reference");
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
        let send = has_bounds(&self.p.decls, &self.t, original.ty, &self.params) & crate::SEND != 0;
        let frame = self.capture_frame.as_mut().unwrap();
        let offset = if owning {
            0
        } else {
            let offset = frame.words as u64 * 8;
            frame.words += 1;
            offset
        };
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
        // The binding is made once, at the first source use, and every later use of the
        // capture reuses it, in whatever block it is reached from. So it is computed in
        // the entry block, which dominates them all, never at that first use.
        let binding = if let Some(environment) = environment {
            let place = self.rep_at(original.ty, n).and_then(Rep::stored);
            let address = if owning {
                place.map(|place| {
                    let base = match self.capture_frame.as_ref().unwrap().own_record {
                        Some(base) => base,
                        None => {
                            let prefix = self.entry_const(Ty::Scalar(Scalar::U64), 8);
                            let base = self.entry_address(
                                AddressBase::Value(environment),
                                &[Step::Byte(prefix)],
                                PlaceTy::Value(Ty::Ptr),
                            );
                            self.capture_frame.as_mut().unwrap().own_record = Some(base);
                            base
                        }
                    };
                    self.entry_place(
                        AddressBase::Address(base),
                        &[Step::Field(index as u32)],
                        place,
                    )
                })
            } else {
                let offset = self.entry_const(Ty::Scalar(Scalar::U64), offset);
                let address = self.entry_address(
                    AddressBase::Value(environment),
                    &[Step::Byte(offset)],
                    PlaceTy::Value(Ty::Ptr),
                );
                let pointer = self.value(Ty::Ptr);
                self.entry_setup.push(Inst::Read {
                    result: pointer,
                    address,
                    mode: ReadMode::Bits,
                });
                place.map(|ty| self.entry_address(AddressBase::Value(pointer), &[], ty))
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
            // A capture is its callback's own slot; the capture itself held the
            // scrutinee place a `mut` pattern binding names (OWN-15).
            lent: None,
            ..original
        });
        if kind(&self.p.decls, &self.t, original.ty, &self.params) == Kind::Region {
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
                        lent: None,
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
            TyKind::Anchored(x, _) => self.capture_holds(*x) | crate::H_REGION,
            TyKind::Tuple(xs) => xs.iter().fold(0, |h, &x| h | self.capture_holds(x)),
            TyKind::Array(x, _) => self.capture_holds(*x),
            TyKind::Adt(d, args) => {
                let summary = &self.p.decls[d.index()].summary;
                args.iter()
                    .enumerate()
                    .filter(|(i, _)| summary.holds_params & (1 << i) != 0)
                    .fold(summary.holds, |h, (_, &x)| h | self.capture_holds(x))
            }
            TyKind::Fn(_, _, _, Callable::Scoped | Callable::ScopedView) => crate::H_SCOPED,
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
            // An immutable value has no place: each literal that views it gets its own
            // copy, written here, where it dominates that literal. Rebinding the
            // variable to this copy would let a literal in a sibling arm read a place
            // its path never wrote.
            (Binding::Value(value), _) => {
                let address = self.slot(var.ty, value, n)?;
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
            let place = self.stored_at(ty, "an environment of no words has no slot", n)?;
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
            let value = self.captureless_callable(code, n);
            self.guarantee_view_descriptor(value, n);
            return value;
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
        if self.descriptors.slots.is_empty()
            && !self.has_callback_parameters
            && self.capture_frame.is_none()
        {
            return;
        }
        if self.capture_frame.as_ref().is_some_and(|f| f.view_only) {
            let var = self.var(at.place.slot);
            let origin = self
                .capture_frame
                .as_ref()
                .and_then(|f| f.outer.get(at.place.slot as usize).copied().flatten())
                .map_or(var.origin, |v| v.origin);
            if matches!(
                origin,
                Origin::ViewParam | Origin::MutParam | Origin::TakeParam
            ) && self.contains_callback_kind::<true>(at.ty)
            {
                self.refuse(
                    "LANG-13",
                    n,
                    "scoped[view] fn cannot use a received unqualified scoped callback",
                );
                return;
            }
        }
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
        if id.is_none()
            && self.has_callback_parameters
            && matches!(
                self.var(at.place.slot).origin,
                Origin::ViewParam | Origin::MutParam | Origin::TakeParam
            )
            && self.contains_callback(at.ty)
        {
            id = Some(self.descriptors.reference(at.place.slot, 0, n));
        }
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
                    self.step_write(c.slot, n);
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
                vec![Cause::declared(cause)],
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
        self.contains_callback_kind::<false>(ty)
    }
    pub(super) fn contains_callback_kind<const UNKNOWN: bool>(&self, ty: TyId) -> bool {
        match self.t.kind(ty) {
            TyKind::Fn(_, _, _, Callable::Scoped) => true,
            TyKind::Fn(_, _, _, Callable::ScopedView) => !UNKNOWN,
            TyKind::Tuple(xs) => xs
                .iter()
                .any(|&x| self.contains_callback_kind::<UNKNOWN>(x)),
            TyKind::View(x) | TyKind::Anchored(x, _) => self.contains_callback_kind::<UNKNOWN>(*x),
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                if decl.structural {
                    return args
                        .iter()
                        .any(|&x| self.contains_callback_kind::<UNKNOWN>(x));
                }
                if decl.repr != Repr::Scoped || decl.summary.cycle {
                    return false;
                }
                match &decl.body {
                    DeclBody::Record(fs) => fs
                        .iter()
                        .any(|f| self.contains_callback_kind::<UNKNOWN>(f.ty)),
                    DeclBody::Variant(cs) => cs.iter().any(|c| {
                        c.fields
                            .iter()
                            .any(|f| self.contains_callback_kind::<UNKNOWN>(f.ty))
                    }),
                    DeclBody::Alias(x) => self.contains_callback_kind::<UNKNOWN>(*x),
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
                        vec![Cause::declared(cause)],
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
                        vec![Cause::declared(cause)],
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
                    vec![Cause::declared(cause)],
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
                vec![Cause::declared(cause)],
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
    /// The closure body discovers its captures in first-use order. Fill in the
    /// type of its environment record once that list is complete.
    fn close_capture_record(&mut self, n: NodeId) -> Option<TypeId> {
        let frame = self.capture_frame.as_ref()?;
        if !frame.owning || frame.captures.is_empty() {
            return None;
        }
        let base = frame.own_record;
        let types = frame
            .captures
            .iter()
            .map(|capture| frame.outer[capture.slot as usize].unwrap().ty)
            .collect();
        let record = self.t.mk(TyKind::Tuple(types));
        let row = self.place_row(
            record,
            "a closure environment without a place is not built",
            n,
        )?;
        if let Some(base) = base {
            self.b.addresses[base.index()].ty = PlaceTy::Declared(row);
        }
        Some(row)
    }
    fn owning_value(
        &mut self,
        callable: TyId,
        code: Option<ValueId>,
        frame: &CaptureFrame,
        row: TypeId,
        n: NodeId,
    ) -> (TyId, Option<ValueId>) {
        let result = callable;
        let types = frame
            .captures
            .iter()
            .map(|c| self.var(c.slot).ty)
            .collect::<Vec<_>>();
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
        let recipe = &self.e.recipes[row.index()];
        let logical = recipe.logical_size.expect("a fixed row's logical size");
        let Some(charge) = hirsel_core_layout::charge::closure_env(logical) else {
            self.refuse("OWN-2", n, "closure charge exceeds u64");
            return (ERROR, None);
        };
        if !self.live() {
            return (result, None);
        }
        let code = code.unwrap();
        let id = match self.rep(callable) {
            Ok(Rep::Place(id)) => id,
            Ok(_) => unreachable!("an owning callable has a carrier"),
            Err(what) => {
                self.gap(n, what);
                return (result, None);
            }
        };
        let allocation = admission::OwnedAllocation {
            row,
            prefix: 8,
            kind: AllocKind::Environment {
                ty: hirsel_core_layout::TypeRef(row.0),
            },
            charge,
            align: 8,
        };
        let to = self.result_slot(PlaceTy::Declared(id), n);
        let Some(block) = self.admit_owned(allocation, n) else {
            return (result, None);
        };
        let prefix = self.konst(Ty::Scalar(Scalar::U64), 8, n).unwrap();
        let fields = self.address(
            AddressBase::Value(block),
            &[Step::Byte(prefix)],
            PlaceTy::Declared(row),
            n,
        );
        for (i, rep) in reps.iter().enumerate() {
            if let Some(place) = rep.stored() {
                let at = self.address(
                    AddressBase::Address(fields),
                    &[Step::Field(i as u32)],
                    place,
                    n,
                );
                self.install(at, reps[i], values[i], n);
            }
        }
        let carrier = self.pointer(to, n);
        self.store_word(carrier, 0, code, n);
        self.store_word(carrier, 8, block, n);
        self.temp(result, Some(carrier));
        let value = Some(carrier);
        (result, value)
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
                let root = self
                    .capture_frame
                    .as_ref()
                    .and_then(|frame| frame.region_roots.get(slot as usize).copied().flatten())
                    .unwrap_or(crate::types::Root::Local(slot));
                self.anchors.named_region_root(root)
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
