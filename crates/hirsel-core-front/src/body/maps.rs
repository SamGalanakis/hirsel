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

impl<'c, 'a> Cx<'c, 'a, '_> {
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
            "slots" | "capacity" => Some(Some(self.read_sequence_word(
                vals[0]?,
                if name == "slots" { 8 } else { 16 },
                Ty::Scalar(Scalar::U64),
                n,
            ))),
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
            "index_len" => Some(self.map_index_len(vals[0]?, n)),
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
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let cap = self.read_sequence_word(header, 16, Ty::Scalar(Scalar::U64), n);
        let stride = self.layout_size(entry.ty, false, n)?;
        let (offset, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, cap, stride, n);
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
    fn map_index_len(&mut self, header: ValueId, n: NodeId) -> Option<ValueId> {
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
        let count = self.map_binary(BinaryOp::Shl, one, bits, n);
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
        let offset = self.map_binary(BinaryOp::Add, entries, eight, n);
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
        let Rep::Place(place) = self.rep(ty).map_err(|what| self.gap(n, what)).ok()? else {
            return None;
        };
        let collection = self
            .t
            .mk(TyKind::Adt(self.p.core_decl("Map")?, vec![key, value]));
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
