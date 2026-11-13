use super::*;

impl Cx<'_, '_, '_> {
    /// MEM-9/12 packs scalar fields without native padding. Decode plain elements
    /// into their ordinary native places; no view of a temporary escapes.
    pub(super) fn packed_plain_size(&mut self, ty: TyId) -> Option<u64> {
        match self.t.kind(ty) {
            TyKind::Scalar(s) => Some(s.size()),
            TyKind::Unit => Some(0),
            _ => {
                let count = self.plain_fields(ty)?;
                let mut size = 0;
                for i in 0..count {
                    let field = self.field_ty(ty, i)?;
                    size += self.packed_plain_size(field)?;
                }
                Some(size)
            }
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
    fn byte_at(&mut self, base: ValueId, offset: ValueId, ty: PlaceTy, n: NodeId) -> AddressId {
        self.address(AddressBase::Value(base), &[Step::Byte(offset)], ty, n)
    }
    fn offset_sum(&mut self, left: ValueId, right: ValueId, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result,
                op: BinaryOp::Add,
                left,
                right,
                fail: None,
            },
            n,
        );
        Some(result)
    }
    fn offset_by(&mut self, at: ValueId, bytes: u64, n: NodeId) -> Option<ValueId> {
        let bytes = self.konst(Ty::Scalar(Scalar::U64), bytes, n)?;
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result,
                op: BinaryOp::Add,
                left: at,
                right: bytes,
                fail: None,
            },
            n,
        );
        Some(result)
    }
    /// W is a property of our blob, so selecting its load width needs no validation.
    fn packed_count(
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
    fn decode_plain(
        &mut self,
        blob: ValueId,
        at: ValueId,
        ty: TyId,
        to: AddressId,
        n: NodeId,
    ) -> Option<()> {
        match self.t.kind(ty) {
            TyKind::Scalar(s) => {
                let ir = Ty::Scalar(*s);
                let from = self.byte_at(blob, at, PlaceTy::Value(ir), n);
                let value = self.read_bits(from, ir, n);
                self.inst(Inst::Store { address: to, value }, n);
            }
            TyKind::Unit => {}
            _ => {
                let count = self.plain_fields(ty)?;
                let mut offset = 0;
                for i in 0..count {
                    let field = self.field_ty(ty, i)?;
                    let place = self.place_ty(field, n)?;
                    let dest = self.address(AddressBase::Address(to), &[Step::Field(i)], place, n);
                    let from = self.offset_by(at, offset, n)?;
                    self.decode_plain(blob, from, field, dest, n)?;
                    offset += self.packed_plain_size(field)?;
                }
            }
        }
        Some(())
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
            self.gap(
                n,
                "the private encoding of this FixedSpan element is not built",
            );
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
        let offset = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: offset,
                op: BinaryOp::Mul,
                left: index,
                right: stride,
                fail: None,
            },
            n,
        );
        let mut header_bytes = w;
        if header_words == 2 {
            header_bytes = self.offset_sum(w, w, n)?;
        }
        let data = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: data,
                op: BinaryOp::Add,
                left: at,
                right: header_bytes,
                fail: None,
            },
            n,
        );
        let from = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: from,
                op: BinaryOp::Add,
                left: data,
                right: offset,
                fail: None,
            },
            n,
        );
        let place = self.place_ty(elem, n)?;
        let to = self.new_slot(place, n);
        self.decode_plain(blob, from, elem, to, n)?;
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
    pub(super) fn scalar_sequence_get(
        &mut self,
        header: ValueId,
        index: ValueId,
        elem: TyId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let TyKind::Scalar(scalar) = self.t.kind(elem) else {
            self.gap(
                n,
                "packed element views need the compact locator representation",
            );
            return None;
        };
        let scalar = *scalar;
        let (blob, at, w, count) = self.span_parts(header, n)?;
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
        let stride = self.konst(Ty::Scalar(Scalar::U64), scalar.size(), n)?;
        let offset = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Binary {
                result: offset,
                op: BinaryOp::Mul,
                left: index,
                right: stride,
                fail: None,
            },
            n,
        );
        let headers = self.offset_sum(w, w, n)?;
        let data = self.offset_sum(at, headers, n)?;
        let offset = self.offset_sum(data, offset, n)?;
        let address = self.byte_at(blob, offset, PlaceTy::Value(Ty::Scalar(scalar)), n);
        let pointer = self.pointer(address, n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let dest = self.address(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            PlaceTy::Value(Ty::Ptr),
            n,
        );
        self.inst(
            Inst::Store {
                address: dest,
                value: pointer,
            },
            n,
        );
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
