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
}
/// A statement temporary: an own value no consumer has taken yet.
#[derive(Clone, Copy)]
pub(super) struct Temp {
    pub value: ValueId,
    pub id: TypeId,
    pub avail: Avail,
    /// Its drop flag, when arms of a match may or may not take it (OWN-18).
    pub flag: Option<AddressId>,
}
/// How a slot that owns a value is cleaned: its place, its row and its drop flag.
#[derive(Clone, Copy)]
pub(super) struct Own {
    pub address: AddressId,
    pub id: TypeId,
    pub flag: AddressId,
}

impl<'c, 'a> Cx<'c, 'a> {
    /// The row of a value of `ty` that needs cleanup.
    pub(super) fn drop_id(&mut self, ty: TyId) -> Option<TypeId> {
        match self.rep(ty) {
            Ok(Rep::Place(id)) if self.e.needs_drop(id) => Some(id),
            _ => None,
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
                id,
                avail: Avail::Live,
                flag: None,
            }));
            self.changed();
        }
    }
    /// The index of the live temporary `v`, if it is one.
    pub(super) fn temp_index(&self, v: Option<ValueId>) -> Option<usize> {
        let v = v?;
        self.owners
            .iter()
            .rposition(|o| matches!(o, Owner::Temp(t) if t.value == v && t.avail == Avail::Live))
    }
    /// A consumer takes `v`: when it is a temporary, nothing cleans it any more.
    pub(super) fn consume(&mut self, v: Option<ValueId>, n: NodeId) {
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
    /// Slot `s` is moved out or taken apart: clear its flag.
    pub(super) fn moved(&mut self, s: u32, n: NodeId) {
        if let Some(own) = self.var(s).own {
            self.store_flag(own.flag, false, n);
        }
    }
    /// Slot `s` was installed whole: set its flag.
    pub(super) fn installed(&mut self, s: u32, n: NodeId) {
        if let Some(own) = self.var(s).own {
            self.store_flag(own.flag, true, n);
        }
    }
    /// OWN-12: before an install into whole slot `s`, its old value is cleaned when
    /// it is live, or maybe with its flag set (T3.2, T3.3).
    pub(super) fn drop_old(&mut self, s: u32, n: NodeId) {
        self.clean(Owner::Slot(s), n);
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
        if !self.live() {
            return;
        }
        let (base, id, avail, flag) = match o {
            Owner::Slot(s) => {
                let v = self.var(s);
                let Some(own) = v.own else { return };
                (
                    AddressBase::Address(own.address),
                    own.id,
                    v.avail,
                    Some(own.flag),
                )
            }
            Owner::Temp(t) => (AddressBase::Value(t.value), t.id, t.avail, t.flag),
        };
        match (avail, flag) {
            (Avail::Live, _) => self.emit_drop(base, id, n),
            (Avail::Maybe, Some(flag)) => {
                let on = self.value(Ty::Scalar(Scalar::Bool));
                self.inst(
                    Inst::Read {
                        result: on,
                        address: flag,
                        mode: ReadMode::Bits,
                    },
                    n,
                );
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
                self.emit_drop(base, id, n);
                self.jump(next, &[], n);
                self.start(next);
            }
            _ => {}
        }
    }
    fn emit_drop(&mut self, base: AddressBase, id: TypeId, n: NodeId) {
        let address = match base {
            AddressBase::Address(a) => a,
            base => self.address(base, &[], PlaceTy::Declared(id), n),
        };
        self.inst(
            Inst::Drop {
                address,
                ty: id,
                fail: None,
            },
            n,
        );
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
        if self.owners.len() == mark {
            return;
        }
        let mut kept = std::mem::take(&mut self.owner_buf);
        for i in (mark..self.owners.len()).rev() {
            let o = self.owners[i];
            match o {
                Owner::Temp(t) if Some(t.value) == keep && t.avail == Avail::Live => kept.push(o),
                Owner::Slot(_) if temps => kept.push(o),
                _ => self.clean(o, n),
            }
        }
        self.owners.truncate(mark);
        self.owners.extend(kept.drain(..).rev());
        self.owner_buf = kept;
        self.changed();
    }
    /// An exit edge's cleanup, which changes no state: the owners begun since
    /// `owners` and the region and limit scopes opened since `scopes`, innermost
    /// first, each scope closing after its owners are cleaned (FAIL-8, T5.3).
    pub(super) fn unwind(&mut self, owners: usize, scopes: usize, n: NodeId) {
        if !self.live() {
            return;
        }
        let mut top = self.owners.len();
        for s in (scopes..self.patterns_len()).rev() {
            let at = self.scope_owners(s).max(owners).min(top);
            for i in (at..top).rev() {
                self.clean(self.owners[i], n);
            }
            top = at;
            self.pattern_scope_exits_one(s, n);
        }
        for i in (owners..top).rev() {
            self.clean(self.owners[i], n);
        }
    }
    /// The failure successor of an instruction emitted here: a block that cleans every
    /// owner and scope of the body, then propagates; none when there is nothing to
    /// clean (FORM v1a: without one the failure leaves at once).
    pub(super) fn fail_to(&mut self, n: NodeId) -> Option<BlockId> {
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
        });
        if !any && !self.open_scopes() {
            return None;
        }
        let cur = self.cur.take();
        let open = std::mem::take(&mut self.open);
        let anchors = std::mem::take(&mut self.open_anchors);
        let blk = self.new_block(&[]);
        self.start(blk);
        self.unwind(0, 0, n);
        self.end(Terminator::Propagate, Some(n));
        self.cur = cur;
        self.open = open;
        self.open_anchors = anchors;
        self.fail_cache = Some((state, blk));
        Some(blk)
    }
}

impl<'c, 'a> Cx<'c, 'a> {
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
        for i in (mark..self.owners.len()).rev() {
            if let o @ Owner::Temp(_) = self.owners[i] {
                self.clean(o, n);
            }
        }
    }
}
