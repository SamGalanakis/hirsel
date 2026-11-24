//! Cleanup of own values (FAIL-8, OWN-18, LANG-6): the owners of a body in the order
//! they began, each a slot of the table or a statement temporary. A scope's exit
//! cleans its owners in reverse; an exit edge cleans every owner and closes every
//! region and limit scope it leaves, innermost first; a failure successor does the
//! same for the whole body and propagates.
use super::*;

/// One owner, in the order owners began.
#[derive(Clone, Copy)]
pub(super) enum Owner {
    /// A slot of the table that owns its value (its `own` row).
    Slot(u32),
    Temp(Temp),
    /// Remaining elements of a consuming List. Its ordinary owner retains the
    /// original allocation with length zero, and is cleaned after this suffix.
    Drain {
        data: ValueId,
        next: AddressId,
        end: ValueId,
        elem: TypeId,
    },
    /// A destructured resource's block, freed with its charge once its omitted
    /// fields are cleaned (T3.7).
    Free {
        block: ValueId,
        charge: ValueId,
    },
}
/// A statement temporary: an own value no consumer has taken yet.
#[derive(Clone, Copy)]
pub(super) struct Temp {
    pub value: ValueId,
    pub source: NodeId,
    pub id: TypeId,
    pub avail: Avail,
    /// Its drop flag, when arms of a match may or may not take it (OWN-18).
    pub flag: Option<AddressId>,
    /// Zero for an ordinary temporary; otherwise its reached-owner arena row.
    pub reached: u32,
}
/// What the cleanup being emitted has already done on its edge, for the failure block
/// of a drop that runs code (FAIL-9, FAIL-12): owners from `from` on are cleaned,
/// except those the edge keeps (the value `keep`, and slots when `temps`); the scopes
/// from `scopes` on are closed; `pending` is the edge's pending result.
#[derive(Clone, Copy)]
pub(super) struct Edge {
    pub from: usize,
    pub keep: Option<ValueId>,
    pub temps: bool,
    pub scopes: usize,
    pub pending: Option<(AddressBase, TypeId)>,
    pub retained: PoolRange<usize>,
}

/// An old value moved aside to be finalized after an install (T3.2), and the value of
/// its drop flag when it was maybe live.
#[derive(Clone, Copy)]
pub(super) struct Saved {
    address: AddressId,
    id: TypeId,
    flag: Option<ValueId>,
}

/// How a slot that owns a value is cleaned: its place, its row and its drop flag.
#[derive(Clone, Copy)]
pub(super) struct Own {
    pub address: AddressId,
    pub id: TypeId,
    pub flag: AddressId,
}

/// An owner whose storage sits in its body's frame.
#[derive(Clone, Copy)]
enum Frame {
    List(frame_lists::FrameList),
    /// A log node (frame_logs.rs), with its entry type.
    Log(frame_logs::FrameLog),
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// The row of a value of `ty` that needs cleanup.
    pub(super) fn drop_id(&mut self, ty: TyId) -> Option<TypeId> {
        crate::cost_scope!("phase", "cleanup emission");
        if ty <= HOLE {
            return None;
        }
        // A view is its type's bytes, a copy that nobody cleans (OWN-8).
        if self.is_view(ty) {
            return None;
        }
        // Only a symbolic row's placeholder can invent cleanup for a plain
        // parameter. Closed rows already have their actual cleanup recipe.
        match self.rep(ty) {
            Ok(Rep::Place(id))
                if self.e.needs_drop(id)
                    && (!self.e.generic_names.contains_key(&id)
                        || self.type_kind(ty) != Kind::Plain
                        || self.e.runs_known_code(id)) =>
            {
                Some(id)
            }
            _ => None,
        }
    }
    /// A value of `ty` is a view: carried as its type, never cleaned (FORM #56).
    pub(super) fn is_view(&self, mut ty: TyId) -> bool {
        loop {
            match self.t.kind(ty) {
                TyKind::Anchored(x, _) => ty = *x,
                TyKind::View(_) => return true,
                _ => return false,
            }
        }
    }
    pub(super) fn owner_mark(&self) -> usize {
        self.owners.len()
    }
    fn changed(&mut self) {
        self.epoch += 1;
    }

    /// `v`, a fresh own value of `ty`, is a temporary until a consumer takes it.
    pub(super) fn temp(&mut self, ty: TyId, v: Option<ValueId>) {
        if let (Some(value), true) = (v, self.live())
            && let Some(id) = self.drop_id(ty)
        {
            self.owners.push(Owner::Temp(Temp {
                value,
                source: self.node,
                id,
                avail: Avail::Live,
                flag: None,
                reached: 0,
            }));
            self.changed();
        }
    }
    /// The index of the live temporary `v`, if it is one.
    pub(super) fn temp_index(&self, v: Option<ValueId>) -> Option<usize> {
        let v = v?;
        self.owners
            .iter()
            .rposition(|o| matches!(o, Owner::Temp(t) if t.value == v && matches!(t.avail,Avail::Live|Avail::Maybe)))
    }
    /// A consumer takes `v`: when it is a temporary, nothing cleans it any more.
    pub(super) fn consume(&mut self, v: Option<ValueId>, n: NodeId) {
        crate::cost_scope!("phase", "cleanup emission");
        if let Some(i) = self.temp_index(v) {
            self.take_temp(i, n);
        }
    }
    pub(super) fn take_temp(&mut self, i: usize, n: NodeId) {
        let Owner::Temp(t) = &mut self.owners[i] else {
            return;
        };
        t.avail = Avail::Moved;
        let flag = t.flag;
        self.changed();
        if let Some(flag) = flag {
            self.store_flag(flag, false, n);
        }
    }
    pub(super) fn set_temp(&mut self, i: usize, avail: Avail) {
        if let Owner::Temp(t) = &mut self.owners[i] {
            t.avail = avail;
        }
        self.changed();
    }
    /// Give temporary `i` a drop flag, set (OWN-18).
    pub(super) fn flag_temp(&mut self, i: usize, n: NodeId) {
        let flag = self.new_flag(true, n);
        if let Owner::Temp(t) = &mut self.owners[i] {
            t.flag = flag;
        }
    }

    /// Slot `b`, just declared or bound, owns its value when its origin does and its
    /// type needs cleanup: it is cleaned at its scope's exit, under a drop flag.
    pub(super) fn own(&mut self, b: u32, n: NodeId) {
        crate::cost_scope!("phase", "cleanup emission");
        let v = self.var(b);
        if v.own.is_some()
            || !matches!(v.origin, Origin::Let | Origin::LetMut | Origin::TakeParam)
            || !self.live()
        {
            return;
        }
        let Some(id) = self.drop_id(v.ty) else {
            return;
        };
        let address = match v.binding {
            Binding::Value(Some(p)) => {
                self.address(AddressBase::Value(p), &[], PlaceTy::Declared(id), n)
            }
            Binding::Slot(Some(a)) => a,
            _ => return,
        };
        let Some(flag) = self.new_flag(v.avail == Avail::Live, n) else {
            return;
        };
        self.vars[b as usize].as_mut().unwrap().own = Some(Own { address, id, flag });
        self.owners.push(Owner::Slot(b));
        self.changed();
    }
    /// FORM #114: whether an immutable `let` of `ty` initialised by `value` holds
    /// nothing a drop would free, so it gets no owner and no drop. True only when
    /// `ty` is `String` or an inline array (nested) of `String`, and `value` is the
    /// same tree written as literals: a string literal or constant (static carrier, capacity
    /// 0, MEM-24 charges nothing), or an `[a, b, ..]` array literal of such trees.
    /// A `let` is never written, lent `mut`, or installed into (OWN-5, OWN-12), so
    /// the capacity stays 0; a value moved out of it is dropped by its new owner.
    /// Anything else (a `let mut`, a parameter, a repeat array, a call, a List
    /// literal, a parenthesised or typed expression) keeps its drop.
    pub(super) fn static_owned(&self, value: NodeId, ty: TyId) -> bool {
        match self.t.kind(ty) {
            TyKind::Array(elem, _) => {
                let elem = *elem;
                self.kind(value) == NodeKind::Array
                    && self.kids(value).iter().all(|&k| self.static_owned(k, elem))
            }
            TyKind::Adt(d, args) => {
                args.is_empty()
                    && self.p.decls[d.index()].role == Some(Role::String)
                    && match self.kind(value) {
                        NodeKind::Literal => self.token_kind(value) == Some(TokenKind::String),
                        NodeKind::Path => self.p.resolved(self.at, value).is_some_and(|use_| {
                            use_.projections.is_empty()
                                && matches!(use_.target, Target::Definition(id)
                                    if self.p.constant_ids[id.0 as usize].is_some())
                        }),
                        _ => false,
                    }
            }
            _ => false,
        }
    }
    /// Slot `s` is moved out or taken apart: clear its flag.
    pub(super) fn moved(&mut self, s: u32, n: NodeId) {
        crate::cost_scope!("phase", "cleanup emission");
        if let Some(own) = self.var(s).own {
            self.store_flag(own.flag, false, n);
        }
    }
    /// Slot `s` was installed whole: set its flag.
    pub(super) fn installed(&mut self, s: u32, n: NodeId) {
        crate::cost_scope!("phase", "cleanup emission");
        if let Some(own) = self.var(s).own {
            self.store_flag(own.flag, true, n);
        }
    }
    /// OWN-12: before an install into whole slot `s`, its old value is cleaned when
    /// it is live, or maybe with its flag set (T3.2, T3.3).
    pub(super) fn drop_old(&mut self, s: u32, n: NodeId) {
        self.clean(Owner::Slot(s), n);
    }

    /// T3.2: before an install into a place whose old value runs code when cleaned,
    /// move the old value aside, so it is finalized after the install and a failing
    /// finalizer leaves the place holding the new value. None when the old value is
    /// cleaned before the install as usual; `Some(None)` when there is none.
    pub(super) fn save_old(
        &mut self,
        q: u32,
        whole: bool,
        address: AddressId,
        ty: TyId,
        n: NodeId,
    ) -> Option<Option<Saved>> {
        let id = self.drop_id(ty)?;
        if !self.live() || !self.e.runs_code(id) {
            return None;
        }
        let v = self.var(q);
        let flag = match (whole, v.own) {
            (true, Some(own)) => match v.avail {
                Avail::Live => None,
                Avail::Maybe => {
                    let on = self.value(Ty::Scalar(Scalar::Bool));
                    self.inst(
                        Inst::Read {
                            result: on,
                            address: own.flag,
                            mode: ReadMode::Bits,
                        },
                        n,
                    );
                    Some(on)
                }
                _ => return Some(None),
            },
            _ if v.avail == Avail::Live => None,
            _ => return Some(None),
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let bytes = self.layout_size(ty, false, n)?;
        self.inst(
            Inst::Copy {
                to,
                from: address,
                bytes,
            },
            n,
        );
        Some(Some(Saved {
            address: to,
            id,
            flag,
        }))
    }
    /// Clean the old value [`Self::save_old`] moved aside.
    pub(super) fn finalize_old(&mut self, saved: Saved, n: NodeId) {
        if !self.live() {
            return;
        }
        let base = AddressBase::Address(saved.address);
        let Some(on) = saved.flag else {
            self.emit_drop(base, saved.id, n);
            return;
        };
        let yes = self.new_block(&[]);
        let next = self.new_block(&[]);
        let (ye, ne) = (self.edge(yes, &[]), self.edge(next, &[]));
        self.end(
            Terminator::Branch {
                condition: on,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(yes);
        self.emit_drop(base, saved.id, n);
        self.jump(next, &[], n);
        self.start(next);
    }

    fn new_flag(&mut self, on: bool, n: NodeId) -> Option<AddressId> {
        if !self.live() {
            return None;
        }
        let a = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::Bool)), n);
        self.store_flag(a, on, n);
        Some(a)
    }
    fn store_flag(&mut self, address: AddressId, on: bool, n: NodeId) {
        if let Some(value) = self.konst(Ty::Scalar(Scalar::Bool), u64::from(on), n) {
            self.inst(Inst::Store { address, value }, n);
        }
    }

    /// Clean one owner by its state: live is cleaned, maybe is cleaned when its flag
    /// is set, moved or uninit is not touched (FAIL-8, OWN-18).
    fn clean(&mut self, o: Owner, n: NodeId) {
        crate::cost_scope!("phase", "cleanup emission");
        if !self.live() {
            return;
        }
        let mut frame = None;
        let (base, id, avail, flag) = match o {
            Owner::Slot(s) => {
                let v = self.var(s);
                let Some(own) = v.own else { return };
                frame = self
                    .frame_list_at(own.address)
                    .map(Frame::List)
                    .or_else(|| {
                        self.frame_log_owners
                            .get(&s)
                            .map(|&frame| Frame::Log(frame))
                    });
                (
                    AddressBase::Address(own.address),
                    own.id,
                    v.avail,
                    Some(own.flag),
                )
            }
            Owner::Temp(t) => {
                // A taken log remains an owner until log.finish consumes it.
                frame = self.frame_logs.get(&t.value).copied().map(Frame::Log);
                (
                    self.temporary_storage(t)
                        .map_or(AddressBase::Value(t.value), AddressBase::Address),
                    t.id,
                    t.avail,
                    t.flag,
                )
            }

            Owner::Drain {
                data,
                next,
                end,
                elem,
            } => {
                self.clean_drain(data, next, end, elem, n);
                return;
            }
            Owner::Free { block, charge } => {
                self.inst(Inst::Free { block, charge }, n);
                return;
            }
        };
        match (avail, flag) {
            (Avail::Live, _) => self.drop_owner(base, id, frame, n),
            (Avail::Maybe, Some(flag)) => {
                let on = if let Some(Frame::Log(frame)) = frame {
                    self.frame_active(frame.storage, n)
                } else {
                    self.read_bits(flag, Ty::Scalar(Scalar::Bool), n)
                };
                let yes = self.new_block(&[]);
                let next = self.new_block(&[]);
                let (ye, ne) = (self.edge(yes, &[]), self.edge(next, &[]));
                self.end(
                    Terminator::Branch {
                        condition: on,
                        yes: ye,
                        no: ne,
                    },
                    Some(n),
                );
                self.start(yes);
                if matches!(o,Owner::Temp(t) if self.temporary_storage(t).is_some()) {
                    self.store_flag(flag, false, n);
                }
                self.drop_owner(base, id, frame, n);
                self.jump(next, &[], n);
                self.start(next);
            }
            _ => {}
        }
    }
    /// When `ty` is a resource destructured from `value`, its block (its storage,
    /// unused cell and grant) is an owner, freed and refunded when cleaned (T3.7); its
    /// fields went on with their blocks.
    pub(super) fn free_owner(&mut self, value: Option<ValueId>, ty: TyId, n: NodeId) {
        if let (Some(value), Ok(Rep::Place(id))) = (value, self.rep(ty))
            && self.e.is_resource(id)
            && self.live()
        {
            let holder = self.address(AddressBase::Value(value), &[], PlaceTy::Value(Ty::Ptr), n);
            let block = self.read_bits(holder, Ty::Ptr, n);
            if let Some(charge) = self.e.resource_charge(id)
                && let Some(charge) = self.konst(Ty::Scalar(Scalar::U64), charge, n)
            {
                self.owners.push(Owner::Free { block, charge });
                self.changed();
            }
        }
    }
    pub(super) fn drain_owner(&mut self, data: ValueId, next: AddressId, end: ValueId, elem: TyId) {
        if let Some(elem) = self.drop_id(elem) {
            self.owners.push(Owner::Drain {
                data,
                next,
                end,
                elem,
            });
            self.changed();
        }
    }
    fn clean_drain(
        &mut self,
        data: ValueId,
        next: AddressId,
        end: ValueId,
        elem: TypeId,
        n: NodeId,
    ) {
        let head = self.new_block(&[]);
        let body = self.new_block(&[]);
        let out = self.new_block(&[]);
        self.jump(head, &[], n);
        self.start(head);
        let index = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Read {
                result: index,
                address: next,
                mode: ReadMode::Bits,
            },
            n,
        );
        let condition = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: condition,
                op: CompareOp::Lt,
                left: index,
                right: end,
            },
            n,
        );
        let yes = self.edge(body, &[]);
        let no = self.edge(out, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
        self.start(body);
        let address = self.address(
            AddressBase::Value(data),
            &[Step::Index(index)],
            PlaceTy::Declared(elem),
            n,
        );
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n).unwrap();
        let after = self.value(Ty::Scalar(Scalar::U64));
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
                address: next,
                value: after,
            },
            n,
        );
        // Advanced first: a failed element's drop resumes after it (FAIL-12).
        self.emit_drop(AddressBase::Address(address), elem, n);
        self.jump(head, &[], n);
        self.start(out);
    }
    fn drop_owner(&mut self, base: AddressBase, id: TypeId, frame: Option<Frame>, n: NodeId) {
        match (frame, base) {
            (Some(Frame::List(f)), AddressBase::Address(address)) => {
                self.drop_frame_list(f, address, id, n);
            }
            (Some(Frame::Log(frame)), _) => {
                let _ = self.drop_frame_log(frame, n);
            }
            _ => self.emit_drop(base, id, n),
        }
    }
    pub(super) fn emit_drop(&mut self, base: AddressBase, id: TypeId, n: NodeId) {
        self.emit_drop_with_refund(base, id, None, n);
    }

    pub(super) fn emit_drop_with_refund(
        &mut self,
        base: AddressBase,
        id: TypeId,
        refund: Option<AddressId>,
        n: NodeId,
    ) {
        crate::cost_scope!("phase", "cleanup emission");
        let address = match base {
            AddressBase::Address(a) => a,
            base => self.address(base, &[], PlaceTy::Declared(id), n),
        };
        if !self.e.runs_code(id) {
            self.inst(
                Inst::Drop {
                    address,
                    ty: id,
                    fail: None,
                },
                n,
            );
            if let Some(storage) = refund {
                self.inst(Inst::FrameRefund { storage }, n);
            }
            return;
        }
        // FAIL-12: a failed drop has cleaned its value's fields. During failure
        // cleanup its cause is appended and cleanup goes on (FAIL-10); on a normal
        // edge it starts propagation, which cleans the rest (FAIL-9).
        let fail = if self.failing {
            self.new_block(&[])
        } else {
            self.drop_failure(refund, n)
        };
        self.inst(
            Inst::Drop {
                address,
                ty: id,
                fail: Some(fail),
            },
            n,
        );
        if self.failing {
            self.jump(fail, &[], n);
            self.start(fail);
        }
        if let Some(storage) = refund {
            self.inst(Inst::FrameRefund { storage }, n);
        }
    }
    /// The failure block of a drop on a normal edge: the edge's pending result, then
    /// every owner and scope the edge has not cleaned, innermost first, then propagate.
    fn drop_failure(&mut self, refund: Option<AddressId>, n: NodeId) -> BlockId {
        let edge = self.edge.unwrap_or(Edge {
            from: self.owners.len(),
            keep: None,
            temps: false,
            scopes: self.patterns_len(),
            pending: None,
            retained: PoolRange::EMPTY,
        });
        let cur = self.cur.take();
        let open = std::mem::take(&mut self.open);
        let anchors = std::mem::take(&mut self.open_anchors);
        let saved = std::mem::replace(&mut self.failing, true);
        let blk = self.new_block(&[]);
        self.start(blk);
        if let Some(storage) = refund {
            self.inst(Inst::FrameRefund { storage }, n);
        }
        if let Some((base, id)) = edge.pending {
            self.emit_drop(base, id, n);
        }
        let done = |cx: &Self, i: usize| match cx.owners[i] {
            // A drain resumes where it stopped, and ends at once when it finished.
            Owner::Drain { .. } => false,
            Owner::Temp(t) if Some(t.value) == edge.keep => edge.pending.is_some(),
            Owner::Slot(_) if edge.temps => false,
            Owner::Temp(t) if cx.temporary_held(t, edge.temps) => false,
            _ if edge.retained.get(&cx.retained_owners).unwrap().contains(&i) => false,
            _ => i >= edge.from,
        };
        let mut top = self.owners.len();
        for s in (0..edge.scopes).rev() {
            let at = self.scope_owners(s).min(top);
            for i in (at..top).rev() {
                if !done(self, i) {
                    self.clean(self.owners[i], n);
                }
            }
            top = at;
            self.pattern_scope_exits_one(s, n);
        }
        for i in (0..top).rev() {
            if !done(self, i) {
                self.clean(self.owners[i], n);
            }
        }
        self.end(Terminator::Propagate, Some(n));
        self.failing = saved;
        self.cur = cur;
        self.open = open;
        self.open_anchors = anchors;
        blk
    }
    /// Drop the live value of type `ty` at `address`, when it needs cleanup.
    pub(super) fn drop_at(&mut self, address: AddressId, ty: TyId, n: NodeId) {
        if self.live()
            && let Some(id) = self.drop_id(ty)
        {
            self.emit_drop(AddressBase::Address(address), id, n);
        }
    }
    /// Drop the field at `address`, of type `ty`, when it needs cleanup.
    pub(super) fn drop_field(&mut self, value: Option<ValueId>, ty: TyId, n: NodeId) {
        if let (Some(value), true) = (value, self.live())
            && let Some(id) = self.drop_id(ty)
        {
            self.emit_drop(AddressBase::Value(value), id, n);
        }
    }

    /// End the owners begun since `mark`, in reverse: temporaries always (except the
    /// value `keep` that reaches the enclosing expression), slots unless `temps`.
    pub(super) fn end_owners(
        &mut self,
        mark: usize,
        keep: Option<ValueId>,
        temps: bool,
        n: NodeId,
    ) {
        if self.owners.len() != mark {
            self.end_reached_owners::<false>(mark, keep, temps, 0, n);
        }
    }
    pub(super) fn end_reached_owners<const REACHED: bool>(
        &mut self,
        mark: usize,
        keep: Option<ValueId>,
        temps: bool,
        reach: anchors::Fact,
        n: NodeId,
    ) {
        crate::cost_scope!("phase", "cleanup emission");
        if self.owners.len() == mark {
            return;
        }
        let mut kept = std::mem::take(&mut self.owner_buf);
        let retained_start = self.retained_owners.len();
        let retained = if REACHED {
            self.retain_anchor_owners(reach);
            PoolRange::new(
                retained_start as u32,
                (self.retained_owners.len() - retained_start) as u32,
            )
        } else {
            PoolRange::EMPTY
        };
        let saved = self.edge;
        let pending = keep.and_then(|v| {
            let i = self.temp_index(Some(v))?;
            let Owner::Temp(t) = self.owners[i] else {
                return None;
            };
            (i >= mark && self.e.runs_code(t.id)).then_some((AddressBase::Value(v), t.id))
        });
        for i in (mark..self.owners.len()).rev() {
            let o = self.owners[i];
            match o {
                Owner::Temp(t) if Some(t.value) == keep && t.avail == Avail::Live => kept.push(o),
                Owner::Slot(_) if temps => kept.push(o),
                Owner::Temp(t) if self.temporary_held(t, temps) => kept.push(o),
                _ if REACHED && retained.get(&self.retained_owners).unwrap().contains(&i) => {
                    kept.push(o)
                }
                _ => {
                    self.edge = Some(Edge {
                        from: i,
                        keep,
                        temps,
                        scopes: self.patterns_len(),
                        pending,
                        retained,
                    });
                    self.clean(o, n);
                }
            }
        }
        self.edge = saved;
        self.owners.truncate(mark);
        self.owners.extend(kept.drain(..).rev());
        self.owner_buf = kept;
        if REACHED {
            self.retained_owners.truncate(retained_start);
        }
        self.changed();
    }
    /// An exit edge's cleanup, which changes no state: the owners begun since
    /// `owners` and the region and limit scopes opened since `scopes`, innermost
    /// first, each scope closing after its owners are cleaned (FAIL-8, T5.3).
    pub(super) fn unwind(&mut self, owners: usize, scopes: usize, n: NodeId) {
        self.unwind_keeping(owners, scopes, PoolRange::EMPTY, n);
    }
    /// As `unwind`, leaving the owners at the `retained` indexes of `retained_owners`
    /// uncleaned: a `break` value's reached temporaries, carried to its frame's exit.
    pub(super) fn unwind_keeping(
        &mut self,
        owners: usize,
        scopes: usize,
        retained: PoolRange<usize>,
        n: NodeId,
    ) {
        crate::cost_scope!("phase", "cleanup emission");
        if !self.live() {
            return;
        }
        let saved = self.edge;
        let pending = self.pending.take();
        let mut top = self.owners.len();
        for s in (scopes..self.patterns_len()).rev() {
            let at = self.scope_owners(s).max(owners).min(top);
            for i in (at..top).rev() {
                if retained.get(&self.retained_owners).unwrap().contains(&i) {
                    continue;
                }
                self.edge = Some(Edge {
                    from: i,
                    keep: None,
                    temps: false,
                    scopes: s + 1,
                    pending,
                    retained,
                });
                self.clean(self.owners[i], n);
            }
            top = at;
            self.pattern_scope_exits_one(s, n);
        }
        for i in (owners..top).rev() {
            if retained.get(&self.retained_owners).unwrap().contains(&i) {
                continue;
            }
            self.edge = Some(Edge {
                from: i,
                keep: None,
                temps: false,
                scopes,
                pending,
                retained,
            });
            self.clean(self.owners[i], n);
        }
        self.edge = saved;
    }
    /// The failure successor of an instruction emitted here: a block that cleans every
    /// owner and scope of the body, then propagates; none when there is nothing to
    /// clean (FORM v1a: without one the failure leaves at once).
    pub(super) fn fail_to(&mut self, n: NodeId) -> Option<BlockId> {
        crate::cost_scope!("phase", "cleanup emission");
        if !self.live() {
            return None;
        }
        let state = (self.epoch, self.owners.len(), self.patterns_len());
        if let Some((s, b)) = self.fail_cache
            && s == state
        {
            return Some(b);
        }
        let any = self.owners.iter().any(|&o| match o {
            Owner::Slot(s) => matches!(self.var(s).avail, Avail::Live | Avail::Maybe),
            Owner::Temp(t) => matches!(t.avail, Avail::Live | Avail::Maybe),
            Owner::Drain { .. } | Owner::Free { .. } => true,
        });
        if !any && !self.open_scopes() {
            return None;
        }
        let cur = self.cur.take();
        let open = std::mem::take(&mut self.open);
        let anchors = std::mem::take(&mut self.open_anchors);
        let blk = self.new_block(&[]);
        self.start(blk);
        let saved = std::mem::replace(&mut self.failing, true);
        self.unwind(0, 0, n);
        self.failing = saved;
        self.end(Terminator::Propagate, Some(n));
        self.cur = cur;
        self.open = open;
        self.open_anchors = anchors;
        self.fail_cache = Some((state, blk));
        Some(blk)
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn temp_live(&self, i: usize) -> bool {
        matches!(self.owners[i], Owner::Temp(t) if t.avail == Avail::Live)
    }
    /// Emit `f`, which ends the block, into block `blk` away from the current one.
    pub(super) fn side(&mut self, blk: BlockId, f: impl FnOnce(&mut Self)) {
        if !self.live() {
            return;
        }
        let cur = self.cur.take();
        let open = std::mem::take(&mut self.open);
        let anchors = std::mem::take(&mut self.open_anchors);
        self.start(blk);
        f(self);
        self.cur = cur;
        self.open = open;
        self.open_anchors = anchors;
    }
    /// Clean the live temporaries begun since `mark`, changing no state.
    pub(super) fn clean_temps(&mut self, mark: usize, n: NodeId) {
        let saved = self.edge;
        for i in (mark..self.owners.len()).rev() {
            if let o @ Owner::Temp(_) = self.owners[i] {
                self.edge = Some(Edge {
                    from: i,
                    keep: None,
                    temps: true,
                    scopes: self.patterns_len(),
                    pending: None,
                    retained: PoolRange::EMPTY,
                });
                self.clean(o, n);
            }
        }
        self.edge = saved;
    }
}
