use super::*;

impl<'c, 'a> Cx<'c, 'a> {
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
        self.vars[b as usize] = Some(Var {
            ty,
            binding,
            origin,
            avail: Avail::Uninit,
            at: n,
            born: self.born,
            flagged: false,
            holds: 0,
        });
        self.born += 1;
        self.set(b, Avail::Live, n);
    }
    pub(super) fn set(&mut self, s: u32, a: Avail, n: NodeId) {
        let v = self.vars[s as usize].as_mut().expect("declared slot");
        if v.avail != a {
            self.log.push((s, v.avail, v.at));
            v.avail = a;
            v.at = n;
        }
    }
    pub(super) fn undo(&mut self, mark: usize) {
        while self.log.len() > mark {
            let (s, a, at) = self.log.pop().expect("logged");
            let v = self.vars[s as usize].as_mut().expect("declared slot");
            v.avail = a;
            v.at = at;
        }
    }
    /// The current state of each slot changed since `mark`: one incoming edge of a join.
    pub(super) fn edge_state(&mut self, mark: usize) -> (u32, u32) {
        let start = self.diffs.len() as u32;
        for i in mark..self.log.len() {
            let s = self.log[i].0;
            let a = self.var(s).avail;
            self.diffs.push((s, a));
        }
        (start, self.diffs.len() as u32 - start)
    }
    /// Join `edges` (OWN-16) into the state at `mark`; a slot no edge changed keeps its
    /// state there. With no edge, nothing reaches the join.
    pub(super) fn merge(&mut self, mark: usize, edges: &[(u32, u32)], n: NodeId) {
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
                let (s, a) = self.diffs[k as usize];
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
                    };
                    touched.push(s);
                } else {
                    x.avail = join(x.avail, a);
                    x.count += 1;
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
            let (s, entry, _) = self.log[i];
            if self.acc[s as usize].merge == m {
                continue;
            }
            self.acc[s as usize].merge = m;
            let v = self.var(s);
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
        self.vars[place.slot as usize].as_mut().unwrap().holds += 1;
        self.freezes.push(Freeze { place, hold });
    }
    /// Rewrite or end the records from `from` on: `f` gives a record's new hold, or
    /// None when it ends.
    pub(super) fn unfreeze(&mut self, from: usize, f: impl Fn(Hold) -> Option<Hold>) {
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
                let Some(var) = self.vars[b as usize] else {
                    self.gap(n, "this kind of name is not built");
                    return Err(());
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
                if matches!(self.t.kind(at.ty), TyKind::View(_)) {
                    self.gap(n, "indexing through a view is not built");
                    return Err(());
                }
                let elem = match self.t.kind(at.ty).clone() {
                    TyKind::Adt(d, args) if self.p.decls[d.index()].role == Some(Role::List) => {
                        args[0]
                    }
                    _ => {
                        self.gap(n, "indexing this type is not built");
                        return Err(());
                    }
                };
                // Index operands are evaluated before places are compared (OWN-9).
                let (_, _) = self.expr(c[1], expect(scalar(Scalar::U64)));
                let (s, l) = (at.place.start as usize, at.place.len as usize);
                if s + l != self.steps.len() {
                    self.steps.extend_from_within(s..s + l);
                    at.place.start = (self.steps.len() - l) as u32;
                }
                self.steps.push(PStep::Any);
                at.place.len += 1;
                at.ty = elem;
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
        let mut stable = false;
        let found = match self.t.kind(inner).clone() {
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                stable = decl.repr == Repr::Stable;
                match &decl.body {
                    DeclBody::Record(fs) => fs
                        .iter()
                        .position(|f| f.name == name)
                        .map(|i| (i, subst(&mut self.t, fs[i].ty, &args))),
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
            self.gap(n, "this field access is not built");
            return Err(());
        };
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
        let s = at.place.slot;
        let v = self.var(s);
        let name = self.slot_name(s);
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
    /// The address of field place `at` (a local and field steps) and how its value is
    /// carried; a local held as a place is its base.
    fn field_address(&mut self, at: At, n: NodeId) -> Option<(AddressId, Rep)> {
        let var = self.var(at.place.slot);
        let base = match (var.binding, self.rep(var.ty)) {
            // A pointer names a typed place only through an address with no steps.
            (Binding::Value(Some(v)), Ok(Rep::Place(id))) => {
                let whole = PlaceTy::Declared(id);
                AddressBase::Address(self.address(AddressBase::Value(v), &[], whole, n))
            }
            (Binding::Slot(Some(a)), _) => AddressBase::Address(a),
            _ => return None,
        };
        let (s, l) = (at.place.start as usize, at.place.len as usize);
        let mut steps = Vec::with_capacity(l);
        for &st in &self.steps[s..s + l] {
            match st {
                PStep::Field(i) => steps.push(Step::Field(i)),
                _ => {
                    self.gap(n, "index places are not built");
                    return None;
                }
            }
        }
        let r = match self.rep(at.ty) {
            Ok(r) => r,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let ty = match r {
            Rep::None => return None,
            Rep::Scalar(t) => PlaceTy::Value(t),
            Rep::Place(id) => PlaceTy::Declared(id),
        };
        Some((self.address(base, &steps, ty, n), r))
    }
    /// The address of a used place as a pointer, for an argument passed by address.
    pub(super) fn place_ptr(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        if at.place.len > 0 {
            let (a, _) = self.field_address(at, n)?;
            return Some(self.pointer(a, n));
        }
        match self.var(at.place.slot).binding {
            Binding::Value(v) => v,
            Binding::Slot(a) => Some(self.pointer(a?, n)),
        }
    }
    /// The value of a used place: a local, or a field of one read in place.
    pub(super) fn emit_place(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        if at.place.len > 0 {
            if !self.live() {
                return None;
            }
            return match self.field_address(at, n)? {
                (address, Rep::Scalar(t)) => {
                    let result = self.value(t);
                    let mode = ReadMode::Bits;
                    self.inst(
                        Inst::Read {
                            result,
                            address,
                            mode,
                        },
                        n,
                    );
                    Some(result)
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
