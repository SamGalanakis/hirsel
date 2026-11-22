//! One link-time compilation unit built from source-neutral side products.
//! No code object is opened or changed here.
use super::{elf, error};
use gimli::write::{self, Address, AttributeValue, FileId, LineProgram, LineString, UnitEntryId};
use hirsel_core_ids::DefPath;
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
    release: FxHashMap<DefPath, Vec<Option<(ItemSource, u32)>>>,
}
impl Sources<'_> {
    fn position(
        &mut self,
        items: &[DefPath],
        location: Option<NativeLocation>,
        lines: &mut LineProgram,
    ) -> Option<Position> {
        let location = location?;
        let owner = &items[location.item as usize];
        let (item, start) = if let Some(&item) = self.map.items.get(owner) {
            (item, location.start)
        } else {
            self.release
                .get(owner)?
                .get(location.start as usize)
                .copied()
                .flatten()?
        };
        let index = self.files.entry(item.file).or_insert_with(|| {
            let source = &self.map.files[item.file as usize];
            let id = lines.add_file(
                LineString::String(source.path.as_bytes().to_vec()),
                lines.default_directory(),
                None,
            );
            FileIndex::new(id, &source.text)
        });
        let offset = item.file_offset as usize + start as usize;
        if let Some(points) = self
            .map
            .points
            .get(&self.map.files[item.file as usize].path)
        {
            let &(line, column) = points.get(offset)?;
            return Some(Position {
                file: index.id,
                line: u64::from(line),
                column: u64::from(column),
            });
        }
        Some(index.position(offset))
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

fn range(
    unit: &mut write::Unit,
    id: UnitEntryId,
    ranges: &[CodeRange],
    address: impl Fn(CodeRange) -> Option<Address>,
) {
    let ranges = ranges
        .iter()
        .filter_map(|&r| {
            Some(write::Range::StartLength {
                begin: address(r)?,
                length: r.end - r.begin,
            })
        })
        .collect::<Vec<_>>();
    if ranges.is_empty() {
        return;
    }
    let ranges = unit.ranges.add(write::RangeList(ranges));
    unit.get_mut(id)
        .set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));
}

pub(crate) fn bind<'a>(
    products: impl IntoIterator<Item = &'a NativeDebug>,
    sources: &SourceMap,
    release: &[ReleaseSite],
    target: &str,
) -> Result<Vec<u8>, String> {
    let architecture = match hirsel_core_layout::Target::from_triple(target) {
        Some(hirsel_core_layout::Target::X86_64Linux) => object::Architecture::X86_64,
        Some(hirsel_core_layout::Target::Aarch64Linux) => object::Architecture::Aarch64,
        None => return Err(format!("no native debug target {target}")),
    };
    let mut object = debug_object(architecture);
    let mut symbols = Vec::new();
    let mut dwarf = unit(products, sources, release, |section| {
        let index = symbols.len();
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
        Some(Address::Symbol {
            symbol: index,
            addend: 0,
        })
    });
    let mut sections = write::Sections::new(elf::Section::default());
    dwarf.write(&mut sections).map_err(error)?;
    elf::write_sections(&mut object, &sections, &symbols)?;
    object.write().map_err(error)
}

fn debug_object(architecture: object::Architecture) -> object::write::Object<'static> {
    object::write::Object::new(
        object::BinaryFormat::Elf,
        architecture,
        object::Endianness::Little,
    )
}

pub(crate) fn bind_linked<'a>(
    products: impl IntoIterator<Item = &'a NativeDebug>,
    sources: &SourceMap,
    release: &[ReleaseSite],
    image: &[u8],
) -> Result<Vec<u8>, String> {
    use object::{Object as _, ObjectSymbol as _};
    let file = object::File::parse(image).map_err(error)?;
    let symbols = file
        .symbols()
        .filter_map(|symbol| {
            let name = symbol.name().ok()?;
            (name.starts_with("__hirsel_debug_") && symbol.section_index().is_some())
                .then_some((name, symbol.address()))
        })
        .collect::<FxHashMap<_, _>>();
    let mut dwarf = unit(products, sources, release, |section| {
        symbols
            .get(section.symbol.as_str())
            .copied()
            .map(Address::Constant)
    });
    let mut object = debug_object(file.architecture());
    let mut sections = write::Sections::new(write::EndianVec::new(gimli::LittleEndian));
    dwarf.write(&mut sections).map_err(error)?;
    sections.for_each(|id, section| {
        if !section.slice().is_empty() {
            let index = object.add_section(
                vec![],
                id.name().as_bytes().to_vec(),
                object::SectionKind::Debug,
            );
            object.append_section_data(index, section.slice(), 1);
        }
        Ok::<_, String>(())
    })?;
    super::detached::Identity::of(&file, image)
        .map_err(error)?
        .write_note(&mut object);
    object.write().map_err(error)
}

fn unit<'a>(
    products: impl IntoIterator<Item = &'a NativeDebug>,
    sources: &SourceMap,
    release: &[ReleaseSite],
    mut section_address: impl FnMut(&CodeSection) -> Option<Address>,
) -> write::DwarfUnit {
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
        release: FxHashMap::default(),
    };
    for binding in release {
        let item = sources.map.items.get(&binding.item).copied();
        let rows = sources
            .release
            .entry(release_debug_owner(&binding.item))
            .or_default();
        debug_assert_eq!(rows.len(), binding.ordinal as usize);
        rows.push(item.map(|item| (item, binding.start)));
    }
    let mut unit_ranges = Vec::new();
    let mut abstracts = FxHashMap::default();
    for product in products {
        let addresses = product
            .sections
            .iter()
            .map(&mut section_address)
            .collect::<Vec<_>>();
        for (section, &begin) in product.sections.iter().zip(&addresses) {
            if let Some(begin) = begin.filter(|_| section.size != 0) {
                unit_ranges.push(write::Range::StartLength {
                    begin,
                    length: section.size,
                });
            }
        }
        let address = |r: CodeRange| match addresses[r.section as usize]? {
            Address::Constant(base) => Some(Address::Constant(base + r.begin)),
            Address::Symbol { symbol, addend } => Some(Address::Symbol {
                symbol,
                addend: addend + r.begin as i64,
            }),
        };
        let mut positions = FxHashMap::default();
        let mut position = |loc: Option<NativeLocation>, lines: &mut LineProgram| {
            *positions
                .entry(loc)
                .or_insert_with(|| sources.position(&product.items, loc, lines))
        };
        for sequence in &product.sequences {
            let r = sequence.range;
            let Some(begin) = address(r) else { continue };
            lines.begin_sequence(Some(begin));
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
            range(&mut dwarf.unit, id, &scope.ranges, address);
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
    dwarf
}

#[cfg(test)]
pub(crate) mod tests;
