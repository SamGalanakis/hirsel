//! Map carrier/kernel sequences. Probing belongs to core/map.hirsel.
use super::*;
use hirsel_core_layout::map_index as idx;

/// The entry tuple uses exactly map_entry's meta/key/value layout. Keeping
/// field steps symbolic lets the ordinary template materializer relocate K and V.
#[derive(Clone, Copy)]
pub(super) struct MapEntry {
    ty: TyId,
    place: TypeId,
    collection: TyId,
}

#[derive(Clone, Copy)]
pub(super) struct MapInsertion {
    full: ValueId,
    existing: ValueId,
    index: ValueId,
    hash: ValueId,
    crowded: ValueId,
    pub(super) replacement: Option<patterns::SlotReplacement>,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn map_key_destination(
        &mut self,
        types: &[TyId; 2],
        header: ValueId,
        n: NodeId,
    ) -> Option<AddressId> {
        let ty = self.nonempty_aggregate(types[0])?;
        let (count, full) = self.map_room(header, n);
        self.choose_spare_slot(ty, full, n, |cx| {
            cx.map_field(types, header, count, 1, PlaceTy::Declared(ty), n)
        })
    }

    pub(super) fn map_insertion(
        &mut self,
        types: &[TyId; 2],
        header: ValueId,
        key: Option<ValueId>,
        construct_value: bool,
        n: NodeId,
    ) -> Option<MapInsertion> {
        if !self.live() {
            return None;
        }
        let probe = self.p.core_helper(crate::CoreHelper::MapProbeStaged)?;
        let probe_ty = self.t.mk(TyKind::Tuple(vec![
            scalar(Scalar::U64),
            scalar(Scalar::U64),
            BOOL,
            BOOL,
        ]));
        let value = self.emit_call(probe, types, &[Some(header), key], probe_ty, n)?;
        let id = self.place_row(probe_ty, "a map probe requires a tuple", n)?;
        let at = self.address(AddressBase::Value(value), &[], PlaceTy::Declared(id), n);
        let (u, boolean) = (Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::Bool));
        let [hash, index, existing, crowded] =
            [(0, u), (1, u), (2, boolean), (3, boolean)].map(|(i, t)| {
                let field = self.address(
                    AddressBase::Address(at),
                    &[Step::Field(i)],
                    PlaceTy::Value(t),
                    n,
                );
                self.read_bits(field, t, n)
            });
        let (count, full) = self.map_room(header, n);
        let replacement = if construct_value {
            self.prepare_map_value(types, header, count, full, existing, index, n)
        } else {
            None
        };
        Some(MapInsertion {
            full,
            existing,
            index,
            hash,
            crowded,
            replacement,
        })
    }

    fn map_room(&mut self, header: ValueId, n: NodeId) -> (ValueId, ValueId) {
        let u = Ty::Scalar(Scalar::U64);
        let count = self.read_sequence_word(header, 8, u, n);
        let cap = self.read_sequence_word(header, 16, u, n);
        (count, self.list_compare(CompareOp::Eq, count, cap, n))
    }

    fn map_field(
        &mut self,
        types: &[TyId; 2],
        header: ValueId,
        index: ValueId,
        field: u32,
        place: PlaceTy,
        n: NodeId,
    ) -> Option<AddressId> {
        let collection = self.map_type(types)?;
        let entry = self.map_entry_at(header, collection, index, n)?;
        Some(self.address(AddressBase::Address(entry), &[Step::Field(field)], place, n))
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_map_value(
        &mut self,
        types: &[TyId; 2],
        header: ValueId,
        count: ValueId,
        full: ValueId,
        existing: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<patterns::SlotReplacement> {
        let ty = self.nonempty_aggregate(types[1])?;
        let place = PlaceTy::Declared(ty);
        let saved = self.new_slot(place, n);
        let old = self.new_block(&[]);
        let absent = self.new_block(&[]);
        let chosen = self.new_block(&[Ty::Ptr]);
        self.list_branch(existing, old, absent, n);
        self.start(old);
        let address = self.map_field(types, header, index, 2, place, n)?;
        let pointer = self.pointer(address, n);
        self.copy_to(saved, pointer, ty, n);
        self.jump(chosen, &[pointer], n);
        self.start(absent);
        let address = self.choose_spare_slot(ty, full, n, |cx| {
            cx.map_field(types, header, count, 2, place, n)
        })?;
        let pointer = self.pointer(address, n);
        self.jump(chosen, &[pointer], n);
        self.start(chosen);
        let pointer = self.block_params(chosen)[0];
        let replacement = patterns::SlotReplacement {
            slot: self.address(AddressBase::Value(pointer), &[], place, n),
            previous: self.pointer(saved, n),
            present: existing,
            ty,
        };
        self.restore_on_exit(replacement);
        Some(replacement)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_map_insertion(
        &mut self,
        insertion: MapInsertion,
        key_at: Option<AddressId>,
        f: usize,
        types: &[TyId; 2],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if insertion.replacement.is_some() {
            self.commit_restored_slot();
        }
        if !self.live() {
            return None;
        }
        let header = vals[0]?;
        let key = vals[1];
        let value = vals[2];
        let direct = |cx: &Self, value: Option<ValueId>, at: Option<AddressId>| {
            at.is_some() && value.and_then(|v| cx.pointer_address(v)) == at
        };
        let direct_key = direct(self, key, key_at);
        let direct_value = direct(self, value, insertion.replacement.map(|r| r.slot));
        let owners = [self.temp_index(key), self.temp_index(value)].map(|i| {
            i.map(|i| {
                let cleanup::Owner::Temp(temp) = self.owners[i] else {
                    unreachable!("the index names a temporary");
                };
                (i, temp.avail)
            })
        });
        let replace = self.new_block(&[]);
        let absent = self.new_block(&[]);
        let full = self.new_block(&[]);
        let spare = self.new_block(&[]);
        let join = self.new_result_join();
        self.list_branch(insertion.existing, replace, absent, n);
        self.start(absent);
        self.list_branch(insertion.full, full, spare, n);
        self.start(full);
        let built = self.emit_call(f, types, vals, result, n);
        self.goto(join, result, built, n);
        for &(owner, state) in owners.iter().flatten() {
            self.set_temp(owner, state);
        }
        self.start(spare);
        let (count, _) = self.map_room(header, n);
        self.map_append(
            types,
            header,
            count,
            [key, value],
            [direct_key, direct_value],
            n,
        )?;
        let vacant = self.new_block(&[]);
        let crowded = self.new_block(&[]);
        let committed = self.new_block(&[]);
        self.list_branch(insertion.crowded, crowded, vacant, n);
        self.start(crowded);
        let commit = self.p.core_helper(crate::CoreHelper::MapCommitCrowded)?;
        self.emit_call(commit, types, &[Some(header), key], UNIT, n);
        self.jump(committed, &[], n);
        self.start(vacant);
        let commit = self.p.core_helper(crate::CoreHelper::MapCommit)?;
        self.emit_call(
            commit,
            types,
            &[Some(header), Some(insertion.index), Some(insertion.hash)],
            UNIT,
            n,
        );
        self.jump(committed, &[], n);
        self.start(committed);
        let built = self.wrap_case(result, 0, UNIT, None, n);
        self.goto(join, result, built, n);
        for &(owner, state) in owners.iter().flatten() {
            self.set_temp(owner, state);
        }
        self.start(replace);
        if let Some(replacement) = insertion.replacement {
            self.install(replacement.slot, Rep::Place(replacement.ty), value, n);
            let mark = self.owner_mark();
            self.temp(types[1], Some(replacement.previous));
            self.end_owners(mark, None, true, n);
            if let Some(key) = key
                && let Some(id) = self.drop_id(types[0])
            {
                self.consume(Some(key), n);
                self.emit_drop(AddressBase::Value(key), id, n);
            }
        } else {
            let built = self.emit_call(f, types, vals, result, n);
            self.goto(join, result, built, n);
            return self.land(join);
        }
        self.consume(key, n);
        let built = self.wrap_case(result, 0, UNIT, None, n);
        self.goto(join, result, built, n);
        self.land(join)
    }

    pub(super) fn map_value(
        &mut self,
        name: &str,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        match name {
            "new" => Some(self.empty_list(result, n)),
            "len" => {
                let entry = self.map_entry(types[0], types[1], n)?;
                Some(self.map_len(vals[0]?, entry, n))
            }
            "push_entry" => Some(self.map_push(types, vals, n)),
            "compact" => {
                let entry = self.map_entry(types[0], types[1], n)?;
                let header = vals[0]?;
                let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
                let cap = self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n);
                let stride = self.layout_size(entry.ty, false, n)?;
                self.map_relocate(header, entry, data, cap, data, cap, stride, stride, None, n)?;
                Some(None)
            }
            "copy_entry" => {
                let header = vals[0]?;
                let entry = self.map_entry(types[0], types[1], n)?;
                let index = self.read_sequence_word(header, 8, Ty::Scalar(Scalar::U64), n);
                self.map_append(types, header, index, [vals[2], vals[3]], [false; 2], n)?;
                let at = self.map_entry_meta(header, entry, index, n)?;
                self.map_store(at, vals[1]?, n);
                self.map_advance(header, entry, n)?;
                Some(None)
            }
            "advance" => {
                let entry = self.map_entry(types[0], types[1], n)?;
                self.map_advance(vals[0]?, entry, n)?;
                Some(None)
            }
            "strong_empty" | "set_strong_empty" => {
                let carrier = vals[0]?;
                let marker = self.konst(Ty::Ptr, idx::STRONG_EMPTY, n)?;
                if name == "strong_empty" {
                    let data = self.read_sequence_word(carrier, 0, Ty::Ptr, n);
                    Some(Some(self.map_compare(CompareOp::Eq, data, marker, n)))
                } else {
                    self.sequence_word(carrier, 0, Ty::Ptr, marker, n);
                    Some(None)
                }
            }
            "mode" | "group_mask" | "set_strong" | "clear_index" | "group_match"
            | "group_empty" | "group_vacant" | "bucket_entry" | "install" | "erase" => {
                Some(self.map_index_value(name, types, vals, n))
            }
            "regrow" => Some(self.map_regrow(types, vals[0]?, vals[1]?, None, n)),
            "try_regrow" => Some(self.map_regrow(types, vals[0]?, vals[1]?, Some(result), n)),
            "with_value" => {
                let collection = self.map_type(types)?;
                let entry = self.map_entry_at(vals[0]?, collection, vals[1]?, n)?;
                let place = self.place_ty(types[1], n)?;
                let at = self.address(AddressBase::Address(entry), &[Step::Field(2)], place, n);
                let pointer = self.pointer(at, n);
                let callable = self.t.mk(TyKind::Fn(
                    vec![(Mode::Mut, types[1])],
                    result,
                    false,
                    Callable::Scoped,
                ));
                Some(self.call_lowered(callable, vals[2]?, &[Some(pointer)], n))
            }
            "take_entry" => {
                let collection = self.map_type(types)?;
                let value = self.map_take_entry_values(
                    vals[0]?, collection, vals[1]?, result, types[0], types[1], n,
                )?;
                self.temp(result, Some(value));
                Some(Some(value))
            }
            "swap_value" | "kill" => {
                let collection = self.map_type(types)?;
                let entry = self.map_entry_at(vals[0]?, collection, vals[1]?, n)?;
                let value_ty = self.place_ty(types[1], n)?;
                let value_at =
                    self.address(AddressBase::Address(entry), &[Step::Field(2)], value_ty, n);
                if name == "swap_value" {
                    let pointer = self.pointer(value_at, n);
                    Some(self.replace_value(types[1], Some(pointer), vals[2], n))
                } else {
                    Some(self.map_kill(vals[0]?, entry, value_at, types, n))
                }
            }
            "hash" | "place" | "strong" | "fast_class" => {
                use key_glue::KeyOp;
                let (op, args) = match name {
                    "place" => (KeyOp::Place, vals),
                    "strong" => (KeyOp::Strong, vals),
                    "fast_class" => (KeyOp::Class, &[][..]),
                    _ => (KeyOp::Hash, vals),
                };
                Some(self.key_operation(op, types[0], args, n))
            }
            "slots" | "capacity" => {
                let word = if name == "slots" {
                    growth::FillWord::Used
                } else {
                    growth::FillWord::Capacity
                };
                Some(Some(self.fill_word(vals[0]?, None, word, n)))
            }
            "slot_meta" | "slot_key" | "slot_value" => {
                let collection = self.map_type(types)?;
                let entry = self.map_entry_at(vals[0]?, collection, vals[1]?, n)?;
                let (field, ty) = match name {
                    "slot_meta" => (0, PlaceTy::Value(Ty::Scalar(Scalar::U64))),
                    "slot_key" => (1, self.place_ty(types[0], n)?),
                    _ => (2, self.place_ty(types[1], n)?),
                };
                let at = self.address(AddressBase::Address(entry), &[Step::Field(field)], ty, n);
                Some(match ty {
                    PlaceTy::Value(scalar) => Some(self.read_bits(at, scalar, n)),
                    PlaceTy::Declared(_) => {
                        self.read_carried(at, types[usize::from(name == "slot_value")], n)
                    }
                })
            }
            _ => None,
        }
    }

    /// Move one live entry out of its insertion-order slot. The caller has
    /// already checked the state word. Marking the slot dead before publishing
    /// the tuple makes Map cleanup skip the moved key and value on every exit.
    pub(super) fn map_take_entry_values(
        &mut self,
        header: ValueId,
        collection: TyId,
        index: ValueId,
        result: TyId,
        key_ty: TyId,
        value_ty: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let entry = self.map_entry_at(header, collection, index, n)?;
        let to = self.slot(result, None, n)?;
        for (field, ty) in [(0, key_ty), (1, value_ty)] {
            let rep = match self.rep(ty) {
                Ok(rep) => rep,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let Some(place) = rep.stored() else {
                continue;
            };
            let from = self.address(
                AddressBase::Address(entry),
                &[Step::Field(field as u32 + 1)],
                place,
                n,
            );
            let value = self.read_carried_rep(from, rep, n);
            let dest = self.address(
                AddressBase::Address(to),
                &[Step::Field(field as u32)],
                place,
                n,
            );
            self.install(dest, rep, value, n);
        }
        let u = Ty::Scalar(Scalar::U64);
        let zero = self.konst(u, 0, n)?;
        let state = self.address(
            AddressBase::Address(entry),
            &[Step::Field(0)],
            PlaceTy::Value(u),
            n,
        );
        self.inst(
            Inst::Store {
                address: state,
                value: zero,
            },
            n,
        );
        let layout = self.map_entry(key_ty, value_ty, n)?;
        let live_at = self.map_live_at(header, layout, n)?;
        let live = self.read_bits(live_at, u, n);
        let one = self.konst(u, 1, n)?;
        let remaining = self.map_binary(BinaryOp::Sub, live, one, n);
        self.inst(
            Inst::Store {
                address: live_at,
                value: remaining,
            },
            n,
        );
        Some(self.pointer(to, n))
    }

    fn map_push(&mut self, types: &[TyId], vals: &[Option<ValueId>], n: NodeId) -> Option<ValueId> {
        let header = vals[0]?;
        let u = Ty::Scalar(Scalar::U64);
        let count = self.read_sequence_word(header, 8, u, n);
        self.map_append(types, header, count, [vals[1], vals[2]], [false; 2], n)?;
        None
    }

    fn map_append(
        &mut self,
        types: &[TyId],
        header: ValueId,
        count: ValueId,
        values: [Option<ValueId>; 2],
        placed: [bool; 2],
        n: NodeId,
    ) -> Option<()> {
        let collection = self.map_type(types)?;
        let entry = self.map_entry_at(header, collection, count, n)?;
        for (i, (value, already_placed)) in values.into_iter().zip(placed).enumerate() {
            if already_placed {
                self.consume(value, n);
                continue;
            }
            let field = i as u32 + 1;
            let ty = types[i];
            let rep = match self.rep(ty) {
                Ok(rep) => rep,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            if let Some(place) = rep.stored() {
                let at = self.address(AddressBase::Address(entry), &[Step::Field(field)], place, n);
                self.install(at, rep, value, n);
            }
        }
        Some(())
    }

    fn map_regrow(
        &mut self,
        types: &[TyId],
        header: ValueId,
        cap: ValueId,
        result: Option<TyId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let entry = self.map_entry(types[0], types[1], n)?;
        let u = Ty::Scalar(Scalar::U64);
        let old_data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let old_cap = self.read_sequence_word(header, 16, u, n);

        let key = self.layout_size(types[0], true, n)?;
        let value = self.layout_size(types[1], true, n)?;
        let extra = self.map_u64(hirsel_core_layout::charge::MAP_ENTRY_EXTRA, n)?;
        let (payload, _) = self.list_arith(ArithOp::Add, ArithMode::Saturating, key, value, n);
        let (logical, _) = self.list_arith(ArithOp::Add, ArithMode::Saturating, payload, extra, n);
        let mut plan = self.list_plan_with_logical(entry.ty, logical, cap, None, n)?;
        plan.edge = FieldTy::Map(
            Box::new(self.edge_field(types[0], n)?),
            Box::new(self.edge_field(types[1], n)?),
        );
        let bound = self.map_u64(idx::MAX_CAPACITY, n)?;
        let over_bound = self.map_compare(CompareOp::Gt, cap, bound, n);
        plan.overflow = self.list_bool(BinaryOp::BitOr, plan.overflow, over_bound, n);
        let bounded = self.map_binary(BinaryOp::Min, cap, bound, n);
        let groups = self.map_group_count(bounded, n)?;
        let wide = self.map_index_width(cap, n)?;
        let stride = self.map_group_stride(wide, n)?;
        let (group_bytes, _) =
            self.list_arith(ArithOp::Mul, ArithMode::Saturating, groups, stride, n);
        let header_bytes = self.map_u64(idx::HEADER_BYTES, n)?;
        let (overhead, _) = self.list_arith(
            ArithOp::Add,
            ArithMode::Saturating,
            group_bytes,
            header_bytes,
            n,
        );
        let (physical, _) =
            self.list_arith(ArithOp::Add, ArithMode::Saturating, plan.size, overhead, n);
        plan.size = physical;

        let zero = self.map_u64(0, n)?;
        let empty = self.map_compare(CompareOp::Eq, old_cap, zero, n);
        let first = self.new_block(&[]);
        let growth = self.new_block(&[]);
        let allocated = self.new_block(&[Ty::Ptr, u, Ty::Scalar(Scalar::Bool)]);
        self.map_branch(empty, first, growth, n);
        self.start(first);
        let strong_empty = self.konst(Ty::Ptr, idx::STRONG_EMPTY, n)?;
        let was_strong = self.map_compare(CompareOp::Eq, old_data, strong_empty, n);
        let class = self.key_operation(key_glue::KeyOp::Class, types[0], &[], n)?;
        let zero_class = self.konst(Ty::Scalar(Scalar::U8), 0, n)?;
        let composite = self.map_compare(CompareOp::Eq, class, zero_class, n);
        let strong = self.list_bool(BinaryOp::BitOr, was_strong, composite, n);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        self.jump(allocated, &[block, available, strong], n);
        self.start(growth);
        let old_header = self.map_header(header, entry, n)?;
        let strong = self.map_mode_bit(old_header, idx::STRONG, n)?;
        let (block, available) = self.list_allocate(&plan, AllocPlace::Like(old_data), n);
        self.jump(allocated, &[block, available, strong], n);
        self.start(allocated);
        let incoming = self.block_params(allocated);
        let (block, available, strong) = (incoming[0], incoming[1], incoming[2]);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let ok = self.map_compare(CompareOp::Ne, block, null, n);
        let charge = plan.charge;
        let strong_bit = self.map_u64(idx::STRONG, n)?;
        let wide_bit = self.map_u64(idx::WIDE, n)?;
        let strong_word = self.map_pick_u64(strong, strong_bit, zero, n);
        let wide_word = self.map_pick_u64(wide, wide_bit, zero, n);
        let mode = self.map_binary(BinaryOp::BitOr, strong_word, wide_word, n);
        let allocate = |cx: &mut Self| {
            cx.map_relocate(
                header,
                entry,
                old_data,
                old_cap,
                block,
                cap,
                plan.stride,
                plan.logical,
                Some([groups, stride, mode]),
                n,
            )
        };
        let Some(result) = result else {
            self.raise_unless(ok, charge, available, n);
            allocate(self)?;
            return None;
        };
        self.runtime_result(
            result,
            ok,
            n,
            |cx, _, _| {
                let _ = allocate(cx);
            },
            |cx, to, capacity| {
                if let Some(place) = cx.place_ty(capacity, n) {
                    let at = cx.address(
                        AddressBase::Address(to),
                        &[Step::CaseField { case: 1, field: 0 }],
                        place,
                        n,
                    );
                    cx.runtime_capacity(at, charge, available, n);
                }
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn map_relocate(
        &mut self,
        header: ValueId,
        entry: MapEntry,
        old_data: ValueId,
        old_cap: ValueId,
        block: ValueId,
        cap: ValueId,
        stride: ValueId,
        logical: ValueId,
        index: Option<[ValueId; 3]>,
        n: NodeId,
    ) -> Option<()> {
        let in_place = index.is_none();
        let u = Ty::Scalar(Scalar::U64);
        let slots = self.read_sequence_word(header, 8, u, n);
        let zero = self.konst(u, 0, n)?;
        let one = self.konst(u, 1, n)?;
        let head = self.new_block(&[u, u]);
        let body = self.new_block(&[]);
        let copied = self.new_block(&[]);
        self.jump(head, &[zero, zero], n);
        self.start(head);
        let ps = self.block_params(head);
        let (i, live) = (ps[0], ps[1]);
        let more = self.map_compare(CompareOp::Lt, i, slots, n);
        self.map_branch(more, body, copied, n);
        self.start(body);
        let source = self.map_entry_at(header, entry.collection, i, n)?;
        let at = self.address(
            AddressBase::Address(source),
            &[Step::Field(0)],
            PlaceTy::Value(u),
            n,
        );
        let state = self.read_bits(at, u, n);
        let active = self.map_compare(CompareOp::Ne, state, zero, n);
        let keep = self.new_block(&[]);
        let skip = self.new_block(&[]);
        self.map_branch(active, keep, skip, n);
        self.start(keep);
        let to = self.address(
            AddressBase::Value(block),
            &[Step::Index(live)],
            PlaceTy::Declared(entry.place),
            n,
        );
        // Distinct entry indices never overlap. Skip an unchanged prefix rather
        // than pass equal pointers to memcpy; later live entries move only left.
        let after_copy = in_place.then(|| self.new_block(&[]));
        if let Some(done) = after_copy {
            let moved = self.new_block(&[]);
            let unchanged = self.map_compare(CompareOp::Eq, i, live, n);
            self.map_branch(unchanged, done, moved, n);
            self.start(moved);
        }
        self.inst(
            Inst::Copy {
                to,
                from: source,
                bytes: stride,
            },
            n,
        );
        if let Some(done) = after_copy {
            self.jump(done, &[], n);
            self.start(done);
        }
        let next_i = self.map_binary(BinaryOp::Add, i, one, n);
        let next_live = self.map_binary(BinaryOp::Add, live, one, n);
        self.jump(head, &[next_i, next_live], n);
        self.start(skip);
        let next_i = self.map_binary(BinaryOp::Add, i, one, n);
        self.jump(head, &[next_i, live], n);
        self.start(copied);
        self.sequence_word(header, 0, Ty::Ptr, block, n);
        self.sequence_word(header, 8, u, live, n);
        self.sequence_word(header, 16, u, cap, n);
        let live_at = self.map_live_at(header, entry, n)?;
        self.inst(
            Inst::Store {
                address: live_at,
                value: live,
            },
            n,
        );
        let Some([groups, group_stride, mode]) = index else {
            return Some(());
        };
        let index_header = self.map_header(header, entry, n)?;
        let group_mask = self.map_binary(BinaryOp::Sub, groups, one, n);
        let at = self.map_header_at(index_header, idx::GROUP_MASK, n);
        self.map_store(at, group_mask, n);
        let at = self.map_header_at(index_header, idx::MODE, n);
        self.map_store(at, mode, n);
        let at = self.map_header_at(index_header, 24, n);
        self.map_store(at, zero, n);
        self.map_clear(index_header, groups, group_stride, n)?;
        let has_old = self.map_compare(CompareOp::Ne, old_cap, zero, n);
        let free = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.map_branch(has_old, free, done, n);
        self.start(free);
        let (raw, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_cap, logical, n);
        let seven = self.konst(u, 7, n)?;
        let rounded = self.map_binary(BinaryOp::Add, raw, seven, n);
        let mask = self.konst(u, !7, n)?;
        let charge = self.map_binary(BinaryOp::BitAnd, rounded, mask, n);
        self.inst(
            Inst::Free {
                block: old_data,
                charge,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }

    fn map_branch(&mut self, condition: ValueId, yes: BlockId, no: BlockId, n: NodeId) {
        let yes = self.edge(yes, &[]);
        let no = self.edge(no, &[]);
        self.end(Terminator::Branch { condition, yes, no }, Some(n));
    }

    fn map_group_count(&mut self, cap: ValueId, n: NodeId) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let min_cap = self.map_u64(idx::GROUP_WIDTH as u64 / 2, n)?;
        let cap = self.map_binary(BinaryOp::Max, cap, min_cap, n);
        let twice = self.map_binary(BinaryOp::Add, cap, cap, n);
        let one = self.map_u64(1, n)?;
        let last = self.map_binary(BinaryOp::Sub, twice, one, n);
        let leading = self.value(u);
        self.inst(
            Inst::ScalarUnary {
                result: leading,
                op: hirsel_core_ir::ScalarUnaryOp::LeadingZeros,
                operand: last,
            },
            n,
        );
        let width = self.map_u64(60, n)?;
        let bits = self.map_binary(BinaryOp::Sub, width, leading, n);
        Some(self.map_binary(BinaryOp::Shl, one, bits, n))
    }

    fn map_kill(
        &mut self,
        header: ValueId,
        entry: AddressId,
        value_at: AddressId,
        types: &[TyId],
        n: NodeId,
    ) -> Option<ValueId> {
        let rep = match self.rep(types[1]) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let value = self.read_carried_rep(value_at, rep, n);
        let saved = self.slot(types[1], value, n)?;
        let value = self.read_carried_rep(saved, rep, n);
        self.temp(types[1], value);
        let u = Ty::Scalar(Scalar::U64);
        let zero = self.konst(u, 0, n)?;
        let state = self.address(
            AddressBase::Address(entry),
            &[Step::Field(0)],
            PlaceTy::Value(u),
            n,
        );
        self.inst(
            Inst::Store {
                address: state,
                value: zero,
            },
            n,
        );
        let layout = self.map_entry(types[0], types[1], n)?;
        let live_at = self.map_live_at(header, layout, n)?;
        let live = self.read_bits(live_at, u, n);
        let one = self.konst(u, 1, n)?;
        let remaining = self.map_binary(BinaryOp::Sub, live, one, n);
        self.inst(
            Inst::Store {
                address: live_at,
                value: remaining,
            },
            n,
        );
        let key_ty = self.place_ty(types[0], n)?;
        let key = self.address(AddressBase::Address(entry), &[Step::Field(1)], key_ty, n);
        self.drop_at(key, types[0], n);
        value
    }
    fn map_live_at(&mut self, header: ValueId, entry: MapEntry, n: NodeId) -> Option<AddressId> {
        self.map_metadata_at(header, entry, 0, n)
    }
    fn map_metadata_at(
        &mut self,
        header: ValueId,
        entry: MapEntry,
        word: u64,
        n: NodeId,
    ) -> Option<AddressId> {
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let cap = self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n);
        let stride = self.layout_size(entry.ty, false, n)?;
        let (mut offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, cap, stride, n);
        if word != 0 {
            let extra = self.konst(Ty::Scalar(Scalar::U64), word * 8, n)?;
            offset = self.map_binary(BinaryOp::Add, offset, extra, n);
        }
        Some(self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            n,
        ))
    }
    fn map_type(&mut self, types: &[TyId]) -> Option<TyId> {
        Some(self.map_entry(types[0], types[1], self.header)?.collection)
    }
    fn map_compare(&mut self, op: CompareOp, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
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
    fn map_binary(&mut self, op: BinaryOp, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        // Private entry/index offsets fit the admitted block; counts stay within
        // its capacity. These are recipe arithmetic, not source integer ops.
        let arithmetic = match op {
            BinaryOp::Add => Some(ArithOp::Add),
            BinaryOp::Sub => Some(ArithOp::Sub),
            _ => None,
        };
        if let Some(op) = arithmetic {
            return self.list_arith(op, ArithMode::Wrapping, left, right, n).0;
        }
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
    fn map_cast(&mut self, to: Ty, operand: ValueId, mode: ConvertMode, n: NodeId) -> ValueId {
        let result = self.value(to);
        self.inst(
            Inst::Convert {
                result,
                flag: None,
                operand,
                mode,
            },
            n,
        );
        result
    }

    fn map_u64(&mut self, bits: u64, n: NodeId) -> Option<ValueId> {
        self.konst(Ty::Scalar(Scalar::U64), bits, n)
    }

    fn map_store(&mut self, address: AddressId, value: ValueId, n: NodeId) {
        self.inst(Inst::Store { address, value }, n);
    }

    fn map_header(&mut self, carrier: ValueId, entry: MapEntry, n: NodeId) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let data = self.read_sequence_word(carrier, 0, Ty::Ptr, n);
        let cap = self.read_sequence_word(carrier, 16, u, n);
        let stride = self.layout_size(entry.ty, false, n)?;
        let (entries, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, cap, stride, n);
        let address = self.address(
            AddressBase::Value(data),
            &[Step::Byte(entries)],
            PlaceTy::Value(u),
            n,
        );
        Some(self.pointer(address, n))
    }

    fn map_header_at(&mut self, header: ValueId, bytes: u64, n: NodeId) -> AddressId {
        let offset = self
            .map_u64(bytes, n)
            .expect("Map header offset in a live block");
        self.address(
            AddressBase::Value(header),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            n,
        )
    }

    fn map_header_word(&mut self, header: ValueId, bytes: u64, n: NodeId) -> ValueId {
        let address = self.map_header_at(header, bytes, n);
        self.read_bits(address, Ty::Scalar(Scalar::U64), n)
    }

    fn map_mode_bit(&mut self, header: ValueId, bit: u64, n: NodeId) -> Option<ValueId> {
        let mode = self.map_header_word(header, idx::MODE, n);
        let bit = self.map_u64(bit, n)?;
        let mode = self.map_binary(BinaryOp::BitAnd, mode, bit, n);
        let zero = self.map_u64(0, n)?;
        Some(self.map_compare(CompareOp::Ne, mode, zero, n))
    }

    fn map_index_width(&mut self, cap: ValueId, n: NodeId) -> Option<ValueId> {
        let narrow_max = self.map_u64(idx::WIDE_CAPACITY, n)?;
        Some(self.map_compare(CompareOp::Gt, cap, narrow_max, n))
    }

    fn map_pick_u64(&mut self, flag: ValueId, yes: ValueId, no: ValueId, n: NodeId) -> ValueId {
        let yes_edge = self.new_block(&[]);
        let no_edge = self.new_block(&[]);
        let join = self.new_block(&[Ty::Scalar(Scalar::U64)]);
        self.map_branch(flag, yes_edge, no_edge, n);
        self.start(yes_edge);
        self.jump(join, &[yes], n);
        self.start(no_edge);
        self.jump(join, &[no], n);
        self.start(join);
        self.block_params(join)[0]
    }

    fn map_group_stride(&mut self, wide: ValueId, n: NodeId) -> Option<ValueId> {
        let wide_bytes = self.map_u64(idx::WIDE_GROUP_BYTES, n)?;
        let narrow_bytes = self.map_u64(idx::NARROW_GROUP_BYTES, n)?;
        Some(self.map_pick_u64(wide, wide_bytes, narrow_bytes, n))
    }

    fn map_group(
        &mut self,
        header: ValueId,
        group: ValueId,
        stride: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        let base = self.map_u64(idx::HEADER_BYTES, n)?;
        let (bytes, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, group, stride, n);
        let offset = self.map_binary(BinaryOp::Add, base, bytes, n);
        let address = self.address(
            AddressBase::Value(header),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        Some(self.pointer(address, n))
    }

    fn map_control_mask(&mut self, group: ValueId, tag: Option<ValueId>, n: NodeId) -> ValueId {
        use hirsel_core_ir::{SimdOp, Vector};

        let shape = Ty::Vector(Vector::U8x16);
        let control = self.value(shape);
        self.inst(
            Inst::Simd {
                result: control,
                op: SimdOp::Lanes,
                first: group,
                second: None,
                fail: None,
            },
            n,
        );
        let first = if let Some(tag) = tag {
            let expected = self.value(shape);
            self.inst(
                Inst::Simd {
                    result: expected,
                    op: SimdOp::Splat,
                    first: tag,
                    second: None,
                    fail: None,
                },
                n,
            );
            let matched = self.value(shape);
            self.inst(
                Inst::Simd {
                    result: matched,
                    op: SimdOp::Eq,
                    first: control,
                    second: Some(expected),
                    fail: None,
                },
                n,
            );
            matched
        } else {
            control
        };
        let result = self.value(Ty::Scalar(Scalar::U64));
        self.inst(
            Inst::Simd {
                result,
                op: SimdOp::Bitmask,
                first,
                second: None,
                fail: None,
            },
            n,
        );
        result
    }

    fn map_bucket(
        &mut self,
        header: ValueId,
        bucket: ValueId,
        n: NodeId,
    ) -> Option<(ValueId, ValueId, ValueId)> {
        let wide = self.map_mode_bit(header, idx::WIDE, n)?;
        let stride = self.map_group_stride(wide, n)?;
        let four = self.map_u64(4, n)?;
        let low = self.map_u64(idx::GROUP_WIDTH as u64 - 1, n)?;
        let group = self.map_binary(BinaryOp::Shr, bucket, four, n);
        let lane = self.map_binary(BinaryOp::BitAnd, bucket, low, n);
        let group = self.map_group(header, group, stride, n)?;
        Some((group, lane, wide))
    }

    fn map_bucket_entry(
        &mut self,
        group: ValueId,
        lane: ValueId,
        wide: ValueId,
        store: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let word = Ty::Scalar(Scalar::U64);
        let large = self.new_block(&[]);
        let small = self.new_block(&[]);
        let types = [word];
        let join = self.new_block(if store.is_some() { &[] } else { &types });
        self.map_branch(wide, large, small, n);
        for (block, scalar) in [(large, Scalar::U64), (small, Scalar::U32)] {
            self.start(block);
            let ty = Ty::Scalar(scalar);
            let bytes = self.map_u64(scalar.size(), n)?;
            let width = self.map_u64(idx::GROUP_WIDTH as u64, n)?;
            let (index, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, lane, bytes, n);
            let offset = self.map_binary(BinaryOp::Add, width, index, n);
            let at = self.address(
                AddressBase::Value(group),
                &[Step::Byte(offset)],
                PlaceTy::Value(ty),
                n,
            );
            if let Some(value) = store {
                let value = if ty == word {
                    value
                } else {
                    self.map_cast(ty, value, ConvertMode::Truncate, n)
                };
                self.map_store(at, value, n);
                self.jump(join, &[], n);
            } else {
                let value = self.read_bits(at, ty, n);
                let value = if ty == word {
                    value
                } else {
                    self.map_cast(word, value, ConvertMode::Widen, n)
                };
                self.jump(join, &[value], n);
            }
        }
        self.start(join);
        self.block_params(join).first().copied()
    }

    fn map_advance(&mut self, carrier: ValueId, entry: MapEntry, n: NodeId) -> Option<()> {
        let u = Ty::Scalar(Scalar::U64);
        let count = self.read_sequence_word(carrier, 8, u, n);
        let one = self.map_u64(1, n)?;
        let next = self.map_binary(BinaryOp::Add, count, one, n);
        let at = self.map_live_at(carrier, entry, n)?;
        let live = self.read_bits(at, u, n);
        let live = self.map_binary(BinaryOp::Add, live, one, n);
        self.map_store(at, live, n);
        self.sequence_word(carrier, 8, u, next, n);
        Some(())
    }

    fn map_entry_meta(
        &mut self,
        carrier: ValueId,
        entry: MapEntry,
        index: ValueId,
        n: NodeId,
    ) -> Option<AddressId> {
        let entry = self.map_entry_at(carrier, entry.collection, index, n)?;
        Some(self.address(
            AddressBase::Address(entry),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            n,
        ))
    }

    fn map_clear(
        &mut self,
        header: ValueId,
        groups: ValueId,
        stride: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let u = Ty::Scalar(Scalar::U64);
        let zero = self.map_u64(0, n)?;
        let one = self.map_u64(1, n)?;
        let empty = self.map_u64(u64::MAX, n)?;
        let head = self.new_block(&[u]);
        let body = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.jump(head, &[zero], n);
        self.start(head);
        let index = self.block_params(head)[0];
        let more = self.map_compare(CompareOp::Lt, index, groups, n);
        self.map_branch(more, body, done, n);
        self.start(body);
        let group = self.map_group(header, index, stride, n)?;
        let first = self.map_header_at(group, 0, n);
        let second = self.map_header_at(group, 8, n);
        self.map_store(first, empty, n);
        self.map_store(second, empty, n);
        let next = self.map_binary(BinaryOp::Add, index, one, n);
        self.jump(head, &[next], n);
        self.start(done);
        Some(())
    }

    fn map_index_value(
        &mut self,
        name: &str,
        types: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let carrier = vals[0]?;
        let entry = self.map_entry(types[0], types[1], n)?;
        let header = self.map_header(carrier, entry, n)?;
        match name {
            "mode" => self.map_mode_bit(header, idx::STRONG, n),
            "group_mask" => Some(self.map_header_word(header, idx::GROUP_MASK, n)),
            "set_strong" => {
                let mode_at = self.map_header_at(header, idx::MODE, n);
                let mode = self.read_bits(mode_at, Ty::Scalar(Scalar::U64), n);
                let bit = self.map_u64(idx::STRONG, n)?;
                let mode = self.map_binary(BinaryOp::BitOr, mode, bit, n);
                self.map_store(mode_at, mode, n);
                None
            }
            "group_match" | "group_empty" | "group_vacant" => {
                let wide = self.map_mode_bit(header, idx::WIDE, n)?;
                let stride = self.map_group_stride(wide, n)?;
                let group = self.map_group(header, vals[1]?, stride, n)?;
                let byte = Ty::Scalar(Scalar::U8);
                let tag = match name {
                    "group_match" => Some(self.map_cast(byte, vals[2]?, ConvertMode::Truncate, n)),
                    "group_empty" => Some(self.konst(byte, u64::from(idx::EMPTY), n)?),
                    _ => None,
                };
                Some(self.map_control_mask(group, tag, n))
            }
            "clear_index" => {
                let mask = self.map_header_word(header, idx::GROUP_MASK, n);
                let one = self.map_u64(1, n)?;
                let groups = self.map_binary(BinaryOp::Add, mask, one, n);
                let wide = self.map_mode_bit(header, idx::WIDE, n)?;
                let stride = self.map_group_stride(wide, n)?;
                self.map_clear(header, groups, stride, n)?;
                None
            }
            "bucket_entry" | "install" | "erase" => {
                let (group, lane, wide) = self.map_bucket(header, vals[1]?, n)?;
                if name == "bucket_entry" {
                    return self.map_bucket_entry(group, lane, wide, None, n);
                }
                let byte = Ty::Scalar(Scalar::U8);
                let control = self.address(
                    AddressBase::Value(group),
                    &[Step::Byte(lane)],
                    PlaceTy::Value(byte),
                    n,
                );
                if name == "install" {
                    let p = vals[2]?;
                    let i = vals[3]?;
                    let shift = self.map_u64(u64::from(idx::TAG_SHIFT), n)?;
                    let tag = self.map_binary(BinaryOp::Shr, p, shift, n);
                    let tag = self.map_cast(byte, tag, ConvertMode::Truncate, n);
                    let _ = self.map_bucket_entry(group, lane, wide, Some(i), n);
                    let at = self.map_entry_meta(carrier, entry, i, n)?;
                    let bit = self.map_u64(idx::LIVE_BIT, n)?;
                    let meta = self.map_binary(BinaryOp::BitOr, p, bit, n);
                    self.map_store(at, meta, n);
                    self.map_store(control, tag, n);
                } else {
                    let empty = self.map_u64(u64::from(idx::EMPTY), n)?;
                    let deleted = self.map_u64(u64::from(idx::DELETED), n)?;
                    let zero = self.map_u64(0, n)?;
                    let tag = self.map_cast(byte, empty, ConvertMode::Truncate, n);
                    let mask = self.map_control_mask(group, Some(tag), n);
                    let has_empty = self.map_compare(CompareOp::Ne, mask, zero, n);
                    let tag = self.map_pick_u64(has_empty, empty, deleted, n);
                    let tag = self.map_cast(byte, tag, ConvertMode::Truncate, n);
                    self.map_store(control, tag, n);
                }
                None
            }
            _ => unreachable!("Map index operation"),
        }
    }

    fn map_entry(&mut self, key: TyId, value: TyId, n: NodeId) -> Option<MapEntry> {
        if let Some(entry) = self.map_entries.get(&(key, value)) {
            return Some(*entry);
        }
        let ty = self
            .t
            .mk(TyKind::Tuple(vec![scalar(Scalar::U64), key, value]));
        let place = self.place_row(ty, "a map entry without a place is not built", n)?;
        let collection = self.prelude("Map", n)?;
        let collection = self.t.mk(TyKind::Adt(collection, vec![key, value]));
        let entry = MapEntry {
            ty,
            place,
            collection,
        };
        self.map_entries.insert((key, value), entry);
        Some(entry)
    }

    pub(super) fn map_entry_at(
        &mut self,
        header: ValueId,
        collection: TyId,
        index: ValueId,
        n: NodeId,
    ) -> Option<AddressId> {
        let TyKind::Adt(_, args) = self.t.kind(collection) else {
            self.gap(n, "a map that is not a declared type is not built");
            return None;
        };
        let entry = self.map_entry(args[0], args[1], n)?;
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        Some(self.address(
            AddressBase::Value(data),
            &[Step::Index(index)],
            PlaceTy::Declared(entry.place),
            n,
        ))
    }

    fn map_len(&mut self, header: ValueId, entry: MapEntry, n: NodeId) -> Option<ValueId> {
        let u64t = Ty::Scalar(Scalar::U64);
        let capacity = self.read_sequence_word(header, 16, u64t, n);
        let zero = self.konst(u64t, 0, n)?;
        let empty = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: empty,
                op: CompareOp::Eq,
                left: capacity,
                right: zero,
            },
            n,
        );
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let done = self.new_block(&[u64t]);
        let ye = self.edge(yes, &[]);
        let ne = self.edge(no, &[]);
        self.end(
            Terminator::Branch {
                condition: empty,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(yes);
        self.jump(done, &[zero], n);
        self.start(no);
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let stride = self.layout_size(entry.ty, false, n)?;
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, capacity, stride, n);
        let at = self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(u64t),
            n,
        );
        let live = self.read_bits(at, u64t, n);
        self.jump(done, &[live], n);
        self.start(done);
        Some(self.block_params(done)[0])
    }
}
