//! `Columns[T]` (MEM-24): a List's rows and charge, stored by column. The carrier
//! is three raw words, data/len/capacity. Column k of a block of capacity `cap`
//! starts at the sum, over the columns before it, of `cap × stride` rounded up to
//! 8 (`hirsel_core_layout::column_strides`); the block is charged as a List of the
//! row, `cap × logical(T)` rounded up to 8. A row moves by copying its columns;
//! the rows `clear` and `truncate` cut are cleaned as a List's are, in ascending
//! index order and each row's owning columns in field order (`ColumnDrain`).
use super::*;
use hirsel_core_ir::{ArithMode, ArithOp};

/// A row type's columns: each column's type, place and native stride.
#[derive(Clone)]
pub(super) struct Cols {
    item: TyId,
    split: bool,
    tys: Vec<TyId>,
    places: Vec<PlaceTy>,
    strides: Vec<ValueId>,
    /// The row's place, when it is held in one.
    row: Option<TypeId>,
    scalar: Option<Ty>,
}

/// The rows `[row, end)` of a cut `Columns` still to clean, row by row and each
/// row's owning columns in field order; `column` is the next column of `row`.
/// Both cursors advance before a drop, so a failed drop resumes after it.
#[derive(Clone)]
pub(super) struct ColumnDrain {
    columns: Vec<(ValueId, TypeId)>,
    row: AddressId,
    column: AddressId,
    end: ValueId,
}

/// Where growth sends a refusal.
#[derive(Clone, Copy)]
enum Refusal {
    Raise,
    Return(AddressId, TypeId),
}

const U64: Ty = Ty::Scalar(Scalar::U64);

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn columns_value(
        &mut self,
        name: &str,
        item: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let cols = self.columns_of(item, n)?;
        let header = |i: usize| vals.get(i).copied().flatten();
        Some(match name {
            "new" => {
                let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
                self.columns_fresh(&cols, zero, result, n)
            }
            "with_capacity" => self.columns_fresh(&cols, vals[0]?, result, n),
            "len" => Some(self.read_sequence_word(header(0)?, 8, U64, n)),
            "capacity" => Some(self.read_sequence_word(header(0)?, 16, U64, n)),
            "reserve" => {
                self.columns_room(&cols, header(0)?, vals[1]?, Refusal::Raise, n)?;
                None
            }
            "try_reserve" => self.columns_try_reserve(&cols, header(0)?, vals[1]?, result, n),
            "push" => {
                let c = header(0)?;
                let one = self.konst(U64, 1, n)?;
                self.columns_room(&cols, c, one, Refusal::Raise, n)?;
                let len = self.read_sequence_word(c, 8, U64, n);
                self.columns_store(&cols, c, len, vals[1], n)?;
                self.consume(vals[1], n);
                let (next, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, one, n);
                self.sequence_word(c, 8, U64, next, n);
                None
            }
            "pop" => self.columns_pop(&cols, header(0)?, result, n),
            "get" => self.columns_get(&cols, header(0)?, vals[1]?, result, n),
            "swap_remove" | "set" => {
                let c = header(0)?;
                let index = vals[1]?;
                let len = self.read_sequence_word(c, 8, U64, n);
                let present = self.list_compare(CompareOp::Lt, index, len, n);
                self.columns_bounds(present, n);
                let old = self.columns_load_value(&cols, c, index, n)?;
                if name == "set" {
                    self.columns_store(&cols, c, index, vals[2], n)?;
                    self.consume(vals[2], n);
                } else {
                    let one = self.konst(U64, 1, n)?;
                    let (last, _) = self.list_arith(ArithOp::Sub, ArithMode::Wrapping, len, one, n);
                    self.columns_move(&cols, c, last, index, n)?;
                    self.sequence_word(c, 8, U64, last, n);
                }
                self.temp(item, Some(old));
                Some(old)
            }
            "fill" => {
                self.columns_fill(&cols, header(0)?, vals[1]?, vals[2], n)?;
                None
            }
            "clear" => {
                let c = header(0)?;
                let zero = self.konst(U64, 0, n)?;
                let len = self.read_sequence_word(c, 8, U64, n);
                self.sequence_word(c, 8, U64, zero, n);
                self.columns_cut(&cols, c, zero, len, n)?;
                None
            }
            "truncate" => {
                let c = header(0)?;
                let keep = vals[1]?;
                let len = self.read_sequence_word(c, 8, U64, n);
                let shorter = self.list_compare(CompareOp::Lt, keep, len, n);
                let (cut, done) = (self.new_block(&[]), self.new_block(&[]));
                self.list_branch(shorter, cut, done, n);
                self.start(cut);
                self.sequence_word(c, 8, U64, keep, n);
                self.columns_cut(&cols, c, keep, len, n)?;
                self.jump(done, &[], n);
                self.start(done);
                None
            }
            _ => return None,
        })
    }

    /// The columns of row type `item`: one per field of a record row, else one.
    pub(super) fn columns_of(&mut self, item: TyId, n: NodeId) -> Option<Cols> {
        let rep = self.rep_at(item, n)?;
        let (row, scalar) = match rep {
            // A generic row splits as its instances do only when its declaration is
            // a record with fields: a bare parameter's instance may split or not.
            Rep::Place(id) if self.e.generic_names.contains_key(&id) => {
                if !self.record_row(item) {
                    self.gap(n, "a Columns of a type parameter's rows is not built");
                    return None;
                }
                (Some(id), None)
            }
            Rep::Place(id) if self.e.size(id) != 0 => (Some(id), None),
            Rep::Scalar(t) => (None, Some(t)),
            _ => {
                self.gap(n, "a Columns of zero-sized rows is not built");
                return None;
            }
        };
        let edge = self.edge_field(item, n)?;
        let fields = match &edge {
            FieldTy::Inline(t) => match &self.e.module.types[t.index()].body {
                hirsel_core_layout::Body::Record(fs) => fs.len(),
                _ => 0,
            },
            _ => 0,
        };
        let split = fields > 0;
        let tys = if split {
            (0..fields as u32)
                .map(|i| self.field_ty(item, i))
                .collect::<Option<Vec<_>>>()?
        } else {
            vec![item]
        };
        let mut places = Vec::new();
        let mut strides = Vec::new();
        for &ty in &tys {
            places.push(self.place_ty(ty, n)?);
            strides.push(self.layout_size(ty, false, n)?);
        }
        Some(Cols {
            item,
            split,
            tys,
            places,
            strides,
            row,
            scalar,
        })
    }

    /// Clean rows `[from, end)` of carrier `c`, whose length is already `from`, as
    /// `List[T]` cleans its cut elements: in ascending row order, each row's
    /// columns in field order. Columns without cleanup cost nothing.
    fn columns_cut(
        &mut self,
        cols: &Cols,
        c: ValueId,
        from: ValueId,
        end: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let owning: Vec<(usize, TypeId)> = (0..cols.tys.len())
            .filter_map(|k| self.drop_id(cols.tys[k]).map(|id| (k, id)))
            .collect();
        if owning.is_empty() {
            return Some(());
        }
        let data = self.read_sequence_word(c, 0, Ty::Ptr, n);
        let cap = self.read_sequence_word(c, 16, U64, n);
        let (offsets, _) = self.column_offsets(cols, cap, n)?;
        let mut columns = Vec::new();
        for (k, id) in owning {
            let at = self.address(
                AddressBase::Value(data),
                &[Step::Byte(offsets[k])],
                PlaceTy::Value(Ty::Scalar(Scalar::U8)),
                n,
            );
            columns.push((self.pointer(at, n), id));
        }
        let row = self.new_slot(PlaceTy::Value(U64), n);
        self.inst(
            Inst::Store {
                address: row,
                value: from,
            },
            n,
        );
        let column = self.new_slot(PlaceTy::Value(U64), n);
        let zero = self.konst(U64, 0, n)?;
        self.inst(
            Inst::Store {
                address: column,
                value: zero,
            },
            n,
        );
        self.column_drains.push(ColumnDrain {
            columns,
            row,
            column,
            end,
        });
        let mark = self.owner_mark();
        self.owners
            .push(cleanup::Owner::Columns(self.column_drains.len() as u32 - 1));
        self.changed();
        self.end_owners(mark, None, false, n);
        Some(())
    }

    /// Emit the loop of drain `i` (`ColumnDrain`), from where it stopped.
    pub(super) fn clean_columns(&mut self, i: u32, n: NodeId) {
        let d = self.column_drains[i as usize].clone();
        let (head, body, out) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.jump(head, &[], n);
        self.start(head);
        let row = self.read_bits(d.row, U64, n);
        let more = self.list_compare(CompareOp::Lt, row, d.end, n);
        self.list_branch(more, body, out, n);
        self.start(body);
        let column = self.read_bits(d.column, U64, n);
        let one = self.konst(U64, 1, n).unwrap();
        let zero = self.konst(U64, 0, n).unwrap();
        let last = d.columns.len() - 1;
        for (j, &(base, id)) in d.columns.iter().enumerate() {
            // The last owning column takes any cursor past the others.
            let this = self.new_block(&[]);
            let next = (j != last).then(|| self.new_block(&[]));
            if let Some(next) = next {
                let at = self.konst(U64, j as u64, n).unwrap();
                let is = self.list_compare(CompareOp::Eq, column, at, n);
                self.list_branch(is, this, next, n);
            } else {
                self.jump(this, &[], n);
            }
            self.start(this);
            if j == last {
                let after = self.word_op(BinaryOp::Add, row, one, n);
                self.inst(
                    Inst::Store {
                        address: d.row,
                        value: after,
                    },
                    n,
                );
                self.inst(
                    Inst::Store {
                        address: d.column,
                        value: zero,
                    },
                    n,
                );
            } else {
                let after = self.konst(U64, j as u64 + 1, n).unwrap();
                self.inst(
                    Inst::Store {
                        address: d.column,
                        value: after,
                    },
                    n,
                );
            }
            let element = self.address(
                AddressBase::Value(base),
                &[Step::Index(row)],
                PlaceTy::Declared(id),
                n,
            );
            self.emit_drop(AddressBase::Address(element), id, n);
            self.jump(head, &[], n);
            if let Some(next) = next {
                self.start(next);
            }
        }
        self.start(out);
    }

    /// Whether `ty` is an instance of a record declaration with fields.
    fn record_row(&mut self, ty: TyId) -> bool {
        match self.t.kind(self.bare(ty)).clone() {
            TyKind::Adt(d, _) => {
                matches!(&self.p.decls[d.index()].body, DeclBody::Record(fs) if !fs.is_empty())
            }
            _ => false,
        }
    }

    /// Each column's byte offset in a block of capacity `cap`, and the block size
    /// (saturating, so a size past the address space fails at the host).
    fn column_offsets(
        &mut self,
        cols: &Cols,
        cap: ValueId,
        n: NodeId,
    ) -> Option<(Vec<ValueId>, ValueId)> {
        let mut at = self.konst(U64, 0, n)?;
        let mut offsets = Vec::new();
        for &stride in &cols.strides {
            offsets.push(at);
            let (bytes, _) = self.list_arith(ArithOp::Mul, ArithMode::Saturating, cap, stride, n);
            let seven = self.konst(U64, 7, n)?;
            let (sum, _) = self.list_arith(ArithOp::Add, ArithMode::Saturating, bytes, seven, n);
            let mask = self.konst(U64, !7, n)?;
            let rounded = self.word_op(BinaryOp::BitAnd, sum, mask, n);
            (at, _) = self.list_arith(ArithOp::Add, ArithMode::Saturating, at, rounded, n);
        }
        Some((offsets, at))
    }

    /// Element `index` of column `k`, whose column starts `offset` bytes into `data`.
    fn column_element(
        &mut self,
        cols: &Cols,
        k: usize,
        data: ValueId,
        offset: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> AddressId {
        let base = self.address(
            AddressBase::Value(data),
            &[Step::Byte(offset)],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let base = self.pointer(base, n);
        self.address(
            AddressBase::Value(base),
            &[Step::Index(index)],
            cols.places[k],
            n,
        )
    }

    /// Every column element of row `index` of carrier `c`.
    fn row_elements(
        &mut self,
        cols: &Cols,
        c: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<Vec<AddressId>> {
        let data = self.read_sequence_word(c, 0, Ty::Ptr, n);
        let cap = self.read_sequence_word(c, 16, U64, n);
        let (offsets, _) = self.column_offsets(cols, cap, n)?;
        Some(
            (0..cols.tys.len())
                .map(|k| self.column_element(cols, k, data, offsets[k], index, n))
                .collect(),
        )
    }

    /// Field `k` of the row held at `row`, or the whole row when it is not split.
    fn row_part(&mut self, cols: &Cols, row: ValueId, k: usize, n: NodeId) -> AddressId {
        let id = cols.row.expect("a split row is held in a place");
        let whole = self.address(AddressBase::Value(row), &[], PlaceTy::Declared(id), n);
        if !cols.split {
            return whole;
        }
        self.address(
            AddressBase::Address(whole),
            &[Step::Field(k as u32)],
            cols.places[k],
            n,
        )
    }

    /// Copy the row `value` into row `index`; the caller consumes `value`.
    fn columns_store(
        &mut self,
        cols: &Cols,
        c: ValueId,
        index: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<()> {
        let value = value?;
        let to = self.row_elements(cols, c, index, n)?;
        if cols.scalar.is_some() {
            self.inst(
                Inst::Store {
                    address: to[0],
                    value,
                },
                n,
            );
            return Some(());
        }
        for (k, &to) in to.iter().enumerate() {
            let from = self.row_part(cols, value, k, n);
            let bytes = cols.strides[k];
            self.inst(Inst::Copy { to, from, bytes }, n);
        }
        Some(())
    }

    /// Copy row `index` into the row place `to`.
    fn columns_load(
        &mut self,
        cols: &Cols,
        c: ValueId,
        index: ValueId,
        to: AddressId,
        n: NodeId,
    ) -> Option<()> {
        let from = self.row_elements(cols, c, index, n)?;
        if let Some(t) = cols.scalar {
            let value = self.read_bits(from[0], t, n);
            self.inst(Inst::Store { address: to, value }, n);
            return Some(());
        }
        let row = self.pointer(to, n);
        for (k, &from) in from.iter().enumerate() {
            let to = self.row_part(cols, row, k, n);
            let bytes = cols.strides[k];
            self.inst(Inst::Copy { to, from, bytes }, n);
        }
        Some(())
    }

    /// Row `index` as a value: a scalar, or a pointer to a fresh row place.
    fn columns_load_value(
        &mut self,
        cols: &Cols,
        c: ValueId,
        index: ValueId,
        n: NodeId,
    ) -> Option<ValueId> {
        if let Some(t) = cols.scalar {
            let from = self.row_elements(cols, c, index, n)?;
            return Some(self.read_bits(from[0], t, n));
        }
        let id = cols.row?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.columns_load(cols, c, index, to, n)?;
        Some(self.pointer(to, n))
    }

    /// Copy row `from` over row `to` (both in bounds).
    fn columns_move(
        &mut self,
        cols: &Cols,
        c: ValueId,
        from: ValueId,
        to: ValueId,
        n: NodeId,
    ) -> Option<()> {
        let source = self.row_elements(cols, c, from, n)?;
        let target = self.row_elements(cols, c, to, n)?;
        let differ = self.list_compare(CompareOp::Ne, from, to, n);
        let (copy, done) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(differ, copy, done, n);
        self.start(copy);
        for k in 0..cols.tys.len() {
            let bytes = cols.strides[k];
            self.inst(
                Inst::Copy {
                    to: target[k],
                    from: source[k],
                    bytes,
                },
                n,
            );
        }
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }

    fn columns_bounds(&mut self, present: ValueId, n: NodeId) {
        let (yes, no) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(present, yes, no, n);
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

    fn columns_header(&mut self, to: AddressId, data: ValueId, cap: ValueId, n: NodeId) {
        let pointer = self.pointer(to, n);
        let zero = self.konst(U64, 0, n).unwrap();
        self.sequence_word(pointer, 0, Ty::Ptr, data, n);
        self.sequence_word(pointer, 8, U64, zero, n);
        self.sequence_word(pointer, 16, U64, cap, n);
    }

    /// The charge of `count` rows, as a List of the row's (MEM-24), and whether
    /// it overflows.
    fn columns_charge(
        &mut self,
        cols: &Cols,
        count: ValueId,
        extra: Option<ValueId>,
        n: NodeId,
    ) -> Option<super::lists::ListPlan> {
        let mut plan = self.list_plan(cols.item, count, extra, n)?;
        plan.edge = FieldTy::Columns(Box::new(self.edge_field(cols.item, n)?));
        (_, plan.size) = self.column_offsets(cols, count, n)?;
        Some(plan)
    }

    /// `columns.new` and `columns.with_capacity`: a carrier with room for `count`
    /// rows, allocated in the current limit; a refusal raises `Capacity`.
    fn columns_fresh(
        &mut self,
        cols: &Cols,
        count: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.place_row(result, "a Columns without a place is not built", n)?;
        let to = self.result_slot(PlaceTy::Declared(id), n);
        let zero = self.konst(U64, 0, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let empty = self.list_compare(CompareOp::Eq, count, zero, n);
        let (none, some, done) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.list_branch(empty, none, some, n);
        self.start(none);
        self.columns_header(to, null, zero, n);
        self.jump(done, &[], n);
        self.start(some);
        let plan = self.columns_charge(cols, count, None, n)?;
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        let admitted = self.list_compare(CompareOp::Ne, block, null, n);
        self.raise_unless(admitted, plan.charge, available, n);
        self.columns_header(to, block, count, n);
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    /// Room for `additional` more rows. Growth admits the new block once, at
    /// `max(needed, 4, 2 × capacity)` rows, copies each column and refunds the
    /// old block; a refusal leaves len, capacity and the charge unchanged
    /// (MEM-21, MEM-26).
    fn columns_room(
        &mut self,
        cols: &Cols,
        c: ValueId,
        additional: ValueId,
        refusal: Refusal,
        n: NodeId,
    ) -> Option<()> {
        let len = self.read_sequence_word(c, 8, U64, n);
        let old_cap = self.read_sequence_word(c, 16, U64, n);
        let (needed, len_over) =
            self.list_arith(ArithOp::Add, ArithMode::Checked, len, additional, n);
        let len_over = len_over?;
        let lacks = self.list_compare(CompareOp::Gt, needed, old_cap, n);
        let grow = self.list_bool(BinaryOp::BitOr, lacks, len_over, n);
        let (growth, success) = (self.new_block(&[]), self.new_block(&[]));
        let yes = self.edge(growth, &[]);
        self.b.edges[yes.index()].cold = true;
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
        let two = self.konst(U64, 2, n)?;
        let four = self.konst(U64, 4, n)?;
        let (double, cap_over) = self.list_arith(ArithOp::Mul, ArithMode::Checked, old_cap, two, n);
        let over = self.list_bool(BinaryOp::BitOr, len_over, cap_over?, n);
        let new_cap = self.list_max_u64(needed, four, n);
        let new_cap = self.list_max_u64(new_cap, double, n);
        let plan = self.columns_charge(cols, new_cap, Some(over), n)?;
        let zero = self.konst(U64, 0, n)?;
        let null = self.konst(Ty::Ptr, 0, n)?;
        let old_data = self.read_sequence_word(c, 0, Ty::Ptr, n);
        let owned = self.list_compare(CompareOp::Ne, old_cap, zero, n);
        let (like, current) = (self.new_block(&[]), self.new_block(&[]));
        let allocated = self.new_block(&[Ty::Ptr, U64]);
        self.list_branch(owned, like, current, n);
        self.start(like);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Like(old_data), n);
        self.jump(allocated, &[block, available], n);
        self.start(current);
        let (block, available) = self.list_allocate(&plan, AllocPlace::Current, n);
        self.jump(allocated, &[block, available], n);
        self.start(allocated);
        let [block, available] = self.block_params(allocated)[..] else {
            unreachable!("an allocation joins its block and room")
        };
        let ok = self.list_compare(CompareOp::Ne, block, null, n);
        let (install, refused) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(ok, install, refused, n);
        self.start(install);
        let nonempty = self.list_compare(CompareOp::Ne, len, zero, n);
        let (copy, copied) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(nonempty, copy, copied, n);
        self.start(copy);
        let (old_offsets, _) = self.column_offsets(cols, old_cap, n)?;
        let (new_offsets, _) = self.column_offsets(cols, new_cap, n)?;
        let byte = PlaceTy::Value(Ty::Scalar(Scalar::U8));
        for k in 0..cols.tys.len() {
            let from = self.address(
                AddressBase::Value(old_data),
                &[Step::Byte(old_offsets[k])],
                byte,
                n,
            );
            let to = self.address(
                AddressBase::Value(block),
                &[Step::Byte(new_offsets[k])],
                byte,
                n,
            );
            let (bytes, _) =
                self.list_arith(ArithOp::Mul, ArithMode::Wrapping, len, cols.strides[k], n);
            self.inst(Inst::Copy { to, from, bytes }, n);
        }
        self.jump(copied, &[], n);
        self.start(copied);
        let (free, freed) = (self.new_block(&[]), self.new_block(&[]));
        self.list_branch(owned, free, freed, n);
        self.start(free);
        let (raw, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, old_cap, plan.logical, n);
        let charge = self.round8(raw, n)?;
        self.inst(
            Inst::Free {
                block: old_data,
                charge,
            },
            n,
        );
        self.jump(freed, &[], n);
        self.start(freed);
        self.sequence_word(c, 0, Ty::Ptr, block, n);
        self.sequence_word(c, 16, U64, new_cap, n);
        if let Refusal::Return(to, _) = refusal {
            self.inst(
                Inst::SetCase {
                    address: to,
                    case: 0,
                },
                n,
            );
        }
        self.jump(success, &[], n);
        self.start(refused);
        match refusal {
            Refusal::Raise => self.raise_capacity(plan.charge, available, n),
            Refusal::Return(to, cap_id) => {
                self.list_refused(to, cap_id, plan.charge, available, n);
                self.jump(success, &[], n);
            }
        }
        self.start(success);
        Some(())
    }

    fn list_max_u64(&mut self, left: ValueId, right: ValueId, n: NodeId) -> ValueId {
        self.word_op(BinaryOp::Max, left, right, n)
    }

    fn columns_try_reserve(
        &mut self,
        cols: &Cols,
        c: ValueId,
        additional: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.place_row(result, "a result without a place is not built", n)?;
        let cap_ty = self.prelude("Capacity", n)?;
        let cap_ty = self.t.mk(TyKind::Adt(cap_ty, vec![]));
        let cap_id = self.place_row(cap_ty, "a capacity without a place is not built", n)?;
        let to = self.new_slot(PlaceTy::Declared(id), n);
        // Room already there is Ok too: the success edge sets the case.
        let len = self.read_sequence_word(c, 8, U64, n);
        let cap = self.read_sequence_word(c, 16, U64, n);
        let (needed, over) = self.list_arith(ArithOp::Add, ArithMode::Checked, len, additional, n);
        let fits = self.list_compare(CompareOp::Le, needed, cap, n);
        let yes = self.konst(Ty::Scalar(Scalar::Bool), 1, n)?;
        let no_over = self.list_bool(BinaryOp::BitXor, over?, yes, n);
        let fits = self.list_bool(BinaryOp::BitAnd, fits, no_over, n);
        let (ok, grow, done) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
        self.list_branch(fits, ok, grow, n);
        self.start(ok);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(done, &[], n);
        self.start(grow);
        self.columns_room(cols, c, additional, Refusal::Return(to, cap_id), n)?;
        self.jump(done, &[], n);
        self.start(done);
        let value = self.pointer(to, n);
        self.temp(result, Some(value));
        Some(value)
    }

    fn columns_pop(&mut self, cols: &Cols, c: ValueId, result: TyId, n: NodeId) -> Option<ValueId> {
        let id = self.place_row(result, "an option without a place is not built", n)?;
        let out = self.new_slot(PlaceTy::Declared(id), n);
        let len = self.read_sequence_word(c, 8, U64, n);
        let zero = self.konst(U64, 0, n)?;
        let present = self.list_compare(CompareOp::Ne, len, zero, n);
        self.columns_option(cols, c, out, present, n, |cx| {
            let one = cx.konst(U64, 1, n)?;
            let (last, _) = cx.list_arith(ArithOp::Sub, ArithMode::Wrapping, len, one, n);
            cx.sequence_word(c, 8, U64, last, n);
            Some(last)
        })?;
        let value = self.pointer(out, n);
        self.temp(result, Some(value));
        Some(value)
    }

    fn columns_get(
        &mut self,
        cols: &Cols,
        c: ValueId,
        index: ValueId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = self.place_row(result, "an option without a place is not built", n)?;
        let out = self.result_slot(PlaceTy::Declared(id), n);
        let len = self.read_sequence_word(c, 8, U64, n);
        let present = self.list_compare(CompareOp::Lt, index, len, n);
        self.columns_option(cols, c, out, present, n, |_| Some(index))?;
        let value = self.pointer(out, n);
        self.temp(result, Some(value));
        Some(value)
    }

    /// `None`, or `Some` of the row `row` picks, into the option place `out`.
    fn columns_option(
        &mut self,
        cols: &Cols,
        c: ValueId,
        out: AddressId,
        present: ValueId,
        n: NodeId,
        row: impl FnOnce(&mut Self) -> Option<ValueId>,
    ) -> Option<()> {
        let (yes, no, done) = (
            self.new_block(&[]),
            self.new_block(&[]),
            self.new_block(&[]),
        );
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
        let index = row(self)?;
        self.inst(
            Inst::SetCase {
                address: out,
                case: 1,
            },
            n,
        );
        let place = self.place_ty(cols.item, n)?;
        let to = self.address(
            AddressBase::Address(out),
            &[Step::CaseField { case: 1, field: 0 }],
            place,
            n,
        );
        self.columns_load(cols, c, index, to, n)?;
        self.jump(done, &[], n);
        self.start(done);
        Some(())
    }

    /// `columns.fill`: `count` copies of `value` appended.
    fn columns_fill(
        &mut self,
        cols: &Cols,
        c: ValueId,
        count: ValueId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<()> {
        self.columns_room(cols, c, count, Refusal::Raise, n)?;
        // One unit per element and one per logical byte, so a zero-sized row
        // still pays for its stores' loop (MEM-30, "Work steps").
        let size = self.layout_size(cols.item, true, n)?;
        let one = self.konst(U64, 1, n)?;
        let (per, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, size, one, n);
        let (units, _) = self.list_arith(ArithOp::Mul, ArithMode::Wrapping, count, per, n);
        self.charge_work(hirsel_core_ids::work_steps::table::COLUMNS_FILL, units, n)?;
        let len = self.read_sequence_word(c, 8, U64, n);
        let (end, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, len, count, n);
        let head = self.new_block(&[U64]);
        let (body, done) = (self.new_block(&[]), self.new_block(&[]));
        self.jump(head, &[len], n);
        self.start(head);
        let i = self.block_params(head)[0];
        let more = self.list_compare(CompareOp::Lt, i, end, n);
        self.list_branch(more, body, done, n);
        self.start(body);
        self.columns_store(cols, c, i, value, n)?;
        let one = self.konst(U64, 1, n)?;
        let (next, _) = self.list_arith(ArithOp::Add, ArithMode::Wrapping, i, one, n);
        self.jump(head, &[next], n);
        self.start(done);
        self.sequence_word(c, 8, U64, end, n);
        Some(())
    }

    /// Column `k` of the Columns at `header` as a `List[F]` carrier in a fresh
    /// place: a lent view of the column (capacity = len), never owned or freed.
    pub(super) fn column_list(
        &mut self,
        item: TyId,
        k: u32,
        header: ValueId,
        list: TyId,
        n: NodeId,
    ) -> Option<AddressId> {
        let cols = self.columns_of(item, n)?;
        let carrier = self.seq_carrier(list, n)?;
        let data = self.read_sequence_word(header, 0, Ty::Ptr, n);
        let len = self.read_sequence_word(header, 8, U64, n);
        let cap = self.read_sequence_word(header, 16, U64, n);
        let (offsets, _) = self.column_offsets(&cols, cap, n)?;
        let base = self.address(
            AddressBase::Value(data),
            &[Step::Byte(offsets[k as usize])],
            PlaceTy::Value(Ty::Scalar(Scalar::U8)),
            n,
        );
        let base = self.pointer(base, n);
        let to = self.new_slot(PlaceTy::Declared(carrier), n);
        let pointer = self.pointer(to, n);
        self.set_carrier_word(pointer, carrier, SeqWord::Data, base, n);
        self.set_carrier_word(pointer, carrier, SeqWord::Cap, len, n);
        self.set_carrier_word(pointer, carrier, SeqWord::Len, len, n);
        Some(to)
    }

    /// Step `at` from a `Columns[T]` to its column `name`, a `List` of that field of
    /// `T` (MEM-24); None when `inner` is not a Columns.
    pub(super) fn column_field(
        &mut self,
        at: &mut At,
        inner: TyId,
        view: bool,
        name: &str,
        n: NodeId,
    ) -> Option<Result<(), ()>> {
        let TyKind::Adt(d, args) = self.t.kind(inner).clone() else {
            return None;
        };
        if self.p.decls[d.index()].role != Some(Role::Columns) {
            return None;
        }
        let TyKind::Adt(row, row_args) = self.t.kind(args[0]).clone() else {
            self.refuse(
                "LANG-9",
                n,
                format!("a Columns of this row has no column `{name}`"),
            );
            return Some(Err(()));
        };
        let decl = &self.p.decls[row.index()];
        if decl.opaque && decl.site.module != self.p.modules[self.at.module as usize].id {
            let row_name = decl.path.0.name.clone();
            self.refuse(
                "PROG-2",
                n,
                format!("`{row_name}` is opaque outside its module"),
            );
            return Some(Err(()));
        }
        let DeclBody::Record(fs) = &decl.body else {
            self.refuse(
                "LANG-9",
                n,
                format!("a Columns of this row has no column `{name}`"),
            );
            return Some(Err(()));
        };
        let Some(i) = fs.iter().position(|f| f.name == name) else {
            self.refuse("LANG-9", n, format!("this row has no field `{name}`"));
            return Some(Err(()));
        };
        let ft = subst(&mut self.t, fs[i].ty, &row_args);
        let list = self.list_ty(ft, n)?;
        self.steps.push(PStep::Column(i as u32));
        at.place.len += 1;
        at.shared |= view;
        at.ty = if view {
            self.t.mk(TyKind::View(list))
        } else {
            list
        };
        Some(Ok(()))
    }

    /// Column `k` of the Columns type `ty`: its field name and its `List` type.
    pub(super) fn column_shown(&mut self, ty: TyId, k: u32) -> (String, TyId) {
        let TyKind::Adt(_, args) = self.t.kind(self.bare(ty)).clone() else {
            unreachable!("a column step follows a Columns")
        };
        let TyKind::Adt(row, row_args) = self.t.kind(args[0]).clone() else {
            unreachable!("a column's row is a record")
        };
        let DeclBody::Record(fs) = &self.p.decls[row.index()].body else {
            unreachable!("a column's row is a record")
        };
        let name = fs[k as usize].name.clone();
        let ft = subst(&mut self.t, fs[k as usize].ty, &row_args);
        let list_decl = self
            .p
            .core_decl("List")
            .expect("a column step was typed with the prelude");
        let list = self.t.mk(TyKind::Adt(list_decl, vec![ft]));
        (name.to_string(), list)
    }

    /// Whether place `at` ends at a whole column.
    pub(super) fn ends_in_column(&self, at: At) -> bool {
        at.place.len > 0
            && matches!(
                self.steps[(at.place.start + at.place.len - 1) as usize],
                PStep::Column(_)
            )
    }
}
