//! One threshold test and the classic triple per site. Large buffers call the
//! shared runtime replacement; no accounting/realloc implementation is cloned.
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
        fail: Option<BlockId>,
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
        let native = if dispatch {
            let cap = self.load_native(at, 16, Type::I64);
            let large = self.cmp(
                Predicate::Uge,
                &i64,
                &cap,
                &int(GROW_REALLOC_MIN_PAYLOAD_BYTES.div_ceil(stride)),
            );
            let native_label = Label::new("grow_native", n);
            let classic = Label::new("grow_classic", n);
            self.cond(&large, native_label, classic);
            self.block(native_label);
            let kind =
                match hirsel_core_layout::cleanup_class(&self.module.types, self.recipes, &edge) {
                    Some(hirsel_core_layout::Cleanup::Terminal) => NATIVE_PLAIN,
                    _ => NATIVE_GRAPH,
                };
            let over = overflow.map_or(FALSE, |v| {
                let flag = self.numeric(v);
                self.cmp(Predicate::Ne, &Type::I8, &flag, &int(0))
            });
            // The runtime borrows only this call-lifetime carrier. Keep the
            // source carrier's address out of the call so local homes promote.
            let shadow = self.temp(Type::words(3));
            self.runtime_void("llvm.lifetime.start.p0", &[ptr(shadow.clone())]);
            let length = self.load_native(at, 8, Type::I64);
            self.store_native(&shadow, 0, Type::Ptr, old);
            self.store_native(&shadow, 8, Type::I64, &length);
            self.store_native(&shadow, 16, Type::I64, &cap);
            let grown = Type::structure([Type::Ptr, Type::I64]);
            let pair = self.intrinsic(
                "hirsel_core_grow",
                grown.clone(),
                &[
                    ptr(CONTEXT),
                    ptr(shadow.clone()),
                    word(size.clone()),
                    word(int(align)),
                    word(charge.clone()),
                    word(capacity.clone()),
                    word(moved.clone()),
                    word(refund.clone()),
                    zeroext(Type::I1, over),
                    zeroext(Type::I8, int(kind)),
                ],
            );
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
            self.block(classic);
            Some((block, room))
        } else {
            None
        };
        let (block, room) = self.allocate_raw(
            &AllocKind::Own { edge },
            &Payer::Like(old.clone()),
            charge,
            size,
            align,
            overflow,
            dispatch,
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
        let mut refused = vec![(room, allocated)];
        if let Some((_, room)) = &native {
            refused.push((room.clone(), native_refused));
        }
        let room = self.emit(Operation::Phi(&i64, &refused));
        let status = self.call_declared(
            "hirsel_core_allocation_status",
            Type::I8,
            &[ptr(CONTEXT), ptr(Value::Null)],
            FnAttrs::default(),
            &CallAttrs {
                cold: true,
                ..CallAttrs::default()
            },
        );
        let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
        self.branch_failure(&failed, fail, Some(6));
        self.br(refused_done);
        self.block(refused_done);
        self.br(done);
        self.block(done);
        let mut pointers = vec![(block, copied), (Value::Null, refused_done)];
        let mut rooms = vec![(int(0), copied), (room, refused_done)];
        if let Some((block, _)) = native {
            pointers.push((block, native_copy));
            rooms.push((int(0), native_copy));
        }
        let block = self.emit(Operation::Phi(&Type::Ptr, &pointers));
        let room = self.emit(Operation::Phi(&i64, &rooms));
        (block, room)
    }
}
