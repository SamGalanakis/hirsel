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
    /// MEM-24: a Columns block's column strides. Field k is then column k, at
    /// the sum of the columns before it, each `capacity × stride` rounded up to 8.
    columns: Option<Vec<u64>>,
}
impl<'a> Entries<'a> {
    fn new(
        module: &'a Module,
        recipes: &[hirsel_core_layout::RecipeRow],
        field: &'a FieldTy,
    ) -> Self {
        match field {
            // MEM-24: Columns charge as List of the row.
            FieldTy::Columns(e) => {
                let logical = field_size(&module.types, recipes, e).2;
                let fields = hirsel_core_layout::columns(&module.types, e);
                let strides = fields
                    .iter()
                    .map(|f| field_size(&module.types, recipes, f).0)
                    .collect();
                Self {
                    stride: 0,
                    logical,
                    fields: fields.into_iter().map(|f| (0, f)).collect(),
                    state: None,
                    columns: Some(strides),
                }
            }
            FieldTy::Seq(e) => {
                let (stride, _, logical) = field_size(&module.types, recipes, e);
                Self {
                    stride,
                    logical,
                    fields: vec![(0, e)],
                    state: None,
                    columns: None,
                }
            }
            FieldTy::Map(k, v) => {
                let layout = map_entry(&module.types, recipes, k, v);
                let logical = field_size(&module.types, recipes, k).2
                    + field_size(&module.types, recipes, v).2
                    + hirsel_core_layout::charge::MAP_ENTRY_EXTRA;
                Self {
                    stride: layout.stride,
                    logical,
                    fields: vec![(layout.key, k), (layout.value, v)],
                    state: Some(layout.meta),
                    columns: None,
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
        FieldTy::Seq(e) | FieldTy::Columns(e) => {
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
    fn release_literal_or_continue(&mut self, byte_seq: bool, cap: &Value, done: Label, n: usize) {
        if byte_seq && self.module.track_pack_owners {
            let literal = Label::new("buffer_literal", n);
            let heap = Label::new("buffer_heap", n);
            let masked = self.bin(
                Binary::And,
                &Type::I64,
                cap,
                &int(hirsel_core_layout::STRING_LITERAL_TAG_MASK),
            );
            let is_heap = self.cmp(
                Predicate::Ne,
                &Type::I64,
                &masked,
                &int(hirsel_core_layout::STRING_LITERAL_TAG),
            );
            self.likely(&is_heap, heap, literal);
            self.block(literal);
            self.runtime_void("hirsel_core_release_static_bytes", &[word(cap.clone())]);
            self.br(done);
            self.block(heap);
        }
    }

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
    pub(super) fn release_collection(
        &mut self,
        field: &FieldTy,
        carrier: &Value,
    ) -> Result<Option<Value>> {
        // SEM-4: a List of a zero-sized element has no storage to clean.
        if hirsel_core_layout::storage_free(&self.module.types, field) {
            return Ok(None);
        }
        let entries = Entries::new(self.module, self.recipes, field);
        let buffer = self.load(&Type::Ptr, carrier, 1);
        let capacity = self.carrier_word(carrier, 16);
        let allocated = self.cmp(Predicate::Ne, &Type::I64, &capacity, &int(0));
        let n = self.counter();
        let present = Label::new("buffer_release", n);
        let done = Label::new("buffer_released", n);
        let byte_seq = matches!(field, FieldTy::Seq(e) if **e == FieldTy::Scalar(Scalar::U8));
        let terminal = hirsel_core_layout::terminal(&self.module.types, self.recipes, field);
        let status = (!terminal).then(|| {
            let status = self.temp(Type::I8);
            self.store(&Type::I8, &int(0), &status, 0);
            status
        });
        self.cond(&allocated, present, done);
        self.block(present);
        self.release_literal_or_continue(byte_seq, &capacity, done, n);
        let charge = self.collection_charge(&capacity, entries.logical);
        // A List of direct elements: cleaned here from index 0 up (SEM-9), each
        // element as its own ordinary release, then freed as the drain frees it.
        let direct = !terminal
            && entries.state.is_none()
            && entries.columns.is_none()
            && entries.fields.iter().all(|(_, f)| {
                super::cleanup::direct_field(self.module, self.recipes, f, &self.items.direct)
            });
        if direct {
            let i64 = Type::I64;
            let length = self.carrier_word(carrier, 8);
            let index = self.temp(Type::I64);
            self.store(&i64, &int(0), &index, 0);
            let head = Label::new("elements_head", n);
            let body = Label::new("elements_body", n);
            let end = Label::new("elements_done", n);
            self.br(head);
            self.block(head);
            let i = self.load(&i64, &index, 0);
            let more = self.cmp(Predicate::Ult, &i64, &i, &length);
            self.cond(&more, body, end);
            self.block(body);
            // Only this edge has index < the allocated collection extent.
            let entry = self.gep(
                &Type::bytes(entries.stride),
                &buffer,
                &i,
                self.facts && entries.stride != 0,
            );
            let scratch = self.temp(Type::I8);
            for (offset, f) in &entries.fields {
                let at = self.offset(&entry, *offset);
                self.direct_field(f, &at, &scratch, &int(0))?;
            }
            let next = self.bin(Binary::Add, &i64, &i, &int(1));
            self.store(&i64, &next, &index, 0);
            self.br(head);
            self.block(end);
            self.runtime_void(
                "hirsel_core_free_box",
                &[ptr(CONTEXT), ptr(buffer), word(charge)],
            );
            self.br(done);
            self.block(done);
            return Ok(None);
        }
        if let Some(status) = &status {
            let site = self.site_word();
            let failed = self.runtime_bool(
                "hirsel_core_release_buffer",
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
        Ok(status.map(|status| self.load(&Type::I8, &status, 0)))
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
        let byte_seq = matches!(field, FieldTy::Seq(e) if **e == FieldTy::Scalar(Scalar::U8));
        self.release_literal_or_continue(byte_seq, &capacity, empty, n);
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
    recipes: &[hirsel_core_layout::RecipeRow],
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
            let inner = e.bin(Binary::URem, &i64, &cursor, &int(count));
            if let Some(strides) = &entries.columns {
                let capacity = e.carrier_word(&payload, 16);
                let mut offset = int(0);
                let mut base = 0;
                for ((_, f), (stride, steps)) in entries.fields.iter().zip(strides.iter().zip(per))
                {
                    let column = e.gep(&Type::bytes(1), &buffer, &offset, false);
                    let at = e.gep(&Type::bytes(*stride), &column, &index, false);
                    e.mechanical_field(f, &at, &inner, base)?;
                    base += steps;
                    let bytes = e.collection_charge(&capacity, *stride);
                    offset = e.bin(Binary::Add, &i64, &offset, &bytes);
                }
                e.br(Label::plain("resume"));
                e.block(done);
                e.ret(&Type::I1, &true.into());
                drop(e);
                sink.end_function();
                continue;
            }
            // Only this edge has index < the allocated collection extent.
            let entry = e.gep(
                &Type::bytes(entries.stride),
                &buffer,
                &index,
                facts && entries.stride != 0,
            );
            if let Some(state) = entries.state {
                let state = e.carrier_word(&entry, state);
                let live = e.cmp(Predicate::Ne, &i64, &state, &int(0));
                let live_label = Label::plain("live");
                let removed = Label::plain("removed");
                e.cond(&live, live_label, removed);
                e.block(removed);
                e.br(Label::plain("resume"));
                e.block(live_label);
            }
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
