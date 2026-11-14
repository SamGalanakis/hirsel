//! FORM#91 relocatable sites and activation fallback. Files and line positions
//! are debug/link inputs; only stable item identities occur in failure code.
use super::*;
use super::function::Function;
use cranelift_codegen::ir::{MemFlagsData, Value};

impl Function<'_, '_, '_> {
    pub fn at_anchor(&mut self, anchor: Option<Anchor>, index: u32) {
        self.anchor = anchor;
        self.srcloc = if self.debug && anchor.is_some() {
            ir::SourceLoc::new(index)
        } else {
            ir::SourceLoc::default()
        };
        self.b.set_srcloc(self.srcloc);
    }
    pub fn site(&mut self) -> Result<Value> {
        let Some(anchor) = self.anchor.filter(|a|
            !self.input.items[a.item.index()].package_is("core", "1")) else {
            return Ok(self.caller_site);
        };
        let mut symbols = self.site_symbols.borrow_mut();
        let data = if let Some(&data) = symbols.get(&anchor.item) {
            data
        } else {
            let name = c::source::site_symbol(&self.input.items[anchor.item.index()]);
            let data = self.object.declare_data(&name, Linkage::Import, false, false).map_err(error)?;
            symbols.insert(anchor.item, data);
            data
        };
        let global = self.object.declare_data_in_func(data, self.b.func);
        let address = self.b.ins().symbol_value(types::I64, global);
        let base = self.b.ins().load(types::I64, MemFlagsData::trusted().with_readonly(), address, 0);
        Ok(self.b.ins().bor_imm_s(base, i64::from(anchor.start)))
    }
}
