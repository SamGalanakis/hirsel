//! A small sequence backing whose charge fits grows inline: admit, allocate,
//! copy, free. Every other case (possible native reuse, refusal,
//! overflow) shares one cold runtime call that makes the same decisions, as
//! Rust's `RawVec` keeps one non-generic `finish_grow` behind each element
//! type's `grow_one`. Zero-sized elements keep the inline replacement.
use super::memory_layout::*;
use super::storage::{Payer, zeroext};
use super::*;
use hirsel_core_layout::{FieldTy, field_size};

impl FunctionEmitter<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn grow_buffer(
        &mut self,
        edge: FieldTy,
        at: &Value,
        old: &Value,
        charge: &Value,
        size: &Value,
        align: u64,
        overflow: Option<ValueId>,
        capacity: &Value,
        moved: &Value,
        refund: &Value,
    ) -> (Value, Value) {
        let FieldTy::Seq(item) = &edge else {
            unreachable!("grow sequence")
        };
        let i64 = Type::I64;
        let stride = field_size(&self.module.types, self.recipes, item).0;
        let dispatch = stride != 0;
        let n = self.counter();
        let done = Label::new("grow_done", n);
        let refused_label = Label::new("grow_refused", n);
        let native_copy = Label::new("grow_native_copy", n);
        let native_refused = Label::new("grow_native_refused", n);
        if dispatch {
            let kind =
                match hirsel_core_layout::cleanup_class(&self.module.types, self.recipes, &edge) {
                    Some(hirsel_core_layout::Cleanup::Terminal) => NATIVE_PLAIN,
                    _ => NATIVE_GRAPH,
                };
            // Hot arm: a small backing whose charge fits is admitted, allocated,
            // copied and freed inline, exactly as an own allocation would be.
            let payer = self.current_payer();
            let cap = self.load_native(at, 16, Type::I64);
            let small = self.cmp(
                Predicate::Ult,
                &i64,
                &cap,
                &int(GROW_CALL_MIN_PAYLOAD_BYTES.div_ceil(stride)),
            );
            let mut short_bytes = None;
            let small = if stride == 1 && kind == NATIVE_PLAIN {
                let beyond_slab = self.cmp(
                    Predicate::Ugt,
                    &i64,
                    size,
                    &int(GROW_SLAB_MAX_PAYLOAD_BYTES),
                );
                if align == 8 {
                    let slab = self.bin(Binary::Xor, &Type::I1, &beyond_slab, &true.into());
                    let fits = self.bin(Binary::And, &Type::I1, &slab, &small);
                    let exact = self.cmp(Predicate::Eq, &i64, size, capacity);
                    short_bytes = Some(self.bin(Binary::And, &Type::I1, &fits, &exact));
                }
                self.bin(Binary::And, &Type::I1, &small, &beyond_slab)
            } else {
                small
            };
            let over = overflow.map_or(FALSE, |v| {
                let flag = self.numeric(v);
                self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
            });
            let eligible = if over == FALSE {
                small
            } else {
                let checked = self.bin(Binary::Xor, &Type::I1, &over, &true.into());
                self.bin(Binary::And, &Type::I1, &small, &checked)
            };
            let admit = Label::new("grow_admit", n);
            let cold = Label::new("grow_cold", n);
            let allocate = Label::new("grow_allocate", n);
            let probe = Label::new("grow_probe", n);
            let probe_refused = Label::new("grow_probe_refused", n);
            let copied = Label::new("grow_copied", n);
            let refused_done = Label::new("grow_refused_done", n);
            let eligible = self.inline_storage(eligible);
            self.likely(&eligible, admit, cold);
            self.block(admit);
            let available = self.load_native(&payer, LIMIT_AVAILABLE, Type::I64);
            // Unsigned charge-1 < room is exactly charge != 0 && charge <= room.
            let last = self.bin(Binary::Sub, &i64, charge, &int(1));
            let fits = self.cmp(Predicate::Ult, &i64, &last, &available);
            self.likely(&fits, if self.test_hooks { probe } else { allocate }, cold);
            if self.test_hooks {
                self.block(probe);
                let allowed = self.admission_probe(charge);
                self.likely(&allowed, allocate, probe_refused);
            }
            self.block(allocate);
            self.admit_balance(&payer, charge, &available);
            let fresh = self.allocate_admitted(payer, size.clone(), align, kind);
            if self.facts {
                let distinct = self.cmp(Predicate::Ne, &Type::Ptr, &fresh, old);
                self.runtime_void("llvm.assume", &[arg(Type::I1, distinct)]);
            }
            self.runtime_void(
                "llvm.memcpy.p0.p0.i64",
                &[
                    ptr(fresh.clone()),
                    ptr(old.clone()),
                    word(moved.clone()),
                    arg(Type::I1, FALSE),
                ],
            );
            self.store_native(at, 0, Type::Ptr, &fresh);
            self.store_native(at, 16, Type::I64, capacity);
            self.free_block(old, refund);
            self.br(copied);
            self.block(copied);
            self.br(done);
            if self.test_hooks {
                self.block(probe_refused);
                self.br(refused_label);
            }
            // Cold arm: large backings (native reuse), refusal, overflow and
            // zero charges share one runtime call, which makes the same
            // admission and threshold decisions.
            self.block(cold);
            // Cold growth reads only data and capacity. Keep the source
            // carrier's address out of the call so local homes promote.
            let shadow = self.temp(Type::words(3));
            self.runtime_void("llvm.lifetime.start.p0", &[ptr(shadow.clone())]);
            self.store_native(&shadow, 0, Type::Ptr, old);
            let grown = Type::structure([Type::Ptr, Type::I64]);
            let specialized = short_bytes.map(|short| {
                let generic = Label::new("grow_generic", n);
                let short_bytes = Label::new("grow_short_bytes", n);
                let joined = Label::new("grow_native_result", n);
                let no_overflow = self.bin(Binary::Xor, &Type::I1, &over, &true.into());
                let eligible = self.bin(Binary::And, &Type::I1, &short, &no_overflow);
                self.likely(&eligible, short_bytes, generic);
                self.block(short_bytes);
                let pair = self.call_declared(
                    "hirsel_core_grow_small_bytes",
                    grown.clone(),
                    &[
                        ptr(CONTEXT),
                        ptr(shadow.clone()),
                        word(capacity.clone()),
                        word(charge.clone()),
                        word(moved.clone()),
                        word(refund.clone()),
                    ],
                    FnAttrs::default(),
                    &CallAttrs::default(),
                );
                self.br(joined);
                self.block(generic);
                (generic, short_bytes, joined, pair)
            });
            self.store_native(&shadow, 16, Type::I64, &cap);
            let pair = self.call_declared(
                "hirsel_core_grow_cold",
                grown.clone(),
                &[
                    ptr(CONTEXT),
                    ptr(shadow.clone()),
                    word(size.clone()),
                    word(int(align)),
                    word(int(stride)),
                    word(charge.clone()),
                    word(capacity.clone()),
                    word(moved.clone()),
                    word(refund.clone()),
                    zeroext(Type::I1, over),
                    zeroext(Type::I8, int(kind)),
                ],
                FnAttrs::default(),
                &CallAttrs {
                    cold: true,
                    ..CallAttrs::default()
                },
            );
            let pair = if let Some((generic, short_bytes, joined, fast)) = specialized {
                self.br(joined);
                self.block(joined);
                self.emit(Operation::Phi(
                    &grown,
                    &[(pair, generic), (fast, short_bytes)],
                ))
            } else {
                pair
            };
            let block = self.extract(&grown, &pair, 0);
            let room = self.extract(&grown, &pair, 1);
            let present = self.cmp(Predicate::Ne, &Type::Ptr, &block, &Value::Null);
            self.likely(&present, native_copy, native_refused);
            self.block(native_copy);
            // Read the helper's carrier word to avoid threading the requested
            // operand through loop-carried values; publish #93's fact on success.
            let grown_capacity = self.load_native(&shadow, 16, Type::I64);
            if self.facts {
                let exact = self.cmp(Predicate::Eq, &i64, &grown_capacity, capacity);
                self.runtime_void("llvm.assume", &[arg(Type::I1, exact)]);
            }
            self.store_native(at, 0, Type::Ptr, &block);
            self.store_native(at, 16, Type::I64, &grown_capacity);
            self.runtime_void("llvm.lifetime.end.p0", &[ptr(shadow.clone())]);
            self.br(done);
            self.block(native_refused);
            self.runtime_void("llvm.lifetime.end.p0", &[ptr(shadow)]);
            self.br(refused_label);
            self.block(refused_label);
            let mut rooms = vec![(room, native_refused)];
            if self.test_hooks {
                rooms.push((available, probe_refused));
            }
            let refused_room = self.emit(Operation::Phi(&i64, &rooms));
            self.br(refused_done);
            self.block(refused_done);
            self.br(done);
            self.block(done);
            let block = self.emit(Operation::Phi(
                &Type::Ptr,
                &[
                    (fresh, copied),
                    (block, native_copy),
                    (Value::Null, refused_done),
                ],
            ));
            let room = self.emit(Operation::Phi(
                &i64,
                &[
                    (int(0), copied),
                    (int(0), native_copy),
                    (refused_room, refused_done),
                ],
            ));
            return (block, room);
        }
        let (block, room) = self.allocate_raw(
            &AllocKind::own(edge),
            &Payer::Current,
            charge,
            size,
            align,
            overflow,
        );
        let allocated = Label::new("grow_allocated", n);
        let copy = Label::new("grow_copy", n);
        let copied = Label::new("grow_copied", n);
        let refused_done = Label::new("grow_refused_done", n);
        self.br(allocated);
        self.block(allocated);
        let present = self.cmp(Predicate::Ne, &Type::Ptr, &block, &Value::Null);
        self.cond(&present, copy, refused_label);
        self.block(copy);
        if self.facts {
            let distinct = self.cmp(Predicate::Ne, &Type::Ptr, &block, old);
            self.runtime_void("llvm.assume", &[arg(Type::I1, distinct)]);
        }
        self.runtime_void(
            "llvm.memcpy.p0.p0.i64",
            &[
                ptr(block.clone()),
                ptr(old.clone()),
                word(moved.clone()),
                arg(Type::I1, FALSE),
            ],
        );
        self.store_native(at, 0, Type::Ptr, &block);
        self.store_native(at, 16, Type::I64, capacity);
        self.free_block(old, refund);
        self.br(copied);
        self.block(copied);
        self.br(done);
        self.block(refused_label);
        let room = self.emit(Operation::Phi(&i64, &[(room, allocated)]));
        self.br(refused_done);
        self.block(refused_done);
        self.br(done);
        self.block(done);
        let block = self.emit(Operation::Phi(
            &Type::Ptr,
            &[(block, copied), (Value::Null, refused_done)],
        ));
        let room = self.emit(Operation::Phi(
            &i64,
            &[(int(0), copied), (room, refused_done)],
        ));
        (block, room)
    }
}
