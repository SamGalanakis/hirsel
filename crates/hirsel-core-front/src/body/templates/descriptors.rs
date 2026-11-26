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

    /// A `descriptor` of the generic body in the instance: its function,
    /// identity bytes and spawn rows substituted.
    pub(super) fn descriptor_ref(
        &mut self,
        inst: &mut Inst,
        template: &GenericTemplate,
    ) -> Result<(), String> {
        let Inst::DescriptorRef {
            function,
            identity,
            spawn,
            ..
        } = inst
        else {
            unreachable!("a descriptor")
        };
        if let Some(recipe) = template.descriptors.get(function) {
            *identity = self.descriptor_bytes(recipe)?;
        }
        *function = self.function(*function);
        *spawn = spawn.map(|t| hirsel_core_ir::SpawnTypes {
            x: self.ty(t.x),
            r: self.ty(t.r),
        });
        Ok(())
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
