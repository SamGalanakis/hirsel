use super::*;

impl FunctionEmitter<'_> {
    /// A `ValueLayout`'s words before its cleanup: bytes, alignment and
    /// logical size. A task input's bytes and alignment cover its entry's
    /// code word.
    pub(in crate::core) fn layout_words(&self, t: TypeId, task_input: bool) -> [u64; 3] {
        let (mut bytes, mut align) = self.place_layout(PlaceTy::Declared(t));
        if task_input {
            bytes += task_entry::input_offset(self.module, self.recipes, t);
            align = align.max(8);
        }
        let logical = hirsel_core_layout::field_size(
            &self.module.types,
            self.recipes,
            &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
        )
        .2;
        [bytes, align, logical]
    }
    pub(super) fn value_descriptors(&mut self, types: &[TypeId], task_input: bool) -> Value {
        let buffer = self.temp(Type::bytes(32 * types.len() as u64));
        for (i, &t) in types.iter().enumerate() {
            let words = self.layout_words(t, task_input && i == 0);
            for (j, value) in words.into_iter().enumerate() {
                let at = self.offset(&buffer, (i * 32 + j * 8) as u64);
                self.store(&Type::I64, &int(value), &at, 8);
            }
            let at = self.offset(&buffer, (i * 32 + 24) as u64);
            // A task input has no cleanup here: the child body always cleans it.
            let clean = if task_input && i == 0 {
                Value::Null
            } else {
                Value::global(format!("core_clean_{}", t.0))
            };
            self.store(&Type::Ptr, &clean, &at, 8);
        }
        buffer
    }
}
