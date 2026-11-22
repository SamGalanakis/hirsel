//! Core operations whose definition is the typed value operation itself.
use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// None means this operation needs a different kernel. Some(None) is a handled
    /// operation with a unit result or an already reported emission gap.
    pub(super) fn core_value(
        &mut self,
        f: usize,
        types: &[TyId],
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        let g = &self.p.fns[f];
        if g.body.is_some() || !g.path.package_is("core", "1") {
            return None;
        }
        let module = g.path.0.modules.as_slice();
        let name = g.path.0.name.as_str();
        if matches!(module, [m] if matches!(m.as_str(), "list" | "deque" | "heap"))
            && name.starts_with("slots_")
        {
            return self.slots_value(name, types[0], vals, result, n);
        }
        if module == ["bytebuf"] {
            return match name {
                "append_inline" => Some(
                    self.append_inline_bytes(types[0], vals[0]?, vals[1], vals[2]?, vals[3]?, n),
                ),
                "freeze" => Some(self.freeze_bytes(vals[0]?, result, n)),
                "as_bytes" => Some(self.view_frozen_bytes(vals[0]?, result, n)),
                _ => None,
            };
        }
        if module == ["map"] {
            return self.map_value(name, types, vals, result, n);
        }
        if module == ["list"] && matches!(name, "word_le" | "word_be") {
            return Some(self.list_word(vals[0]?, vals[1]?, result, name == "word_be", n));
        }
        if module == ["list"] && matches!(name, "put_word_le" | "put_word_be") {
            self.list_put_word(vals[0]?, vals[1]?, types[0], name == "put_word_be", n)?;
            return Some(None);
        }
        if module == ["order"] && name == "compare" {
            return Some(self.order_compare(types[0], vals[0], vals[1], result, n));
        }
        if module == ["list"] && name == "compare_bytes" {
            return Some(self.list_compare_bytes(vals[0]?, vals[1]?, result, n));
        }
        if module == ["list"] && matches!(name, "set_word_le" | "set_word_be") {
            let big_endian = name == "set_word_be";
            self.list_set_word(vals[0]?, vals[1]?, vals[2]?, types[0], big_endian, n)?;
            return Some(None);
        }
        if module == ["inline"] && matches!(name, "set_word_le" | "set_word_be") {
            let big_endian = name == "set_word_be";
            let (array, word) = (types[0], types[1]);
            self.inline_set_word(array, vals[0], vals[1]?, vals[2]?, word, big_endian, n)?;
            return Some(None);
        }
        if module == ["inline"] && matches!(name, "word_le" | "word_be") {
            return Some(self.inline_word(
                types[0],
                vals[0],
                vals[1]?,
                result,
                name == "word_be",
                n,
            ));
        }
        if module == ["inline"] && name == "copy_within" {
            self.inline_copy_within(types[0], vals[0], vals[1]?, vals[2]?, vals[3]?, n)?;
            return Some(None);
        }
        if module == ["inline"] && name == "copy_from_slice" {
            self.inline_copy_from_slice(types[0], vals[0], vals[1]?, vals[2], vals[3]?, n)?;
            return Some(None);
        }
        if module == ["check"] {
            return match name {
                "xxh3_short" => Some(self.xxh3_short(vals[0]?, None, n)),
                "xxh3_short_inline" => Some(self.xxh3_short(vals[0]?, Some(vals[1]?), n)),
                "xxh3_stripes" => {
                    Some(self.xxh3_stripes(vals[0]?, vals[1]?, vals[2]?, vals[3]?, n))
                }
                "xxh3_stripe" => Some(self.xxh3_stripe(vals[0]?, vals[1]?, vals[2]?, vals[3]?, n)),
                _ => None,
            };
        }
        if module == ["crc32c"] && name == "update" {
            return Some(self.crc32c_update(vals[0]?, vals[1]?, n));
        }
        if module == ["sha1"] && name == "blocks" {
            return Some(self.sha1_blocks(vals[0]?, vals[1]?, n));
        }
        if module == ["sha256"] {
            return match name {
                "blocks" => Some(self.sha256_blocks(vals[0]?, vals[1]?, vals[2]?, vals[3]?, n)),
                "block" => Some(self.sha256_block(vals[0]?, vals[1]?, n)),
                _ => None,
            };
        }
        let verifier = match (module, name) {
            ([m], "verify") if m == "ed25519" => Some(hirsel_core_ir::RtOp::Ed25519Verify),
            ([m], "verify") if m == "p256" => Some(hirsel_core_ir::RtOp::P256Verify),
            ([m], "verify_der") if m == "p256" => Some(hirsel_core_ir::RtOp::P256VerifyDer),
            _ => None,
        };
        if let Some(op) = verifier {
            return Some(self.verify_signature(op, [vals[0]?, vals[1]?, vals[2]?], n));
        }
        if module == ["list"] {
            return self.list_value(name, types[0], vals, result, n);
        }
        if module == ["core"] {
            match name {
                "replace" => return Some(self.replace_value(types[0], vals[0], vals[1], n)),
                "site_word" => return Some(vals[0]),
                _ => {}
            }
        }
        if (module == ["option"] || module == ["result"]) && name == "unwrap" {
            let input = subst(&mut self.t, g.params[0].ty, types);
            return Some(self.unwrap_value(input, result, vals[0], module == ["option"], n));
        }
        None
    }

    /// Shared class-A carrier lowering, also used by string rows at T = u8.
    pub(super) fn list_value(
        &mut self,
        name: &str,
        item: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<Option<ValueId>> {
        match name {
            "new" => return Some(self.list_new(item, result, n)),
            "slice" => return Some(self.list_slice(item, vals[0]?, vals[1]?, vals[2]?, result, n)),
            "pop" => return Some(self.list_pop(item, vals[0]?, result, n)),
            "clear" => {
                self.list_clear(item, vals[0]?, n);
                return Some(None);
            }
            "truncate" => {
                self.list_truncate(item, vals[0]?, vals[1]?, n);
                return Some(None);
            }
            "push" => return Some(self.list_push(item, vals[0]?, vals[1], n, None)),
            "fill" => {
                self.list_fill(item, vals[0]?, vals[1]?, vals[2], n)?;
                return Some(None);
            }
            "append" => {
                self.list_append(item, vals[0]?, vals[1]?, n)?;
                return Some(None);
            }
            "copy_from_slice" => {
                self.list_copy_from_slice(item, vals[0]?, vals[1]?, vals[2]?, n)?;
                return Some(None);
            }
            "copy_within" => {
                self.list_copy_within(item, vals[0]?, vals[1]?, vals[2]?, vals[3]?, n)?;
                return Some(None);
            }
            "fill_slice" => {
                self.list_fill_slice(item, vals[0]?, vals[1]?, vals[2]?, vals[3], n)?;
                return Some(None);
            }
            "reserve" => {
                self.list_reserve_shared(item, vals[0]?, vals[1]?, super::lists::Ask::Reserve, n)?;
                return Some(None);
            }
            "try_reserve" => {
                return Some(self.list_try_reserve(item, vals[0]?, vals[1]?, result, n));
            }
            "get" => return Some(self.list_get(item, vals[0]?, vals[1]?, result, n)),
            "set" => return Some(self.list_set(item, vals[0]?, vals[1]?, vals[2], n)),
            "swap" => {
                self.list_swap(item, vals[0]?, vals[1]?, vals[2]?, n)?;
                return Some(None);
            }
            "with_capacity" => {
                return Some(self.list_with_capacity(item, vals[0]?, result, n));
            }
            "len" | "capacity" => {
                let word = if name == "len" {
                    growth::FillWord::Used
                } else {
                    growth::FillWord::Capacity
                };
                let carrier = self.list_carrier(item, n)?;
                return Some(Some(self.fill_word(vals[0]?, Some(carrier), word, n)));
            }
            _ => {}
        }
        None
    }

    pub(super) fn empty_list(&mut self, ty: TyId, n: NodeId) -> Option<ValueId> {
        let id = match self.rep(ty) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let slot = self.result_slot(PlaceTy::Declared(id), n);
        let pointer = self.pointer(slot, n);
        let null = self.konst(Ty::Ptr, 0, n)?;
        let zero = self.konst(Ty::Scalar(Scalar::U64), 0, n)?;
        // An own List or String's words are typed (FORM #113), capacity before len so
        // a len store is checked against it; a Map or slots header is three raw words.
        match self.seq_carrier(ty, n) {
            Some(carrier) => {
                self.set_carrier_word(pointer, carrier, SeqWord::Data, null, n);
                self.set_carrier_word(pointer, carrier, SeqWord::Cap, zero, n);
                self.set_carrier_word(pointer, carrier, SeqWord::Len, zero, n);
            }
            None => {
                self.sequence_word(pointer, 0, Ty::Ptr, null, n);
                self.sequence_word(pointer, 8, Ty::Scalar(Scalar::U64), zero, n);
                self.sequence_word(pointer, 16, Ty::Scalar(Scalar::U64), zero, n);
            }
        }
        self.temp(ty, Some(pointer));
        Some(pointer)
    }

    pub(super) fn sequence_word(
        &mut self,
        pointer: ValueId,
        offset: u64,
        ty: Ty,
        value: ValueId,
        n: NodeId,
    ) {
        let offset = self.konst(Ty::Scalar(Scalar::U64), offset, n).unwrap();
        let address = self.address(
            AddressBase::Value(pointer),
            &[Step::Byte(offset)],
            PlaceTy::Value(ty),
            n,
        );
        self.inst(Inst::Store { address, value }, n);
    }

    pub(super) fn replace_value(
        &mut self,
        ty: TyId,
        pointer: Option<ValueId>,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let rep = match self.rep(ty) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        // A zero-sized value has no place: nothing is replaced and the result is unit.
        let Some(place) = rep.stored() else {
            return None;
        };
        let address = self.address(AddressBase::Value(pointer?), &[], place, n);
        let bare = self.bare(ty);
        if matches!(rep, Rep::Place(_))
            && (has_param(&self.t, bare)
                || matches!(self.t.kind(bare), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::List)))
        {
            self.inst(Inst::PromoteFrame { carrier: address }, n);
        }
        let old = match rep {
            Rep::Scalar(t) => Some(self.read_bits(address, t, n)),
            Rep::Place(_) => Some(self.pointer(address, n)),
            Rep::None => None,
        };
        let saved = self.slot(ty, old, n)?;
        self.install(address, rep, value, n);
        match rep {
            Rep::Scalar(t) => Some(self.read_bits(saved, t, n)),
            Rep::Place(_) => {
                let value = self.pointer(saved, n);
                self.temp(ty, Some(value));
                Some(value)
            }
            Rep::None => None,
        }
    }

    fn unwrap_value(
        &mut self,
        input: TyId,
        output: TyId,
        value: Option<ValueId>,
        option: bool,
        n: NodeId,
    ) -> Option<ValueId> {
        let value = value?;
        let id = match self.rep(input) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        let address = self.address(AddressBase::Value(value), &[], PlaceTy::Declared(id), n);
        let tag = self.value(Ty::Scalar(Scalar::U32));
        self.inst(
            Inst::Case {
                result: tag,
                address,
            },
            n,
        );
        let success = u32::from(option);
        let expected = self.konst(Ty::Scalar(Scalar::U32), u64::from(success), n)?;
        let on = self.value(Ty::Scalar(Scalar::Bool));
        self.inst(
            Inst::Compare {
                result: on,
                op: CompareOp::Eq,
                left: tag,
                right: expected,
            },
            n,
        );
        let yes = self.new_block(&[]);
        let no = self.new_block(&[]);
        let ye = self.edge(yes, &[]);
        let ne = self.edge(no, &[]);
        self.end(
            Terminator::Branch {
                condition: on,
                yes: ye,
                no: ne,
            },
            Some(n),
        );
        self.start(no);
        let cleanup = self.fail_to(n);
        self.end(
            Terminator::Raise {
                cause: hirsel_core_ir::RaiseCause::Unwrap,
                cleanup,
            },
            Some(n),
        );
        self.start(yes);
        let rep = match self.rep(output) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        self.consume(Some(value), n);
        // A zero-sized payload has no place: the unwrapped value is unit.
        let Some(place) = rep.stored() else {
            return None;
        };
        let payload = self.address(
            AddressBase::Address(address),
            &[Step::CaseField {
                case: success,
                field: 0,
            }],
            place,
            n,
        );
        match rep {
            Rep::Scalar(t) => Some(self.read_bits(payload, t, n)),
            Rep::Place(id) => {
                let source = self.pointer(payload, n);
                let slot = self.new_slot(PlaceTy::Declared(id), n);
                self.copy_to(slot, source, id, n);
                let value = self.pointer(slot, n);
                self.temp(output, Some(value));
                Some(value)
            }
            Rep::None => None,
        }
    }
}
