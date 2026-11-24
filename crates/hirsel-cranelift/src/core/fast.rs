//! Frozen runtime prefixes supply the common path. Calls own identity changes,
//! physical exhaustion and last-release traversal. No compiler output is checked.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{MemFlagsData as Flags, Value, condcodes::IntCC};
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const B: Ty = Ty::Scalar(Scalar::Bool);
const K: Ty = Ty::Scalar(Scalar::U8);

impl Function<'_, '_, '_> {
    pub(super) fn load_word(&mut self, base: Value, offset: usize) -> Value {
        self.b
            .ins()
            .load(types::I64, Flags::trusted(), base, offset as i32)
    }
    pub(super) fn store_word(&mut self, base: Value, offset: usize, value: Value) {
        self.b
            .ins()
            .store(Flags::trusted(), value, base, offset as i32);
    }
    pub fn current_limit(&mut self) -> Value {
        self.load_word(self.context, memory_layout::CONTEXT_CURRENT_LIMIT)
    }
    fn cold_block(&mut self) -> ir::Block {
        let block = self.b.create_block();
        self.b.set_cold_block(block);
        block
    }
    pub fn arc_event(&mut self, name: &'static str, types: &[Ty], args: &[Value]) -> Result<()> {
        arc_event(
            &mut self.b,
            &mut self.calls,
            self.object,
            self.context,
            name,
            types,
            args,
        )
    }
    pub fn arc_poll_event(
        &mut self,
        name: &'static str,
        types: &[Ty],
        args: &[Value],
    ) -> Result<()> {
        arc_hook(
            &mut self.b,
            &mut self.calls,
            self.object,
            self.context,
            name,
            types,
            args,
            false,
        )
    }
    pub fn admission(&mut self, payer: Value, charge: Value) -> Result<(Value, Value)> {
        let available = self.load_word(payer, memory_layout::LIMIT_AVAILABLE);
        let fits = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, charge, available);
        let fits = if self.testing {
            // Only emitted test objects poll the deterministic refusal injector.
            // This hook counts the same attempt without debiting it a second time.
            let refused = self.c_call(
                "hirsel_core_test_inline_admission",
                &[P],
                &[B],
                &[self.context],
            )?[0];
            let allowed = self.b.ins().icmp_imm_s(IntCC::Equal, refused, 0);
            self.b.ins().band(fits, allowed)
        } else {
            fits
        };
        Ok((fits, available))
    }
    fn debit(&mut self, payer: Value, charge: Value, available: Value) -> Result<()> {
        let held = self.load_word(payer, memory_layout::LIMIT_OUTSTANDING);
        let held = self.b.ins().iadd(held, charge);
        self.store_word(payer, memory_layout::LIMIT_OUTSTANDING, held);
        self.debit_room(payer, charge, available)
    }
    /// Take `charge` from the payer's room only. Frame charges pin no record
    /// (`Payer::admit_frame`), so they leave `outstanding` alone.
    fn debit_room(&mut self, payer: Value, charge: Value, available: Value) -> Result<()> {
        let available = self.b.ins().isub(available, charge);
        self.store_word(payer, memory_layout::LIMIT_AVAILABLE, available);
        if self.observe_charge {
            self.c_call("hirsel_core_observe_charge", &[], &[], &[])?;
        }
        Ok(())
    }
    pub fn own_alloc(
        &mut self,
        native_kind: u8,
        size: Value,
        align: u64,
        charge: Value,
        overflow: Option<Value>,
    ) -> Result<[Value; 2]> {
        let slow = self.cold_block();
        let fast = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let payer = self.current_limit();
        let overflow = overflow.unwrap_or_else(|| self.b.ins().iconst(types::I8, 0));
        self.b.ins().brif(overflow, slow, &[], fast, &[]);
        self.b.switch_to_block(fast);
        let available = self.load_word(payer, memory_layout::LIMIT_AVAILABLE);
        // Wrapping charge-1 < available combines positive charge and room.
        // Zero becomes MAX and keeps the runtime's zero-pin path even at MAX.
        let last = self.b.ins().iadd_imm_s(charge, -1);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThan, last, available);
        let probe = self.b.create_block();
        let allocate = self.b.create_block();
        let refused = self.cold_block();
        self.b.ins().brif(fits, probe, &[], slow, &[]);
        self.b.switch_to_block(probe);
        if self.testing {
            // The test-only probe emits an injected refusal event and counts the
            // attempt once. Real shortages reached the ordinary cold allocator.
            let allowed = self.c_call(
                "hirsel_core_test_probe_admission",
                &[P, U],
                &[B],
                &[self.context, charge],
            )?[0];
            self.b.ins().brif(allowed, allocate, &[], refused, &[]);
        } else {
            self.b.ins().jump(allocate, &[]);
        }
        self.b.switch_to_block(refused);
        let null = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[null.into(), available.into()]);
        self.b.switch_to_block(allocate);
        self.debit(payer, charge, available)?;
        let align_value = self.b.ins().iconst(types::I64, align as i64);
        let native = self.b.ins().iconst(types::I8, i64::from(native_kind));
        let pointer = self.c_call(
            "hirsel_core_alloc_admitted",
            &[P, P, U, U, K],
            &[P],
            &[self.context, payer, size, align_value, native],
        )?[0];
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[pointer.into(), zero.into()]);
        self.b.switch_to_block(slow);
        let reserved = self.b.ins().iconst(types::I64, 0);
        let align_value = self.b.ins().iconst(types::I64, align as i64);
        let native = self.b.ins().iconst(types::I8, i64::from(native_kind));
        let values = self.c_call(
            "hirsel_core_alloc",
            &[P, P, U, U, U, B, K],
            &[P, U, U],
            &[
                self.context,
                reserved,
                size,
                align_value,
                charge,
                overflow,
                native,
            ],
        )?;
        self.b
            .ins()
            .jump(done, &[values[0].into(), values[2].into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
    /// Admit `charge` for frame bytes at `frame` (`alloc .. frame`): the own
    /// allocation's inline admission, with nothing allocated; a refusal, overflow
    /// or test probe takes `hirsel_core_frame_admit`, which keeps the Capacity event.
    /// Frame charges move only the room: no block names the record.
    pub fn frame_admit(
        &mut self,
        frame: Value,
        charge: Value,
        overflow: Option<Value>,
    ) -> Result<[Value; 2]> {
        let slow = self.cold_block();
        let fast = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let payer = self.current_limit();
        let overflow = overflow.unwrap_or_else(|| self.b.ins().iconst(types::I8, 0));
        self.b.ins().brif(overflow, slow, &[], fast, &[]);
        self.b.switch_to_block(fast);
        let available = self.load_word(payer, memory_layout::LIMIT_AVAILABLE);
        let last = self.b.ins().iadd_imm_s(charge, -1);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThan, last, available);
        let admit = self.b.create_block();
        let inline = if self.testing { slow } else { admit };
        self.b.ins().brif(fits, inline, &[], slow, &[]);
        self.b.switch_to_block(admit);
        self.debit_room(payer, charge, available)?;
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[frame.into(), zero.into()]);
        self.b.switch_to_block(slow);
        let values = self.c_call(
            "hirsel_core_frame_admit",
            &[P, U, B],
            &[P, U, U],
            &[self.context, charge, overflow],
        )?;
        let refused = self.b.ins().icmp_imm_s(IntCC::Equal, values[0], 0);
        let zero = self.b.ins().iconst(types::I64, 0);
        let pointer = self.b.ins().select(refused, zero, frame);
        self.b.ins().jump(done, &[pointer.into(), values[2].into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
    pub fn region_alloc(
        &mut self,
        slot: Value,
        size: Value,
        align: u64,
        charge: Value,
    ) -> Result<[Value; 2]> {
        let slow = self.cold_block();
        let fast = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let open = self.b.create_block();
        let region = self.load_word(slot, 0);
        self.b.ins().brif(region, open, &[], slow, &[]);
        self.b.switch_to_block(open);
        let payer = self.current_limit();
        let original = self.load_word(region, memory_layout::REGION_PAYER);
        let held = self.load_word(region, memory_layout::REGION_CHARGE);
        let same = self.b.ins().icmp(IntCC::Equal, original, payer);
        // A zero charge can leave an exhausted entry's payer pointer dangling.
        // Re-establish that entry through the full runtime before using it again.
        let live = self.b.ins().icmp_imm_s(IntCC::NotEqual, held, 0);
        let positive = self.b.ins().icmp_imm_s(IntCC::NotEqual, charge, 0);
        let valid = self.b.ins().band(same, live);
        let valid = self.b.ins().band(valid, positive);
        self.b.ins().brif(valid, fast, &[], slow, &[]);
        self.b.switch_to_block(fast);
        let (fits, available) = self.admission(payer, charge)?;
        let admitted = self.b.create_block();
        let refused = self.cold_block();
        self.b.ins().brif(fits, admitted, &[], refused, &[]);
        self.b.switch_to_block(refused);
        let null = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[null.into(), available.into()]);
        self.b.switch_to_block(admitted);
        self.debit(payer, charge, available)?;
        let held = self.b.ins().iadd(held, charge);
        self.store_word(region, memory_layout::REGION_CHARGE, held);
        let stored_size = self.load_word(region, memory_layout::REGION_SIZE);
        let stored_alignment = self.load_word(region, memory_layout::REGION_ALIGNMENT);
        let same_size = self.b.ins().icmp(IntCC::Equal, stored_size, size);
        let same_align = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, stored_alignment, align as i64);
        let same_pool = self.b.ins().band(same_size, same_align);
        let cursor = self.load_word(region, memory_layout::REGION_CURSOR);
        let limit = self.load_word(region, memory_layout::REGION_END);
        let stride = self.load_word(region, memory_layout::REGION_STRIDE);
        let remaining = self.b.ins().isub(limit, cursor);
        let room = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, stride, remaining);
        let room = self.b.ins().band(room, same_pool);
        let bump = self.b.create_block();
        let refill = self.cold_block();
        self.b.ins().brif(room, bump, &[], refill, &[]);
        self.b.switch_to_block(bump);
        let end = self.b.ins().iadd(cursor, stride);
        self.store_word(region, memory_layout::REGION_CURSOR, end);
        let occupied = self.load_word(region, memory_layout::REGION_OCCUPIED);
        let occupied = self.b.ins().iadd(occupied, stride);
        self.store_word(region, memory_layout::REGION_OCCUPIED, occupied);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[cursor.into(), zero.into()]);
        self.b.switch_to_block(refill);
        let zero = self.b.ins().iconst(types::I64, 0);
        let alignment = self.b.ins().iconst(types::I64, align as i64);
        let pointer = self.c_call(
            "hirsel_core_region_refill",
            &[P, U, U],
            &[P],
            &[region, size, alignment],
        )?[0];
        self.b.ins().jump(done, &[pointer.into(), zero.into()]);
        self.b.switch_to_block(slow);
        let alignment = self.b.ins().iconst(types::I64, align as i64);
        let no_overflow = self.b.ins().iconst(types::I8, 0);
        let values = self.c_call(
            "hirsel_core_region_slot_alloc",
            &[P, P, U, U, U, B],
            &[P, U, U],
            &[self.context, slot, size, alignment, charge, no_overflow],
        )?;
        self.b
            .ins()
            .jump(done, &[values[0].into(), values[2].into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
}

pub(super) fn free(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    context: Value,
    pointer: Value,
    charge: Value,
) -> Result<()> {
    calls.c(
        object,
        b,
        "hirsel_core_free_value",
        &[P, P, U],
        &[],
        &[context, pointer, charge],
    )?;
    Ok(())
}

pub(super) fn branch_on_arc_replay(
    b: &mut FunctionBuilder<'_>,
    context: Value,
    inline: ir::Block,
    replay: ir::Block,
) {
    let gate = b.create_block();
    b.set_cold_block(replay);
    let task = b.ins().load(
        types::I64,
        Flags::trusted().with_readonly(),
        context,
        memory_layout::CONTEXT_EVENTS as i32,
    );
    let empty = b.ins().icmp_imm_s(IntCC::Equal, task, 0);
    b.ins().brif(empty, inline, &[], gate, &[]);
    b.switch_to_block(gate);
    let active = b.ins().load(
        types::I8,
        Flags::trusted(),
        task,
        memory_layout::TASK_ARC_REPLAY as i32,
    );
    b.ins().brif(active, replay, &[], inline, &[]);
}

/// A scheduled Arc init, share or release folds posted credits and records its
/// event. The hook runs only when the task's TASK_ARC_HOOKS byte is set or its
/// payer's domain has posted credit; otherwise it would do neither.
pub(super) fn arc_event(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    context: Value,
    name: &'static str,
    argument_types: &[Ty],
    arguments: &[Value],
) -> Result<()> {
    arc_hook(
        b,
        calls,
        object,
        context,
        name,
        argument_types,
        arguments,
        true,
    )
}

/// `gated` false is the Arc unique test: a poll, so its hook always runs.
#[allow(clippy::too_many_arguments)]
fn arc_hook(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    context: Value,
    name: &'static str,
    argument_types: &[Ty],
    arguments: &[Value],
    gated: bool,
) -> Result<()> {
    let call = b.create_block();
    let done = b.create_block();
    b.set_cold_block(call);
    let events = b.ins().load(
        types::I64,
        Flags::trusted().with_readonly(),
        context,
        memory_layout::CONTEXT_EVENTS as i32,
    );
    let empty = b.ins().icmp_imm_s(IntCC::Equal, events, 0);
    if gated {
        let hooks = b.create_block();
        let credit = b.create_block();
        b.ins().brif(empty, done, &[], hooks, &[]);
        b.switch_to_block(hooks);
        let flag = b.ins().load(
            types::I8,
            Flags::trusted(),
            events,
            memory_layout::TASK_ARC_HOOKS as i32,
        );
        b.ins().brif(flag, call, &[], credit, &[]);
        b.switch_to_block(credit);
        // The payer and its domain may change between events; neither load
        // is readonly, and the pending byte is an atomic (Acquire) load.
        let payer = b.ins().load(
            types::I64,
            Flags::trusted(),
            context,
            memory_layout::CONTEXT_CURRENT_LIMIT as i32,
        );
        let domain = b.ins().load(
            types::I64,
            Flags::trusted(),
            payer,
            memory_layout::SLOT_DOMAIN as i32,
        );
        let at = b
            .ins()
            .iadd_imm(domain, memory_layout::DOMAIN_PENDING as i64);
        let pending = b.ins().atomic_load(types::I8, Flags::trusted(), at);
        b.ins().brif(pending, call, &[], done, &[]);
    } else {
        b.ins().brif(empty, done, &[], call, &[]);
    }
    b.switch_to_block(call);
    // These four frozen hooks take context and at most one event operand.
    let mut params = [P; 2];
    params[1..1 + argument_types.len()].copy_from_slice(argument_types);
    let mut args = [context; 2];
    args[1..1 + arguments.len()].copy_from_slice(arguments);
    calls.c(
        object,
        b,
        name,
        &params[..1 + argument_types.len()],
        &[],
        &args[..1 + arguments.len()],
    )?;
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    Ok(())
}

pub(super) fn share(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    context: Value,
    node: Value,
    atomic: bool,
) -> Result<()> {
    let done = b.create_block();
    let overflow = b.create_block();
    b.set_cold_block(overflow);
    if atomic {
        let inline = b.create_block();
        let replay = b.create_block();
        branch_on_arc_replay(b, context, inline, replay);
        b.switch_to_block(replay);
        let atomic = b.ins().iconst(types::I8, 1);
        calls.c(
            object,
            b,
            "hirsel_core_share",
            &[P, B],
            &[],
            &[node, atomic],
        )?;
        b.ins().jump(done, &[]);
        b.switch_to_block(inline);
    }
    // Rc and Arc counts are plain words: an Arc never leaves the thread
    // running its unit (CONC-2).
    let old = b.ins().load(
        types::I64,
        Flags::trusted(),
        node,
        memory_layout::NODE_COUNT as i32,
    );
    let one = b.ins().iconst(types::I64, 1);
    let (next, wrapped) = b.ins().uadd_overflow(old, one);
    let store = b.create_block();
    b.ins().brif(wrapped, overflow, &[], store, &[]);
    b.switch_to_block(store);
    b.ins().store(
        Flags::trusted(),
        next,
        node,
        memory_layout::NODE_COUNT as i32,
    );
    if atomic {
        arc_event(
            b,
            calls,
            object,
            context,
            "hirsel_core_arc_shared",
            &[P],
            &[node],
        )?;
    }
    b.ins().jump(done, &[]);
    b.switch_to_block(overflow);
    let atomic = b.ins().iconst(types::I8, i64::from(atomic));
    calls.c(
        object,
        b,
        "hirsel_core_share",
        &[P, B],
        &[],
        &[node, atomic],
    )?;
    b.ins().trap(ir::TrapCode::unwrap_user(1));
    b.switch_to_block(done);
    Ok(())
}

pub(super) fn decrement(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    context: Value,
    node: Value,
    atomic: bool,
) -> Result<Value> {
    let done = b.create_block();
    b.append_block_param(done, types::I8);
    if atomic {
        let inline = b.create_block();
        let replay = b.create_block();
        branch_on_arc_replay(b, context, inline, replay);
        b.switch_to_block(replay);
        let last = calls.c(
            object,
            b,
            "hirsel_core_arc_decrement_for_replay",
            &[P],
            &[B],
            &[node],
        )?[0];
        b.ins().jump(done, &[last.into()]);
        b.switch_to_block(inline);
    }
    let one = b.ins().iconst(types::I64, 1);
    let old = b.ins().load(
        types::I64,
        Flags::trusted(),
        node,
        memory_layout::NODE_COUNT as i32,
    );
    let next = b.ins().isub(old, one);
    b.ins().store(
        Flags::trusted(),
        next,
        node,
        memory_layout::NODE_COUNT as i32,
    );
    let last = b.ins().icmp_imm_s(IntCC::Equal, next, 0);
    b.ins().jump(done, &[last.into()]);
    b.switch_to_block(done);
    Ok(b.block_params(done)[0])
}

pub(super) fn release(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    operands: [Value; 2],
    cleanup: (u64, Option<cranelift_module::DataId>),
    atomic: bool,
) -> Result<()> {
    let [context, node] = operands;
    let last = decrement(b, calls, object, context, node, atomic)?;
    if atomic {
        arc_event(
            b,
            calls,
            object,
            context,
            "hirsel_core_arc_released",
            &[B],
            &[last],
        )?;
    }
    let slow = b.create_block();
    let done = b.create_block();
    b.set_cold_block(slow);
    b.ins().brif(last, slow, &[], done, &[]);
    b.switch_to_block(slow);
    if let Some(recipe) = cleanup.1 {
        let reference = object.declare_data_in_func(recipe, b.func);
        let recipe = b.ins().symbol_value(types::I64, reference);
        // A shareable payload holds no resource or finalizer, so its release
        // cannot fail and needs no site.
        let zero = b.ins().iconst(types::I64, 0);
        calls.c(
            object,
            b,
            "hirsel_core_release",
            &[P, P, P, U],
            &[B],
            &[context, node, recipe, zero],
        )?;
    } else {
        let charge = b.ins().iconst(types::I64, cleanup.0 as i64);
        free(b, calls, object, context, node, charge)?;
    }
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
    Ok(())
}

// This common empty-bound shape has no operation that can charge, refund or
// observe a limit while it is active. The successful reservation/refund cancels;
// preserve its checked admission and failure-injection event alone. Other shapes
// retain ordinary runtime scope identity, including nested scopes and calls.
pub(super) fn empty_bound(body: &Body) -> bool {
    let mut enters = 0;
    let mut exits = 0;
    for inst in &body.insts {
        match inst {
            Inst::LimitEnter { .. } => enters += 1,
            Inst::LimitExit => exits += 1,
            Inst::Alloc { .. }
            | Inst::Grow { .. }
            | Inst::Free { .. }
            | Inst::FrameRefund { .. }
            | Inst::PromoteFrame { .. }
            | Inst::Drop { .. }
            | Inst::Call { .. }
            | Inst::RegionOpen { .. }
            | Inst::RegionClose { .. }
            | Inst::Trim { .. }
            | Inst::Release { .. } => return false,
            _ => {}
        }
    }
    enters == 1 && exits == 1
}
