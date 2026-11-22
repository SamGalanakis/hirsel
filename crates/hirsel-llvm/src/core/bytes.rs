//! Expose equal-length comparison separately so an equality user can discard
//! the ordering-only prefix work after LLVM inlines this total bytewise order.
use super::*;

/// Fact attributes of the read-only byte helpers.
fn read_only(facts: bool) -> FnAttrs {
    FnAttrs {
        nounwind: facts,
        argmem_read: facts,
        ..FnAttrs::default()
    }
}
fn memcmp() -> FnSig {
    FnSig::plain(Type::I32, [Type::Ptr, Type::Ptr, Type::I64])
}
fn compare_bytes_signature() -> FnSig {
    FnSig {
        extension: Extension::Sign,
        ..FnSig::plain(Type::I8, [Type::Ptr, Type::I64, Type::Ptr, Type::I64])
    }
}

impl FunctionEmitter<'_> {
    pub(super) fn map_hash(&mut self, symbol: &str, arguments: &[CArg]) -> Vec<Value> {
        vec![self.call_declared(
            symbol,
            Type::I64,
            arguments,
            read_only(self.facts),
            &CallAttrs::default(),
        )]
    }

    pub(super) fn map_hash_word(&mut self, word: &Value, key: usize) -> Value {
        // CAP-8 prepares the lanes before the task enters generated code.
        // The lanes stay immutable throughout that task.
        let memory = Memory {
            invariant: self.facts,
            ..Memory::aligned(8)
        };
        let mut lanes = [0, 8, 16, 24].map(|offset| {
            let at = self.field_pointer(&CONTEXT, key + offset);
            self.load_with(&Type::I64, &at, &memory)
        });
        // One eight-byte scalar word, followed by the SipHash length word.
        for message in [word.clone(), int(8_i128 << 56)] {
            lanes[3] = self.bin(Binary::Xor, &Type::I64, &lanes[3], &message);
            self.sip_round(&mut lanes);
            lanes[0] = self.bin(Binary::Xor, &Type::I64, &lanes[0], &message);
        }
        lanes[2] = self.bin(Binary::Xor, &Type::I64, &lanes[2], &int(0xff));
        for _ in 0..3 {
            self.sip_round(&mut lanes);
        }
        let [v0, v1, v2, v3] = lanes;
        let left = self.bin(Binary::Xor, &Type::I64, &v0, &v1);
        let right = self.bin(Binary::Xor, &Type::I64, &v2, &v3);
        self.bin(Binary::Xor, &Type::I64, &left, &right)
    }

    fn map_fast_salt(&mut self, index: usize) -> Value {
        let at = self.field_pointer(&CONTEXT, memory_layout::CONTEXT_MAP_FAST_SALT + index * 8);
        let memory = Memory {
            invariant: self.facts,
            ..Memory::aligned(8)
        };
        self.load_with(&Type::I64, &at, &memory)
    }

    fn map_product_halves(&mut self, a: &Value, b: &Value) -> [Value; 2] {
        let wide = Type::Int(128);
        let a = self.zext(&Type::I64, a, &wide);
        let b = self.zext(&Type::I64, b, &wide);
        let product = self.bin(Binary::Mul, &wide, &a, &b);
        let low = self.cast(Cast::Trunc, &wide, &product, &Type::I64);
        let high = self.bin(Binary::LShr, &wide, &product, &int(64));
        let high = self.cast(Cast::Trunc, &wide, &high, &Type::I64);
        [low, high]
    }

    fn map_fold(&mut self, a: &Value, b: &Value) -> Value {
        let [low, high] = self.map_product_halves(a, b);
        self.bin(Binary::Xor, &Type::I64, &low, &high)
    }

    pub(super) fn map_place_word(&mut self, word: &Value) -> Value {
        let salt = self.map_fast_salt(0);
        let mixed = self.bin(Binary::Xor, &Type::I64, word, &salt);
        self.map_fold(&mixed, &int(hirsel_core_layout::map_index::MIX))
    }

    pub(super) fn map_place_bytes(&mut self, data: &Value, len: &Value) -> Value {
        let u = Type::I64;
        let s0 = self.map_fast_salt(0);
        let s1 = self.map_fast_salt(1);
        let early = self.current.expect("a runtime operation is in a block");
        let id = self.counter();
        let head = Label::new("map_bytes_head", id);
        let chunk = Label::new("map_bytes_chunk", id);
        let tail = Label::new("map_bytes_tail", id);
        let next_data = self.fresh();
        let next_len = self.fresh();
        let next_z = self.fresh();
        self.br(head);
        self.block(head);
        let p = self.emit(Operation::Phi(
            &Type::Ptr,
            &[(data.clone(), early), (next_data.into(), chunk)],
        ));
        let n = self.emit(Operation::Phi(
            &u,
            &[(len.clone(), early), (next_len.into(), chunk)],
        ));
        let z = self.emit(Operation::Phi(&u, &[(s0, early), (next_z.into(), chunk)]));
        let more = self.cmp(Predicate::Ugt, &u, &n, &int(16));
        self.cond(&more, chunk, tail);
        self.block(chunk);
        let low = self.load(&u, &p, 1);
        let high_at = self.offset(&p, 8);
        let high = self.load(&u, &high_at, 1);
        let a = self.bin(Binary::Xor, &u, &z, &low);
        let b = self.bin(Binary::Xor, &u, &s1, &high);
        let [low, high] = self.map_product_halves(&a, &b);
        self.emit_to(next_z, Operation::Binary(Binary::Xor, &u, &low, &high));
        self.emit_to(
            next_data,
            Operation::Gep {
                ty: &Type::I8,
                base: &p,
                indices: &[(Type::I64, int(16))],
                inbounds: false,
            },
        );
        self.emit_to(next_len, Operation::Binary(Binary::Sub, &u, &n, &int(16)));
        self.br(head);
        self.block(tail);

        let above_eight = self.cmp(Predicate::Ugt, &u, &n, &int(8));
        let low_len = self.select(&u, &above_eight, &int(8), &n);
        let w0 = self.map_place_tail(&p, &low_len);
        let high_at = self.offset(&p, 8);
        let remaining = self.bin(Binary::Sub, &u, &n, &int(8));
        let high_len = self.select(&u, &above_eight, &remaining, &int(0));
        let w1 = self.map_place_tail(&high_at, &high_len);
        let a = self.bin(Binary::Xor, &u, &z, &w0);
        let b = self.bin(Binary::Xor, &u, &s1, &w1);
        let b = self.bin(Binary::Add, &u, &b, len);
        let t = self.map_fold(&a, &b);
        self.map_fold(&t, &int(hirsel_core_layout::map_index::MIX))
    }

    fn map_place_tail(&mut self, p: &Value, len: &Value) -> Value {
        let u = Type::I64;
        let mut word = int(0);
        let mut consumed = int(0);
        for bytes in [8, 4, 2, 1] {
            let initial = self.current.expect("a tail load is in a block");
            let id = self.counter();
            let load = Label::new("map_tail_load", id);
            let done = Label::new("map_tail_done", id);
            let next = self.bin(Binary::Add, &u, &consumed, &int(bytes));
            let enough = self.cmp(Predicate::Uge, &u, len, &next);
            self.cond(&enough, load, done);
            self.block(load);
            let at = self.offset_by(p, &consumed);
            let ty = Type::Int(8 * bytes as u32);
            let element = self.load(&ty, &at, 1);
            let element = if bytes == 8 {
                element
            } else {
                self.zext(&ty, &element, &u)
            };
            let shift = self.bin(Binary::Shl, &u, &consumed, &int(3));
            let part = self.bin(Binary::Shl, &u, &element, &shift);
            let combined = self.bin(Binary::Or, &u, &word, &part);
            self.br(done);
            self.block(done);
            consumed = self.emit(Operation::Phi(&u, &[(consumed, initial), (next, load)]));
            word = self.emit(Operation::Phi(&u, &[(word, initial), (combined, load)]));
        }
        word
    }

    fn sip_round(&mut self, [v0, v1, v2, v3]: &mut [Value; 4]) {
        *v0 = self.bin(Binary::Add, &Type::I64, v0, v1);
        *v1 = self.sip_rotate_xor(v1, 13, v0);
        *v0 = self.sip_rotate(v0, 32);
        *v2 = self.bin(Binary::Add, &Type::I64, v2, v3);
        *v3 = self.sip_rotate_xor(v3, 16, v2);
        *v0 = self.bin(Binary::Add, &Type::I64, v0, v3);
        *v3 = self.sip_rotate_xor(v3, 21, v0);
        *v2 = self.bin(Binary::Add, &Type::I64, v2, v1);
        *v1 = self.sip_rotate_xor(v1, 17, v2);
        *v2 = self.sip_rotate(v2, 32);
    }

    fn sip_rotate_xor(&mut self, value: &Value, count: u8, other: &Value) -> Value {
        let rotated = self.sip_rotate(value, count);
        self.bin(Binary::Xor, &Type::I64, &rotated, other)
    }

    fn sip_rotate(&mut self, value: &Value, count: u8) -> Value {
        self.intrinsic(
            "llvm.fshl.i64",
            Type::I64,
            &[word(value.clone()), word(value.clone()), word(int(count))],
        )
    }

    pub(super) fn compare_bytes(&mut self, arguments: &[Value]) -> Value {
        self.items
            .declare("memcmp", memcmp(), read_only(self.facts));
        self.items.compare_bytes(self.facts);
        self.call(
            &compare_bytes_signature(),
            &Value::global("core_compare_bytes"),
            arguments,
            &CallAttrs::default(),
        )
    }
}

/// The total bytewise order `core_compare_bytes(a, al, b, bl)`: -1, 0 or 1.
pub(super) fn define_compare_bytes(sink: &mut dyn Sink, facts: bool) {
    const A: Local = Local::Named("a");
    const AL: Local = Local::Named("al");
    const B: Local = Local::Named("b");
    const BL: Local = Local::Named("bl");
    let attrs = FnAttrs {
        linkage: Linkage::Private,
        inline: Inline::Always,
        ..read_only(facts)
    };
    sink.begin_function(
        "core_compare_bytes",
        &compare_bytes_signature(),
        &[A, AL, B, BL],
        &attrs,
    );
    let memcmp = memcmp();
    let callee = Value::global("memcmp");
    let mut emit = |name: &'static str, op: Operation<'_>| {
        sink.instruction(Some(Local::Named(name)), op, None);
        Value::Local(Local::Named(name))
    };
    let (i8, i32, i64) = (Type::I8, Type::I32, Type::I64);
    let (a, al, b, bl): (Value, Value, Value, Value) = (A.into(), AL.into(), B.into(), BL.into());
    let same = emit("same", Operation::Compare(Predicate::Eq, &i64, &al, &bl));
    let equal_length = Label::plain("equal_length");
    let different_length = Label::plain("different_length");
    let empty_equal = Label::plain("empty_equal");
    let same_compare = Label::plain("same_compare");
    let length_result = Label::plain("length_result");
    let prefix_compare = Label::plain("prefix_compare");
    let (minus, zero, one) = (int(-1), int(0), int(1));
    let terminate = |sink: &mut dyn Sink, op: Operation<'_>| sink.instruction(None, op, None);
    terminate(
        sink,
        Operation::Conditional(&same, equal_length, different_length, false),
    );
    sink.block(equal_length);
    let mut emit = |name: &'static str, op: Operation<'_>| {
        sink.instruction(Some(Local::Named(name)), op, None);
        Value::Local(Local::Named(name))
    };
    let empty = emit("empty", Operation::Compare(Predicate::Eq, &i64, &al, &zero));
    drop(emit);
    terminate(
        sink,
        Operation::Conditional(&empty, empty_equal, same_compare, false),
    );
    sink.block(empty_equal);
    terminate(sink, Operation::Return(Some((&i8, &zero))));
    sink.block(same_compare);
    let order = |sink: &mut dyn Sink, names: [&'static str; 5], length: &Value, tie: &Value| {
        let mut emit = |name: &'static str, op: Operation<'_>| {
            sink.instruction(Some(Local::Named(name)), op, None);
            Value::Local(Local::Named(name))
        };
        let cmp = emit(
            names[0],
            Operation::Call(
                &memcmp,
                &callee,
                &[a.clone(), b.clone(), length.clone()],
                &CallAttrs {
                    step_neutral: true,
                    ..CallAttrs::default()
                },
            ),
        );
        let zero_cmp = emit(
            names[1],
            Operation::Compare(Predicate::Eq, &i32, &cmp, &zero),
        );
        let negative = emit(
            names[2],
            Operation::Compare(Predicate::Slt, &i32, &cmp, &zero),
        );
        let sign = emit(names[3], Operation::Select(&i8, &negative, &minus, &one));
        let order = emit(names[4], Operation::Select(&i8, &zero_cmp, tie, &sign));
        sink.instruction(None, Operation::Return(Some((&i8, &order))), None);
    };
    order(
        sink,
        ["cmp", "zero", "negative", "sign", "order"],
        &al,
        &zero,
    );
    sink.block(different_length);
    let mut emit = |name: &'static str, op: Operation<'_>| {
        sink.instruction(Some(Local::Named(name)), op, None);
        Value::Local(Local::Named(name))
    };
    let shorter = emit(
        "shorter",
        Operation::Compare(Predicate::Ult, &i64, &al, &bl),
    );
    let common = emit("common", Operation::Select(&i64, &shorter, &al, &bl));
    let length_order = emit(
        "length_order",
        Operation::Select(&i8, &shorter, &minus, &one),
    );
    let no_prefix = emit(
        "no_prefix",
        Operation::Compare(Predicate::Eq, &i64, &common, &zero),
    );
    drop(emit);
    terminate(
        sink,
        Operation::Conditional(&no_prefix, length_result, prefix_compare, false),
    );
    sink.block(length_result);
    terminate(sink, Operation::Return(Some((&i8, &length_order))));
    sink.block(prefix_compare);
    order(
        sink,
        [
            "prefix",
            "prefix_zero",
            "prefix_negative",
            "prefix_sign",
            "prefix_order",
        ],
        &common,
        &length_order,
    );
    sink.end_function();
}
