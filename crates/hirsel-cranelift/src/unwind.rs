//! System V CFI for position-independent Cranelift ELF objects.
use crate::core::Error;
use cranelift_codegen::{Context, isa::TargetIsa, isa::unwind::UnwindInfo};
use cranelift_module::{FuncId, Module as _};
use cranelift_object::{ObjectModule, ObjectProduct, object};
use gimli::{
    LittleEndian,
    constants::{DW_EH_PE_pcrel, DW_EH_PE_sdata4, DwEhPe},
    write::{
        Address, CieId, EhFrame, EndianVec, FrameTable, RelocateWriter, Relocation,
        RelocationTarget,
    },
};

const POINTER_ENCODING: DwEhPe = DwEhPe(DW_EH_PE_pcrel.0 | DW_EH_PE_sdata4.0);

/// Collects unwind instructions for each definition of one ELF object.
pub struct Unwind {
    frames: FrameTable,
    cie: CieId,
    functions: Vec<FuncId>,
}

fn backend(error: impl std::fmt::Display) -> Error {
    Error::Backend(error.to_string())
}

impl Unwind {
    pub fn new(isa: &dyn TargetIsa) -> Result<Self, Error> {
        let mut cie = isa
            .create_systemv_cie()
            .ok_or_else(|| backend("target has no System V CIE"))?;
        // A read-only frame section must link into a PIE without dynamic relocations.
        cie.fde_address_encoding = POINTER_ENCODING;
        let mut frames = FrameTable::default();
        let cie = frames.add_cie(cie);
        Ok(Self {
            frames,
            cie,
            functions: Vec::new(),
        })
    }

    pub fn define(
        &mut self,
        module: &mut ObjectModule,
        function: FuncId,
        context: &mut Context,
    ) -> Result<(), Error> {
        module.define_function(function, context).map_err(backend)?;
        self.record(module.isa(), function, context)
    }

    pub(crate) fn record(
        &mut self,
        isa: &dyn TargetIsa,
        function: FuncId,
        context: &Context,
    ) -> Result<(), Error> {
        let Some(UnwindInfo::SystemV(info)) = context
            .compiled_code()
            .expect("defined function")
            .create_unwind_info(isa)
            .map_err(backend)?
        else {
            return Err(backend(
                "defined ELF function has no System V unwind information",
            ));
        };
        let address = Address::Symbol {
            symbol: self.functions.len(),
            addend: 0,
        };
        self.frames.add_fde(self.cie, info.to_fde(address));
        self.functions.push(function);
        Ok(())
    }

    pub fn write(self, product: &mut ObjectProduct) -> Result<(), Error> {
        if self.functions.is_empty() {
            return Ok(());
        }
        let mut output = EhFrame(Section {
            bytes: EndianVec::new(LittleEndian),
            relocations: Vec::new(),
        });
        self.frames.write_eh_frame(&mut output).map_err(backend)?;
        let EhFrame(output) = output;
        let section = product
            .object
            .section_id(object::write::StandardSection::EhFrame);
        let frame_offset = product
            .object
            .append_section_data(section, output.bytes.slice(), 8);
        for relocation in output.relocations {
            let RelocationTarget::Symbol(index) = relocation.target else {
                return Err(backend("unexpected non-symbol .eh_frame relocation"));
            };
            let symbol = product.function_symbol(self.functions[index]);
            // Weak function symbols can resolve to a definition in another object.
            // An FDE describes the code at this definition's section offset.
            let (symbol, code_offset) = if product.object.format() == object::BinaryFormat::MachO {
                // A Mach-O relocation against a section stores the target's
                // address, not its offset, and code need not start a file at
                // zero: name the definition by a local symbol of its own.
                let (section, offset) = {
                    let defined = product.object.symbol(symbol);
                    match defined.section {
                        object::write::SymbolSection::Section(section) => (section, defined.value),
                        _ => return Err(backend("unwind function has no section")),
                    }
                };
                let local = product.object.add_symbol(object::write::Symbol {
                    name: format!("hirsel.fde.{index}").into_bytes(),
                    value: offset,
                    size: 0,
                    kind: object::SymbolKind::Text,
                    scope: object::SymbolScope::Compilation,
                    weak: false,
                    section: object::write::SymbolSection::Section(section),
                    flags: object::SymbolFlags::None,
                });
                (local, 0)
            } else {
                product
                    .object
                    .symbol_section_and_offset(symbol)
                    .expect("unwind function has a definition in this object")
            };
            // ELF applies S + A - P, with P the address of the encoded field.
            product
                .object
                .add_relocation(
                    section,
                    object::write::Relocation {
                        offset: frame_offset + relocation.offset as u64,
                        symbol,
                        addend: relocation.addend + code_offset as i64,
                        flags: object::RelocationFlags::Generic {
                            kind: object::RelocationKind::Relative,
                            encoding: object::RelocationEncoding::Generic,
                            size: 32,
                        },
                    },
                )
                .map_err(backend)?;
        }
        Ok(())
    }
}

struct Section {
    bytes: EndianVec<LittleEndian>,
    relocations: Vec<Relocation>,
}

impl RelocateWriter for Section {
    type Writer = EndianVec<LittleEndian>;

    fn writer(&self) -> &Self::Writer {
        &self.bytes
    }

    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.bytes
    }

    fn relocate(&mut self, relocation: Relocation) {
        debug_assert_eq!(relocation.eh_pe, Some(POINTER_ENCODING));
        self.relocations.push(relocation);
    }
}
