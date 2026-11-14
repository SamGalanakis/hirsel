//! One link-time compilation unit built from source-neutral side products.
//! No code object is opened or changed here.
use super::{elf, error};
use gimli::write::{self, Address, AttributeValue, FileId, LineProgram, LineString, UnitEntryId};
use hirsel_core_ir::source::*;
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct Position {
    file: FileId,
    line: u64,
    column: u64,
}

struct FileIndex {
    id: FileId,
    lines: Vec<usize>,
    // Cumulative excess UTF-8 bytes, indexed at each non-ASCII scalar's end.
    excess: Vec<(usize, usize)>,
}
impl FileIndex {
    fn new(id: FileId, text: &str) -> Self {
        let mut lines = vec![0];
        let mut excess = Vec::new();
        let mut total = 0;
        for (at, c) in text.char_indices() {
            if c == '\n' {
                lines.push(at + 1);
            } else if !c.is_ascii() {
                total += c.len_utf8() - 1;
                excess.push((at + c.len_utf8(), total));
            }
        }
        Self { id, lines, excess }
    }
    fn excess(&self, byte: usize) -> usize {
        let n = self.excess.partition_point(|&(at, _)| at <= byte);
        n.checked_sub(1).map_or(0, |i| self.excess[i].1)
    }
    fn position(&self, byte: usize) -> Position {
        let line = self.lines.partition_point(|&at| at <= byte) - 1;
        let start = self.lines[line];
        Position {
            file: self.id,
            line: line as u64 + 1,
            column: (byte - start - (self.excess(byte) - self.excess(start))) as u64 + 1,
        }
    }
}

struct Sources<'a> {
    map: &'a SourceMap,
    files: FxHashMap<u32, FileIndex>,
}
impl Sources<'_> {
    fn position(
        &mut self,
        items: &[Option<ItemSource>],
        location: Option<NativeLocation>,
        lines: &mut LineProgram,
    ) -> Option<Position> {
        let location = location?;
        let item = items[location.item as usize]?;
        let index = self.files.entry(item.file).or_insert_with(|| {
            let source = &self.map.files[item.file as usize];
            let id = lines.add_file(
                LineString::String(source.path.as_bytes().to_vec()),
                lines.default_directory(),
                None,
            );
            FileIndex::new(id, &source.text)
        });
        Some(index.position(item.file_offset as usize + location.start as usize))
    }
}

fn location(unit: &mut write::Unit, id: UnitEntryId, p: Option<Position>, call: bool) {
    if let Some(p) = p {
        let (file, line, column) = if call {
            (
                gimli::DW_AT_call_file,
                gimli::DW_AT_call_line,
                gimli::DW_AT_call_column,
            )
        } else {
            (
                gimli::DW_AT_decl_file,
                gimli::DW_AT_decl_line,
                gimli::DW_AT_decl_column,
            )
        };
        let entry = unit.get_mut(id);
        entry.set(file, AttributeValue::FileIndex(Some(p.file)));
        entry.set(line, AttributeValue::Udata(p.line));
        entry.set(column, AttributeValue::Udata(p.column));
    }
}

fn range(unit: &mut write::Unit, id: UnitEntryId, ranges: &[CodeRange], base: usize) {
    if ranges.is_empty() {
        return;
    }
    let ranges = unit.ranges.add(write::RangeList(
        ranges
            .iter()
            .map(|r| write::Range::StartLength {
                begin: Address::Symbol {
                    symbol: base + r.section as usize,
                    addend: r.begin as i64,
                },
                length: r.end - r.begin,
            })
            .collect(),
    ));
    unit.get_mut(id)
        .set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));
}

pub(crate) fn bind<'a>(
    products: impl IntoIterator<Item = &'a NativeDebug>,
    sources: &SourceMap,
    target: &str,
) -> Result<Vec<u8>, String> {
    let architecture = match hirsel_core_layout::Target::from_triple(target) {
        Some(hirsel_core_layout::Target::X86_64Linux) => object::Architecture::X86_64,
        Some(hirsel_core_layout::Target::Aarch64Linux) => object::Architecture::Aarch64,
        None => return Err(format!("no native debug target {target}")),
    };
    let mut object = object::write::Object::new(
        object::BinaryFormat::Elf,
        architecture,
        object::Endianness::Little,
    );
    let encoding = gimli::Encoding {
        format: gimli::Format::Dwarf32,
        version: 5,
        address_size: 8,
    };
    let mut dwarf = write::DwarfUnit::new(encoding);
    let root = dwarf.unit.root();
    dwarf.unit.get_mut(root).set(
        gimli::DW_AT_name,
        AttributeValue::String(b"Hirsel".to_vec()),
    );
    dwarf.unit.get_mut(root).set(
        gimli::DW_AT_producer,
        AttributeValue::String(b"Hirsel".to_vec()),
    );
    let mut lines = LineProgram::new(
        encoding,
        gimli::LineEncoding::default(),
        LineString::String(vec![]),
        None,
        LineString::String(b"Hirsel".to_vec()),
        None,
    );
    let mut sources = Sources {
        map: sources,
        files: FxHashMap::default(),
    };
    let mut symbols = Vec::new();
    let mut unit_ranges = Vec::new();
    let mut abstracts = FxHashMap::default();
    for product in products {
        let base = symbols.len();
        for section in &product.sections {
            if section.size != 0 {
                unit_ranges.push(write::Range::StartLength {
                    begin: Address::Symbol {
                        symbol: symbols.len(),
                        addend: 0,
                    },
                    length: section.size,
                });
            }
            symbols.push(object.add_symbol(object::write::Symbol {
                name: section.symbol.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: object::SymbolKind::Unknown,
                scope: object::SymbolScope::Linkage,
                weak: false,
                section: object::write::SymbolSection::Undefined,
                flags: object::SymbolFlags::None,
            }));
        }
        let items = product
            .items
            .iter()
            .map(|item| sources.map.items.get(item).copied())
            .collect::<Vec<_>>();
        let mut positions = FxHashMap::default();
        let mut position = |loc: Option<NativeLocation>, lines: &mut LineProgram| {
            *positions
                .entry(loc)
                .or_insert_with(|| sources.position(&items, loc, lines))
        };
        for sequence in &product.sequences {
            let r = sequence.range;
            lines.begin_sequence(Some(Address::Symbol {
                symbol: base + r.section as usize,
                addend: r.begin as i64,
            }));
            for row in &sequence.rows {
                let p = position(row.source, &mut lines);
                let target = lines.row();
                target.address_offset = row.offset - r.begin;
                if let Some(p) = p {
                    target.file = p.file;
                    target.line = p.line;
                    target.column = p.column;
                    target.is_statement = true;
                } else {
                    target.line = 0;
                    target.column = 0;
                    target.is_statement = false;
                }
                lines.generate_row();
            }
            lines.end_sequence(r.end - r.begin);
        }
        let mut scopes = Vec::with_capacity(product.scopes.len());
        for scope in &product.scopes {
            let parent = scope.parent.map_or(root, |id| scopes[id as usize]);
            let declaration = position(scope.declaration, &mut lines);
            let abstract_key = match &scope.kind {
                ScopeKind::Function { linkage } if scope.ranges.is_empty() => {
                    Some((linkage.clone(), scope.name.clone(), declaration))
                }
                _ => None,
            };
            if let Some(id) = abstract_key.as_ref().and_then(|key| abstracts.get(key)) {
                scopes.push(*id);
                continue;
            }
            let tag = match scope.kind {
                ScopeKind::Function { .. } => gimli::DW_TAG_subprogram,
                ScopeKind::Inline => gimli::DW_TAG_inlined_subroutine,
            };
            let id = dwarf.unit.add(parent, tag);
            scopes.push(id);
            if let Some(key) = abstract_key {
                abstracts.insert(key, id);
                dwarf.unit.get_mut(id).set(
                    gimli::DW_AT_inline,
                    AttributeValue::Inline(gimli::DW_INL_inlined),
                );
            }
            let name = dwarf.strings.add(scope.name.as_bytes());
            dwarf
                .unit
                .get_mut(id)
                .set(gimli::DW_AT_name, AttributeValue::StringRef(name));
            if let ScopeKind::Function { linkage } = &scope.kind {
                let linkage = dwarf.strings.add(linkage.as_bytes());
                dwarf.unit.get_mut(id).set(
                    gimli::DW_AT_linkage_name,
                    AttributeValue::StringRef(linkage),
                );
            }
            location(&mut dwarf.unit, id, declaration, false);
            location(&mut dwarf.unit, id, position(scope.call, &mut lines), true);
            range(&mut dwarf.unit, id, &scope.ranges, base);
        }
        for (scope, id) in product.scopes.iter().zip(&scopes) {
            if let Some(origin) = scope.origin {
                dwarf.unit.get_mut(*id).set(
                    gimli::DW_AT_abstract_origin,
                    AttributeValue::UnitRef(scopes[origin as usize]),
                );
            }
        }
    }
    if !unit_ranges.is_empty() {
        let ranges = dwarf.unit.ranges.add(write::RangeList(unit_ranges));
        dwarf
            .unit
            .get_mut(root)
            .set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));
    }
    dwarf.unit.line_program = lines;
    let mut sections = write::Sections::new(elf::Section::default());
    dwarf.write(&mut sections).map_err(error)?;
    elf::write_sections(&mut object, &sections, &symbols)?;
    object.write().map_err(error)
}

#[cfg(test)]
pub(crate) mod tests;
