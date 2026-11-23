//! Calls to the landed core memory C ABI. Native layout is not a logical charge.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{
    InstructionData, MemFlagsData, Opcode, Value, ValueDef, condcodes::IntCC,
};

impl Function<'_, '_, '_> {
    fn small_copy(&mut self, to: Value, from: Value, bytes: Value) -> bool {
        let ValueDef::Result(inst, _) = self.b.func.dfg.value_def(bytes) else {
            return false;
        };
        let InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } = self.b.func.dfg.insts[inst]
        else {
            return false;
        };
        let size = imm.bits();
        if !(0..=128).contains(&size) {
            return false;
        }
        // Unaligned, exact-width accesses. Keep all source bytes in registers
        // before storing, including odd tails and the 24-byte sequence carrier.
        let mut loaded = [None; 11];
        let mut offset = 0;
        for chunk in &mut loaded {
            let remaining = size - i64::from(offset);
            if remaining == 0 {
                break;
            }
            let (width, ty) = if size > 32 && remaining >= 16 {
                (16, types::I8X16)
            } else if remaining >= 8 {
                (8, types::I64)
            } else if remaining >= 4 {
                (4, types::I32)
            } else if remaining >= 2 {
                (2, types::I16)
            } else {
                (1, types::I8)
            };
            let value = self.b.ins().load(ty, MemFlagsData::new(), from, offset);
            *chunk = Some((value, offset));
            offset += width;
        }
        for (value, offset) in loaded.into_iter().flatten() {
            self.b.ins().store(MemFlagsData::new(), value, to, offset);
        }
        true
    }

    pub fn c_call(
        &mut self,
        name: &'static str,
        params: &[Ty],
        results: &[Ty],
        args: &[Value],
    ) -> Result<calls::NativeValues> {
        self.calls
            .c(self.object, &mut self.b, name, params, results, args)
    }
    fn step(&mut self, fail: Option<BlockId>) -> Result<()> {
        // Keep the counter in its existing Context word. A function-long SSA
        // window links every slow-path join; coalescing that chain retains
        // quadratic range-list storage in the backtracking allocator's arena.
        let before = self.b.ins().load(
            types::I64,
            MemFlagsData::trusted(),
            self.context,
            memory_layout::CONTEXT_STEPS as i32,
        );
        let after = self.b.ins().iadd_imm_s(before, -1);
        self.b.ins().store(
            MemFlagsData::trusted(),
            after,
            self.context,
            memory_layout::CONTEXT_STEPS as i32,
        );
        let exhausted = self
            .b
            .ins()
            .icmp_imm_s(IntCC::SignedLessThanOrEqual, after, 0);
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let ready = self.b.create_block();
        self.b.ins().brif(exhausted, slow, &[], ready, &[]);
        self.b.switch_to_block(slow);
        let site = self.site()?;
        let status = self.c_call(
            "hirsel_core_step_out",
            &[Ty::Ptr, Ty::Scalar(Scalar::U64)],
            &[Ty::Scalar(Scalar::U8)],
            &[self.context, site],
        )?[0];
        self.call_failure(status, fail)?;
        self.b.ins().jump(ready, &[]);
        self.b.switch_to_block(ready);
        Ok(())
    }
    pub fn memory(&mut self, inst: &Inst) -> Result<()> {
        let p = Ty::Ptr;
        let u = Ty::Scalar(Scalar::U64);
        let boolean = Ty::Scalar(Scalar::Bool);
        match *inst {
            Inst::Alloc {
                result,
                available,
                region_entry,
                ref kind,
                place,
                charge,
                overflow,
                size,
                align,
            } => {
                let ctx = self.context;
                let native_kind = match kind.cleanup(&self.input.types, self.layouts) {
                    Some(
                        hirsel_core_layout::Cleanup::Terminal | hirsel_core_layout::Cleanup::Fixed,
                    ) => memory_layout::NATIVE_PLAIN,
                    _ => memory_layout::NATIVE_GRAPH,
                };
                let resource = match *kind {
                    AllocKind::Resource { grant } => Some(grant),
                    _ => None,
                };
                let counted = kind.counted();
                let charge = self.get(charge);
                let size = self.get(size);
                // A fixed block's uncharged spare frame words follow its value.
                let spares = match kind {
                    AllocKind::Own { edge } => {
                        hirsel_core_layout::spare_words(&self.input.types, self.layouts, edge)
                    }
                    AllocKind::Environment { ty } => {
                        hirsel_core_layout::frames(&self.input.types, self.layouts, *ty).spares
                    }
                    _ => 0,
                };
                let size = if spares == 0 {
                    size
                } else {
                    let up = self.b.ins().iadd_imm_s(size, 7);
                    let up = self.b.ins().band_imm(up, !7);
                    self.b.ins().iadd_imm_s(up, 8 * spares as i64)
                };
                let alignment = align;
                let align = self.b.ins().iconst(types::I64, align as i64);
                let payer = match place {
                    AllocPlace::Region(v) => self.get(v),
                    _ => self.b.ins().iconst(types::I64, 0),
                };
                // Allocation = {pointer, Capacity{requested,available}}, a C
                // struct of three words. On refusal only the pointer is null;
                // there is no guessed function status or call to the old runtime.
                let values = if matches!(
                    kind,
                    AllocKind::Own {
                        edge: hirsel_core_layout::FieldTy::FrozenBytes
                    }
                ) {
                    let tag = self.b.ins().iconst(
                        types::I64,
                        hirsel_core_layout::FROZEN_STATIC_UNCHARGED as i64,
                    );
                    let zero = self.b.ins().iconst(types::I64, 0);
                    [tag, zero, zero]
                } else if let AllocPlace::Frame(frame) = place {
                    let frame = self.get(frame);
                    let overflow = overflow.map(|v| self.get(v));
                    let values = self.frame_admit(frame, charge, overflow)?;
                    [values[0], self.b.ins().iconst(types::I64, 0), values[1]]
                } else if let AllocPlace::LentFrame(frame) = place {
                    let frame = self.get(frame);
                    let overflow = overflow.map_or_else(
                        || self.b.ins().iconst(types::I8, 0),
                        |v| self.values[v.index()],
                    );
                    let values = self.c_call(
                        "hirsel_core_admit_lent_frame",
                        &[p, p, u, u, boolean],
                        &[p, u, u],
                        &[ctx, frame, size, charge, overflow],
                    )?;
                    [values[0], values[1], values[2]]
                } else if matches!(place, AllocPlace::ByteHead | AllocPlace::ByteTail(_)) {
                    let overflow = overflow.map_or_else(
                        || self.b.ins().iconst(types::I8, 0),
                        |v| self.values[v.index()],
                    );
                    let mut args = vec![ctx];
                    let mut signature = vec![p];
                    let name = if let AllocPlace::ByteTail(head) = place {
                        args.push(self.get(head));
                        signature.push(p);
                        "hirsel_core_alloc_byte_tail"
                    } else {
                        "hirsel_core_alloc_byte_head"
                    };
                    args.extend([size, charge, overflow]);
                    signature.extend([u, u, boolean]);
                    let r = self.c_call(name, &signature, &[p, u, u], &args)?;
                    [r[0], r[1], r[2]]
                } else if resource.is_none() && !matches!(place, AllocPlace::Region(_)) {
                    let overflow = overflow.map(|v| self.get(v));
                    let values = self.own_alloc(native_kind, size, alignment, charge, overflow)?;
                    [values[0], self.b.ins().iconst(types::I64, 0), values[1]]
                } else if let Some(overflow) = overflow {
                    let overflow = self.get(overflow);
                    let r = if matches!(place, AllocPlace::Region(_)) {
                        self.c_call(
                            "hirsel_core_region_slot_alloc",
                            &[p, p, u, u, u, boolean],
                            &[p, u, u],
                            &[ctx, payer, size, align, charge, overflow],
                        )?
                    } else {
                        let reserved = self.b.ins().iconst(types::I64, 0);
                        let kind = self.b.ins().iconst(types::I8, i64::from(native_kind));
                        self.c_call(
                            "hirsel_core_alloc",
                            &[p, p, u, u, u, boolean, Ty::Scalar(Scalar::U8)],
                            &[p, u, u],
                            &[ctx, reserved, size, align, charge, overflow, kind],
                        )?
                    };
                    [r[0], r[1], r[2]]
                } else if let Some(grant) = resource {
                    let storage = self.b.ins().iadd_imm_s(
                        charge,
                        -(hirsel_core_layout::charge::CAUSE_CELL as i64) - grant as i64,
                    );
                    let grant = self.b.ins().iconst(types::I64, grant as i64);
                    let resource = self.c_call(
                        "hirsel_core_resource_alloc",
                        &[p, u, u, u, u],
                        &[p, u, u],
                        &[ctx, size, align, storage, grant],
                    )?;
                    [resource[0], resource[1], resource[2]]
                } else {
                    let values = match place {
                        AllocPlace::Region(_) => {
                            self.region_alloc(payer, size, alignment, charge)?
                        }
                        _ => self.own_alloc(native_kind, size, alignment, charge, None)?,
                    };
                    [values[0], self.b.ins().iconst(types::I64, 0), values[1]]
                };
                self.set(result, values[0]);
                self.set(available, values[2]);
                if let Some(token) = region_entry {
                    let loaded = self.b.create_block();
                    let join = self.b.create_block();
                    self.b.append_block_param(join, types::I64);
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.b
                        .ins()
                        .brif(values[0], loaded, &[], join, &[zero.into()]);
                    self.b.switch_to_block(loaded);
                    let region = self.load_word(payer, 0);
                    let entry = self.load_word(region, memory_layout::REGION_ENTRY);
                    self.b.ins().jump(join, &[entry.into()]);
                    self.b.switch_to_block(join);
                    self.set(token, self.b.block_params(join)[0]);
                }
                let refused = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
                if let Some(count) = counted {
                    let init = self.b.create_block();
                    let ready = self.b.create_block();
                    self.b.ins().brif(refused, ready, &[], init, &[]);
                    self.b.switch_to_block(init);
                    let one = self.b.ins().iconst(types::I64, 1);
                    if count == Counted::Arc {
                        let inline = self.b.create_block();
                        let replay = self.b.create_block();
                        fast::branch_on_arc_replay(&mut self.b, self.context, inline, replay);
                        self.b.switch_to_block(replay);
                        let atomic = self.b.ins().iconst(types::I8, 1);
                        self.c_call(
                            "hirsel_core_counted_init",
                            &[Ty::Ptr, boolean],
                            &[],
                            &[values[0], atomic],
                        )?;
                        self.b.ins().jump(ready, &[]);
                        self.b.switch_to_block(inline);
                    }
                    self.b.ins().store(
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        one,
                        values[0],
                        memory_layout::NODE_COUNT as i32,
                    );
                    if count == Counted::Arc {
                        self.arc_event("hirsel_core_arc_initialized", &[], &[])?;
                    }
                    self.b.ins().jump(ready, &[]);
                    self.b.switch_to_block(ready);
                }
                // Word 0 of an environment names its fixed descriptor.
                if let AllocKind::Environment { ty } = *kind {
                    let init = self.b.create_block();
                    let ready = self.b.create_block();
                    self.b.ins().brif(refused, ready, &[], init, &[]);
                    self.b.switch_to_block(init);
                    let id = self.glues.environments[ty.index()].unwrap();
                    let reference = self.object.declare_data_in_func(id, self.b.func);
                    let descriptor = self.b.ins().symbol_value(types::I64, reference);
                    self.b.ins().store(
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        descriptor,
                        values[0],
                        0,
                    );
                    self.b.ins().jump(ready, &[]);
                    self.b.switch_to_block(ready);
                }
            }
            Inst::Drop { address, ty, fail } => {
                if let Some(glue) = self.glues.drops[ty.index()] {
                    let at = self.addr(address);
                    let glue = self.calls.reference(self.object, &mut self.b, glue);
                    let site = self.site()?;
                    let call = self.b.ins().call(glue, &[self.context, at, site]);
                    let status = self.b.inst_results(call)[0];
                    self.call_failure(status, fail)?;
                }
            }
            Inst::Share { node, kind } => {
                let node = self.get(node);
                fast::share(
                    &mut self.b,
                    &mut self.calls,
                    self.object,
                    self.context,
                    node,
                    kind == Counted::Arc,
                )?;
            }
            Inst::TryUnwrap { result, node, kind } => {
                let node = self.get(node);
                let value = if kind == Counted::Arc {
                    let inline = self.b.create_block();
                    let replay = self.b.create_block();
                    let done = self.b.create_block();
                    self.b.append_block_param(done, types::I8);
                    fast::branch_on_arc_replay(&mut self.b, self.context, inline, replay);
                    self.b.switch_to_block(replay);
                    let atomic = self.b.ins().iconst(types::I8, 1);
                    let unique = self.c_call(
                        "hirsel_core_try_unwrap",
                        &[Ty::Ptr, boolean],
                        &[boolean],
                        &[node, atomic],
                    )?[0];
                    self.b.ins().jump(done, &[unique.into()]);
                    self.b.switch_to_block(inline);
                    let count = self.b.ins().load(
                        types::I64,
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        node,
                        memory_layout::NODE_COUNT as i32,
                    );
                    let unique = self.b.ins().icmp_imm_s(IntCC::Equal, count, 1);
                    self.arc_poll_event("hirsel_core_arc_unwrapped", &[boolean], &[unique])?;
                    self.b.ins().jump(done, &[unique.into()]);
                    self.b.switch_to_block(done);
                    self.b.block_params(done)[0]
                } else {
                    let count = self.b.ins().load(
                        types::I64,
                        cranelift_codegen::ir::MemFlagsData::trusted(),
                        node,
                        memory_layout::NODE_COUNT as i32,
                    );
                    self.b.ins().icmp_imm_s(IntCC::Equal, count, 1)
                };
                self.set(result, value);
            }
            Inst::Release { node, kind, ty } => {
                let node = self.get(node);
                let charge = hirsel_core_layout::charge::counted(
                    self.layouts[ty.index()].logical_size.unwrap(),
                )
                .unwrap();
                fast::release(
                    &mut self.b,
                    &mut self.calls,
                    self.object,
                    [self.context, node],
                    (
                        charge,
                        (!hirsel_core_layout::terminal(
                            &self.input.types,
                            self.layouts,
                            &if kind == Counted::Arc {
                                hirsel_core_layout::FieldTy::Arc(hirsel_core_layout::TypeRef(ty.0))
                            } else {
                                hirsel_core_layout::FieldTy::Rc(hirsel_core_layout::TypeRef(ty.0))
                            },
                        ))
                        .then(|| self.glues.fixed[ty.index()].unwrap()[1]),
                    ),
                    kind == Counted::Arc,
                )?;
            }
            Inst::Case { result, address } => {
                let value = self.case(address, None)?.unwrap();
                self.set(result, value);
            }
            Inst::SetCase { address, case } => {
                self.case(address, Some(case))?;
            }
            Inst::Trim {
                block: _,
                region,
                entry,
                previous,
                charge,
            } => {
                let slot = self.get(region);
                let region = self.load_word(slot, 0);
                let entry = self.get(entry);
                let previous = self.get(previous);
                let charge = self.get(charge);
                self.c_call(
                    "hirsel_core_region_lower_charge",
                    &[p, u, u, u],
                    &[],
                    &[region, entry, previous, charge],
                )?;
            }
            Inst::Free { block, charge } => {
                let block = self.get(block);
                let charge = self.get(charge);
                fast::free(
                    &mut self.b,
                    &mut self.calls,
                    self.object,
                    self.context,
                    block,
                    charge,
                )?;
            }
            Inst::PromoteFrame { carrier } => {
                if let Some(hirsel_core_layout::FieldTy::Seq(elem)) =
                    c::sequence_edge(&self.input.types, self.body.addresses[carrier.index()].ty)
                {
                    let stride =
                        hirsel_core_layout::field_size(&self.input.types, self.layouts, elem).0;
                    let at = self.addr(carrier);
                    let data = self.load_word(at, 0);
                    let len = self.load_word(at, 8);
                    let cap = self.load_word(at, 16);
                    let stride = self.b.ins().iconst(types::I64, stride as i64);
                    let promoted = self.c_call(
                        "hirsel_core_promote_frame_list",
                        &[p, p, u, u, u],
                        &[p],
                        &[self.context, data, cap, len, stride],
                    )?[0];
                    self.store_word(at, 0, promoted);
                }
            }
            Inst::FrameRefund { charge } => {
                let charge = self.get(charge);
                let payer = self.current_limit();
                self.c_call(
                    "hirsel_core_refund_in",
                    &[p, p, u],
                    &[],
                    &[self.context, payer, charge],
                )?;
            }
            Inst::Copy { to, from, bytes } => {
                let to = self.addr(to);
                let from = self.addr(from);
                let bytes = self.get(bytes);
                if !self.small_copy(to, from, bytes) {
                    self.c_call("memcpy", &[p, p, u], &[p], &[to, from, bytes])?;
                }
            }
            Inst::Move { to, from, bytes } => {
                let to = self.addr(to);
                let from = self.addr(from);
                let bytes = self.get(bytes);
                self.c_call("memmove", &[p, p, u], &[p], &[to, from, bytes])?;
            }
            // Full-new admission and install-before-refund are unchanged.
            // Growth may reuse ordinary local backing under the shared policy;
            // every other buffer takes the original allocate/copy/free path.
            Inst::Grow {
                result,
                available,
                carrier,
                charge,
                overflow,
                size,
                align,
                capacity,
                moved,
                refund,
            } => {
                let edge =
                    c::sequence_edge(&self.input.types, self.body.addresses[carrier.index()].ty)
                        .expect("wf: grow of a sequence carrier")
                        .clone();
                let native_kind =
                    match hirsel_core_layout::cleanup_class(&self.input.types, self.layouts, &edge)
                    {
                        Some(
                            hirsel_core_layout::Cleanup::Terminal
                            | hirsel_core_layout::Cleanup::Fixed,
                        ) => memory_layout::NATIVE_PLAIN,
                        _ => memory_layout::NATIVE_GRAPH,
                    };
                let at = self.addr(carrier);
                let old = self
                    .b
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), at, 0);
                let (charge, size) = (self.get(charge), self.get(size));
                let overflow = overflow.map(|v| self.get(v));
                let (moved, capacity, refund) =
                    (self.get(moved), self.get(capacity), self.get(refund));
                let hirsel_core_layout::FieldTy::Seq(item) = edge else {
                    unreachable!("grow sequence")
                };
                let stride =
                    hirsel_core_layout::field_size(&self.input.types, self.layouts, &item).0;
                let values = self.grow_buffer(
                    native_kind,
                    stride,
                    at,
                    old,
                    size,
                    align,
                    charge,
                    overflow,
                    capacity,
                    moved,
                    refund,
                )?;
                self.set(result, values[0]);
                self.set(available, values[1]);
            }
            Inst::LimitEnter {
                admitted,
                available,
                bytes,
            } => {
                let ctx = self.context;
                let bytes = self.get(bytes);
                if self.empty_bound {
                    let payer = self.current_limit();
                    let (ok, available_value) = self.admission(payer, bytes)?;
                    let zero = self.b.ins().iconst(types::I64, 0);
                    let room = self.b.ins().select(ok, zero, available_value);
                    self.set(admitted, ok);
                    self.set(available, room);
                    return Ok(());
                }
                let values = self.c_call(
                    "hirsel_core_limit_enter",
                    &[p, u],
                    &[p, u, u],
                    &[ctx, bytes],
                )?;
                let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, values[0], 0);
                self.set(admitted, ok);
                self.set(available, values[2]);
            }
            Inst::Step { fail } => self.step(fail)?,
            Inst::LimitExit => {
                if self.empty_bound {
                    return Ok(());
                }
                self.c_call("hirsel_core_limit_exit", &[p], &[], &[self.context])?;
            }
            Inst::RegionOpen { result } => {
                use cranelift_codegen::ir::{StackSlotData, StackSlotKind};
                let slot = self.b.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let null = self.b.ins().iconst(types::I64, 0);
                let handle = self.b.ins().stack_addr(types::I64, slot, 0);
                self.store_word(handle, 0, null);
                self.set(result, handle);
            }
            Inst::RegionClose { region } => {
                let slot = self.get(region);
                let region = self.load_word(slot, 0);
                let close = self.b.create_block();
                let done = self.b.create_block();
                self.b.ins().brif(region, close, &[], done, &[]);
                self.b.switch_to_block(close);
                self.c_call("hirsel_core_region_close", &[p], &[], &[region])?;
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            _ => {
                return Err(unsupported(format!(
                    "core failure/count/resource runtime: {inst:?}"
                )));
            }
        }
        Ok(())
    }
}
