use super::*;

impl Cx<'_, '_, '_> {
    fn log_ty(&mut self, item: TyId, n: NodeId) -> Option<TyId> {
        let log = self
            .p
            .decls
            .iter()
            .position(|d| d.role == Some(crate::decls::Role::Log) && d.path.0.name == "Log");
        let log = self.named(log, "a log without the prelude's Log is not built", n)?;
        Some(
            self.t
                .mk(TyKind::Adt(crate::types::DeclId(log as u32), vec![item])),
        )
    }

    /// MEM-29: the log node is a box allocation whose payload is the List carrier.
    pub(super) fn runtime_log_new(
        &mut self,
        item: TyId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let list = self.list_ty(item, n)?;
        let (charge, overflow) = self.owner_charge(list, None, n)?;
        let id = self.runtime_type(list, n)?;
        let carrier = self.list_carrier(item, n)?;
        let payload = self.layout_size(list, false, n)?;
        // A log whose node never leaves this body keeps it in the frame, charged
        // as on the heap (frame_logs.rs).
        let framed = self.frame_log_next.take();
        let frame = if framed.is_some() {
            Some(self.frame_log_bytes(n)?)
        } else {
            None
        };
        let data = if framed == Some(true) && item == scalar(Scalar::U64) {
            self.frame_log_entries(n)?
        } else {
            self.konst(Ty::Ptr, 0, n)?
        };
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let (node, ok, available) = if let Some(storage) = frame {
            let node = self.pointer(storage, n);
            let (ok, available) = self.frame_admit(storage, charge, Some(overflow), false, n);
            (node, ok, available)
        } else {
            let node = self.value(Ty::Ptr);
            let available = self.value(Ty::Scalar(Scalar::U64));
            let edge = FieldTy::Box(TypeRef(id.0));
            self.inst(
                Inst::Alloc {
                    result: node,
                    available,
                    region_entry: None,
                    kind: AllocKind::Own { edge },
                    place: AllocPlace::Current,
                    charge,
                    overflow: Some(overflow),
                    size: payload,
                    align: 8,
                },
                n,
            );
            (node, self.runtime_nonzero(Ty::Ptr, node, n)?, available)
        };
        let log = self.runtime_admitted(result, ok, [charge, available], n, |cx, to| {
            // The node itself is the List carrier: {data, len, capacity}.
            cx.set_carrier_word(node, carrier, SeqWord::Data, data, n);
            cx.set_carrier_word(node, carrier, SeqWord::Cap, zero, n);
            cx.set_carrier_word(node, carrier, SeqWord::Len, zero, n);
            cx.runtime_store(to, &[Step::Field(0)], Ty::Ptr, node, n);
        })?;
        if let Some(storage) = frame {
            self.frame_logs
                .insert(log, super::frame_logs::FrameLog { storage, item });
        }
        Some(log)
    }

    /// Append through the pointer loaded from the Log carrier. A refused reserve
    /// raises with the taken element still the caller's, so cleanup drops it.
    pub(super) fn runtime_log_append(
        &mut self,
        item: TyId,
        log: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let log_ty = self.log_ty(item, n)?;
        let log_id = self.runtime_type(log_ty, n)?;
        let log_base = self.address(AddressBase::Value(log), &[], PlaceTy::Declared(log_id), n);
        let node_at = self.address(
            AddressBase::Address(log_base),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let node = self.read_bits(node_at, Ty::Ptr, n);
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        self.log_reserve(item, node, one, n)?;
        let item_rep = self.rep_at(item, n)?;
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(node, carrier, SeqWord::Len, n);
        let element = self.list_element(item, node, len, n)?;
        if let Some(place) = item_rep.stored() {
            let at = self.address(AddressBase::Value(element), &[], place, n);
            self.install(at, item_rep, value, n);
        }
        let (new_len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, one, n);
        self.set_carrier_word(node, carrier, SeqWord::Len, new_len, n);
        self.consume(value, n);
        None
    }

    pub(super) fn runtime_log_finish(
        &mut self,
        item: TyId,
        log: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let log_ty = self.log_ty(item, n)?;
        let log_id = self.runtime_type(log_ty, n)?;
        let log_base = self.address(AddressBase::Value(log), &[], PlaceTy::Declared(log_id), n);
        let node_at = self.address(
            AddressBase::Address(log_base),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        let node = self.read_bits(node_at, Ty::Ptr, n);
        let list = self.list_ty(item, n)?;
        let list_id = self.runtime_type(list, n)?;
        let to = self.new_slot(PlaceTy::Declared(list_id), n);
        self.copy_to(to, node, list_id, n);
        self.consume(Some(log), n);
        if let Some(&frame) = self.frame_logs.get(&log) {
            self.inst(
                Inst::FrameRefund {
                    storage: frame.storage,
                },
                n,
            );
        } else {
            let (charge, _) = self.owner_charge(list, None, n)?;
            self.inst(
                Inst::Free {
                    block: node,
                    charge,
                },
                n,
            );
        }
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }
}
