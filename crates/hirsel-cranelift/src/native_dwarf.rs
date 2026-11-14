//! DWARF5 line/subprogram records. CoreIR supplies item-relative pseudo positions;
//! its final source coordinates are written by the driver at link.
use cranelift_object::object;
use gimli::write::{
    Address, AttributeValue, DwarfUnit, EndianVec, LineProgram, LineString, Range, RangeList,
    RelocateWriter, Relocation, RelocationTarget, Sections,
};
use gimli::{Encoding, Format, LineEncoding, LittleEndian};
use object::write::{Symbol, SymbolSection};
use std::collections::HashMap;

/// A source position the link resolved an interval's item and offset to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    /// The package-qualified file name artifacts carry (`SourceFile::logical_path`).
    pub path: String,
    pub line: u64,
    pub column: u64,
}

pub(crate) struct Function {
    pub symbol: String,
    pub name: String,
    pub size: u64,
    pub declaration: Resolved,
    pub rows: Vec<(u64, u64, Resolved)>,
}

#[derive(Clone)]
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
        self.relocations.push(relocation);
    }
}

fn error(e: impl ToString) -> String {
    e.to_string()
}

pub(crate) fn append(
    product: &mut object::write::Object<'_>,
    functions: &[Function],
    compilation_directory: &str,
) -> Result<(), String> {
    let Some(first) = functions.first().map(|f| &f.declaration) else {
        return Ok(());
    };
    let encoding = Encoding {
        format: Format::Dwarf32,
        version: 5,
        address_size: 8,
    };
    let mut dwarf = DwarfUnit::new(encoding);
    let mut lines = LineProgram::new(
        encoding,
        LineEncoding::default(),
        LineString::String(compilation_directory.as_bytes().to_vec()),
        None,
        LineString::String(first.path.as_bytes().to_vec()),
        None,
    );
    let root = dwarf.unit.root();
    for (name, value) in [
        (gimli::DW_AT_name, first.path.as_str()),
        (gimli::DW_AT_comp_dir, compilation_directory),
        (gimli::DW_AT_producer, "hirsel"),
    ] {
        dwarf
            .unit
            .get_mut(root)
            .set(name, AttributeValue::String(value.as_bytes().to_vec()));
    }
    let mut ranges = Vec::new();
    let mut symbols = Vec::new();
    for (symbol, function) in functions.iter().enumerate() {
        let declaration = &function.declaration;
        symbols.push(
            product
                .symbol_id(function.symbol.as_bytes())
                .unwrap_or_else(|| {
                    product.add_symbol(Symbol {
                        name: function.symbol.as_bytes().to_vec(),
                        value: 0,
                        size: 0,
                        kind: object::SymbolKind::Text,
                        scope: object::SymbolScope::Linkage,
                        weak: false,
                        section: SymbolSection::Undefined,
                        flags: object::SymbolFlags::None,
                    })
                }),
        );
        let address = Address::Symbol { symbol, addend: 0 };
        ranges.push(Range::StartLength {
            begin: address,
            length: function.size,
        });
        let mut file = |path: &str| {
            let directory = lines.default_directory();
            lines.add_file(
                LineString::String(path.as_bytes().to_vec()),
                directory,
                None,
            )
        };
        let declaration_file = file(&declaration.path);
        let rows: Vec<_> = function
            .rows
            .iter()
            .map(|(start, stop, at)| (*start, *stop, file(&at.path), at))
            .collect();
        let mut end = None;
        for (start, stop, file, at) in rows {
            if end != Some(start) {
                if let Some(end) = end {
                    lines.end_sequence(end);
                }
                lines.begin_sequence(Some(address));
            }
            let row = lines.row();
            row.address_offset = start;
            row.file = file;
            row.line = u64::from(at.line);
            row.column = u64::from(at.column);
            lines.generate_row();
            end = Some(stop);
        }
        if let Some(end) = end {
            lines.end_sequence(end);
        }
        let entry = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
        let entry = dwarf.unit.get_mut(entry);
        entry.set(
            gimli::DW_AT_name,
            AttributeValue::String(function.name.as_bytes().to_vec()),
        );
        entry.set(
            gimli::DW_AT_linkage_name,
            AttributeValue::String(function.symbol.as_bytes().to_vec()),
        );
        entry.set(gimli::DW_AT_external, AttributeValue::Flag(true));
        entry.set(
            gimli::DW_AT_decl_file,
            AttributeValue::FileIndex(Some(declaration_file)),
        );
        entry.set(
            gimli::DW_AT_decl_line,
            AttributeValue::Udata(u64::from(declaration.line)),
        );
        entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(address));
        entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(function.size));
    }
    let ranges = dwarf.unit.ranges.add(RangeList(ranges));
    dwarf
        .unit
        .get_mut(root)
        .set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));
    dwarf.unit.line_program = lines;
    let mut sections = Sections::new(Section {
        bytes: EndianVec::new(LittleEndian),
        relocations: Vec::new(),
    });
    dwarf.write(&mut sections).map_err(error)?;
    let mut ids = HashMap::new();
    sections.for_each(|id, section| -> Result<(), String> {
        if !section.bytes.slice().is_empty() {
            let object_id = product.add_section(
                Vec::new(),
                id.name().as_bytes().to_vec(),
                object::SectionKind::Debug,
            );
            product
                .section_mut(object_id)
                .set_data(section.bytes.slice().to_vec(), 1);
            ids.insert(id, object_id);
        }
        Ok(())
    })?;
    sections.for_each(|id, section| -> Result<(), String> {
        for relocation in &section.relocations {
            let symbol = match relocation.target {
                RelocationTarget::Symbol(index) => symbols[index],
                RelocationTarget::Section(id) => product.section_symbol(ids[&id]),
            };
            product
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
    Ok(())
}
