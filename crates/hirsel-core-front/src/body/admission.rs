//! T1.2 admission and ordered handback, shared by resource and closure literals.
use super::*;

pub(super) struct OwnedAllocation {
    pub kind: AllocKind,
    pub charge: u64,
    pub size: u64,
    pub align: u64,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn admit_with_handback(
        &mut self,
        ty: TyId,
        allocation: OwnedAllocation,
        reps: &[Rep],
        vals: &[Option<ValueId>],
        n: NodeId,
        success: impl FnOnce(&mut Self, AddressId, ValueId),
    ) -> Option<ValueId> {
        let id = match self.rep(ty).ok()? {
            Rep::Place(id) => id,
            _ => return None,
        };
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let c = self.konst(Ty::Scalar(Scalar::U64), allocation.charge, n)?;
        let sz = self.konst(Ty::Scalar(Scalar::U64), allocation.size, n)?;
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: allocation.kind,
                place: AllocPlace::Current,
                charge: c,
                overflow: None,
                size: sz,
                align: allocation.align,
                fail,
            },
            n,
        );
        let null = self.konst(Ty::Ptr, 0, n)?;
        let admitted = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: admitted,
                op: CompareOp::Ne,
                left: block,
                right: null,
            },
            n,
        );
        let (yes, no, join) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        let (ye, ne) = (self.edge(yes, &[]), self.edge(no, &[]));
        self.end(
            Terminator::Branch {
                condition: admitted,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(yes);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        success(self, to, block);
        self.jump(join, &[], n);
        // Error: `Capacity` and the fields, moved back to the caller.
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let decl_ty = |cx: &mut Self, t: TyId| match cx.rep(t) {
            Ok(Rep::Place(id)) => Some(id),
            _ => None,
        };
        let back = match self.t.kind(ty) {
            TyKind::Adt(_, args) => args[1],
            _ => unreachable!("a resource literal is a Result"),
        };
        let back_id = decl_ty(self, back)?;
        let err = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(back_id),
            n,
        );
        let (cap_at, rest) = match self.t.kind(back).clone() {
            TyKind::Tuple(parts) => {
                let cap_id = decl_ty(self, parts[0])?;
                let cap_at = self.address(
                    AddressBase::Address(err),
                    &[Step::Field(0)],
                    PlaceTy::Declared(cap_id),
                    n,
                );
                let rest = if reps.len() == 1 {
                    None
                } else {
                    let f_id = decl_ty(self, parts[1])?;
                    Some(self.address(
                        AddressBase::Address(err),
                        &[Step::Field(1)],
                        PlaceTy::Declared(f_id),
                        n,
                    ))
                };
                (cap_at, rest)
            }
            _ => (err, None),
        };
        for (k, value) in [c, available].into_iter().enumerate() {
            let at = self.address(
                AddressBase::Address(cap_at),
                &[Step::Field(k as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        for (i, r) in reps.iter().enumerate() {
            if let Some(place) = r.stored() {
                let at = match rest {
                    None => self.address(AddressBase::Address(err), &[Step::Field(1)], place, n),
                    Some(f) => {
                        self.address(AddressBase::Address(f), &[Step::Field(i as u32)], place, n)
                    }
                };
                self.install(at, *r, vals[i], n);
            }
        }
        self.jump(join, &[], n);
        self.start(join);
        let p = self.pointer(to, n);
        self.temp(ty, Some(p));
        Some(p)
    }
}
