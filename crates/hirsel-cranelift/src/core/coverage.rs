//! Coverage flag stores (`docs/coverage.md`, "Sites" and "The raw file"). A
//! coverage body owns one record in the `hirsel_cov` section: the header the
//! raw file format pins, then one zeroed byte per site. Its entry sets flag 0
//! when the body starts and each decision sets the flag of the arm it takes,
//! on the edge, before the jump. No CoreIR instruction can address a record.
use super::function::Function;
use super::*;
use c::coverage::{RECORD_HEADER, SECTION, Sites};

/// A coverage body's sites and the record its flags live in.
pub(super) struct Record {
    pub sites: Sites,
    pub data: cranelift_module::DataId,
}

/// Define the record of `function`'s sites, when the module flags it.
pub(super) fn declare(
    object: &mut ObjectModule,
    input: &c::Module,
    function: c::FuncId,
) -> Result<Option<Record>> {
    let Some(sites) = c::coverage::sites(input, function) else {
        return Ok(None);
    };
    let mut data = DataDescription::new();
    data.define(sites.record().into_boxed_slice());
    data.set_align(8);
    data.set_custom_section(SECTION);
    // Retained against section garbage collection: a body the linker drops
    // still counts in the denominator (`docs/coverage.md`, "The denominator").
    data.set_used(true);
    let id = object.declare_anonymous_data(true, false).map_err(error)?;
    object.define_data(id, &data).map_err(error)?;
    Ok(Some(Record { sites, data: id }))
}

/// The record a body being emitted writes its flags into.
pub(super) struct Flags {
    /// By block, the flag of its decision's first arm ([`Sites::decisions`]).
    decisions: Vec<Option<u32>>,
    table: ir::GlobalValue,
}

impl Flags {
    pub fn new(record: &Record, object: &mut ObjectModule, func: &mut ir::Function) -> Self {
        Self {
            decisions: record.sites.decisions.clone(),
            table: object.declare_data_in_func(record.data, func),
        }
    }
}

impl Function<'_, '_, '_> {
    /// The flag of the first arm of the decision that ends `block`, in a
    /// coverage body.
    pub fn decision(&self, block: usize) -> Option<u32> {
        self.coverage.as_ref()?.decisions[block]
    }

    /// Set flag `flag`, plus `taken` when given (a branch's condition, 0 or not).
    pub fn set_flag(&mut self, flag: u32, taken: Option<ir::Value>) {
        let Some(coverage) = &self.coverage else {
            return;
        };
        let mut base = self.b.ins().symbol_value(types::I64, coverage.table);
        if let Some(condition) = taken {
            let yes = self
                .b
                .ins()
                .icmp_imm(ir::condcodes::IntCC::NotEqual, condition, 0);
            let yes = self.b.ins().uextend(types::I64, yes);
            base = self.b.ins().iadd(base, yes);
        }
        let one = self.b.ins().iconst(types::I8, 1);
        self.b.ins().store(
            ir::MemFlagsData::trusted(),
            one,
            base,
            (RECORD_HEADER as u32 + flag) as i32,
        );
    }
}
