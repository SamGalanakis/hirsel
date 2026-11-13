use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    // ---- blocks and control flow
    pub(super) fn block(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.depth += 1;
        let (scope, freezes) = (self.scope_slots.len(), self.freezes.len());
        let owners = self.owner_mark();
        self.tail_ty = UNIT;
        let r = self.statements(n, want);
        let value = std::mem::replace(&mut self.tail_ty, UNIT);
        self.descriptor_scope(scope, r.1, n);
        self.close_scope(scope, freezes, n, value);
        // The tail's temporaries end at the block's exit, before its locals, unless
        // the block's value is one (LANG-6).
        self.end_owners(owners, r.1, false, n);
        self.depth -= 1;
        r
    }
    pub(super) fn statements(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let stmts = self.kids(n);
        let mut diverged = false;
        for (i, &s) in stmts.iter().enumerate() {
            self.out.counts.nodes += 1;
            let last = i + 1 == stmts.len();
            let freezes = self.freezes.len();
            let owners = self.owner_mark();
            match self.kind(s) {
                // The tail's temporaries last to the end of the enclosing statement.
                NodeKind::ExpressionStatement if last => {
                    let r = self.expr(self.kids(s)[0], want);
                    self.tail_ty = self.synth_ty;
                    return r;
                }
                NodeKind::ExpressionStatement | NodeKind::Discard => {
                    let (t, _) = self.expr(self.kids(s)[0], None);
                    if self.result_parts(t).is_some_and(|(_, _, e)| e.is_some()) {
                        self.refuse(
                            "LANG-18",
                            self.kids(s)[0],
                            "an unused Result is discarded explicitly with `let _`",
                        );
                        return (ERROR, None);
                    }
                    diverged |= t == NEVER;
                }
                NodeKind::Let | NodeKind::LetMut => diverged |= self.let_(s),
                NodeKind::Assign => diverged |= self.assign(s),
                NodeKind::Error => self.halt(),
                _ => self.gap(s, "statement form is not built"),
            }
            // A temporary view ends with its statement (OWN-8), and so does every
            // temporary nothing took (LANG-6).
            self.unfreeze(freezes, |h| (h != Hold::View(Extent::Temp)).then_some(h));
            self.end_owners(owners, None, true, s);
        }
        let t = if diverged { NEVER } else { UNIT };
        match want {
            Some(w) if t != NEVER => {
                self.coerce(t, w, n);
                (if w.payload == 0 { w.ty } else { t }, None)
            }
            _ => (t, None),
        }
    }

    pub(super) fn let_(&mut self, s: NodeId) -> bool {
        let c = self.kids(s);
        let pat = c[0];
        let mut rest = &c[1..];
        let declared = match rest.first() {
            Some(&t) if is_type_node(self.kind(t)) => {
                rest = &rest[1..];
                Some(self.ty(t))
            }
            _ => None,
        };
        let Some((&value, rest)) = rest.split_first() else {
            return self.let_uninit(s, pat, declared);
        };
        if !rest.is_empty()
            || !matches!(self.kind(pat), NodeKind::BindPattern | NodeKind::Wildcard)
            || self.kind(pat) == NodeKind::BindPattern && self.binder(pat).is_none()
        {
            return self.destructure_let(s, pat, value, declared, rest.first().copied());
        }
        let freezes = self.freezes.len();
        let (t, v) = self.expr(value, declared.and_then(expect));
        let ty = declared.unwrap_or(t);
        // A let-bound view lasts to the end of its block (OWN-8).
        if crate::decls::kind(&self.p.decls, &self.t, ty, &self.params) == Kind::Scoped {
            let d = self.depth;
            self.unfreeze(freezes, |h| {
                Some(if h == Hold::View(Extent::Temp) {
                    Hold::View(Extent::Block(d))
                } else {
                    h
                })
            });
        }
        self.out.counts.nodes += 1;
        match self.kind(pat) {
            NodeKind::Wildcard => {}
            NodeKind::BindPattern => {
                if let Err(what) = self.rep(ty) {
                    self.gap(pat, what);
                }
                let Some(b) = self.binder(pat) else {
                    self.halt();
                    return t == NEVER;
                };
                let (binding, origin) = if self.kind(s) == NodeKind::LetMut {
                    (Binding::Slot(self.slot(ty, v, pat)), Origin::LetMut)
                } else {
                    // The binding owns the temporary's place itself.
                    self.consume(v, pat);
                    (Binding::Value(v), Origin::Let)
                };
                self.declare(b, ty, binding, origin, pat);
                self.bind_descriptor(b, v);
                self.own(b, pat);
                self.scope_slots.push(b);
            }
            _ => self.gap(pat, "patterns are not built"),
        }
        t == NEVER
    }
    /// `let x: T` with no value: an uninit slot (OWN-5), assigned before it is read.
    pub(super) fn let_uninit(&mut self, s: NodeId, pat: NodeId, declared: Option<TyId>) -> bool {
        let Some(ty) = declared else {
            self.refuse("LANG-5", s, "a `let` without a value needs a type");
            return false;
        };
        if self.kind(pat) != NodeKind::BindPattern {
            self.gap(pat, "patterns are not built");
            return false;
        }
        if let Err(what) = self.rep(ty) {
            self.gap(pat, what);
        }
        let Some(b) = self.binder(pat) else {
            self.halt();
            return false;
        };
        let origin = if self.kind(s) == NodeKind::LetMut {
            Origin::LetMut
        } else {
            Origin::Let
        };
        let binding = Binding::Slot(self.slot(ty, None, pat));
        self.declare(b, ty, binding, origin, pat);
        self.set(b, Avail::Uninit, pat);
        self.own(b, pat);
        self.scope_slots.push(b);
        false
    }
    /// A `let mut` slot holding `v`; its address is evaluated once, where it begins.
    pub(super) fn slot(&mut self, ty: TyId, v: Option<ValueId>, n: NodeId) -> Option<AddressId> {
        let r = self.rep(ty).ok()?;
        if !self.live() {
            return None;
        }
        let address = match r {
            Rep::None => {
                let id = self.e.type_row(self.p, &mut self.t, ty)?;
                self.new_slot(PlaceTy::Declared(id), n)
            }
            r => self.new_slot(r.stored().unwrap(), n),
        };
        self.install(address, r, v, n);
        Some(address)
    }
    /// Write `v`, a value carried as `r`, into the fresh place at `address`; an own
    /// value is moved, so nothing cleans `v` afterwards.
    pub(super) fn install(&mut self, address: AddressId, r: Rep, v: Option<ValueId>, n: NodeId) {
        match (r, v) {
            (Rep::Scalar(_) | Rep::View(_), Some(value)) => {
                self.inst(Inst::Store { address, value }, n)
            }
            (Rep::Place(id), Some(from)) if self.live() => self.copy_to(address, from, id, n),
            _ => {}
        }
    }

    /// `q = e` and `q op= e` (OWN-12): `q` is writable (OWN-5), not frozen, and reserved
    /// while `e` is evaluated.
    pub(super) fn assign(&mut self, s: NodeId) -> bool {
        if self.patterns.guard {
            self.refuse("LANG-17", s, "a match guard cannot assign");
            return false;
        }
        let c = self.kids(s);
        let (place, value) = (c[0], c[1]);
        let op = self.token_text(s);
        self.out.counts.uses += 1;
        let at = match self.place(place) {
            Ok(Some(at)) => at,
            Ok(None) => {
                self.gap(place, "assignment to this place is not built");
                return false;
            }
            Err(()) => return false,
        };
        let q = at.place.slot;
        if !self.capture_assignment(q, place) {
            return false;
        }
        let var = self.var(q);
        // OWN-12: a writable place, or a `let` declared without a value while uninit.
        let fresh = var.origin == Origin::Let && var.avail == Avail::Uninit && at.place.len == 0;
        if !(var.origin.writable() || fresh) || at.shared {
            let name = self.slot_name(q);
            self.refuse(
                "OWN-12",
                place,
                format!("this place of `{name}` is not writable"),
            );
            return false;
        }
        // OWN-5: a part of a slot is assigned only while the slot is live.
        if at.place.len > 0 && var.avail != Avail::Live {
            self.need_live(q, place);
            return false;
        }
        self.check_holds(at.place, Act::Write, place);
        let compound = op != "=";
        if compound {
            self.need_live(q, place);
        }
        let reserved = self.freezes.len();
        self.freeze(at.place, Hold::Assign);
        let (address, emitted) = match var.binding {
            Binding::Slot(a) if at.place.len == 0 => (a, true),
            _ => match self.field_address(at, place) {
                Some((a, _)) => (Some(a), true),
                None => (None, false),
            },
        };
        if !emitted && self.live() && self.ir_ty(at.ty).is_some() {
            self.gap(place, "assignment to this place is not built");
        }
        let diverged = if !compound {
            let (t, v) = self.expr(value, expect(at.ty));
            self.descriptor_store(at, v, place);
            self.capture_store(at, t, place);
            if let (Some(address), Ok(r)) = (address, self.rep(at.ty)) {
                // OWN-12: the old value is cleaned when it is live, or maybe with its
                // flag set; then the new one is installed and the flag set (T3.2-T3.4).
                // A part is assigned only while its slot is live (OWN-5), so the old
                // part is live and cleaned alone.
                // A `mut` parameter owns no row here but is always live. An old value
                // whose cleanup runs code is finalized after the install (T3.2).
                let saved = self.save_old(q, at.place.len == 0, address, at.ty, s);
                if saved.is_none() {
                    if at.place.len == 0 && self.var(q).own.is_some() {
                        self.drop_old(q, s);
                    } else if var.avail == Avail::Live {
                        self.drop_at(address, at.ty, s);
                    }
                }
                self.install(address, r, v, s);
                if at.place.len == 0 {
                    self.installed(q, s);
                }
                if let Some(Some(saved)) = saved {
                    self.finalize_old(saved, s);
                }
            }
            t == NEVER
        } else {
            // Compound: resolve once, read the old value, evaluate the right side,
            // compute, install; plain-only, and every type built here is plain.
            self.out.counts.places += 1;
            let bop = op.trim_end_matches('=');
            let old = match (address, value_ty(&self.t, at.ty)) {
                (Some(address), Ok(Some(t))) if self.live() => {
                    let result = self.value(t);
                    self.inst(
                        Inst::Read {
                            result,
                            address,
                            mode: ReadMode::Bits,
                        },
                        s,
                    );
                    Some(result)
                }
                _ => None,
            };
            let before = self.out.refusals.len();
            let rv = self.second_operand(s, value, at.ty);
            let t = if self.out.refusals.len() > before {
                ERROR
            } else {
                at.ty
            };
            if let Some(new) = self.arith(bop, at.ty, old, rv, s)
                && let Some(address) = address
            {
                self.inst(
                    Inst::Store {
                        address,
                        value: new,
                    },
                    s,
                );
            }
            t == NEVER
        };
        // The install ends the reservation; the record is the one pushed above, and
        // `e`'s own temporaries after it stay to the statement's end.
        self.unfreeze_at(reserved);
        if at.place.len == 0 {
            self.set(q, Avail::Live, s);
        }
        diverged
    }

    /// An `if` or `while` condition; its temporaries end with it (OWN-8).
    pub(super) fn condition(&mut self, n: NodeId) -> Option<ValueId> {
        let freezes = self.freezes.len();
        let owners = self.owner_mark();
        let (_, cv) = self.expr(n, expect(BOOL));
        self.unfreeze(freezes, |h| (h != Hold::View(Extent::Temp)).then_some(h));
        self.end_owners(owners, None, true, n);
        cv
    }
    pub(super) fn if_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        if self.kind(c[0]) == NodeKind::ConditionLet {
            return self.if_let(n, want);
        }
        let cv = self.condition(c[0]);
        let join = self.new_join();
        let mut no_block = None;
        if self.live()
            && let Some(cv) = cv
        {
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            let (ye, ne) = (self.edge(yes, &[]), self.edge(no, &[]));
            self.end(
                Terminator::Branch {
                    condition: cv,
                    yes: ye,
                    no: ne,
                },
                Some(c[0]),
            );
            self.start(yes);
            no_block = Some(no);
        }
        // The arms join by OWN-16; an arm that leaves contributes nothing.
        let (was_dead, mark) = (self.dead, self.log.len());
        let Some(&other) = c.get(2) else {
            // Without `else` the type is unit (LANG-11).
            let (_, _) = self.expr(c[1], expect(UNIT));
            let yes = self.arm_edge(mark);
            self.goto(join, UNIT, None, c[1]);
            if let Some(no) = no_block {
                self.start(no);
            }
            self.goto(join, UNIT, None, n);
            self.land(join);
            let no = Some((self.diffs.len() as u32, 0));
            self.merge_arms(mark, [yes, no], was_dead, n);
            return (UNIT, None);
        };
        let (yt, yv) = self.expr(c[1], want);
        let yes = self.arm_edge(mark);
        self.goto(join, yt, yv, c[1]);
        if let Some(no) = no_block {
            self.start(no);
        }
        // An arm of type Never constrains nothing (LANG-11).
        let other_want = want
            .filter(|w| w.payload == 0)
            .or(if yt == NEVER { None } else { expect(yt) })
            .or(want);
        let (ot, ov) = self.expr(other, other_want);
        let no = self.arm_edge(mark);
        let ty = if yt == NEVER { ot } else { yt };
        self.goto(join, ty, ov, other);
        let v = self.land(join);
        self.merge_arms(mark, [yes, no], was_dead, n);
        (ty, v)
    }
    /// The state an arm ends in, then back to the state at `mark` for the next arm.
    pub(super) fn arm_edge(&mut self, mark: usize) -> Option<(u32, u32)> {
        let e = (!self.dead).then(|| self.edge_state(mark));
        self.undo(mark);
        self.dead = false;
        e
    }
    pub(super) fn merge_arms(
        &mut self,
        mark: usize,
        arms: [Option<(u32, u32)>; 2],
        was_dead: bool,
        n: NodeId,
    ) {
        let mut edges = std::mem::take(&mut self.edge_buf);
        edges.clear();
        edges.extend(arms.into_iter().flatten());
        self.merge(mark, &edges, n);
        self.edge_buf = edges;
        self.dead |= was_dead;
    }

    pub(super) fn push_frame(
        &mut self,
        label: Option<String>,
        kind: FrameKind,
        hint: Option<Want>,
    ) {
        let exit = self.new_join();
        self.frames.push(Frame {
            label,
            kind,
            header: None,
            exit,
            ty: hint.filter(|w| w.payload == 0).map(|w| w.ty),
            hint,
            left: false,
            mark: self.log.len(),
            born: self.born,
            exits: self.exits.len(),
            scopes: self.pattern_scope_mark(),
            owners: self.owner_mark(),
        });
    }
    /// Pop the innermost frame, joining its exits (OWN-16).
    pub(super) fn pop_frame(&mut self, n: NodeId) -> Frame {
        let fi = self.frames.len() - 1;
        self.merge_exits(fi, n);
        self.frames.pop().unwrap()
    }

    pub(super) fn loop_(
        &mut self,
        n: NodeId,
        want: Option<Want>,
        label: Option<String>,
    ) -> (TyId, Option<ValueId>) {
        let body = self.kids(n)[0];
        self.push_frame(label, FrameKind::Loop, want);
        if self.live() {
            let header = self.new_block(&[]);
            self.jump(header, &[], n);
            self.start(header);
            self.frames.last_mut().unwrap().header = Some(header);
        }
        self.loop_body(body);
        if let Some(h) = self.frames.last().unwrap().header {
            self.jump(h, &[], body);
        }
        self.back_edge(self.frames.len() - 1, self.close_brace(body));
        let f = self.pop_frame(n);
        let v = self.land(f.exit);
        if !f.left {
            return (NEVER, None);
        }
        (f.ty.unwrap_or(UNIT), v)
    }
    /// A loop body is checked against unit (LANG-15, LANG-11).
    pub(super) fn loop_body(&mut self, body: NodeId) {
        let _ = self.expr(body, expect(UNIT));
    }

    pub(super) fn while_(&mut self, n: NodeId, label: Option<String>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        if self.kind(c[0]) == NodeKind::ConditionLet {
            return self.while_let(n, label);
        }
        self.push_frame(label, FrameKind::While, expect(UNIT));
        let mut header = None;
        if self.live() {
            let h = self.new_block(&[]);
            self.jump(h, &[], n);
            self.start(h);
            header = Some(h);
            self.frames.last_mut().unwrap().header = header;
        }
        let cv = self.condition(c[0]);
        // The condition's false edge leaves for the exit (OWN-17).
        self.exit_edge(self.frames.len() - 1);
        let exit = self.frames.last().unwrap().exit;
        if self.live()
            && let Some(cv) = cv
        {
            let body = self.new_block(&[]);
            let out = self.new_block(&[]);
            self.joins[exit].block = Some(out);
            let (ye, ne) = (self.edge(body, &[]), self.edge(out, &[]));
            self.end(
                Terminator::Branch {
                    condition: cv,
                    yes: ye,
                    no: ne,
                },
                Some(c[0]),
            );
            self.start(body);
        }
        self.loop_body(c[1]);
        if let Some(h) = header {
            self.jump(h, &[], c[1]);
        }
        self.back_edge(self.frames.len() - 1, self.close_brace(c[1]));
        let f = self.pop_frame(n);
        self.land(f.exit);
        (UNIT, None)
    }

    /// SEM-6 / OWN-17: evaluate and freeze the source once. The forward body check
    /// uses the entry state at its head and checks each back edge without a fixpoint.
    pub(super) fn for_(&mut self, n: NodeId, label: Option<String>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let source = c[1];
        let consuming = self.kind(n) == NodeKind::ForTake;
        let source_owners = self.owner_mark();
        let mut ungrouped = source;
        while self.kind(ungrouped) == NodeKind::Group {
            ungrouped = self.kids(ungrouped)[0];
        }
        let range = self.kind(ungrouped) == NodeKind::Binary
            && matches!(self.token_text(ungrouped), ".." | "..=");
        let freezes = self.freezes.len();
        let (elem, start, limit, inclusive, sequence) = if range {
            if self.kind(n) == NodeKind::ForTake {
                self.refuse(
                    "LANG-16",
                    source,
                    "only an own List is consumed by iteration",
                );
                return (ERROR, None);
            }
            let bounds = self.kids(ungrouped);
            if !self.synthesises(bounds[0]) && !self.synthesises(bounds[1]) {
                return self.gap_ty(source, "the type of an unsuffixed range is unspecified");
            }
            let (ty, lo, hi) = self.operands(ungrouped, None);
            if ty != ERROR && int_of(&self.t, ty).is_none() {
                return self.gap_ty(source, "iteration of a non-integer range is unspecified");
            }
            (ty, lo, hi, self.token_text(ungrouped) == "..=", None)
        } else {
            let at = match self.place(source) {
                Ok(at) => at,
                Err(()) => return (ERROR, None),
            };
            let (ty, value) = if let Some(at) = at {
                self.out.counts.nodes += 1;
                if consuming {
                    if !matches!(self.t.kind(at.ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::List))
                    {
                        self.refuse(
                            "LANG-16",
                            source,
                            "only an own List is consumed by iteration",
                        );
                        return (ERROR, None);
                    }
                    self.use_place(at, Use::Take, source);
                    let value = self.emit_place(at, source);
                    self.temp(at.ty, value);
                    (at.ty, value)
                } else {
                    self.need_live(at.place.slot, source);
                    self.check_holds(at.place, Act::Read, source);
                    (at.ty, self.place_ptr(at, source))
                }
            } else {
                self.expr(source, None)
            };
            let (inner, _) = self.indexed_type(ty);
            let (elem, count) = match self.t.kind(inner) {
                TyKind::Array(elem, count) => (*elem, Some(*count)),
                TyKind::Adt(d, args) if self.p.decls[d.index()].role == Some(Role::List) => {
                    (args[0], None)
                }
                TyKind::Adt(d, args) if self.p.decls[d.index()].role == Some(Role::Map) => {
                    let (key, value) = (args[0], args[1]);
                    let key = self.t.mk(TyKind::View(key));
                    let value = self.t.mk(TyKind::View(value));
                    let pair = self.t.mk(TyKind::Tuple(vec![key, value]));
                    (pair, None)
                }
                TyKind::Error => return (ERROR, None),
                TyKind::Adt(d, _)
                    if is_core_prelude(
                        &self.p.decls[d.index()].path,
                        &["FixedSpan", "SeqSpan", "IndexedSpan", "Causes"],
                    ) =>
                {
                    return self.gap_ty(
                        source,
                        "iteration of this stable or map representation is not built",
                    );
                }
                _ => {
                    self.refuse("LANG-16", source, "this source is not iterable");
                    return (ERROR, None);
                }
            };
            if consuming
                && (ty != inner
                    || !matches!(self.t.kind(inner), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::List)))
            {
                self.refuse(
                    "LANG-16",
                    source,
                    "only an own List is consumed by iteration",
                );
                return (ERROR, None);
            }
            if !consuming && let Some(at) = at {
                self.freeze(at.place, Hold::View(Extent::Block(self.depth + 1)));
            }
            let start = self.konst(Ty::Scalar(Scalar::U64), 0, n);
            let limit = match count {
                Some(count) => self.konst(Ty::Scalar(Scalar::U64), count, n),
                None => value
                    .filter(|_| self.live())
                    .map(|v| self.read_sequence_word(v, 8, Ty::Scalar(Scalar::U64), n)),
            };
            (elem, start, limit, false, Some((inner, value)))
        };
        let index_ty = if range { elem } else { scalar(Scalar::U64) };
        let index_ir = self.ir_ty(index_ty);
        let index_slot = self.slot(index_ty, start, n);
        let drain_data = if consuming {
            match (sequence, index_slot, limit) {
                (Some((_, Some(header))), Some(next), Some(end)) if self.live() => {
                    let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
                    let eight = self.konst(Ty::Scalar(Scalar::U64), 8, n).unwrap();
                    let len = self.address(
                        AddressBase::Value(header),
                        &[Step::Byte(eight)],
                        PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                        n,
                    );
                    let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n).unwrap();
                    self.inst(
                        Inst::Store {
                            address: len,
                            value: zero,
                        },
                        n,
                    );
                    self.drain_owner(data, next, end, elem);
                    Some(data)
                }
                _ => None,
            }
        } else {
            None
        };
        self.push_frame(label, FrameKind::While, expect(UNIT));
        let fi = self.frames.len() - 1;
        let mut flow = None;
        if let (true, Some(_), Some(limit), Some(index_ir), Some(index_slot)) =
            (self.live(), start, limit, index_ir, index_slot)
        {
            let head = self.new_block(&[]);
            let body = self.new_block(&[]);
            let advance = self.new_block(&[]);
            let out = self.new_block(&[]);
            self.joins[self.frames[fi].exit].block = Some(out);
            self.frames[fi].header = Some(advance);
            self.jump(head, &[], n);
            self.start(head);
            let index = self.value(index_ir);
            self.inst(
                Inst::Read {
                    result: index,
                    address: index_slot,
                    mode: ReadMode::Bits,
                },
                n,
            );
            let condition = self.value(Ty::Scalar(Scalar::Bool));
            self.inst(
                Inst::Compare {
                    result: condition,
                    op: if inclusive {
                        CompareOp::Le
                    } else {
                        CompareOp::Lt
                    },
                    left: index,
                    right: limit,
                },
                n,
            );
            let yes = self.edge(body, &[]);
            let no = self.edge(out, &[]);
            self.end(Terminator::Branch { condition, yes, no }, Some(n));
            self.start(body);
            flow = Some((head, advance, out, index, limit, index_ir, index_slot));
        }
        // The empty source reaches the exit without changing any outer slot.
        self.exit_edge(fi);
        let scope = self.scope_slots.len();
        let iteration_holds = self.freezes.len();
        self.depth += 1;
        let value = match (flow, sequence) {
            (Some((_, _, _, index, _, _, _)), Some((collection, Some(pointer)))) => {
                if matches!(self.t.kind(collection), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Map))
                {
                    let advance = flow.unwrap().1;
                    self.map_pair(pointer, collection, index, elem, advance, n)
                } else {
                    let place = self.place_ty(collection, n);
                    let element = self.place_ty(elem, n);
                    match (place, element) {
                        (Some(place), Some(element)) => {
                            let base = self.address(AddressBase::Value(pointer), &[], place, n);
                            let at = if let Some(data) = drain_data {
                                Some(self.address(
                                    AddressBase::Value(data),
                                    &[Step::Index(index)],
                                    element,
                                    n,
                                ))
                            } else {
                                self.index_address(base, collection, index, element, n)
                            };
                            at.and_then(|at| self.iteration_value(at, elem, n))
                        }
                        _ => None,
                    }
                }
            }
            (Some((_, _, _, index, _, _, _)), None) => Some(index),
            _ => None,
        };
        if consuming
            && let Some((_, _, _, index, _, index_ir, next_slot)) = flow
            && self.live()
        {
            let one = self.konst(index_ir, 1, n).unwrap();
            let after = self.value(index_ir);
            self.inst(
                Inst::Binary {
                    result: after,
                    op: BinaryOp::Add,
                    left: index,
                    right: one,
                    fail: None,
                },
                n,
            );
            self.inst(
                Inst::Store {
                    address: next_slot,
                    value: after,
                },
                n,
            );
            self.temp(elem, value);
        }
        self.for_pattern(c[0], elem, value, range || consuming);
        self.loop_body(c[2]);
        self.close_scope(scope, iteration_holds, c[2], UNIT);
        self.end_owners(self.frames[fi].owners, None, false, c[2]);
        self.depth -= 1;
        self.back_edge(fi, self.close_brace(c[2]));
        if let Some((head, advance, out, index, limit, index_ir, index_slot)) = flow
            && !self.broken
        {
            self.jump(advance, &[], c[2]);
            self.start(advance);
            if inclusive {
                // Test the inclusive end before incrementing, so MAX never overflows.
                let increment = self.new_block(&[]);
                let finished = self.value(Ty::Scalar(Scalar::Bool));
                self.inst(
                    Inst::Compare {
                        result: finished,
                        op: CompareOp::Eq,
                        left: index,
                        right: limit,
                    },
                    n,
                );
                let yes = self.edge(out, &[]);
                let no = self.edge(increment, &[]);
                self.end(
                    Terminator::Branch {
                        condition: finished,
                        yes,
                        no,
                    },
                    Some(n),
                );
                self.start(increment);
            }
            if consuming {
                self.jump(head, &[], n);
            } else {
                let one = self.konst(index_ir, 1, n).unwrap();
                let next = self.value(index_ir);
                self.inst(
                    Inst::Binary {
                        result: next,
                        op: BinaryOp::Add,
                        left: index,
                        right: one,
                        fail: None,
                    },
                    n,
                );
                self.inst(
                    Inst::Store {
                        address: index_slot,
                        value: next,
                    },
                    n,
                );
                self.jump(head, &[], n);
            }
        }
        let frame = self.pop_frame(n);
        self.land(frame.exit);
        if consuming {
            self.end_owners(source_owners, None, false, n);
        }
        self.unfreeze(freezes, |_| None);
        (UNIT, None)
    }
    fn iteration_value(&mut self, address: AddressId, ty: TyId, n: NodeId) -> Option<ValueId> {
        match self.rep(ty).ok()? {
            Rep::None => None,
            Rep::Scalar(t) => {
                let result = self.value(t);
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
            Rep::View(_) => {
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
            Rep::Place(_) => Some(self.pointer(address, n)),
        }
    }

    pub(super) fn labelled(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let label = Some(self.token_text(n).to_owned());
        let e = self.kids(n)[0];
        match self.kind(e) {
            NodeKind::Loop => self.loop_(e, want, label),
            NodeKind::While => self.while_(e, label),
            NodeKind::For | NodeKind::ForTake => self.for_(e, label),
            NodeKind::Block => {
                self.push_frame(label, FrameKind::Block, want);
                let (t, v) = self.block(e, want);
                let exit = self.frames.last().unwrap().exit;
                let fixed = self.frames.last().unwrap().ty;
                let ty = match fixed {
                    Some(f) if t != NEVER => {
                        self.coerce(
                            t,
                            Want {
                                ty: f,
                                view: false,
                                payload: 0,
                            },
                            e,
                        );
                        f
                    }
                    Some(f) => f,
                    None => t,
                };
                self.goto(exit, ty, v, e);
                self.exit_edge(self.frames.len() - 1);
                let f = self.pop_frame(n);
                let v = self.land(f.exit);
                if t == NEVER && !f.left {
                    return (NEVER, None);
                }
                (ty, v)
            }
            _ => self.gap_ty(e, "labelled form is not built"),
        }
    }

    /// The frame an exit leaves: the labelled one, or the innermost loop.
    pub(super) fn frame_of(&mut self, n: NodeId) -> Option<usize> {
        let label = self.item.tree.node(n).token.map(|_| self.token_text(n));
        let i = self.frames.iter().rposition(|f| match label {
            Some(l) => f.label.as_deref() == Some(l),
            None => f.kind != FrameKind::Block,
        });
        if i.is_none() {
            // Refused LANG-15 by the resolver.
            self.halt();
        }
        i
    }

    pub(super) fn break_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let value = self.kids(n).first().copied();
        let Some(i) = self.frame_of(n) else {
            return (NEVER, None);
        };
        if self.frames[i].kind == FrameKind::While && value.is_some() {
            // Refused LANG-15 by the resolver.
            self.halt();
            return (NEVER, None);
        }
        let fixed = self.frames[i].ty;
        let (t, v) = match value {
            Some(e) => self.expr(e, fixed.and_then(expect).or(self.frames[i].hint)),
            None => {
                if let Some(f) = fixed {
                    self.coerce(
                        UNIT,
                        Want {
                            ty: f,
                            view: false,
                            payload: 0,
                        },
                        n,
                    );
                }
                (UNIT, None)
            }
        };
        let f = &mut self.frames[i];
        f.left = true;
        if f.ty.is_none() && t != NEVER {
            f.ty = Some(t);
        }
        let (exit, ty) = (f.exit, f.ty.unwrap_or(t));
        // The carried value goes to the exit's join; the rest is cleaned (FAIL-8, 9).
        self.consume(v, n);
        if let (Some(v), Ok(Rep::Place(id))) = (v, self.rep(ty))
            && self.e.runs_code(id)
        {
            self.pending = Some((AddressBase::Value(v), id));
        }
        self.unwind(self.frames[i].owners, self.frames[i].scopes, n);
        self.goto(exit, ty, v, n);
        self.exit_edge(i);
        self.dead = true;
        (NEVER, None)
    }

    pub(super) fn continue_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let Some(i) = self.frame_of(n) else {
            return (NEVER, None);
        };
        if self.frames[i].kind == FrameKind::Block {
            // Refused LANG-15 by the resolver.
            self.halt();
            return (NEVER, None);
        }
        self.unwind(self.frames[i].owners, self.frames[i].scopes, n);
        if let Some(h) = self.frames[i].header {
            self.jump(h, &[], n);
        }
        self.cur = None;
        self.back_edge(i, self.span(n));
        self.dead = true;
        (NEVER, None)
    }

    pub(super) fn return_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let w = Want {
            ty: self.fd.result,
            view: false,
            payload: 0,
        };
        let v = match self.kids(n).first() {
            Some(&e) => self.expr(e, Some(w)).1,
            None => {
                self.coerce(UNIT, w, n);
                None
            }
        };
        self.ret(v, n);
        self.dead = true;
        (NEVER, None)
    }
}
