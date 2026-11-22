//! Shared sequence/map carrier cleanup. Counted buffers borrow the carrier
//! from their suspended parent worklist frame, so cleanup stays constant-stack.
use super::cleanup::{STEP_PARAMS, glue_emitter, step_signature};
use super::*;
use hirsel_core_layout::{FieldTy, field_size, map_entry};

struct Entries<'a> {
    stride: u64,
    logical: u64,
    fields: Vec<(u64, &'a FieldTy)>,
    state: Option<u64>,
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
                    state: None,
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
                    state: Some(layout.state),
                }
            }
            _ => unreachable!(),
        }
    }
}
/// The step glue of one collection carrier type.
pub(super) fn symbol(field: &FieldTy) -> String {
    format!("core_collection_drop_{field:?}")
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
    fn carrier_word(&mut self, carrier: &Value, offset: u64) -> Value {
        let at = self.offset(carrier, offset);
        self.load(&Type::I64, &at, 1)
    }
    fn collection_charge(&mut self, capacity: &Value, logical: u64) -> Value {
        let i64 = Type::I64;
        let bytes = self.bin(Binary::Mul, &i64, capacity, &int(logical));
        let rounded = self.bin(Binary::Add, &i64, &bytes, &int(7));
        self.bin(Binary::And, &i64, &rounded, &int(-8))
    }
    /// Seq or Map storage held outside a drain: terminal storage is an
    /// inline free; any other is released by the drain from its buffer recipe
    /// (LANG-19), whose status this returns. A static carrier (capacity 0)
    /// owns nothing.
    pub(super) fn release_collection(&mut self, field: &FieldTy, carrier: &Value) -> Option<Value> {
        // SEM-4: a List of a zero-sized element has no storage to clean.
        if hirsel_core_layout::storage_free(&self.module.types, field) {
            return None;
        }
        let entries = Entries::new(self.module, self.recipes, field);
        let buffer = self.load(&Type::Ptr, carrier, 1);
        let capacity = self.carrier_word(carrier, 16);
        let allocated = self.cmp(Predicate::Ne, &Type::I64, &capacity, &int(0));
        let n = self.counter();
        let present = Label::new("buffer_release", n);
        let done = Label::new("buffer_released", n);
        let terminal = hirsel_core_layout::terminal(&self.module.types, self.recipes, field);
        let status = (!terminal).then(|| {
            let status = self.temp(Type::I8);
            self.store(&Type::I8, &int(0), &status, 0);
            status
        });
        self.cond(&allocated, present, done);
        self.block(present);
        let charge = self.collection_charge(&capacity, entries.logical);
        if let Some(status) = &status {
            let site = self.site_word();
            let failed = self.runtime(
                "hirsel_core_release_buffer",
                Type::I8,
                &[
                    ptr(CONTEXT),
                    ptr(buffer),
                    word(charge),
                    ptr(carrier.clone()),
                    ptr(Value::global(symbol(field))),
                    word(site),
                ],
                &CallAttrs::default(),
            );
            self.store(&Type::I8, &failed, status, 0);
        } else {
            self.free_block(&buffer, &charge);
        }
        self.br(done);
        self.block(done);
        status.map(|status| self.load(&Type::I8, &status, 0))
    }
    pub(super) fn enqueue_collection(&mut self, field: &FieldTy, carrier: &Value) {
        if hirsel_core_layout::storage_free(&self.module.types, field) {
            self.br(Label::plain("resume"));
            return;
        }
        let entries = Entries::new(self.module, self.recipes, field);
        let buffer = self.load(&Type::Ptr, carrier, 1);
        let capacity = self.carrier_word(carrier, 16);
        let allocated = self.cmp(Predicate::Ne, &Type::I64, &capacity, &int(0));
        let n = self.counter();
        let present = Label::new("buffer", n);
        let empty = Label::new("empty", n);
        self.cond(&allocated, present, empty);
        self.block(present);
        let charge = self.collection_charge(&capacity, entries.logical);
        if hirsel_core_layout::terminal(&self.module.types, self.recipes, field) {
            self.free_block(&buffer, &charge);
        } else {
            let point = self.next_point();
            self.runtime_void(
                "hirsel_core_release_buffer_enqueue",
                &[
                    ptr(Local::WORKLIST),
                    ptr(buffer),
                    word(charge),
                    ptr(carrier.clone()),
                    ptr(Value::global(symbol(field))),
                    word(int(point)),
                ],
            );
            self.ret(&Type::I1, &FALSE);
            self.block(empty);
            self.br(Label::plain("resume"));
            return;
        }
        self.br(Label::plain("resume"));
        self.block(empty);
        self.br(Label::plain("resume"));
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn glue(
    module: &Module,
    target: Target,
    recipes: &[hirsel_core_layout::Recipe],
    failures: &failure::Failures,
    facts: bool,
    collections: &[FieldTy],
    sink: &mut dyn Sink,
    items: &mut Items,
) -> Result<()> {
    let empty = Body::default();
    let i64 = Type::I64;
    for field in collections {
        let entries = Entries::new(module, recipes, field);
        sink.begin_function(
            &symbol(field),
            &step_signature(),
            &STEP_PARAMS,
            &FnAttrs::private(),
        );
        let mut e = glue_emitter(
            module,
            recipes,
            failures,
            facts,
            &empty,
            abi::Return::on_target(target, &[]),
            false,
            int(0),
            &mut *sink,
            &mut *items,
        );
        e.release_site();
        let per = entries
            .fields
            .iter()
            .map(|(_, f)| cleanup::mechanical_count(module, recipes, f))
            .collect::<Result<Vec<_>>>()?;
        let count: u64 = per.iter().sum();
        if count != 0 {
            let cursor_at = Value::Local(Local::CURSOR);
            let payload = Value::Local(Local::PAYLOAD);
            let element = Label::plain("element");
            let done = Label::plain("done");
            e.br(Label::plain("resume"));
            e.block(Label::plain("resume"));
            let cursor = e.load(&i64, &cursor_at, 0);
            let length = e.carrier_word(&payload, 8);
            let index = e.bin(Binary::UDiv, &i64, &cursor, &int(count));
            let more = e.cmp(Predicate::Ult, &i64, &index, &length);
            e.cond(&more, element, done);
            e.block(element);
            let next = e.bin(Binary::Add, &i64, &cursor, &int(1));
            e.store(&i64, &next, &cursor_at, 0);
            let buffer = e.load(&Type::Ptr, &payload, 1);
            // Only this edge has index < the allocated collection extent.
            let entry = e.gep(
                &Type::bytes(entries.stride),
                &buffer,
                &index,
                facts && entries.stride != 0,
            );
            if let Some(state) = entries.state {
                let state = e.carrier_word(&entry, state);
                let live = e.cmp(Predicate::Eq, &i64, &state, &int(1));
                let live_label = Label::plain("live");
                let removed = Label::plain("removed");
                e.cond(&live, live_label, removed);
                e.block(removed);
                e.br(Label::plain("resume"));
                e.block(live_label);
            }
            let inner = e.bin(Binary::URem, &i64, &cursor, &int(count));
            let mut base = 0;
            for ((offset, f), steps) in entries.fields.iter().zip(per) {
                let at = e.offset(&entry, *offset);
                e.mechanical_field(f, &at, &inner, base)?;
                base += steps;
            }
            e.br(Label::plain("resume"));
            e.block(done);
        }
        e.ret(&Type::I1, &true.into());
        drop(e);
        sink.end_function();
    }
    Ok(())
}
