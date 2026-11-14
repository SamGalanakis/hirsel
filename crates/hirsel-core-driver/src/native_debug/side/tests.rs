use super::*;
use hirsel_core_ids::DefPath;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use std::collections::BTreeMap;

fn product() -> NativeDebug {
    let declaration = Some(NativeLocation { item: 0, start: 0 });
    NativeDebug {
        items: vec![DefPath::from_full("app@1/main/f").unwrap()],
        sections: vec![CodeSection {
            symbol: "__test_code_1".into(),
            size: 8,
        }],
        sequences: vec![LineSequence {
            range: CodeRange {
                section: 0,
                begin: 0,
                end: 8,
            },
            rows: vec![
                LineRow {
                    offset: 0,
                    source: Some(NativeLocation { item: 0, start: 9 }),
                },
                LineRow {
                    offset: 2,
                    source: None,
                },
                LineRow {
                    offset: 4,
                    source: Some(NativeLocation { item: 0, start: 12 }),
                },
            ],
        }],
        scopes: vec![
            DebugScope {
                parent: None,
                origin: None,
                kind: ScopeKind::Function {
                    linkage: "hprivate1".into(),
                },
                name: "f".into(),
                declaration,
                call: None,
                ranges: vec![],
            },
            DebugScope {
                parent: None,
                origin: None,
                kind: ScopeKind::Function {
                    linkage: "hprivate2".into(),
                },
                name: "f".into(),
                declaration,
                call: None,
                ranges: vec![],
            },
            DebugScope {
                parent: None,
                origin: Some(0),
                kind: ScopeKind::Function {
                    linkage: "hworker1".into(),
                },
                name: "worker".into(),
                declaration,
                call: None,
                ranges: vec![CodeRange {
                    section: 0,
                    begin: 0,
                    end: 8,
                }],
            },
            DebugScope {
                parent: Some(2),
                origin: Some(1),
                kind: ScopeKind::Inline,
                name: "f".into(),
                declaration,
                call: Some(NativeLocation { item: 0, start: 12 }),
                ranges: vec![CodeRange {
                    section: 0,
                    begin: 4,
                    end: 8,
                }],
            },
        ],
    }
}

pub(crate) fn binding_case() {
    let first = product();
    let mut second = first.clone();
    second.sections[0].symbol = "__test_code_2".into();
    second.scopes[2].kind = ScopeKind::Function {
        linkage: "hworker2".into(),
    };
    let neutral = crate::encode(&(&first, &second)).unwrap();
    let mut sources = SourceMap {
        files: vec![SourceFile {
            path: "/a/source.hirsel".into(),
            text: "// π\n\nfn f(){\n\tλ + 1\r\n}\n".into(),
        }],
        items: BTreeMap::from([(
            first.items[0].clone(),
            ItemSource {
                file: 0,
                file_offset: 7,
            },
        )]),
    };
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for moved in [false, true] {
            if moved {
                sources.files[0].text.insert_str(0, "\n\n");
                sources.items.values_mut().next().unwrap().file_offset += 2;
            }
            let bytes = bind([&first, &second], &sources, target).unwrap();
            let file = object::File::parse(bytes.as_slice()).unwrap();
            assert!(file.section_by_name(".text").is_none());
            let line = if moved { 6 } else { 4 };
            assert_eq!(
                super::super::source_positions(&bytes).unwrap(),
                [
                    ("/a/source.hirsel".into(), line, 2),
                    ("/a/source.hirsel".into(), line, 4),
                    ("/a/source.hirsel".into(), line, 2),
                    ("/a/source.hirsel".into(), line, 4),
                ]
            );
            let sections = gimli::DwarfSections::load(|id| -> Result<Vec<u8>, String> {
                let Some(section) = file.section_by_name(id.name()) else {
                    return Ok(vec![]);
                };
                let mut bytes = section.data().unwrap().to_vec();
                for (offset, relocation) in section.relocations() {
                    let value = match relocation.target() {
                        object::RelocationTarget::Symbol(id) => {
                            file.symbol_by_index(id).unwrap().address()
                        }
                        object::RelocationTarget::Section(_) => 0,
                        _ => panic!("unexpected debug relocation target"),
                    }
                    .wrapping_add_signed(relocation.addend());
                    let at = offset as usize;
                    let n = relocation.size() as usize / 8;
                    bytes[at..at + n].copy_from_slice(&value.to_le_bytes()[..n]);
                }
                Ok(bytes)
            })
            .unwrap();
            let dwarf = sections.borrow(|b| gimli::EndianSlice::new(b, gimli::LittleEndian));
            let mut units = dwarf.units();
            let unit = dwarf.unit(units.next().unwrap().unwrap()).unwrap();
            assert!(
                units.next().unwrap().is_none(),
                "one CU across all body units"
            );
            let mut entries = unit.entries();
            let mut abstracts = 0;
            let mut inlines = 0;
            while let Some(entry) = entries.next_dfs().unwrap() {
                if entry.attr_value(gimli::DW_AT_inline).is_some() {
                    abstracts += 1;
                }
                if entry.tag() == gimli::DW_TAG_inlined_subroutine {
                    inlines += 1;
                    assert_eq!(
                        entry
                            .attr_value(gimli::DW_AT_call_line)
                            .unwrap()
                            .udata_value(),
                        Some(line as u64)
                    );
                    assert_eq!(
                        entry
                            .attr_value(gimli::DW_AT_call_column)
                            .unwrap()
                            .udata_value(),
                        Some(4)
                    );
                    let Some(gimli::AttributeValue::UnitRef(origin)) =
                        entry.attr_value(gimli::DW_AT_abstract_origin)
                    else {
                        panic!("missing abstract origin")
                    };
                    let origin = unit.entry(origin).unwrap();
                    let name = dwarf
                        .attr_string(&unit, origin.attr_value(gimli::DW_AT_linkage_name).unwrap())
                        .unwrap();
                    assert_eq!(
                        name.to_string_lossy(),
                        "hprivate2",
                        "same source/name must not conflate workers"
                    );
                    assert!(entry.attr_value(gimli::DW_AT_ranges).is_some());
                }
            }
            assert_eq!(abstracts, 2, "abstract scopes deduplicate across objects");
            assert_eq!(inlines, 2);
            assert_eq!(crate::encode(&(&first, &second)).unwrap(), neutral);
        }
        sources.files[0].text.replace_range(..2, "");
        sources.items.values_mut().next().unwrap().file_offset -= 2;
    }
}

#[test]
fn indexed_scalar_positions_match_the_source_map() {
    let product = product();
    let text = "// π\n\tλ雪\r\nabc\t🙂\n";
    let map = SourceMap {
        files: vec![SourceFile {
            path: "unicode.hirsel".into(),
            text: text.into(),
        }],
        items: BTreeMap::from([(
            product.items[0].clone(),
            ItemSource {
                file: 0,
                file_offset: 0,
            },
        )]),
    };
    let mut lines = LineProgram::new(
        gimli::Encoding {
            format: gimli::Format::Dwarf32,
            version: 5,
            address_size: 8,
        },
        gimli::LineEncoding::default(),
        LineString::String(vec![]),
        None,
        LineString::String(b"Hirsel".to_vec()),
        None,
    );
    let mut sources = Sources {
        map: &map,
        files: FxHashMap::default(),
    };
    for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
        let position = sources
            .position(
                &[Some(ItemSource {
                    file: 0,
                    file_offset: 0,
                })],
                Some(NativeLocation {
                    item: 0,
                    start: byte as u32,
                }),
                &mut lines,
            )
            .unwrap();
        let (_, line, column) = map.position(&product.items[0], byte as u32).unwrap();
        assert_eq!(
            (position.line, position.column),
            (u64::from(line), u64::from(column))
        );
    }
    assert_eq!(sources.files.len(), 1);
}
