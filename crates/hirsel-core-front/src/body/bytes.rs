//! Read-only byte storage. Its descriptor has the same size and capacity
//! niche as Bytes, but word 1 holds an encoded length or the static payer.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn byte_word(
        &mut self,
        base: AddressBase,
        index: u64,
        ty: Ty,
        n: NodeId,
    ) -> Option<AddressId> {
        let offset = self.konst(Ty::Scalar(Scalar::U64), index * 8, n)?;
        Some(self.address(base, &[Step::Byte(offset)], PlaceTy::Value(ty), n))
    }

    pub(super) fn freeze_bytes(
        &mut self,
        from: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(result, n)?;
        let to = self.result_slot(PlaceTy::Declared(id), n);
        let u = Ty::Scalar(Scalar::U64);
        for (index, ty) in [(0, Ty::Ptr), (1, u), (2, u)] {
            let from = self.byte_word(AddressBase::Value(from), index, ty, n)?;
            let at = self.byte_word(AddressBase::Address(to), index, ty, n)?;
            let mut value = self.read_bits(from, ty, n);
            if index == 1 {
                // Every materialized heap buffer fits in a native Layout, so its
                // length is at most isize::MAX and doubling it is lossless.
                let len = value;
                value = self.value(u);
                self.inst(
                    Inst::IntArith {
                        result: value,
                        flag: None,
                        op: ArithOp::Add,
                        mode: ArithMode::Wrapping,
                        left: len,
                        right: len,
                    },
                    n,
                );
            }
            self.inst(Inst::Store { address: at, value }, n);
        }
        self.consume(Some(from), n);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    pub(super) fn view_frozen_bytes(
        &mut self,
        from: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.runtime_type(result, n)?;
        let to = self.result_slot(PlaceTy::Declared(id), n);
        let uint = scalar(Scalar::U64);
        let u = Ty::Scalar(Scalar::U64);
        let p = AddressBase::Value(from);
        let data = self.byte_word(p, 0, Ty::Ptr, n)?;
        let data = self.read_bits(data, Ty::Ptr, n);
        let metadata = self.byte_word(p, 1, u, n)?;
        let metadata = self.read_bits(metadata, u, n);
        let capacity = self.byte_word(p, 2, u, n)?;
        let capacity = self.read_bits(capacity, u, n);
        let one = self.konst(u, 1, n)?;
        let zero = self.konst(u, 0, n)?;
        let tag = self.arith("&", uint, Some(metadata), Some(one), n)?;
        let static_data = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: static_data,
                op: CompareOp::Ne,
                left: tag,
                right: zero,
            },
            n,
        );
        let static_len = self.new_block(&[]);
        let heap_len = self.new_block(&[]);
        let join = self.new_block(&[u]);
        self.runtime_branch(static_data, static_len, heap_len, n);
        self.start(static_len);
        self.jump(join, &[capacity], n);
        self.start(heap_len);
        let heap_length = self.arith(">>", uint, Some(metadata), Some(one), n)?;
        self.jump(join, &[heap_length], n);
        self.start(join);
        let length = self.b.blocks[join.index()]
            .params
            .get(&self.b.pools.values)?[0];
        for (index, (value, ty)) in [(data, Ty::Ptr), (length, u), (capacity, u)]
            .into_iter()
            .enumerate()
        {
            let at = self.byte_word(AddressBase::Address(to), index as u64, ty, n)?;
            self.inst(Inst::Store { address: at, value }, n);
        }
        Some(self.pointer(to, n))
    }

    pub(super) fn inline_byte_count(&self, source: TyId) -> Option<u64> {
        match self.t.kind(self.bare(source)) {
            TyKind::Array(byte, count) if *byte == scalar(Scalar::U8) => Some(*count),
            _ => None,
        }
    }

    pub(super) fn append_inline_bytes(
        &mut self,
        source_ty: TyId,
        header: ValueId,
        source: Option<ValueId>,
        start: ValueId,
        end: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let count = self.inline_byte_count(source_ty)?;
        let u = Ty::Scalar(Scalar::U64);
        let zero = self.konst(u, 0, n)?;
        let bound = self.konst(u, count, n)?;
        let ordered = self.list_compare(CompareOp::Le, start, end, n);
        let within = self.list_compare(CompareOp::Le, end, bound, n);
        let valid = self.emit_binary(BinaryOp::BitAnd, BOOL, Some(ordered), Some(within), n)?;
        if count == 0 {
            return Some(valid);
        }
        let source = source?;
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let length = self.carrier_word(header, carrier, SeqWord::Len, n);
        let cap = self.carrier_word(header, carrier, SeqWord::Cap, n);
        let (room, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, cap, length, n);
        let (bytes, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, end, start, n);
        let fits = self.list_compare(CompareOp::Le, bytes, room, n);
        let okay = self.emit_binary(BinaryOp::BitAnd, BOOL, Some(valid), Some(fits), n)?;
        let nonempty = self.list_compare(CompareOp::Ne, bytes, zero, n);
        let copying = self.emit_binary(BinaryOp::BitAnd, BOOL, Some(okay), Some(nonempty), n)?;
        let copy = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.runtime_branch(copying, copy, done, n);
        self.start(copy);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let to = self.address(AddressBase::Value(data), &[Step::Byte(length)], byte, n);
        let from = self.address(AddressBase::Value(source), &[Step::Byte(start)], byte, n);
        self.inst(Inst::Copy { to, from, bytes }, n);
        let (len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, length, bytes, n);
        self.set_carrier_word(header, carrier, SeqWord::Len, len, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(okay)
    }
}
