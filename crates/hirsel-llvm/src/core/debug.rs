//! Extract LLVM's source-neutral DWARF once, before publishing a native product.
//! Cached linker inputs contain code and section aliases; rows live beside them.
use gimli::{EndianSlice, LittleEndian};
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::source::{
    CodeRange, CodeSection, DebugScope, LineRow, LineSequence, NativeDebug, NativeLocation,
    NativeObject, ScopeKind, debug_symbol,
};
use object::{Object as _, ObjectSection as _, ObjectSymbol as _, build::Id as _};
use std::collections::BTreeMap;

type R<'a> = EndianSlice<'a, LittleEndian>;

fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Finish a freshly emitted object, never a cached or linked one. Neutral LLVM
/// positions are item byte offsets, so this operation needs no current source.
pub fn finish_native_object(
    bytes: Vec<u8>,
    unit: ContentHash,
    debug_lines: bool,
) -> std::result::Result<NativeObject, String> {
    if !debug_lines {
        return Ok(NativeObject { bytes, debug: None });
    }
    let file = object::File::parse(bytes.as_slice()).map_err(error)?;
    let addresses = Addresses::new(&file, unit)?;
    let sections = gimli::DwarfSections::load(|id| addresses.section(&file, id.name()))?;
    let dwarf = sections.borrow(|bytes| R::new(bytes, LittleEndian));
    let mut debug = NativeDebug {
        sections: addresses.sections.iter().map(|s| s.code.clone()).collect(),
        ..NativeDebug::default()
    };
    let mut indices = BTreeMap::new();
    let mut origins = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().map_err(error)? {
        let unit = dwarf.unit(header).map_err(error)?;
        let unit = unit.unit_ref(&dwarf);
        extract_lines(unit, &addresses, &mut debug)?;
        extract_scopes(unit, &addresses, &mut debug, &mut indices, &mut origins)?;
    }
    for (index, offset) in origins {
        debug.scopes[index as usize].origin = Some(
            *indices
                .get(&offset)
                .ok_or("native debug origin is not a function scope")?,
        );
    }
    Ok(NativeObject {
        bytes: addresses.strip(&bytes)?,
        debug: Some(debug),
    })
}

struct Section {
    index: usize,
    base: u64,
    code: CodeSection,
}
struct Addresses {
    sections: Vec<Section>,
}
impl Addresses {
    fn new(file: &object::File<'_>, unit: ContentHash) -> Result<Self, String> {
        // Raw range-list offsets use the same address reader as relocated code.
        // Keep synthetic addresses above all raw section offsets.
        let mut base = file
            .sections()
            .map(|s| s.size())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("native debug address space exhausted")?;
        let mut sections = Vec::new();
        for section in file
            .sections()
            .filter(|s| s.kind() == object::SectionKind::Text)
        {
            let id = u32::try_from(sections.len()).map_err(error)?;
            sections.push(Section {
                index: section.index().0,
                base,
                code: CodeSection {
                    symbol: debug_symbol(unit, id),
                    size: section.size(),
                },
            });
            base = base
                .checked_add(section.size())
                .and_then(|v| v.checked_add(1))
                .ok_or("native debug address space exhausted")?;
        }
        Ok(Self { sections })
    }
    fn address(&self, value: u64) -> Result<(u32, u64), String> {
        self.sections
            .iter()
            .enumerate()
            .find(|(_, s)| s.base <= value && value - s.base <= s.code.size)
            .map(|(id, s)| (id as u32, value - s.base))
            .ok_or_else(|| format!("native debug address {value:#x} has no code section"))
    }
    fn range(&self, begin: u64, end: u64) -> Result<CodeRange, String> {
        let (section, begin) = self.address(begin)?;
        let (end_section, end) = self.address(end)?;
        if section != end_section || end < begin {
            return Err("native debug range crosses code sections".into());
        }
        Ok(CodeRange {
            section,
            begin,
            end,
        })
    }
    fn section(&self, file: &object::File<'_>, name: &str) -> Result<Vec<u8>, String> {
        let Some(section) = file.section_by_name(name) else {
            return Ok(vec![]);
        };
        let mut bytes = section.data().map_err(error)?.to_vec();
        for (offset, relocation) in section.relocations() {
            let (target, addend) = match relocation.target() {
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
            let base = self
                .sections
                .iter()
                .find(|s| s.index == target.0)
                .map_or(0, |s| s.base);
            let value = base
                .wrapping_add(addend)
                .wrapping_add_signed(relocation.addend());
            let at = usize::try_from(offset).map_err(error)?;
            let data: Vec<u8> = match relocation.size() {
                64 => value.to_le_bytes().to_vec(),
                32 => u32::try_from(value).map_err(error)?.to_le_bytes().to_vec(),
                size => return Err(format!("unsupported {size}-bit debug relocation")),
            };
            bytes
                .get_mut(at..at + data.len())
                .ok_or("debug relocation outside section")?
                .copy_from_slice(&data);
        }
        Ok(bytes)
    }
    fn strip(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut elf = object::build::elf::Builder::read(bytes).map_err(error)?;
        // LLVM can share the symbol and section-name string tables. The editor
        // emits them separately, and needs an explicit section-name table.
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
            } else if let Some(location) = self
                .sections
                .iter()
                .find(|s| s.index == section.id().index() + 1)
            {
                aliases.push((section.id(), &location.code.symbol));
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

fn item(unit: gimli::UnitRef<'_, R<'_>>, index: u64) -> Result<Option<DefPath>, String> {
    let Some(header) = unit.line_program.as_ref().map(|p| p.header()) else {
        return Ok(None);
    };
    let Some(file) = header.file(index) else {
        return Ok(None);
    };
    let path = unit.attr_string(file.path_name()).map_err(error)?;
    let path = path.to_string_lossy();
    if let Some(item) = DefPath::from_full(&path) {
        return Ok(Some(item));
    }
    let Some(directory) = file.directory(header) else {
        return Ok(None);
    };
    let directory = unit.attr_string(directory).map_err(error)?;
    Ok(DefPath::from_full(&format!(
        "{}/{path}",
        directory.to_string_lossy()
    )))
}
fn location(
    unit: gimli::UnitRef<'_, R<'_>>,
    file: u64,
    line: u64,
    debug: &mut NativeDebug,
) -> Result<Option<NativeLocation>, String> {
    let Some(start) = line.checked_sub(1) else {
        return Ok(None);
    };
    let Some(item) = item(unit, file)? else {
        return Ok(None);
    };
    let index = match debug.items.iter().position(|p| p == &item) {
        Some(index) => index,
        None => {
            debug.items.push(item);
            debug.items.len() - 1
        }
    };
    Ok(Some(NativeLocation {
        item: u32::try_from(index).map_err(error)?,
        start: u32::try_from(start).map_err(error)?,
    }))
}
fn extract_lines(
    unit: gimli::UnitRef<'_, R<'_>>,
    addresses: &Addresses,
    debug: &mut NativeDebug,
) -> Result<(), String> {
    let Some(program) = unit.line_program.clone() else {
        return Ok(());
    };
    let mut rows = program.rows();
    let mut pending = Vec::new();
    let mut begin = None;
    while let Some((_, row)) = rows.next_row().map_err(error)? {
        if row.end_sequence() {
            if let Some(begin) = begin.take() {
                debug.sequences.push(LineSequence {
                    range: addresses.range(begin, row.address())?,
                    rows: std::mem::take(&mut pending),
                });
            }
            continue;
        }
        begin.get_or_insert(row.address());
        let (_, offset) = addresses.address(row.address())?;
        let source = location(
            unit,
            row.file_index(),
            row.line().map_or(0, |v| v.get()),
            debug,
        )?;
        pending.push(LineRow { offset, source });
    }
    if !pending.is_empty() {
        return Err("unterminated native debug line sequence".into());
    }
    Ok(())
}

fn entry_location(
    unit: gimli::UnitRef<'_, R<'_>>,
    entry: &gimli::DebuggingInformationEntry<R<'_>>,
    file: gimli::DwAt,
    line: gimli::DwAt,
    debug: &mut NativeDebug,
) -> Result<Option<NativeLocation>, String> {
    let Some(gimli::AttributeValue::FileIndex(file)) = entry.attr_value(file) else {
        return Ok(None);
    };
    let Some(line) = entry.attr_value(line).and_then(|v| v.udata_value()) else {
        return Ok(None);
    };
    location(unit, file, line, debug)
}
fn string<'a>(
    unit: gimli::UnitRef<'_, R<'a>>,
    entry: &gimli::DebuggingInformationEntry<R<'a>>,
    attr: gimli::DwAt,
) -> Result<String, String> {
    entry
        .attr_value(attr)
        .map(|v| {
            unit.attr_string(v)
                .map(|v| v.to_string_lossy().into_owned())
                .map_err(error)
        })
        .unwrap_or_else(|| Ok(String::new()))
}
fn extract_scopes(
    unit: gimli::UnitRef<'_, R<'_>>,
    addresses: &Addresses,
    debug: &mut NativeDebug,
    indices: &mut BTreeMap<gimli::DebugInfoOffset<usize>, u32>,
    origins: &mut Vec<(u32, gimli::DebugInfoOffset<usize>)>,
) -> Result<(), String> {
    let mut entries = unit.entries();
    let mut parents = Vec::new();
    while let Some(entry) = entries.next_dfs().map_err(error)? {
        while parents
            .last()
            .is_some_and(|(depth, _)| *depth >= entry.depth())
        {
            parents.pop();
        }
        let kind = match entry.tag() {
            gimli::DW_TAG_subprogram => ScopeKind::Function {
                linkage: string(unit, entry, gimli::DW_AT_linkage_name)?,
            },
            gimli::DW_TAG_inlined_subroutine => ScopeKind::Inline,
            _ => continue,
        };
        let index = u32::try_from(debug.scopes.len()).map_err(error)?;
        indices.insert(
            entry
                .offset()
                .to_debug_info_offset(&unit.header)
                .ok_or("native debug scope outside .debug_info")?,
            index,
        );
        if let Some(origin) = entry.attr_value(gimli::DW_AT_abstract_origin) {
            match origin {
                gimli::AttributeValue::UnitRef(offset) => origins.push((
                    index,
                    offset
                        .to_debug_info_offset(&unit.header)
                        .ok_or("native debug origin outside .debug_info")?,
                )),
                gimli::AttributeValue::DebugInfoRef(offset) => origins.push((index, offset)),
                _ => return Err("unsupported cross-unit native debug origin".into()),
            }
        }
        let mut ranges = unit.die_ranges(entry).map_err(error)?;
        let mut code = Vec::new();
        while let Some(range) = ranges.next().map_err(error)? {
            if range.begin != range.end {
                code.push(addresses.range(range.begin, range.end)?);
            }
        }
        let declaration = entry_location(
            unit,
            entry,
            gimli::DW_AT_decl_file,
            gimli::DW_AT_decl_line,
            debug,
        )?;
        let call = entry_location(
            unit,
            entry,
            gimli::DW_AT_call_file,
            gimli::DW_AT_call_line,
            debug,
        )?;
        debug.scopes.push(DebugScope {
            parent: parents.last().map(|(_, index)| *index),
            origin: None,
            kind,
            name: string(unit, entry, gimli::DW_AT_name)?,
            declaration,
            call,
            ranges: code,
        });
        parents.push((entry.depth(), index));
    }
    Ok(())
}

#[cfg(all(test, not(hirsel_lazy_core)))]
mod tests;
