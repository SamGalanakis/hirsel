use super::*;
use hirsel_core_ir::StaticId;

impl Substitution<'_> {
    pub(super) fn descriptor_adapter_key(&mut self, raw: FuncId) -> InstanceKey {
        // Only the adapter's name needs the raw key here. Specialize the raw
        // signature from the adapter body, which has its call's ABI recipe.
        let instance = self.module.functions[raw.index()].instance;
        let raw_key = self.module.instances[instance.index()].clone();
        let mut key = self.call_key(raw_key, &self.call_names[&raw]);
        let owner = self
            .module
            .identity(self.module.functions[self.owner.index()].instance);
        key.definition
            .0
            .name
            .push_str(&format!("$callable[instance:{}]", owner.symbol()));
        key
    }

    pub(super) fn descriptor_bytes(
        &mut self,
        recipe: &crate::identity::DescriptorRecipe,
    ) -> Result<StaticId, String> {
        let bytes = recipe
            .render(&self.names)
            .ok_or("a described instance's type argument names a region")?;
        let row = self
            .module
            .statics
            .iter()
            .position(|row| row.literal_bytes() == Some(bytes.as_slice()));
        Ok(if let Some(row) = row {
            hirsel_core_ir::StaticId(row as u32)
        } else {
            let row = hirsel_core_ir::StaticId(self.module.statics.len() as u32);
            self.module
                .statics
                .push(hirsel_core_ir::Static::Bytes { align: 1, bytes });
            row
        })
    }
}
