//! PACK-14/15 typed export adapters, real loader handle binding, sharing, and reflection.
use super::*;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::RtOp;

/// Metadata emitted for one PACK-14/15 module export.
pub struct PackExportMeta {
    pub adapter: FuncId,
    pub adapter_symbol: String,
    pub entry_identity: ContentHash,
    pub descriptor_symbol: Option<String>,
    pub signature: ContentHash,
    pub waits: bool,
}

impl Emitter {
    /// Lowered PACK-15 adapter CoreIR signature, PACK-14 export digest, and `waits` flag.
    pub fn pack_export_signature(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        callable: TyId,
    ) -> Option<(Signature, ContentHash, bool)> {
        let TyKind::Fn(parameters, result, waits, Callable::Plain) = t.kind(callable).clone()
        else {
            return None;
        };
        let spelling = crate::identity::closed_type_text(&p.decls, t, callable)?;
        let mut parts = Vec::with_capacity(parameters.len() + 2);
        let mut params = Vec::new();
        for (mode, ty) in parameters {
            parts.push(self.held_type_hash(p, t, ty)?);
            let rep = self.rep(p, t, ty).ok()?;
            match (mode, rep) {
                (Mode::Mut, Rep::Place(row)) => params.push(Param::Address {
                    place: scalar_abi::mut_place(&self.module.types, row),
                    loan: Loan::Mut,
                }),
                (Mode::Mut, _) => params.push(Param::Address {
                    place: self.mut_place(p, t, ty, rep)?,
                    loan: Loan::Mut,
                }),
                (_, Rep::None) => {}
                (_, Rep::Scalar(value)) => params.push(Param::Value(value)),
                (_, Rep::Place(row)) => match self.module.types[row.index()].carried {
                    Carried::Value(value) => params.push(Param::Value(value)),
                    Carried::Nothing => {}
                    Carried::Address => params.push(Param::Address {
                        place: PlaceTy::Declared(row),
                        loan: loan(mode),
                    }),
                },
            }
        }
        parts.push(self.held_type_hash(p, t, result)?);
        let dest = if self.rep(p, t, result).ok()? == Rep::None {
            None
        } else {
            Some(self.type_row(p, t, result)?)
        };
        let mut adapter = Signature::new(params, vec![], dest);
        adapter.never = result == NEVER;
        parts.push(hirsel_core_ir::normal::abi_signature(
            &self.module,
            &adapter,
        ));
        let digest = ContentHash::of_parts(
            "hirsel.pack-export.v1",
            std::iter::once(spelling.as_bytes()).chain(parts.iter().map(|h| h.0.as_slice())),
        );
        Some((adapter, digest, waits))
    }

    /// Emit a PACK-15 adapter function and PROG-9 descriptor for closed top-level
    /// function `item` (with closed generic arguments `args`, empty for non-generic
    /// items), returning metadata for `--emit module`.
    pub fn emit_pack_export(
        &mut self,
        p: &Program,
        item: usize,
        args: &[TyId],
    ) -> Option<PackExportMeta> {
        let fd = &p.fns[item];
        if fd.generics.iter().any(|g| !g.region) && args.is_empty() {
            return None;
        }
        let mut t = Local::new(&p.types);
        let ty = if args.is_empty() {
            fd.callable
        } else {
            subst(&mut t, fd.callable, args)
        };
        let (adapter_sig, signature, waits) = self.pack_export_signature(p, &mut t, ty)?;
        let (raw, _) = self.instance(p, &mut t, item, args).ok()?;
        let entry_identity = self.module.code_identity(raw);
        let key = &self.module.instances[self.module.functions[raw.index()].instance.index()];
        let identity_static = crate::identity::instance_identity(&p.decls, &t, fd, key, args)
            .and_then(|recipe| recipe.render(&[]))
            .map(|bytes| self.static_bytes(&bytes));
        let adapter = self.build_pack_export_adapter(
            p,
            &mut t,
            raw,
            item,
            args,
            ty,
            adapter_sig,
            identity_static,
        )?;
        let adapter_symbol = self.module.link_symbols()[adapter.index()].clone();
        let descriptor_symbol =
            identity_static.map(|_| self.module.descriptor_symbol(adapter, &adapter_symbol));
        Some(PackExportMeta {
            adapter,
            adapter_symbol,
            entry_identity,
            descriptor_symbol,
            signature,
            waits,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_pack_export_adapter(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        raw: FuncId,
        item: usize,
        args: &[TyId],
        ty: TyId,
        adapter_sig: Signature,
        descriptor_identity: Option<hirsel_core_ir::StaticId>,
    ) -> Option<FuncId> {
        let mut key =
            self.module.instances[self.module.functions[raw.index()].instance.index()].clone();
        key.definition.0.name.push_str("$pack_export");
        if let Some(&existing) = self.funcs.get(&key) {
            return Some(existing);
        }
        let TyKind::Fn(source_params, result, _, Callable::Plain) = t.kind(ty).clone() else {
            return None;
        };
        let sig_id = self.sig(adapter_sig.clone());
        let row = self.add_function(key, adapter_sig.clone());
        self.module.functions[row.index()].pack_export = true;
        self.install_source_signature(row, sig_id);

        let raw_sig_id = self.module.functions[raw.index()].signature;
        let raw_sig = self.module.signatures[raw_sig_id.index()].clone();

        let mut body = Body::default();
        let entry_tys: Vec<Ty> = adapter_sig
            .dest
            .map(|_| Ty::Ptr)
            .into_iter()
            .chain(adapter_sig.params.iter().map(|p| match p {
                Param::Value(v) => *v,
                Param::Address { .. } => Ty::Ptr,
            }))
            .collect();
        let mut entry_params = Vec::with_capacity(entry_tys.len());
        for vty in entry_tys {
            let v = ValueId(body.values.len() as u32);
            body.values.push(vty);
            entry_params.push(v);
        }
        let params_range = PoolRange::push(&mut body.pools.values, entry_params.iter().copied());
        body.blocks.push(Block {
            params: params_range,
            insts: PoolRange::EMPTY,
            anchor: None,
            decision: false,
        });
        body.terminators.push(Terminator::Unreachable);

        let push_inst = |body: &mut Body, inst: Inst| {
            body.insts.push(inst);
            body.inst_anchors.push(None);
        };
        let make_addr = |body: &mut Body, base: AddressBase, ty: PlaceTy| -> AddressId {
            let address = AddressId(body.addresses.len() as u32);
            body.addresses.push(AddressInfo {
                base,
                steps: PoolRange::EMPTY,
                ty,
            });
            push_inst(
                body,
                Inst::Address {
                    address,
                    fail: None,
                },
            );
            address
        };

        let mut call_args = Vec::with_capacity(raw_sig.params.len() + 1);
        // Forward caller result_place directly whenever `raw` constructs into `dest`.
        if let Some(raw_dest) = raw_sig.dest {
            let dest_addr = make_addr(
                &mut body,
                AddressBase::Value(entry_params[0]),
                PlaceTy::Declared(raw_dest),
            );
            call_args.push(Arg::Address(dest_addr));
        }

        // Map each source parameter from the sealed PACK-15 entry params into `raw`'s signature.
        let mut entry_cursor = usize::from(adapter_sig.dest.is_some());
        let mut adapter_param_idx = 0usize;
        let mut raw_param_idx = 0usize;
        for (mode, pty) in source_params {
            let rep = self.rep(p, t, pty).ok()?;
            let in_adapter = match (mode, rep) {
                (Mode::Mut, _) => true,
                (_, Rep::None) => false,
                (_, Rep::Scalar(_)) => true,
                (_, Rep::Place(r)) => {
                    !matches!(self.module.types[r.index()].carried, Carried::Nothing)
                }
            };
            let in_raw = raw_param_idx < raw_sig.params.len()
                && match (mode, rep) {
                    (Mode::Mut, _) => true,
                    (_, Rep::None) => false,
                    (_, Rep::Scalar(_)) => true,
                    (_, Rep::Place(r)) => {
                        // Before `seal()`, `raw` still carries `Carried::Nothing` as an address param;
                        // after `seal()`, `Carried::Nothing` has been removed from `raw_sig`.
                        if matches!(self.module.types[r.index()].carried, Carried::Nothing) {
                            matches!(
                                raw_sig.params[raw_param_idx],
                                Param::Address {
                                    place: PlaceTy::Declared(d),
                                    ..
                                } if d == r
                            )
                        } else {
                            true
                        }
                    }
                };
            let adapter_val = if in_adapter {
                let v = entry_params[entry_cursor];
                entry_cursor += 1;
                adapter_param_idx += 1;
                Some(v)
            } else {
                None
            };
            if !in_raw {
                continue;
            }
            let raw_param = raw_sig.params[raw_param_idx];
            raw_param_idx += 1;
            match (adapter_val, raw_param) {
                (Some(v), Param::Value(_)) => {
                    call_args.push(Arg::Value(v));
                }
                (Some(v), Param::Address { place, .. }) => {
                    let adapter_param = adapter_sig.params[adapter_param_idx - 1];
                    match adapter_param {
                        Param::Address { .. } => {
                            let addr = make_addr(&mut body, AddressBase::Value(v), place);
                            call_args.push(Arg::Address(addr));
                        }
                        Param::Value(vty) => {
                            // `raw` is pre-seal and expects an address to a scalar-carried row.
                            // Spill into a local slot; `scalar_abi::seal` loads and promotes it away.
                            let slot = SlotId(body.slots.len() as u32);
                            body.slots.push(place);
                            let slot_addr = make_addr(&mut body, AddressBase::Slot(slot), place);
                            let val_addr = make_addr(
                                &mut body,
                                AddressBase::Address(slot_addr),
                                PlaceTy::Value(vty),
                            );
                            push_inst(
                                &mut body,
                                Inst::Store {
                                    address: val_addr,
                                    value: v,
                                },
                            );
                            call_args.push(Arg::Address(slot_addr));
                        }
                    }
                }
                (None, Param::Address { place, .. }) => {
                    let slot = SlotId(body.slots.len() as u32);
                    body.slots.push(place);
                    let slot_addr = make_addr(&mut body, AddressBase::Slot(slot), place);
                    call_args.push(Arg::Address(slot_addr));
                }
                (None, Param::Value(_)) => return None,
            }
        }
        debug_assert_eq!(raw_param_idx, raw_sig.params.len());
        let _ = (item, args);

        let call_args_range = PoolRange::push(&mut body.pools.args, call_args.into_iter());
        let call_results_vec: Vec<ValueId> = raw_sig
            .results
            .iter()
            .map(|&rty| {
                let v = ValueId(body.values.len() as u32);
                body.values.push(rty);
                v
            })
            .collect();
        let call_results_range =
            PoolRange::push(&mut body.pools.values, call_results_vec.iter().copied());
        let call_id = CallId(body.calls.len() as u32);
        body.calls.push(CallSite {
            callee: Callee::Direct(raw),
            signature: raw_sig_id,
            args: call_args_range,
            results: call_results_range,
            fail: None,
            cold: false,
        });
        if let Some(identity) = descriptor_identity {
            let desc = ValueId(body.values.len() as u32);
            body.values.push(Ty::Ptr);
            push_inst(
                &mut body,
                Inst::DescriptorRef {
                    result: desc,
                    function: row,
                    identity,
                    spawn: None,
                },
            );
        }
        push_inst(&mut body, Inst::Call { call: call_id });

        if result == NEVER {
            body.terminators[0] = Terminator::Unreachable;
        } else {
            // When `raw` returns a scalar in registers, write it directly into `result_place`.
            if raw_sig.dest.is_none()
                && let (&[rty], &[rval]) = (raw_sig.results.as_slice(), call_results_vec.as_slice())
                && adapter_sig.dest.is_some()
            {
                let dest_addr = make_addr(
                    &mut body,
                    AddressBase::Value(entry_params[0]),
                    PlaceTy::Value(rty),
                );
                push_inst(
                    &mut body,
                    Inst::Store {
                        address: dest_addr,
                        value: rval,
                    },
                );
            }
            body.terminators[0] = Terminator::Return(PoolRange::EMPTY);
        }
        body.blocks[0].insts = PoolRange::new(0, body.insts.len() as u32);
        self.module.functions[row.index()].body = Some(body);
        Some(row)
    }
}

impl Cx<'_, '_, '_> {
    /// Emit `code.bind(f)`: produce a `Code[F]` handle backed by the PACK-15 adapter.
    pub(super) fn lower_code_bind(
        &mut self,
        item: usize,
        args: &[TyId],
        ty: TyId,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let Some((adapter_sig, _hash, _waits)) =
            self.e.pack_export_signature(self.p, &mut self.t, ty)
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
        let (raw, _) = match self.instance_row(item, args) {
            Ok(row) => row,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        let Some(recipe) = self.code_identity_recipe(item, raw, args) else {
            self.gap(n, "Code identity source is not built");
            return None;
        };
        let Some(adapter) = self.e.build_pack_export_adapter(
            self.p,
            &mut self.t,
            raw,
            item,
            args,
            ty,
            adapter_sig,
            None,
        ) else {
            self.gap(n, "PACK-15 typed export adapter is not built");
            return None;
        };
        let bytes = recipe.render(&[]).expect("closed descriptor recipe");
        let identity = self.e.static_bytes(&bytes);
        let descriptor = self.value(Ty::Ptr);
        self.inst(
            Inst::DescriptorRef {
                result: descriptor,
                function: adapter,
                identity,
                spawn: None,
            },
            n,
        );
        let [handle, ..] = self.runtime_row(RtOp::CodeBind, &[Arg::Value(descriptor)], n)?;
        self.runtime_carrier(result, handle, n)
    }

    /// Emit `code.share(c)`: retain one more counted `Code[F]` handle.
    pub(super) fn lower_code_share(
        &mut self,
        callable: TyId,
        code: Option<ValueId>,
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
        let handle = self.runtime_handle(code?, n);
        let [shared, ..] = self.runtime_row(RtOp::CodeShare, &[Arg::Value(handle)], n)?;
        self.runtime_carrier(result, shared, n)
    }

    /// Emit `code.slot[F](loader)`: allocate an empty version-0 `CodeSlot[F]`.
    pub(super) fn lower_code_slot(
        &mut self,
        callable: TyId,
        loader: Option<ValueId>,
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
        let [slot, ..] = self.runtime_row(RtOp::CodeSlotNew, &[Arg::Value(loader?)], n)?;
        self.runtime_carrier(result, slot, n)
    }

    /// Emit `code.identity(c)` for `Code[F]`.
    pub(super) fn lower_code_identity(
        &mut self,
        code: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let handle = self.runtime_handle(code?, n);
        let res_id = self.runtime_type(result, n)?;
        let to = self.result_slot(PlaceTy::Declared(res_id), n);
        let out_ptr = self.pointer(to, n);
        self.runtime_row(
            RtOp::CodeIdentity,
            &[Arg::Value(handle), Arg::Value(out_ptr)],
            n,
        )?;
        Some(out_ptr)
    }

    /// Emit `code.revision(c)` for `Code[F]`.
    pub(super) fn lower_code_revision(
        &mut self,
        code: Option<ValueId>,
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let TyKind::Adt(_, arguments) = self.t.kind(result).clone() else {
            self.gap(n, "code.revision result is not an Option");
            return None;
        };
        let payload = arguments[0];
        let handle = self.runtime_handle(code?, n);
        let res_id = self.runtime_type(result, n)?;
        let to = self.result_slot(PlaceTy::Declared(res_id), n);
        let payload_addr =
            self.runtime_field(to, &[Step::CaseField { case: 1, field: 0 }], payload, n)?;
        let payload_ptr = self.pointer(payload_addr, n);
        let [present, ..] = self.runtime_row(
            RtOp::CodeRevision,
            &[Arg::Value(handle), Arg::Value(payload_ptr)],
            n,
        )?;
        let is_some = self.runtime_compare(Ty::Scalar(Scalar::U8), present, 0, n)?;
        let none_block = self.new_block(&[]);
        let some_block = self.new_block(&[]);
        let join = self.new_block(&[]);
        self.runtime_branch(is_some, none_block, some_block, n);

        self.start(some_block);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 1,
            },
            n,
        );
        self.jump(join, &[], n);

        self.start(none_block);
        self.inst(
            Inst::SetCase {
                address: to,
                case: 0,
            },
            n,
        );
        self.jump(join, &[], n);

        self.start(join);
        Some(self.pointer(to, n))
    }
}
