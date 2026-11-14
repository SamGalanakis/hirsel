//! Bind source-neutral DWARF from either engine in the fresh link product.
//! The interchange is ordinary DWARF: file = DefPath, line = byte offset + 1.
//! Gimli's conversion keeps ranges, scopes and inline origins intact; only
//! source coordinates and code-address relocations change at this boundary.
use gimli::write::{self, Address, AttributeValue, LineProgram, LineString};
use gimli::{EndianSlice, LittleEndian};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::source::SourceMap;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use std::collections::BTreeMap;

mod elf;
use elf::{Addresses, Section, error};
#[cfg(test)]
mod tests;

type R<'a> = EndianSlice<'a, LittleEndian>;

/// Engines may still include rows while their policy seam is being consumed.
/// Off means no debug object and no synthetic coordinates in the executable.
pub(crate) fn code_only(objects: &[super::ObjectProduct]) -> Result<Vec<Vec<u8>>, String> {
    objects
        .iter()
        .map(|object| {
            let file = object::File::parse(&*object.bytes).map_err(error)?;
            if file.section_by_name(".debug_info").is_some() {
                Addresses::default().strip(&object.bytes)
            } else {
                Ok(object.bytes.clone())
            }
        })
        .collect()
}

pub(crate) fn bind(
    objects: &[super::ObjectProduct],
    sources: &SourceMap,
    target: &str,
) -> Result<(Vec<Vec<u8>>, Vec<u8>), String> {
    let architecture = match hirsel_core_layout::Target::from_triple(target) {
        Some(hirsel_core_layout::Target::X86_64Linux) => object::Architecture::X86_64,
        Some(hirsel_core_layout::Target::Aarch64Linux) => object::Architecture::Aarch64,
        None => return Err(format!("no native debug target {target}")),
    };
    let mut output = object::write::Object::new(
        object::BinaryFormat::Elf,
        architecture,
        object::Endianness::Little,
    );
    let mut dwarf = write::Dwarf::new();
    let mut symbols = Vec::new();
    let mut code = Vec::with_capacity(objects.len());
    for object in objects {
        let file = object::File::parse(&*object.bytes).map_err(error)?;
        if file.section_by_name(".debug_info").is_none() {
            code.push(object.bytes.clone());
            continue;
        }
        let addresses = Addresses::new(&file, object.identity, &mut output, &mut symbols)?;
        let sections = gimli::DwarfSections::load(|id| addresses.section(&file, id.name()))?;
        let read = sections.borrow(|bytes| R::new(bytes, LittleEndian));
        let mut convert = dwarf.convert(&read).map_err(error)?;
        while let Some((mut unit, root)) = convert.read_unit().map_err(error)? {
            let items = lines(&mut unit, sources, &addresses)?;
            let root_id = unit.unit.root();
            attributes(&mut unit, root_id, &root, &items, sources, &addresses)?;
            let mut entry = root;
            while let Some(id) = unit.read_entry(&mut entry).map_err(error)? {
                let id = unit.add_entry(id, &entry);
                attributes(&mut unit, id, &entry, &items, sources, &addresses)?;
            }
        }
        code.push(addresses.strip(&object.bytes)?);
    }
    let mut sections = write::Sections::new(Section::default());
    dwarf.write(&mut sections).map_err(error)?;
    elf::write_sections(&mut output, &sections, &symbols)?;
    Ok((code, output.write().map_err(error)?))
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
    let directory = directory.to_string_lossy();
    Ok(DefPath::from_full(&format!("{directory}/{path}")))
}

fn lines(
    unit: &mut write::ConvertUnit<'_, R<'_>>,
    sources: &SourceMap,
    addresses: &Addresses,
) -> Result<Vec<Option<DefPath>>, String> {
    let read_unit = unit.read_unit;
    let encoding = unit.unit.encoding();
    let line_encoding = read_unit
        .line_program
        .as_ref()
        .map(|p| p.header().line_encoding());
    let Some(mut converted) = unit.read_line_program(None, None).map_err(error)? else {
        return Ok(vec![]);
    };
    let mut events = Vec::new();
    while let Some(row) = converted.read_row().map_err(error)? {
        events.push(row);
    }
    let (_, file_ids) = converted.program();
    let mut program = LineProgram::new(
        encoding,
        line_encoding.unwrap(),
        LineString::String(vec![]),
        None,
        LineString::String(b"Hirsel".to_vec()),
        None,
    );
    let mut items = Vec::new();
    let mut mapping = Vec::new();
    let mut row_items = rustc_hash::FxHashMap::default();
    for (index, old_id) in file_ids.iter().enumerate() {
        let item = item(read_unit, index as u64)?;
        let path = item
            .as_ref()
            .and_then(|p| sources.items.get(p))
            .map(|at| sources.files[at.file as usize].path.as_bytes().to_vec())
            .unwrap_or_else(|| b"Hirsel".to_vec());
        let id = program.add_file(LineString::String(path), program.default_directory(), None);
        row_items.insert(*old_id, (item.clone(), id));
        items.push(item);
        mapping.push(id);
    }
    let mut address = None;
    for event in events {
        match event {
            write::ConvertLineRow::SetAddress(value) => {
                address = addresses.address(value);
                if program.in_sequence() {
                    program.set_address(address.ok_or("unresolved debug address")?);
                }
            }
            write::ConvertLineRow::Row(mut row) => {
                let (item, file) = &row_items[&row.file];
                let at = item.as_ref().and_then(|item| {
                    row.line
                        .checked_sub(1)
                        .and_then(|start| u32::try_from(start).ok())
                        .and_then(|start| sources.position(item, start))
                });
                if let Some((_, line, column)) = at {
                    if !program.in_sequence() {
                        program.begin_sequence(address);
                    }
                    row.file = *file;
                    row.line = u64::from(line);
                    row.column = u64::from(column);
                    *program.row() = row;
                    program.generate_row();
                } else if program.in_sequence() {
                    program.end_sequence(row.address_offset);
                }
            }
            write::ConvertLineRow::EndSequence(end) => {
                if program.in_sequence() {
                    program.end_sequence(end);
                }
                address = None;
            }
        }
    }
    unit.set_line_program(program, mapping);
    Ok(items)
}

fn attributes<'data>(
    unit: &mut write::ConvertUnit<'_, R<'data>>,
    id: write::UnitEntryId,
    entry: &write::ConvertUnitEntry<'_, R<'data>>,
    items: &[Option<DefPath>],
    sources: &SourceMap,
    addresses: &Addresses,
) -> Result<(), String> {
    for attr in entry.attrs() {
        let value = unit
            .convert_attribute_value(entry.read_unit, attr, &|v| addresses.address(v))
            .map_err(error)?;
        unit.unit.get_mut(id).set(attr.name(), value);
    }
    for (file, line, column) in [
        (
            gimli::DW_AT_decl_file,
            gimli::DW_AT_decl_line,
            gimli::DW_AT_decl_column,
        ),
        (
            gimli::DW_AT_call_file,
            gimli::DW_AT_call_line,
            gimli::DW_AT_call_column,
        ),
    ] {
        let index = match entry.attr_value(file) {
            Some(gimli::AttributeValue::FileIndex(index)) => Some(index),
            _ => None,
        };
        let offset = entry
            .attr_value(line)
            .and_then(|v| v.udata_value())
            .and_then(|v| v.checked_sub(1))
            .and_then(|v| u32::try_from(v).ok());
        let at = index.zip(offset).and_then(|(index, offset)| {
            items
                .get(index as usize)
                .and_then(Option::as_ref)
                .and_then(|item| sources.position(item, offset))
        });
        let target = unit.unit.get_mut(id);
        if let Some((_, actual_line, actual_column)) = at {
            target.set(line, AttributeValue::Udata(u64::from(actual_line)));
            target.set(column, AttributeValue::Udata(u64::from(actual_column)));
        } else {
            target.delete(file);
            target.delete(line);
            target.delete(column);
        }
    }
    if entry.tag() == gimli::DW_TAG_compile_unit {
        unit.unit
            .get_mut(id)
            .set(gimli::DW_AT_comp_dir, AttributeValue::String(vec![]));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn source_positions(bytes: &[u8]) -> Result<Vec<(String, u32, u32)>, String> {
    let file = object::File::parse(bytes).map_err(error)?;
    let sections = gimli::DwarfSections::load(|id| -> Result<Vec<u8>, String> {
        file.section_by_name(id.name())
            .map(|s| s.data().map(Vec::from).map_err(error))
            .unwrap_or_else(|| Ok(vec![]))
    })?;
    let dwarf = sections.borrow(|bytes| R::new(bytes, LittleEndian));
    let mut units = dwarf.units();
    let mut result = vec![];
    while let Some(header) = units.next().map_err(error)? {
        let unit = dwarf.unit(header).map_err(error)?;
        let Some(program) = unit.line_program.clone() else {
            continue;
        };
        let mut rows = program.rows();
        while let Some((header, row)) = rows.next_row().map_err(error)? {
            if row.end_sequence() {
                continue;
            }
            let Some(file) = row.file(header) else {
                continue;
            };
            let path = dwarf.attr_string(&unit, file.path_name()).map_err(error)?;
            let mut path = path.to_string_lossy().into_owned();
            if !std::path::Path::new(&path).is_absolute() {
                if let Some(directory) = file.directory(header) {
                    let directory = dwarf.attr_string(&unit, directory).map_err(error)?;
                    let directory = directory.to_string_lossy();
                    if !directory.is_empty() {
                        path = format!("{directory}/{path}");
                    }
                }
            }
            let column = match row.column() {
                gimli::ColumnType::LeftEdge => 0,
                gimli::ColumnType::Column(column) => column.get() as u32,
            };
            if let Some(line) = row.line() {
                result.push((path, line.get() as u32, column));
            }
        }
    }
    Ok(result)
}
