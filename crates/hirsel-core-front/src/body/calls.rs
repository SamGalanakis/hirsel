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
    pub(super) ir_args: Vec<Arg>,
}

fn fill_call_type(
    t: &mut impl TyRead,
    ty: TyId,
    found: &[Option<TyId>],
    roots: &[Option<TyId>],
) -> TyId {
    if roots.is_empty() && !has_param(t, ty) {
        return ty;
    }
    match t.kind(ty).clone() {
        TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => found[i as usize].unwrap_or(HOLE),
        TyKind::Region(Root::Parameter(i)) => {
            roots.get(i as usize).copied().flatten().unwrap_or(ty)
        }
        TyKind::Anchored(x, root) => {
            let x = fill_call_type(t, x, found, roots);
            let root = fill_call_type(t, root, found, roots);
            t.mk(TyKind::Anchored(x, root))
        }
        TyKind::Array(x, n) => {
            let x = fill_call_type(t, x, found, roots);
            t.mk(TyKind::Array(x, n))
        }
        TyKind::Tuple(xs) => {
            let xs = xs
                .into_iter()
                .map(|x| fill_call_type(t, x, found, roots))
                .collect();
            t.mk(TyKind::Tuple(xs))
        }
        TyKind::Adt(d, xs) => {
            let xs = xs
                .into_iter()
                .map(|x| fill_call_type(t, x, found, roots))
                .collect();
            t.mk(TyKind::Adt(d, xs))
        }
        TyKind::View(x) => {
            let x = fill_call_type(t, x, found, roots);
            t.mk(TyKind::View(x))
        }
        TyKind::Fn(ps, r, w, k) => {
            let ps = ps
                .into_iter()
                .map(|(m, p)| (m, fill_call_type(t, p, found, roots)))
                .collect();
            let r = fill_call_type(t, r, found, roots);
            t.mk(TyKind::Fn(ps, r, w, k))
        }
        _ => ty,
    }
}

fn slot_push_kind(path: &DefPath) -> Option<slots::SlotPush> {
    if path.is_core("deque.push_back") {
        Some(slots::SlotPush::Back)
    } else if path.is_core("deque.push_front") {
        Some(slots::SlotPush::Front)
    } else if path.is_core("heap.push") {
        Some(slots::SlotPush::Heap)
    } else {
        None
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// These shapes give an inner closure its own callable expectation even
    /// when the enclosing argument still contains a hole elsewhere.
    pub(super) fn structured_argument_type(&self, ty: TyId) -> bool {
        matches!(
            self.t.kind(ty),
            TyKind::Fn(..) | TyKind::Tuple(_) | TyKind::Array(..)
        )
    }

    /// LANG-11 step 3 extends the existing literal-operand deferral only to
    /// tuples and arrays whose leaves are all unsuffixed numeric literals.
    /// Mixed aggregates and constructor calls still infer left to right.
    fn defer_call_argument(&mut self, n: NodeId) -> bool {
        match self.kind(n) {
            NodeKind::Tuple | NodeKind::Array => self.literal_call_aggregate(n),
            NodeKind::Group => self.defer_call_argument(self.kids(n)[0]),
            _ => !self.synthesises(n),
        }
    }

    fn literal_call_aggregate(&self, n: NodeId) -> bool {
        match self.kind(n) {
            NodeKind::Literal => self.unsuffixed_number(n),
            NodeKind::Tuple | NodeKind::Array | NodeKind::Group => self
                .kids(n)
                .iter()
                .all(|&child| self.literal_call_aggregate(child)),
            _ => false,
        }
    }

    pub(super) fn step_argument(&mut self, arg: NodeId, mode: Mode) -> NodeId {
        if self.kind(arg) == NodeKind::StepsArgument {
            if mode != Mode::Steps {
                self.refuse_compared(
                    "OWN-7",
                    arg,
                    "steps",
                    mode.keyword(),
                    "this parameter takes no `steps` mark",
                );
            }
            self.kids(arg)[0]
        } else {
            if mode == Mode::Steps {
                self.refuse_compared(
                    "OWN-7",
                    arg,
                    self.written_mode(arg),
                    "steps",
                    "this parameter needs a `steps` argument",
                );
            }
            arg
        }
    }

    // ---- calls
    pub(super) fn call(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        let c = self.kids(n);
        if self.writer_call(c[0]) && !self.writer_placed(n) {
            self.refuse(
                "MEM-4",
                n,
                "a writer call's Result is the operand of a `?` or the returned tail",
            );
        }
        let result = self.call_inner(n, c[0], &c[1..], want, &mut scratch);
        self.call_scratch.push(scratch);
        result
    }

    /// LANG-18: apply a `?=>` path mapper `m` to the error payload `value` of type
    /// `error`, as the call `m(take x)` checked against `expect(mapped)`.
    pub(super) fn apply_mapper(
        &mut self,
        n: NodeId,
        mapper: NodeId,
        error: TyId,
        value: Option<ValueId>,
        mapped: TyId,
    ) -> (TyId, Option<ValueId>) {
        let destination = self.destination.take();
        self.mapped_error = Some((n, error, value));
        let mut scratch = self.call_scratch.pop().unwrap_or_default();
        let result = self.call_inner(n, mapper, &[n], expect(mapped), &mut scratch);
        self.call_scratch.push(scratch);
        // A refused mapper leaves its argument unread; it is not owned here.
        self.mapped_error = None;
        self.destination = destination;
        result
    }

    /// LANG-18: a mapper takes its one argument, the error payload, by `take`.
    pub(super) fn mapper_mode(&mut self, n: NodeId, mode: Option<Mode>) -> bool {
        if !self.mapped_error.is_some_and(|(node, ..)| node == n) {
            return true;
        }
        if let Some(mode) = mode
            && mode != Mode::Take
        {
            let message = "a `?=>` mapper takes the error payload by `take`";
            self.refuse_compared("LANG-18", n, mode.keyword(), "take", message);
            return false;
        }
        true
    }

    pub(super) fn call_target(&self, callee: NodeId) -> Option<&'c Target> {
        if self.kind(callee) != NodeKind::Path {
            return None;
        }
        let resolved = self.p.resolved(self.at, callee)?;
        if matches!(resolved.target, Target::Definition(_))
            && resolved.projections.start != resolved.projections.end
        {
            return None;
        }
        Some(&resolved.target)
    }

    fn lend_direct_argument(&self, call: u32, at: At) -> bool {
        let Some((active, destination)) = self.direct_call else {
            return false;
        };
        if active != call {
            return false;
        }
        let Some(destination) = destination else {
            return true;
        };
        let Binding::Slot(Some(argument)) = self.var(at.place.slot).binding else {
            return false;
        };
        at.place.len == 0
            && matches!(self.b.addresses[argument.index()].base,
                AddressBase::Slot(slot) if slot != destination)
    }

    fn call_inner(
        &mut self,
        n: NodeId,
        callee: NodeId,
        args: &[NodeId],
        want: Option<Want>,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        self.out.counts.calls += 1;
        self.out.counts.uses += 1;
        let p: &'c Program<'a> = self.p;
        let f = match self.call_target(callee) {
            Some(Target::Definition(id)) => match p.by_definition[id.0 as usize].as_ref() {
                Some(Def::Fn(f)) => *f as usize,
                Some(Def::Type(d)) => return self.construct_record(n, *d, want),
                // A constant is a value, so use the ordinary callable type check.
                None => return self.call_value(n, callee, args, want, scratch),
            },
            Some(Target::Missing) => {
                self.halt();
                return (ERROR, None);
            }
            Some(&Target::ContextualConstructor { name }) => {
                return self.construct_variant(n, name, args, want);
            }
            _ => return self.call_value(n, callee, args, want, scratch),
        };
        let g = &p.fns[f];
        // MEM-4: `emit(take w, x)` writes the literal `x` in place; it is a form.
        if self.is_emit(f) {
            self.builtin_use(f, n);
            return self.emit_form(n, f, args, want);
        }
        if self.is_put(f) {
            self.builtin_use(f, n);
            return self.put_form(n, f, args, want);
        }
        if g.body.is_none()
            && let Some(op) = core_ops::simd_call(p.core_operations[f])
        {
            return self.simd_call(op, f, n, args, want);
        }
        if g.path.0.name == "callable" && g.path.is_core("code.callable") {
            self.flow_callable(n);
        }
        if g.result == ERROR {
            // The callee's refused header owns the diagnostic. Its error result
            // cannot supply pattern payloads or a value for live emission.
            self.inherited_refusal = true;
            self.halt();
        }
        // PROG-9 is decided at the argument even when a generic function item
        // cannot otherwise be materialized without an expected callable type.
        // Keep this refusal ahead of function-value inference so the diagnostic
        // names the offending generic item rather than cascading LANG-12.
        if g.path.0.name == "describe"
            && g.path.is_core("code.describe")
            && let Some(argument) = args.first()
            && let Some(Target::Definition(id)) = p.target(self.at, *argument)
            && let Some(Def::Fn(item)) = p.by_definition[id.0 as usize]
            && p.fns[item as usize]
                .generics
                .iter()
                .any(|generic| !generic.region)
        {
            self.refuse(
                "PROG-9",
                *argument,
                "code.describe requires a top-level non-generic function name",
            );
            return (ERROR, None);
        }
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
        if rooted.iter().any(|&i| {
            args.get(i)
                .is_none_or(|&e| self.region_argument_path(e).is_none())
        }) {
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
            let message = format!("expected {k} arguments");
            self.refuse_arity_at(self.close_brace(n), args.len(), k, message);
            return (ERROR, None);
        }
        if args.len() > g.params.len() {
            let k = g.params.len();
            let message = format!("expected {k} arguments");
            self.refuse_arity_at(self.span(args[k]), args.len(), k, message);
            return (ERROR, None);
        }
        if !self.mapper_mode(n, g.params.first().map(|q| q.mode)) {
            return (ERROR, None);
        }
        if g.waits {
            self.waited = true;
            if !self.fd.waits && !self.entry {
                let message = "calls a `waits` function but is not declared `waits`";
                self.refuse_compared("LANG-8", n, "not waits", "waits", message);
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
            self.determined(f, found, checked, n, n);
        }
        // With no width from context, retain the original u64 word read.
        if matches!(found.as_slice(), [None])
            && (g.path.is_core("list.word_le") || g.path.is_core("list.word_be"))
        {
            found[0] = Some(scalar(Scalar::U64));
        }
        // A copyable view argument read from a place is passed by that place's
        // address when nothing can write the place before the callee returns:
        // every parameter views, every argument is evaluated without a write,
        // the callee does not wait, and any supplied result is a field of a
        // fresh aggregate. Each lent argument names a different slot from the result.
        // That every argument is plain data is known only once the arguments
        // are typed; otherwise the passed places are snapshotted after the
        // arguments, which no argument wrote.
        let fresh = self.fresh_destination_root(n);
        let direct = g.body.is_some()
            && !g.waits
            && g.params
                .iter()
                .all(|q| matches!(q.mode, Mode::View | Mode::Steps | Mode::Bytes))
            && (self.expression_destination(n).is_none() || fresh.is_some())
            && args.iter().all(|&a| self.writes_nothing(a));
        let outer_call = std::mem::replace(&mut self.direct_call, direct.then_some((call, fresh)));
        let outer_args = std::mem::take(&mut self.direct_args);
        let mut push = None;
        let mut replacement = None;
        let mut slot_push = None;
        let mut map_key_at = None;
        let mut map_insertion = None;
        let prior_refusals = self.out.refusals.len();
        for (i, &a) in args.iter().enumerate() {
            let a_mode = g.params[i].mode;
            let a = self.step_argument(a, a_mode);
            let a_mode = if a_mode == Mode::Steps {
                Mode::View
            } else {
                a_mode
            };
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
                    let mode = g.params[i].mode.keyword();
                    let message = "this parameter takes no `bytes` mark";
                    self.refuse_compared("OWN-7", a, "bytes", mode, message);
                    self.kids(a)[0]
                }
                (_, true) => {
                    let message = "this parameter needs a `bytes` argument";
                    self.refuse_compared("OWN-7", a, self.written_mode(a), "bytes", message);
                    a
                }
                _ => a,
            };
            if self.argument_rooted(
                a_mode,
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
                self.step_callback(g.params[i].ty, vals[i], e);
                self.activate(vals[i], call, e);
                self.determined(f, found, checked, e, n);
                continue;
            }
            // A literal operand waits for the other arguments and the result (LANG-21).
            if self.defer_call_argument(e)
                && !(i == 1
                    && g.path.is_core("map.insert")
                    && self.known_rooted(g.params[i].ty, found, roots).is_some())
            {
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
                    let place = if i == 1
                        && g.path.is_core("list.push")
                        && let Some(header) = vals[0]
                        && self.can_construct(e)
                    {
                        push = self.list_push_destination(t, header, e);
                        push.map(|(_, to)| to)
                    } else if i == 1
                        && let Some(kind) = slot_push_kind(&g.path)
                        && let Some(header) = vals[0]
                        && self.can_construct(e)
                    {
                        slot_push = self.prepare_slot_push(kind, t, header, e);
                        slot_push.map(|prepared| prepared.slot)
                    } else if i == 2
                        && g.path.is_core("list.set")
                        && let (Some(header), Some(index)) = (vals[0], vals[1])
                        && self.can_construct(e)
                    {
                        replacement = self.list_replacement(t, header, index, e);
                        replacement.map(|slot| slot.slot)
                    } else if i != 0
                        && g.path.is_core("map.insert")
                        && let Some(header) = vals[0]
                        && let [Some(k), Some(v)] = found.as_slice()
                    {
                        let types = [*k, *v];
                        if i == 1 && self.can_construct(e) {
                            map_key_at = self.map_key_destination(&types, header, e);
                            map_key_at
                        } else if i == 2 {
                            let construct =
                                self.can_construct(e) && self.nonempty_aggregate(t).is_some();
                            if map_key_at.is_some() || construct {
                                map_insertion =
                                    self.map_insertion(&types, header, vals[1], construct, e);
                            }
                            map_insertion.and_then(|p| p.replacement.map(|r| r.slot))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    self.expr_into(e, Some(w), place).1
                }
                None => {
                    // A call takes the partly known parameter type as its expected type.
                    let (at, mut v) = if let Some(hint) =
                        want.and_then(|w| self.payload_argument(g.result, pt, w))
                    {
                        self.expr_hinted(e, Some(hint))
                    } else if self.structured_argument_type(pt)
                        || matches!(self.kind(e), NodeKind::Closure | NodeKind::ScopedClosure)
                        || self.kind(e) == NodeKind::Path
                            && matches!(self.p.target(self.at, e), Some(Target::Definition(id))
                                if matches!(self.p.by_definition[id.0 as usize], Some(Def::Fn(_))))
                    {
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
                        let before = self.partial_rooted(pt, found, roots);
                        let message = if self.holder_payload(inferred).is_some()
                            || self
                                .result_parts(inferred)
                                .is_some_and(|(_, ok, _)| self.holder_payload(ok).is_some())
                        {
                            "a type parameter is determined twice differently; ascribe the holder or bind it with a written type first"
                        } else {
                            "a type parameter is determined twice differently"
                        };
                        self.refuse_twice(e, inferred, before, message);
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
                        // Inference fixed the target; a type check alone does not
                        // turn an eight-byte code word into its scoped carrier.
                        if self.plain_view(at, inferred) {
                            v = self.read_plain_view(inferred, v, e);
                        }
                        if self.plain_callable_fit(inferred, t) {
                            v = self.fit_plain_callable(v, t, e);
                        }
                    }
                    self.determined(f, found, checked, e, n);
                    v
                }
            };
            if g.anchors.contains(&(i as u32)) {
                let fact = self.anchor_argument(e);
                if fact != 0 {
                    self.anchors.expressions.insert(e, fact);
                }
            }
            self.step_callback(g.params[i].ty, vals[i], e);
            self.activate(vals[i], call, e);
        }
        self.direct_call = outer_call;
        let passed = std::mem::replace(&mut self.direct_args, outer_args);
        if !passed.is_empty() {
            let plain = (0..g.params.len()).all(|i| {
                let t = self.partial_rooted(g.params[i].ty, found, roots);
                t != ERROR && t != HOLE && self.type_kind(t) == Kind::Plain
            });
            if !plain && self.live() {
                for (i, id, e) in passed {
                    vals[i] = vals[i].map(|from| self.snapshot(from, id, e));
                }
            }
        }
        // A refused argument could have fixed the type of a deferred literal.
        let argument_refused = self.out.refusals.len() != prior_refusals;
        for &(i, e) in later.iter() {
            match self.known_rooted(g.params[i].ty, found, roots) {
                Some(t) => vals[i] = self.expr(e, expect(t)).1,
                None if !argument_refused => {
                    self.refuse(
                        "LANG-11",
                        e,
                        "a literal argument needs a fixed parameter type; suffix its numbers",
                    );
                }
                None => {}
            }
        }
        // Region generics are erased from the instance identity and may remain
        // unconstrained when they occur in neither a parameter nor the result.
        // Preserve their generic roots for the signature while refusing only open
        // type generics (LANG-12).
        tys.extend(found.iter().enumerate().map(|(i, s)| {
            s.or_else(|| g.generics[i].region.then(|| self.t.mk(generic_arg(g, i))))
                .unwrap_or(ERROR)
        }));
        // An undetermined type argument is refused once; the call's type is then the
        // refused type, so nothing built from it is refused again. Unfixed region
        // generics are legal PROG-9 headers and are erased by instance formation.
        let undetermined = found
            .iter()
            .enumerate()
            .any(|(i, t)| t.is_none() && !g.generics[i].region);
        if undetermined && !self.broken {
            self.refuse("LANG-12", n, "a type parameter of the callee is still open");
        }
        // The call completes: its arguments' holds end, except an argument the scoped
        // result is anchored to. A view argument stays frozen, and a `mut` argument's
        // loan becomes a freeze in this same step, before any later sibling is
        // checked; both last for the result's extent, whatever variant it holds
        // (OWN-10, OWN-11, OWN-20). A call that fails delivers no result, so no
        // freeze outlives it.
        let anchors = &g.anchors;
        self.unfreeze(holds, |h| match h {
            Hold::CallView(c, i) | Hold::CallMut(c, i) if c == call => {
                anchors.contains(&i).then_some(Hold::View(Extent::Temp))
            }
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
        if g.path.0.name == "stage" && g.path.is_core("flow.stage") {
            self.flow_stage(args[6], vals[6]);
        }
        if !undetermined
            && g.path.package_is("core", "1")
            && g.path.0.modules == ["core"]
            && matches!(g.path.0.name.as_str(), "clone" | "clone_in")
            && let Some(why) = self.clone_refusal(tys[0], tys[1], g.path.0.name == "clone_in")
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
        // A plain or explicitly taken value can skip the ordinary expression
        // arm. A staged key still needs to publish into its vacant entry.
        if map_insertion.is_none()
            && map_key_at.is_some()
            && let Some(header) = vals[0]
        {
            map_insertion = self.map_insertion(&[tys[0], tys[1]], header, vals[1], false, n);
        }
        self.source_step(n);
        let v = if let Some(insertion) = map_insertion {
            self.builtin_use(f, n);
            self.finish_map_insertion(insertion, map_key_at, f, &[tys[0], tys[1]], vals, result, n)
        } else if let Some(replacement) = replacement {
            self.builtin_use(f, n);
            self.finish_list_replacement(tys[0], replacement, vals[2], n)
        } else if let Some(prepared) = slot_push
            && let Some(header) = vals[0]
        {
            self.builtin_use(f, n);
            self.finish_slot_push(prepared, f, tys[0], header, vals[1], result, n)
        } else if let Some((full, to)) = push
            && self.live()
            && let Some(header) = vals[0]
        {
            self.builtin_use(f, n);
            let direct = vals[1].and_then(|v| self.pointer_address(v)) == Some(to);
            self.list_push(tys[0], header, vals[1], n, Some((full, direct)))
        } else if builtin {
            self.intrinsic(f, tys, vals, n)
        } else {
            self.emit_call(f, tys, vals, result, n)
        };
        if self.type_kind(result) == Kind::Scoped {
            if self.contains_callback(result) {
                self.returned_descriptor(v, &g.anchors, vals);
            }
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

    // MEM-1 / OWN-3: a marked Region argument still names the same region.
    // Only root substitution strips the mark; argument checking sees the original
    // expression and enforces its mode, place and loan rules as usual.
    pub(super) fn region_argument_path(&self, mut n: NodeId) -> Option<NodeId> {
        loop {
            match self.kind(n) {
                NodeKind::Group | NodeKind::Ascription => n = self.kids(n)[0],
                NodeKind::Unary if matches!(self.token_text(n), "take" | "mut") => {
                    n = self.kids(n)[0];
                }
                _ => return self.binder_path(n),
            }
        }
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
                && let Some(path) = self.region_argument_path(arg)
            {
                roots.resize(i + 1, None);
                roots[i] = Some(self.body_root(path, true));
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
                    let message = "an argument for a `mut` parameter is `mut p`";
                    self.refuse_compared("OWN-7", e, self.written_mode(e), "mut", message);
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
                        self.use_place(at, Use::Mut(Hold::CallMut(call, i as u32)), e);
                        self.flow_expression(e, at.ty);
                        self.arg_ty_rooted(pt, at.ty, e, found, roots);
                        if self.live()
                            && at.ty.0 > HOLE.0
                            && matches!(
                                self.t.kind(self.bare(at.ty)),
                                TyKind::Fn(
                                    _,
                                    _,
                                    _,
                                    Callable::Plain
                                        | Callable::Own
                                        | Callable::OwnSend
                                        | Callable::ScopedView
                                )
                            )
                            && let Some(expected) = self.known_rooted(pt, found, roots)
                            && (self.plain_callable_fit(at.ty, expected)
                                || matches!(
                                    self.t.kind(self.bare(at.ty)),
                                    TyKind::Fn(
                                        _,
                                        _,
                                        _,
                                        Callable::Own | Callable::OwnSend | Callable::ScopedView
                                    )
                                ) && self.scoped_callable_view(at.ty, expected))
                        {
                            self.refuse(
                                "OWN-8",
                                e,
                                "a callable kind fit cannot borrow a different carrier as mut",
                            );
                            return true;
                        }
                        *val = self.place_ptr(at, e);
                        self.mut_descriptor(at);
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
                let message = "`mut` is written only for a `mut` parameter";
                self.refuse_compared("OWN-7", e, "mut", mode.keyword(), message);
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
                if t.0 > HOLE.0
                    && matches!(
                        self.t.kind(self.bare(t)),
                        TyKind::Fn(_, _, _, Callable::Plain)
                    )
                    && let Some(expected) = self.known_rooted(pt, found, roots)
                    && self.plain_callable_fit(t, expected)
                {
                    *val = self.fit_plain_callable(*val, expected, e);
                }
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
                self.flow_expression(e, ty);
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
                    // A copyable value is copied (OWN-7), unless the call reads
                    // its place directly (`direct_call`).
                    self.use_place(at, Use::Value, e);
                    *val = match self.rep(ty) {
                        Ok(Rep::Place(id))
                            if self.lend_direct_argument(call, at)
                                && mode == Mode::View
                                && !self.is_view(ty)
                                && self.type_kind(ty) == Kind::Plain =>
                        {
                            self.direct_args.push((i, id, e));
                            self.place_ptr(at, e)
                        }
                        _ => self.emit_place(at, e),
                    };
                } else if mode == Mode::Take {
                    let name = self.slot_name(at.place.slot);
                    let message = format!("a moving place is passed as `take {name}`");
                    self.refuse_compared("OWN-7", e, "unmarked", "take", message);
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
                if t.0 > HOLE.0
                    && matches!(
                        self.t.kind(self.bare(t)),
                        TyKind::Fn(_, _, _, Callable::Plain)
                    )
                    && let Some(expected) = self.known_rooted(pt, found, roots)
                    && self.plain_callable_fit(t, expected)
                {
                    *val = self.fit_plain_callable(*val, expected, e);
                }
                true
            }
            _ => false,
        }
    }
    /// Evaluating `n` writes no place: it calls nothing and runs no statement.
    fn writes_nothing(&self, n: NodeId) -> bool {
        match self.kind(n) {
            NodeKind::Path | NodeKind::Literal | NodeKind::Unit => true,
            NodeKind::Unary if matches!(self.token_text(n), "mut" | "take") => false,
            NodeKind::Ascription => self.writes_nothing(self.kids(n)[0]),
            NodeKind::Group
            | NodeKind::Unary
            | NodeKind::Binary
            | NodeKind::Index
            | NodeKind::Projection
            | NodeKind::BytesArgument => self.kids(n).iter().all(|&k| self.writes_nothing(k)),
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
                let view = self.type_kind(t) == Kind::Region;
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
                    let before = self.partial_rooted(pt, found, roots);
                    self.refuse_twice(
                        e,
                        at,
                        before,
                        "a type parameter is determined twice differently",
                    );
                }
            }
        }
    }

    fn call_substitution(
        &self,
        ty: TyId,
        found: &[Option<TyId>],
        roots: &[Option<TyId>],
    ) -> (Option<(TyId, Option<TyId>)>, CallSubstitution) {
        let ([arg], []) = (found, roots) else {
            return (None, CallSubstitution::default());
        };
        let key = (ty, *arg);
        let result = self
            .t
            .call_substitution
            .filter(|(old, _)| *old == key)
            .map(|(_, result)| result)
            .unwrap_or_default();
        (Some(key), result)
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
        if pt <= HOLE || (roots.is_empty() && !has_param(&self.t, pt)) {
            return pt;
        }
        match self.t.kind(pt) {
            TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => {
                return s[*i as usize].unwrap_or(HOLE);
            }
            TyKind::Region(Root::Parameter(i)) => {
                return roots.get(*i as usize).copied().flatten().unwrap_or(pt);
            }
            _ => {}
        }
        let (key, mut memo) = self.call_substitution(pt, s, roots);
        if let Some(result) = memo.partial {
            return result;
        }
        let result = fill_call_type(&mut self.t, pt, s, roots);
        memo.partial = Some(result);
        if let Some(key) = key {
            self.t.call_substitution = Some((key, memo));
        }
        result
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
        if pt <= HOLE || (roots.is_empty() && !has_param(&self.t, pt)) {
            return Some(pt);
        }
        match self.t.kind(pt) {
            TyKind::Param(i) | TyKind::Region(Root::Generic(i)) => return s[*i as usize],
            TyKind::Region(Root::Parameter(i)) => {
                return Some(roots.get(*i as usize).copied().flatten().unwrap_or(pt));
            }
            _ => {}
        }
        fn ready(t: &impl TyRead, ty: TyId, found: &[Option<TyId>]) -> bool {
            if !has_param(t, ty) {
                return true;
            }
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
        let (key, mut memo) = self.call_substitution(pt, s, roots);
        if let Some(known) = memo.known {
            return known;
        }
        // A fully bound call environment already proves readiness. The fill
        // still substitutes the same parameters and lexical roots.
        let known = (s.iter().all(Option::is_some) || ready(&self.t, pt, s)).then(|| {
            *memo
                .partial
                .get_or_insert_with(|| fill_call_type(&mut self.t, pt, s, roots))
        });
        memo.known = Some(known);
        if let Some(key) = key {
            self.t.call_substitution = Some((key, memo));
        }
        known
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
        use_site: NodeId,
    ) {
        let p: &'c Program<'a> = self.p;
        for (i, t) in found.iter().enumerate() {
            let Some(t) = *t else { continue };
            if checked[i] {
                continue;
            }
            checked[i] = true;
            if crate::foreign::holds_raw_handle(&p.decls, &self.t, t) {
                self.refuse(
                    "FFI-6",
                    use_site,
                    "a raw handle cannot be an inferred type argument",
                );
                return;
            }
            if self.outside_vector(n, t, "a type argument") == ERROR && t != ERROR {
                return;
            }
            if !p.fns[f].generics[i].region && self.type_kind(t) == Kind::Scoped {
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
                    let a = self.shown(t);
                    let message = "a type parameter cannot be a scoped type";
                    self.refuse_compared("OWN-19", n, a, "not scoped", message);
                    return;
                }
            }
            let g = &p.fns[f].generics[i];
            let missing =
                crate::decls::missing_bounds(&p.decls, &self.t, t, &self.params, g.bounds);
            let path = &p.fns[f].path;
            if missing == 0
                && path.is_core("bytebuf.append_inline")
                && self.inline_byte_count(t).is_none()
            {
                self.refuse("LANG-11", n, "append_inline requires Inline[u8, N]");
                return;
            }
            if missing == 0
                && path.is_core("inline.copy_from_slice")
                && !matches!(self.t.kind(t), TyKind::Array(..))
            {
                self.refuse("LANG-11", n, "inline.copy_from_slice requires Inline[T, N]");
                return;
            }
            if missing != 0 {
                let (a, b) = (self.shown(t), crate::decls::bound_names(missing));
                let transfer = p.fns[f].path.package_is("core", "1")
                    && matches!(p.fns[f].path.0.modules.as_slice(), [module] if module == "task" || module == "channel");
                let rule = if transfer && missing & crate::decls::SEND != 0 {
                    "CONC-2"
                } else {
                    "LANG-12"
                };
                let message = format!("`{a}` is not {b}");
                self.refuse_compared(rule, n, a, b, message);
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
        // Equal closed types bind no generic or lexical root. A generic pattern
        // still has to populate `s`, even when its numeric ID equals the operand.
        if pt == at && roots.is_empty() && !has_param(&self.t, pt) {
            return true;
        }
        let mut comparisons = 0;
        let equal = self.unify_part(pt, at, s, roots, &mut comparisons);
        self.out.counts.types += comparisons;
        equal
    }
    fn unify_part(
        &self,
        pt: TyId,
        at: TyId,
        s: &mut [Option<TyId>],
        roots: &[Option<TyId>],
        comparisons: &mut u64,
    ) -> bool {
        *comparisons += 1;
        if at == ERROR || at == HOLE {
            return true;
        }
        match (self.t.kind(pt), self.t.kind(at)) {
            (TyKind::Region(Root::Parameter(i)), _)
                if let Some(root) = roots.get(*i as usize).copied().flatten() =>
            {
                root == at
            }
            // A partly known type fixes nothing whole (LANG-11).
            (TyKind::Param(_) | TyKind::Region(Root::Generic(_)), _) if has_hole(&self.t, at) => {
                true
            }
            (TyKind::Param(i), _) | (TyKind::Region(Root::Generic(i)), TyKind::Region(_)) => {
                match s[*i as usize] {
                    None => {
                        s[*i as usize] = Some(at);
                        true
                    }
                    Some(x) => x == at || x == ERROR,
                }
            }
            (TyKind::Tuple(a), TyKind::Tuple(b)) if a.len() == b.len() => a
                .iter()
                .zip(b)
                .all(|(&x, &y)| self.unify_part(x, y, s, roots, comparisons)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e && a.len() == b.len() => a
                .iter()
                .zip(b)
                .all(|(&x, &y)| self.unify_part(x, y, s, roots, comparisons)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"])
                    && a.len() == b.len() =>
            {
                a.iter()
                    .zip(b)
                    .all(|(&x, &y)| self.unify_part(x, y, s, roots, comparisons))
            }
            (TyKind::Array(a, n), TyKind::Array(b, m)) if n == m => {
                self.unify_part(*a, *b, s, roots, comparisons)
            }
            (TyKind::View(a), TyKind::View(b)) => self.unify_part(*a, *b, s, roots, comparisons),
            (TyKind::Anchored(a, ar), TyKind::Anchored(b, br)) => {
                self.unify_part(*a, *b, s, roots, comparisons)
                    && self.unify_part(*ar, *br, s, roots, comparisons)
            }
            (_, TyKind::Anchored(b, _))
                if crate::decls::kind(&self.p.decls, &self.t, *b, &self.params) == Kind::Region =>
            {
                self.unify_part(pt, *b, s, roots, comparisons)
            }
            (TyKind::Fn(a, ar, aw, ak), TyKind::Fn(b, br, bw, bk))
                if (ak == bk
                    || *ak == Callable::Scoped && *bk == Callable::ScopedView
                    || *bk == Callable::Plain
                        && matches!(
                            ak,
                            Callable::Own
                                | Callable::OwnSend
                                | Callable::Scoped
                                | Callable::ScopedView
                        ))
                    && a.len() == b.len()
                    && (!*bw || *aw) =>
            {
                a.iter().zip(b).all(|(&(am, at), &(bm, bt))| {
                    am == bm && self.unify_part(at, bt, s, roots, comparisons)
                }) && self.unify_part(*ar, *br, s, roots, comparisons)
            }
            _ => pt == at || pt == ERROR || pt == HOLE,
        }
    }

    pub(super) fn instance_row(
        &mut self,
        f: usize,
        types: &[TyId],
    ) -> Result<(FuncId, SigId), &'static str> {
        use std::hash::BuildHasher;
        if types.is_empty() {
            return self.e.row(self.p, &mut self.t, f);
        }
        let hash = rustc_hash::FxBuildHasher.hash_one((f, types));
        if let Some(instance) = self.instance_rows.find(hash, |instance| {
            instance.definition == f && instance.arguments.get(&self.instance_args) == Some(types)
        }) {
            return Ok(instance.row);
        }
        let row = self.e.instance(self.p, &mut self.t, f, types)?;
        let arguments = PoolRange::push(&mut self.instance_args, types.iter().copied());
        self.instance_rows.insert_unique(
            hash,
            BodyInstance {
                definition: f,
                arguments,
                hash,
                row,
            },
            |instance| instance.hash,
        );
        Ok(row)
    }

    /// A call of `std.codec.of` (SEM-10, EXPANSION 9.1) whose T is not a parameter: a
    /// call of T's codec maker, std's for a scalar, String, List, Map, Option or Result. At a type parameter it is None, and the call stays a
    /// call of `of`'s instance row, which substitution replaces by the instance's maker.
    fn codec_of(
        &mut self,
        path: &DefPath,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let mut ty = types[0];
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *self.t.kind(ty) {
            ty = x;
        }
        if let TyKind::Param(_) = self.t.kind(ty) {
            return None;
        }
        // A declared codec type's maker is the derived `<Name>$codec` at its arguments.
        if let TyKind::Adt(d, args) = self.t.kind(ty).clone()
            && let Some(maker) = self.p.decls[d.index()].codec().and_then(|c| c.maker)
        {
            return Some(self.emit_call(maker as usize, &args, vals, result, n));
        }
        // A core List, Map, Option or Result's maker is std's `of_list` (`of_bytes` at u8) and the rest at
        // its arguments, when theirs are built.
        if let TyKind::Adt(d, args) = self.t.kind(ty).clone()
            && let Some((maker, args)) = self.p.structural_codecs().maker(&self.t, d, &args)
        {
            if !self.codec_built(ty) {
                self.gap(n, "the codec of this type is not built");
                return Some(None);
            }
            return Some(self.emit_call(maker as usize, &args, vals, result, n));
        }
        // A tuple's maker is the one the front derives, its row's codec hook.
        if let TyKind::Tuple(_) = self.t.kind(ty) {
            let Some(row) = self.e.type_row(self.p, &mut self.t, ty) else {
                self.gap(n, "codec type layout is not built");
                return Some(None);
            };
            self.e.tuple_codec(self.p, &mut self.t, ty, row);
            let maker = self.e.module.types[row.index()].codec;
            return Some(match maker {
                Some(maker) if self.codec_built(ty) => self.call_maker(FuncId(maker.0), result, n),
                _ => {
                    self.gap(n, "the codec of this type is not built");
                    None
                }
            });
        }
        // A sibling read of `of` (`crate::SIBLING_READS`).
        let name = crate::decls::layout_name(&self.p.decls, &self.t, ty);
        let maker = templates::builtin_codec(&name).and_then(|maker| {
            let mut path = path.clone();
            path.0.name = maker.into();
            self.p.function_id(&path)
        });
        let Some(maker) = maker else {
            self.gap(n, "the codec of this type is not built");
            return Some(None);
        };
        Some(self.emit_call(maker, &[], vals, result, n))
    }

    /// Whether `of` at `ty` reaches a maker: a parameter's is its instance's; a tuple's,
    /// or a List, Map, Option or Result's, is built when its arguments' are; a declared
    /// codec type has its derived one.
    fn codec_built(&mut self, ty: TyId) -> bool {
        let mut ty = ty;
        while let TyKind::Anchored(x, _) | TyKind::View(x) = *self.t.kind(ty) {
            ty = x;
        }
        match self.t.kind(ty).clone() {
            TyKind::Param(_) => true,
            TyKind::Tuple(xs) => xs.into_iter().all(|x| self.codec_built(x)),
            TyKind::Adt(d, _)
                if self.p.decls[d.index()]
                    .codec()
                    .and_then(|c| c.maker)
                    .is_some() =>
            {
                true
            }
            TyKind::Adt(d, args) if self.p.structural_codecs().has(d) => {
                args.into_iter().all(|x| self.codec_built(x))
            }
            _ => {
                let name = crate::decls::layout_name(&self.p.decls, &self.t, ty);
                templates::builtin_codec(&name).is_some()
            }
        }
    }
    /// A call of the maker row `maker`, `() -> Codec[T]`, whose result has type `result`.
    fn call_maker(&mut self, maker: FuncId, result: TyId, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let signature = self.e.module.functions[maker.index()].signature;
        let dest = self.e.module.signatures[signature.index()].dest?;
        let at = self.new_slot(PlaceTy::Declared(dest), n);
        let args = PoolRange::push(&mut self.b.pools.args, [Arg::Address(at)]);
        let results = PoolRange::push(&mut self.b.pools.values, []);
        let fail = self.fail_to(n);
        self.b.calls.push(CallSite {
            callee: Callee::Direct(maker),
            signature,
            args,
            results,
            fail,
            cold: false,
        });
        let call = CallId(self.b.calls.len() as u32 - 1);
        self.inst(Inst::Call { call }, n);
        let p = self.pointer(at, n);
        self.temp(result, Some(p));
        Some(p)
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
        self.builtin_use(f, n);
        let g = &self.p.fns[f];
        let codec = is_codec_of(&g.path);
        if codec && let Some(value) = self.codec_of(&g.path, types, vals, result, n) {
            return value;
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
        if codec {
            // `of` at a type parameter P: the instance's P chooses the maker (9.1).
            let Some(row) = self.e.type_row(self.p, &mut self.t, types[0]) else {
                self.gap(n, "codec type layout is not built");
                return None;
            };
            if let Some(call) = self.e.generic_calls.get_mut(&func) {
                call.operation = Some(CallOperation::CodecOf(row));
            }
        }
        if !g.generics.is_empty() && g.body.is_none() && g.path.package_is("core", "1") {
            self.gap(n, "this generic core operation is not built");
            return None;
        }
        let sig = &self.e.module.signatures[signature.index()];
        let (dest, result_ty) = (sig.dest, sig.results.first().copied());
        // The source signature omits uncarried parameters. With no omissions,
        // it already supplies each argument's carriage in source order.
        let all_carried =
            sig.params.len() == g.params.len() && g.drop_of.is_none() && g.clone_of.is_none();
        // The destination is the first argument: a fresh place the result is written to.
        let source_call = self.expression_destination(n).is_some()
            && self.kind(n) == NodeKind::Call
            && matches!(self.p.target(self.at, self.kids(n)[0]), Some(Target::Definition(id))
                if matches!(self.p.by_definition[id.0 as usize], Some(Def::Fn(source)) if source as usize == f));
        let dest = dest.map(|id| {
            if source_call {
                self.result_slot(PlaceTy::Declared(id), n)
            } else {
                self.new_slot(PlaceTy::Declared(id), n)
            }
        });
        let start = self.b.pools.args.len();
        self.b.pools.args.extend(dest.map(Arg::Address));
        for (i, p) in g.params.iter().enumerate() {
            if p.mode == Mode::Take {
                // A1 cleanup: the callee owns a taken argument from entry (T2.3).
                self.consume(vals[i], n);
            }
            let place = if all_carried {
                match self.e.module.signatures[signature.index()].params[i] {
                    Param::Value(_) => None,
                    Param::Address { place, .. } => Some(place),
                }
            } else {
                let ty = subst(&mut self.t, p.ty, types);
                let rep = match self.rep(ty) {
                    Ok(Rep::None) if p.mode != Mode::Mut => continue,
                    Ok(rep) => rep,
                    Err(what) => {
                        self.gap(n, what);
                        return None;
                    }
                };
                match rep {
                    _ if p.mode == Mode::Mut => {
                        Some(self.e.mut_place(self.p, &mut self.t, ty, rep)?)
                    }
                    Rep::Place(id) => Some(PlaceTy::Declared(id)),
                    _ => None,
                }
            };
            // A live call has a value for each carried source argument.
            let v = vals[i].expect("a live call has every carried argument value");
            let arg = match place {
                Some(place) => Arg::Address(self.address(AddressBase::Value(v), &[], place, n)),
                None => Arg::Value(v),
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
            cold: false,
        });
        let call = CallId(self.b.calls.len() as u32 - 1);
        self.inst(Inst::Call { call }, n);
        if let Some(release) = self.p.foreign_release_for(g) {
            let (function, _) = match self.instance_row(release, &[]) {
                Ok(row) => row,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let result = self.value(Ty::Ptr);
            self.inst(Inst::FunctionRef { result, function }, n);
        }
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

    /// MEM-17 selects the expected type from the source. Only an ordinary clone
    /// can select a payload of a Box, Rc or Arc.
    fn clone_refusal(&self, x: TyId, u: TyId, into_region: bool) -> Option<&'static str> {
        let mut part = self.bare(x);
        let u = match self.t.kind(u) {
            TyKind::Anchored(u, _) => *u,
            _ => u,
        };
        if matches!(self.t.kind(part), TyKind::Error | TyKind::Hole)
            || matches!(self.t.kind(u), TyKind::Error | TyKind::Hole)
        {
            return None;
        }
        if into_region {
            return (part != u || kind(&self.p.decls, &self.t, u, &self.params) != Kind::Region)
                .then_some("clone_in copies a region value of the expected type");
        }
        if self.capture_holds(x) & (crate::H_RESOURCE | crate::H_SCOPED | crate::H_OPAQUE) != 0 {
            return Some(
                "a value holding a resource, a scoped value or an opaque owner is never cloned",
            );
        }
        loop {
            if part == u {
                return None;
            }
            match self.role_of(part) {
                Some((Role::Box | Role::Rc | Role::Arc, args)) => part = self.bare(args[0]),
                _ => return Some("no part of this value is the expected type"),
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
        self.builtin_use(f, n);
        let g = &self.p.fns[f];
        if g.path.0.modules == ["core"] && g.path.0.name == "fail" {
            // `fail(msg)` raises with the message it takes (FORM `raise fail`).
            let Some(string) = self.p.core_decl("String") else {
                self.gap(n, "fail without the prelude's String is not built");
                return None;
            };
            let string = self.t.mk(TyKind::Adt(string, vec![]));
            let id = self.place_row(string, "fail's message without a place is not built", n)?;
            let msg = vals[0]?;
            if !self.live() {
                return None;
            }
            let address = self.address(AddressBase::Value(msg), &[], PlaceTy::Declared(id), n);
            // The message moves into the cause: the raise's cleanup must not clean it too
            // (that cleanup ran its storage's free twice, once here and once with the Causes).
            self.consume(vals[0], n);
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
                // This implementation gap is independent of earlier refusals
                // and does not require live emission to be reported.
                self.gap(n, "a float operation on a type parameter is not built");
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
        let (signed, width) = int_of(input)?;
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
        if name == "abs" {
            // `if a < 0 { -a } else { a }`: the negation is `-a`'s own checked
            // negation, so MIN fails with `-a`'s cause at the call's site.
            let a = vals[0]?;
            if !signed {
                return Some(a);
            }
            let zero = self.konst(ty, 0, n)?;
            let negative = self.numeric_compare(CompareOp::Lt, a, zero, n);
            let arm = self.new_block(&[]);
            let join = self.new_block(&[ty]);
            let (yes, no) = (self.edge(arm, &[]), self.edge(join, &[a]));
            self.end(
                Terminator::Branch {
                    condition: negative,
                    yes,
                    no,
                },
                Some(n),
            );
            self.start(arm);
            let result = self.value(ty);
            let fail = self.fail_to(n);
            self.inst(
                Inst::Unary {
                    result,
                    op: UnaryOp::Neg,
                    operand: a,
                    fail,
                },
                n,
            );
            self.jump(join, &[result], n);
            self.start(join);
            return self.block_params(join).first().copied();
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
        let Some(&target) = tys.get(1) else {
            self.gap(n, "a numeric conversion without a target type is not built");
            return None;
        };
        let Some(target_ty) = value_ty(&self.t, target).ok().flatten() else {
            self.gap(
                n,
                "a numeric conversion to a type without a scalar is not built",
            );
            return None;
        };
        let mode = match name {
            "widen" => {
                let (ts, b) = int_of(target)?;
                if !((signed == ts && width <= b) || (!signed && ts && width < b)) {
                    let (x, y) = (self.shown(input), self.shown(target));
                    self.refuse("SEM-1", n, format!("not every `{x}` fits `{y}`"));
                    return None;
                }
                ConvertMode::Widen
            }
            "truncate" if int_of(target)?.1 <= width => ConvertMode::Truncate,
            "truncate" => {
                let (x, y) = (self.shown(input), self.shown(target));
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
        let id = self.place_row(
            ty,
            "a checked numeric result without a place is not built",
            n,
        )?;
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
        let rep = self.rep_at(payload, n)?;
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
            "eq" | "lt" | "le" | "gt" | "ge" => {
                let op = match name {
                    "eq" => CompareOp::Eq,
                    "lt" => CompareOp::Lt,
                    "le" => CompareOp::Le,
                    "gt" => CompareOp::Gt,
                    _ => CompareOp::Ge,
                };
                return Some(self.numeric_compare(op, operand, vals[1]?, n));
            }
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
        let error = self.prelude("FloatError", n)?;
        let error = self.t.mk(TyKind::Adt(error, vec![]));
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
    fn float_order_tie_key(&mut self, operand: ValueId, n: NodeId) -> Option<ValueId> {
        let (unsigned, signed) = match self.b.values[operand.index()] {
            Ty::Scalar(Scalar::F32) => (Scalar::U32, Scalar::I32),
            Ty::Scalar(Scalar::F64) => (Scalar::U64, Scalar::I64),
            _ => return None,
        };
        let bits =
            self.numeric_convert(Ty::Scalar(unsigned), ConvertMode::Reinterpret, operand, n)?;
        self.numeric_convert(Ty::Scalar(signed), ConvertMode::Truncate, bits, n)
    }

    fn float_total_order(
        &mut self,
        ty: TyId,
        a: ValueId,
        b: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let place = self.place_ty(ty, n)?;
        let to = self.new_slot(place, n);
        let lt = self.new_block(&[]);
        let eq = self.new_block(&[]);
        let gt = self.new_block(&[]);
        let check_greater = self.new_block(&[]);
        let tie = self.new_block(&[]);
        let check_equal = self.new_block(&[]);
        let join = self.new_block(&[]);

        let less = self.numeric_compare(CompareOp::Lt, a, b, n);
        self.numeric_branch(less, lt, check_greater, n);
        self.start(check_greater);
        let greater = self.numeric_compare(CompareOp::Gt, a, b, n);
        self.numeric_branch(greater, gt, tie, n);

        // Only numeric equality and unordered pairs reach this comparison.
        // Signed canonical bits order -0 before +0 and every NaN after all numbers.
        self.start(tie);
        let a = self.float_order_tie_key(a, n)?;
        let b = self.float_order_tie_key(b, n)?;
        let less = self.numeric_compare(CompareOp::Lt, a, b, n);
        self.numeric_branch(less, lt, check_equal, n);
        self.start(check_equal);
        let equal = self.numeric_compare(CompareOp::Eq, a, b, n);
        self.numeric_branch(equal, eq, gt, n);

        for (block, case) in [(lt, 0), (eq, 1), (gt, 2)] {
            self.start(block);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(join, &[], n);
        }
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

/// Whether `path` is `std.codec.of`, whose calls name a maker per instance.
pub(super) fn is_codec_of(path: &DefPath) -> bool {
    path.0.name == "of" && path.0.package.name == "std" && path.0.modules == ["codec"]
}
