//! PACK-14/15 activate, load, direct call, and dispatch call lowering.
use super::calls::CallScratch;
use super::*;
use hirsel_core_ir::RtOp;

impl Cx<'_, '_, '_> {
    /// Emit `code.activate(mut s, expected, c) -> Result[u64, ActivateError]`.
    pub(super) fn lower_code_activate(
        &mut self,
        callable: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if self
            .e
            .pack_export_hash(self.p, &mut self.t, callable)
            .is_none()
        {
            self.refuse(
                "PACK-14",
                n,
                "the operation requires a closed type with a compiled witness",
            );
            return None;
        }
        if !self.live() {
            return None;
        }
        let (_, _, Some(err_ty)) = self.result_parts(result)? else {
            self.gap(n, "code.activate result type is unavailable");
            return None;
        };
        let res_id = self.runtime_type(result, n)?;
        let slot_handle = self.runtime_handle(vals[0]?, n);
        let expected = vals[1]?;
        let code_handle = self.runtime_handle(vals[2]?, n);
        let out_ver_slot = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::U64)), n);
        let out_ver_ptr = self.pointer(out_ver_slot, n);
        let [tag, ..] = self.runtime_row(
            RtOp::CodeActivate,
            &[
                Arg::Value(slot_handle),
                Arg::Value(expected),
                Arg::Value(code_handle),
                Arg::Value(out_ver_ptr),
            ],
            n,
        )?;
        let to = self.result_slot(PlaceTy::Declared(res_id), n);
        let ok_block = self.new_block(&[]);
        let wrong_pool_block = self.new_block(&[]);
        let stale_block = self.new_block(&[]);
        let cap_block = self.new_block(&[]);
        let join = self.new_block(&[]);
        let ok_edge = self.edge(ok_block, &[]);
        let wrong_pool_edge = self.edge(wrong_pool_block, &[]);
        let stale_edge = self.edge(stale_block, &[]);
        let arms = PoolRange::push(
            &mut self.b.pools.arms,
            [
                hirsel_core_ir::SwitchArm {
                    value: 0,
                    edge: ok_edge,
                },
                hirsel_core_ir::SwitchArm {
                    value: 1,
                    edge: wrong_pool_edge,
                },
                hirsel_core_ir::SwitchArm {
                    value: 2,
                    edge: stale_edge,
                },
            ],
        );
        let otherwise = self.edge(cap_block, &[]);
        self.end(
            Terminator::Switch {
                value: tag,
                arms,
                otherwise,
            },
            Some(n),
        );

        // Ok(ver)
        self.start(ok_block);
        let ver = self.read_bits(out_ver_slot, Ty::Scalar(Scalar::U64), n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.runtime_store(
            to,
            &[Step::CaseField { case: 0, field: 0 }],
            Ty::Scalar(Scalar::U64),
            ver,
            n,
        );
        self.jump(join, &[], n);

        // Error(WrongPool) = case 0 of ActivateError
        self.start(wrong_pool_block);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        if let Some(err) =
            self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], err_ty, n)
        {
            self.inst(
                Inst::SetCase {
                    address: err,
                    case: 0,
                },
                n,
            );
        }
        self.jump(join, &[], n);

        // Error(Stale(cur)) = case 1 of ActivateError
        self.start(stale_block);
        let cur = self.read_bits(out_ver_slot, Ty::Scalar(Scalar::U64), n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        if let Some(err) =
            self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], err_ty, n)
        {
            self.inst(
                Inst::SetCase {
                    address: err,
                    case: 1,
                },
                n,
            );
            self.runtime_store(
                err,
                &[Step::CaseField { case: 1, field: 0 }],
                Ty::Scalar(Scalar::U64),
                cur,
                n,
            );
        }
        self.jump(join, &[], n);

        // Error(Capacity) = case 2 of ActivateError
        self.start(cap_block);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        if let Some(err) =
            self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], err_ty, n)
        {
            self.inst(
                Inst::SetCase {
                    address: err,
                    case: 2,
                },
                n,
            );
        }
        self.jump(join, &[], n);

        self.start(join);
        let p = self.pointer(to, n);
        self.temp(result, Some(p));
        Some(p)
    }

    /// Emit `code.load[F](loader, key, entry_id) -> Result[Code[F], LoadError]`.
    pub(super) fn lower_code_load(
        &mut self,
        callable: TyId,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let Some((_adapter_sig, sig_hash, waits)) =
            self.e.pack_export_signature(self.p, &mut self.t, callable)
        else {
            self.refuse(
                "PACK-14",
                n,
                "the operation requires a closed type with a compiled witness",
            );
            return None;
        };
        if !self.live() {
            return None;
        }
        let (_, ok_ty, Some(err_ty)) = self.result_parts(result)? else {
            self.gap(n, "code.load result type is unavailable");
            return None;
        };
        let res_id = self.runtime_type(result, n)?;
        let bytes32_ty = self.t.mk(TyKind::Array(scalar(Scalar::U8), 32));
        let bytes32_id = self.runtime_type(bytes32_ty, n)?;
        let sig_static = self.e.static_bytes(&sig_hash.0);
        let sig_ptr = self.value(Ty::Ptr);
        self.inst(
            Inst::ReadStatic {
                result: sig_ptr,
                data: sig_static,
            },
            n,
        );
        let waits_byte = self.konst(Ty::Scalar(Scalar::U8), u64::from(waits), n)?;
        let out_code_slot = self.new_slot(PlaceTy::Value(Ty::Ptr), n);
        let out_code_ptr = self.pointer(out_code_slot, n);
        let out_word_slot = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::U64)), n);
        let out_word_ptr = self.pointer(out_word_slot, n);
        let out_bytes_slot = self.new_slot(PlaceTy::Declared(bytes32_id), n);
        let out_bytes_ptr = self.pointer(out_bytes_slot, n);

        let [tag, ..] = self.runtime_row(
            RtOp::CodeLoad,
            &[
                Arg::Value(vals[0]?),
                Arg::Value(vals[1]?),
                Arg::Value(vals[2]?),
                Arg::Value(sig_ptr),
                Arg::Value(waits_byte),
                Arg::Value(out_code_ptr),
                Arg::Value(out_word_ptr),
                Arg::Value(out_bytes_ptr),
            ],
            n,
        )?;
        let to = self.result_slot(PlaceTy::Declared(res_id), n);
        let join = self.new_block(&[]);
        let ok_block = self.new_block(&[]);
        // 11 LoadError cases (tags 1..=11 -> variant cases 0..=10).
        let err_blocks: Vec<BlockId> = (0..11).map(|_| self.new_block(&[])).collect();
        let mut arm_list = Vec::with_capacity(11);
        arm_list.push(hirsel_core_ir::SwitchArm {
            value: 0,
            edge: self.edge(ok_block, &[]),
        });
        for (case_idx, &blk) in err_blocks.iter().enumerate().take(10) {
            arm_list.push(hirsel_core_ir::SwitchArm {
                value: (case_idx + 1) as u64,
                edge: self.edge(blk, &[]),
            });
        }
        let arms = PoolRange::push(&mut self.b.pools.arms, arm_list);
        let otherwise = self.edge(err_blocks[10], &[]);
        self.end(
            Terminator::Switch {
                value: tag,
                arms,
                otherwise,
            },
            Some(n),
        );

        // Ok(Code[F])
        self.start(ok_block);
        let handle = self.read_bits(out_code_slot, Ty::Ptr, n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        if let Some(ok_field) =
            self.runtime_field(to, &[Step::CaseField { case: 0, field: 0 }], ok_ty, n)
        {
            self.runtime_store(ok_field, &[Step::Field(0)], Ty::Ptr, handle, n);
        }
        self.jump(join, &[], n);

        // LoadError cases 0..=10:
        // 0: NotAdmitted, 1: Malformed(String), 2: Mismatch(String), 3: Import(u32),
        // 4: Unbound(Inline[u8, 32]), 5: Fixup(u32), 6: NoEntry, 7: Capacity,
        // 8: Quarantined, 9: NotPrelinked, 10: Unsupported
        for (case_idx, blk) in err_blocks.into_iter().enumerate() {
            self.start(blk);
            self.inst(
                Inst::SetCase {
                    address: to,
                    case: 1,
                },
                n,
            );
            if let Some(err) =
                self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], err_ty, n)
            {
                self.inst(
                    Inst::SetCase {
                        address: err,
                        case: case_idx as u32,
                    },
                    n,
                );
                match case_idx {
                    1 | 2 => {
                        let str_ty = self
                            .t
                            .mk(TyKind::Adt(self.p.core_decl("String").unwrap(), vec![]));
                        let str_id = self.runtime_type(str_ty, n)?;
                        let s_addr = self.address(
                            AddressBase::Address(err),
                            &[Step::CaseField {
                                case: case_idx as u32,
                                field: 0,
                            }],
                            PlaceTy::Declared(str_id),
                            n,
                        );
                        let len = self.konst(Ty::Scalar(Scalar::U64), 24, n)?;
                        self.inst(
                            Inst::Copy {
                                to: s_addr,
                                from: out_bytes_slot,
                                bytes: len,
                            },
                            n,
                        );
                    }
                    3 | 5 => {
                        let word = self.read_bits(out_word_slot, Ty::Scalar(Scalar::U64), n);
                        let narrowed = self.value(Ty::Scalar(Scalar::U32));
                        self.inst(
                            Inst::Convert {
                                result: narrowed,
                                flag: None,
                                operand: word,
                                mode: ConvertMode::Truncate,
                            },
                            n,
                        );
                        self.runtime_store(
                            err,
                            &[Step::CaseField {
                                case: case_idx as u32,
                                field: 0,
                            }],
                            Ty::Scalar(Scalar::U32),
                            narrowed,
                            n,
                        );
                    }
                    4 => {
                        let dst = self.address(
                            AddressBase::Address(err),
                            &[Step::CaseField { case: 4, field: 0 }],
                            PlaceTy::Declared(bytes32_id),
                            n,
                        );
                        let len = self.konst(Ty::Scalar(Scalar::U64), 32, n)?;
                        self.inst(
                            Inst::Copy {
                                to: dst,
                                from: out_bytes_slot,
                                bytes: len,
                            },
                            n,
                        );
                    }
                    _ => {}
                }
            }
            self.jump(join, &[], n);
        }

        self.start(join);
        let p = self.pointer(to, n);
        self.temp(result, Some(p));
        Some(p)
    }

    /// Lower a direct `c(args...)` or `code.dispatch(slot, args...)` call through
    /// the PACK-15 `Callee::PackExport` boundary.
    pub(super) fn lower_pack_call(
        &mut self,
        callee_val: Option<ValueId>,
        entry: TyId,
        dispatch: bool,
        result: TyId,
        n: NodeId,
        scratch: &mut CallScratch,
    ) -> (TyId, Option<ValueId>) {
        let out_ty = if dispatch {
            let Some(Def::Type(error)) = self.p.def(&DefPath::core("code.DispatchError")) else {
                return self.gap_ty(n, "code dispatch error type is unavailable");
            };
            let error = self.t.mk(TyKind::Adt(error, vec![]));
            self.t.mk(TyKind::Adt(
                self.p.core_decl("Result").unwrap(),
                vec![result, error],
            ))
        } else {
            result
        };
        let Some((adapter_sig, _hash, _waits)) =
            self.e.pack_export_signature(self.p, &mut self.t, entry)
        else {
            self.refuse(
                "PACK-14",
                n,
                "the operation requires a closed type with a compiled witness",
            );
            return (out_ty, None);
        };
        if !self.live() {
            if out_ty == NEVER {
                self.dead = true;
            }
            return (out_ty, None);
        }
        let Some(callee_val) = callee_val else {
            return (out_ty, None);
        };
        let sig_id = self.e.sig(adapter_sig.clone());
        let ret_rep = match self.rep(result) {
            Ok(rep) => rep,
            Err(what) => {
                self.gap(n, what);
                return (out_ty, None);
            }
        };

        if !dispatch {
            // Direct `c(args...)`: charge the call-site step BEFORE consuming taken args,
            // matching linked calls (PACK-15).
            self.source_step(n);
            for (i, &(mode, _)) in scratch.params.iter().enumerate() {
                if mode == Mode::Take {
                    self.consume(scratch.vals[i], n);
                }
            }
            // Entry pointer is word 0 at `(handle & !1)`.
            let handle_addr = self.address(
                AddressBase::Value(callee_val),
                &[],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            let raw_bits = self.read_bits(handle_addr, Ty::Scalar(Scalar::U64), n);
            let Some(mask) = self.konst(Ty::Scalar(Scalar::U64), !1u64, n) else {
                return (out_ty, None);
            };
            let aligned_bits = self.value(Ty::Scalar(Scalar::U64));
            self.inst(
                Inst::Binary {
                    result: aligned_bits,
                    op: BinaryOp::BitAnd,
                    left: raw_bits,
                    right: mask,
                    fail: None,
                },
                n,
            );
            let Some(aligned_ptr) =
                self.reinterpret(aligned_bits, Ty::Scalar(Scalar::U64), Ty::Ptr, n)
            else {
                return (out_ty, None);
            };
            let entry_addr = self.address(
                AddressBase::Value(aligned_ptr),
                &[],
                PlaceTy::Value(Ty::Ptr),
                n,
            );
            let entry_ptr = self.read_bits(entry_addr, Ty::Ptr, n);

            let dest = adapter_sig
                .dest
                .map(|id| self.result_slot(PlaceTy::Declared(id), n));
            let Some(call_args) = self.pack_call_args(&adapter_sig, dest, scratch, n) else {
                return (out_ty, None);
            };
            let fail = self.fail_to(n);
            let call = CallId(self.b.calls.len() as u32);
            self.b.calls.push(CallSite {
                callee: Callee::PackExport(entry_ptr),
                signature: sig_id,
                args: call_args,
                results: PoolRange::EMPTY,
                fail,
                cold: false,
            });
            self.inst(Inst::Call { call }, n);
            if result == NEVER {
                self.dead = true;
                self.end(Terminator::Unreachable, Some(n));
                return (out_ty, None);
            }
            let value = match (dest, ret_rep) {
                (_, Rep::None) => None,
                (Some(d), Rep::Place(_)) => Some(self.pointer(d, n)),
                (Some(d), Rep::Scalar(t)) => {
                    let at = self.address(AddressBase::Address(d), &[], PlaceTy::Value(t), n);
                    Some(self.read_bits(at, t, n))
                }
                (None, _) => None,
            };
            self.temp(result, value);
            return (out_ty, value);
        }

        // Snapshot taken argument places/types for `Empty` and pre-call step failure cleanup.
        let mut taken_for_empty: Vec<(TyId, Option<ValueId>)> = Vec::new();
        for (i, &(mode, pt)) in scratch.params.iter().enumerate() {
            if mode == Mode::Take {
                self.consume(scratch.vals[i], n);
                taken_for_empty.push((pt, scratch.vals[i]));
            }
        }

        // `code.dispatch(slot, args...)`: borrow worker-cached generation, construct
        // directly into Result::Ok payload, and release borrow on both normal and failure paths.
        let Some(out_id) = self.runtime_type(out_ty, n) else {
            return (out_ty, None);
        };
        let to = self.result_slot(PlaceTy::Declared(out_id), n);
        let slot_handle = self.runtime_handle(callee_val, n);
        let token_slot = self.new_slot(PlaceTy::Value(Ty::Scalar(Scalar::U64)), n);
        let token_ptr = self.pointer(token_slot, n);
        let Some([entry_ptr, ..]) = self.runtime_row(
            RtOp::CodeDispatchBegin,
            &[Arg::Value(slot_handle), Arg::Value(token_ptr)],
            n,
        ) else {
            return (out_ty, None);
        };
        let Some(is_empty) = self.runtime_compare(Ty::Ptr, entry_ptr, 0, n) else {
            return (out_ty, None);
        };
        let empty_block = self.new_block(&[]);
        let call_block = self.new_block(&[]);
        let step_fail_block = self.steps_in_source.then(|| self.new_block(&[]));
        let fail_end_block = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(is_empty, empty_block, call_block, n);

        // Empty slot: drop any taken arguments (0 steps for bookkeeping), return Error(Empty).
        self.start(empty_block);
        let outer_fail = self.fail_to(n);
        self.drop_dispatch_arguments(&taken_for_empty, n);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        self.jump(join, &[], n);

        // Populated slot: charge the call-site step (releasing borrow & taken args on failure),
        // invoke PACK-15 adapter, then release borrow on both return and fail.
        self.start(call_block);
        if let Some(step_fail) = step_fail_block {
            self.inst(
                Inst::Step {
                    fail: Some(step_fail),
                },
                n,
            );
        }
        let token = self.read_bits(token_slot, Ty::Scalar(Scalar::U64), n);
        let (call_dest, scalar_tmp) = match (adapter_sig.dest, ret_rep) {
            (Some(dest_row), Rep::Place(_)) => {
                // Direct construction right into Result::Ok payload (zero copy!).
                let ok_payload = self.address(
                    AddressBase::Address(to),
                    &[Step::CaseField { case: 0, field: 0 }],
                    PlaceTy::Declared(dest_row),
                    n,
                );
                (Some(ok_payload), None)
            }
            (Some(dest_row), Rep::Scalar(t)) => {
                let tmp = self.new_slot(PlaceTy::Declared(dest_row), n);
                (Some(tmp), Some((tmp, t)))
            }
            _ => (None, None),
        };
        let Some(call_args) = self.pack_call_args(&adapter_sig, call_dest, scratch, n) else {
            return (out_ty, None);
        };
        let call = CallId(self.b.calls.len() as u32);
        self.b.calls.push(CallSite {
            callee: Callee::PackExport(entry_ptr),
            signature: sig_id,
            args: call_args,
            results: PoolRange::EMPTY,
            fail: Some(fail_end_block),
            cold: false,
        });
        self.inst(Inst::Call { call }, n);
        let _ = self.runtime_row(RtOp::CodeDispatchEnd, &[Arg::Value(token)], n);
        if result == NEVER {
            self.end(Terminator::Unreachable, Some(n));
        } else {
            if let Some((tmp, t)) = scalar_tmp {
                let at = self.address(AddressBase::Address(tmp), &[], PlaceTy::Value(t), n);
                let v = self.read_bits(at, t, n);
                self.runtime_store(to, &[Step::CaseField { case: 0, field: 0 }], t, v, n);
            }
            self.inst(
                Inst::SetCase {
                    address: to,
                    case: 0,
                },
                n,
            );
            self.jump(join, &[], n);
        }

        // Pre-call step failure: release borrow and drop taken args before unwinding.
        if let Some(step_fail) = step_fail_block {
            self.start(step_fail);
            let fail_token = self.read_bits(token_slot, Ty::Scalar(Scalar::U64), n);
            let _ = self.runtime_row(RtOp::CodeDispatchEnd, &[Arg::Value(fail_token)], n);
            let was_failing = std::mem::replace(&mut self.failing, true);
            self.drop_dispatch_arguments(&taken_for_empty, n);
            self.failing = was_failing;
            if let Some(target) = outer_fail {
                let edge = self.edge(target, &[]);
                self.end(Terminator::Jump(edge), Some(n));
            } else {
                self.end(Terminator::Propagate, Some(n));
            }
        }

        // Callee failure edge: always release worker borrow / continuation pin before unwinding!
        self.start(fail_end_block);
        let fail_token = self.read_bits(token_slot, Ty::Scalar(Scalar::U64), n);
        let _ = self.runtime_row(RtOp::CodeDispatchEnd, &[Arg::Value(fail_token)], n);
        if let Some(target) = outer_fail {
            let edge = self.edge(target, &[]);
            self.end(Terminator::Jump(edge), Some(n));
        } else {
            self.end(Terminator::Propagate, Some(n));
        }

        self.start(join);
        let p = self.pointer(to, n);
        self.temp(out_ty, Some(p));
        (out_ty, Some(p))
    }

    /// On a branch where the callee did not receive its arguments, the caller
    /// owns them as temporaries again. Ordinary scope cleanup handles FAIL-12.
    fn drop_dispatch_arguments(&mut self, taken: &[(TyId, Option<ValueId>)], n: NodeId) {
        let mark = self.owners.len();
        for &(ty, value) in taken {
            self.temp(ty, value);
        }
        self.end_owners(mark, None, false, n);
    }

    fn pack_call_args(
        &mut self,
        adapter_sig: &Signature,
        dest: Option<AddressId>,
        scratch: &mut CallScratch,
        n: NodeId,
    ) -> Option<PoolRange<Arg>> {
        scratch.ir_args.clear();
        scratch.ir_args.extend(dest.map(Arg::Address));
        let mut param_idx = 0usize;
        for (i, &(mode, pt)) in scratch.params.iter().enumerate() {
            let rep = match self.rep(pt) {
                Ok(rep) => rep,
                Err(what) => {
                    self.gap(n, what);
                    return None;
                }
            };
            let carried_in_sig = match (mode, rep) {
                (Mode::Mut, _) => true,
                (_, Rep::None) => false,
                (_, Rep::Scalar(_)) => true,
                (_, Rep::Place(row)) => {
                    !matches!(self.e.module.types[row.index()].carried, Carried::Nothing)
                }
            };
            if !carried_in_sig {
                continue;
            }
            let param = adapter_sig.params[param_idx];
            param_idx += 1;
            let val = scratch.vals[i]?;
            match param {
                Param::Value(t) => {
                    if matches!(rep, Rep::Place(_)) {
                        let at = self.address(AddressBase::Value(val), &[], PlaceTy::Value(t), n);
                        scratch.ir_args.push(Arg::Value(self.read_bits(at, t, n)));
                    } else {
                        scratch.ir_args.push(Arg::Value(val));
                    }
                }
                Param::Address { place, .. } => {
                    let pointer = if mode == Mode::Mut || matches!(rep, Rep::Place(_)) {
                        val
                    } else {
                        let at = self.new_slot(rep.stored().unwrap_or(place), n);
                        self.install(at, rep, Some(val), n);
                        self.pointer(at, n)
                    };
                    let at = self.address(AddressBase::Value(pointer), &[], place, n);
                    scratch.ir_args.push(Arg::Address(at));
                }
            }
        }
        Some(PoolRange::push(
            &mut self.b.pools.args,
            scratch.ir_args.iter().copied(),
        ))
    }
}
