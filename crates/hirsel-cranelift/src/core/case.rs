//! Variant cases. Every read and write of a tag word goes through
//! hirsel-core-layout's `read_case` and `case_word`, so a niche variant (NICHES-14)
//! and a direct one are told apart in one place.
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Type, Value, condcodes::IntCC, types};
use cranelift_frontend::FunctionBuilder;
use hirsel_core_layout::{CaseOps, Encoding, Tag, case_word, read_case};

struct Ops<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    ty: Type,
}
impl CaseOps for Ops<'_, '_> {
    type V = Value;
    fn constant(&mut self, n: u64) -> Value {
        self.b.ins().iconst(self.ty, n as i64)
    }
    fn add(&mut self, a: Value, b: Value) -> Value {
        self.b.ins().iadd(a, b)
    }
    fn sub(&mut self, a: Value, b: Value) -> Value {
        self.b.ins().isub(a, b)
    }
    fn and(&mut self, a: Value, b: Value) -> Value {
        self.b.ins().band(a, b)
    }
    fn below(&mut self, a: Value, b: Value) -> Value {
        let c = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
        self.b.ins().uextend(self.ty, c)
    }
    fn select(&mut self, c: Value, yes: Value, no: Value) -> Value {
        self.b.ins().select(c, yes, no)
    }
}

fn native(width: u64) -> Type {
    match width {
        1 => types::I8,
        2 => types::I16,
        4 => types::I32,
        _ => types::I64,
    }
}

/// The case ordinal, as an I32, of the variant at `base`.
pub(super) fn case_value(
    b: &mut FunctionBuilder<'_>,
    tag: &Tag,
    base: Value,
    flags: MemFlagsData,
) -> Value {
    let word = b
        .ins()
        .load(native(tag.width), flags, base, tag.offset as i32);
    // A direct tag is at most 4 bytes and is its own case; a niche word is read in
    // 64 bits.
    let ty = match tag.encoding {
        Encoding::Direct => types::I32,
        Encoding::Niche { .. } => types::I64,
    };
    let word = if native(tag.width) == ty {
        word
    } else {
        b.ins().uextend(ty, word)
    };
    let case = read_case(tag, &mut Ops { b: &mut *b, ty }, word);
    if ty == types::I32 {
        case
    } else {
        b.ins().ireduce(types::I32, case)
    }
}

/// Make the variant at `base` case `case`: its tag word, or nothing for a niche
/// variant's dataful case, whose payload alone makes it.
pub(super) fn set_case(
    b: &mut FunctionBuilder<'_>,
    tag: &Tag,
    case: u32,
    base: Value,
    flags: MemFlagsData,
) {
    if let Some(word) = case_word(tag, case) {
        let value = b.ins().iconst(native(tag.width), word as i64);
        b.ins().store(flags, value, base, tag.offset as i32);
    }
}
