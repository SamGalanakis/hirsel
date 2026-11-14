//! Bind one DWARF object from source-neutral side products, without opening code.
#[cfg(test)]
use gimli::{EndianSlice, LittleEndian};
#[cfg(test)]
use object::{Object as _, ObjectSection as _};
mod elf;
mod side;
use elf::error;
pub(crate) use side::bind as bind_side_products;
#[cfg(test)]
mod tests;
#[cfg(test)]
type R<'a> = EndianSlice<'a, LittleEndian>;
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
