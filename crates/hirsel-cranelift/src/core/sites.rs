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
    cause_owner: Option<cranelift_module::DataId>,
    offsets: std::collections::HashMap<(ItemId, u32), u32>,
    words: Vec<(cranelift_module::DataId, u32)>,
}
impl Constants {
    fn cause_owner(&mut self, object: &mut ObjectModule) -> Result<cranelift_module::DataId> {
        if let Some(id) = self.cause_owner {
            return Ok(id);
        }
        let id = object
            .declare_data("hirsel_core_cause_owner", Linkage::Import, false, false)
            .map_err(error)?;
        self.cause_owner = Some(id);
        Ok(id)
    }
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
    /// Defines the pool. An ELF word is one absolute relocation against its
    /// site symbol. A Mach-O word is measured from the sites origin
    /// ([`c::source::SITES_ORIGIN_SYMBOL`]): `cranelift-module` has no
    /// relocation of two symbols, so the pool is defined without relocations
    /// here and [`Self::measure`] adds the pairs to the finished object. Gives
    /// the origin when the words still need it.
    pub fn define(
        &self,
        object: &mut ObjectModule,
        format: hirsel_core_layout::ObjectFormat,
    ) -> Result<Option<cranelift_module::DataId>> {
        let Some(data) = self.data else {
            return Ok(None);
        };
        let mut description = DataDescription::new();
        description.define(vec![0; self.words.len() * 8].into_boxed_slice());
        description.set_align(8);
        let origin = match format {
            hirsel_core_layout::ObjectFormat::Elf => {
                for (i, &(symbol, start)) in self.words.iter().enumerate() {
                    let reference = object.declare_data_in_data(symbol, &mut description);
                    description.write_data_addr((i * 8) as u32, reference, i64::from(start));
                }
                None
            }
            hirsel_core_layout::ObjectFormat::MachO => Some(
                object
                    .declare_data(
                        c::source::SITES_ORIGIN_SYMBOL,
                        Linkage::Import,
                        false,
                        false,
                    )
                    .map_err(error)?,
            ),
        };
        object.define_data(data, &description).map_err(error)?;
        Ok(origin)
    }

    /// Writes each Mach-O pool word as `site symbol - origin + start`.
    pub fn measure(
        &self,
        product: &mut cranelift_object::ObjectProduct,
        origin: cranelift_module::DataId,
    ) -> Result<()> {
        use cranelift_object::object;
        let pool = product
            .object
            .symbol(product.data_symbol(self.data.unwrap()));
        let (Some(section), base) = (pool.section.id(), pool.value) else {
            return Err(error("the site pool has no section"));
        };
        let origin = product.data_symbol(origin);
        for (i, &(symbol, start)) in self.words.iter().enumerate() {
            product
                .object
                .add_relocation_with_subtractor(
                    section,
                    object::write::Relocation {
                        offset: base + (i * 8) as u64,
                        symbol: product.data_symbol(symbol),
                        addend: i64::from(start),
                        flags: object::RelocationFlags::Generic {
                            kind: object::RelocationKind::Absolute,
                            encoding: object::RelocationEncoding::Generic,
                            size: 64,
                        },
                    },
                    Some(origin),
                )
                .map_err(error)?;
        }
        Ok(())
    }
}

pub(super) fn caller_site_owner(
    b: &mut FunctionBuilder<'_>,
    inherits: bool,
    context: Value,
) -> Value {
    if inherits {
        context_site_owner(b, context)
    } else {
        b.ins().iconst(types::I64, 0)
    }
}

pub(super) fn context_site_owner(b: &mut FunctionBuilder<'_>, context: Value) -> Value {
    b.ins().load(
        types::I64,
        MemFlagsData::trusted(),
        context,
        memory_layout::CONTEXT_SITE_OWNER as i32,
    )
}

pub(super) fn store_site_owner(b: &mut FunctionBuilder<'_>, context: Value, owner: Value) {
    b.ins().store(
        MemFlagsData::trusted(),
        owner,
        context,
        memory_layout::CONTEXT_SITE_OWNER as i32,
    );
}

pub(super) fn worklist_site(b: &mut FunctionBuilder<'_>, list: Value) -> (Value, Value) {
    let site = b.ins().load(
        types::I64,
        MemFlagsData::trusted(),
        list,
        memory_layout::WORKLIST_SITE as i32,
    );
    let owner = b.ins().load(
        types::I64,
        MemFlagsData::trusted(),
        list,
        memory_layout::WORKLIST_SITE_OWNER as i32,
    );
    (site, owner)
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
            if c::source::inherits_site(self.instance) {
                self.b.ins().store(
                    MemFlagsData::trusted(),
                    self.caller_site_owner,
                    self.context,
                    memory_layout::CONTEXT_SITE_OWNER as i32,
                );
                return Ok(self.caller_site);
            }
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.ins().store(
                MemFlagsData::trusted(),
                zero,
                self.context,
                memory_layout::CONTEXT_SITE_OWNER as i32,
            );
            return Ok(zero);
        };
        let owner_data = self.site_constants.borrow_mut().cause_owner(self.object)?;
        let owner_global = *self
            .cause_owner_reference
            .get_or_insert_with(|| self.object.declare_data_in_func(owner_data, self.b.func));
        let owner_slot = self.b.ins().symbol_value(types::I64, owner_global);
        let owner = self.b.ins().load(
            types::I64,
            MemFlagsData::trusted().with_readonly(),
            owner_slot,
            0,
        );
        self.b.ins().store(
            MemFlagsData::trusted(),
            owner,
            self.context,
            memory_layout::CONTEXT_SITE_OWNER as i32,
        );
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
