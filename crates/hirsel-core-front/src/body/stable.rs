use super::*;

impl Cx<'_, '_, '_> {
    /// MEM-9/12 packs scalar fields without native padding. Decode plain elements
    /// into their ordinary native places; no view of a temporary escapes.
    pub(super) fn packed_plain_size(&mut self, ty: TyId) -> Option<u64> {
        let ty = self.bare(ty);
        if let Ok(Rep::Scalar(Ty::Scalar(s))) = self.rep(ty) {
            return Some(s.size());
        }
        match self.t.kind(ty).clone() {
            TyKind::Unit => Some(0),
            TyKind::Array(elem, count) => self.packed_plain_size(elem)?.checked_mul(count),
            _ if self.private_variant_cases(ty).is_some() => {
                let mut largest = 0u64;
                for case in 0..self.private_variant_cases(ty)? as u32 {
                    let mut size = 0u64;
                    for field in self.compact_fields(ty, Some(case))? {
                        size = size.checked_add(self.packed_plain_size(field)?)?;
                    }
                    largest = largest.max(size);
                }
                largest.checked_add(8)
            }
            _ => {
                let count = self.plain_fields(ty)?;
                let mut size = 0u64;
                for i in 0..count {
                    let field = self.field_ty(ty, i)?;
                    size = size.checked_add(self.packed_plain_size(field)?)?;
                }
                Some(size)
            }
        }
    }
    /// FORM #58: plain variants have a private fixed-stride logical encoding.
    pub(super) fn private_variant_cases(&self, ty: TyId) -> Option<usize> {
        match self.t.kind(self.bare(ty)) {
            TyKind::Adt(d, _) if self.p.decls[d.index()].repr == Repr::Own && self.copies(ty) => {
                self.variant_cases(ty)
            }
            _ => None,
        }
    }
    pub(super) fn plain_fields(&self, ty: TyId) -> Option<u32> {
        match self.t.kind(ty) {
            TyKind::Tuple(xs) => Some(xs.len() as u32),
            TyKind::Adt(d, _) if self.p.decls[d.index()].repr == Repr::Own && self.copies(ty) => {
                match &self.p.decls[d.index()].body {
                    DeclBody::Record(fs) => Some(fs.len() as u32),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    pub(super) fn byte_at(
        &mut self,
        base: ValueId,
        offset: ValueId,
        ty: PlaceTy,
        n: NodeId,
    ) -> AddressId {
        self.address(AddressBase::Value(base), &[Step::Byte(offset)], ty, n)
    }
    /// W is a property of our blob, so selecting its load width needs no validation.
    pub(super) fn packed_count(
        &mut self,
        blob: ValueId,
        at: ValueId,
        w: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let done = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        let result = self.b.blocks[done.index()]
            .params
            .get(&self.b.pools.values)
            .unwrap()[0];
        for (bytes, scalar) in [(2, Scalar::U16), (4, Scalar::U32), (8, Scalar::U64)] {
            if bytes != 8 {
                let width = self.konst(Ty::Scalar(Scalar::U64), bytes, n)?;
                let condition = self.value(Ty::Scalar(Scalar::Bool));
                self.inst(
                    Inst::Compare {
                        result: condition,
                        op: CompareOp::Eq,
                        left: w,
                        right: width,
                    },
                    n,
                );
                let load = self.new_block(&[]);
                let next = self.new_block(&[]);
                let yes = self.edge(load, &[]);
                let no = self.edge(next, &[]);
                self.end(Terminator::Branch { condition, yes, no }, Some(n));
                self.start(load);
                let address = self.byte_at(blob, at, PlaceTy::Value(Ty::Scalar(scalar)), n);
                let value = self.read_bits(address, Ty::Scalar(scalar), n);
                let wide = self.value(Ty::Scalar(Scalar::U64));
                self.inst(
                    Inst::Convert {
                        result: wide,
                        flag: None,
                        operand: value,
                        mode: ConvertMode::Widen,
                    },
                    n,
                );
                self.jump(done, &[wide], n);
                self.start(next);
            } else {
                let address = self.byte_at(blob, at, PlaceTy::Value(Ty::Scalar(scalar)), n);
                let value = self.read_bits(address, Ty::Scalar(scalar), n);
                self.jump(done, &[value], n);
            }
        }
        self.start(done);
        Some(result)
    }
    pub(super) fn span_parts(
        &mut self,
        header: ValueId,
        n: NodeId,
    ) -> Option<(ValueId, ValueId, ValueId, ValueId)> {
        if !self.live() {
            return None;
        }
        let blob = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let at = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
        let w = self.read_sequence_word(blob, 8, Ty::Scalar(Scalar::U64), n);
        let count = self.packed_count(blob, at, w, n)?;
        Some((blob, at, w, count))
    }
    /// Decode the one packed encoding into the value's native representation.
    pub(super) fn decode_plain(
        &mut self,
        blob: ValueId,
        at: ValueId,
        w: ValueId,
        ty: TyId,
        to: AddressId,
        n: NodeId,
    ) -> Option<()> {
        if self.is_locator(ty) {
            for (off, value) in [(0, blob), (8, at)] {
                let off = self.konst(Ty::Scalar(Scalar::U64), off, n)?;
                let field = self.address(
                    AddressBase::Address(to),
                    &[Step::Byte(off)],
                    PlaceTy::Value(self.b.values[value.index()]),
                    n,
                );
                self.inst(
                    Inst::Store {
                        address: field,
                        value,
                    },
                    n,
                );
            }
            return Some(());
        }
        match self.rep(ty).ok()? {
            Rep::Scalar(t) => {
                let from = self.byte_at(blob, at, PlaceTy::Value(t), n);
                let value = self.read_bits(from, t, n);
                self.inst(Inst::Store { address: to, value }, n);
            }
            Rep::None => {}
            Rep::Place(_) => {
                if let Some(cases) = self.private_variant_cases(ty) {
                    let tag_at = self.byte_at(blob, at, PlaceTy::Value(Ty::Scalar(Scalar::U64)), n);
                    let tag = self.read_bits(tag_at, Ty::Scalar(Scalar::U64), n);
                    let done = self.new_block(&[]);
                    for case in 0..cases as u32 {
                        let next = if case + 1 < cases as u32 {
                            let value = self.konst(Ty::Scalar(Scalar::U64), u64::from(case), n)?;
                            let yes = self.test(CompareOp::Eq, tag, value, n);
                            let body = self.new_block(&[]);
                            let next = self.new_block(&[]);
                            self.branch(yes, body, next, n);
                            self.start(body);
                            Some(next)
                        } else {
                            None
                        };
                        self.inst(Inst::SetCase { address: to, case }, n);
                        let mut off = self.packed_add_k(at, 8, n)?;
                        for (i, field) in
                            self.compact_fields(ty, Some(case))?.into_iter().enumerate()
                        {
                            let place = self.place_ty(field, n)?;
                            let dest = self.address(
                                AddressBase::Address(to),
                                &[Step::CaseField {
                                    case,
                                    field: i as u32,
                                }],
                                place,
                                n,
                            );
                            self.decode_plain(blob, off, w, field, dest, n)?;
                            let size = self.packed_plain_size(field)?;
                            off = self.packed_add_k(off, size, n)?;
                        }
                        self.jump(done, &[], n);
                        if let Some(next) = next {
                            self.start(next);
                        }
                    }
                    self.start(done);
                    return Some(());
                }
                if let TyKind::Array(elem, count) = *self.t.kind(ty) {
                    let stride = self.packed_plain_size(elem)?;
                    let u64t = Ty::Scalar(Scalar::U64);
                    let limit = self.konst(u64t, count, n)?;
                    let zero = self.konst(u64t, 0, n)?;
                    let stride = self.konst(u64t, stride, n)?;
                    let head = self.new_block(&[u64t, u64t]);
                    let body = self.new_block(&[]);
                    let done = self.new_block(&[]);
                    self.jump(head, &[zero, at], n);
                    self.start(head);
                    let params = self.block_params(head);
                    let (index, off) = (params[0], params[1]);
                    let more = self.test(CompareOp::Lt, index, limit, n);
                    self.branch(more, body, done, n);
                    self.start(body);
                    let place = self.place_ty(elem, n)?;
                    let dest = self.address(
                        AddressBase::Address(to),
                        &[Step::Field(0), Step::Element(index)],
                        place,
                        n,
                    );
                    self.decode_plain(blob, off, w, elem, dest, n)?;
                    let next = self.packed_add_k(index, 1, n)?;
                    let after = self.packed_add(off, stride, n);
                    self.jump(head, &[next, after], n);
                    self.start(done);
                    return Some(());
                }
                let count = self.plain_fields(ty)?;
                let mut from = at;
                for i in 0..count {
                    let field = self.field_ty(ty, i)?;
                    let place = self.place_ty(field, n)?;
                    let dest = self.address(AddressBase::Address(to), &[Step::Field(i)], place, n);
                    self.decode_plain(blob, from, w, field, dest, n)?;
                    from = self.skip(blob, from, w, field, n)?;
                }
            }
        }
        Some(())
    }
    /// A FixedSpan element with no packed encoding: a non-plain one was refused at its
    /// type (MEM-12), so lowering stops quietly; a plain one is a gap.
    pub(super) fn unencoded_element(&mut self, elem: TyId, n: NodeId) {
        if self.copies(elem) {
            self.gap(
                n,
                "the private encoding of this FixedSpan element is not built",
            );
        } else {
            self.halt();
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fixed_span_element(
        &mut self,
        header: ValueId,
        elem: TyId,
        index: ValueId,
        checked: bool,
        header_words: u64,
        n: NodeId,
    ) -> Option<AddressId> {
        let Some(stride) = self.packed_plain_size(elem) else {
            self.unencoded_element(elem, n);
            return None;
        };
        let (blob, at, w, count) = self.span_parts(header, n)?;
        if checked {
            let condition = self.value(Ty::Scalar(Scalar::Bool));
            self.inst(
                Inst::Compare {
                    result: condition,
                    op: CompareOp::Lt,
                    left: index,
                    right: count,
                },
                n,
            );
            let load = self.new_block(&[]);
            let fail = self.new_block(&[]);
            let yes = self.edge(load, &[]);
            let no = self.edge(fail, &[]);
            self.end(Terminator::Branch { condition, yes, no }, Some(n));
            self.start(fail);
            let cleanup = self.fail_to(n);
            self.end(
                Terminator::Raise {
                    cause: hirsel_core_ir::RaiseCause::Bounds,
                    cleanup,
                },
                Some(n),
            );
            self.start(load);
        }
        let stride = self.konst(Ty::Scalar(Scalar::U64), stride, n)?;
        let offset = self.packed_mul(index, stride, n);
        let mut header_bytes = w;
        if header_words == 2 {
            header_bytes = self.packed_add(w, w, n);
        }
        let data = self.packed_add(at, header_bytes, n);
        let from = self.packed_add(data, offset, n);
        let place = self.place_ty(elem, n)?;
        let to = self.new_slot(place, n);
        self.decode_plain(blob, from, w, elem, to, n)?;
        Some(to)
    }
    pub(super) fn fixed_span_get(
        &mut self,
        header: ValueId,
        index: ValueId,
        elem: TyId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let count = self.span_parts(header, n)?.3;
        let rep = self.rep(result).ok()?;
        let to = self.new_slot(rep.stored()?, n);
        let condition = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: condition,
                op: CompareOp::Lt,
                left: index,
                right: count,
            },
            n,
        );
        let load = self.new_block(&[]);
        let empty = self.new_block(&[]);
        let done = self.new_block(&[]);
        let yes = self.edge(load, &[]);
        let no = self.edge(empty, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
        self.start(load);
        let value = self.fixed_span_element(header, elem, index, false, 1, n)?;
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let elem_rep = self.rep(elem).ok()?;
        if let Some(place) = elem_rep.stored() {
            let dest = self.address(
                AddressBase::Address(to),
                &[Step::CaseField { case: 1, field: 0 }],
                place,
                n,
            );
            match elem_rep {
                Rep::Place(id) => {
                    let p = self.pointer(value, n);
                    self.copy_to(dest, p, id, n);
                }
                Rep::Scalar(t) => {
                    let value = self.read_bits(value, t, n);
                    self.inst(
                        Inst::Store {
                            address: dest,
                            value,
                        },
                        n,
                    );
                }
                _ => {}
            }
        }
        self.jump(done, &[], n);
        self.start(empty);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(done);
        Some(self.pointer(to, n))
    }
}
