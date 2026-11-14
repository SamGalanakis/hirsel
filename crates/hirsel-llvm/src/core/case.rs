//! Variant cases. Every read and write of a tag word goes through
//! hirsel-core-layout's `read_case` and `case_word`, so a niche variant (NICHES-14)
//! and a direct one are told apart in one place.
use super::{Binary, Cast, FunctionEmitter, Predicate, Type, Value, int};
use hirsel_core_layout::{CaseOps, Encoding, Tag, case_word, read_case};

/// `read_case` emitted as IR. Values are indices into `values`, since the trait's
/// values are `Copy`; `below` yields an `i1` that only `select` consumes.
struct Ir<'a, 'b> {
    e: &'a mut FunctionEmitter<'b>,
    ty: Type,
    values: Vec<Value>,
}
impl Ir<'_, '_> {
    fn put(&mut self, value: Value) -> usize {
        self.values.push(value);
        self.values.len() - 1
    }
    fn op(&mut self, op: Binary, a: usize, b: usize) -> usize {
        let value = self
            .e
            .bin(op, &self.ty, &self.values[a], &self.values[b]);
        self.put(value)
    }
}
impl CaseOps for Ir<'_, '_> {
    type V = usize;
    fn constant(&mut self, n: u64) -> usize {
        self.put(int(n))
    }
    fn add(&mut self, a: usize, b: usize) -> usize {
        self.op(Binary::Add, a, b)
    }
    fn sub(&mut self, a: usize, b: usize) -> usize {
        self.op(Binary::Sub, a, b)
    }
    fn and(&mut self, a: usize, b: usize) -> usize {
        self.op(Binary::And, a, b)
    }
    fn below(&mut self, a: usize, b: usize) -> usize {
        let value = self
            .e
            .cmp(Predicate::Ult, &self.ty, &self.values[a], &self.values[b]);
        self.put(value)
    }
    fn select(&mut self, c: usize, yes: usize, no: usize) -> usize {
        let value = self.e.select(
            &self.ty,
            &self.values[c],
            &self.values[yes],
            &self.values[no],
        );
        self.put(value)
    }
}

impl FunctionEmitter<'_> {
    fn tag_at(&mut self, tag: &Tag, base: &Value) -> Value {
        if tag.offset == 0 {
            base.clone()
        } else {
            self.offset(base, tag.offset as i128)
        }
    }
    /// The case ordinal, as an `i32`, of the variant at `base`.
    pub(super) fn case_value(&mut self, tag: &Tag, base: &Value) -> Value {
        let at = self.tag_at(tag, base);
        let bits = Type::Int(tag.width as u32 * 8);
        let word = self.load(&bits, &at, 1);
        // A direct tag is at most 4 bytes and is its own case; a niche word is read
        // in 64 bits.
        let ty = match tag.encoding {
            Encoding::Direct => Type::I32,
            Encoding::Niche { .. } => Type::I64,
        };
        let word = if tag.width * 8 < if ty == Type::I32 { 32 } else { 64 } {
            self.zext(&bits, &word, &ty)
        } else {
            word
        };
        let wide = ty == Type::I64;
        let mut ir = Ir {
            e: &mut *self,
            ty,
            values: vec![word],
        };
        let case = read_case(tag, &mut ir, 0);
        let case = ir.values[case].clone();
        if wide {
            self.cast(Cast::Trunc, &Type::I64, &case, &Type::I32)
        } else {
            case
        }
    }
    /// Make the variant at `base` case `case`: its tag word, or nothing for a niche
    /// variant's dataful case, whose payload alone makes it.
    pub(super) fn set_case(&mut self, tag: &Tag, case: u32, base: &Value) {
        if let Some(word) = case_word(tag, case) {
            let at = self.tag_at(tag, base);
            self.store(&Type::Int(tag.width as u32 * 8), &int(word), &at, 1);
        }
    }
}
