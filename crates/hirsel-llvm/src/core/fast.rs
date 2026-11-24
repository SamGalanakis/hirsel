//! Frozen runtime-prefix fast paths. Only allocation/refill, nonlocal payers,
//! compound resource construction and last-holder cleanup cross the C ABI.
use super::memory_layout::*;
use super::storage::{Payer, zeroext};
use super::*;

/// `void core_release_last_N_{rc,arc}(ptr context, ptr node)`.
pub(super) fn release_last_signature() -> FnSig {
    FnSig::plain(Type::Void, [Type::Ptr, Type::Ptr])
}

impl FunctionEmitter<'_> {
    pub(super) fn field_pointer(&mut self, p: &Value, offset: usize) -> Value {
        if offset == 0 {
            p.clone()
        } else {
            self.offset(p, offset as u64)
        }
    }
    pub(super) fn load_native(&mut self, p: &Value, offset: usize, ty: Type) -> Value {
        let at = self.field_pointer(p, offset);
        self.load(&ty, &at, 8)
    }
    pub(super) fn store_native(&mut self, p: &Value, offset: usize, ty: Type, value: &Value) {
        let at = self.field_pointer(p, offset);
        self.store(&ty, value, &at, 8);
    }
    pub(super) fn current_payer(&mut self) -> Value {
        self.load_native(&CONTEXT, CONTEXT_CURRENT_LIMIT, Type::Ptr)
    }
    pub(super) fn likely(&mut self, test: &Value, hot: Label, cold: Label) {
        self.effect(Operation::Conditional(test, hot, cold, true));
    }
    pub(super) fn cold_void(&mut self, name: &str, args: &[CArg]) {
        let attrs = CallAttrs {
            cold: true,
            ..CallAttrs::default()
        };
        self.runtime(name, Type::Void, args, &attrs);
    }
    fn branch_on_arc_replay(&mut self, inline: Label, replay: Label) {
        let n = self.counter();
        let gate = Label::new("arc_replay_gate", n);
        let events = self.load_native(&CONTEXT, CONTEXT_EVENTS, Type::Ptr);
        let no_task = self.cmp(Predicate::Eq, &Type::Ptr, &events, &Value::Null);
        self.likely(&no_task, inline, gate);
        self.block(gate);
        let at = self.field_pointer(&events, TASK_ARC_REPLAY);
        let flag = self.load(&Type::I8, &at, 1);
        let live = self.cmp(Predicate::Eq, &Type::I8, &flag, &int(0));
        self.likely(&live, inline, replay);
    }

    pub(super) fn initialize_arc(&mut self, node: &Value) {
        let n = self.counter();
        let inline = Label::new("inline_arc_init", n);
        let replay = Label::new("replay_arc_init", n);
        let done = Label::new("arc_init_done", n);
        self.branch_on_arc_replay(inline, replay);
        self.block(replay);
        self.cold_void(
            "hirsel_core_counted_init",
            &[ptr(node.clone()), zeroext(Type::I1, true)],
        );
        self.br(done);
        self.block(inline);
        // Arc counts are plain words: an Arc never leaves the thread running
        // its unit (CONC-2).
        self.store(&Type::I64, &int(1), node, 8);
        self.arc_event("hirsel_core_arc_initialized", &[]);
        self.br(done);
        self.block(done);
    }

    /// A scheduled Arc init, share or release folds posted credits and records
    /// its event. The hook runs only when the task's TASK_ARC_HOOKS byte is set
    /// or its payer's domain has posted credit; otherwise it would do neither.
    pub(super) fn arc_event(&mut self, name: &str, arguments: &[CArg]) {
        self.arc_hook(name, arguments, true);
    }
    /// An Arc unique test is a poll, so its hook always runs.
    pub(super) fn arc_poll_event(&mut self, name: &str, arguments: &[CArg]) {
        self.arc_hook(name, arguments, false);
    }
    fn arc_hook(&mut self, name: &str, arguments: &[CArg], gated: bool) {
        let n = self.counter();
        let at = self.field_pointer(&CONTEXT, CONTEXT_EVENTS);
        let fixed = Memory {
            invariant: self.facts,
            ..Memory::aligned(8)
        };
        let events = self.load_with(&Type::Ptr, &at, &fixed);
        let empty = self.cmp(Predicate::Eq, &Type::Ptr, &events, &Value::Null);
        let done = Label::new("arc_event_done", n);
        let call = Label::new("arc_event_call", n);
        if gated {
            let hooks = Label::new("arc_event_hooks", n);
            let credit = Label::new("arc_event_credit", n);
            self.likely(&empty, done, hooks);
            self.block(hooks);
            let at = self.field_pointer(&events, TASK_ARC_HOOKS);
            let flag = self.load(&Type::I8, &at, 1);
            let quiet = self.cmp(Predicate::Eq, &Type::I8, &flag, &int(0));
            self.likely(&quiet, credit, call);
            self.block(credit);
            // The payer and its domain may change between events; neither
            // load is invariant, and the pending byte is an Acquire atomic.
            let payer = self.current_payer();
            let domain = self.load_native(&payer, SLOT_DOMAIN, Type::Ptr);
            let at = self.field_pointer(&domain, DOMAIN_PENDING);
            let pending = Memory {
                ordering: Some(Ordering::Acquire),
                ..Memory::aligned(1)
            };
            let flag = self.load_with(&Type::I8, &at, &pending);
            let quiet = self.cmp(Predicate::Eq, &Type::I8, &flag, &int(0));
            self.likely(&quiet, done, call);
        } else {
            self.likely(&empty, done, call);
        }
        self.block(call);
        let mut args = vec![ptr(CONTEXT)];
        args.extend_from_slice(arguments);
        self.cold_void(name, &args);
        self.br(done);
        self.block(done);
    }
    pub(super) fn admission_probe(&mut self, charge: &Value) -> Value {
        if self.test_hooks {
            self.intrinsic(
                "hirsel_core_test_probe_admission",
                Type::I1,
                &[ptr(CONTEXT), word(charge.clone())],
            )
        } else {
            let _ = charge;
            true.into()
        }
    }
    fn admission_test(&mut self, payer: &Value, charge: &Value) -> (Value, Value) {
        let allowed = self.admission_probe(charge);
        let available = self.load_native(payer, LIMIT_AVAILABLE, Type::I64);
        let fits = self.cmp(Predicate::Ule, &Type::I64, charge, &available);
        let fits = if self.test_hooks {
            self.bin(Binary::And, &Type::I1, &fits, &allowed)
        } else {
            fits
        };
        (fits, available)
    }
    pub(super) fn admit_balance(&mut self, payer: &Value, charge: &Value, available: &Value) {
        self.debit_room(payer, charge, available);
        let outstanding = self.load_native(payer, LIMIT_OUTSTANDING, Type::I64);
        let used = self.bin(Binary::Add, &Type::I64, &outstanding, charge);
        self.store_native(payer, LIMIT_OUTSTANDING, Type::I64, &used);
        self.observe_admission();
    }
    fn debit_room(&mut self, payer: &Value, charge: &Value, available: &Value) {
        let left = self.bin(Binary::Sub, &Type::I64, available, charge);
        self.store_native(payer, LIMIT_AVAILABLE, Type::I64, &left);
    }
    fn observe_admission(&mut self) {
        if self.observe_charge {
            self.cold_void("hirsel_core_observe_charge", &[]);
        }
    }
    /// Empty scopes have no admission or external observer between entry/exit.
    /// Their reservation and refund cancel; the source Capacity check remains.
    pub(super) fn empty_bound(&mut self, charge: &Value) -> (Value, Value) {
        let payer = self.current_payer();
        let (fits, available) = self.admission_test(&payer, charge);
        let admitted = self.zext(&Type::I1, &fits, &Type::I8);
        let room = self.select(&Type::I64, &fits, &int(0), &available);
        (admitted, room)
    }
    pub(super) fn allocate_admitted(
        &mut self,
        payer: Value,
        bytes: Value,
        align: u64,
        kind: u8,
    ) -> Value {
        let name = "hirsel_core_alloc_admitted";
        let sig = FnSig {
            nonnull: self.facts,
            params: vec![
                Type::Ptr.into(),
                Type::Ptr.into(),
                Type::I64.into(),
                Type::I64.into(),
                Parameter::extended(Type::I8, Extension::Zero),
            ],
            ..FnSig::plain(Type::Ptr, [])
        };
        self.items.declare(name, sig.clone(), FnAttrs::default());
        self.call(
            &sig,
            &Value::global(name.to_owned()),
            &[CONTEXT, payer, bytes, int(align), int(kind)],
            &CallAttrs::default(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fast_allocate(
        &mut self,
        resource: bool,
        native_kind: u8,
        place: &Payer,
        charge: &Value,
        bytes: &Value,
        align: u64,
        overflow: Option<ValueId>,
    ) -> Option<(Value, Value)> {
        if resource {
            return None;
        }
        if let Payer::Region(region) = place {
            if overflow.is_some() {
                return None;
            }
            return Some(self.fast_region(region, charge, bytes, align));
        }
        let i64 = Type::I64;
        let n = self.counter();
        let payer = self.current_payer();
        let overflow = overflow.map_or(FALSE, |v| {
            let flag = self.numeric(v);
            self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
        });
        let eligible = if overflow == FALSE {
            true.into()
        } else {
            self.bin(Binary::Xor, &Type::I1, &overflow, &true.into())
        };
        let admit = Label::new("admit", n);
        let slow_label = Label::new("allocation_slow", n);
        self.likely(&eligible, admit, slow_label);
        self.block(admit);
        let available = self.load_native(&payer, LIMIT_AVAILABLE, Type::I64);
        // Unsigned charge-1 < room is exactly charge != 0 && charge <= room,
        // including room=MAX. Zero retains the runtime's payer pinning path.
        let last = self.bin(Binary::Sub, &i64, charge, &int(1));
        let fits = self.cmp(Predicate::Ult, &i64, &last, &available);
        // Real refusal uses the one runtime fallback, retaining Capacity events.
        // Probe only a fitting admission, so tests never count an attempt twice.
        let allocate = Label::new("allocate", n);
        let probe = Label::new("allocation_probe", n);
        let refused = Label::new("refused", n);
        let done = Label::new("allocation_done", n);
        self.likely(
            &fits,
            if self.test_hooks { probe } else { allocate },
            slow_label,
        );
        // Refusal injection exists only in the test engine. Production needs
        // neither its always-true branch nor its unreachable refusal join.
        if self.test_hooks {
            self.block(probe);
            let allowed = self.admission_probe(charge);
            self.likely(&allowed, allocate, refused);
        }
        self.block(allocate);
        self.admit_balance(&payer, charge, &available);
        let payload = self.allocate_admitted(payer, bytes.clone(), align, native_kind);
        self.br(done);
        if self.test_hooks {
            self.block(refused);
            self.br(done);
        }
        self.block(slow_label);
        let (slow, room) = self.allocation_cold(
            "hirsel_core_alloc",
            &[
                ptr(CONTEXT),
                ptr(Value::Null),
                word(bytes.clone()),
                word(int(align)),
                word(charge.clone()),
                zeroext(Type::I1, overflow),
                zeroext(Type::I8, int(native_kind)),
            ],
        );
        self.br(done);
        self.block(done);
        let mut pointers = vec![(payload, allocate), (slow, slow_label)];
        let mut rooms = vec![(int(0), allocate), (room, slow_label)];
        if self.test_hooks {
            pointers.push((Value::Null, refused));
            rooms.push((available, refused));
        }
        let pointer = self.emit(Operation::Phi(&Type::Ptr, &pointers));
        let room = self.emit(Operation::Phi(&i64, &rooms));
        Some((pointer, room))
    }
    /// Admit `charge` for the frame bytes at `frame` (`alloc .. frame`): the own
    /// allocation's inline admission with nothing allocated. A refusal, overflow
    /// or test probe takes `hirsel_core_frame_admit`, which keeps the Capacity event.
    /// Frame charges move only the room: no block names the record.
    pub(super) fn frame_admit(
        &mut self,
        frame: &Value,
        charge: &Value,
        overflow: Option<ValueId>,
    ) -> (Value, Value) {
        let i64 = Type::I64;
        let n = self.counter();
        let payer = self.current_payer();
        let overflow = overflow.map_or(FALSE, |v| {
            let flag = self.numeric(v);
            self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
        });
        let eligible = if overflow == FALSE {
            true.into()
        } else {
            self.bin(Binary::Xor, &Type::I1, &overflow, &true.into())
        };
        let admit = Label::new("frame_admit", n);
        let debit = Label::new("frame_debit", n);
        let slow = Label::new("frame_slow", n);
        let done = Label::new("frame_done", n);
        self.likely(&eligible, admit, slow);
        self.block(admit);
        let available = self.load_native(&payer, LIMIT_AVAILABLE, Type::I64);
        let last = self.bin(Binary::Sub, &i64, charge, &int(1));
        let fits = self.cmp(Predicate::Ult, &i64, &last, &available);
        self.likely(&fits, if self.test_hooks { slow } else { debit }, slow);
        self.block(debit);
        self.debit_room(&payer, charge, &available);
        self.observe_admission();
        self.br(done);
        self.block(slow);
        let (admitted, room) = self.allocation_cold(
            "hirsel_core_frame_admit",
            &[
                ptr(CONTEXT),
                word(charge.clone()),
                zeroext(Type::I1, overflow),
            ],
        );
        let present = self.cmp(Predicate::Ne, &Type::Ptr, &admitted, &Value::Null);
        let slow_frame = self.emit(Operation::Select(&Type::Ptr, &present, frame, &Value::Null));
        self.br(done);
        self.block(done);
        let pointer = self.emit(Operation::Phi(
            &Type::Ptr,
            &[(frame.clone(), debit), (slow_frame, slow)],
        ));
        let room = self.emit(Operation::Phi(&i64, &[(int(0), debit), (room, slow)]));
        (pointer, room)
    }
    fn fast_region(
        &mut self,
        slot: &Value,
        charge: &Value,
        bytes: &Value,
        align: u64,
    ) -> (Value, Value) {
        let (i1, i64) = (Type::I1, Type::I64);
        let n = self.counter();
        let open = Label::new("region_open", n);
        let slow_label = Label::new("region_slow", n);
        let region = &self.load(&Type::Ptr, slot, 8);
        let has_backing = self.cmp(Predicate::Ne, &Type::Ptr, region, &Value::Null);
        self.likely(&has_backing, open, slow_label);
        self.block(open);
        let payer = self.current_payer();
        let saved = self.load_native(region, REGION_PAYER, Type::Ptr);
        let accumulated = self.load_native(region, REGION_CHARGE, Type::I64);
        let same = self.cmp(Predicate::Eq, &Type::Ptr, &saved, &payer);
        // A fully trimmed entry no longer pins its payer. Pointer equality alone
        // would let a recycled slot masquerade as this run's live payer.
        let live = self.cmp(Predicate::Ne, &i64, &accumulated, &int(0));
        let positive = self.cmp(Predicate::Ne, &i64, charge, &int(0));
        let reusable = self.bin(Binary::And, &i1, &same, &live);
        let reusable = self.bin(Binary::And, &i1, &reusable, &positive);
        let admit = Label::new("region_admit", n);
        let bump = Label::new("region_bump", n);
        let refused = Label::new("region_refused", n);
        let store = Label::new("region_store", n);
        let refill_label = Label::new("region_refill", n);
        let done = Label::new("region_done", n);
        self.likely(&reusable, admit, slow_label);
        self.block(admit);
        let (fits, available) = self.admission_test(&payer, charge);
        self.likely(&fits, bump, refused);
        self.block(bump);
        self.admit_balance(&payer, charge, &available);
        let total = self.bin(Binary::Add, &i64, &accumulated, charge);
        self.store_native(region, REGION_CHARGE, Type::I64, &total);
        let stored_size = self.load_native(region, REGION_SIZE, Type::I64);
        let stored_alignment = self.load_native(region, REGION_ALIGNMENT, Type::I64);
        let same_size = self.cmp(Predicate::Eq, &i64, &stored_size, bytes);
        let same_align = self.cmp(Predicate::Eq, &i64, &stored_alignment, &int(align));
        let same_pool = self.bin(Binary::And, &i1, &same_size, &same_align);
        let cursor = self.load_native(region, REGION_CURSOR, Type::Ptr);
        let end = self.load_native(region, REGION_END, Type::Ptr);
        let stride = self.load_native(region, REGION_STRIDE, Type::I64);
        let start = self.cast(Cast::PtrToInt, &Type::Ptr, &cursor, &i64);
        let limit = self.cast(Cast::PtrToInt, &Type::Ptr, &end, &i64);
        let remaining = self.bin(Binary::Sub, &i64, &limit, &start);
        let room = self.cmp(Predicate::Ule, &i64, &stride, &remaining);
        let fits = self.bin(Binary::And, &i1, &same_pool, &room);
        self.likely(&fits, store, refill_label);
        self.block(store);
        let next = self.offset_by(&cursor, &stride);
        self.store_native(region, REGION_CURSOR, Type::Ptr, &next);
        let occupied = self.load_native(region, REGION_OCCUPIED, Type::I64);
        let occupied = self.bin(Binary::Add, &i64, &occupied, &stride);
        self.store_native(region, REGION_OCCUPIED, Type::I64, &occupied);
        self.br(done);
        self.block(refill_label);
        let refill = self.call_declared(
            "hirsel_core_region_refill",
            Type::Ptr,
            &[ptr(region.clone()), word(bytes.clone()), word(int(align))],
            FnAttrs::default(),
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        self.br(done);
        self.block(refused);
        self.br(done);
        self.block(slow_label);
        let (slow, slow_room) = self.allocation_cold(
            "hirsel_core_region_slot_alloc",
            &[
                ptr(CONTEXT),
                ptr(slot.clone()),
                word(bytes.clone()),
                word(int(align)),
                word(charge.clone()),
                zeroext(Type::I1, FALSE),
            ],
        );
        self.br(done);
        self.block(done);
        let pointer = self.emit(Operation::Phi(
            &Type::Ptr,
            &[
                (cursor, store),
                (refill, refill_label),
                (Value::Null, refused),
                (slow, slow_label),
            ],
        ));
        let room = self.emit(Operation::Phi(
            &i64,
            &[
                (int(0), store),
                (int(0), refill_label),
                (available, refused),
                (slow_room, slow_label),
            ],
        ));
        (pointer, room)
    }
    pub(super) fn free_block(&mut self, pointer: &Value, charge: &Value) {
        self.runtime_void(
            "hirsel_core_free_value",
            &[ptr(CONTEXT), ptr(pointer.clone()), word(charge.clone())],
        );
    }
    /// Share adds one holder in a single read-modify-write; a count that wraps
    /// past `2^64 - 1` stops the run (FAIL-15 CountOverflow) before any holder
    /// observes it, so the wrapped word is never read.
    pub(super) fn share_node(&mut self, node: &Value, atomic: bool) {
        let i64 = Type::I64;
        let n = self.counter();
        let stop = Label::new("count_stop", n);
        let retained = Label::new("retained", n);
        let done = Label::new("arc_share_done", n);
        if atomic {
            let inline = Label::new("inline_arc_share", n);
            let replay = Label::new("replay_arc_share", n);
            self.branch_on_arc_replay(inline, replay);
            self.block(replay);
            self.cold_void(
                "hirsel_core_share",
                &[ptr(node.clone()), zeroext(Type::I1, true)],
            );
            self.br(done);
            self.block(inline);
        }
        let count = self.load(&i64, node, 8);
        let next = self.bin(Binary::Add, &i64, &count, &int(1));
        self.store(&i64, &next, node, 8);
        let fits = self.cmp(Predicate::Ne, &i64, &next, &int(0));
        self.likely(&fits, retained, stop);
        self.block(stop);
        self.cold_void("hirsel_core_count_overflow", &[]);
        self.effect(Operation::Unreachable);
        self.block(retained);
        if atomic {
            self.arc_event("hirsel_core_arc_shared", &[ptr(node.clone())]);
            self.br(done);
            self.block(done);
        }
    }
    pub(super) fn unique_node(&mut self, node: &Value, atomic: bool) -> Value {
        if !atomic {
            let count = self.load(&Type::I64, node, 8);
            return self.cmp(Predicate::Eq, &Type::I64, &count, &int(1));
        }
        let n = self.counter();
        let inline = Label::new("inline_arc_unique", n);
        let replay = Label::new("replay_arc_unique", n);
        let observed = Label::new("inline_arc_observed", n);
        let done = Label::new("arc_unique_done", n);
        self.branch_on_arc_replay(inline, replay);
        self.block(replay);
        let runtime = self.runtime(
            "hirsel_core_try_unwrap",
            Type::I1,
            &[ptr(node.clone()), zeroext(Type::I1, true)],
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        self.br(done);
        self.block(inline);
        let count = self.load(&Type::I64, node, 8);
        let unique = self.cmp(Predicate::Eq, &Type::I64, &count, &int(1));
        self.arc_poll_event(
            "hirsel_core_arc_unwrapped",
            &[zeroext(Type::I1, unique.clone())],
        );
        self.br(observed);
        self.block(observed);
        self.br(done);
        self.block(done);
        self.emit(Operation::Phi(
            &Type::I1,
            &[(runtime, replay), (unique, observed)],
        ))
    }
    pub(super) fn decrement_node(&mut self, node: &Value, atomic: bool) -> Value {
        if atomic {
            let n = self.counter();
            let inline = Label::new("inline_arc_decrement", n);
            let replay = Label::new("replay_arc_decrement", n);
            let done = Label::new("arc_decrement_done", n);
            self.branch_on_arc_replay(inline, replay);
            self.block(replay);
            let runtime = self.runtime(
                "hirsel_core_arc_decrement_for_replay",
                Type::I1,
                &[ptr(node.clone())],
                &CallAttrs {
                    cold: true,
                    ..CallAttrs::default()
                },
            );
            self.br(done);
            self.block(inline);
            let last = self.plain_decrement(node);
            self.br(done);
            self.block(done);
            self.emit(Operation::Phi(
                &Type::I1,
                &[(runtime, replay), (last, inline)],
            ))
        } else {
            self.plain_decrement(node)
        }
    }
    fn plain_decrement(&mut self, node: &Value) -> Value {
        let i64 = Type::I64;
        let old = self.load_native(node, NODE_COUNT, Type::I64);
        let next = self.bin(Binary::Sub, &i64, &old, &int(1));
        self.store_native(node, NODE_COUNT, Type::I64, &next);
        self.cmp(Predicate::Eq, &i64, &next, &int(0))
    }
    /// Every release site keeps only the decrement, its branch and the
    /// non-last event guard. The last holder's cleanup is one cold call to the
    /// type's out-of-line `last` body, as Rust's `Arc::drop_slow` is.
    pub(super) fn release_fast(&mut self, node: &Value, last: &str, atomic: bool) {
        let n = self.counter();
        let is_last = self.decrement_node(node, atomic);
        let retained = self.bin(Binary::Xor, &Type::I1, &is_last, &true.into());
        let nonlast = Label::new("release_nonlast", n);
        let last_label = Label::new("release_last", n);
        let released = Label::new("released", n);
        self.likely(&retained, nonlast, last_label);
        self.block(nonlast);
        if atomic {
            self.arc_event("hirsel_core_arc_released", &[zeroext(Type::I1, false)]);
        }
        self.br(released);
        self.block(last_label);
        self.call(
            &release_last_signature(),
            &Value::global(last.to_owned()),
            &[CONTEXT, node.clone()],
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        self.br(released);
        self.block(released);
    }
    /// The body of a last release: the holder that took the count to zero
    /// reports the event, then frees.
    pub(super) fn release_last_body(
        &mut self,
        node: &Value,
        charge: u64,
        recipe: Option<&Value>,
        atomic: bool,
    ) {
        if atomic {
            self.arc_event("hirsel_core_arc_released", &[zeroext(Type::I1, true)]);
        }
        if let Some(recipe) = recipe {
            // A shareable payload holds no resource or finalizer, so its
            // release cannot fail and needs no site.
            let attrs = CallAttrs {
                cold: true,
                ..CallAttrs::default()
            };
            self.runtime(
                "hirsel_core_release",
                Type::I8,
                &[
                    ptr(CONTEXT),
                    ptr(node.clone()),
                    ptr(recipe.clone()),
                    word(int(0)),
                ],
                &attrs,
            );
        } else {
            self.free_block(node, &int(charge));
        }
    }
}
