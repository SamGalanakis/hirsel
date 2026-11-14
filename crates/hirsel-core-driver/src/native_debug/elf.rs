//! Object's ELF editor preserves code, groups and relocations while removing
//! synthetic debug sections. Section aliases let a separate debug object refer
//! to local/private code without changing its linkage or guessing function ends.
use super::*;
use gimli::write::{EndianVec, RelocateWriter, Relocation, RelocationTarget};
use object::build::Id as _;

pub(super) fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn debug(name: &str) -> bool {
    name.starts_with(".debug") && name != ".debug_frame"
}

struct Location {
    base: u64,
    end: u64,
    symbol: usize,
    alias: String,
}
pub(super) struct Addresses {
    sections: BTreeMap<usize, Location>,
}

impl Addresses {
    pub fn new(
        file: &object::File<'_>,
        identity: hirsel_core_ids::ContentHash,
        output: &mut object::write::Object<'_>,
        symbols: &mut Vec<object::write::SymbolId>,
    ) -> Result<Self, String> {
        use std::fmt::Write as _;
        let mut hash = String::new();
        for byte in identity.0 {
            write!(hash, "{byte:02x}").unwrap();
        }
        let mut sections = BTreeMap::new();
        let mut base = 1u64;
        for section in file.sections() {
            if debug(section.name().map_err(error)?)
                || section.kind() == object::SectionKind::Metadata
            {
                continue;
            }
            let end = base
                .checked_add(section.size())
                .ok_or("native debug address space exhausted")?;
            let alias = format!("__hirsel_debug_{hash}_{}", section.index().0);
            let symbol = symbols.len();
            symbols.push(output.add_symbol(object::write::Symbol {
                name: alias.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: object::SymbolKind::Unknown,
                scope: object::SymbolScope::Linkage,
                weak: false,
                section: object::write::SymbolSection::Undefined,
                flags: object::SymbolFlags::None,
            }));
            sections.insert(
                section.index().0,
                Location {
                    base,
                    end,
                    symbol,
                    alias,
                },
            );
            base = end
                .checked_add(1)
                .ok_or("native debug address space exhausted")?;
        }
        Ok(Self { sections })
    }

    pub fn address(&self, value: u64) -> Option<Address> {
        if value == 0 {
            return Some(Address::Constant(0));
        }
        self.sections
            .values()
            .find(|s| s.base <= value && value <= s.end)
            .map(|s| Address::Symbol {
                symbol: s.symbol,
                addend: (value - s.base) as i64,
            })
    }

    pub fn section(&self, file: &object::File<'_>, name: &str) -> Result<Vec<u8>, String> {
        let Some(section) = file.section_by_name(name) else {
            return Ok(vec![]);
        };
        let mut bytes = section.data().map_err(error)?.to_vec();
        for (offset, relocation) in section.relocations() {
            let (target, offset_in_target) = match relocation.target() {
                object::RelocationTarget::Symbol(id) => {
                    let symbol = file.symbol_by_index(id).map_err(error)?;
                    (
                        symbol
                            .section_index()
                            .ok_or("debug relocation has no section")?,
                        symbol.address(),
                    )
                }
                object::RelocationTarget::Section(id) => (id, 0),
                _ => return Err("unsupported debug relocation target".into()),
            };
            let base = self.sections.get(&target.0).map_or(0, |s| s.base);
            let value = base
                .wrapping_add(offset_in_target)
                .wrapping_add_signed(relocation.addend());
            let at = offset as usize;
            match relocation.size() {
                64 => bytes[at..at + 8].copy_from_slice(&value.to_le_bytes()),
                32 => bytes[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes()),
                size => return Err(format!("unsupported {size}-bit debug relocation")),
            }
        }
        Ok(bytes)
    }

    pub fn strip(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut elf = object::build::elf::Builder::read(bytes).map_err(error)?;
        // LLVM shares its symbol and section-name strings. Object's editor
        // writes those tables separately, so provide the section-name table.
        if !elf
            .sections
            .iter()
            .any(|s| matches!(s.data, object::build::elf::SectionData::SectionString))
        {
            let strings = elf.sections.add();
            strings.name = ".shstrtab".into();
            strings.sh_type = object::elf::SHT_STRTAB;
            strings.sh_addralign = 1;
            strings.data = object::build::elf::SectionData::SectionString;
        }
        let mut aliases = Vec::new();
        for section in &mut elf.sections {
            let name = section.name.as_slice();
            if name.starts_with(b".debug") && name != b".debug_frame"
                || name.starts_with(b".rela.debug") && name != b".rela.debug_frame"
                || name.starts_with(b".rel.debug") && name != b".rel.debug_frame"
                || name == b".llvm_addrsig"
            {
                section.delete = true;
            } else if let Some(location) = self.sections.get(&(section.id().index() + 1)) {
                aliases.push((section.id(), &location.alias));
            }
        }
        for (section, name) in aliases {
            let symbol = elf.symbols.add();
            symbol.name = name.as_bytes().to_vec().into();
            symbol.section = Some(section);
            symbol.set_st_info(object::elf::STB_GLOBAL, object::elf::STT_NOTYPE);
            symbol.st_other = object::elf::SymbolOther(object::elf::STV_HIDDEN.0);
        }
        let mut output = Vec::new();
        elf.write(&mut output).map_err(error)?;
        Ok(output)
    }
}

#[derive(Clone)]
pub(super) struct Section {
    bytes: EndianVec<LittleEndian>,
    relocations: Vec<Relocation>,
}
impl Default for Section {
    fn default() -> Self {
        Self {
            bytes: EndianVec::new(LittleEndian),
            relocations: vec![],
        }
    }
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
        self.relocations.push(relocation);
    }
}

pub(super) fn write_sections(
    output: &mut object::write::Object<'_>,
    sections: &write::Sections<Section>,
    symbols: &[object::write::SymbolId],
) -> Result<(), String> {
    let mut ids = BTreeMap::new();
    sections.for_each(|id, data| -> Result<(), String> {
        if !data.bytes.slice().is_empty() {
            let section = output.add_section(
                vec![],
                id.name().as_bytes().to_vec(),
                object::SectionKind::Debug,
            );
            output
                .section_mut(section)
                .set_data(data.bytes.slice().to_vec(), 1);
            ids.insert(id, section);
        }
        Ok(())
    })?;
    sections.for_each(|id, data| -> Result<(), String> {
        for relocation in &data.relocations {
            let symbol = match relocation.target {
                RelocationTarget::Symbol(id) => symbols[id],
                RelocationTarget::Section(id) => output.section_symbol(ids[&id]),
            };
            output
                .add_relocation(
                    ids[&id],
                    object::write::Relocation {
                        offset: relocation.offset as u64,
                        symbol,
                        addend: relocation.addend,
                        flags: object::RelocationFlags::Generic {
                            kind: object::RelocationKind::Absolute,
                            encoding: object::RelocationEncoding::Generic,
                            size: relocation.size * 8,
                        },
                    },
                )
                .map_err(error)?;
        }
        Ok(())
    })?;
    let stack = output.add_section(
        vec![],
        b".note.GNU-stack".to_vec(),
        object::SectionKind::Other,
    );
    output.section_mut(stack).set_data(vec![], 1);
    Ok(())
}
