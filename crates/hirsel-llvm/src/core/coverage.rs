//! Coverage flag stores (`docs/coverage.md`, "Sites" and "The raw file"), with
//! the Cranelift engine's records and arithmetic. A coverage body owns one
//! record in the `hirsel_cov` section: the header the raw file format pins,
//! then one zeroed byte per site. Every emission of the body (each ISA level,
//! the checked and step-free bodies) writes that record: its entry sets flag
//! 0, and each decision sets the flag of the arm it takes, `[no, yes]` for a
//! branch, beside the terminator. No CoreIR instruction can address a record.
use super::sink::Name;
use super::*;
use hirsel_core_ir::coverage::{RECORD_HEADER, SECTION};
use hirsel_core_layout::ObjectFormat;

/// The record a body being emitted writes its flags into.
pub(super) struct Flags {
    record: Name,
    /// By block, the flag of its decision's first arm
    /// ([`hirsel_core_ir::coverage::Sites::decisions`]).
    decisions: Vec<Option<u32>>,
}

/// Define the record of each function `module` flags, by function. The
/// records are retained against the optimiser and section collection: a body
/// the linker drops still counts in the denominator (`docs/coverage.md`, "The
/// denominator").
pub(super) fn records(
    module: &Module,
    target: Target,
    sink: &mut dyn Sink,
) -> Result<Vec<Option<Flags>>> {
    if module.coverage.is_none() {
        return Ok(Vec::new());
    }
    let mut records = Vec::with_capacity(module.functions.len());
    let mut names = Vec::new();
    for f in 0..module.functions.len() {
        let sites = hirsel_core_ir::coverage::sites(module, FuncId(f as u32))
            .map_err(|mismatch| Error::Llvm(mismatch.to_string()))?;
        let Some(sites) = sites else {
            records.push(None);
            continue;
        };
        let section = match target.object_format() {
            ObjectFormat::Elf => SECTION,
            // The entry bounds the section by its ELF start and stop symbols.
            ObjectFormat::MachO => {
                return Err(Error::Unsupported("coverage records on Mach-O".into()));
            }
        };
        let record: Name = format!("hirsel_cov.{f}").into();
        let bytes = sites.record();
        sink.global(&Global {
            name: record.clone(),
            linkage: Linkage::Private,
            constant: false,
            externally_initialized: false,
            ty: Type::bytes(bytes.len() as u64),
            init: Some(Value::Bytes(bytes)),
            align: 8,
            absolute_symbol: false,
            section: Some(section),
        });
        names.push(record.clone());
        records.push(Some(Flags {
            record,
            decisions: sites.decisions,
        }));
    }
    Ok(if names.is_empty() {
        Vec::new()
    } else {
        records
    })
}

/// `@llvm.used` over the records `records` defined, when there are any.
pub(super) fn retain(records: &[Option<Flags>], sink: &mut dyn Sink) {
    let names: Vec<Name> = records.iter().flatten().map(|f| f.record.clone()).collect();
    if !names.is_empty() {
        sink.used(&names);
    }
}

impl FunctionEmitter<'_> {
    /// The flag of the first arm of the decision that ends `block`, in a
    /// coverage body.
    pub(super) fn decision(&self, block: usize) -> Option<u32> {
        self.coverage.as_ref()?.decisions[block]
    }

    /// Set flag `flag`, plus one when `taken` (an `i1`) holds.
    pub(super) fn set_flag(&mut self, flag: u32, taken: Option<&Value>) {
        let Some(coverage) = &self.coverage else {
            return;
        };
        let record = Value::Global(coverage.record.clone());
        let at = self.offset(&record, RECORD_HEADER as i128 + i128::from(flag));
        let at = match taken {
            Some(yes) => {
                let yes = self.zext(&Type::I1, yes, &Type::I64);
                self.offset_by(&at, &yes)
            }
            None => at,
        };
        self.effect(Operation::Store(
            &Type::I8,
            &int(1),
            &at,
            &Memory::aligned(1),
        ));
    }

    /// Set the arm flag of the branch that ends `block` on `condition` (an
    /// `i8`, taken when nonzero), in a coverage body.
    pub(super) fn set_branch_flag(&mut self, block: usize, condition: &Value) {
        if let Some(flag) = self.decision(block) {
            let yes = self.cmp(Predicate::Ne, &Type::I8, condition, &int(0));
            self.set_flag(flag, Some(&yes));
        }
    }
}
