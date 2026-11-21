//! Map carrier/kernel sequences. Probing belongs to core/map.hirsel.
use super::*;

/// The entry tuple uses exactly map_entry's hash/state/key/value layout. Keeping
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
            cx.map_field(types, header, count, 2, PlaceTy::Declared(ty), n)
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
        let probe = self.p.core_fn("map", "probe_insert")?;
        let probe_ty = self.t.mk(TyKind::Tuple(vec![
            scalar(Scalar::U64),
            scalar(Scalar::U64),
            BOOL,
        ]));
        let value = self.emit_call(probe, types, &[Some(header), key], probe_ty, n)?;
        let id = self.place_row(probe_ty, "a map probe requires a tuple", n)?;
        let at = self.address(AddressBase::Value(value), &[], PlaceTy::Declared(id), n);
        let (u, boolean) = (Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::Bool));
        let [hash, index, existing] = [(0, u), (1, u), (2, boolean)].map(|(i, t)| {
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
        let address = self.map_field(types, header, index, 3, place, n)?;
        let pointer = self.pointer(address, n);
        self.copy_to(saved, pointer, ty, n);
        self.jump(chosen, &[pointer], n);
        self.start(absent);
        let address = self.choose_spare_slot(ty, full, n, |cx| {
            cx.map_field(types, header, count, 3, place, n)
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
        let hash = insertion.hash;
        self.map_append(
            types,
            header,
            count,
            hash,
            [key, value],
            [direct_key, direct_value],
            n,
        )?;
        let u = Ty::Scalar(Scalar::U64);
        let one = self.konst(u, 1, n)?;
        let next = self.map_binary(BinaryOp::Add, count, one, n);
        let recipe = self.map_entry(types[0], types[1], n)?;
        // core.map's index word: the hash's bits above the index mask over the
        // one-based slot. A spare slot means a nonempty block, so the index
        // length is stored.
        let len_at = self.map_metadata_at(header, recipe, 1, n)?;
        let len = self.read_bits(len_at, u, n);
        let mask = self.map_binary(BinaryOp::Sub, len, one, n);
        let high = self.konst(u, u64::MAX, n)?;
        let high = self.map_binary(BinaryOp::BitXor, mask, high, n);
        let tag = self.map_binary(BinaryOp::BitAnd, hash, high, n);
        let word = self.map_binary(BinaryOp::BitOr, tag, next, n);
        let index = self.map_index_at(header, recipe, insertion.index, n)?;
        self.inst(
            Inst::Store {
                address: index,
                value: word,
            },
            n,
        );
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
                let plan = self.list_plan(entry.ty, cap, None, n)?;
                let index_len = self.map_index_len(header, entry, n)?;
                self.map_relocate(
                    header, entry, data, cap, data, cap, index_len, plan, true, n,
                )?;
                Some(None)
            }
            "regrow" => Some(self.map_regrow(types, vals[0]?, vals[1]?, None, n)),
            "try_regrow" => Some(self.map_regrow(types, vals[0]?, vals[1]?, Some(result), n)),
            "with_value" => {
                let collection = self.map_type(types)?;
                let entry = self.map_entry_at(vals[0]?, collection, vals[1]?, n)?;
                let place = self.place_ty(types[1], n)?;
                let at = self.address(AddressBase::Address(entry), &[Step::Field(3)], place, n);
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
                    self.address(AddressBase::Address(entry), &[Step::Field(3)], value_ty, n);
                if name == "swap_value" {
                    let pointer = self.pointer(value_at, n);
                    Some(self.replace_value(types[1], Some(pointer), vals[2], n))
                } else {
                    Some(self.map_kill(vals[0]?, entry, value_at, types, n))
                }
            }
            "hash" => Some(self.key_operation(key_glue::KeyOp::Hash, types[0], &[vals[0]], n)),
            "slots" | "capacity" => {
                let word = if name == "slots" {
                    growth::FillWord::Used
                } else {
                    growth::FillWord::Capacity
                };
                Some(Some(self.fill_word(vals[0]?, None, word, n)))
            }
            "slot_hash" | "slot_live" | "slot_key" | "slot_value" => {
                let collection = self.map_type(types)?;
                let entry = self.map_entry_at(vals[0]?, collection, vals[1]?, n)?;
                let (field, ty) = match name {
                    "slot_hash" => (0, PlaceTy::Value(Ty::Scalar(Scalar::U64))),
                    "slot_live" => (1, PlaceTy::Value(Ty::Scalar(Scalar::U64))),
                    "slot_key" => (2, self.place_ty(types[0], n)?),
                    _ => (3, self.place_ty(types[1], n)?),
                };
                let at = self.address(AddressBase::Address(entry), &[Step::Field(field)], ty, n);
                Some(if name == "slot_key" || name == "slot_value" {
                    self.read_carried(at, types[usize::from(name == "slot_value")], n)
                } else {
                    let value = self.read_bits(at, Ty::Scalar(Scalar::U64), n);
                    if name == "slot_live" {
                        let one = self.konst(Ty::Scalar(Scalar::U64), 1, n)?;
                        Some(self.map_compare(CompareOp::Eq, value, one, n))
                    } else {
                        Some(value)
                    }
                })
            }
            "index_len" => {
                let entry = self.map_entry(types[0], types[1], n)?;
                Some(self.map_index_len(vals[0]?, entry, n))
            }
            "index_get" | "index_set" => {
                let entry = self.map_entry(types[0], types[1], n)?;
                let at = self.map_index_at(vals[0]?, entry, vals[1]?, n)?;
                if name == "index_get" {
                    Some(Some(self.read_bits(at, Ty::Scalar(Scalar::U64), n)))
                } else {
                    self.inst(
                        Inst::Store {
                            address: at,
                            value: vals[2]?,
                        },
                        n,
                    );
                    Some(None)
                }
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
                &[Step::Field(field as u32 + 2)],
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
            &[Step::Field(1)],
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
        self.map_append(
            types,
            header,
            count,
            vals[1]?,
            [vals[2], vals[3]],
            [false; 2],
            n,
        )?;
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn map_append(
        &mut self,
        types: &[TyId],
        header: ValueId,
        count: ValueId,
        hash: ValueId,
        values: [Option<ValueId>; 2],
        placed: [bool; 2],
        n: NodeId,
    ) -> Option<()> {
        let u = Ty::Scalar(Scalar::U64);
        let collection = self.map_type(types)?;
        let entry = self.map_entry_at(header, collection, count, n)?;
        let state = self.konst(u, 1, n)?;
        for (field, value) in [(0, hash), (1, state)] {
            let at = self.address(
                AddressBase::Address(entry),
                &[Step::Field(field)],
                PlaceTy::Value(u),
                n,
            );
            self.inst(Inst::Store { address: at, value }, n);
        }
        for (i, (value, already_placed)) in values.into_iter().zip(placed).enumerate() {
            if already_placed {
                self.consume(value, n);
                continue;
            }
            let field = i as u32 + 2;
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
        let next = self.map_binary(BinaryOp::Add, count, state, n);
        self.sequence_word(header, 8, u, next, n);
        let layout = self.map_entry(types[0], types[1], n)?;
        let at = self.map_live_at(header, layout, n)?;
        let live = self.read_bits(at, u, n);
        let live = self.map_binary(BinaryOp::Add, live, state, n);
        self.inst(
            Inst::Store {
                address: at,
                value: live,
            },
            n,
        );
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
        let mut plan = self.list_plan(entry.ty, cap, None, n)?;
        plan.edge = FieldTy::Map(
            Box::new(self.edge_field(types[0], n)?),
            Box::new(self.edge_field(types[1], n)?),
        );
        let index_len = self.map_index_count(cap, n)?;
        let eight = self.konst(u, 8, n)?;
        let (index_bytes, _) =
            self.list_arith(ArithOp::Mul, ArithMode::Saturating, index_len, eight, n);
        let sixteen = self.konst(u, 16, n)?;
        let (overhead, _) =
            self.list_arith(ArithOp::Add, ArithMode::Saturating, index_bytes, sixteen, n);
        let (physical, _) =
            self.list_arith(ArithOp::Add, ArithMode::Saturating, plan.size, overhead, n);
        plan.size = physical;
        let zero = self.konst(u, 0, n)?;
        let empty = self.map_compare(CompareOp::Eq, old_cap, zero, n);
        let first = self.new_block(&[]);
        let growth = self.new_block(&[]);
        let allocated = self.new_block(&[Ty::Ptr, u]);
        self.map_branch(empty, first, growth, n);
        self.start(first);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        self.jump(allocated, &[block, available], n);
        self.start(growth);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Like(old_data), n);
        self.jump(allocated, &[block, available], n);
        self.start(allocated);
        let incoming = self.block_params(allocated);
        let (block, available) = (incoming[0], incoming[1]);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let ok = self.map_compare(CompareOp::Ne, block, null, n);
        let charge = plan.charge;
        // `regrow` raises a refusal with the map unchanged; `try_regrow` returns it.
        let Some(result) = result else {
            self.raise_unless(ok, charge, available, n);
            self.map_relocate(
                header, entry, old_data, old_cap, block, cap, index_len, plan, false, n,
            );
            return None;
        };
        self.runtime_result(
            result,
            ok,
            n,
            |cx, _, _| {
                cx.map_relocate(
                    header, entry, old_data, old_cap, block, cap, index_len, plan, false, n,
                );
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
        index_len: ValueId,
        plan: lists::ListPlan,
        in_place: bool,
        n: NodeId,
    ) -> Option<()> {
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
            &[Step::Field(1)],
            PlaceTy::Value(u),
            n,
        );
        let state = self.read_bits(at, u, n);
        let active = self.map_compare(CompareOp::Eq, state, one, n);
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
                bytes: plan.stride,
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
        let index_len_at = self.map_metadata_at(header, entry, 1, n)?;
        self.inst(
            Inst::Store {
                address: index_len_at,
                value: index_len,
            },
            n,
        );
        // The new block and its index do not move during initialization. Compute
        // the index base once so each store cannot invalidate a carrier reload.
        let index_base = self.map_index_at(header, entry, zero, n)?;
        let eight = self.konst(u, 8, n)?;
        let zero_head = self.new_block(&[u]);
        let zero_body = self.new_block(&[]);
        let cleared = self.new_block(&[]);
        self.jump(zero_head, &[zero], n);
        self.start(zero_head);
        let j = self.block_params(zero_head)[0];
        let more = self.map_compare(CompareOp::Lt, j, index_len, n);
        self.map_branch(more, zero_body, cleared, n);
        self.start(zero_body);
        let offset = self.map_binary(BinaryOp::Mul, j, eight, n);
        let at = self.address(
            AddressBase::Address(index_base),
            &[Step::Byte(offset)],
            PlaceTy::Value(u),
            n,
        );
        self.inst(
            Inst::Store {
                address: at,
                value: zero,
            },
            n,
        );
        let next = self.map_binary(BinaryOp::Add, j, one, n);
        self.jump(zero_head, &[next], n);
        self.start(cleared);
        if in_place {
            return Some(());
        }
        let has_old = self.map_compare(CompareOp::Ne, old_cap, zero, n);
        let free = self.new_block(&[]);
        let done = self.new_block(&[]);
        self.map_branch(has_old, free, done, n);
        self.start(free);
        let (raw, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_cap, plan.logical, n);
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

    // cap is positive in the only private caller (regrow); an admitted Map has
    // logical stride >=16, so an index's power-of-two word count fits u64.
    fn map_index_count(&mut self, cap: ValueId, n: NodeId) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let two = self.konst(u, 2, n)?;
        let (twice, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, cap, two, n);
        let one = self.konst(u, 1, n)?;
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
        let width = self.konst(u, 64, n)?;
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
            &[Step::Field(1)],
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
        let key = self.address(AddressBase::Address(entry), &[Step::Field(2)], key_ty, n);
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
    fn map_index_len(&mut self, header: ValueId, entry: MapEntry, n: NodeId) -> Option<ValueId> {
        let u = Ty::Scalar(Scalar::U64);
        let cap = self.read_sequence_word(header, 16, u, n);
        let zero = self.konst(u, 0, n)?;
        let empty = self.map_compare(CompareOp::Eq, cap, zero, n);
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let done = self.new_block(&[u]);
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
        let at = self.map_metadata_at(header, entry, 1, n)?;
        let count = self.read_bits(at, u, n);
        self.jump(done, &[count], n);
        self.start(done);
        Some(self.block_params(done)[0])
    }
    fn map_index_at(
        &mut self,
        header: ValueId,
        entry: MapEntry,
        index: ValueId,
        n: NodeId,
    ) -> Option<AddressId> {
        let u = Ty::Scalar(Scalar::U64);
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let cap = self.read_sequence_word(header, 16, u, n);
        let stride = self.layout_size(entry.ty, false, n)?;
        let (entries, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, cap, stride, n);
        let eight = self.konst(u, 8, n)?;
        let (index, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, index, eight, n);
        let sixteen = self.konst(u, 16, n)?;
        let offset = self.map_binary(BinaryOp::Add, entries, sixteen, n);
        let offset = self.map_binary(BinaryOp::Add, offset, index, n);
        Some(self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(u),
            n,
        ))
    }
    fn map_entry(&mut self, key: TyId, value: TyId, n: NodeId) -> Option<MapEntry> {
        if let Some(entry) = self.map_entries.get(&(key, value)) {
            return Some(*entry);
        }
        let ty = self.t.mk(TyKind::Tuple(vec![
            scalar(Scalar::U64),
            scalar(Scalar::U64),
            key,
            value,
        ]));
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
