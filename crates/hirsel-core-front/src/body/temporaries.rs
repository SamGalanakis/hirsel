//! Temporary owners carried by scoped block values (LANG-6). Uses the source
//! provenance arena and the existing flagged cleanup machinery.
use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct Reached {
    storage: Option<AddressId>,
    held: Option<u32>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
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
    fn entry_slot(&mut self, ty: PlaceTy, _n: NodeId) -> AddressId {
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
        let bytes = self
            .konst(Ty::Scalar(Scalar::U64), self.e.size(t.id), n)
            .unwrap();
        if self.e.generic_names.contains_key(&t.id) {
            self.copies.push(CopySize {
                value: bytes,
                ty: t.id,
                logical: false,
            });
        }
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
        self.b.inst_anchors.extend(std::iter::repeat_n(
            Some(self.anchor(self.header)),
            setup_len,
        ));
        self.b
            .inst_anchors
            .extend_from_within(old.start as usize..(old.start + old.len) as usize);
        self.b.blocks[0].insts = PoolRange::new(start as u32, (self.b.insts.len() - start) as u32);
    }
}
