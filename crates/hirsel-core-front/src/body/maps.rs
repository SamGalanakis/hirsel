//! Map carrier/kernel sequences. Probing belongs to core/map.hirsel.
use super::*;

/// The entry tuple uses exactly map_entry's hash/state/key/value layout. Keeping
/// field steps symbolic lets the ordinary template materializer relocate K and V.
#[derive(Clone, Copy)]
pub(super) struct MapEntry {
    ty: TyId,
    place: TypeId,
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
            _ => None,
        }
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
        let entry = MapEntry { ty, place };
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
