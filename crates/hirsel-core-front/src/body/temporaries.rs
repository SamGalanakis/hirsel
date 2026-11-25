//! Temporary owners carried by scoped block values (LANG-6). Uses the source
//! provenance arena and the existing flagged cleanup machinery. Also the temporary a
//! by-value scalar argument is read into before places are compared (OWN-9).
use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct Reached {
    storage: Option<AddressId>,
    held: Option<u32>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// Copy argument `at` of open call `call` (OWN-7), read first as a scalar (OWN-9).
    pub(super) fn copy_argument(&mut self, at: At, call: u32, n: NodeId) {
        self.scalar_argument(call, at.ty, |cx| cx.use_place(at, Use::Value, n));
    }
    /// Evaluate `eval`, an argument of type `ty` of open call `call`. A scalar, not a
    /// view, is read into a temporary before the call, so it shares no storage with the
    /// call's `mut` arguments: its copies, and the views its own calls take, cross those
    /// loans (OWN-9). A `mut` capture a callable argument reserves (index `u32::MAX`), a
    /// view that outlives the call it is taken for and every write still stop it.
    pub(super) fn scalar_argument<R>(
        &mut self,
        call: u32,
        ty: TyId,
        eval: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let scalar =
            self.copies(ty) && !self.is_view(ty) && matches!(self.rep(ty), Ok(Rep::Scalar(_)));
        if scalar {
            self.scalar_calls.push(call);
        }
        let r = eval(self);
        if scalar {
            self.scalar_calls.pop();
        }
        r
    }
    /// Argument `e` of open call `call`, built at `place` when it has one; a scalar
    /// built in no place is read before the call (OWN-9).
    pub(super) fn argument_into(
        &mut self,
        call: u32,
        e: NodeId,
        w: Want,
        place: Option<AddressId>,
        placed: bool,
    ) -> Option<ValueId> {
        if place.is_some() || placed {
            return self.expr_into(e, Some(w), place).1;
        }
        self.scalar_argument(call, w.ty, |cx| cx.expr_into(e, Some(w), None).1)
    }
    /// Call `call` completes inside a scalar argument: a view argument its result is
    /// anchored to outlives it, so it is compared again with every hold (OWN-9).
    pub(super) fn scalar_kept_views(&mut self, from: usize, call: u32, anchors: &[u32]) {
        if self.scalar_calls.is_empty() {
            return;
        }
        for r in from..self.freezes.len() {
            let f = self.freezes[r];
            if matches!(f.hold, Hold::CallView(c, i) if c == call && anchors.contains(&i)) {
                let site = self.span(f.site);
                self.check_holds_skipping(f.place, Act::Read, site, |j, _| j == r);
            }
        }
    }
    /// `check_holds` for a read: a copy, or a view held only by its call (`crosses`),
    /// crosses the `mut` argument loans of the calls in `scalar_calls`.
    pub(super) fn check_read_holds(&mut self, p: Place, crosses: bool, n: NodeId) -> bool {
        if !crosses || self.scalar_calls.is_empty() {
            return self.check_holds(p, Act::Read, n);
        }
        let calls = std::mem::take(&mut self.scalar_calls);
        let site = self.span(n);
        let ok = self.check_holds_skipping(
            p,
            Act::Read,
            site,
            |_, hold| matches!(hold, Hold::CallMut(c, i) if i != u32::MAX && calls.contains(&c)),
        );
        self.scalar_calls = calls;
        ok
    }
    #[inline(always)]
    pub(super) fn temporary_storage(&self, t: cleanup::Temp) -> Option<AddressId> {
        if t.reached == 0 {
            None
        } else {
            self.reached_temps[t.reached as usize - 1].storage
        }
    }
    #[inline(always)]
    pub(super) fn temporary_held(&self, t: cleanup::Temp, statement: bool) -> bool {
        t.reached != 0
            && self.reached_temps[t.reached as usize - 1]
                .held
                .is_some_and(|d| statement || d < self.depth)
    }
    fn reached_temporary(&mut self, i: usize) -> usize {
        let cleanup::Owner::Temp(t) = self.owners[i] else {
            unreachable!("a reached owner is temporary")
        };
        if t.reached != 0 {
            return t.reached as usize - 1;
        }
        let id = self.reached_temps.len();
        self.reached_temps.push(Reached::default());
        if let cleanup::Owner::Temp(t) = &mut self.owners[i] {
            t.reached = id as u32 + 1;
        }
        id
    }
    pub(super) fn hold_reached(&mut self, fact: anchors::Fact) {
        if fact == 0 {
            return;
        }
        let start = self.retained_owners.len();
        self.retain_anchor_owners(fact);
        for k in start..self.retained_owners.len() {
            let i = self.retained_owners[k];
            let id = self.reached_temporary(i);
            let held = &mut self.reached_temps[id].held;
            *held = Some(held.map_or(self.depth, |d| d.min(self.depth)));
        }
        self.retained_owners.truncate(start);
        self.epoch += 1;
    }
    /// Carry a scoped value out of a block or pattern arm. Each reached owner uses
    /// an ordinary flagged slot, so an unselected arm never reads an SSA value or
    /// cleans an owner that only the other arm created.
    pub(super) fn end_value_owners(
        &mut self,
        mark: usize,
        value: Option<ValueId>,
        ty: TyId,
        n: NodeId,
    ) {
        if self.owners.len() == mark {
            return;
        }
        // The returned expression already published its provenance. Most tails
        // have none, so they need neither a kind query nor reached-owner work.
        let fact = self.anchors.last;
        let reach = if fact != 0 && self.type_kind(ty) == Kind::Scoped {
            fact
        } else {
            0
        };
        if reach == 0 {
            self.end_owners(mark, value, false, n);
            return;
        }
        let start = self.retained_owners.len();
        self.retain_anchor_owners(reach);
        for k in start..self.retained_owners.len() {
            let i = self.retained_owners[k];
            if i >= mark {
                self.spill_temporary(i, n);
            }
        }
        self.retained_owners.truncate(start);
        self.end_reached_owners::<true>(mark, value, false, reach, n);
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn entry_slot(&mut self, ty: PlaceTy, _n: NodeId) -> AddressId {
        self.b.slots.push(ty);
        let slot = SlotId(self.b.slots.len() as u32 - 1);
        self.entry_setup.push(Inst::StorageLive { slot });
        self.b.addresses.push(AddressInfo {
            base: AddressBase::Slot(slot),
            steps: PoolRange::EMPTY,
            ty,
        });
        let address = AddressId(self.b.addresses.len() as u32 - 1);
        self.entry_setup.push(Inst::Address {
            address,
            fail: None,
        });
        address
    }
    /// A constant defined in the entry block, so it dominates every use.
    pub(super) fn entry_const(&mut self, t: Ty, bits: u64) -> ValueId {
        let result = self.value(t);
        self.entry_setup.push(Inst::Const { result, bits });
        result
    }
    /// An address computed in the entry block from values the entry block defines.
    pub(super) fn entry_address(
        &mut self,
        base: AddressBase,
        steps: &[Step],
        ty: PlaceTy,
    ) -> AddressId {
        debug_assert!(!steps.iter().any(|s| matches!(s, Step::Element(_))));
        let steps = PoolRange::push(&mut self.b.pools.steps, steps.iter().copied());
        self.b.addresses.push(AddressInfo { base, steps, ty });
        let address = AddressId(self.b.addresses.len() as u32 - 1);
        self.entry_setup.push(Inst::Address {
            address,
            fail: None,
        });
        address
    }
    /// An entry-block place of `ty` that is rooted at its own row, like a field
    /// place after each step (`field_address`): the field it is projected from may
    /// be typed as a holder (`arc t`, a box) rather than the row its uses project.
    pub(super) fn entry_place(
        &mut self,
        base: AddressBase,
        steps: &[Step],
        ty: PlaceTy,
    ) -> AddressId {
        let address = self.entry_address(base, steps, ty);
        if !matches!(ty, PlaceTy::Declared(_)) {
            return address;
        }
        let result = self.value(Ty::Ptr);
        self.entry_setup.push(Inst::Pointer { result, address });
        self.note_pointer(result, address);
        self.entry_address(AddressBase::Value(result), &[], ty)
    }
    pub(super) fn spill_temporary(&mut self, i: usize, n: NodeId) {
        let cleanup::Owner::Temp(t) = self.owners[i] else {
            return;
        };
        if self.temporary_storage(t).is_some() || !self.live() {
            return;
        }
        let storage = self.entry_slot(PlaceTy::Declared(t.id), n);
        let flag = self.entry_slot(PlaceTy::Value(Ty::Scalar(Scalar::Bool)), n);
        let off = self.value(Ty::Scalar(Scalar::Bool));
        self.entry_setup.push(Inst::Const {
            result: off,
            bits: 0,
        });
        self.entry_setup.push(Inst::Store {
            address: flag,
            value: off,
        });
        let from = self.address(AddressBase::Value(t.value), &[], PlaceTy::Declared(t.id), n);
        let bytes = self.native_size(t.id, n);
        self.inst(
            Inst::Copy {
                to: storage,
                from,
                bytes,
            },
            n,
        );
        let on = self.konst(Ty::Scalar(Scalar::Bool), 1, n).unwrap();
        self.inst(
            Inst::Store {
                address: flag,
                value: on,
            },
            n,
        );
        let reached = self.reached_temporary(i);
        self.reached_temps[reached].storage = Some(storage);
        if let cleanup::Owner::Temp(t) = &mut self.owners[i] {
            t.flag = Some(flag);
            t.avail = Avail::Maybe;
        }
        self.epoch += 1;
    }
    /// A copy of temporary owner `i` behind a drop flag set on this path, for a
    /// `break` to carry to its frame's exit. The owner itself is left as it is, so
    /// the path that does not break still cleans it; a temporary already spilled
    /// keeps its own storage and flag.
    pub(super) fn carry_temporary(&mut self, i: usize, n: NodeId) -> Option<cleanup::Owner> {
        let cleanup::Owner::Temp(t) = self.owners[i] else {
            return None;
        };
        if !self.live() || !matches!(t.avail, Avail::Live | Avail::Maybe) {
            return None;
        }
        if self.temporary_storage(t).is_some() {
            return Some(cleanup::Owner::Temp(t));
        }
        let storage = self.entry_slot(PlaceTy::Declared(t.id), n);
        let flag = self.entry_slot(PlaceTy::Value(Ty::Scalar(Scalar::Bool)), n);
        let off = self.value(Ty::Scalar(Scalar::Bool));
        self.entry_setup.push(Inst::Const {
            result: off,
            bits: 0,
        });
        self.entry_setup.push(Inst::Store {
            address: flag,
            value: off,
        });
        self.reached_temps.push(Reached {
            storage: Some(storage),
            held: None,
        });
        let carried = cleanup::Temp {
            flag: Some(flag),
            avail: Avail::Maybe,
            reached: self.reached_temps.len() as u32,
            ..t
        };
        self.carry_into(i, carried, n);
        Some(cleanup::Owner::Temp(carried))
    }
    /// On this path, move temporary owner `i` into the carried owner `to`'s storage
    /// and set `to`'s flag to whether `i` is live here.
    pub(super) fn carry_into(&mut self, i: usize, to: cleanup::Temp, n: NodeId) {
        let cleanup::Owner::Temp(t) = self.owners[i] else {
            return;
        };
        let (Some(storage), Some(flag)) = (self.temporary_storage(to), to.flag) else {
            return;
        };
        if !self.live() {
            return;
        }
        let on = match (t.avail, t.flag) {
            (Avail::Live, _) => self.konst(Ty::Scalar(Scalar::Bool), 1, n),
            (Avail::Maybe, Some(f)) => {
                let on = self.value(Ty::Scalar(Scalar::Bool));
                self.inst(
                    Inst::Read {
                        result: on,
                        address: f,
                        mode: ReadMode::Bits,
                    },
                    n,
                );
                Some(on)
            }
            _ => None,
        };
        let Some(on) = on else { return };
        if self.temporary_storage(t) != Some(storage) {
            let from = match self.temporary_storage(t) {
                Some(at) => at,
                None => self.address(AddressBase::Value(t.value), &[], PlaceTy::Declared(t.id), n),
            };
            let bytes = self.native_size(t.id, n);
            self.inst(
                Inst::Copy {
                    to: storage,
                    from,
                    bytes,
                },
                n,
            );
        }
        if t.flag != Some(flag) {
            self.inst(
                Inst::Store {
                    address: flag,
                    value: on,
                },
                n,
            );
        }
    }
    pub(super) fn install_entry_setup(&mut self) {
        if self.entry_setup.is_empty() {
            return;
        }
        let old = self.b.blocks[0].insts;
        self.entry_setup
            .extend_from_slice(old.get(&self.b.insts).unwrap());
        let start = self.b.insts.len();
        let setup_len = self.entry_setup.len() - old.len as usize;
        self.b.insts.append(&mut self.entry_setup);
        self.b
            .inst_anchors
            .extend(std::iter::repeat_n(self.anchor(self.header), setup_len));
        self.b
            .inst_anchors
            .extend_from_within(old.start as usize..(old.start + old.len) as usize);
        self.b.blocks[0].insts = PoolRange::new(start as u32, (self.b.insts.len() - start) as u32);
    }
}
