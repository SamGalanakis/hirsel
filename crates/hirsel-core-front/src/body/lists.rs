//! Fixed-layout List storage, with portable charges independent of native stride.
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

/// What asks a list for room: it decides the room test and whether growth is cold.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Ask {
    /// A small constant or another list's length.
    Bounded,
    /// `list.reserve(n)` with a caller's n; it usually asks for a few at a time.
    Reserve,
    /// `list.fill(n)` with a caller's n; a fresh list's fill always grows.
    Fill,
}

#[derive(Clone, Copy)]
enum ListStorage {
    Empty,
    Stored,
    Dynamic(ValueId),
}

enum ListGrowth {
    Raise,
    ReturnCapacity(AddressId, TypeId),
    Call { grow: usize, single: bool },
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
    /// Some(true) when the element is known, without emitting code, to occupy
    /// storage; None when the representation is unknown.
    pub(super) fn list_storage_static(&mut self, item: TyId) -> Option<bool> {
        match self.rep(item) {
            Ok(Rep::None) => Some(false),
            Ok(Rep::Scalar(_)) => Some(true),
            Ok(Rep::Place(id)) if !self.e.generic_names.contains_key(&id) => {
                Some(self.e.size(id) != 0)
            }
            _ => None,
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
        let carrier = self.list_carrier(item, n)?;
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
        self.set_carrier_word(value, carrier, SeqWord::Cap, cap, n);
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
    fn list_count_overflow(&mut self, item: TyId, refused: BlockId, n: NodeId) -> Option<()> {
        // FORM60's declined counter-overflow admission: the limit reports its room.
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
        self.jump(refused, &[max, available], n);
        Some(())
    }

    fn word_scalar(&mut self, ty: TyId, n: NodeId) -> Option<Scalar> {
        match *self.t.kind(ty) {
            TyKind::Scalar(s @ (Scalar::U16 | Scalar::U32 | Scalar::U64)) => Some(s),
            _ => {
                self.refuse("SEM-4", n, "a word must be u16, u32 or u64");
                None
            }
        }
    }

    fn word_order(&mut self, word: ValueId, ty: Ty, big_endian: bool, n: NodeId) -> ValueId {
        if !big_endian {
            return word;
        }
        let result = self.value(ty);
        self.inst(
            Inst::ScalarUnary {
                result,
                op: hirsel_core_ir::ScalarUnaryOp::SwapBytes,
                operand: word,
            },
            n,
        );
        result
    }

    /// SEM-4: keep the checked end and unsigned bound in one predicate.
    fn require_word_range(
        &mut self,
        at: ValueId,
        len: ValueId,
        scalar: Scalar,
        n: NodeId,
    ) -> Option<()> {
        let width = self.konst(Ty::Scalar(Scalar::U64), scalar.size(), n)?;
        let (end, overflow) = self.list_arith(ArithOp::Add, ArithMode::Checked, at, width, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, end, len, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, within, n);
        self.list_require_element(valid, n);
        Some(())
    }

    fn read_word(
        &mut self,
        data: ValueId,
        at: ValueId,
        scalar: Scalar,
        big_endian: bool,
        n: NodeId,
    ) -> ValueId {
        let ty = Ty::Scalar(scalar);
        let address = self.address(
            AddressBase::Value(data),
            &[Step::Byte(at)],
            PlaceTy::Value(ty),
            n,
        );
        let word = self.read_bits(address, ty, n);
        self.word_order(word, ty, big_endian, n)
    }

    pub(super) fn list_word(
        &mut self,
        header: ValueId,
        at: ValueId,
        result: TyId,
        big_endian: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let word = self.word_scalar(result, n)?;
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.require_word_range(at, len, word, n)?;
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        Some(self.read_word(data, at, word, big_endian, n))
    }

    /// SEM-4: the word width of an Inline byte array access, after its range
    /// check; an array shorter than the word raises Bounds statically.
    fn inline_word_range(
        &mut self,
        array: TyId,
        at: ValueId,
        word: TyId,
        n: NodeId,
    ) -> Option<Scalar> {
        let len = match *self.t.kind(array) {
            TyKind::Array(item, len) if item == scalar(Scalar::U8) => len,
            _ => {
                self.refuse(
                    "SEM-4",
                    n,
                    "an Inline word access takes an Inline array of u8",
                );
                return None;
            }
        };
        let scalar = self.word_scalar(word, n)?;
        if len < scalar.size() {
            let cleanup = self.fail_to(n);
            self.end(
                Terminator::Raise {
                    cause: hirsel_core_ir::RaiseCause::Bounds,
                    cleanup,
                },
                Some(n),
            );
            return None;
        }
        let bound = self.konst(Ty::Scalar(Scalar::U64), len, n)?;
        self.require_word_range(at, bound, scalar, n)?;
        Some(scalar)
    }

    /// `list.compare_bytes`: one `CompareBytes` pass, its -1, 0 or 1 mapped to the
    /// `Less`, `Equal` or `Greater` case of the result slot.
    pub(super) fn list_compare_bytes(
        &mut self,
        a: ValueId,
        b: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Ok(_) => return None,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let mut args = Vec::with_capacity(4);
        for header in [a, b] {
            args.push(Arg::Value(self.carrier_word(
                header,
                carrier,
                SeqWord::Data,
                n,
            )));
            args.push(Arg::Value(self.carrier_word(
                header,
                carrier,
                SeqWord::Len,
                n,
            )));
        }
        let order = self.runtime_row(hirsel_core_ir::RtOp::CompareBytes, &args, n)?[0];
        let zero = self.konst(Ty::Scalar(Scalar::I8), 0, n)?;
        self.order_of(order, zero, id, result, n)
    }

    /// The `Order` value at type `result` (place row `id`): `Less` when `left < right`,
    /// `Greater` when `left > right`, else `Equal`, for two scalars of one type.
    pub(super) fn order_of(
        &mut self,
        left: ValueId,
        right: ValueId,
        id: TypeId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let to = self.result_slot(PlaceTy::Declared(id), n);
        // Order is `Less | Equal | Greater`, cases 0, 1 and 2. Equal is written before the
        // branch, so the slot's first write dominates both overwrites and the read.
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        let (less, not_less) = (self.new_block(&[]), self.new_block(&[]));
        let greater = self.new_block(&[]);
        let done = self.new_block(&[]);
        let below = self.list_compare(CompareOp::Lt, left, right, n);
        self.list_branch(below, less, not_less, n);
        self.start(not_less);
        let above = self.list_compare(CompareOp::Gt, left, right, n);
        self.list_branch(above, greater, done, n);
        for (block, case) in [(less, 0), (greater, 2)] {
            self.start(block);
            self.inst(Inst::SetCase { address: to, case }, n);
            self.jump(done, &[], n);
        }
        self.start(done);
        let p = self.pointer(to, n);
        self.temp(result, Some(p));
        Some(p)
    }

    pub(super) fn inline_word(
        &mut self,
        array: TyId,
        data: Option<ValueId>,
        at: ValueId,
        result: TyId,
        big_endian: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        let scalar = self.inline_word_range(array, at, result, n)?;
        Some(self.read_word(data?, at, scalar, big_endian, n))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn inline_set_word(
        &mut self,
        array: TyId,
        data: Option<ValueId>,
        at: ValueId,
        word: ValueId,
        input: TyId,
        big_endian: bool,
        n: NodeId,
    ) -> Option<()> {
        let scalar = self.inline_word_range(array, at, input, n)?;
        self.write_word(data?, at, word, scalar, big_endian, n);
        Some(())
    }

    fn write_word(
        &mut self,
        data: ValueId,
        at: ValueId,
        word: ValueId,
        scalar: Scalar,
        big_endian: bool,
        n: NodeId,
    ) {
        let ty = Ty::Scalar(scalar);
        let address = self.address(
            AddressBase::Value(data),
            &[Step::Byte(at)],
            PlaceTy::Value(ty),
            n,
        );
        let value = self.word_order(word, ty, big_endian, n);
        self.inst(Inst::Store { address, value }, n);
    }

    /// `list.set_word_le`/`_be`: Bounds unless `at + width <= len`, before the store.
    pub(super) fn list_set_word(
        &mut self,
        header: ValueId,
        at: ValueId,
        word: ValueId,
        input: TyId,
        big_endian: bool,
        n: NodeId,
    ) -> Option<()> {
        if !self.live() {
            return None;
        }
        let width = self.word_scalar(input, n)?;
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.require_word_range(at, len, width, n)?;
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        self.write_word(data, at, word, width, big_endian, n);
        Some(())
    }

    /// Reserve first so refusal preserves the carrier and all its bytes.
    pub(super) fn list_put_word(
        &mut self,
        header: ValueId,
        word: ValueId,
        input: TyId,
        big_endian: bool,
        n: NodeId,
    ) -> Option<()> {
        let word_scalar = self.word_scalar(input, n)?;
        let ty = Ty::Scalar(word_scalar);
        let Some(carrier) = self.list_carrier(scalar(Scalar::U8), n) else {
            self.gap(n, "put_word without the Bytes carrier row is not built");
            return None;
        };
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let width = self.konst(Ty::Scalar(Scalar::U64), word_scalar.size(), n)?;
        self.list_reserve(scalar(Scalar::U8), header, width, Ask::Bounded, n)?;
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let at = self.address(
            AddressBase::Value(data),
            &[Step::Byte(len)],
            PlaceTy::Value(ty),
            n,
        );
        let word = self.word_order(word, ty, big_endian, n);
        self.inst(
            Inst::Store {
                address: at,
                value: word,
            },
            n,
        );
        let (len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, width, n);
        self.set_carrier_word(header, carrier, SeqWord::Len, len, n);
        Some(())
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
        let list_id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let to = self.result_slot(PlaceTy::Declared(list_id), n);
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
        let empty = self.list_compare(CompareOp::Eq, count, zero, n);
        let empty = if let ListStorage::Dynamic(on) = storage {
            self.list_bool(BinaryOp::BitOr, empty, on, n)
        } else {
            empty
        };
        let yes = self.new_block(&[]);
        let allocate = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(empty, yes, allocate, n);
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
        let admitted = self.list_compare(CompareOp::Ne, block, null, n);
        self.raise_unless(admitted, plan.charge, available, n);
        self.list_allocated(to, list_id, block, count, n);
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    pub(super) fn list_bool(
        &mut self,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        n: NodeId,
    ) -> ValueId {
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
    pub(super) fn list_compare(
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
    pub(super) fn list_branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
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
        self.list_plan_with_logical(item, logical, count, extra, n)
    }

    pub(super) fn list_plan_with_logical(
        &mut self,
        item: TyId,
        logical: ValueId,
        count: ValueId,
        extra: Option<ValueId>,
        n: NodeId,
    ) -> Option<ListPlan> {
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
            },
            n,
        );
        (block, available)
    }

    /// Room for `additional` more elements; a refusal raises `Capacity` and leaves
    /// the list unchanged (MEM-21, MEM-24).
    pub(super) fn list_reserve(
        &mut self,
        item: TyId,
        header: ValueId,
        additional: ValueId,
        ask: Ask,
        n: NodeId,
    ) -> Option<()> {
        self.list_room(item, header, additional, ask, ListGrowth::Raise, n)
    }

    /// `list.reserve`, `list.fill` and `list.append`: [`Self::list_reserve`]
    /// whose growth arm is one cold call to core's `grow_by[T](xs, additional)`,
    /// so every such site shares one growth body per element
    /// type. The callee makes exactly the inline arm's decisions (it is that
    /// arm, expanded once), charges no step (a core body), raises with the
    /// caller's site word, and its failure takes this node's cleanup. A frame
    /// list keeps its inline arm (`grow` would free its frame bytes), and so
    /// do the growth helpers' own bodies.
    pub(super) fn list_reserve_shared(
        &mut self,
        item: TyId,
        header: ValueId,
        additional: ValueId,
        ask: Ask,
        n: NodeId,
    ) -> Option<()> {
        let path = &self.p.fns[self.definition].path;
        let in_growth = path.is_core("list.grow") || path.is_core("list.grow_by");
        let growth = if in_growth || self.frame_list(header).is_some() {
            ListGrowth::Raise
        } else {
            let single = ask == Ask::Reserve && self.growth.recent_one == Some(additional);
            let helper = if single {
                crate::CoreHelper::ListGrow
            } else {
                crate::CoreHelper::ListGrowBy
            };
            self.p
                .core_helper(helper)
                .map_or(ListGrowth::Raise, |grow| ListGrowth::Call { grow, single })
        };
        self.list_room(item, header, additional, ask, growth, n)
    }

    /// `list.try_reserve`: `list_reserve` with the refusal as `Error(c)`.
    pub(super) fn list_try_reserve(
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
        let cap_ty = self.prelude("Capacity", n)?;
        let cap_ty = self.t.mk(TyKind::Adt(cap_ty, vec![]));
        let cap_id = self.place_row(cap_ty, "a capacity without a place is not built", n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.list_room(
            item,
            header,
            additional,
            Ask::Reserve,
            ListGrowth::ReturnCapacity(to, cap_id),
            n,
        )?;
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    /// Test for room before taking the growth path.
    fn list_room(
        &mut self,
        item: TyId,
        header: ValueId,
        additional: ValueId,
        ask: Ask,
        mode: ListGrowth,
        n: NodeId,
    ) -> Option<()> {
        let carrier = self.list_carrier(item, n)?;
        // A stored element has a nonzero size, so len <= isize::MAX; a bounded
        // `additional` (a small constant or another list's length) is too, and
        // len + additional cannot wrap: the room test is one compare.
        let unwrappable =
            ask == Ask::Bounded && matches!(self.list_storage_static(item), Some(true));
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let old_cap = self.carrier_word(header, carrier, SeqWord::Cap, n);
        let (needed, len_over) =
            self.list_arith(ArithOp::Add, ArithMode::Checked, len, additional, n);
        let len_over = len_over?;
        let lacks_room = self.list_compare(CompareOp::Gt, needed, old_cap, n);
        let grow = if unwrappable {
            lacks_room
        } else {
            self.list_bool(BinaryOp::BitOr, lacks_room, len_over, n)
        };
        let growth = self.new_block(&[]);
        let success = self.new_block(&[]);
        // Growth is amortized and rare except for fill: that edge is cold.
        let yes = self.edge(growth, &[]);
        self.b.edges[yes.index()].cold = ask != Ask::Fill;
        let no = self.edge(success, &[]);
        self.end(
            Terminator::Branch {
                condition: grow,
                yes,
                no,
            },
            Some(n),
        );
        self.start(growth);
        if let ListGrowth::Call { grow, single } = mode {
            // Filling a fresh list is construction. Keep that allocation in
            // the caller, where its header and the other live inputs can
            // stay in registers. Growing existing storage still takes the
            // cold shared call.
            let initial = if ask == Ask::Fill {
                let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
                let allocated = self.list_compare(CompareOp::Ne, old_cap, zero, n);
                let existing = self.new_block(&[]);
                let initial = self.new_block(&[]);
                self.list_branch(allocated, existing, initial, n);
                self.start(existing);
                Some(initial)
            } else {
                None
            };
            let args = [Some(header), Some(additional)];
            self.true_arm(true, |cx| {
                cx.emit_call(grow, &[item], &args[..if single { 1 } else { 2 }], UNIT, n)
            });
            self.jump(success, &[], n);
            let Some(initial) = initial else {
                self.start(success);
                return Some(());
            };
            self.start(initial);
        }
        let u64t = Ty::Scalar(Scalar::U64);
        let refused = self.new_block(&[u64t, u64t]);
        let storage = self.list_storage(item, n)?;
        match storage {
            ListStorage::Stored => {}
            ListStorage::Empty => self.list_count_overflow(item, refused, n)?,
            ListStorage::Dynamic(on) => {
                let zero_sized = self.new_block(&[]);
                let stored = self.new_block(&[]);
                self.list_branch(on, zero_sized, stored, n);
                self.start(zero_sized);
                self.list_count_overflow(item, refused, n)?;
                self.start(stored);
            }
        }
        if !matches!(storage, ListStorage::Empty) {
            self.list_grow(
                item,
                header,
                carrier,
                [len, old_cap, needed],
                len_over,
                [success, refused],
                n,
            )?;
        }
        self.start(refused);
        let [requested, available] = self.block_params(refused)[..] else {
            unreachable!("the refusal block takes the charge and the room")
        };
        let ListGrowth::ReturnCapacity(to, cap_id) = mode else {
            self.raise_capacity(requested, available, n);
            self.start(success);
            return Some(());
        };
        let done = self.new_block(&[]);
        self.list_refused(to, cap_id, requested, available, n);
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
        Some(())
    }

    /// Grow the list's storage for `needed` elements, then jump to `success`, or
    /// to `refused(charge, available)` with the list unchanged.
    #[allow(clippy::too_many_arguments)]
    fn list_grow(
        &mut self,
        item: TyId,
        header: ValueId,
        carrier: TypeId,
        [len, old_cap, needed]: [ValueId; 3],
        len_over: ValueId,
        [success, refused]: [BlockId; 2],
        n: NodeId,
    ) -> Option<()> {
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let two = self.konst(Ty::Scalar(Scalar::U64), 2, n)?;
        let four = self.konst(Ty::Scalar(Scalar::U64), 4, n)?;
        let (double, cap_over) = self.list_arith(ArithOp::Mul, ArithMode::Checked, old_cap, two, n);
        let count_over = self.list_bool(BinaryOp::BitOr, len_over, cap_over?, n);
        let new_cap = self.list_max(needed, four, n);
        let new_cap = self.list_max(new_cap, double, n);
        let plan = self.list_plan(item, new_cap, Some(count_over), n)?;
        let owned = self.list_compare(CompareOp::Ne, old_cap, zero, n);
        let current = self.new_block(&[]);
        let like = self.new_block(&[]);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let (moved, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, len, plan.stride, n);
        match self.frame_list(header) {
            Some(f) => self.frame_growth(
                f,
                header,
                carrier,
                &plan,
                [old_cap, new_cap, len, moved],
                owned,
                [like, current],
                success,
                refused,
                n,
            )?,
            None => self.list_branch(owned, like, current, n),
        }
        // Capacity 0 owns no storage (empty, or static bytes): allocate in the
        // current limit and copy the elements over.
        self.start(current);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let copy = self.new_block(&[]);
        let refused_here = self.new_block(&[]);
        self.list_branch(ok, copy, refused_here, n);
        self.start(refused_here);
        self.jump(refused, &[plan.charge, available], n);
        self.start(copy);
        let nonempty = self.list_compare(CompareOp::Ne, len, zero, n);
        let copy_data = self.new_block(&[]);
        let commit = self.new_block(&[]);
        self.list_branch(nonempty, copy_data, commit, n);
        self.start(copy_data);
        let old_data = self.carrier_word(header, carrier, SeqWord::Data, n);
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
        self.set_carrier_word(header, carrier, SeqWord::Data, block, n);
        self.set_carrier_word(header, carrier, SeqWord::Cap, new_cap, n);
        self.jump(success, &[], n);
        // Owned storage is replaced by one `grow` (crate docs, "Growth"), which
        // admits like the old block and refunds its charge after the install.
        self.start(like);
        let list = self.prelude("List", n)?;
        let list = self.t.mk(TyKind::Adt(list, vec![item]));
        let list_id = self.place_row(list, "a list without a place is not built", n)?;
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
            },
            n,
        );
        let grown = self.list_compare(CompareOp::Ne, block, null, n);
        let refused_here = self.new_block(&[]);
        self.list_branch(grown, success, refused_here, n);
        self.start(refused_here);
        self.jump(refused, &[plan.charge, available], n);
        Some(())
    }

    /// `push`, expanded in place: a full test, a cold call to the source `grow` in
    /// its true arm (growth.rs), then the element stored within capacity. The taken
    /// element stays the caller's temporary until the store or the `Error((c, x))`
    /// install consumes it, so a raise from `grow` drops it once (A1 cleanup).
    /// Select storage before evaluating the element. OWN-11 reserves the list
    /// through the argument, so its allocation and length cannot change. Only
    /// the full path uses a local place, and admission remains after evaluation.
    pub(super) fn list_push_destination(
        &mut self,
        item: TyId,
        header: ValueId,
        n: NodeId,
    ) -> Option<(ValueId, AddressId)> {
        let ty = self.nonempty_aggregate(item)?;
        let carrier = self.list_carrier(item, n)?;
        let full = self.full_test(header, carrier, n);
        let slot = self.choose_spare_slot(ty, full, n, |cx| {
            let len = cx.carrier_word(header, carrier, SeqWord::Len, n);
            let element = cx.list_element(item, header, len, n)?;
            cx.pointer_address(element)
        })?;
        Some((full, slot))
    }

    pub(super) fn nonempty_aggregate(&mut self, item: TyId) -> Option<TypeId> {
        let Ok(Rep::Place(ty)) = self.rep(item) else {
            return None;
        };
        (self.live() && !self.e.generic_names.contains_key(&ty) && self.e.size(ty) != 0)
            .then_some(ty)
    }

    pub(super) fn choose_spare_slot(
        &mut self,
        ty: TypeId,
        full: ValueId,
        n: NodeId,
        spare_slot: impl FnOnce(&mut Self) -> Option<AddressId>,
    ) -> Option<AddressId> {
        let local = self.new_block(&[]);
        let spare = self.new_block(&[]);
        let chosen = self.new_block(&[Ty::Ptr]);
        self.list_branch(full, local, spare, n);
        let place = PlaceTy::Declared(ty);
        self.start(local);
        let at = self.new_slot(place, n);
        let pointer = self.pointer(at, n);
        self.jump(chosen, &[pointer], n);
        self.start(spare);
        let at = spare_slot(self)?;
        let pointer = self.pointer(at, n);
        self.jump(chosen, &[pointer], n);
        self.start(chosen);
        let pointer = self.block_params(chosen)[0];
        Some(self.address(AddressBase::Value(pointer), &[], place, n))
    }

    pub(super) fn list_push(
        &mut self,
        item: TyId,
        header: ValueId,
        value: Option<ValueId>,
        n: NodeId,
        destination: Option<(ValueId, bool)>,
    ) -> Option<ValueId> {
        let Some(carrier) = self.list_carrier(item, n) else {
            self.gap(n, "a push onto a List without its carrier row is not built");
            return None;
        };
        let rep = self.rep_at(item, n)?;
        // `list.grow` is a sibling read of `list.push` (`crate::SIBLING_READS`).
        let Some(grow) = self.p.core_helper(crate::CoreHelper::ListGrow) else {
            self.gap(n, "push without core's list.grow is not built");
            return None;
        };
        let (full, direct) =
            destination.unwrap_or_else(|| (self.full_test(header, carrier, n), false));
        let (grow_block, store) = (self.new_block(&[]), self.new_block(&[]));
        let publish = if direct { self.new_block(&[]) } else { store };
        let cold = self.is_full_test(Some(full));
        let yes = self.edge(grow_block, &[]);
        self.b.edges[yes.index()].cold = cold;
        let no = self.edge(publish, &[]);
        self.end(
            Terminator::Branch {
                condition: full,
                yes,
                no,
            },
            Some(n),
        );
        self.start(grow_block);
        // A frame list grows here: `grow`'s body would free its frame bytes. A
        // refusal raises with the element still the caller's, so cleanup drops it.
        if self.frame_list(header).is_some() {
            let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
            self.list_reserve(item, header, one, Ask::Bounded, n)?;
        } else {
            self.true_arm(cold, |cx| {
                cx.emit_call(grow, &[item], &[Some(header)], UNIT, n);
                Some(())
            })?;
        }
        self.jump(store, &[], n);
        self.start(store);
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        if let Some(place) = rep.stored() {
            let element = self.list_element(item, header, len, n)?;
            let at = self.address(AddressBase::Value(element), &[], place, n);
            self.install(at, rep, value, n);
        }
        let len = if direct {
            self.jump(publish, &[], n);
            self.start(publish);
            self.consume(value, n);
            self.carrier_word(header, carrier, SeqWord::Len, n)
        } else {
            len
        };
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        let (new_len, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, one, n);
        self.set_carrier_word(header, carrier, SeqWord::Len, new_len, n);
        None
    }

    pub(super) fn list_append(
        &mut self,
        item: TyId,
        header: ValueId,
        source: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let carrier = self.list_carrier(item, n)?;
        let old_len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let count = self.carrier_word(source, carrier, SeqWord::Len, n);
        self.list_reserve_shared(item, header, count, Ask::Bounded, n)?;
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let copying = self.list_compare(CompareOp::Ne, count, zero, n);
        let copy = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(copying, copy, done, n);
        self.start(copy);
        let target = self.list_element(item, header, old_len, n)?;
        let data = self.carrier_word(source, carrier, SeqWord::Data, n);
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
        self.set_carrier_word(header, carrier, SeqWord::Len, len, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }

    pub(super) fn list_copy_from_slice(
        &mut self,
        item: TyId,
        dst: ValueId,
        at: ValueId,
        src: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(dst, carrier, SeqWord::Len, n);
        let count = self.carrier_word(src, carrier, SeqWord::Len, n);
        self.require_copy_range(at, count, len, n);
        let dst = self.carrier_word(dst, carrier, SeqWord::Data, n);
        let src = self.carrier_word(src, carrier, SeqWord::Data, n);
        self.copy_plain_range(item, dst, at, src, None, count, n)
    }

    /// `list.copy_within`: Bounds unless both `src + count` and `dst + count`
    /// fit `len` without overflow, checked before any write; then one memmove,
    /// so overlapping ranges copy as if through a temporary.
    pub(super) fn list_copy_within(
        &mut self,
        item: TyId,
        header: ValueId,
        src: ValueId,
        dst: ValueId,
        count: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.require_copy_range(src, count, len, n);
        self.require_copy_range(dst, count, len, n);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        self.copy_plain_range(item, data, dst, data, Some(src), count, n)
    }

    /// `list.fill_slice`: Bounds unless `at + count <= len`, checked before any
    /// store; then one store of `value` per element of the range, ascending.
    pub(super) fn list_fill_slice(
        &mut self,
        item: TyId,
        header: ValueId,
        at: ValueId,
        count: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<()> {
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.require_copy_range(at, count, len, n);
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
        let u64_ = Ty::Scalar(Scalar::U64);
        let stride = self.layout_size(item, false, n)?;
        let zero = self.konst(u64_, 0, n)?;
        let one = self.konst(u64_, 1, n)?;
        let empty = self.list_compare(CompareOp::Eq, stride, zero, n);
        let setup = self.new_block(&[]);
        let test = self.new_block(&[u64_]);
        let store = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(empty, done, setup, n);
        self.start(setup);
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
        let (end, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, at, count, n);
        self.jump(test, &[at], n);
        self.start(test);
        let index = self.block_params(test)[0];
        let more = self.list_compare(CompareOp::Lt, index, end, n);
        self.list_branch(more, store, done, n);
        self.start(store);
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, index, stride, n);
        let target = self.address(AddressBase::Value(data), &[Step::Byte(offset)], place, n);
        self.install(target, rep, value, n);
        let (next, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, index, one, n);
        self.jump(test, &[next], n);
        self.start(done);
        Some(())
    }

    pub(super) fn inline_copy_from_slice(
        &mut self,
        array: TyId,
        dst: Option<ValueId>,
        at: ValueId,
        src: Option<ValueId>,
        count: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let TyKind::Array(item, len) = *self.t.kind(array) else {
            return None;
        };
        let bound = self.konst(Ty::Scalar(Scalar::U64), len, n)?;
        // Both arrays have the same length, so the destination range check also
        // proves that count fits the source prefix.
        self.require_copy_range(at, count, bound, n);
        if len != 0 && self.list_storage_static(item) != Some(false) {
            self.copy_plain_range(item, dst?, at, src?, None, count, n)?;
        }
        Some(())
    }

    /// `inline.copy_within`: the same bounds as `list.copy_within` against the
    /// static length, then one memmove inside the array.
    pub(super) fn inline_copy_within(
        &mut self,
        array: TyId,
        data: Option<ValueId>,
        src: ValueId,
        dst: ValueId,
        count: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let TyKind::Array(item, len) = *self.t.kind(array) else {
            return None;
        };
        let bound = self.konst(Ty::Scalar(Scalar::U64), len, n)?;
        self.require_copy_range(src, count, bound, n);
        self.require_copy_range(dst, count, bound, n);
        if len != 0 && self.list_storage_static(item) != Some(false) {
            let data = data?;
            self.copy_plain_range(item, data, dst, data, Some(src), count, n)?;
        }
        Some(())
    }

    pub(super) fn xxh3_short(
        &mut self,
        input: ValueId,
        inline_len: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let (data, len) = match inline_len {
            Some(len) => (input, len),
            None => {
                let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
                (
                    self.read_sequence_word(input, 0, Ty::Ptr, n),
                    self.carrier_word(input, carrier, SeqWord::Len, n),
                )
            }
        };
        let max = self.konst(u64_, 240, n)?;
        let valid = self.list_compare(CompareOp::Le, len, max, n);
        self.list_require_element(valid, n);
        Some(
            self.runtime_row(
                hirsel_core_ir::RtOp::Xxh3Short,
                &[Arg::Value(data), Arg::Value(len)],
                n,
            )?[0],
        )
    }

    pub(super) fn xxh3_stripes(
        &mut self,
        acc: ValueId,
        header: ValueId,
        secret: ValueId,
        offset: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let mask = self.konst(u64_, 63, n)?;
        let zero = self.konst(u64_, 0, n)?;
        let tail = self.value(u64_);
        self.inst(
            Inst::Binary {
                result: tail,
                op: BinaryOp::BitAnd,
                left: len,
                right: mask,
                fail: None,
            },
            n,
        );
        let whole = self.list_compare(CompareOp::Eq, tail, zero, n);
        self.list_require_element(whole, n);

        let key_end = self.konst(u64_, 128, n)?;
        let offset_in_range = self.list_compare(CompareOp::Le, offset, key_end, n);
        self.list_require_element(offset_in_range, n);
        let nonempty = self.list_compare(CompareOp::Ne, len, zero, n);
        let stripes = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(nonempty, stripes, done, n);
        self.start(stripes);

        let one = self.konst(u64_, 1, n)?;
        let six = self.konst(u64_, 6, n)?;
        let eight = self.konst(u64_, 8, n)?;
        let count = self.value(u64_);
        let fail = self.fail_to(n);
        self.inst(
            Inst::Binary {
                result: count,
                op: BinaryOp::Shr,
                left: len,
                right: six,
                fail,
            },
            n,
        );
        let (last, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, count, one, n);
        let (advance, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, last, eight, n);
        self.require_copy_range(offset, advance, key_end, n);
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        self.xxh3_stripe_row(acc, data, secret, offset, count, n)?;
        self.jump(done, &[], n);
        self.start(done);
        None
    }

    pub(super) fn xxh3_stripe(
        &mut self,
        acc: ValueId,
        data: ValueId,
        secret: ValueId,
        offset: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let key_end = self.konst(u64_, 128, n)?;
        let offset_in_range = self.list_compare(CompareOp::Le, offset, key_end, n);
        self.list_require_element(offset_in_range, n);
        let one = self.konst(u64_, 1, n)?;
        self.xxh3_stripe_row(acc, data, secret, offset, one, n)?;
        None
    }

    fn xxh3_stripe_row(
        &mut self,
        acc: ValueId,
        data: ValueId,
        secret: ValueId,
        offset: ValueId,
        count: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let key = self.address(AddressBase::Value(secret), &[Step::Byte(offset)], byte, n);
        let key = self.pointer(key, n);
        self.runtime_row(
            hirsel_core_ir::RtOp::Xxh3Stripes,
            &[
                Arg::Value(acc),
                Arg::Value(data),
                Arg::Value(key),
                Arg::Value(count),
            ],
            n,
        )?;
        Some(())
    }

    /// Extend over one chunk per step so a budget can stop large checksums.
    pub(super) fn crc32c_update(
        &mut self,
        crc: ValueId,
        header: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u32_ = Ty::Scalar(Scalar::U32);
        let u64_ = Ty::Scalar(Scalar::U64);
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let len = self.read_sequence_word(header, 8, u64_, n);
        let zero = self.konst(u64_, 0, n)?;
        let limit = self.konst(u64_, 16 * 1024, n)?;
        let head = self.new_block(&[u64_, u32_]);
        let chunk = self.new_block(&[]);
        let update = self.new_block(&[u64_]);
        let done = self.new_block(&[]);
        self.jump(head, &[zero, crc], n);

        self.start(head);
        let params = self.block_params(head);
        let (offset, crc) = (params[0], params[1]);
        let more = self.list_compare(CompareOp::Lt, offset, len, n);
        self.list_branch(more, chunk, done, n);

        self.start(chunk);
        let fail = self.fail_to(n);
        self.inst(Inst::Step { fail }, n);
        let (left, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, len, offset, n);
        let full = self.list_compare(CompareOp::Ge, left, limit, n);
        let yes = self.edge(update, &[limit]);
        let no = self.edge(update, &[left]);
        self.end(
            Terminator::Branch {
                condition: full,
                yes,
                no,
            },
            Some(n),
        );

        self.start(update);
        let count = self.block_params(update)[0];
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let from = self.address(AddressBase::Value(data), &[Step::Byte(offset)], byte, n);
        let from = self.pointer(from, n);
        let next = self.runtime_row(
            hirsel_core_ir::RtOp::Crc32c,
            &[Arg::Value(crc), Arg::Value(from), Arg::Value(count)],
            n,
        )?[0];
        let (after, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, offset, count, n);
        self.jump(head, &[after, next], n);
        self.start(done);
        Some(crc)
    }

    pub(super) fn sha1_blocks(
        &mut self,
        state: ValueId,
        header: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.runtime_row(
            hirsel_core_ir::RtOp::Sha1Blocks,
            &[Arg::Value(state), Arg::Value(data), Arg::Value(len)],
            n,
        )?;
        None
    }

    /// `sha256.blocks`: Bounds unless `at + 64 × count ≤ len`, checked before
    /// the state changes; no data address is formed for zero blocks.
    pub(super) fn sha256_blocks(
        &mut self,
        state: ValueId,
        header: ValueId,
        at: ValueId,
        count: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let u64_ = Ty::Scalar(Scalar::U64);
        let carrier = self.list_carrier(scalar(Scalar::U8), n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let block = self.konst(u64_, 64, n)?;
        let (bytes, overflow) = self.list_arith(ArithOp::Mul, ArithMode::Checked, count, block, n);
        let no_overflow = self.emit_unary(UnaryOp::Not, BOOL, overflow, n)?;
        let within = self.list_compare(CompareOp::Le, at, len, n);
        let (room, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, len, at, n);
        let fits = self.list_compare(CompareOp::Le, bytes, room, n);
        let valid = self.list_bool(BinaryOp::BitAnd, within, fits, n);
        let valid = self.list_bool(BinaryOp::BitAnd, no_overflow, valid, n);
        self.list_require_element(valid, n);
        let zero = self.konst(u64_, 0, n)?;
        let compressing = self.list_compare(CompareOp::Ne, count, zero, n);
        let compress = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(compressing, compress, done, n);
        self.start(compress);
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let from = self.address(AddressBase::Value(data), &[Step::Byte(at)], byte, n);
        let from = self.pointer(from, n);
        self.runtime_row(
            hirsel_core_ir::RtOp::Sha256Blocks,
            &[Arg::Value(state), Arg::Value(from), Arg::Value(count)],
            n,
        )?;
        self.jump(done, &[], n);
        self.start(done);
        None
    }

    /// `sha256.block`: one block from an Inline array; nothing to check.
    pub(super) fn sha256_block(
        &mut self,
        state: ValueId,
        data: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
        self.runtime_row(
            hirsel_core_ir::RtOp::Sha256Blocks,
            &[Arg::Value(state), Arg::Value(data), Arg::Value(one)],
            n,
        )?;
        None
    }

    fn require_copy_range(&mut self, at: ValueId, count: ValueId, len: ValueId, n: NodeId) {
        let within = self.list_compare(CompareOp::Le, at, len, n);
        let (room, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, len, at, n);
        let fits = self.list_compare(CompareOp::Le, count, room, n);
        let valid = self.list_bool(BinaryOp::BitAnd, within, fits, n);
        self.list_require_element(valid, n);
    }

    /// Copies `count` plain elements from `src` (at element `within`, or its
    /// start) to `dst` at element `at`. A `within` offset means both ranges lie
    /// in one carrier and may overlap, so the bytes move as one memmove.
    #[allow(clippy::too_many_arguments)]
    fn copy_plain_range(
        &mut self,
        item: TyId,
        dst: ValueId,
        at: ValueId,
        src: ValueId,
        within: Option<ValueId>,
        count: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let stride = self.layout_size(item, false, n)?;
        let (bytes, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, count, stride, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let copying = self.list_compare(CompareOp::Ne, bytes, zero, n);
        let copy = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(copying, copy, done, n);
        self.start(copy);
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, at, stride, n);
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        let to = self.address(AddressBase::Value(dst), &[Step::Byte(offset)], byte, n);
        match within {
            None => {
                let from = self.address(AddressBase::Value(src), &[], byte, n);
                self.inst(Inst::Copy { to, from, bytes }, n);
            }
            Some(within) => {
                let (offset, _) =
                    self.list_arith(ArithOp::Mul, ArithMode::Wrapping, within, stride, n);
                let from = self.address(AddressBase::Value(src), &[Step::Byte(offset)], byte, n);
                self.inst(Inst::Move { to, from, bytes }, n);
            }
        }
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }

    pub(super) fn list_fill(
        &mut self,
        item: TyId,
        header: ValueId,
        count: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<()> {
        let carrier = self.list_carrier(item, n)?;
        let old_len = self.carrier_word(header, carrier, SeqWord::Len, n);
        self.list_reserve_shared(item, header, count, Ask::Fill, n)?;
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        let filling = self.list_compare(CompareOp::Ne, count, zero, n);
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
            let data = self.carrier_word(header, carrier, SeqWord::Data, n);
            let stride = self.layout_size(item, false, n)?;
            let empty = self.list_compare(CompareOp::Eq, stride, zero, n);
            let setup = self.new_block(&[]);
            self.list_branch(empty, commit, setup, n);
            self.start(setup);
            let (offset, _) =
                self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_len, stride, n);
            let base = self.address(AddressBase::Value(data), &[Step::Byte(offset)], place, n);
            let base = self.pointer(base, n);
            if let (Rep::Scalar(ty @ Ty::Scalar(s)), Some(value)) = (rep, value)
                && !matches!(s, Scalar::Bool | Scalar::Char | Scalar::F32 | Scalar::F64)
            {
                // A large fill with integer zero writes the same bytes through
                // rt.zero_bytes, which leaves never-backed pages untouched
                // (hirsel-core-ir crate docs, "Zero fill").
                let (bytes, _) =
                    self.list_arith(ArithOp::Mul, ArithMode::Wrapping, count, stride, n);
                let least =
                    self.konst(Ty::Scalar(Scalar::U64), hirsel_core_ir::ZERO_FILL_MIN, n)?;
                let large = self.list_compare(CompareOp::Ge, bytes, least, n);
                let none = self.konst(ty, 0, n)?;
                let zeroes = self.list_compare(CompareOp::Eq, value, none, n);
                let both = self.list_bool(BinaryOp::BitAnd, large, zeroes, n);
                let zero_fill = self.new_block(&[]);
                let stores = self.new_block(&[]);
                self.list_branch(both, zero_fill, stores, n);
                self.start(zero_fill);
                self.runtime_row(
                    hirsel_core_ir::RtOp::ZeroBytes,
                    &[Arg::Value(data), Arg::Value(base), Arg::Value(bytes)],
                    n,
                )?;
                self.jump(commit, &[], n);
                self.start(stores);
            }
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
        self.set_carrier_word(header, carrier, SeqWord::Len, len, n);
        self.jump(done, &[], n);
        self.start(done);
        Some(())
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
        let carrier = self.list_carrier(item, n)?;
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
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
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
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
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
        self.set_carrier_word(value, carrier, SeqWord::Data, data, n);
        // Capacity first: a typed len store is checked against it (FORM #113).
        self.set_carrier_word(value, carrier, SeqWord::Cap, count, n);
        self.set_carrier_word(value, carrier, SeqWord::Len, count, n);
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
        let carrier = self.list_carrier(item, n)?;
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
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
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
        self.set_carrier_word(header, carrier, SeqWord::Len, last, n);
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
        let Some(carrier) = self.list_carrier(item, n) else {
            return;
        };
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n).unwrap();
        // The carrier retains its allocation, while a drain owns the old elements.
        // Advancing that drain before each drop also resumes correctly on failure.
        self.set_carrier_word(header, carrier, SeqWord::Len, zero, n);
        if self.drop_id(item).is_some() {
            let data = self.carrier_word(header, carrier, SeqWord::Data, n);
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

    /// `list.truncate`: when `keep < len`, the length drops to `keep` first and a
    /// drain then cleans the cut elements in ascending index order, as
    /// `list.clear` does; the capacity stays. Plain elements cost no loop.
    pub(super) fn list_truncate(&mut self, item: TyId, header: ValueId, keep: ValueId, n: NodeId) {
        let Some(carrier) = self.list_carrier(item, n) else {
            return;
        };
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let shorter = self.list_compare(CompareOp::Lt, keep, len, n);
        let cut = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.list_branch(shorter, cut, done, n);
        self.start(cut);
        self.set_carrier_word(header, carrier, SeqWord::Len, keep, n);
        if self.drop_id(item).is_some() {
            let data = self.carrier_word(header, carrier, SeqWord::Data, n);
            let next = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::U64)), n);
            self.inst(
                Inst::Store {
                    address: next,
                    value: keep,
                },
                n,
            );
            let mark = self.owner_mark();
            self.drain_owner(data, next, len, item);
            self.end_owners(mark, None, false, n);
        }
        self.jump(done, &[], n);
        self.start(done);
    }

    /// The old occupant keeps custody through argument evaluation. Save its
    /// inline bytes and restore them if evaluation leaves the call before commit.
    /// An invalid index still evaluates the item in a local place.
    pub(super) fn list_replacement(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<patterns::SlotReplacement> {
        let ty = self.nonempty_aggregate(item)?;
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let present = self.list_compare(CompareOp::Lt, index, len, n);
        let place = PlaceTy::Declared(ty);
        let saved = self.new_slot(place, n);
        let valid = self.new_block(&[]);
        let invalid = self.new_block(&[]);
        let chosen = self.new_block(&[Ty::Ptr]);
        self.list_branch(present, valid, invalid, n);
        self.start(valid);
        let pointer = self.list_element(item, header, index, n)?;
        self.copy_to(saved, pointer, ty, n);
        self.jump(chosen, &[pointer], n);
        self.start(invalid);
        let local = self.new_slot(place, n);
        let pointer = self.pointer(local, n);
        self.jump(chosen, &[pointer], n);
        self.start(chosen);
        let pointer = self.block_params(chosen)[0];
        let replacement = patterns::SlotReplacement {
            slot: self.address(AddressBase::Value(pointer), &[], place, n),
            previous: self.pointer(saved, n),
            present,
            ty,
        };
        self.restore_on_exit(replacement);
        Some(replacement)
    }

    pub(super) fn finish_list_replacement(
        &mut self,
        item: TyId,
        replacement: patterns::SlotReplacement,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        self.commit_restored_slot();
        if !self.live() {
            return None;
        }
        self.list_require_element(replacement.present, n);
        self.install(replacement.slot, Rep::Place(replacement.ty), value, n);
        let old = match self.expression_destination(n) {
            Some(to) if self.b.addresses[to.index()].ty == PlaceTy::Declared(replacement.ty) => {
                self.copy_to(to, replacement.previous, replacement.ty, n);
                self.pointer(to, n)
            }
            _ => replacement.previous,
        };
        self.temp(item, Some(old));
        Some(old)
    }

    pub(super) fn list_set(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
        let in_bounds = self.list_compare(CompareOp::Lt, index, len, n);
        self.list_require_element(in_bounds, n);
        let pointer = self.list_element(item, header, index, n)?;
        self.replace_value(item, Some(pointer), value, n)
    }

    fn list_require_element(&mut self, in_bounds: ValueId, n: NodeId) {
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
        let carrier = self.list_carrier(item, n)?;
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
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

    pub(super) fn list_element(
        &mut self,
        item: TyId,
        header: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let carrier = self.list_carrier(item, n)?;
        let data = self.carrier_word(header, carrier, SeqWord::Data, n);
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
        let carrier = self.list_carrier(item, n)?;
        let id = match self.rep(result) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let out = self.new_slot(PlaceTy::Declared(id), n);
        let len = self.carrier_word(header, carrier, SeqWord::Len, n);
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
        // A zero-sized element has no stored place, so `Some` is its case alone (#60).
        if let Some(place) = self.rep_at(item, n)?.stored() {
            let at = self.address(
                AddressBase::Address(out),
                &[Step::CaseField { case: 1, field: 0 }],
                place,
                n,
            );
            self.install_view(at, item, pointer, n)?;
        }
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
        let pointer = self.pointer(to, n);
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n).unwrap();
        self.set_carrier_word(pointer, list_id, SeqWord::Data, data, n);
        self.set_carrier_word(pointer, list_id, SeqWord::Len, zero, n);
        self.set_carrier_word(pointer, list_id, SeqWord::Cap, capacity, n);
    }
}
