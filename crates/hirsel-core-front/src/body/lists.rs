//! Fixed-layout List storage, with portable charges independent of native stride.
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

#[derive(Clone, Copy)]
enum ListStorage {
    Empty,
    Stored,
    Dynamic(ValueId),
}

#[derive(Clone)]
pub(super) struct ListPlan {
    /// The edge owning the storage block: `Seq` of the element, or a map's `Map`.
    pub(super) edge: FieldTy,
    pub(super) charge: ValueId,
    pub(super) overflow: ValueId,
    pub(super) size: ValueId,
    pub(super) logical: ValueId,
    pub(super) stride: ValueId,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn list_storage(&mut self, item: TyId, n: NodeId) -> Option<ListStorage> {
        match self.rep(item) {
            Ok(Rep::None) => Some(ListStorage::Empty),
            Ok(Rep::Scalar(_)) => Some(ListStorage::Stored),
            Ok(Rep::Place(id)) if !self.e.generic_names.contains_key(&id) => {
                Some(if self.e.size(id) == 0 {
                    ListStorage::Empty
                } else {
                    ListStorage::Stored
                })
            }
            Ok(Rep::Place(_)) => {
                let stride = self.layout_size(item, false, n)?;
                let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
                Some(ListStorage::Dynamic(self.list_compare(
                    CompareOp::Eq,
                    stride,
                    zero,
                    n,
                )))
            }
            Err(what) => {
                self.gap(n, what);
                None
            }
        }
    }
    fn list_choose(
        &mut self,
        on: ValueId,
        yes_value: ValueId,
        no_value: ValueId,
        n: NodeId,
    ) -> ValueId {
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let done = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        self.list_branch(on, yes, no, n);
        self.start(yes);
        self.jump(done, &[yes_value], n);
        self.start(no);
        self.jump(done, &[no_value], n);
        self.start(done);
        self.b.blocks[done.index()]
            .params
            .get(&self.b.pools.values)
            .unwrap()[0]
    }
    pub(super) fn list_new(&mut self, item: TyId, result: TyId, n: NodeId) -> Option<ValueId> {
        let value = self.empty_list(result, n)?;
        let storage = self.list_storage(item, n)?;
        let cap = match storage {
            ListStorage::Stored => return Some(value),
            ListStorage::Empty => self.konst(Ty::Scalar(Scalar::U64), u64::MAX, n)?,
            ListStorage::Dynamic(on) => {
                let max = self.konst(Ty::Scalar(Scalar::U64), u64::MAX, n)?;
                let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
                self.list_choose(on, max, zero, n)
            }
        };
        self.sequence_word(value, 16, Ty::Scalar(Scalar::U64), cap, n);
        Some(value)
    }
    fn list_refused(
        &mut self,
        to: AddressId,
        cap_id: TypeId,
        charge: ValueId,
        available: ValueId,
        n: NodeId,
    ) {
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let cap = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(cap_id),
            n,
        );
        for (i, value) in [charge, available].into_iter().enumerate() {
            let at = self.address(
                AddressBase::Address(cap),
                &[Step::Field(i as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
    }
    fn list_count_overflow(
        &mut self,
        to: AddressId,
        cap_id: TypeId,
        item: TyId,
        n: NodeId,
    ) -> Option<()> {
        // FORM60's declined counter-overflow admission. Under a finalizer grant it
        // follows the ordinary allocation failure edge; otherwise it is Capacity.
        let max = self.konst(Ty::Scalar(Scalar::U64), u64::MAX, n)?;
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let overflow = self.konst(Ty::Scalar(Scalar::Bool), 1, n)?;
        let plan = ListPlan {
            edge: FieldTy::Seq(Box::new(self.edge_field(item, n)?)),
            charge: max,
            overflow,
            size: zero,
            logical: zero,
            stride: zero,
        };
        let (_, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        self.list_refused(to, cap_id, max, available, n);
        Some(())
    }

    /// SEM-4: an unaligned little-endian word, with Bounds even when `at + 8`
    /// would overflow. Keep the checked end and unsigned bound in one predicate so
    /// native optimizers can combine it with the caller's checked offsets.
    pub(super) fn list_word_le(
        &mut self,
        header: ValueId,
        at: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let width = self.konst(Ty::Scalar(Scalar::U64), 8, n)?;
        let (end, overflow) = self.list_arith(ArithOp::Add, ArithMode::Checked, at, width, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, end, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, within, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        self.list_branch(valid, yes, no, n);
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
        let address = self.address(
            AddressBase::Value(data),
            &[Step::Byte(at)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            n,
        );
        Some(self.read_bits(address, Ty::Scalar(Scalar::U64), n))
    }

    pub(super) fn list_arith(
        &mut self,
        op: ArithOp,
        mode: ArithMode,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> (ValueId, Option<ValueId>) {
        let result = self.value(Ty::Scalar(Scalar::U64));
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
        (result, flag)
    }

    pub(super) fn list_with_capacity(
        &mut self,
        item: TyId,
        count: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let payload = match self.t.kind(result) {
            TyKind::Adt(_, args) => args[0],
            _ => unreachable!(),
        };
        let list_id = match self.rep(payload) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let capacity = self
            .t
            .mk(TyKind::Adt(self.p.core_decl("Capacity")?, vec![]));
        let capacity_id = match self.rep(capacity) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let storage = self.list_storage(item, n)?;
        if matches!(storage, ListStorage::Empty) {
            let max = self.konst(Ty::Scalar(Scalar::U64), u64::MAX, n)?;
            self.list_allocated(to, list_id, null, max, n);
            let value = self.pointer(to, n);
            self.temp(result, Some(value));
            return Some(value);
        }
        let empty = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: empty,
                op: CompareOp::Eq,
                left: count,
                right: zero,
            },
            n,
        );
        let empty = if let ListStorage::Dynamic(on) = storage {
            self.list_bool(BinaryOp::BitOr, empty, on, n)
        } else {
            empty
        };
        let yes = self.new_block(&[]);
        let allocate = self.new_block(&[]);
        let done = self.new_block(&[]);
        let ye = self.edge(yes, &[]);
        let ne = self.edge(allocate, &[]);
        self.end(
            Terminator::Branch {
                condition: empty,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(yes);
        let cap = if let ListStorage::Dynamic(on) = storage {
            let max = self.konst(Ty::Scalar(Scalar::U64), u64::MAX, n)?;
            self.list_choose(on, max, zero, n)
        } else {
            zero
        };
        self.list_allocated(to, list_id, null, cap, n);
        self.jump(done, &[], n);

        self.start(allocate);
        let plan = self.list_plan(item, count, None, n)?;
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        let charge = plan.charge;
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
        let ok = self.new_block(&[]);
        let error = self.new_block(&[]);
        let oe = self.edge(ok, &[]);
        let ee = self.edge(error, &[]);
        self.end(
            Terminator::Branch {
                condition: admitted,
                yes: oe,
                no: ee,
            },
            Some(n),
        );
        self.start(ok);
        self.list_allocated(to, list_id, block, count, n);
        self.jump(done, &[], n);
        self.start(error);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let cap = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(capacity_id),
            n,
        );
        for (i, value) in [charge, available].into_iter().enumerate() {
            let at = self.address(
                AddressBase::Address(cap),
                &[Step::Field(i as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    fn list_bool(&mut self, op: BinaryOp, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail: None,
            },
            n,
        );
        result
    }
    fn list_compare(&mut self, op: CompareOp, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
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
    fn list_branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }
    fn list_max(&mut self, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result,
                op: BinaryOp::Max,
                left,
                right,
                fail: None,
            },
            n,
        );
        result
    }
    pub(super) fn list_plan(
        &mut self,
        item: TyId,
        count: ValueId,
        extra: Option<ValueId>,
        n: NodeId,
    ) -> Option<ListPlan> {
        let logical = self.layout_size(item, true, n)?;
        let stride = self.layout_size(item, false, n)?;
        let (raw_charge, mul_overflow) =
            self.list_arith(ArithOp::Mul, ArithMode::Checked, count, logical, n);
        let seven = self.konst(Ty::Scalar(Scalar::U64), 7, n)?;
        let (rounded, add_overflow) =
            self.list_arith(ArithOp::Add, ArithMode::Checked, raw_charge, seven, n);
        let mask = self.konst(Ty::Scalar(Scalar::U64), !7, n)?;
        let rounded_charge = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: rounded_charge,
                op: BinaryOp::BitAnd,
                left: rounded,
                right: mask,
                fail: None,
            },
            n,
        );
        let overflow = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Binary {
                result: overflow,
                op: BinaryOp::BitOr,
                left: mul_overflow?,
                right: add_overflow?,
                fail: None,
            },
            n,
        );
        let overflow = match extra {
            Some(extra) => self.list_bool(BinaryOp::BitOr, overflow, extra, n),
            None => overflow,
        };
        let max = self.konst(Ty::Scalar(Scalar::U64), u64::MAX, n)?;
        let over = self.new_block(&[]);
        let fits = self.new_block(&[]);
        let charged = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        let oe = self.edge(over, &[]);
        let fe = self.edge(fits, &[]);
        self.end(
            Terminator::Branch {
                condition: overflow,
                yes: oe,
                no: fe,
            },
            Some(n),
        );
        self.start(over);
        self.jump(charged, &[max], n);
        self.start(fits);
        self.jump(charged, &[rounded_charge], n);
        self.start(charged);
        let charge = self.b.blocks[charged.index()]
            .params
            .get(&self.b.pools.values)
            .unwrap()[0];
        // A native size beyond the address space is host exhaustion, after logical
        // admission. Saturating bytes cannot wrap into a small successful block.
        let (size, _) = self.list_arith(ArithOp::Mul, ArithMode::Saturating, count, stride, n);
        let edge = FieldTy::Seq(Box::new(self.edge_field(item, n)?));
        Some(ListPlan {
            edge,
            charge,
            overflow,
            size,
            logical,
            stride,
        })
    }
    pub(super) fn list_allocate(
        &mut self,
        plan: &ListPlan,
        place: AllocPlace,
        n: NodeId,
    ) -> (ValueId, ValueId) {
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let fail = self.fail_to(n);
        let kind = AllocKind::Own {
            edge: plan.edge.clone(),
        };
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind,
                place,
                charge: plan.charge,
                overflow: Some(plan.overflow),
                size: plan.size,
                align: 8,
                fail,
            },
            n,
        );
        (block, available)
    }

    pub(super) fn list_reserve(
        &mut self,
        item: TyId,
        header: ValueId,
        additional: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let cap_ty = self
            .t
            .mk(TyKind::Adt(self.p.core_decl("Capacity")?, vec![]));
        let cap_id = match self.rep(cap_ty) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let old_cap = self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n);
        let (needed, len_over) =
            self.list_arith(ArithOp::Add, ArithMode::Checked, len, additional, n);
        let lacks_room = self.list_compare(CompareOp::Gt, needed, old_cap, n);
        let grow = self.list_bool(BinaryOp::BitOr, lacks_room, len_over?, n);
        let growth = self.new_block(&[]);
        let success = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(grow, growth, success, n);
        self.start(growth);
        match self.list_storage(item, n)? {
            ListStorage::Stored => {}
            ListStorage::Empty => {
                self.list_count_overflow(to, cap_id, item, n)?;
                self.jump(done, &[], n);
                self.start(success);
                self.inst(
                    Inst::SetCase {
                        address: to,
                        case: 0,
                    },
                    n,
                );
                self.jump(done, &[], n);
                self.start(done);
                let value = self.pointer(to, n);
                self.temp(result, Some(value));
                return Some(value);
            }
            ListStorage::Dynamic(on) => {
                let zero_sized = self.new_block(&[]);
                let stored = self.new_block(&[]);
                self.list_branch(on, zero_sized, stored, n);
                self.start(zero_sized);
                self.list_count_overflow(to, cap_id, item, n)?;
                self.jump(done, &[], n);
                self.start(stored);
            }
        }
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let two = self.konst(Ty::Scalar(Scalar::U64), 2, n)?;
        let four = self.konst(Ty::Scalar(Scalar::U64), 4, n)?;
        let (double, cap_over) = self.list_arith(ArithOp::Mul, ArithMode::Checked, old_cap, two, n);
        let count_over = self.list_bool(BinaryOp::BitOr, len_over?, cap_over?, n);
        let new_cap = self.list_max(needed, four, n);
        let new_cap = self.list_max(new_cap, double, n);
        let plan = self.list_plan(item, new_cap, Some(count_over), n)?;
        let owned = self.list_compare(CompareOp::Ne, old_cap, zero, n);
        let current = self.new_block(&[]);
        let like = self.new_block(&[]);
        let error = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let (moved, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, len, plan.stride, n);
        self.list_branch(owned, like, current, n);
        // Capacity 0 owns no storage (empty, or static bytes): allocate in the
        // current limit and copy the elements over.
        self.start(current);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let copy = self.new_block(&[]);
        let refused = self.new_block(&[]);
        self.list_branch(ok, copy, refused, n);
        self.start(refused);
        self.jump(error, &[available], n);
        self.start(copy);
        let nonempty = self.list_compare(CompareOp::Ne, len, zero, n);
        let copy_data = self.new_block(&[]);
        let commit = self.new_block(&[]);
        self.list_branch(nonempty, copy_data, commit, n);
        self.start(copy_data);
        let old_data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let from = self.address(
            AddressBase::Value(old_data),
            &[],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let to_data = self.address(
            AddressBase::Value(block),
            &[],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        self.inst(
            Inst::Copy {
                to: to_data,
                from,
                bytes: moved,
            },
            n,
        );
        self.jump(commit, &[], n);
        self.start(commit);
        self.sequence_word(header, 0, Ty::Ptr, block, n);
        self.sequence_word(header, 16, Ty::Scalar(Scalar::U64), new_cap, n);
        self.jump(success, &[], n);
        // Owned storage is replaced by one `grow` (crate docs, "Growth"), which
        // admits like the old block and refunds its charge after the install.
        self.start(like);
        let list = self.p.core_decl("List")?;
        let list = self.t.mk(TyKind::Adt(list, vec![item]));
        let Ok(Rep::Place(list_id)) = self.rep(list) else {
            return None;
        };
        let carrier = self.address(
            AddressBase::Value(header),
            &[],
            PlaceTy::Declared(list_id),
            n,
        );
        let (charge, _) =
            self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_cap, plan.logical, n);
        let seven = self.konst(Ty::Scalar(Scalar::U64), 7, n)?;
        let (rounded, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, charge, seven, n);
        let mask = self.konst(Ty::Scalar(Scalar::U64), !7, n)?;
        let refund = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: refund,
                op: BinaryOp::BitAnd,
                left: rounded,
                right: mask,
                fail: None,
            },
            n,
        );
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let fail = self.fail_to(n);
        self.inst(
            Inst::Grow {
                result: block,
                available,
                carrier,
                charge: plan.charge,
                overflow: Some(plan.overflow),
                size: plan.size,
                align: 8,
                capacity: new_cap,
                moved,
                refund,
                fail,
            },
            n,
        );
        let grown = self.list_compare(CompareOp::Ne, block, null, n);
        let refused = self.new_block(&[]);
        self.list_branch(grown, success, refused, n);
        self.start(refused);
        self.jump(error, &[available], n);
        self.start(error);
        let available = self.b.blocks[error.index()]
            .params
            .get(&self.b.pools.values)
            .unwrap()[0];
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let cap = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(cap_id),
            n,
        );
        for (i, value) in [plan.charge, available].into_iter().enumerate() {
            let at = self.address(
                AddressBase::Address(cap),
                &[Step::Field(i as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        self.jump(done, &[], n);
        self.start(success);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    pub(super) fn list_push(
        &mut self,
        item: TyId,
        header: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let cap_ty = self
            .t
            .mk(TyKind::Adt(self.p.core_decl("Capacity")?, vec![]));
        let reserve_ty = self
            .t
            .mk(TyKind::Adt(self.p.core_decl("Result")?, vec![UNIT, cap_ty]));
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        // The taken element stays live while admission can still fail, including
        // a finalizer-grant failure; either value result transfers it exactly once.
        let reserved = self.list_reserve(item, header, one, reserve_ty, n)?;
        let reserve_id = match self.rep(reserve_ty) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let cap_id = match self.rep(cap_ty) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let out_id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let error_ty = match self.t.kind(result) {
            TyKind::Adt(_, args) => args[1],
            _ => unreachable!(),
        };
        let error_id = match self.rep(error_ty) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let to = self.new_slot(PlaceTy::Declared(out_id), n);
        let from = self.address(
            AddressBase::Value(reserved),
            &[],
            PlaceTy::Declared(reserve_id),
            n,
        );
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: from,
            },
            n,
        );
        let zero = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.list_compare(CompareOp::Eq, tag, zero, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(ok, yes, no, n);
        let rep = match self.rep(item) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        self.start(yes);
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let element = self.list_element(item, header, len, n)?;
        if let Some(place) = rep.stored() {
            let at = self.address(AddressBase::Value(element), &[], place, n);
            self.install(at, rep, value, n);
        }
        let (new_len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, one, n);
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), new_len, n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let error = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(error_id),
            n,
        );
        let cap = self.address(
            AddressBase::Address(error),
            &[Step::Field(0)],
            PlaceTy::Declared(cap_id),
            n,
        );
        let old_cap = self.address(
            AddressBase::Address(from),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Declared(cap_id),
            n,
        );
        let source = self.pointer(old_cap, n);
        self.copy_to(cap, source, cap_id, n);
        if let Some(place) = rep.stored() {
            let at = self.address(AddressBase::Address(error), &[Step::Field(1)], place, n);
            self.install(at, rep, value, n);
        }
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    pub(super) fn list_append(
        &mut self,
        item: TyId,
        header: ValueId,
        source: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let old_len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let count = self.read_sequence_word(source, 8, Ty::Scalar(Scalar::U64), n);
        let reserved = self.list_reserve(item, header, count, result, n)?;
        let result_id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let at = self.address(
            AddressBase::Value(reserved),
            &[],
            PlaceTy::Declared(result_id),
            n,
        );
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: at,
            },
            n,
        );
        let ok_tag = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.list_compare(CompareOp::Eq, tag, ok_tag, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let nonempty = self.list_compare(CompareOp::Ne, count, zero, n);
        let copying = self.list_bool(BinaryOp::BitAnd, ok, nonempty, n);
        let copy = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(copying, copy, done, n);
        self.start(copy);
        let target = self.list_element(item, header, old_len, n)?;
        let data = self.read_sequence_word(source, 0, Ty::Ptr, n);
        let stride = self.layout_size(item, false, n)?;
        let (bytes, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, count, stride, n);
        let from = self.address(
            AddressBase::Value(data),
            &[],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let to = self.address(
            AddressBase::Value(target),
            &[],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        self.inst(Inst::Copy { to, from, bytes }, n);
        let (len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, old_len, count, n);
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), len, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(reserved)
    }

    pub(super) fn list_fill(
        &mut self,
        item: TyId,
        header: ValueId,
        count: ValueId,
        value: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let old_len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let reserved = self.list_reserve(item, header, count, result, n)?;
        let result_id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        let at = self.address(
            AddressBase::Value(reserved),
            &[],
            PlaceTy::Declared(result_id),
            n,
        );
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address: at,
            },
            n,
        );
        let ok_tag = self.konst(Ty::Scalar(Scalar::U32), 0, n)?;
        let ok = self.list_compare(CompareOp::Eq, tag, ok_tag, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let nonempty = self.list_compare(CompareOp::Ne, count, zero, n);
        let filling = self.list_bool(BinaryOp::BitAnd, ok, nonempty, n);
        let fill = self.new_block(&[]);
        let commit = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(filling, fill, done, n);
        self.start(fill);
        let rep = match self.rep(item) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        if let Some(place) = rep.stored() {
            let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
            let stride = self.layout_size(item, false, n)?;
            let empty = self.list_compare(CompareOp::Eq, stride, zero, n);
            let setup = self.new_block(&[]);
            self.list_branch(empty, commit, setup, n);
            self.start(setup);
            let (offset, _) =
                self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_len, stride, n);
            let base = self.address(AddressBase::Value(data), &[Step::Byte(offset)], place, n);
            let base = self.pointer(base, n);
            let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
            let test = self.new_block(&[Ty::Scalar(Scalar::U64)]);
            let store = self.new_block(&[]);
            self.jump(test, &[zero], n);
            self.start(test);
            let index = self.b.blocks[test.index()]
                .params
                .get(&self.b.pools.values)
                .unwrap()[0];
            let more = self.list_compare(CompareOp::Lt, index, count, n);
            self.list_branch(more, store, commit, n);
            self.start(store);
            let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, index, stride, n);
            let target = self.address(AddressBase::Value(base), &[Step::Byte(offset)], place, n);
            self.install(target, rep, value, n);
            let (next, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, index, one, n);
            self.jump(test, &[next], n);
        } else {
            self.jump(commit, &[], n);
        }
        self.start(commit);
        let (len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, old_len, count, n);
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), len, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(reserved)
    }

    /// FORM #61: a range is a new non-owning carrier, copied through the result dest.
    pub(super) fn list_slice(
        &mut self,
        item: TyId,
        header: ValueId,
        lo: ValueId,
        hi: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            // core.list.slice returns View[List[T]] in xs. A List always has
            // an address-carried fixed carrier, including when T has size zero.
            _ => unreachable!("a List slice result has its list carrier"),
        };
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let ordered = self.list_compare(CompareOp::Le, lo, hi, n);
        let contained = self.list_compare(CompareOp::Le, hi, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, ordered, contained, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        self.list_branch(valid, yes, no, n);
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
        let stride = self.layout_size(item, false, n)?;
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, lo, stride, n);
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let at = self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let data = self.pointer(at, n);
        let (count, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, hi, lo, n);
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let value = self.pointer(to, n);
        self.sequence_word(value, 0, Ty::Ptr, data, n);
        self.sequence_word(value, 8, Ty::Scalar(Scalar::U64), count, n);
        self.sequence_word(value, 16, Ty::Scalar(Scalar::U64), count, n);
        // The view has no owner and never drops the source elements or storage.
        Some(value)
    }

    pub(super) fn list_pop(
        &mut self,
        item: TyId,
        header: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let rep = match self.rep(item) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let out = self.new_slot(PlaceTy::Declared(id), n);
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let present = self.list_compare(CompareOp::Ne, len, zero, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(present, yes, no, n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: out,
                case: 0,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(yes);
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        let (last, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, len, one, n);
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), last, n);
        self.inst(
            Inst::SetCase {
                address: out,
                case: 1,
            },
            n,
        );
        if let Some(place) = rep.stored() {
            let element = self.list_element(item, header, last, n)?;
            let value = match rep {
                Rep::Scalar(ty) => {
                    let at = self.address(AddressBase::Value(element), &[], place, n);
                    self.read_bits(at, ty, n)
                }
                _ => element,
            };
            let to = self.address(
                AddressBase::Address(out),
                &[Step::CaseField { case: 1, field: 0 }],
                place,
                n,
            );
            self.install(to, rep, Some(value), n);
        }
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(out, n);
        self.temp(result, Some(value));
        Some(value)
    }

    pub(super) fn list_clear(&mut self, item: TyId, header: ValueId, n: NodeId) {
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n).unwrap();
        // The carrier retains its allocation, while a drain owns the old elements.
        // Advancing that drain before each drop also resumes correctly on failure.
        self.sequence_word(header, 8, Ty::Scalar(Scalar::U64), zero, n);
        if self.drop_id(item).is_some() {
            let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
            let next = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::U64)), n);
            self.inst(
                Inst::Store {
                    address: next,
                    value: zero,
                },
                n,
            );
            let mark = self.owner_mark();
            self.drain_owner(data, next, len, item);
            self.end_owners(mark, None, false, n);
        }
    }

    pub(super) fn list_set(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let in_bounds = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: in_bounds,
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
                condition: in_bounds,
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
        let pointer = self.list_element(item, header, index, n)?;
        self.replace_value(item, Some(pointer), value, n)
    }

    /// Both indices are checked before moving either element. The scratch place
    /// is unowned during this non-failing three-copy exchange.
    pub(super) fn list_swap(
        &mut self,
        item: TyId,
        header: ValueId,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let a = self.list_compare(CompareOp::Lt, left, len, n);
        let b = self.list_compare(CompareOp::Lt, right, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, a, b, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        self.list_branch(valid, yes, no, n);
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
        let rep = match self.rep(item) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let Some(place) = rep.stored() else {
            return Some(());
        };
        let same = self.list_compare(CompareOp::Eq, left, right, n);
        let exchange = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(same, done, exchange, n);
        self.start(exchange);
        let a = self.list_element(item, header, left, n)?;
        let b = self.list_element(item, header, right, n)?;
        let a = self.address(AddressBase::Value(a), &[], place, n);
        let b = self.address(AddressBase::Value(b), &[], place, n);
        let saved = self.new_slot(place, n);
        let bytes = self.layout_size(item, false, n)?;
        self.inst(
            Inst::Copy {
                to: saved,
                from: a,
                bytes,
            },
            n,
        );
        self.inst(
            Inst::Copy {
                to: a,
                from: b,
                bytes,
            },
            n,
        );
        self.inst(
            Inst::Copy {
                to: b,
                from: saved,
                bytes,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }

    fn list_element(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let stride = self.layout_size(item, false, n)?;
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, index, stride, n);
        let place = match self.rep(item) {
            Ok(rep) => rep
                .stored()
                .unwrap_or(PlaceTy::Value(Ty::Scalar(Scalar::U8))),
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let at = self.address(AddressBase::Value(data), &[Step::Byte(offset)], place, n);
        let pointer = self.pointer(at, n);
        Some(pointer)
    }

    pub(super) fn list_get(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let out = self.new_slot(PlaceTy::Declared(id), n);
        let len = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let present = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: present,
                op: CompareOp::Lt,
                left: index,
                right: len,
            },
            n,
        );
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let join = self.new_block(&[]);
        let ye = self.edge(yes, &[]);
        let ne = self.edge(no, &[]);
        self.end(
            Terminator::Branch {
                condition: present,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: out,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);
        self.start(yes);
        let pointer = self.list_element(item, header, index, n)?;
        self.inst(
            Inst::SetCase {
                address: out,
                case: 1,
            },
            n,
        );
        let place = self.rep(item).ok()?.stored()?;
        let at = self.address(
            AddressBase::Address(out),
            &[Step::CaseField { case: 1, field: 0 }],
            place,
            n,
        );
        self.install_view(at, item, pointer, n)?;
        self.jump(join, &[], n);
        self.start(join);
        let value = self.pointer(out, n);
        self.temp(result, Some(value));
        Some(value)
    }

    fn list_allocated(
        &mut self,
        to: AddressId,
        list_id: TypeId,
        data: ValueId,
        capacity: ValueId,
        n: NodeId,
    ) {
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let list = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 0, field: 0 }],
            PlaceTy::Declared(list_id),
            n,
        );
        let pointer = self.pointer(list, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n).unwrap();
        self.sequence_word(pointer, 0, Ty::Ptr, data, n);
        self.sequence_word(pointer, 8, Ty::Scalar(Scalar::U64), zero, n);
        self.sequence_word(pointer, 16, Ty::Scalar(Scalar::U64), capacity, n);
    }
}
