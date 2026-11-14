//! Frozen runtime-prefix fast paths. Only allocation/refill, nonlocal payers,
//! compound resource construction and last-holder cleanup cross the C ABI.
// The shared abi_layout.rs documents the runtime raw-payer equality sequence:
// refund current directly on exact word equality, otherwise resolve the original
// payer. Outstanding bytes pin the original slot across exit/re-entry. Native
// tag bits may cause a safe slow-path false negative. No offset changes.
use super::memory_layout::*;
use super::storage::{Payer, zeroext};
use super::*;

/// The negated byte offset of a prefix field behind a payload.
fn back(bytes: usize) -> i64 {
    -(bytes as i64)
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
    fn current_payer(&mut self) -> Value {
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
    /// Every inline Arc operation has one immutable context gate. No sink pays
    /// one test; a recorded run calls the context-first hook on the cold branch.
    pub(super) fn arc_event(&mut self, name: &str, arguments: &[CArg]) {
        let n = self.counter();
        let at = self.field_pointer(&CONTEXT, CONTEXT_EVENTS);
        let memory = Memory {
            invariant: self.facts,
            ..Memory::aligned(8)
        };
        let events = self.load_with(&Type::I64, &at, &memory);
        let empty = self.cmp(Predicate::Eq, &Type::I64, &events, &int(0));
        let done = Label::new("arc_event_done", n);
        let call = Label::new("arc_event_call", n);
        self.likely(&empty, done, call);
        self.block(call);
        let mut args = vec![ptr(CONTEXT)];
        args.extend_from_slice(arguments);
        self.cold_void(name, &args);
        self.br(done);
        self.block(done);
    }
    fn admission_probe(&mut self, charge: &Value) -> Value {
        #[cfg(test)]
        {
            self.intrinsic(
                "hirsel_core_test_probe_admission",
                Type::I1,
                &[ptr(CONTEXT), word(charge.clone())],
            )
        }
        #[cfg(not(test))]
        {
            let _ = charge;
            true.into()
        }
    }
    fn admission_test(&mut self, payer: &Value, charge: &Value) -> (Value, Value) {
        let allowed = self.admission_probe(charge);
        let available = self.load_native(payer, LIMIT_AVAILABLE, Type::I64);
        let fits = self.cmp(Predicate::Ule, &Type::I64, charge, &available);
        let fits = if cfg!(test) {
            self.bin(Binary::And, &Type::I1, &fits, &allowed)
        } else {
            fits
        };
        (fits, available)
    }
    fn admit_balance(&mut self, payer: &Value, charge: &Value, available: &Value) {
        let left = self.bin(Binary::Sub, &Type::I64, available, charge);
        self.store_native(payer, LIMIT_AVAILABLE, Type::I64, &left);
        let outstanding = self.load_native(payer, LIMIT_OUTSTANDING, Type::I64);
        let used = self.bin(Binary::Add, &Type::I64, &outstanding, charge);
        self.store_native(payer, LIMIT_OUTSTANDING, Type::I64, &used);
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
    /// A native allocator entry: it returns a base or stops.
    fn native_alloc(&mut self, name: &str, args: &[Value]) -> Value {
        let sig = FnSig {
            nonnull: self.facts,
            ..FnSig::plain(Type::Ptr, args.iter().map(|_| Type::I64))
        };
        self.items.declare(name, sig.clone(), FnAttrs::default());
        self.call(
            &sig,
            &Value::global(name.to_owned()),
            args,
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
        owned_like: bool,
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
        if align > 512 {
            return None;
        }
        let i64 = Type::I64;
        let prefix = own_prefix(native_kind) as u64;
        let alignment = align.max(8);
        let offset = prefix.next_multiple_of(alignment);
        let maximum = ((i64::MAX as u64) & !(alignment - 1)) - offset;
        let n = self.counter();
        let payer = self.current_payer();
        let like = match place {
            Payer::Like(old) => old.clone(),
            _ => Value::Null,
        };
        let overflow = overflow.map_or(FALSE, |v| {
            let flag = self.numeric(v);
            self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
        });
        let sized = self.cmp(Predicate::Ule, &i64, bytes, &int(maximum));
        let eligible = if overflow == FALSE {
            sized
        } else {
            let exact = self.bin(Binary::Xor, &Type::I1, &overflow, &true.into());
            self.bin(Binary::And, &Type::I1, &sized, &exact)
        };
        let admit = Label::new("admit", n);
        let slow_label = Label::new("allocation_slow", n);
        let payer_label = Label::new("allocation_payer", n);
        self.likely(
            &eligible,
            if matches!(place, Payer::Like(_)) {
                payer_label
            } else {
                admit
            },
            slow_label,
        );
        if matches!(place, Payer::Like(_)) {
            self.block(payer_label);
            if !owned_like {
                let empty = self.cmp(Predicate::Eq, &Type::Ptr, &like, &Value::Null);
                let header = Label::new("allocation_header", n);
                self.likely(&empty, admit, header);
                self.block(header);
            }
            let header = self.offset(&like, back(OWN_PAYER_BACK));
            let raw = self.load(&i64, &header, 8);
            let original = self.bin(Binary::And, &i64, &raw, &int(!(OWN_TAG_MASK as u64)));
            let current = self.cast(Cast::PtrToInt, &Type::Ptr, &payer, &i64);
            let same = self.cmp(Predicate::Eq, &i64, &original, &current);
            self.likely(&same, admit, slow_label);
        }
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
        self.likely(&fits, if cfg!(test) { probe } else { allocate }, slow_label);
        // Refusal injection exists only in the test engine. Production needs
        // neither its always-true branch nor its unreachable refusal join.
        if cfg!(test) {
            self.block(probe);
            let allowed = self.admission_probe(charge);
            self.likely(&allowed, allocate, refused);
        }
        self.block(allocate);
        self.admit_balance(&payer, charge, &available);
        let zero = self.cmp(Predicate::Eq, &i64, bytes, &int(0));
        let nonzero = self.select(&i64, &zero, &int(1), bytes);
        let native_bytes = self.bin(Binary::Add, &i64, &nonzero, &int(offset));
        let base = if alignment <= 16 {
            self.native_alloc("hirsel_core_native_alloc_ordinary", &[native_bytes])
        } else {
            self.native_alloc("hirsel_core_native_alloc", &[native_bytes, int(alignment)])
        };
        // The native allocator returns a base or stops. The admitted native
        // extent contains this payload offset; it cannot yield a null payload.
        let payload = self.gep(&Type::I8, &base, &int(offset), self.facts);
        let tag = ((alignment.trailing_zeros() - 3) << 2) | u32::from(native_kind);
        let tagged = self.offset(&payer, tag);
        let header = self.offset(&payload, back(OWN_PAYER_BACK));
        self.store(&Type::Ptr, &tagged, &header, 8);
        self.br(done);
        if cfg!(test) {
            self.block(refused);
            self.br(done);
        }
        self.block(slow_label);
        let (slow, room) = self.allocation_cold(
            "hirsel_core_alloc",
            &[
                ptr(CONTEXT),
                ptr(like),
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
        if cfg!(test) {
            pointers.push((Value::Null, refused));
            rooms.push((available, refused));
        }
        let pointer = self.emit(Operation::Phi(&Type::Ptr, &pointers));
        let room = self.emit(Operation::Phi(&i64, &rooms));
        Some((pointer, room))
    }
    fn fast_region(
        &mut self,
        region: &Value,
        charge: &Value,
        bytes: &Value,
        align: u64,
    ) -> (Value, Value) {
        let (i1, i64) = (Type::I1, Type::I64);
        let n = self.counter();
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
        let slow_label = Label::new("region_slow", n);
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
        let cursor = self.load_native(region, REGION_CURSOR, Type::Ptr);
        let end = self.load_native(region, REGION_END, Type::Ptr);
        let address = self.cast(Cast::PtrToInt, &Type::Ptr, &cursor, &i64);
        let rounded = self.bin(Binary::Add, &i64, &address, &int(align - 1));
        let start = self.bin(Binary::And, &i64, &rounded, &int(-i128::from(align)));
        let offset = self.bin(Binary::Sub, &i64, &start, &address);
        let result = self.offset_by(&cursor, &offset);
        let zero = self.cmp(Predicate::Eq, &i64, bytes, &int(0));
        let size = self.select(&i64, &zero, &int(1), bytes);
        let next = self.bin(Binary::Add, &i64, &start, &size);
        let wrapped = self.cmp(Predicate::Uge, &i64, &next, &start);
        let limit = self.cast(Cast::PtrToInt, &Type::Ptr, &end, &i64);
        let room = self.cmp(Predicate::Ule, &i64, &next, &limit);
        let fits = self.bin(Binary::And, &i1, &room, &wrapped);
        self.likely(&fits, store, refill_label);
        self.block(store);
        let next_pointer = self.offset_by(&result, &size);
        self.store_native(region, REGION_CURSOR, Type::Ptr, &next_pointer);
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
            "hirsel_core_region_alloc",
            &[
                ptr(CONTEXT),
                ptr(region.clone()),
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
                (result, store),
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
    /// Return `charge` to the current payer's balance.
    fn refund(&mut self, current: &Value, charge: &Value) {
        let i64 = Type::I64;
        let available = self.load_native(current, LIMIT_AVAILABLE, Type::I64);
        let restored = self.bin(Binary::Add, &i64, &available, charge);
        self.store_native(current, LIMIT_AVAILABLE, Type::I64, &restored);
        let outstanding = self.load_native(current, LIMIT_OUTSTANDING, Type::I64);
        let remaining = self.bin(Binary::Sub, &i64, &outstanding, charge);
        self.store_native(current, LIMIT_OUTSTANDING, Type::I64, &remaining);
    }
    pub(super) fn free_block(&mut self, pointer: &Value, charge: &Value) {
        let (i1, i64) = (Type::I1, Type::I64);
        let n = self.counter();
        let at = self.offset(pointer, back(OWN_PAYER_BACK));
        let tagged = self.load(&Type::Ptr, &at, 8);
        let word_value = self.cast(Cast::PtrToInt, &Type::Ptr, &tagged, &i64);
        let current = self.current_payer();
        let current_word = self.cast(Cast::PtrToInt, &Type::Ptr, &current, &i64);
        // Exact raw equality with the canonical current pointer, optionally its
        // ordinary GRAPH tag, proves both local custody and the native layout.
        let tag = self.bin(Binary::Xor, &i64, &word_value, &current_word);
        let ordinary = self.cmp(Predicate::Ule, &i64, &tag, &int(OWN_ORDINARY_MAX_TAG as u64));
        let positive = self.cmp(Predicate::Ne, &i64, charge, &int(0));
        let fast = self.bin(Binary::And, &i1, &ordinary, &positive);
        let local = Label::new("free_local", n);
        let slow = Label::new("free_slow", n);
        let fixed_label = Label::new("free_static", n);
        let generic = Label::new("free_generic", n);
        let freed = Label::new("freed", n);
        self.likely(&fast, local, slow);
        self.block(local);
        self.refund(&current, charge);
        let plain = self.cmp(Predicate::Eq, &i64, &tag, &int(OWN_ORDINARY_PLAIN_TAG as u64));
        let offset = self.select(
            &i64,
            &plain,
            &int(PLAIN_PREFIX_BYTES as u64),
            &int(GRAPH_PREFIX_BYTES as u64),
        );
        let neg = self.bin(Binary::Sub, &i64, &int(0), &offset);
        let base = self.offset_by(pointer, &neg);
        self.runtime_void("hirsel_core_native_free", &[ptr(base)]);
        self.br(freed);
        self.block(slow);
        let fixed = self.cmp(Predicate::Eq, &i64, &tag, &int(NATIVE_STATIC));
        let fixed = self.bin(Binary::And, &i1, &fixed, &positive);
        self.cond(&fixed, fixed_label, generic);
        self.block(fixed_label);
        self.refund(&current, charge);
        let base = self.offset(pointer, back(STATIC_PREFIX_BYTES));
        self.runtime_void("hirsel_core_native_free", &[ptr(base)]);
        self.br(freed);
        self.block(generic);
        self.cold_void(
            "hirsel_core_free_value",
            &[ptr(pointer.clone()), word(charge.clone())],
        );
        self.br(freed);
        self.block(freed);
    }
    pub(super) fn share_node(&mut self, node: &Value, atomic: bool) {
        let i64 = Type::I64;
        let n = self.counter();
        let retain = Label::new("retain", n);
        let store = Label::new("retain_store", n);
        let stop = Label::new("count_stop", n);
        let retained = Label::new("retained", n);
        self.br(retain);
        self.block(retain);
        let memory = Memory {
            ordering: atomic.then_some(Ordering::Monotonic),
            ..Memory::aligned(8)
        };
        let count = self.load_with(&i64, node, &memory);
        let fits = self.cmp(Predicate::Ne, &i64, &count, &int(-1));
        self.likely(&fits, store, stop);
        self.block(stop);
        self.cold_void(
            "hirsel_core_share",
            &[ptr(node.clone()), zeroext(Type::I1, atomic)],
        );
        self.effect(Operation::Unreachable);
        self.block(store);
        let next = self.bin(Binary::Add, &i64, &count, &int(1));
        if atomic {
            let exchange = self.emit(Operation::CompareExchange(node, &i64, &count, &next));
            let changed = self.extract(&Type::structure([Type::I64, Type::I1]), &exchange, 1);
            self.likely(&changed, retained, retain);
        } else {
            self.store(&i64, &next, node, 8);
            self.br(retained);
        }
        self.block(retained);
        if atomic {
            self.arc_event("hirsel_core_arc_shared", &[ptr(node.clone())]);
        }
    }
    pub(super) fn unique_node(&mut self, node: &Value, atomic: bool) -> Value {
        let memory = Memory {
            ordering: atomic.then_some(Ordering::Acquire),
            ..Memory::aligned(8)
        };
        let count = self.load_with(&Type::I64, node, &memory);
        let unique = self.cmp(Predicate::Eq, &Type::I64, &count, &int(1));
        if atomic {
            self.arc_event(
                "hirsel_core_arc_unwrapped",
                &[zeroext(Type::I1, unique.clone())],
            );
        }
        unique
    }
    pub(super) fn decrement_node(&mut self, node: &Value, atomic: bool) -> Value {
        let i64 = Type::I64;
        if atomic {
            let old = self.emit(Operation::AtomicSub(node, &i64, &int(1)));
            self.cmp(Predicate::Eq, &i64, &old, &int(1))
        } else {
            let old = self.load_native(node, NODE_COUNT, Type::I64);
            let next = self.bin(Binary::Sub, &i64, &old, &int(1));
            self.store_native(node, NODE_COUNT, Type::I64, &next);
            self.cmp(Predicate::Eq, &i64, &next, &int(0))
        }
    }
    pub(super) fn release_fast(
        &mut self,
        node: &Value,
        charge: u64,
        recipe: Option<&Value>,
        atomic: bool,
    ) {
        let n = self.counter();
        let last = self.decrement_node(node, atomic);
        let retained = self.bin(Binary::Xor, &Type::I1, &last, &true.into());
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
        if atomic {
            self.effect(Operation::Fence(Ordering::Acquire));
            self.arc_event("hirsel_core_arc_released", &[zeroext(Type::I1, true)]);
        }
        if let Some(recipe) = recipe {
            self.cold_void(
                "hirsel_core_release_static_last",
                &[ptr(CONTEXT), ptr(node.clone()), ptr(recipe.clone())],
            );
        } else {
            self.free_block(node, &int(charge));
        }
        self.br(released);
        self.block(released);
    }
}
