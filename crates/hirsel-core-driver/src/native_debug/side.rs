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
    lines: Vec<usize>,
    // Cumulative excess UTF-8 bytes, indexed at each non-ASCII scalar's end.
    excess: Vec<(usize, usize)>,
}
impl FileIndex {
    fn new(text: &str) -> Self {
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
        Self { lines, excess }
    }
    fn excess(&self, byte: usize) -> usize {
        let n = self.excess.partition_point(|&(at, _)| at <= byte);
        n.checked_sub(1).map_or(0, |i| self.excess[i].1)
    }
    fn position(&self, byte: usize) -> (u64, u64) {
        let line = self.lines.partition_point(|&at| at <= byte) - 1;
        let start = self.lines[line];
        (
            line as u64 + 1,
            (byte - start - (self.excess(byte) - self.excess(start))) as u64 + 1,
        )
    }
}

/// A file some position names: its annex points or, failing those, its line
/// index once needed.
struct File<'a> {
    points: Option<&'a [(u32, u32)]>,
    index: Option<FileIndex>,
}

/// Where one location of a product's rows lands: the map's file, when
/// resolving the location reached one, and the line and column there.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Located {
    file: Option<u32>,
    at: Option<(u64, u64)>,
}

/// A product's locations in first-lookup order, each with where it lands
/// (`crate::resident::DEBUG_POSITIONS`).
pub(crate) type Locations = std::sync::Arc<[(Option<NativeLocation>, Located)]>;

struct Sources<'a> {
    map: &'a SourceMap,
    files: FxHashMap<u32, File<'a>>,
    ids: FxHashMap<u32, FileId>,
    release: FxHashMap<DefPath, Vec<Option<(ItemSource, u32)>>>,
}
impl<'a> Sources<'a> {
    fn new(map: &'a SourceMap, release: &[ReleaseSite]) -> Self {
        let mut sources = Self {
            map,
            files: FxHashMap::default(),
            ids: FxHashMap::default(),
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
        sources
    }

    fn resolve(&self, owner: &DefPath, start: u32) -> Option<(ItemSource, u32)> {
        if let Some(&item) = self.map.items.get(owner) {
            Some((item, start))
        } else {
            self.release
                .get(owner)?
                .get(start as usize)
                .copied()
                .flatten()
        }
    }

    /// Where `location` lands in a product whose items are `items`; `owners`
    /// remembers each item's map entry across the product's lookups.
    fn locate(
        &mut self,
        items: &[DefPath],
        owners: &mut [Option<Option<ItemSource>>],
        location: Option<NativeLocation>,
    ) -> Located {
        let resolved = location.and_then(|location| {
            let index = location.item as usize;
            let map = self.map;
            match *owners[index].get_or_insert_with(|| map.items.get(&items[index]).copied()) {
                Some(item) => Some((item, location.start)),
                None => self
                    .release
                    .get(&items[index])?
                    .get(location.start as usize)
                    .copied()
                    .flatten(),
            }
        });
        match resolved {
            Some((item, start)) => Located {
                file: Some(item.file),
                at: self.coordinates(item, start),
            },
            None => Located {
                file: None,
                at: None,
            },
        }
    }

    fn coordinates(&mut self, item: ItemSource, start: u32) -> Option<(u64, u64)> {
        let map = self.map;
        let source = &map.files[item.file as usize];
        let file = self.files.entry(item.file).or_insert_with(|| File {
            points: map.points.get(&source.path).map(Vec::as_slice),
            index: None,
        });
        let offset = item.file_offset as usize + start as usize;
        match file.points {
            Some(points) => {
                let &(line, column) = points.get(offset)?;
                Some((u64::from(line), u64::from(column)))
            }
            None => Some(
                file.index
                    .get_or_insert_with(|| FileIndex::new(&source.text))
                    .position(offset),
            ),
        }
    }

    /// The line program's entry for a located position, its file added at
    /// the first position to name it.
    fn position(&mut self, located: Located, lines: &mut LineProgram) -> Option<Position> {
        let file = located.file?;
        let map = self.map;
        let id = *self.ids.entry(file).or_insert_with(|| {
            lines.add_file(
                LineString::String(map.files[file as usize].path.as_bytes().to_vec()),
                lines.default_directory(),
                None,
            )
        });
        let (line, column) = located.at?;
        Some(Position {
            file: id,
            line,
            column,
        })
    }

    fn position_at(
        &mut self,
        item: ItemSource,
        start: u32,
        lines: &mut LineProgram,
    ) -> Option<Position> {
        let located = Located {
            file: Some(item.file),
            at: self.coordinates(item, start),
        };
        self.position(located, lines)
    }
}

fn write_remarks(
    object: &mut object::write::Object<'_>,
    lines: &mut LineProgram,
    sources: &SourceMap,
    release: &[ReleaseSite],
    remarks: &[CodegenRemark],
) -> Result<(), String> {
    if remarks.is_empty() {
        return Ok(());
    }
    let mut sources = Sources::new(sources, release);
    let mut counts = std::collections::BTreeMap::new();
    for remark in remarks {
        let Some((item, start)) = sources.resolve(&remark.owner, remark.start) else {
            continue;
        };
        let Some(pos) = sources.position_at(item, start, lines) else {
            continue;
        };
        let file = &sources.map.files[item.file as usize].path;
        let (kind, outcome) = match remark.kind {
            RemarkKind::BoundsCheck => ("bounds_check", "elided"),
            RemarkKind::OverflowCheck => ("overflow_check", "elided"),
            RemarkKind::StepSync => ("step_sync", "coalesced"),
        };
        *counts
            .entry((
                &remark.definition,
                &remark.linkage,
                file,
                pos.line,
                pos.column,
                kind,
                outcome,
            ))
            .or_insert(0u64) += 1;
    }
    let records = counts
        .into_iter()
        .map(
            |((definition, linkage, file, line, col, kind, outcome), count)| {
                (
                    definition.to_string(),
                    linkage,
                    file,
                    line,
                    col,
                    kind,
                    outcome,
                    count,
                )
            },
        )
        .collect::<Vec<_>>();
    let data = serde_json::to_vec(&records).map_err(error)?;
    let section = object.add_section(
        vec![],
        hirsel_prof::CODEGEN_REMARK_SECTION.as_bytes().to_vec(),
        object::SectionKind::Debug,
    );
    object.append_section_data(section, &data, 1);
    Ok(())
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
    remarks: &[CodegenRemark],
    target: &str,
) -> Result<Vec<u8>, String> {
    let architecture = match hirsel_core_layout::Target::from_triple(target) {
        Some(hirsel_core_layout::Target::X86_64Linux) => object::Architecture::X86_64,
        Some(hirsel_core_layout::Target::Aarch64Linux) => object::Architecture::Aarch64,
        None => return Err(format!("no native debug target {target}")),
    };
    let mut object = debug_object(architecture);
    let mut symbols = Vec::new();
    let products = products.into_iter().map(|product| (product, None));
    let mut dwarf = unit(products, sources, None, release, |section| {
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
    write_remarks(
        &mut object,
        &mut dwarf.unit.line_program,
        sources,
        release,
        remarks,
    )?;
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

/// The detached debug object of a linked image. A resident compiler
/// remembers where each shared product's locations land in the source map
/// keyed `sources_key`.
pub(crate) fn bind_linked<'a>(
    products: impl IntoIterator<Item = &'a std::sync::Arc<NativeDebug>>,
    sources: &SourceMap,
    sources_key: Option<hirsel_core_ids::ContentHash>,
    release: &[ReleaseSite],
    remarks: &[CodegenRemark],
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
    let products = products
        .into_iter()
        .map(|product| (&**product, Some(product)));
    let mut dwarf = unit(products, sources, sources_key, release, |section| {
        symbols
            .get(section.symbol.as_str())
            .copied()
            .map(Address::Constant)
    });
    let mut object = debug_object(file.architecture());
    write_remarks(
        &mut object,
        &mut dwarf.unit.line_program,
        sources,
        release,
        remarks,
    )?;
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
    products: impl IntoIterator<Item = (&'a NativeDebug, Option<&'a std::sync::Arc<NativeDebug>>)>,
    sources: &SourceMap,
    sources_key: Option<hirsel_core_ids::ContentHash>,
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
    let mut sources = Sources::new(sources, release);
    let mut unit_ranges = Vec::new();
    let mut abstracts = FxHashMap::default();
    // A release binding resolves through the sites the entry plan names,
    // which the map's key does not cover.
    let sources_key = sources_key.filter(|_| release.is_empty());
    for (product, shared) in products {
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
        // The key holds the rows' address; the entry holds the rows, so no
        // other rows take that address while it is remembered.
        let key = sources_key
            .zip(shared)
            .map(|(map, shared)| (map, std::sync::Arc::as_ptr(shared) as usize));
        let remembered = key.and_then(|key| crate::resident::DEBUG_POSITIONS.get(&key));
        let mut replay = remembered.as_ref().map(|(_, locations)| locations.iter());
        let mut located = Vec::new();
        let mut owners = vec![None; product.items.len()];
        let mut positions = FxHashMap::default();
        let mut position = |loc: Option<NativeLocation>, lines: &mut LineProgram| {
            if let Some(&p) = positions.get(&loc) {
                return p;
            }
            let at = match replay.as_mut() {
                Some(replay) => {
                    let &(at_loc, at) = replay.next().expect("a remembered location");
                    debug_assert_eq!(at_loc, loc);
                    at
                }
                None => {
                    let at = sources.locate(&product.items, &mut owners, loc);
                    located.push((loc, at));
                    at
                }
            };
            let p = sources.position(at, lines);
            positions.insert(loc, p);
            p
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
        if let (Some(key), Some(shared), None) = (key, shared, remembered) {
            crate::resident::DEBUG_POSITIONS
                .insert(key, (std::sync::Arc::clone(shared), located.into()));
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
