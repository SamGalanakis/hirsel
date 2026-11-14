use super::*;
use gimli::write::{
    self, Address, AttributeValue as Value, EndianVec, LineProgram, LineString, RelocateWriter,
    Relocation, RelocationTarget,
};

#[derive(Clone)]
struct Data {
    bytes: EndianVec<LittleEndian>,
    relocations: Vec<Relocation>,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            bytes: EndianVec::new(LittleEndian),
            relocations: vec![],
        }
    }
}
impl RelocateWriter for Data {
    type Writer = EndianVec<LittleEndian>;
    fn writer(&self) -> &Self::Writer {
        &self.bytes
    }
    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.bytes
    }
    fn relocate(&mut self, r: Relocation) {
        self.relocations.push(r);
    }
}

#[test]
fn extraction_preserves_forward_origins_disjoint_ranges_and_generated_rows() {
    for version in [4, 5] {
        for arch in [object::Architecture::X86_64, object::Architecture::Aarch64] {
            let mut object = object::write::Object::new(
                object::BinaryFormat::Elf,
                arch,
                object::Endianness::Little,
            );
            let text = object.add_section(vec![], b".text".to_vec(), object::SectionKind::Text);
            object.section_mut(text).set_data(vec![0; 32], 4);
            // All relocations refer to a symbol four bytes into its section.
            let symbol = object.add_symbol(object::write::Symbol {
                name: b"worker".to_vec(),
                value: 4,
                size: 20,
                kind: object::SymbolKind::Text,
                scope: object::SymbolScope::Compilation,
                weak: false,
                section: object::write::SymbolSection::Section(text),
                flags: object::SymbolFlags::None,
            });
            let encoding = gimli::Encoding {
                format: gimli::Format::Dwarf32,
                version,
                address_size: 8,
            };
            let path = b"app@1/main/same".to_vec();
            let mut dwarf = write::Dwarf::new();
            let mut physical = write::Unit::new(
                encoding,
                LineProgram::new(
                    encoding,
                    gimli::LineEncoding::default(),
                    LineString::String(vec![]),
                    None,
                    LineString::String(path.clone()),
                    None,
                ),
            );
            let file = physical.line_program.add_file(
                LineString::String(path.clone()),
                physical.line_program.default_directory(),
                None,
            );
            physical.line_program.begin_sequence(Some(Address::Symbol {
                symbol: 0,
                addend: 0,
            }));
            for (offset, line) in [(0, 8), (2, 0), (8, 28)] {
                let row = physical.line_program.row();
                row.address_offset = offset;
                row.file = file;
                row.line = line;
                physical.line_program.generate_row();
            }
            physical.line_program.end_sequence(12);
            let root = physical.root();
            let function = physical.add(root, gimli::DW_TAG_subprogram);
            physical
                .get_mut(function)
                .set(gimli::DW_AT_linkage_name, Value::String(b"worker".to_vec()));
            let ranges = physical.ranges.add(write::RangeList(vec![
                write::Range::StartLength {
                    begin: Address::Symbol {
                        symbol: 0,
                        addend: 0,
                    },
                    length: 2,
                },
                write::Range::StartLength {
                    begin: Address::Symbol {
                        symbol: 0,
                        addend: 8,
                    },
                    length: 2,
                },
            ]));
            physical
                .get_mut(function)
                .set(gimli::DW_AT_ranges, Value::RangeListRef(ranges));
            let inline = physical.add(function, gimli::DW_TAG_inlined_subroutine);
            physical
                .get_mut(inline)
                .set(gimli::DW_AT_call_file, Value::FileIndex(Some(file)));
            physical
                .get_mut(inline)
                .set(gimli::DW_AT_call_line, Value::Udata(28));
            let physical_id = dwarf.units.add(physical);
            let mut abstracts = write::Unit::new(encoding, LineProgram::none());
            let root = abstracts.root();
            let first = abstracts.add(root, gimli::DW_TAG_subprogram);
            let second = abstracts.add(root, gimli::DW_TAG_subprogram);
            for id in [first, second] {
                abstracts
                    .get_mut(id)
                    .set(gimli::DW_AT_name, Value::String(b"same".to_vec()));
            }
            let abstract_id = dwarf.units.add(abstracts);
            let unit = dwarf.units.get_mut(physical_id);
            unit.get_mut(function).set(
                gimli::DW_AT_abstract_origin,
                Value::DebugInfoRef(write::DebugInfoRef::Entry(abstract_id, second)),
            );
            unit.get_mut(inline).set(
                gimli::DW_AT_abstract_origin,
                Value::DebugInfoRef(write::DebugInfoRef::Entry(abstract_id, first)),
            );
            let mut sections = write::Sections::new(Data::default());
            dwarf.write(&mut sections).unwrap();
            let mut ids = BTreeMap::new();
            sections
                .for_each(|id, data| -> Result<(), String> {
                    if !data.bytes.slice().is_empty() {
                        let section = object.add_section(
                            vec![],
                            id.name().as_bytes().to_vec(),
                            object::SectionKind::Debug,
                        );
                        object
                            .section_mut(section)
                            .set_data(data.bytes.slice().to_vec(), 1);
                        ids.insert(id, section);
                    }
                    Ok(())
                })
                .unwrap();
            sections
                .for_each(|id, data| -> Result<(), String> {
                    for r in &data.relocations {
                        let target = match r.target {
                            RelocationTarget::Symbol(0) => symbol,
                            RelocationTarget::Section(id) => object.section_symbol(ids[&id]),
                            _ => panic!("unknown fixture symbol"),
                        };
                        object
                            .add_relocation(
                                ids[&id],
                                object::write::Relocation {
                                    offset: r.offset as u64,
                                    symbol: target,
                                    addend: r.addend,
                                    flags: object::RelocationFlags::Generic {
                                        kind: object::RelocationKind::Absolute,
                                        encoding: object::RelocationEncoding::Generic,
                                        size: r.size * 8,
                                    },
                                },
                            )
                            .unwrap();
                    }
                    Ok(())
                })
                .unwrap();
            let bytes = object.write().unwrap();
            let key = ContentHash::of_bytes(b"fixture");
            let product = finish_native_object(bytes.clone(), key, true).unwrap();
            let debug = product.debug.unwrap();
            assert_eq!(debug.scopes.len(), 4);
            assert_eq!(debug.scopes[0].origin, Some(3));
            assert_eq!(debug.scopes[1].origin, Some(2));
            assert_eq!(debug.scopes[1].parent, Some(0));
            assert_eq!(
                debug.scopes[0].ranges,
                [
                    CodeRange {
                        section: 0,
                        begin: 4,
                        end: 6
                    },
                    CodeRange {
                        section: 0,
                        begin: 12,
                        end: 14
                    }
                ]
            );
            let seq = &debug.sequences[0];
            assert_eq!(
                seq.range,
                CodeRange {
                    section: 0,
                    begin: 4,
                    end: 16
                }
            );
            assert_eq!(
                seq.rows
                    .iter()
                    .map(|r| (r.offset, r.source.map(|s| s.start)))
                    .collect::<Vec<_>>(),
                [(4, Some(7)), (6, None), (12, Some(27))]
            );
            assert_eq!(
                debug.items,
                [DefPath::from_full("app@1/main/same").unwrap()]
            );
            assert_eq!(
                debug.scopes[1].call,
                Some(NativeLocation { item: 0, start: 27 })
            );
            let changed = finish_native_object(bytes, ContentHash::of_bytes(b"other"), true)
                .unwrap()
                .debug
                .unwrap();
            assert_ne!(debug.sections[0].symbol, changed.sections[0].symbol);
            assert_eq!(debug.scopes, changed.scopes);
            assert_eq!(debug.sequences, changed.sequences);
            let code = object::File::parse(product.bytes.as_slice()).unwrap();
            assert!(
                code.sections()
                    .all(|s| !s.name().unwrap().starts_with(".debug"))
            );
        }
    }
}
