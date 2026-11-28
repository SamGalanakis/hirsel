use super::super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(in crate::body) fn held_new(
        &mut self,
        item: TyId,
        value: Option<ValueId>,
        hash: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let size = self.layout_size(item, true, n)?;
        let base = self.konst(Ty::Scalar(Scalar::U64), 32, n)?;
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        let requested = self.runtime_charge(one, size, base, n)?;
        let (row, input) = self.runtime_place(item, value, n)?;
        let [node, available, ..] = self.runtime_row(
            hirsel_core_ir::RtOp::HeldNew { item: row },
            &[Arg::Address(input), Arg::Value(hash), Arg::Value(requested)],
            n,
        )?;
        let ok = self.runtime_nonzero(Ty::Ptr, node, n)?;
        self.runtime_admitted(result, ok, [requested, available], n, |cx, to| {
            cx.consume(value, n);
            cx.runtime_store(to, &[Step::Field(0)], Ty::Ptr, node, n);
        })
    }

    pub(in crate::body) fn held_take(
        &mut self,
        item: TyId,
        value: ValueId,
        hash: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let row = self.runtime_type(item, n)?;
        let output = self.new_slot(PlaceTy::Declared(row), n);
        let handle = self.runtime_handle(value, n);
        let [ok, ..] = self.runtime_row(
            hirsel_core_ir::RtOp::HeldTake { item: row },
            &[Arg::Value(handle), Arg::Value(hash), Arg::Address(output)],
            n,
        )?;
        self.consume(Some(value), n);
        self.runtime_result(
            result,
            ok,
            n,
            |cx, to, ty| {
                let v = cx.runtime_read(output, ty, n);
                cx.runtime_case_payload(to, 0, ty, v, n);
            },
            |cx, to, error| {
                let pair = cx
                    .runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], error, n)
                    .unwrap();
                let TyKind::Tuple(parts) = cx.t.kind(error).clone() else {
                    unreachable!()
                };
                if let Some(tag) = cx.runtime_field(pair, &[Step::Field(0)], parts[0], n) {
                    cx.inst(
                        Inst::SetCase {
                            address: tag,
                            case: 0,
                        },
                        n,
                    );
                }
                let held = cx
                    .runtime_field(pair, &[Step::Field(1)], parts[1], n)
                    .unwrap();
                cx.runtime_store(held, &[Step::Field(0)], Ty::Ptr, handle, n);
            },
        )
    }

    pub(in crate::body) fn static_string(
        &mut self,
        data: &[u8],
        ty: TyId,
        owned: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let id = self.runtime_type(ty, n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let u64t = Ty::Scalar(Scalar::U64);
        let length = self.konst(u64t, data.len() as u64, n)?;
        let zero = self.konst(u64t, 0, n)?;
        let (start, capacity) = if data.is_empty() {
            (self.konst(Ty::Ptr, 0, n)?, zero)
        } else {
            let d = self.e.static_bytes(data);
            let v = self.value(Ty::Ptr);
            self.inst(Inst::ReadStatic { result: v, data: d }, n);
            if owned {
                let [ptr, capacity, ..] = self.runtime_row(
                    hirsel_core_ir::RtOp::StaticString,
                    &[Arg::Value(v), Arg::Value(length)],
                    n,
                )?;
                (ptr, capacity)
            } else {
                (v, zero)
            }
        };
        for (k, (value, t)) in [(start, Ty::Ptr), (length, u64t), (capacity, u64t)]
            .into_iter()
            .enumerate()
        {
            let Some(offset) = self.konst(u64t, 8 * k as u64, n) else {
                return None;
            };
            let at = self.address(
                AddressBase::Address(to),
                &[Step::Byte(offset)],
                PlaceTy::Value(t),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        Some(self.pointer(to, n))
    }

    pub(in crate::body) fn static_frozen_literal(
        &mut self,
        source: ValueId,
        block: ValueId,
        length: ValueId,
        n: NodeId,
    ) -> Option<(ValueId, ValueId)> {
        let [data, meta, ..] = self.runtime_row(
            hirsel_core_ir::RtOp::StaticFrozen,
            &[Arg::Value(source), Arg::Value(block), Arg::Value(length)],
            n,
        )?;
        Some((data, meta))
    }

    pub(in crate::body) fn record_resource_origin(
        &mut self,
        rid: TypeId,
        block: ValueId,
        n: NodeId,
    ) {
        self.runtime_row(
            hirsel_core_ir::RtOp::ResourceOrigin { item: rid },
            &[Arg::Value(block)],
            n,
        );
    }
}
