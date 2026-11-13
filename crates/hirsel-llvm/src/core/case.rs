//! Variant cases. Every read and write of a tag word goes through
//! hirsel-core-layout's `read_case` and `case_word`, so a niche variant (NICHES-14)
//! and a direct one are told apart in one place.
use super::FunctionEmitter;
use hirsel_core_layout::{CaseOps, Encoding, Tag, case_word, read_case};

/// `read_case` emitted as IR. Values are indices into `names`, since the trait's
/// values are `Copy`; `below` yields an `i1` that only `select` consumes.
struct Ir<'a, 'b> {
    e: &'a mut FunctionEmitter<'b>,
    ty: &'static str,
    names: Vec<String>,
}
impl Ir<'_, '_> {
    fn put(&mut self, name: String) -> usize {
        self.names.push(name);
        self.names.len() - 1
    }
    fn op(&mut self, op: &str, a: usize, b: usize) -> usize {
        let line = format!("{op} {} {}, {}", self.ty, self.names[a], self.names[b]);
        let name = self.e.calc(line);
        self.put(name)
    }
}
impl CaseOps for Ir<'_, '_> {
    type V = usize;
    fn constant(&mut self, n: u64) -> usize {
        self.put(n.to_string())
    }
    fn add(&mut self, a: usize, b: usize) -> usize {
        self.op("add", a, b)
    }
    fn sub(&mut self, a: usize, b: usize) -> usize {
        self.op("sub", a, b)
    }
    fn and(&mut self, a: usize, b: usize) -> usize {
        self.op("and", a, b)
    }
    fn below(&mut self, a: usize, b: usize) -> usize {
        self.op("icmp ult", a, b)
    }
    fn select(&mut self, c: usize, yes: usize, no: usize) -> usize {
        let line = format!(
            "select i1 {}, {ty} {}, {ty} {}",
            self.names[c],
            self.names[yes],
            self.names[no],
            ty = self.ty
        );
        let name = self.e.calc(line);
        self.put(name)
    }
}

impl FunctionEmitter<'_> {
    fn tag_at(&mut self, tag: &Tag, base: &str) -> String {
        if tag.offset == 0 {
            base.to_string()
        } else {
            self.calc(format!("getelementptr i8, ptr {base}, i64 {}", tag.offset))
        }
    }
    /// The case ordinal, as an `i32`, of the variant at `base`.
    pub(super) fn case_value(&mut self, tag: &Tag, base: &str) -> String {
        let at = self.tag_at(tag, base);
        let bits = tag.width * 8;
        let word = self.calc(format!("load i{bits}, ptr {at}, align 1"));
        // A direct tag is at most 4 bytes and is its own case; a niche word is read
        // in 64 bits.
        let (ty, wide) = match tag.encoding {
            Encoding::Direct => ("i32", 32),
            Encoding::Niche { .. } => ("i64", 64),
        };
        let word = if bits < wide {
            self.calc(format!("zext i{bits} {word} to {ty}"))
        } else {
            word
        };
        let mut ir = Ir {
            e: &mut *self,
            ty,
            names: vec![word],
        };
        let case = read_case(tag, &mut ir, 0);
        let case = ir.names[case].clone();
        if wide == 32 {
            case
        } else {
            self.calc(format!("trunc i64 {case} to i32"))
        }
    }
    /// Make the variant at `base` case `case`: its tag word, or nothing for a niche
    /// variant's dataful case, whose payload alone makes it.
    pub(super) fn set_case(&mut self, tag: &Tag, case: u32, base: &str) {
        if let Some(word) = case_word(tag, case) {
            let at = self.tag_at(tag, base);
            self.line(format!(
                "store i{} {word}, ptr {at}, align 1",
                tag.width * 8
            ));
        }
    }
}
