use crate::function::FnEmitter;
use crate::module::Emitted;
use cranelift_codegen::ir::Value;
use hirsel_lir::layout_types::LayoutId;
use hirsel_lir::runtime_abi::RuntimeEntry;

impl FnEmitter<'_, '_, '_> {
    pub(crate) fn needs_value_program(&self, layout: LayoutId) -> bool {
        self.unit
            .tables
            .structural
            .iter()
            .find(|p| p.root_layout() == layout)
            .is_some_and(|p| p.needs_value_program(self.em.inputs.layouts))
    }

    fn value_program(&mut self, layout: LayoutId) -> Emitted<(Value, Value)> {
        let Some(plan) = self
            .unit
            .tables
            .structural
            .iter()
            .find(|p| p.root_layout() == layout)
        else {
            return self.invalid("missing structural value program");
        };
        let words = plan
            .program(self.em.inputs.layouts, &|_| Some(0))
            .map_err(|what| crate::CodegenError::Invalid {
                definition: self.definition().clone(),
                what,
            })?;
        let bytes: Vec<_> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let program = self.em.constant_data(8, &bytes)?;
        let program = self.data_addr(program, 0);
        let length = self.u64c(words.len() as u64);
        Ok((program, length))
    }

    pub(crate) fn structural_order_at(
        &mut self,
        layout: LayoutId,
        left: Value,
        right: Value,
    ) -> Emitted<Value> {
        let (program, length) = self.value_program(layout)?;
        let context = self.context;
        self.rt1(
            RuntimeEntry::StructuralOrder,
            &[context, program, length, left, right],
        )
    }

    pub(crate) fn structural_hash_at(
        &mut self,
        layout: LayoutId,
        at: Value,
        seed: Value,
    ) -> Emitted<Value> {
        let (program, length) = self.value_program(layout)?;
        let context = self.context;
        self.rt1(
            RuntimeEntry::StructuralHash,
            &[context, program, length, at, seed],
        )
    }
}
