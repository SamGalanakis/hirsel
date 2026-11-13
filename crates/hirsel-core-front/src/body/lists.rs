//! Fixed-layout List storage, with portable charges independent of native stride.
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

impl<'c, 'a> Cx<'c, 'a, '_> {
    fn list_arith(
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
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: AllocKind::Own,
                place: AllocPlace::Current,
                charge,
                overflow: Some(overflow),
                size,
                align: 8,
                fail,
            },
            n,
        );
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
        self.replace_value(item, Some(pointer), value, n)
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
