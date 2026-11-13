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
    fn load_word(&mut self, base: Value, offset: usize) -> Value {
        self.b
            .ins()
            .load(types::I64, Flags::trusted(), base, offset as i32)
    }
    fn store_word(&mut self, base: Value, offset: usize, value: Value) {
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
    fn debit(&mut self, payer: Value, charge: Value, available: Value) {
        let available = self.b.ins().isub(available, charge);
        let held = self.load_word(payer, memory_layout::LIMIT_OUTSTANDING);
        let held = self.b.ins().iadd(held, charge);
        self.store_word(payer, memory_layout::LIMIT_AVAILABLE, available);
        self.store_word(payer, memory_layout::LIMIT_OUTSTANDING, held);
    }
    pub fn own_alloc(
        &mut self,
        kind: AllocKind,
        place: AllocPlace,
        size: Value,
        align: u64,
        charge: Value,
    ) -> Result<[Value; 2]> {
        let slow = self.cold_block();
        let fast = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let payer = self.current_limit();
        let alignment = align.max(8);
        let prefix = if kind == AllocKind::Plain {
            memory_layout::PLAIN_PREFIX_BYTES
        } else {
            memory_layout::GRAPH_PREFIX_BYTES
        } as u64;
        let offset = prefix.next_multiple_of(alignment);
        let one = self.b.ins().iconst(types::I64, 1);
        let nonzero_size = self.b.ins().umax(size, one);
        let offset_value = self.b.ins().iconst(types::I64, offset as i64);
        let (native_size, overflow) = self.b.ins().uadd_overflow(nonzero_size, offset_value);
        let zero_charge = self.b.ins().icmp_imm_s(IntCC::Equal, charge, 0);
        let too_large = self.b.ins().icmp_imm_s(
            IntCC::UnsignedGreaterThan,
            native_size,
            (i64::MAX as u64 - (alignment - 1)) as i64,
        );
        let fallback = self.b.ins().bor(overflow, zero_charge);
        let fallback = self.b.ins().bor(fallback, too_large);
        let eligible = self.b.create_block();
        let fallback = if alignment > 512 {
            self.b.ins().iconst(types::I8, 1)
        } else {
            fallback
        };
        self.b.ins().brif(fallback, slow, &[], eligible, &[]);
        self.b.switch_to_block(eligible);
        let like = match place {
            AllocPlace::Like(v) => Some(self.get(v)),
            _ => None,
        };
        if let Some(like) = like {
            // Empty owners use null to select the ambient payer on their first growth.
            let header = self.b.create_block();
            let empty = self.b.ins().icmp_imm_s(IntCC::Equal, like, 0);
            self.b.ins().brif(empty, fast, &[], header, &[]);
            self.b.switch_to_block(header);
            let tagged = self.b.ins().load(
                types::I64,
                Flags::trusted(),
                like,
                -(memory_layout::OWN_PAYER_BACK as i32),
            );
            let original = self
                .b
                .ins()
                .band_imm_s(tagged, !(memory_layout::OWN_TAG_MASK as i64));
            let same = self.b.ins().icmp(IntCC::Equal, original, payer);
            self.b.ins().brif(same, fast, &[], slow, &[]);
        } else {
            self.b.ins().jump(fast, &[]);
        }
        self.b.switch_to_block(fast);
        let (fits, available) = self.admission(payer, charge)?;
        let allocate = self.b.create_block();
        let refused = self.cold_block();
        self.b.ins().brif(fits, allocate, &[], refused, &[]);
        self.b.switch_to_block(refused);
        let null = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[null.into(), available.into()]);
        self.b.switch_to_block(allocate);
        self.debit(payer, charge, available);
        let align_value = self.b.ins().iconst(types::I64, alignment as i64);
        let base = self.c_call(
            "hirsel_core_native_alloc",
            &[U, U],
            &[P],
            &[native_size, align_value],
        )?[0];
        let pointer = self.b.ins().iadd_imm_s(base, offset as i64);
        let native_kind = u64::from(kind != AllocKind::Plain);
        let tag = native_kind
            | (u64::from(alignment.trailing_zeros() - 3) << memory_layout::OWN_ALIGNMENT_SHIFT);
        let tagged = self.b.ins().bor_imm_s(payer, tag as i64);
        self.b.ins().store(
            Flags::trusted(),
            tagged,
            pointer,
            -(memory_layout::OWN_PAYER_BACK as i32),
        );
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[pointer.into(), zero.into()]);
        self.b.switch_to_block(slow);
        let like = like.unwrap_or_else(|| self.b.ins().iconst(types::I64, 0));
        let align_value = self.b.ins().iconst(types::I64, align as i64);
        let zero = self.b.ins().iconst(types::I8, 0);
        let native = self.b.ins().iconst(types::I8, native_kind as i64);
        let values = self.c_call(
            "hirsel_core_alloc",
            &[P, P, U, U, U, B, K],
            &[P, U, U],
            &[self.context, like, size, align_value, charge, zero, native],
        )?;
        self.b
            .ins()
            .jump(done, &[values[0].into(), values[2].into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
    pub fn region_alloc(
        &mut self,
        region: Value,
        size: Value,
        align: u64,
        charge: Value,
    ) -> Result<[Value; 2]> {
        let slow = self.cold_block();
        let fast = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
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
        self.debit(payer, charge, available);
        let held = self.b.ins().iadd(held, charge);
        self.store_word(region, memory_layout::REGION_CHARGE, held);
        let cursor = self.load_word(region, memory_layout::REGION_CURSOR);
        let limit = self.load_word(region, memory_layout::REGION_END);
        let mask = self.b.ins().iconst(types::I64, (align - 1) as i64);
        let (rounded, overflow) = self.b.ins().uadd_overflow(cursor, mask);
        let start = self.b.ins().band_imm_s(rounded, -(align as i64));
        let one = self.b.ins().iconst(types::I64, 1);
        let bytes = self.b.ins().umax(size, one);
        let (end, end_overflow) = self.b.ins().uadd_overflow(start, bytes);
        let over = self.b.ins().bor(overflow, end_overflow);
        let room = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, end, limit);
        let valid = self.b.ins().icmp_imm_s(IntCC::Equal, over, 0);
        let room = self.b.ins().band(room, valid);
        let bump = self.b.create_block();
        let refill = self.cold_block();
        self.b.ins().brif(room, bump, &[], refill, &[]);
        self.b.switch_to_block(bump);
        self.store_word(region, memory_layout::REGION_CURSOR, end);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b.ins().jump(done, &[start.into(), zero.into()]);
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
            "hirsel_core_region_alloc",
            &[P, P, U, U, U, B],
            &[P, U, U],
            &[self.context, region, size, alignment, charge, no_overflow],
        )?;
        self.b
            .ins()
            .jump(done, &[values[0].into(), values[2].into()]);
        self.b.switch_to_block(done);
        Ok([self.b.block_params(done)[0], self.b.block_params(done)[1]])
    }
}

pub(super) fn arc_event(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    context: Value,
    name: &'static str,
    argument_types: &[Ty],
    arguments: &[Value],
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
    b.ins().brif(empty, done, &[], call, &[]);
    b.switch_to_block(call);
    let mut params = vec![P];
    params.extend_from_slice(argument_types);
    let mut args = vec![context];
    args.extend_from_slice(arguments);
    calls.c(object, b, name, &params, &[], &args)?;
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
        let initial = b.ins().atomic_load(types::I64, Flags::trusted(), node);
        let head = b.create_block();
        let update = b.create_block();
        b.append_block_param(head, types::I64);
        b.ins().jump(head, &[initial.into()]);
        b.switch_to_block(head);
        let old = b.block_params(head)[0];
        let one = b.ins().iconst(types::I64, 1);
        let (next, wrapped) = b.ins().uadd_overflow(old, one);
        b.ins().brif(wrapped, overflow, &[], update, &[]);
        b.switch_to_block(update);
        // Cranelift atomics are SeqCst, stronger than the runtime's Relaxed CAS.
        let actual = b.ins().atomic_cas(Flags::trusted(), node, old, next);
        let same = b.ins().icmp(IntCC::Equal, actual, old);
        let observed = b.create_block();
        b.ins().brif(same, observed, &[], head, &[actual.into()]);
        b.switch_to_block(observed);
        arc_event(
            b,
            calls,
            object,
            context,
            "hirsel_core_arc_shared",
            &[P],
            &[node],
        )?;
        b.ins().jump(done, &[]);
    } else {
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
        b.ins().jump(done, &[]);
    }
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

pub(super) fn release(
    b: &mut FunctionBuilder<'_>,
    calls: &mut calls::Calls<'_>,
    object: &mut ObjectModule,
    operands: [Value; 2],
    cleanup: (u64, cranelift_module::FuncId),
    atomic: bool,
) -> Result<()> {
    let [context, node] = operands;
    let one = b.ins().iconst(types::I64, 1);
    let last = if atomic {
        // SeqCst includes the required Release decrement and Acquire on last.
        let old = b.ins().atomic_rmw(
            types::I64,
            Flags::trusted(),
            ir::AtomicRmwOp::Sub,
            node,
            one,
        );
        b.ins().icmp_imm_s(IntCC::Equal, old, 1)
    } else {
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
        b.ins().icmp_imm_s(IntCC::Equal, next, 0)
    };
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
    let reference = calls.reference(object, b, cleanup.1);
    let recipe = b.ins().func_addr(types::I64, reference);
    let charge = b.ins().iconst(types::I64, cleanup.0 as i64);
    calls.c(
        object,
        b,
        "hirsel_core_release_last",
        &[P, P, U, P],
        &[],
        &[context, node, charge, recipe],
    )?;
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
            | Inst::Free { .. }
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
