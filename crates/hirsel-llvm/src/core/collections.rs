//! Shared sequence/map carrier cleanup. Counted buffers borrow the carrier
//! from their suspended parent worklist frame, so cleanup stays constant-stack.
use super::*;
use hirsel_core_layout::{FieldTy, field_size, map_entry};

struct Entries<'a> {
    stride: u64,
    logical: u64,
    fields: Vec<(u64, &'a FieldTy)>,
    map: bool,
}
impl<'a> Entries<'a> {
    fn new(module: &Module, recipes: &[hirsel_core_layout::Recipe], field: &'a FieldTy) -> Self {
        match field {
            FieldTy::Seq(e) => {
                let (stride, _, logical) = field_size(&module.types, recipes, e);
                Self {
                    stride,
                    logical,
                    fields: vec![(0, e)],
                    map: false,
                }
            }
            FieldTy::Map(k, v) => {
                let layout = map_entry(&module.types, recipes, k, v);
                let logical = field_size(&module.types, recipes, k).2
                    + field_size(&module.types, recipes, v).2
                    + 16;
                Self {
                    stride: layout.stride,
                    logical,
                    fields: vec![(layout.key, k), (layout.value, v)],
                    map: true,
                }
            }
            _ => unreachable!(),
        }
    }
}
pub(super) fn symbol(field: &FieldTy) -> String {
    format!("@\"core_collection_drop_{field:?}\"")
}

pub(super) fn collect(field: &FieldTy, collections: &mut Vec<FieldTy>) {
    match field {
        FieldTy::Seq(e) => {
            if !collections.contains(field) {
                collections.push(field.clone());
            }
            collect(e, collections);
        }
        FieldTy::Map(k, v) => {
            if !collections.contains(field) {
                collections.push(field.clone());
            }
            collect(k, collections);
            collect(v, collections);
        }
        FieldTy::Array(e, _) => collect(e, collections),
        _ => {}
    }
}
impl FunctionEmitter<'_> {
    fn carrier_word(&mut self, carrier: &str, offset: u64) -> String {
        let at = self.calc(format!("getelementptr i8, ptr {carrier}, i64 {offset}"));
        self.calc(format!("load i64, ptr {at}, align 1"))
    }
    fn collection_charge(&mut self, capacity: &str, logical: u64) -> String {
        let bytes = self.calc(format!("mul i64 {capacity}, {logical}"));
        let rounded = self.calc(format!("add i64 {bytes}, 7"));
        self.calc(format!("and i64 {rounded}, -8"))
    }
    pub(super) fn clean_collection(
        &mut self,
        field: &FieldTy,
        carrier: &str,
        status: &str,
    ) -> Result<()> {
        // SEM-4: a List of a zero-sized element has no storage to clean.
        if hirsel_core_layout::storage_free(&self.module.types, field) {
            return Ok(());
        }
        let entries = Entries::new(self.module, self.recipes, field);
        let block = self.calc(format!("load ptr, ptr {carrier}, align 1"));
        let length = self.carrier_word(carrier, 8);
        let capacity = self.carrier_word(carrier, 16);
        let index = self.temp("i64");
        self.line(format!("store i64 0, ptr {index}"));
        let n = self.next;
        self.next += 1;
        let owned = self.calc(format!("icmp ne i64 {capacity}, 0"));
        self.line(format!(
            "br i1 {owned}, label %collection_head{n}, label %collection_done{n}"
        ));
        writeln!(self.code, "collection_head{n}:").unwrap();
        let i = self.calc(format!("load i64, ptr {index}"));
        let more = self.calc(format!("icmp ult i64 {i}, {length}"));
        self.line(format!(
            "br i1 {more}, label %collection_body{n}, label %collection_settle{n}"
        ));
        writeln!(self.code, "collection_body{n}:").unwrap();
        let entry = self.calc(format!(
            "getelementptr [{} x i8], ptr {block}, i64 {i}",
            entries.stride
        ));
        if entries.map {
            let state = self.carrier_word(&entry, 8);
            let live = self.calc(format!("icmp eq i64 {state}, 1"));
            self.line(format!(
                "br i1 {live}, label %collection_live{n}, label %collection_next{n}"
            ));
            writeln!(self.code, "collection_live{n}:").unwrap();
        }
        for (offset, f) in entries.fields {
            let at = self.calc(format!("getelementptr i8, ptr {entry}, i64 {offset}"));
            self.clean_field(f, &at, status)?;
        }
        self.line(format!("br label %collection_next{n}"));
        writeln!(self.code, "collection_next{n}:").unwrap();
        let next = self.calc(format!("add i64 {i}, 1"));
        self.line(format!("store i64 {next}, ptr {index}"));
        self.line(format!("br label %collection_head{n}"));
        writeln!(self.code, "collection_settle{n}:").unwrap();
        let allocated = self.calc(format!("icmp ne i64 {capacity}, 0"));
        self.line(format!(
            "br i1 {allocated}, label %collection_free{n}, label %collection_done{n}"
        ));
        writeln!(self.code, "collection_free{n}:").unwrap();
        let charge = self.collection_charge(&capacity, entries.logical);
        self.free_block(&block, &charge);
        self.line(format!("br label %collection_done{n}"));
        writeln!(self.code, "collection_done{n}:").unwrap();
        Ok(())
    }
    pub(super) fn enqueue_collection(&mut self, field: &FieldTy, carrier: &str) {
        if hirsel_core_layout::storage_free(&self.module.types, field) {
            self.line("ret i1 false");
            return;
        }
        let entries = Entries::new(self.module, self.recipes, field);
        let buffer = self.calc(format!("load ptr, ptr {carrier}, align 1"));
        let capacity = self.carrier_word(carrier, 16);
        let allocated = self.calc(format!("icmp ne i64 {capacity}, 0"));
        let n = self.next;
        self.next += 1;
        self.line(format!(
            "br i1 {allocated}, label %buffer{n}, label %empty{n}"
        ));
        writeln!(self.code, "buffer{n}:").unwrap();
        let charge = self.collection_charge(&capacity, entries.logical);
        if hirsel_core_layout::terminal(&self.module.types, self.recipes, field) {
            self.free_block(&buffer, &charge);
        } else {
            self.runtime_void(
                "hirsel_core_release_buffer_enqueue",
                &[
                    ("ptr".into(), "%worklist".into()),
                    ("ptr".into(), buffer),
                    ("i64".into(), charge),
                    ("ptr".into(), carrier.into()),
                    ("ptr".into(), symbol(field)),
                ],
            );
        }
        self.line("ret i1 false");
        writeln!(self.code, "empty{n}:").unwrap();
        self.line("ret i1 false");
    }
}

pub(super) fn glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    collections: &[FieldTy],
    text: &mut String,
    intrinsics: &mut BTreeSet<String>,
) -> Result<()> {
    let empty = Body::default();
    for field in collections {
        let entries = Entries::new(module, recipes, field);
        let mut e = FunctionEmitter {
            append_fast: false,
            module,
            body: &empty,
            recipes,
            abi: abi::Return::on_target(target, &[]),
            has_status: false,
            failures,
            constants: &[],
            observe_charge: false,
            facts,
            aliases: None,
            caller_site: "0",
            debug_location: None,
            anchor: None,
            relocatable_sites: false,
            private_float: Vec::new(),
            value_slots: Vec::new(),
            empty_bounds: empty::EmptyBounds::default(),
            code: String::new(),
            allocations: String::new(),
            next: 0,
            intrinsics,
        };
        let per = entries
            .fields
            .iter()
            .map(|(_, f)| cleanup::mechanical_count(module, recipes, f))
            .collect::<Result<Vec<_>>>()?;
        let count: u64 = per.iter().sum();
        if count != 0 {
            let cursor = e.calc("load i64, ptr %cursor");
            let length = e.carrier_word("%payload", 8);
            let index = e.calc(format!("udiv i64 {cursor}, {count}"));
            let more = e.calc(format!("icmp ult i64 {index}, {length}"));
            e.line(format!("br i1 {more}, label %element, label %done"));
            writeln!(e.code, "element:").unwrap();
            let next = e.calc(format!("add i64 {cursor}, 1"));
            e.line(format!("store i64 {next}, ptr %cursor"));
            let buffer = e.calc("load ptr, ptr %payload, align 1");
            let entry = e.calc(format!(
                "getelementptr [{} x i8], ptr {buffer}, i64 {index}",
                entries.stride
            ));
            if entries.map {
                let state = e.carrier_word(&entry, 8);
                let live = e.calc(format!("icmp eq i64 {state}, 1"));
                e.line(format!("br i1 {live}, label %live, label %removed"));
                writeln!(e.code, "removed:").unwrap();
                e.line("ret i1 false");
                writeln!(e.code, "live:").unwrap();
            }
            let inner = e.calc(format!("urem i64 {cursor}, {count}"));
            let mut base = 0;
            for ((offset, f), steps) in entries.fields.iter().zip(per) {
                let at = e.calc(format!("getelementptr i8, ptr {entry}, i64 {offset}"));
                e.mechanical_field(f, &at, &inner, base)?;
                base += steps;
            }
            e.line("ret i1 false");
            writeln!(e.code, "done:").unwrap();
        }
        e.line("ret i1 true");
        writeln!(text, "define private zeroext i1 {}(ptr %context, ptr %payload, ptr %cursor, ptr %worklist) {{\nentry:\n{}{} }}", symbol(field), e.allocations, e.code).unwrap();
    }
    Ok(())
}
