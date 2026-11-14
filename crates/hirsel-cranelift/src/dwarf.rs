//! Debug intervals and the link-time debug object (design §1e). A unit's object carries no
//! DWARF: emission records each function's intervals (`intervals`) from Cranelift's final
//! machine source-location ranges, and each link writes one object of DWARF 5 line tables
//! and subprogram entries over every unit's intervals (`debug_object`), relocated against
//! the units' function symbols. Both backends' intervals feed it.

use cranelift_codegen::CompiledCode;
use cranelift_object::object;
use hirsel_lir::debug::DebugFunction;
use hirsel_lir::ir::LirUnit;
use hirsel_lir::layout_types::TargetSpec;
use hirsel_lir::stage::Ready;
use hirsel_syntax::DefPath;

use crate::CodegenError;

/// The intervals of `unit`'s function `symbol`, compiled as `code`. Its source locations
/// encode complete sites in the unit's canonical `SiteTable` (`SourceLoc` bits).
pub(crate) fn intervals(symbol: &str, unit: &LirUnit<Ready>, code: &CompiledCode) -> DebugFunction {
    let sites = hirsel_lir::debug::SiteTable::new(unit);
    let entry = sites.declaration(unit);
    let mut items = vec![(unit.header.definition.clone(), entry.source)];
    let rows: Vec<_> = code
        .buffer
        .get_srclocs_sorted()
        .iter()
        .filter_map(|range| {
            let site = sites.site(range.loc.bits())?;
            let item = match items.iter().position(|(_, source)| *source == site.source) {
                Some(i) => i,
                None => {
                    items.push((unit.header.definition.clone(), site.source));
                    items.len() - 1
                }
            };
            Some((
                u64::from(range.start),
                u64::from(range.end),
                item as u32,
                site.start,
            ))
        })
        .collect();
    DebugFunction::from_source_rows(
        symbol.to_owned(),
        code.code_buffer().len() as u64,
        items,
        entry.start,
        rows,
    )
}

/// A source position the link resolved an interval's item and offset to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    /// The package-qualified file name artifacts carry (`SourceFile::logical_path`).
    pub path: String,
    pub line: u32,
    pub column: u32,
}

fn error(message: impl ToString) -> CodegenError {
    CodegenError::Object {
        message: message.to_string(),
    }
}

/// The debug object of one link for `target`: a relocatable object holding only DWARF 5
/// `.debug_line`, `.debug_info` and their companions for `functions`, whose symbols it
/// imports. `resolve` places an item-relative offset in the link's current sources; a
/// range it cannot place is unmapped code, and a function whose declaration it cannot
/// place gets no entry. Every gap between mapped ranges ends a line sequence. Without a
/// function, the object is empty.
pub fn debug_object(
    functions: &[&DebugFunction],
    resolve: &dyn Fn(&DefPath, u32) -> Option<Resolved>,
    target: &TargetSpec,
) -> Result<Vec<u8>, CodegenError> {
    debug_object_sources(
        functions,
        &|definition, source, offset| (source == 0).then(|| resolve(definition, offset)).flatten(),
        target,
    )
}

/// Link-time debug binding with the complete source identity of each interval.
pub fn debug_object_sources(
    functions: &[&DebugFunction],
    resolve: &dyn Fn(&DefPath, u32, u32) -> Option<Resolved>,
    target: &TargetSpec,
) -> Result<Vec<u8>, CodegenError> {
    use object::write::Object;
    let architecture = if target.triple.starts_with("x86_64") {
        object::Architecture::X86_64
    } else if target.triple.starts_with("aarch64") {
        object::Architecture::Aarch64
    } else {
        return Err(CodegenError::Target {
            triple: target.triple.clone(),
            message: "no debug object for this architecture".into(),
        });
    };
    let mut product = Object::new(
        object::BinaryFormat::Elf,
        architecture,
        object::Endianness::Little,
    );
    // A library body is in every object that calls it, and the link keeps one copy: its
    // first intervals stand for it.
    let mut seen = std::collections::HashSet::new();
    let declared: Vec<(&DebugFunction, Resolved)> = functions
        .iter()
        .filter(|f| seen.insert(f.symbol.as_str()))
        .filter_map(|f| {
            Some((
                *f,
                resolve(f.items.first()?, *f.sources.first()?, f.declaration)?,
            ))
        })
        .collect();
    let position = |at: Resolved| crate::native_dwarf::Resolved {
        path: at.path,
        line: u64::from(at.line),
        column: u64::from(at.column),
    };
    let resolved: Vec<_> = declared
        .into_iter()
        .map(|(function, declaration)| crate::native_dwarf::Function {
            symbol: function.symbol.clone(),
            name: function.items[0].to_string(),
            size: function.size,
            declaration: position(declaration),
            rows: function
                .ranges
                .iter()
                .filter_map(|range| {
                    let item = function.items.get(range.item as usize)?;
                    Some((
                        range.start,
                        range.stop,
                        position(resolve(
                            item,
                            *function.sources.get(range.item as usize)?,
                            range.offset,
                        )?),
                    ))
                })
                .collect(),
        })
        .collect();
    crate::native_dwarf::append(
        &mut product,
        &resolved,
        hirsel_lir::profile::ProfileSource::COMPILATION_DIRECTORY,
    )
    .map_err(error)?;
    product.write().map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_object::object::read::{Object as _, ObjectSection as _};
    use gimli::LittleEndian;
    use std::collections::HashMap;

    fn item(name: &str) -> DefPath {
        DefPath::core(name)
    }

    /// The line rows of `object`'s `.debug_line`, addresses relative to their sequence's
    /// symbol: (address, line, column, end of sequence).
    fn rows(object: &[u8]) -> Vec<(u64, u64, u64, bool)> {
        let file = cranelift_object::object::File::parse(object).unwrap();
        let data = file
            .section_by_name(".debug_line")
            .expect("a line table")
            .data()
            .unwrap();
        let program = gimli::DebugLine::new(data, gimli::LittleEndian)
            .program(gimli::DebugLineOffset(0), 8, None, None)
            .unwrap();
        let mut rows = program.rows();
        let mut out = Vec::new();
        while let Some((_, row)) = rows.next_row().unwrap() {
            let column = match row.column() {
                gimli::ColumnType::LeftEdge => 0,
                gimli::ColumnType::Column(c) => c.get(),
            };
            out.push((
                row.address(),
                row.line().map_or(0, |l| l.get()),
                column,
                row.end_sequence(),
            ));
        }
        out
    }

    /// One line per ten bytes of item offset, so each range's line says where it came from;
    /// offsets at or past 1000 do not resolve.
    fn resolve(definition: &DefPath, offset: u32) -> Option<Resolved> {
        (offset < 1000).then(|| Resolved {
            path: format!("core@0/{}.hirsel", definition.0.name),
            line: offset / 10 + 1,
            column: offset % 10 + 1,
        })
    }

    #[test]
    fn imported_function_ranges_and_lines_relocate_to_their_own_symbols() {
        use gimli::Reader as _;
        use object::ObjectSymbol as _;
        use std::collections::BTreeMap;

        // gimli's RelocateWriter writes zero plus a relocation for an imported
        // address (src/write/relocate.rs). Read it with the corresponding
        // RelocateReader, assigning distinct final addresses by symbol name.
        #[derive(Clone, Debug)]
        struct Relocations(HashMap<usize, u64>);
        impl gimli::Relocate for Relocations {
            fn relocate_address(&self, offset: usize, value: u64) -> gimli::Result<u64> {
                assert_eq!(value, 0);
                Ok(*self.0.get(&offset).expect("every address has a relocation"))
            }
            fn relocate_offset(&self, offset: usize, value: usize) -> gimli::Result<usize> {
                Ok(self.0.get(&offset).map_or(value, |value| *value as usize))
            }
        }
        for target in [
            TargetSpec::x86_64_unknown_linux_gnu(),
            TargetSpec::aarch64_unknown_linux_gnu(),
        ] {
            let empty = debug_object(&[], &resolve, &target).unwrap();
            let empty = object::File::parse(&*empty).unwrap();
            assert!(
                empty
                    .sections()
                    .all(|s| !s.name().unwrap().starts_with(".debug"))
            );
            // Equal sizes are accepted by LLVM's identical-code-folding check;
            // unequal sizes expose the false overlap of unresolved addresses.
            for sizes in [(4, 4), (4, 8), (0, 0)] {
                let f = DebugFunction::from_rows(
                    "f".into(),
                    sizes.0,
                    vec![item("f")],
                    0,
                    [(0, sizes.0, 0, 10)],
                );
                let g = DebugFunction::from_rows(
                    "g".into(),
                    sizes.1,
                    vec![item("g")],
                    0,
                    [(0, sizes.1, 0, 20)],
                );
                for functions in [[&f, &g], [&g, &f]] {
                    let bytes = debug_object(&functions, &resolve, &target).unwrap();
                    let file = object::File::parse(&*bytes).unwrap();
                    let dwarf = gimli::Dwarf::load(|id| {
                        let section = file.section_by_name(id.name());
                        let data = section.as_ref().map_or(&[][..], |s| s.data().unwrap());
                        let mut values = HashMap::new();
                        if let Some(section) = section {
                            for (offset, relocation) in section.relocations() {
                                assert_eq!(relocation.kind(), object::RelocationKind::Absolute);
                                let object::RelocationTarget::Symbol(index) = relocation.target()
                                else {
                                    panic!("not a symbol relocation")
                                };
                                let symbol = file.symbol_by_index(index).unwrap();
                                let base = if symbol.is_undefined() {
                                    assert_eq!(symbol.address(), 0);
                                    assert_eq!(relocation.size(), 64);
                                    assert_eq!(relocation.addend(), 0);
                                    match symbol.name().unwrap() {
                                        "f" => 0x1000,
                                        "g" => 0x2000,
                                        name => panic!("unexpected import {name}"),
                                    }
                                } else {
                                    assert_eq!(symbol.kind(), object::SymbolKind::Section);
                                    assert_eq!(relocation.size(), 32);
                                    symbol.address()
                                };
                                assert!(
                                    values
                                        .insert(
                                            offset as usize,
                                            base.wrapping_add_signed(relocation.addend())
                                        )
                                        .is_none()
                                );
                            }
                        }
                        Ok::<_, gimli::Error>(gimli::RelocateReader::new(
                            gimli::EndianSlice::new(data, LittleEndian),
                            Relocations(values),
                        ))
                    })
                    .unwrap();
                    let unit = dwarf.unit(dwarf.units().next().unwrap().unwrap()).unwrap();
                    let mut entries = unit.entries();
                    let mut actual = BTreeMap::new();
                    while let Some(entry) = entries.next_dfs().unwrap() {
                        if entry.tag() != gimli::DW_TAG_subprogram {
                            continue;
                        }
                        let name = dwarf
                            .attr_string(
                                &unit,
                                entry.attr_value(gimli::DW_AT_linkage_name).unwrap(),
                            )
                            .unwrap()
                            .to_string_lossy()
                            .unwrap()
                            .into_owned();
                        let mut ranges = dwarf.die_ranges(&unit, entry).unwrap();
                        let range = ranges.next().unwrap().unwrap();
                        assert!(ranges.next().unwrap().is_none());
                        actual.insert(name, (range.begin, range.end));
                    }
                    assert_eq!(
                        actual,
                        BTreeMap::from([
                            ("f".into(), (0x1000, 0x1000 + sizes.0)),
                            ("g".into(), (0x2000, 0x2000 + sizes.1))
                        ])
                    );
                    let mut rows = unit.line_program.clone().unwrap().rows();
                    let mut actual = BTreeMap::new();
                    while let Some((_, row)) = rows.next_row().unwrap() {
                        actual.insert(
                            row.address(),
                            (row.line().unwrap().get(), row.end_sequence()),
                        );
                    }
                    let expected = if sizes.0 == 0 {
                        BTreeMap::new()
                    } else {
                        BTreeMap::from([
                            (0x1000, (2, false)),
                            (0x1000 + sizes.0, (2, true)),
                            (0x2000, (3, false)),
                            (0x2000 + sizes.1, (3, true)),
                        ])
                    };
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    #[test]
    fn a_gap_ends_a_sequence_and_unresolved_code_is_a_gap() {
        let function = DebugFunction::from_rows(
            "hirsel_f".into(),
            24,
            vec![item("f"), item("g")],
            0,
            // Mapped [0, 4) and [4, 8) from one item each, a gap [8, 12), then [12, 16)
            // from a site that does not resolve (a gap too) and [16, 20).
            [
                (0, 4, 0, 10),
                (4, 8, 1, 21),
                (12, 16, 0, 5000),
                (16, 20, 0, 30),
            ],
        );
        assert!(function.well_formed());
        let target = TargetSpec::x86_64_unknown_linux_gnu();
        let object = debug_object(&[&function], &resolve, &target).unwrap();
        assert_eq!(
            rows(&object),
            [
                (0, 2, 1, false),
                (4, 3, 2, false),
                (8, 3, 2, true),
                (16, 4, 1, false),
                (20, 4, 1, true),
            ],
            "the row at 4 ends at the gap; [12, 16) maps to nothing"
        );
        // One debug object per input: the same intervals and sources write the same bytes.
        assert_eq!(
            object,
            debug_object(&[&function], &resolve, &target).unwrap()
        );
        // A function whose declaration does not resolve gets no entry; with none, the
        // object holds no DWARF.
        let unplaced = DebugFunction {
            declaration: 5000,
            ..function.clone()
        };
        let empty = debug_object(&[&unplaced], &resolve, &target).unwrap();
        let file = cranelift_object::object::File::parse(&*empty).unwrap();
        assert!(file.section_by_name(".debug_line").is_none());
    }
    #[test]
    fn generated_sources_with_equal_offsets_and_unmapped_gaps_stay_distinct() {
        let function = DebugFunction::from_source_rows(
            "hirsel_f".into(),
            24,
            vec![(item("f"), 0), (item("f"), 1), (item("f"), u32::MAX)],
            0,
            [
                (0, 4, 0, 10),
                (4, 8, 1, 10),
                (8, 12, 2, 10),
                (16, 20, 1, 30),
            ],
        );
        let target = TargetSpec::x86_64_unknown_linux_gnu();
        let resolve_source = |definition: &DefPath, source: u32, offset: u32| {
            if source == u32::MAX {
                return None;
            }
            let mut at = resolve(definition, offset)?;
            if source == 1 {
                at.line += 100;
                at.path = "<generated:core@0/f>".into();
            }
            Some(at)
        };
        let bytes = debug_object_sources(&[&function], &resolve_source, &target).unwrap();
        assert_eq!(
            rows(&bytes),
            [
                (0, 2, 1, false),
                (4, 102, 1, false),
                (8, 102, 1, true),
                (16, 104, 1, false),
                (20, 104, 1, true)
            ]
        );
        // The declaration-only compatibility API never reinterprets a generated
        // selector under its declaration's anchor.
        let legacy = debug_object(&[&function], &resolve, &target).unwrap();
        assert_eq!(rows(&legacy), [(0, 2, 1, false), (4, 2, 1, true)]);
    }
}
