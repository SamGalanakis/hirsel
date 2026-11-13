use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    // ---- ownership state (OWN-5, OWN-9 to OWN-18)
    pub(super) fn var(&self, s: u32) -> Var {
        self.vars[s as usize].expect("declared slot")
    }
    pub(super) fn slot_name(&self, s: u32) -> &'c str {
        let p: &'c Program<'a> = self.p;
        p.index.symbols.text(
            p.items[self.at.module as usize][self.at.item as usize]
                .resolution
                .binders[s as usize]
                .name,
        )
    }
    pub(super) fn declare(
        &mut self,
        b: u32,
        ty: TyId,
        binding: Binding,
        origin: Origin,
        n: NodeId,
    ) {
        crate::cost_scope!("phase", "ownership state");
        self.vars[b as usize] = Some(Var {
            ty,
            binding,
            origin,
            avail: Avail::Uninit,
            at: n,
            born: self.born,
            depth: self.depth,
            flagged: false,
            holds: 0,
            own: None,
        });
        self.born += 1;
        self.set(b, Avail::Live, n);
    }
    pub(super) fn set(&mut self, s: u32, a: Avail, n: NodeId) {
        crate::cost_scope!("phase", "ownership state");
        let v = self.vars[s as usize].as_mut().expect("declared slot");
        if v.avail != a {
            self.epoch += 1;
            self.log
                .push((s, v.avail, v.at, self.descriptors.slots.get(&s).copied()));
            v.avail = a;
            v.at = n;
        }
    }
    pub(super) fn undo(&mut self, mark: usize) {
        crate::cost_scope!("phase", "ownership state");
        while self.log.len() > mark {
            let (s, a, at, descriptor) = self.log.pop().expect("logged");
            self.epoch += 1;
            let v = self.vars[s as usize].as_mut().expect("declared slot");
            v.avail = a;
            v.at = at;
            self.descriptors.replace_slot(s, descriptor);
        }
    }
    /// The current state of each slot changed since `mark`: one incoming edge of a join.
    pub(super) fn edge_state(&mut self, mark: usize) -> (u32, u32) {
        let start = self.diffs.len() as u32;
        for i in mark..self.log.len() {
            let s = self.log[i].0;
            let a = self.var(s).avail;
            self.diffs
                .push((s, a, self.descriptors.slots.get(&s).copied()));
        }
        (start, self.diffs.len() as u32 - start)
    }
    /// Join `edges` (OWN-16) into the state at `mark`; a slot no edge changed keeps its
    /// state there. With no edge, nothing reaches the join.
    pub(super) fn merge(&mut self, mark: usize, edges: &[(u32, u32)], n: NodeId) {
        crate::cost_scope!("phase", "ownership state");
        self.undo(mark);
        self.dead = edges.is_empty();
        self.merges += 1;
        let m = self.merges;
        let mut touched = std::mem::take(&mut self.touched);
        touched.clear();
        for &(start, len) in edges {
            self.edges_seen += 1;
            let e = self.edges_seen;
            for k in start..start + len {
                let (s, a, descriptor) = self.diffs[k as usize];
                let previous = self.acc[s as usize];
                let descriptor = if previous.merge == m && previous.edge != e {
                    self.union_descriptor(previous.descriptor, descriptor)
                } else {
                    descriptor
                };
                let x = &mut self.acc[s as usize];
                if x.edge == e {
                    continue;
                }
                x.edge = e;
                if x.merge != m {
                    *x = Acc {
                        avail: a,
                        count: 1,
                        merge: m,
                        edge: e,
                        descriptor,
                    };
                    touched.push(s);
                } else {
                    x.avail = join(x.avail, a);
                    x.count += 1;
                    x.descriptor = descriptor;
                }
            }
        }
        for &s in &touched {
            let x = self.acc[s as usize];
            let base = self.var(s).avail;
            let a = if (x.count as usize) < edges.len() {
                join(x.avail, base)
            } else {
                x.avail
            };
            if a == Avail::Maybe {
                self.vars[s as usize].as_mut().unwrap().flagged = true;
            }
            let descriptor = if (x.count as usize) < edges.len() {
                self.union_descriptor(x.descriptor, self.descriptors.slots.get(&s).copied())
            } else {
                x.descriptor
            };
            self.set_slot_descriptor(s, descriptor);
            self.set(s, a, n);
        }
        self.touched = touched;
    }
    /// OWN-17: at a back edge every slot declared outside loop `fi` is in its entry
    /// state; the site is the edge, a `continue` or the body's closing `}`.
    pub(super) fn back_edge(&mut self, fi: usize, site: Span) {
        if self.dead {
            return;
        }
        let (mark, born) = (self.frames[fi].mark, self.frames[fi].born);
        self.merges += 1;
        let m = self.merges;
        let end = self.log.len();
        for i in mark..end {
            let (s, entry, _, descriptor) = self.log[i];
            if self.acc[s as usize].merge == m {
                continue;
            }
            self.acc[s as usize].merge = m;
            let v = self.var(s);
            if v.born < born
                && !self.broken
                && !self
                    .descriptors
                    .same(self.descriptors.slots.get(&s).copied(), descriptor)
            {
                let name = self.slot_name(s);
                self.refuse_at(
                    "OWN-17",
                    site,
                    format!("`{name}` changes its captured places or modes when the loop repeats"),
                );
            }
            if v.born < born && v.avail != entry {
                let name = self.slot_name(s);
                self.refuse_at(
                    "OWN-17",
                    site,
                    format!("`{name}` is not in its loop-entry state when the loop repeats"),
                );
            }
        }
    }
    /// Leave for frame `fi`'s exit, carrying this state to its join (OWN-16).
    pub(super) fn exit_edge(&mut self, fi: usize) {
        if !self.dead {
            let (start, len) = self.edge_state(self.frames[fi].mark);
            self.exits.push((fi, start, len));
        }
    }
    /// Join frame `fi`'s exit edges; it is the innermost open frame.
    pub(super) fn merge_exits(&mut self, fi: usize, n: NodeId) {
        let from = self.frames[fi].exits;
        let mut edges = std::mem::take(&mut self.edge_buf);
        edges.clear();
        edges.extend(
            self.exits[from..]
                .iter()
                .filter(|e| e.0 == fi)
                .map(|e| (e.1, e.2)),
        );
        self.exits.truncate(from);
        let mark = self.frames[fi].mark;
        self.merge(mark, &edges, n);
        self.edge_buf = edges;
    }

    /// OWN-5, OWN-16: a read, view, loan or move needs a live slot.
    pub(super) fn need_live(&mut self, s: u32, n: NodeId) {
        let v = self.var(s);
        let name = self.slot_name(s);
        match v.avail {
            Avail::Live => return,
            Avail::Maybe => {
                self.refuse("OWN-16", n, format!("`{name}` may have been moved"));
            }
            Avail::Moved => self.refuse("OWN-5", n, format!("`{name}` was moved")),
            Avail::Uninit => self.refuse("OWN-5", n, format!("`{name}` is not initialised")),
        }
        // Refused once; later uses are not refused again for it.
        self.set(s, Avail::Live, n);
    }
    /// The freeze or reservation `p` crosses, if any (OWN-9 to OWN-12).
    pub(super) fn check_holds(&mut self, p: Place, act: Act, n: NodeId) -> bool {
        self.check_holds_at(p, act, self.span(n))
    }
    pub(super) fn check_holds_at(&mut self, p: Place, act: Act, site: Span) -> bool {
        crate::cost_scope!("phase", "ownership state");
        if matches!(act, Act::Write) {
            self.capture_write(p.slot, site);
        }
        if self.var(p.slot).holds == 0 {
            return true;
        }
        for i in (0..self.freezes.len()).rev() {
            let f = self.freezes[i];
            if overlap(&self.steps, f.place, p)
                && let Some(rule) = breaks(f.hold, act)
            {
                let name = self.slot_name(p.slot);
                let why = match f.hold {
                    Hold::View(_) => "is viewed",
                    Hold::CallView(..) => "is viewed by an argument of this call",
                    Hold::CallMut(_) => "is lent `mut` to this call",
                    Hold::Assign => "is reserved by an assignment",
                };
                self.refuse_at(rule, site, format!("a place of `{name}` {why}"));
                return false;
            }
        }
        true
    }
    pub(super) fn unfreeze_at(&mut self, i: usize) {
        let x = self.freezes.remove(i);
        self.vars[x.place.slot as usize].as_mut().unwrap().holds -= 1;
    }
    pub(super) fn freeze(&mut self, place: Place, hold: Hold) {
        crate::cost_scope!("phase", "ownership state");
        self.vars[place.slot as usize].as_mut().unwrap().holds += 1;
        self.freezes.push(Freeze { place, hold });
    }
    /// Rewrite or end the records from `from` on: `f` gives a record's new hold, or
    /// None when it ends.
    pub(super) fn unfreeze(&mut self, from: usize, f: impl Fn(Hold) -> Option<Hold>) {
        crate::cost_scope!("phase", "ownership state");
        let mut w = from;
        for r in from..self.freezes.len() {
            let mut x = self.freezes[r];
            match f(x.hold) {
                Some(h) => {
                    x.hold = h;
                    self.freezes[w] = x;
                    w += 1;
                }
                None => self.vars[x.place.slot as usize].as_mut().unwrap().holds -= 1,
            }
        }
        self.freezes.truncate(w);
    }

    /// The place `n` denotes: a local and its field and index steps (OWN-9). None when
    /// `n` is not a place; Err when it was refused or is not built.
    pub(super) fn place(&mut self, n: NodeId) -> Result<Option<At>, ()> {
        match self.kind(n) {
            NodeKind::Group => self.place(self.kids(n)[0]),
            NodeKind::Path => {
                let p: &'c Program<'a> = self.p;
                let Some(u) = p.resolved(self.at, n) else {
                    self.halt();
                    return Err(());
                };
                let Target::Binder(b) = u.target else {
                    return Ok(None);
                };
                self.capture_slot(b, n);
                let var = match self.vars[b as usize] {
                    Some(var) => var,
                    // An earlier refused pattern or capture did not declare this binder.
                    // Continue checking without manufacturing a second implementation gap.
                    None if self.broken => return Err(()),
                    None => {
                        unreachable!("a resolved place binder is declared before a checked use")
                    }
                };
                self.out.counts.places += 1;
                let start = self.steps.len() as u32;
                let mut at = At {
                    place: Place {
                        slot: b,
                        start,
                        len: 0,
                    },
                    ty: var.ty,
                    shared: false,
                };
                let item = self.item;
                for token in
                    &item.tree.tokens[u.projections.start as usize..u.projections.end as usize]
                {
                    if token.kind == TokenKind::Ident {
                        self.field(&mut at, self.p.index.symbols.text(token.symbol), n)?;
                    }
                }
                Ok(Some(at))
            }
            NodeKind::Projection => {
                let Some(mut at) = self.place(self.kids(n)[0])? else {
                    return Ok(None);
                };
                self.field(&mut at, self.token_text(n), n)?;
                Ok(Some(at))
            }
            NodeKind::Index => {
                let c = self.kids(n);
                let Some(mut at) = self.place(c[0])? else {
                    return Ok(None);
                };
                let (collection, viewed) = self.indexed_type(at.ty);
                let elem = self.index_element(collection, n)?;
                // Index operands are evaluated once, before places are compared (OWN-9).
                let (_, value) = self.expr(c[1], expect(scalar(Scalar::U64)));
                let (s, l) = (at.place.start as usize, at.place.len as usize);
                if s + l != self.steps.len() {
                    self.steps.extend_from_within(s..s + l);
                    at.place.start = (self.steps.len() - l) as u32;
                }
                self.steps.push(PStep::Index { collection, value });
                at.place.len += 1;
                at.shared |= viewed
                    || matches!(self.t.kind(collection), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::FixedSpan));
                at.ty = if viewed && !self.copies(elem) {
                    self.t.mk(TyKind::View(elem))
                } else {
                    elem
                };
                Ok(Some(at))
            }
            _ => Ok(None),
        }
    }
    /// Step `at` to its field `name`; through a view, a field that is not plain reads
    /// as a view (OWN-8).
    pub(super) fn field(&mut self, at: &mut At, name: &str, n: NodeId) -> Result<(), ()> {
        let (inner, view) = match self.t.kind(at.ty) {
            TyKind::View(x) => (*x, true),
            _ => (at.ty, false),
        };
        // A Box, Rc or Arc reads as its payload; a counted payload is shared, so its
        // fields read as views (OWN-8, MEM-14, MEM-15).
        if let TyKind::Adt(d, args) = self.t.kind(inner).clone()
            && let Some(role @ (Role::Box | Role::Rc | Role::Arc)) = self.p.decls[d.index()].role
        {
            let shared = view || role != Role::Box;
            self.steps.push(PStep::Payload);
            at.place.len += 1;
            at.shared |= shared;
            at.ty = if shared {
                self.t.mk(TyKind::View(args[0]))
            } else {
                args[0]
            };
            return self.field(at, name, n);
        }
        // A stable value in region `r`: its stable fields are in `r` unless they name
        // another region (MEM-6).
        let (inner, root) = match self.t.kind(inner) {
            TyKind::Anchored(x, root) => (*x, Some(*root)),
            _ => (inner, None),
        };
        let mut stable = false;
        let found = match self.t.kind(inner).clone() {
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                stable = decl.repr == Repr::Stable;
                match &decl.body {
                    DeclBody::Record(fs) => fs.iter().position(|f| f.name == name).map(|i| {
                        let ft = subst(&mut self.t, fs[i].ty, &args);
                        match root {
                            Some(r) if stable => (i, reroot(&mut self.t, ft, Root::Enclosing, r)),
                            _ => (i, ft),
                        }
                    }),
                    _ => None,
                }
            }
            TyKind::Tuple(xs) => name
                .parse::<usize>()
                .ok()
                .and_then(|i| xs.get(i).map(|&x| (i, x))),
            _ => None,
        };
        let Some((i, ft)) = found else {
            if inner != ERROR {
                self.refuse("LANG-9", n, format!("this type has no field `{name}`"));
            }
            return Err(());
        };
        let ft = self.linked(ft);
        self.steps.push(PStep::Field(i as u32));
        at.place.len += 1;
        at.shared |= view || stable;
        at.ty = if view && !self.copies(ft) && !matches!(self.t.kind(ft), TyKind::View(_)) {
            self.t.mk(TyKind::View(ft))
        } else {
            ft
        };
        Ok(())
    }
    pub(super) fn copies(&self, ty: TyId) -> bool {
        copyable(&self.p.decls, &self.t, ty, &self.params)
    }
    /// Use place `at` at `n` (OWN-5 to OWN-14); its type.
    pub(super) fn use_place(&mut self, at: At, how: Use, n: NodeId) {
        crate::cost_scope!("phase", "ownership state");
        let s = at.place.slot;
        if !self.capture_move(s, how, at.ty, n) {
            return;
        }
        let v = self.var(s);
        let name = self.slot_name(s);
        if self.patterns.guard
            && (matches!(how, Use::Take | Use::Mut(_))
                || matches!(how, Use::Value) && !self.copies(at.ty))
        {
            self.refuse("LANG-17", n, "a match guard cannot move or lend mut");
            return;
        }
        if v.origin == Origin::PatternView && matches!(how, Use::Take) {
            self.refuse("OWN-15", n, "this pattern binding views the scrutinee");
            return;
        }
        match how {
            Use::Value | Use::Take => {
                let copy = match how {
                    Use::Value => self.copies(at.ty),
                    _ => at.place.len > 0 && self.copies(at.ty),
                };
                if copy {
                    self.need_live(s, n);
                    self.check_holds(at.place, Act::Read, n);
                    return;
                }
                if at.place.len > 0 {
                    self.refuse(
                        "OWN-14",
                        n,
                        "only a whole slot is moved; use `replace` or a whole destructure",
                    );
                    return;
                }
                match v.origin {
                    Origin::ViewParam => {
                        // OWN-7: a view parameter of a moving type is never moved.
                        self.refuse("OWN-7", n, format!("`{name}` is a view parameter"));
                        return;
                    }
                    Origin::MutParam => {
                        self.refuse("OWN-11", n, format!("`{name}` is a `mut` parameter"));
                        return;
                    }
                    _ => {}
                }
                self.need_live(s, n);
                if self.check_holds(at.place, Act::Write, n) {
                    self.set(s, Avail::Moved, n);
                    self.moved(s, n);
                }
            }
            Use::View(hold) => {
                self.need_live(s, n);
                if self.check_holds(at.place, Act::Read, n) {
                    self.freeze(at.place, hold);
                }
            }
            Use::Mut(hold) => {
                if !v.origin.writable() {
                    self.refuse("OWN-5", n, format!("`{name}` is not writable"));
                    return;
                }
                self.need_live(s, n);
                if self.check_holds(at.place, Act::Write, n) {
                    self.freeze(at.place, hold);
                }
            }
        }
    }
    /// The address of field place `at` (a local and field steps) and how the value
    /// stored there is carried; a local held as a place is its base. A view on the way
    /// holds its type's own bytes (FORM #56), so its fields are found as the type's.
    pub(super) fn field_address(&mut self, at: At, n: NodeId) -> Option<(AddressId, Rep)> {
        let var = self.var(at.place.slot);
        let mut address = match (var.binding, self.rep(var.ty)) {
            (Binding::Value(Some(v)), Ok(r)) => {
                let id = r.pointee()?;
                self.address(AddressBase::Value(v), &[], PlaceTy::Declared(id), n)
            }
            (Binding::Slot(Some(a)), _) => a,
            _ => return None,
        };
        let mut ty = var.ty;
        for k in at.place.start..at.place.start + at.place.len {
            match self.steps[k as usize] {
                // A compact value's field is found in its blob (MEM-9).
                PStep::Field(i) if self.compact_adt(ty).is_some() => {
                    let header = self.pointer(address, n);
                    (address, ty) = self.compact_field(header, ty, None, i, n)?;
                }
                PStep::Field(i) => {
                    // A resource's or stable value's fields are in the block its
                    // holder points at.
                    if let Ok(Rep::Place(id)) = self.rep(ty)
                        && self.e.by_block(id)
                    {
                        address = self.fields_of(AddressBase::Address(address), id, n);
                    }
                    ty = self.field_ty(ty, i)?;
                    let place = self.place_ty(ty, n)?;
                    address =
                        self.address(AddressBase::Address(address), &[Step::Field(i)], place, n);
                }
                PStep::Index { collection, value } => {
                    let value = value?;
                    ty = match self.t.kind(collection) {
                        TyKind::Array(elem, _) => *elem,
                        TyKind::Adt(_, args) => args[0],
                        _ => unreachable!(),
                    };
                    let place = self.place_ty(ty, n)?;
                    address = self.index_address(address, collection, value, place, n)?;
                }
                PStep::Payload => {
                    let holder = match self.t.kind(ty) {
                        TyKind::View(x) => *x,
                        _ => ty,
                    };
                    let TyKind::Adt(_, args) = self.t.kind(holder) else {
                        unreachable!("a payload step follows a Box, Rc or Arc")
                    };
                    ty = args[0];
                    let place = self.place_ty(ty, n)?;
                    address = self.address(
                        AddressBase::Address(address),
                        &[Step::Field(0), Step::Deref],
                        place,
                        n,
                    );
                }
                PStep::Any => unreachable!("payload overlap marker is not an emitted place"),
            }
            if let Some(place @ PlaceTy::Declared(_)) = self.place_ty(ty, n) {
                let pointer = self.pointer(address, n);
                address = self.address(AddressBase::Value(pointer), &[], place, n);
            }
        }
        Some((address, self.rep(ty).ok()?))
    }
    /// The place of row `id` that the view stored at `address` points at.
    pub(super) fn read_bits(&mut self, address: AddressId, t: Ty, n: NodeId) -> ValueId {
        let result = self.value(t);
        self.inst(
            Inst::Read {
                result,
                address,
                mode: ReadMode::Bits,
            },
            n,
        );
        result
    }
    pub(super) fn borrowed_index(&self, at: At) -> bool {
        !self.copies(at.ty)
            && self.steps[at.place.start as usize..(at.place.start + at.place.len) as usize]
                .iter()
                .any(|s| matches!(s, PStep::Index { .. }))
    }
    /// Indexing an evaluated expression retains its temporary owner until the
    /// surrounding expression ends. Plain results are copied before that cleanup.
    pub(super) fn temporary_index(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let (ty, value) = self.expr(c[0], None);
        let (collection, _) = self.indexed_type(ty);
        let Ok(elem) = self.index_element(collection, n) else {
            return (ERROR, None);
        };
        let (_, index) = self.expr(c[1], expect(scalar(Scalar::U64)));
        if !self.copies(elem) {
            return self.gap_ty(n, "a view of an indexed temporary owner is not built");
        }
        let result = (|| {
            let pointer = value?;
            let index = index?;
            let place = self.place_ty(collection, n)?;
            let element = self.place_ty(elem, n)?;
            let base = self.address(AddressBase::Value(pointer), &[], place, n);
            let at = self.index_address(base, collection, index, element, n)?;
            match self.rep(elem).ok()? {
                Rep::None => None,
                Rep::Scalar(t) => Some(self.read_bits(at, t, n)),
                Rep::Place(id) => {
                    let pointer = self.pointer(at, n);
                    Some(self.snapshot(pointer, id, n))
                }
            }
        })();
        (elem, result)
    }
    /// LANG-21's closed index row, shared by named and temporary bases.
    pub(super) fn index_element(&mut self, collection: TyId, n: NodeId) -> Result<TyId, ()> {
        match self.t.kind(collection) {
            TyKind::Array(elem, _) => Ok(*elem),
            TyKind::Adt(d, args)
                if matches!(
                    self.p.decls[d.index()].role,
                    Some(Role::List | Role::FixedSpan)
                ) =>
            {
                Ok(args[0])
            }
            TyKind::Error => Err(()),
            _ => {
                self.refuse("LANG-21", n, "this type has no index operation");
                Err(())
            }
        }
    }
    pub(super) fn indexed_type(&self, mut ty: TyId) -> (TyId, bool) {
        let mut viewed = false;
        while let TyKind::View(inner) | TyKind::Anchored(inner, _) = self.t.kind(ty) {
            ty = *inner;
            viewed = true;
        }
        (ty, viewed)
    }
    pub(super) fn index_address(
        &mut self,
        base: AddressId,
        collection: TyId,
        index: ValueId,
        element: PlaceTy,
        n: NodeId,
    ) -> Option<AddressId> {
        if !self.live() {
            return None;
        }
        if let TyKind::Adt(d, args) = self.t.kind(collection)
            && self.p.decls[d.index()].role == Some(Role::FixedSpan)
        {
            let elem = args[0];
            let header = self.pointer(base, n);
            return self.fixed_span_element(header, elem, index, true, 1, n);
        }
        match self.t.kind(collection) {
            TyKind::Array(..) => Some(self.address(
                AddressBase::Address(base),
                &[Step::Field(0), Step::Element(index)],
                element,
                n,
            )),
            TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::List) => {
                let header = self.pointer(base, n);
                let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
                let condition = self.value(Ty::Scalar(Scalar::Bool));
                self.inst(
                    Inst::Compare {
                        result: condition,
                        op: CompareOp::Lt,
                        left: index,
                        right: len,
                    },
                    n,
                );
                let yes = self.new_block(&[]);
                let no = self.new_block(&[]);
                let ye = self.edge(yes, &[]);
                let ne = self.edge(no, &[]);
                self.end(
                    Terminator::Branch {
                        condition,
                        yes: ye,
                        no: ne,
                    },
                    Some(n),
                );
                self.start(no);
                let cleanup = self.fail_to(n);
                self.end(
                    Terminator::Raise {
                        cause: hirsel_core_ir::RaiseCause::Bounds,
                        cleanup,
                    },
                    Some(n),
                );
                self.start(yes);
                let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
                Some(self.address(AddressBase::Value(data), &[Step::Index(index)], element, n))
            }
            _ => None,
        }
    }
    /// Native Map slots retain insertion order; a removed slot has state 0.
    /// The two metadata words precede the key/value at map_entry's offsets.
    pub(super) fn map_pair(
        &mut self,
        header: ValueId,
        collection: TyId,
        index: ValueId,
        pair: TyId,
        advance: BlockId,
        n: NodeId,
    ) -> Option<ValueId> {
        let entry = self.map_entry_at(header, collection, index, n)?;
        let address = self.address(
            AddressBase::Address(entry),
            &[Step::Field(1)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            n,
        );
        let state = self.read_bits(address, Ty::Scalar(Scalar::U64), n);
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        let condition = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: condition,
                op: CompareOp::Eq,
                left: state,
                right: one,
            },
            n,
        );
        let live = self.new_block(&[]);
        let yes = self.edge(live, &[]);
        let no = self.edge(advance, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
        self.start(live);
        let place = self.place_ty(pair, n)?;
        let to = self.new_slot(place, n);
        let TyKind::Tuple(views) = self.t.kind(pair).clone() else {
            return None;
        };
        // Each half of the pair is a view: the entry's own bytes (FORM #56).
        for (i, field) in [2, 3].into_iter().enumerate() {
            let elem = match self.t.kind(views[i]) {
                TyKind::View(x) => *x,
                _ => views[i],
            };
            let address = self.address(
                AddressBase::Address(entry),
                &[Step::Field(field)],
                PlaceTy::Value(Ty::Scalar(Scalar::U8)),
                n,
            );
            let pointer = self.pointer(address, n);
            let stored = self.rep(elem).ok()?.stored();
            let Some(stored) = stored else {
                continue;
            };
            let field = self.address(
                AddressBase::Address(to),
                &[Step::Field(i as u32)],
                stored,
                n,
            );
            self.install_view(field, elem, pointer, n)?;
        }
        Some(self.pointer(to, n))
    }
    /// FORM's Seq carrier is data/length/capacity; a word read does not copy storage.
    pub(super) fn read_sequence_word(
        &mut self,
        header: ValueId,
        offset: u64,
        ty: Ty,
        n: NodeId,
    ) -> ValueId {
        // Past a gap or a diverging point nothing is emitted, and the value is unused.
        let Some(offset) = self.konst(Ty::Scalar(Scalar::U64), offset, n) else {
            return self.value(ty);
        };
        let address = self.address(
            AddressBase::Value(header),
            &[Step::Byte(offset)],
            PlaceTy::Value(ty),
            n,
        );
        let result = self.value(ty);
        self.inst(
            Inst::Read {
                result,
                address,
                mode: ReadMode::Bits,
            },
            n,
        );
        result
    }
    pub(super) fn field_ty(&mut self, ty: TyId, i: u32) -> Option<TyId> {
        match self.t.kind(self.bare(ty)).clone() {
            TyKind::Adt(d, args) => match &self.p.decls[d.index()].body {
                DeclBody::Record(fs) => Some(subst(&mut self.t, fs[i as usize].ty, &args)),
                _ => None,
            },
            TyKind::Tuple(xs) => Some(xs[i as usize]),
            _ => None,
        }
    }
    pub(super) fn place_ty(&mut self, ty: TyId, n: NodeId) -> Option<PlaceTy> {
        match self.rep(ty) {
            Ok(Rep::None) if ty == UNIT => self
                .e
                .type_row(self.p, &mut self.t, ty)
                .map(PlaceTy::Declared),
            Ok(r) => r.stored(),
            Err(what) => {
                self.gap(n, what);
                None
            }
        }
    }

    /// SEM-9: array elements are evaluated left to right; a repeat evaluates once,
    /// including a zero-length repeat. The expected length is part of its type.
    pub(super) fn array(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let children = self.kids(n);
        let repeat = self.kind(n) == NodeKind::RepeatArray;
        let expected = want.and_then(|w| match self.t.kind(w.ty) {
            TyKind::Array(elem, count) => Some((w.ty, *elem, *count)),
            _ => None,
        });
        let count = if repeat {
            let Some(Num::Int(count)) = self.fold(children[1], Scalar::U64, true) else {
                return (ERROR, None);
            };
            count as u64
        } else {
            children.len() as u64
        };
        if let Some((_, _, required)) = expected
            && count != required
        {
            self.refuse(
                "LANG-11",
                n,
                "the array length differs from its expected type",
            );
            return (ERROR, None);
        }
        if children.is_empty() && expected.is_none() {
            self.refuse("LANG-11", n, "an empty array needs an expected type");
            return (ERROR, None);
        }
        let parts = self.array_parts.len();
        let mut elem = expected.map(|(_, elem, _)| elem);
        let mut result = expected.map(|(ty, _, _)| ty);
        let mut to = None;
        for (i, &child) in children[..if repeat { 1 } else { children.len() }]
            .iter()
            .enumerate()
        {
            let (ty, value) = self.expr(
                child,
                if expected.is_some() {
                    elem.and_then(expect)
                } else {
                    None
                },
            );
            if expected.is_none()
                && let Some(elem) = elem
            {
                self.coerce(
                    ty,
                    Want {
                        ty: elem,
                        view: false,
                        payload: 0,
                    },
                    child,
                );
            }
            let elem = *elem.get_or_insert(ty);
            let array = *result.get_or_insert_with(|| self.t.mk(TyKind::Array(elem, count)));
            if repeat && kind(&self.p.decls, &self.t, elem, &self.params) != Kind::Plain {
                self.refuse("SEM-9", child, "a repeated array element must be plain");
            }
            if to.is_none() {
                to = self
                    .place_ty(array, n)
                    .filter(|_| self.live())
                    .map(|p| self.new_slot(p, n));
            }
            if let (Some(to), Some(place), Ok(rep)) = (to, self.place_ty(elem, n), self.rep(elem)) {
                if repeat {
                    // A compact CFG fills the array; source length never expands the checker work.
                    self.repeat_array(to, place, rep, value, count, n);
                } else if let Some(index) = self.konst(Ty::Scalar(Scalar::U64), i as u64, child) {
                    let address = self.address(
                        AddressBase::Address(to),
                        &[Step::Field(0), Step::Element(index)],
                        place,
                        child,
                    );
                    self.install(address, rep, value, child);
                    // Keep each completed element owned until the array is complete.
                    // Failure cleans those temporaries in reverse construction order.
                    if let Some(value) = value
                        && self.drop_id(elem).is_some()
                    {
                        self.temp(elem, Some(value));
                        self.array_parts.push(value);
                    }
                }
            }
        }
        let ty = result.unwrap_or_else(|| self.t.mk(TyKind::Array(elem.unwrap(), count)));
        if to.is_none()
            && let Some(place) = self.place_ty(ty, n)
            && self.live()
        {
            to = Some(self.new_slot(place, n));
        }
        let value = to.filter(|_| self.live()).map(|a| self.pointer(a, n));
        for i in parts..self.array_parts.len() {
            self.consume(Some(self.array_parts[i]), n);
        }
        self.array_parts.truncate(parts);
        self.temp(ty, value);
        (ty, value)
    }
    fn repeat_array(
        &mut self,
        to: AddressId,
        place: PlaceTy,
        rep: Rep,
        value: Option<ValueId>,
        count: u64,
        n: NodeId,
    ) {
        if !self.live() || count == 0 {
            return;
        }
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n).unwrap();
        let len = self.konst(Ty::Scalar(Scalar::U64), count, n).unwrap();
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n).unwrap();
        let head = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        let body = self.new_block(&[]);
        let out = self.new_block(&[]);
        self.jump(head, &[zero], n);
        self.start(head);
        let index = self.b.blocks[head.index()]
            .params
            .get(&self.b.pools.values)
            .unwrap()[0];
        let condition = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: condition,
                op: CompareOp::Lt,
                left: index,
                right: len,
            },
            n,
        );
        let yes = self.edge(body, &[]);
        let no = self.edge(out, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
        self.start(body);
        let address = self.address(
            AddressBase::Address(to),
            &[Step::Field(0), Step::Element(index)],
            place,
            n,
        );
        self.install(address, rep, value, n);
        let next = self.value(Ty::Scalar(Scalar::U64));
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
        self.jump(head, &[next], n);
        self.start(out);
    }
    /// The address of a used place as a pointer, for an argument passed by address.
    pub(super) fn place_ptr(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        let value = self.place_ptr_inner(at, n);
        self.read_descriptor(at.place.slot, value);
        value
    }
    fn place_ptr_inner(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let a = if at.place.len > 0 {
            self.field_address(at, n)?.0
        } else {
            match self.var(at.place.slot).binding {
                Binding::Value(v) => return v,
                Binding::Slot(a) => a?,
            }
        };
        Some(self.pointer(a, n))
    }
    /// The value of a used place: a local, or a field of one read in place.
    pub(super) fn emit_place(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        let value = self.emit_place_inner(at, n);
        self.read_descriptor(at.place.slot, value);
        value
    }
    fn emit_place_inner(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        if at.place.len > 0 {
            if !self.live() {
                return None;
            }
            return match self.field_address(at, n)? {
                (address, r @ Rep::Scalar(_)) => Some(self.read_bits(address, r.ty().unwrap(), n)),
                // A part reached through a view is read in place, frozen while the
                // view lives (OWN-10).
                (address, Rep::Place(_)) if matches!(self.t.kind(at.ty), TyKind::View(_)) => {
                    Some(self.pointer(address, n))
                }
                (address, Rep::Place(id)) => {
                    let p = self.pointer(address, n);
                    Some(self.snapshot(p, id, n))
                }
                (_, Rep::None) => None,
            };
        }
        let var = self.var(at.place.slot);
        match var.binding {
            Binding::Value(v) => v,
            Binding::Slot(a) => {
                let (Some(address), Ok(r)) = (a, self.rep(var.ty)) else {
                    return None;
                };
                if !self.live() {
                    return None;
                }
                let t = match r {
                    Rep::None => return None,
                    Rep::Scalar(t) => t,
                    // The slot may be written again while the value lives on: the
                    // value is a copy (a move leaves the slot moved out, OWN-6).
                    Rep::Place(id) => {
                        let p = self.pointer(address, n);
                        return Some(self.snapshot(p, id, n));
                    }
                };
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
        }
    }
    /// Close a block: its let-bound views end, then its live owners are dropped, which
    /// a remaining freeze forbids (OWN-10, at the block's closing `}`).
    /// Close block `n` whose value has type `value`. A temporary in its tail ends at its
    /// exit, before its owners are cleaned, unless the value reaches it (LANG-6, OWN-8):
    /// a value of a scoped type may hold a view, so its temporaries stay.
    pub(super) fn close_scope(&mut self, start: usize, freezes: usize, n: NodeId, value: TyId) {
        let d = self.depth;
        let tail = kind(&self.p.decls, &self.t, value, &self.params) != Kind::Scoped;
        self.unfreeze(freezes, |h| {
            let ends = h == Hold::View(Extent::Block(d)) || tail && h == Hold::View(Extent::Temp);
            (!ends).then_some(h)
        });
        if !self.dead {
            for i in start..self.scope_slots.len() {
                let s = self.scope_slots[i];
                let v = self.var(s);
                if matches!(v.avail, Avail::Live | Avail::Maybe) && !self.copies(v.ty) {
                    self.check_holds_at(Place::whole(s), Act::Write, self.close_brace(n));
                }
            }
        }
        self.scope_slots.truncate(start);
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// A typed borrow can follow Box/Rc/Arc payloads, keeping the holder as its
    /// place root. A generic parameter without a selected type keeps its holder.
    fn payload_read_matches(&self, actual: TyId, expected: TyId) -> bool {
        let expected = match self.t.kind(expected) {
            TyKind::View(t) => *t,
            _ => expected,
        };
        actual == expected
            || crate::decls::stable_parameter(&self.p.decls, &self.t, actual, expected)
    }
    pub(super) fn payload_view_at(&mut self, mut at: At, expected: TyId) -> Option<At> {
        if self.payload_read_matches(at.ty, expected) {
            return None;
        }
        let saved = self.steps.len();
        let (start, len) = (at.place.start as usize, at.place.len as usize);
        if start + len != saved {
            self.steps.extend_from_within(start..start + len);
            at.place.start = (self.steps.len() - len) as u32;
        }
        loop {
            let TyKind::Adt(d, args) = self.t.kind(self.bare(at.ty)) else {
                break;
            };
            if !matches!(
                self.p.decls[d.index()].role,
                Some(Role::Box | Role::Rc | Role::Arc)
            ) {
                break;
            }
            let payload = args[0];
            self.steps.push(PStep::Payload);
            at.place.len += 1;
            at.shared = true;
            at.ty = payload;
            if self.payload_read_matches(payload, expected) {
                if self.is_view(expected) {
                    at.ty = expected;
                }
                return Some(at);
            }
        }
        self.steps.truncate(saved);
        None
    }
    /// The same read for a temporary holder. Its own temporary remains live; the
    /// resulting pointer or plain scalar never consumes or cleans the payload.
    pub(super) fn payload_view_value(
        &mut self,
        ty: TyId,
        value: Option<ValueId>,
        expected: TyId,
        n: NodeId,
    ) -> Option<(TyId, Option<ValueId>)> {
        let mut current = ty;
        let mut levels = 0;
        while !self.payload_read_matches(current, expected) {
            let TyKind::Adt(d, args) = self.t.kind(self.bare(current)) else {
                return None;
            };
            if !matches!(
                self.p.decls[d.index()].role,
                Some(Role::Box | Role::Rc | Role::Arc)
            ) {
                return None;
            }
            current = args[0];
            levels += 1;
        }
        if levels == 0 {
            return None;
        }
        let mut result = value;
        let mut current = ty;
        for _ in 0..levels {
            let TyKind::Adt(_, args) = self.t.kind(self.bare(current)) else {
                unreachable!()
            };
            let payload = args[0];
            result = (|| {
                if !self.live() {
                    return None;
                }
                let pointer = result?;
                let place = self.place_ty(current, n)?;
                let element = self.place_ty(payload, n)?;
                let from = self.address(AddressBase::Value(pointer), &[], place, n);
                let to = self.address(
                    AddressBase::Address(from),
                    &[Step::Field(0), Step::Deref],
                    element,
                    n,
                );
                match self.rep(payload).ok()? {
                    Rep::None => None,
                    Rep::Scalar(ty) => Some(self.read_bits(to, ty, n)),
                    Rep::Place(_) => Some(self.pointer(to, n)),
                }
            })();
            current = payload;
        }
        self.reach_temporary(value);
        Some((
            if self.is_view(expected) {
                expected
            } else {
                current
            },
            result,
        ))
    }
}
