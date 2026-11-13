//! MEM-3 span builders. A builder holds its region blob and its region; the blob
//! (private, MEM-9) is `{capacity @0, W = 8 @8, count @16, packed elements @24}`,
//! so `finish` publishes `{blob, 16}`, the `FixedSpan` header `span_parts` reads. The
//! builder's allocation is charged `capacity × size + 8` (MEM-24); a builder that never
//! pushed holds no blob and finishes to a static empty one.
use super::*;

/// The blob's words before its elements.
const HEAD: u64 = 24;

impl Cx<'_, '_, '_> {
    /// `span.builder`, `span.with_capacity`, `span.push` and `span.finish`; `result` is
    /// the call's result type.
    pub(super) fn span_builder_op(
        &mut self,
        name: &str,
        elem: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let Some(es) = self.packed_plain_size(elem) else {
            self.gap(n, "span elements of this type are not built");
            return None;
        };
        match name {
            "builder" => {
                let id = self.declared(result)?;
                let to = self.new_slot(PlaceTy::Declared(id), n);
                let null = self.konst(Ty::Ptr, 0, n)?;
                self.put_word(AddressBase::Address(to), &[], 0, null, n)?;
                self.put_word(AddressBase::Address(to), &[], 8, vals[0]?, n)?;
                Some(self.pointer(to, n))
            }
            "with_capacity" => self.span_with_capacity(vals[0]?, vals[1]?, es, result, n),
            "push" => self.span_push(vals[0]?, vals[1], elem, es, result, n),
            "finish" => self.span_finish(vals[0]?, result, n),
            _ => {
                self.gap(n, "this span construction operation is not built");
                None
            }
        }
    }

    pub(super) fn declared(&mut self, ty: TyId) -> Option<TypeId> {
        match self.rep(ty).ok()? {
            Rep::Place(id) => Some(id),
            _ => None,
        }
    }
    pub(super) fn word_at(
        &mut self,
        base: AddressBase,
        steps: &[Step],
        offset: u64,
        t: Ty,
        n: NodeId,
    ) -> Option<AddressId> {
        let o = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
        let mut all = steps.to_vec();
        all.push(Step::Byte(o));
        Some(self.address(base, &all, PlaceTy::Value(t), n))
    }
    pub(super) fn put_word(
        &mut self,
        base: AddressBase,
        steps: &[Step],
        offset: u64,
        value: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let t = self.b.values[value.index()];
        let a = self.word_at(base, steps, offset, t, n)?;
        self.inst(Inst::Store { address: a, value }, n);
        Some(())
    }
    pub(super) fn get_word(
        &mut self,
        base: AddressBase,
        offset: u64,
        t: Ty,
        n: NodeId,
    ) -> Option<ValueId> {
        let a = self.word_at(base, &[], offset, t, n)?;
        Some(self.read_bits(a, t, n))
    }
    pub(super) fn word_op(
        &mut self,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> ValueId {
        let result = self.value(Ty::Scalar(Scalar::U64));
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
    pub(super) fn test(
        &mut self,
        op: CompareOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> ValueId {
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
    pub(super) fn branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
        let (ye, ne) = (self.edge(yes, &[]), self.edge(no, &[]));
        self.end(
            Terminator::Branch {
                condition,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
    }

    /// The charge and size of a blob of `capacity` elements of `es` bytes: the charge
    /// `capacity × es + 8` rounded up to 8 (MEM-24), the size `HEAD` more. A capacity
    /// no limit can hold saturates both, so the admission fails with `Capacity`.
    pub(super) fn blob_size(
        &mut self,
        capacity: ValueId,
        es: u64,
        n: NodeId,
    ) -> Option<(ValueId, ValueId)> {
        let u64t = Ty::Scalar(Scalar::U64);
        let done = self.new_block(&[u64t, u64t]);
        let limit = (u64::MAX - HEAD - 7) / es.max(1);
        let lim = self.konst(u64t, limit, n)?;
        let over = self.test(CompareOp::Gt, capacity, lim, n);
        let (big, fits) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(over, big, fits, n);
        self.start(big);
        let max = self.konst(u64t, u64::MAX, n)?;
        self.jump(done, &[max, max], n);
        self.start(fits);
        let size = self.konst(u64t, es, n)?;
        let bytes = self.word_op(BinaryOp::Mul, capacity, size, n);
        let fifteen = self.konst(u64t, 15, n)?;
        let up = self.word_op(BinaryOp::Add, bytes, fifteen, n);
        let mask = self.konst(u64t, !7, n)?;
        let charge = self.word_op(BinaryOp::BitAnd, up, mask, n);
        let head = self.konst(u64t, HEAD, n)?;
        let total = self.word_op(BinaryOp::Add, bytes, head, n);
        self.jump(done, &[charge, total], n);
        self.start(done);
        let p = self.block_params(done);
        Some((p[0], p[1]))
    }

    /// Admit a blob of `capacity` elements in `region`: the block (null when refused),
    /// the room left, and the charge asked.
    pub(super) fn region_blob(
        &mut self,
        region: ValueId,
        capacity: ValueId,
        es: u64,
        n: NodeId,
    ) -> Option<(ValueId, ValueId, ValueId)> {
        let (charge, size) = self.blob_size(capacity, es, n)?;
        let block = self.value(Ty::Ptr);
        let available = self.value(Ty::Scalar(Scalar::U64));
        let fail = self.fail_to(n);
        self.inst(
            Inst::Alloc {
                result: block,
                available,
                region_entry: None,
                kind: AllocKind::Plain,
                place: AllocPlace::Region(region),
                charge,
                overflow: None,
                size,
                align: 8,
                fail,
            },
            n,
        );
        Some((block, available, charge))
    }

    /// `Capacity { requested, available }` at `steps` from `base`.
    pub(super) fn capacity_at(
        &mut self,
        base: AddressBase,
        steps: &[Step],
        charge: ValueId,
        available: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let cty = self.capacity_ty(n);
        let id = self.declared(cty)?;
        let at = self.address(base, steps, PlaceTy::Declared(id), n);
        for (i, v) in [charge, available].into_iter().enumerate() {
            let f = self.address(
                AddressBase::Address(at),
                &[Step::Field(i as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            self.inst(
                Inst::Store {
                    address: f,
                    value: v,
                },
                n,
            );
        }
        Some(())
    }

    fn span_with_capacity(
        &mut self,
        region: ValueId,
        count: ValueId,
        es: u64,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.declared(result)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let (block, available, charge) = self.region_blob(region, count, es, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let admitted = self.test(CompareOp::Ne, block, null, n);
        let (yes, no, join) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.branch(admitted, yes, no, n);
        self.start(yes);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let w = self.konst(Ty::Scalar(Scalar::U64), 8, n)?;
        for (offset, v) in [(0, count), (8, w), (16, zero)] {
            self.put_word(AddressBase::Value(block), &[], offset, v, n)?;
        }
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        let ok = [Step::CaseField { case: 0, field: 0 }];
        self.put_word(AddressBase::Address(to), &ok, 0, block, n)?;
        self.put_word(AddressBase::Address(to), &ok, 8, region, n)?;
        self.jump(join, &[], n);
        self.start(no);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        self.capacity_at(
            AddressBase::Address(to),
            &[Step::CaseField { case: 1, field: 0 }],
            charge,
            available,
            n,
        )?;
        self.jump(join, &[], n);
        self.start(join);
        Some(self.pointer(to, n))
    }

    /// T1.6: a push that cannot grow returns `(Capacity, x)` and leaves the builder as
    /// it was; growth moves to `max(4, 2 × capacity)` and leaves the old bytes charged
    /// until the region closes (MEM-3, FAIL-6).
    fn span_push(
        &mut self,
        b: ValueId,
        x: Option<ValueId>,
        elem: TyId,
        es: u64,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let id = self.declared(result)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let blob = self.get_word(AddressBase::Value(b), 0, Ty::Ptr, n)?;
        let region = self.get_word(AddressBase::Value(b), 8, Ty::Ptr, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let empty = self.test(CompareOp::Eq, blob, null, n);
        let sized = self.new_block(&[u64t, u64t]);
        let (none, some) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(empty, none, some, n);
        self.start(none);
        let zero = self.konst(u64t, 0, n)?;
        self.jump(sized, &[zero, zero], n);
        self.start(some);
        let cap = self.get_word(AddressBase::Value(blob), 0, u64t, n)?;
        let count = self.get_word(AddressBase::Value(blob), 16, u64t, n)?;
        self.jump(sized, &[cap, count], n);
        self.start(sized);
        let p = self.block_params(sized);
        let (cap, count) = (p[0], p[1]);
        let put = self.new_block(&[Ty::Ptr]);
        let full = self.test(CompareOp::Eq, count, cap, n);
        let (grow, room) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(full, grow, room, n);
        self.start(room);
        self.jump(put, &[blob], n);
        // Grow into a new blob, or hand `x` back.
        self.start(grow);
        let two = self.konst(u64t, 2, n)?;
        let doubled = self.word_op(BinaryOp::Mul, cap, two, n);
        let four = self.konst(u64t, 4, n)?;
        let new_cap = self.word_op(BinaryOp::Max, doubled, four, n);
        let (fresh, available, charge) = self.region_blob(region, new_cap, es, n)?;
        let admitted = self.test(CompareOp::Ne, fresh, null, n);
        let (moved, refused, end) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.branch(admitted, moved, refused, n);
        self.start(moved);
        let w = self.konst(u64t, 8, n)?;
        for (offset, v) in [(0, new_cap), (8, w), (16, count)] {
            self.put_word(AddressBase::Value(fresh), &[], offset, v, n)?;
        }
        let has = self.test(CompareOp::Ne, count, zero, n);
        let (copy, after) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(has, copy, after, n);
        self.start(copy);
        let size = self.konst(u64t, es, n)?;
        let bytes = self.word_op(BinaryOp::Mul, count, size, n);
        let from = self.word_at(
            AddressBase::Value(blob),
            &[],
            HEAD,
            Ty::Scalar(Scalar::U8),
            n,
        )?;
        let into = self.word_at(
            AddressBase::Value(fresh),
            &[],
            HEAD,
            Ty::Scalar(Scalar::U8),
            n,
        )?;
        self.inst(
            Inst::Copy {
                to: into,
                from,
                bytes,
            },
            n,
        );
        self.jump(after, &[], n);
        self.start(after);
        self.put_word(AddressBase::Value(b), &[], 0, fresh, n)?;
        self.jump(put, &[fresh], n);
        self.start(refused);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let err = Step::CaseField { case: 1, field: 0 };
        self.capacity_at(
            AddressBase::Address(to),
            &[err, Step::Field(0)],
            charge,
            available,
            n,
        )?;
        let back_ty = self.place_ty(elem, n)?;
        let back = self.address(AddressBase::Address(to), &[err, Step::Field(1)], back_ty, n);
        let r = self.rep(elem).ok()?;
        self.install(back, r, x, n);
        self.jump(end, &[], n);
        // Room: encode `x` after the count elements and count it.
        self.start(put);
        let blob = self.block_params(put)[0];
        let count = self.get_word(AddressBase::Value(blob), 16, u64t, n)?;
        let size = self.konst(u64t, es, n)?;
        let bytes = self.word_op(BinaryOp::Mul, count, size, n);
        let head = self.konst(u64t, HEAD, n)?;
        let at = self.word_op(BinaryOp::Add, bytes, head, n);
        self.encode_plain(blob, at, elem, r, x?, n)?;
        let one = self.konst(u64t, 1, n)?;
        let next = self.word_op(BinaryOp::Add, count, one, n);
        self.put_word(AddressBase::Value(blob), &[], 16, next, n)?;
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(end, &[], n);
        self.start(end);
        Some(self.pointer(to, n))
    }

    /// Write plain `x` (a scalar, or a pointer to its place) packed at `blob + at`
    /// (MEM-9: fields in order, no padding, little-endian).
    pub(super) fn encode_plain(
        &mut self,
        blob: ValueId,
        at: ValueId,
        ty: TyId,
        r: Rep,
        x: ValueId,
        n: NodeId,
    ) -> Option<()> {
        match (self.t.kind(ty).clone(), r) {
            (TyKind::Unit, _) => Some(()),
            (_, Rep::Scalar(t)) => {
                let a = self.address(
                    AddressBase::Value(blob),
                    &[Step::Byte(at)],
                    PlaceTy::Value(t),
                    n,
                );
                self.inst(
                    Inst::Store {
                        address: a,
                        value: x,
                    },
                    n,
                );
                Some(())
            }
            (_, Rep::Place(id)) => {
                let whole = self.address(AddressBase::Value(x), &[], PlaceTy::Declared(id), n);
                if let TyKind::Array(elem, count) = *self.t.kind(ty) {
                    let u64t = Ty::Scalar(Scalar::U64);
                    let stride = self.packed_plain_size(elem)?;
                    let stride = self.konst(u64t, stride, n)?;
                    let limit = self.konst(u64t, count, n)?;
                    let zero = self.konst(u64t, 0, n)?;
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
                    let rep = self.rep(elem).ok()?;
                    if let Some(place) = rep.stored() {
                        let from = self.address(
                            AddressBase::Address(whole),
                            &[Step::Field(0), Step::Element(index)],
                            place,
                            n,
                        );
                        let value = match rep {
                            Rep::Scalar(t) => self.read_bits(from, t, n),
                            _ => self.pointer(from, n),
                        };
                        self.encode_plain(blob, off, elem, rep, value, n)?;
                    }
                    let next = self.add_k(index, 1, n)?;
                    let after = self.add(off, stride, n);
                    self.jump(head, &[next, after], n);
                    self.start(done);
                    return Some(());
                }
                let count = self.plain_fields(ty)?;
                let mut offset = 0;
                for i in 0..count {
                    let field = self.field_ty(ty, i)?;
                    let fr = self.rep(field).ok()?;
                    let place = self.place_ty(field, n)?;
                    let from =
                        self.address(AddressBase::Address(whole), &[Step::Field(i)], place, n);
                    let v = match fr {
                        Rep::None => continue,
                        Rep::Scalar(t) => self.read_bits(from, t, n),
                        Rep::Place(_) => self.pointer(from, n),
                    };
                    let k = self.konst(Ty::Scalar(Scalar::U64), offset, n)?;
                    let fat = self.word_op(BinaryOp::Add, at, k, n);
                    self.encode_plain(blob, fat, field, fr, v, n)?;
                    offset += self.packed_plain_size(field)?;
                }
                Some(())
            }
            _ => None,
        }
    }

    fn span_finish(&mut self, b: ValueId, result: TyId, n: NodeId) -> Option<ValueId> {
        let id = self.declared(result)?;
        let blob = self.get_word(AddressBase::Value(b), 0, Ty::Ptr, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let empty = self.test(CompareOp::Eq, blob, null, n);
        let done = self.new_block(&[Ty::Ptr]);
        let (none, some) = (self.new_block(&[]), self.new_block(&[]));
        self.branch(empty, none, some, n);
        self.start(none);
        // The empty span allocates nothing: a static blob of count 0.
        let mut bytes = vec![0u8; HEAD as usize];
        bytes[8] = 8;
        let d = self.e.static_bytes(&bytes);
        let s = self.value(Ty::Ptr);
        self.inst(Inst::ReadStatic { result: s, data: d }, n);
        self.jump(done, &[s], n);
        self.start(some);
        self.jump(done, &[blob], n);
        self.start(done);
        let blob = self.block_params(done)[0];
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let at = self.konst(Ty::Scalar(Scalar::U64), 16, n)?;
        self.put_word(AddressBase::Address(to), &[], 0, blob, n)?;
        self.put_word(AddressBase::Address(to), &[], 8, at, n)?;
        Some(self.pointer(to, n))
    }
}
