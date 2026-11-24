//! T1.2 admission and ordered handback, shared by resource and closure literals.
use super::*;

pub(super) struct OwnedAllocation {
    pub kind: AllocKind,
    pub charge: u64,
    pub align: u64,
    pub row: TypeId,
    pub prefix: u64,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// `ty` as a field of the edge an allocation names (FORM #89): a list's or
    /// map's storage is `Seq`/`Map` of these.
    pub(super) fn edge_field(&mut self, ty: TyId, n: NodeId) -> Option<FieldTy> {
        let field = self.e.field_ty(self.p, &mut self.t, ty);
        if field.is_none() {
            self.gap(n, "allocation edge layout is not built");
        }
        field
    }

    /// Called immediately before emitting a typed block allocation. Copies and
    /// owner operations use held-value sizes, which differ for a resource.
    pub(super) fn allocation_layout(
        &mut self,
        ty: TypeId,
        result: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.e.generic_names.contains_key(&ty) {
            return None;
        }
        let overflow = self.konst(Ty::Scalar(Scalar::Bool), 0, n)?;
        self.allocations.push(AllocationLayout { result, ty });
        Some(overflow)
    }

    /// MEM-21: end the open block raising `Capacity { requested, available }`
    /// (FAIL-2); FAIL-8 cleanup drops what the path still owns, a taken argument
    /// included, since the refused operation consumes nothing.
    pub(super) fn raise_capacity(&mut self, requested: ValueId, available: ValueId, n: NodeId) {
        let cleanup = self.fail_to(n);
        self.end(
            Terminator::Raise {
                cause: hirsel_core_ir::RaiseCause::Capacity {
                    requested,
                    available,
                },
                cleanup,
            },
            Some(n),
        );
    }

    /// Unless `admitted`, raise `Capacity` (`raise_capacity`); lowering goes on in
    /// the admitted block.
    pub(super) fn raise_unless(
        &mut self,
        admitted: ValueId,
        requested: ValueId,
        available: ValueId,
        n: NodeId,
    ) {
        let (yes, no) = (self.new_block(&[]), self.new_block(&[]));
        let (ye, ne) = (self.edge(yes, &[]), self.edge(no, &[]));
        self.b.edges[ne.index()].cold = true;
        self.end(
            Terminator::Branch {
                condition: admitted,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(no);
        self.raise_capacity(requested, available, n);
        self.start(yes);
    }

    /// Admit `allocation` from the current limit and return its block; a refusal
    /// raises `Capacity` (`raise_capacity`).
    pub(super) fn admit_owned(
        &mut self,
        allocation: OwnedAllocation,
        n: NodeId,
    ) -> Option<ValueId> {
        let c = self.konst(Ty::Scalar(Scalar::U64), allocation.charge, n)?;
        let sz = self.layout_word(
            LayoutQuery::BlockSize {
                ty: allocation.row,
                prefix: allocation.prefix,
            },
            n,
        );
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
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
            },
            n,
        );
        let null = self.konst(Ty::Ptr, 0, n)?;
        let admitted = self.test(CompareOp::Ne, block, null, n);
        self.raise_unless(admitted, c, available, n);
        Some(block)
    }

    pub(super) fn admit_with_handback(
        &mut self,
        ty: TyId,
        allocation: OwnedAllocation,
        reps: &[Rep],
        vals: &[Option<ValueId>],
        n: NodeId,
        success: impl FnOnce(&mut Self, AddressId, ValueId),
    ) -> Option<ValueId> {
        let id = self.place_row(ty, "an admitted value without a place is not built", n)?;
        if !self.live() {
            return None;
        }
        let to = self.result_slot(PlaceTy::Declared(id), n);
        let c = self.konst(Ty::Scalar(Scalar::U64), allocation.charge, n)?;
        let sz = self.layout_word(
            LayoutQuery::BlockSize {
                ty: allocation.row,
                prefix: allocation.prefix,
            },
            n,
        );
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let overflow = if matches!(allocation.kind, AllocKind::Resource { .. }) {
            self.allocation_layout(allocation.row, block, n)
        } else {
            None
        };
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: allocation.kind,
                place: AllocPlace::Current,
                charge: c,
                overflow,
                size: sz,
                align: allocation.align,
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

/// The bytes a value of a row with `repr` and `recipe` takes where it is held (a
/// local, a field, an element, a copy), or with `logical` what it counts in a
/// portable size (FORM #116). A resource's or a region value's holder is its block
/// pointer, and a region holder counts as a `RegionRef` field, 16 (MEM-24); only
/// allocating the value takes its block. The emitter and the generic materialiser
/// both ask here, so a rigid `T` and its instance cannot disagree.
pub(super) fn held_size(repr: LRepr, recipe: &Recipe, logical: bool) -> Result<u64, &'static str> {
    if logical {
        return hirsel_core_layout::charge::held(repr, recipe)
            .ok_or("logical size of a compact generic value is not built");
    }
    Ok(match repr {
        LRepr::Resource => hirsel_core_layout::POINTER,
        LRepr::Region => hirsel_core_layout::POINTER,
        _ => match &recipe.layout {
            Layout::Fixed(f) => f.size,
            Layout::Compact(_) => return Err("copying a compact generic value is not built"),
        },
    })
}
