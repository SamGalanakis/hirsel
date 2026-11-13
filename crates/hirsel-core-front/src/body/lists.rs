//! Fixed-layout List storage, with portable charges independent of native stride.
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

#[derive(Clone, Copy)]
struct ListPlan {
    charge: ValueId,
    overflow: ValueId,
    size: ValueId,
    logical: ValueId,
    stride: ValueId,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
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
        self.list_allocated(to, list_id, null, zero, n);
        self.jump(done, &[], n);

        self.start(allocate);
        let plan = self.list_plan(item, count, None, false, n)?;
        let (block, available) = self.list_allocate(plan, AllocPlace::Current, n);
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
    fn list_plan(
        &mut self,
        item: TyId,
        count: ValueId,
        extra: Option<ValueId>,
        nonzero: bool,
        n: NodeId,
    ) -> Option<ListPlan> {
        let logical = if nonzero {
            self.nonzero_size(item, n)?
        } else {
            self.layout_size(item, true, n)?
        };
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
        Some(ListPlan {
            charge,
            overflow,
            size,
            logical,
            stride,
        })
    }
    fn list_allocate(
        &mut self,
        plan: ListPlan,
        place: AllocPlace,
        n: NodeId,
    ) -> (ValueId, ValueId) {
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: AllocKind::Own,
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
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let two = self.konst(Ty::Scalar(Scalar::U64), 2, n)?;
        let four = self.konst(Ty::Scalar(Scalar::U64), 4, n)?;
        let (double, cap_over) = self.list_arith(ArithOp::Mul, ArithMode::Checked, old_cap, two, n);
        let count_over = self.list_bool(BinaryOp::BitOr, len_over?, cap_over?, n);
        let new_cap = self.list_max(needed, four, n);
        let new_cap = self.list_max(new_cap, double, n);
        let plan = self.list_plan(item, new_cap, Some(count_over), true, n)?;
        let old_data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let owned = self.list_compare(CompareOp::Ne, old_cap, zero, n);
        let current = self.new_block(&[]);
        let like = self.new_block(&[]);
        let admitted = self.new_block(&[Ty::Ptr, Ty::Scalar(Scalar::U64)]);
        self.list_branch(owned, like, current, n);
        self.start(current);
        let (block, available) = self.list_allocate(plan, AllocPlace::Current, n);
        self.jump(admitted, &[block, available], n);
        self.start(like);
        let (block, available) = self.list_allocate(plan, AllocPlace::Like(old_data), n);
        self.jump(admitted, &[block, available], n);
        self.start(admitted);
        let parameters = self.b.blocks[admitted.index()]
            .params
            .get(&self.b.pools.values)
            .unwrap();
        let (block, available) = (parameters[0], parameters[1]);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let copy = self.new_block(&[]);
        let error = self.new_block(&[]);
        self.list_branch(ok, copy, error, n);
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
        self.start(copy);
        let nonempty = self.list_compare(CompareOp::Ne, len, zero, n);
        let copy_data = self.new_block(&[]);
        let commit = self.new_block(&[]);
        self.list_branch(nonempty, copy_data, commit, n);
        self.start(copy_data);
        let (bytes, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, len, plan.stride, n);
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
                bytes,
            },
            n,
        );
        self.jump(commit, &[], n);
        self.start(commit);
        self.sequence_word(header, 0, Ty::Ptr, block, n);
        self.sequence_word(header, 16, Ty::Scalar(Scalar::U64), new_cap, n);
        let free = self.new_block(&[]);
        self.list_branch(owned, free, success, n);
        self.start(free);
        let (charge, _) =
            self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_cap, plan.logical, n);
        let seven = self.konst(Ty::Scalar(Scalar::U64), 7, n)?;
        let (rounded, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, charge, seven, n);
        let mask = self.konst(Ty::Scalar(Scalar::U64), !7, n)?;
        let charge = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: charge,
                op: BinaryOp::BitAnd,
                left: rounded,
                right: mask,
                fail: None,
            },
            n,
        );
        self.inst(
            Inst::Free {
                block: old_data,
                charge,
            },
            n,
        );
        self.jump(success, &[], n);
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
        if matches!(
            self.t.kind(item),
            TyKind::Scalar(Scalar::U8) | TyKind::Param(_)
        ) {
            self.gap(
                n,
                "replacement through a potentially static List is not specified (FOR SPEC)",
            );
            return None;
        }
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
