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
    pub(super) fn hash_bytes(&mut self, data: &Value, len: &Value) -> Value {
        let facts = read_only(self.facts);
        self.call_declared(
            "hirsel_core_hash_bytes",
            Type::I64,
            &[ptr(data.clone()), word(len.clone())],
            facts,
            &CallAttrs::default(),
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
            Operation::Call(&memcmp, &callee, &[a.clone(), b.clone(), length.clone()], &CallAttrs::default()),
        );
        let zero_cmp = emit(names[1], Operation::Compare(Predicate::Eq, &i32, &cmp, &zero));
        let negative = emit(names[2], Operation::Compare(Predicate::Slt, &i32, &cmp, &zero));
        let sign = emit(names[3], Operation::Select(&i8, &negative, &minus, &one));
        let order = emit(names[4], Operation::Select(&i8, &zero_cmp, tie, &sign));
        sink.instruction(None, Operation::Return(Some((&i8, &order))), None);
    };
    order(sink, ["cmp", "zero", "negative", "sign", "order"], &al, &zero);
    sink.block(different_length);
    let mut emit = |name: &'static str, op: Operation<'_>| {
        sink.instruction(Some(Local::Named(name)), op, None);
        Value::Local(Local::Named(name))
    };
    let shorter = emit("shorter", Operation::Compare(Predicate::Ult, &i64, &al, &bl));
    let common = emit("common", Operation::Select(&i64, &shorter, &al, &bl));
    let length_order = emit("length_order", Operation::Select(&i8, &shorter, &minus, &one));
    let no_prefix = emit("no_prefix", Operation::Compare(Predicate::Eq, &i64, &common, &zero));
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
