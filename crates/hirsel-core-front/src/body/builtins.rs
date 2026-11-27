//! Core operations whose definition is the typed value operation itself.
use super::*;
use hirsel_core_ir::RtOp;

// Arity of the compiler kernels below, including the delegated collection kernels.
// The selected core sources declare the call's signature, so they can disagree
// with these operand layouts even after the call has passed type checking.
fn builtin_arity(module: &str, name: &str) -> Option<(usize, usize)> {
    Some(match (module, name) {
        ("list" | "deque" | "heap", "slots_empty") => (1, 0),
        (
            "list" | "deque" | "heap",
            "slots_new" | "slots_needs_drop" | "slots_free" | "slots_finish_list",
        ) => (1, 1),
        (
            "list" | "deque" | "heap",
            "slots_unmark_spare"
            | "slots_publish_spare"
            | "slots_take"
            | "slots_view"
            | "slots_list_view"
            | "slots_forget_ring",
        ) => (1, 2),
        ("list" | "deque" | "heap", "slots_put" | "slots_shift") => (1, 3),
        ("list" | "deque" | "heap", "slots_move" | "slots_list_move") => (1, 5),
        ("int" | "string" | "list" | "deque", "work_steps") => (0, 2),
        ("bytebuf", "append_inline") => (1, 4),
        ("bytebuf", "freeze" | "as_bytes" | "with_tail") => (0, 1),
        ("bytebuf", "from_tail") => (0, 2),
        ("bytebuf", "try_reserve_hint") => (0, 3),
        ("map", "new") => (2, 0),
        ("map", "hash" | "place" | "strong") => (1, 1),
        (
            "map",
            "len" | "compact" | "advance" | "strong_empty" | "set_strong_empty" | "mode"
            | "group_mask" | "set_strong" | "clear_index" | "fast_class" | "slots" | "capacity",
        ) => (2, 1),
        (
            "map",
            "group_empty" | "group_vacant" | "bucket_entry" | "erase" | "regrow" | "try_regrow"
            | "take_entry" | "kill" | "slot_meta" | "slot_key" | "slot_value",
        ) => (2, 2),
        ("map", "push_entry" | "group_match" | "swap_value") => (2, 3),
        ("map", "copy_entry" | "install") => (2, 4),
        ("map", "with_value") => (3, 3),
        ("list" | "columns", "new") => (1, 0),
        ("list" | "columns", "len" | "capacity" | "pop" | "clear" | "with_capacity") => (1, 1),
        ("list" | "columns", "truncate" | "push" | "reserve" | "try_reserve" | "get") => (1, 2),
        ("list" | "columns", "fill" | "set") => (1, 3),
        ("columns", "swap_remove") => (1, 2),
        ("list", "compare_bytes") => (0, 2),
        (
            "list",
            "word_le"
            | "word_be"
            | "put_word_le"
            | "put_word_be"
            | "append"
            | "with_capacity_aligned",
        ) => (1, 2),
        ("list", "set_word_le" | "set_word_be" | "slice" | "swap" | "copy_from_slice") => (1, 3),
        ("list", "copy_within" | "fill_slice") => (1, 4),
        ("list" | "inline", "prefetch") | ("order", "compare") => (1, 2),
        ("inline", "word_le" | "word_be") => (2, 2),
        ("inline", "set_word_le" | "set_word_be") => (2, 3),
        ("inline", "copy_within" | "copy_from_slice") => (1, 4),
        ("check", "xxh3_short") => (0, 1),
        ("check", "xxh3_short_inline") => (0, 2),
        ("check", "xxh3_stripe" | "xxh3_stripes") => (0, 4),
        ("crc32c" | "crc64nvme", "update") => (0, 2),
        ("crc32c" | "crc64nvme", "combine") => (0, 3),
        ("sha1", "blocks") | ("sha1" | "sha256" | "sha512", "block") => (0, 2),
        ("sha256" | "sha512", "blocks") | ("sha1" | "sha256" | "sha512", "compress") => (0, 4),
        ("argon2", "id") => (0, 7),
        ("ed25519" | "p256" | "p384", "verify") | ("p256" | "p384", "verify_der") => (0, 3),
        ("rsa", "verify_pkcs1_sha256" | "verify_pss_sha256") => (0, 4),
        ("core", "replace") => (1, 2),
        ("core", "site_word") => (0, 1),
        ("option", "unwrap") => (1, 1),
        ("result", "unwrap") => (2, 1),
        _ => return None,
    })
}

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
        let [kernel_module] = module else { return None };
        let required = builtin_arity(kernel_module, name)?;
        let declared = (types.len(), g.params.len());
        if declared != required || vals.len() != required.1 {
            self.refuse_builtin_arity(&g.path, declared, required, n);
            return Some(None);
        }
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
                "with_tail" => Some(self.list_with_capacity_at(
                    scalar(Scalar::U8),
                    vals[0]?,
                    result,
                    AllocPlace::ByteHead,
                    8,
                    n,
                )),
                "from_tail" => Some(self.bytes_from_tail(vals[0]?, vals[1]?, result, n)),
                "try_reserve_hint" => Some(self.list_try_reserve(
                    scalar(Scalar::U8),
                    vals[0]?,
                    vals[1]?,
                    Some(vals[2]?),
                    result,
                    n,
                )),
                _ => None,
            };
        }
        if name == "work_steps"
            && matches!(module, [m] if matches!(m.as_str(), "int" | "string" | "list" | "deque"))
        {
            // MEM-30's work charge for a loop written in core source: the
            // table row is named by a constant code (hirsel_core_ids
            // work_steps::code), the units by the body.
            let code = vals[0]?;
            let code = self.open.iter().rev().find_map(|inst| match *inst {
                Inst::Const { result, bits } if result == code => Some(bits),
                _ => None,
            });
            let Some(work) = code.and_then(hirsel_core_ids::work_steps::code::work) else {
                self.gap(n, "core.work_steps needs a constant table code");
                return Some(None);
            };
            self.has_work_code = true;
            self.charge_work(work, vals[1]?, n)?;
            return Some(None);
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
        if matches!(module, [m] if m == "list" || m == "inline") && name == "prefetch" {
            self.prefetch(module == ["inline"], types[0], vals[0], vals[1]?, n);
            return Some(None);
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
        let checksum = match (module, name) {
            ([m], "update") if m == "crc32c" => Some((RtOp::Crc32c, Scalar::U32)),
            ([m], "update") if m == "crc64nvme" => Some((RtOp::Crc64Nvme, Scalar::U64)),
            _ => None,
        };
        if let Some((op, width)) = checksum {
            return Some(self.crc_update(op, width, vals[0]?, vals[1]?, n));
        }
        if name == "combine" {
            let op = match module {
                [m] if m == "crc32c" => Some(RtOp::Crc32cCombine),
                [m] if m == "crc64nvme" => Some(RtOp::Crc64NvmeCombine),
                _ => None,
            };
            if let Some(op) = op {
                let args = [
                    Arg::Value(vals[0]?),
                    Arg::Value(vals[1]?),
                    Arg::Value(vals[2]?),
                ];
                return Some(Some(self.runtime_row(op, &args, n)?[0]));
            }
        }
        if module == ["sha1"] && name == "blocks" {
            return Some(self.sha1_blocks(vals[0]?, vals[1]?, n));
        }
        if let Some(op) = match module {
            [m] if m == "sha256" => Some(RtOp::Sha256Blocks),
            [m] if m == "sha1" => Some(RtOp::Sha1Compress),
            [m] if m == "sha512" => Some(RtOp::Sha512Blocks),
            _ => None,
        } {
            return match name {
                "blocks" | "compress" => {
                    Some(self.sha_blocks(op, vals[0]?, vals[1]?, vals[2]?, vals[3]?, n))
                }
                "block" => Some(self.sha_block(op, vals[0]?, vals[1]?, n)),
                _ => None,
            };
        }
        if module == ["argon2"] && name == "id" {
            return Some(self.argon2id(g.params[4].ty, vals, result, n));
        }
        let verifier = match (module, name) {
            ([m], "verify") if m == "ed25519" => Some(hirsel_core_ir::RtOp::Ed25519Verify),
            ([m], "verify") if m == "p256" => Some(hirsel_core_ir::RtOp::P256Verify),
            ([m], "verify_der") if m == "p256" => Some(hirsel_core_ir::RtOp::P256VerifyDer),
            ([m], "verify") if m == "p384" => Some(hirsel_core_ir::RtOp::P384Verify),
            ([m], "verify_der") if m == "p384" => Some(hirsel_core_ir::RtOp::P384VerifyDer),
            _ => None,
        };
        if let Some(op) = verifier {
            return Some(self.verify_signature(op, [vals[0]?, vals[1]?, vals[2]?], n));
        }
        if module == ["rsa"] {
            let op = match name {
                "verify_pkcs1_sha256" => hirsel_core_ir::RtOp::RsaVerifyPkcs1Sha256,
                "verify_pss_sha256" => hirsel_core_ir::RtOp::RsaVerifyPssSha256,
                _ => return None,
            };
            return Some(self.verify_signature(op, [vals[0]?, vals[1]?, vals[2]?, vals[3]?], n));
        }
        if module == ["list"] {
            return self.list_value(name, types[0], vals, result, n);
        }
        if module == ["columns"] {
            return self.columns_value(name, types[0], vals, result, n);
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

    fn refuse_builtin_arity(
        &mut self,
        builtin: impl std::fmt::Display,
        declared: (usize, usize),
        required: (usize, usize),
        n: NodeId,
    ) {
        let describe =
            |(types, values)| format!("{types} type arguments, {values} value arguments");
        self.refuse_compared(
            "LANG-8",
            n,
            describe(declared),
            describe(required),
            format!(
                "incompatible core sources for builtin `{builtin}`. Use the core libraries bundled with this compiler or rebuild the compiler for the selected --libraries sources"
            ),
        );
    }

    fn verify_signature<const N: usize>(
        &mut self,
        op: RtOp,
        carriers: [ValueId; N],
        n: NodeId,
    ) -> Option<ValueId> {
        use hirsel_core_ids::work_steps::table;
        let u64_ = Ty::Scalar(Scalar::U64);
        let args = carriers.map(|carrier| {
            [
                Arg::Value(self.read_sequence_word(carrier, 0, Ty::Ptr, n)),
                Arg::Value(self.read_sequence_word(carrier, 8, u64_, n)),
            ]
        });
        // The message is the carrier before the signature.
        let Arg::Value(msg_len) = args[N - 2][1] else {
            unreachable!()
        };
        let work = match op {
            RtOp::Ed25519Verify => table::ED25519_VERIFY,
            RtOp::P256Verify | RtOp::P256VerifyDer => table::P256_VERIFY,
            RtOp::P384Verify | RtOp::P384VerifyDer => table::P384_VERIFY,
            RtOp::RsaVerifyPkcs1Sha256 | RtOp::RsaVerifyPssSha256 => table::RSA_VERIFY,
            _ => unreachable!(),
        };
        self.charge_work(work, msg_len, n)?;
        let word = self.runtime_row(op, args.as_flattened(), n)?[0];
        self.runtime_nonzero(u64_, word, n)
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
        // String calls also reach this kernel without going through core_value.
        if let Some((_, required)) = builtin_arity("list", name)
            && vals.len() != required
        {
            let builtin = match name {
                "len" => "core@1/string/byte_len",
                "append" => "core@1/string/push",
                "reserve" => "core@1/string/reserve",
                "new" => "core@1/string/new",
                _ => name,
            };
            self.refuse_builtin_arity(builtin, (0, vals.len()), (0, required), n);
            return Some(None);
        }
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
                return Some(self.list_try_reserve(item, vals[0]?, vals[1]?, None, result, n));
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
            "with_capacity_aligned" => {
                let value = vals[1]?;
                let align = self.open.iter().rev().find_map(|inst| match *inst {
                    Inst::Const { result, bits } if result == value => Some(bits),
                    _ => None,
                });
                return Some(match align {
                    Some(align) if align.is_power_of_two() && align <= i64::MAX as u64 => self
                        .list_with_capacity_at(
                            item,
                            vals[0]?,
                            result,
                            AllocPlace::Current,
                            align.max(8),
                            n,
                        ),
                    _ => {
                        self.refuse("SEM-4", n, "alignment must be a constant power of two");
                        None
                    }
                });
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
