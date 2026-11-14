//! FORM#91 relocatable sites and activation fallback. Files and line positions
//! are debug/link inputs; only stable item identities occur in failure code.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value};

/// One private pool per object, with one absolute data relocation per distinct
/// item-relative site. Code addresses the pool, never the absolute site symbol.
#[derive(Default)]
pub(super) struct Constants {
    data: Option<cranelift_module::DataId>,
    offsets: std::collections::HashMap<(ItemId, u32), u32>,
    words: Vec<(cranelift_module::DataId, u32)>,
}
impl Constants {
    fn get(
        &mut self,
        object: &mut ObjectModule,
        item: ItemId,
        start: u32,
        symbol: cranelift_module::DataId,
    ) -> Result<(cranelift_module::DataId, u32)> {
        let data = match self.data {
            Some(data) => data,
            None => {
                let data = object
                    .declare_data("hirsel.core.sites", Linkage::Local, false, false)
                    .map_err(error)?;
                self.data = Some(data);
                data
            }
        };
        let offset = if let Some(&offset) = self.offsets.get(&(item, start)) {
            offset
        } else {
            let offset = u32::try_from(self.words.len() * 8).map_err(error)?;
            self.words.push((symbol, start));
            self.offsets.insert((item, start), offset);
            offset
        };
        Ok((data, offset))
    }
    pub fn define(&self, object: &mut ObjectModule) -> Result<()> {
        let Some(data) = self.data else { return Ok(()) };
        let mut description = DataDescription::new();
        description.define(vec![0; self.words.len() * 8].into_boxed_slice());
        description.set_align(8);
        for (i, &(symbol, start)) in self.words.iter().enumerate() {
            let reference = object.declare_data_in_data(symbol, &mut description);
            description.write_data_addr((i * 8) as u32, reference, i64::from(start));
        }
        object.define_data(data, &description).map_err(error)
    }
}

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
        let Some(anchor) = self
            .anchor
            .filter(|a| !self.input.items[a.item.index()].package_is("core", "1"))
        else {
            return Ok(if c::source::inherits_site(self.instance) {
                self.caller_site
            } else {
                self.b.ins().iconst(types::I64, 0)
            });
        };
        let mut symbols = self.site_symbols.borrow_mut();
        let data = if let Some(&data) = symbols.get(&anchor.item) {
            data
        } else {
            let name = c::source::site_symbol(&self.input.items[anchor.item.index()]);
            let data = self
                .object
                .declare_data(&name, Linkage::Import, false, false)
                .map_err(error)?;
            symbols.insert(anchor.item, data);
            data
        };
        let (data, offset) =
            self.site_constants
                .borrow_mut()
                .get(self.object, anchor.item, anchor.start, data)?;
        let global = *self
            .site_reference
            .get_or_insert_with(|| self.object.declare_data_in_func(data, self.b.func));
        let address = self.b.ins().symbol_value(types::I64, global);
        let address = self.b.ins().iadd_imm_s(address, i64::from(offset));
        Ok(self.b.ins().load(
            types::I64,
            MemFlagsData::trusted().with_readonly(),
            address,
            0,
        ))
    }
}
