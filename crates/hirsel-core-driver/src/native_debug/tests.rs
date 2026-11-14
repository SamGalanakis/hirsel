use super::*;
use hirsel_core_ir::source::{ItemSource, SourceFile};

#[test]
fn link_rebinds_rows_and_inline_calls_without_changing_native_code() {
    for version in [4, 5] {
        binding_case(version);
    }
}

fn binding_case(version: u16) {
    let item = DefPath::from_full("app@1/main/f").unwrap();
    let mut raw = object::write::Object::new(
        object::BinaryFormat::Elf,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    let text = raw.add_section(vec![], b".text".to_vec(), object::SectionKind::Text);
    raw.section_mut(text).set_data(vec![0x90, 0xc3], 1);
    let symbol = raw.add_symbol(object::write::Symbol {
        name: b"witness".to_vec(),
        value: 0,
        size: 2,
        kind: object::SymbolKind::Text,
        scope: object::SymbolScope::Linkage,
        weak: false,
        section: object::write::SymbolSection::Section(text),
        flags: object::SymbolFlags::None,
    });
    let encoding = gimli::Encoding {
        format: gimli::Format::Dwarf32,
        version,
        address_size: 8,
    };
    let mut dwarf = write::DwarfUnit::new(encoding);
    let root = dwarf.unit.root();
    dwarf.unit.get_mut(root).set(
        gimli::DW_AT_low_pc,
        AttributeValue::Address(Address::Symbol {
            symbol: 0,
            addend: 0,
        }),
    );
    let ranges = dwarf
        .unit
        .ranges
        .add(write::RangeList(vec![write::Range::OffsetPair {
            begin: 0,
            end: 2,
        }]));
    dwarf
        .unit
        .get_mut(root)
        .set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));
    let mut lines = LineProgram::new(
        encoding,
        gimli::LineEncoding::default(),
        LineString::String(vec![]),
        None,
        LineString::String(item.to_string().into_bytes()),
        None,
    );
    let file = lines.add_file(
        LineString::String(item.to_string().into_bytes()),
        lines.default_directory(),
        None,
    );
    lines.begin_sequence(Some(Address::Symbol {
        symbol: 0,
        addend: 0,
    }));
    lines.row().file = file;
    lines.row().line = 10; // Byte offset9: lambda, after one tab, in the item's second line.
    lines.row().column = 1;
    lines.generate_row();
    lines.end_sequence(2);
    dwarf.unit.line_program = lines;
    let function = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
    dwarf
        .unit
        .get_mut(function)
        .set(gimli::DW_AT_name, AttributeValue::String(b"f".to_vec()));
    dwarf.unit.get_mut(function).set(
        gimli::DW_AT_decl_file,
        AttributeValue::FileIndex(Some(file)),
    );
    dwarf
        .unit
        .get_mut(function)
        .set(gimli::DW_AT_decl_line, AttributeValue::Udata(1));
    dwarf.unit.get_mut(function).set(
        gimli::DW_AT_low_pc,
        AttributeValue::Address(Address::Symbol {
            symbol: 0,
            addend: 0,
        }),
    );
    dwarf
        .unit
        .get_mut(function)
        .set(gimli::DW_AT_high_pc, AttributeValue::Udata(2));
    let inline = dwarf.unit.add(function, gimli::DW_TAG_inlined_subroutine);
    dwarf.unit.get_mut(inline).set(
        gimli::DW_AT_abstract_origin,
        AttributeValue::UnitRef(function),
    );
    dwarf.unit.get_mut(inline).set(
        gimli::DW_AT_call_file,
        AttributeValue::FileIndex(Some(file)),
    );
    dwarf
        .unit
        .get_mut(inline)
        .set(gimli::DW_AT_call_line, AttributeValue::Udata(10));
    let mut sections = write::Sections::new(Section::default());
    dwarf.write(&mut sections).unwrap();
    elf::write_sections(&mut raw, &sections, &[symbol]).unwrap();
    let bytes = raw.write().unwrap();
    let object = super::super::ObjectProduct {
        instance: hirsel_core_ir::InstanceKey {
            definition: item.clone(),
            types: vec![],
            dictionaries: vec![],
        },
        identity: hirsel_core_ids::ContentHash::of_bytes(&bytes),
        bytes,
        compiled: false,
        private_variants: 0,
    };
    let mut sources = SourceMap {
        files: vec![SourceFile {
            path: "/a/source.hirsel".into(),
            text: "// π\n\nfn f(){\n\tλ + 1\n}\n".into(),
        }],
        items: BTreeMap::from([(
            item,
            ItemSource {
                file: 0,
                file_offset: 7,
            },
        )]),
    };
    let (code, debug) = bind(
        std::slice::from_ref(&object),
        &sources,
        super::super::native::TARGET,
    )
    .unwrap();
    assert_eq!(
        object::File::parse(&*code[0])
            .unwrap()
            .section_by_name(".text")
            .unwrap()
            .data()
            .unwrap(),
        &[0x90, 0xc3]
    );
    assert!(
        object::File::parse(&*code[0])
            .unwrap()
            .section_by_name(".debug_line")
            .is_none()
    );
    assert_eq!(
        source_positions(&debug).unwrap(),
        [("/a/source.hirsel".into(), 4, 2)]
    );
    sources.files[0].text.insert_str(0, "\n\n");
    sources.items.values_mut().next().unwrap().file_offset += 2;
    let (moved_code, moved_debug) =
        bind(&[object], &sources, super::super::native::TARGET).unwrap();
    assert_eq!(code, moved_code);
    assert_eq!(
        source_positions(&moved_debug).unwrap(),
        [("/a/source.hirsel".into(), 6, 2)]
    );
    let sections = gimli::DwarfSections::load(|id| -> Result<Vec<u8>, String> {
        let file = object::File::parse(&*moved_debug).map_err(error)?;
        file.section_by_name(id.name())
            .map(|s| s.data().map(Vec::from).map_err(error))
            .unwrap_or_else(|| Ok(vec![]))
    })
    .unwrap();
    let dwarf = sections.borrow(|bytes| R::new(bytes, LittleEndian));
    let unit = dwarf.unit(dwarf.units().next().unwrap().unwrap()).unwrap();
    let mut entries = unit.entries();
    let mut inline_count = 0;
    while let Some(entry) = entries.next_dfs().unwrap() {
        if entry.tag() == gimli::DW_TAG_inlined_subroutine {
            inline_count += 1;
            assert_eq!(
                entry
                    .attr_value(gimli::DW_AT_call_line)
                    .unwrap()
                    .udata_value(),
                Some(6)
            );
            assert_eq!(
                entry
                    .attr_value(gimli::DW_AT_call_column)
                    .unwrap()
                    .udata_value(),
                Some(2)
            );
            assert!(matches!(
                entry.attr_value(gimli::DW_AT_abstract_origin),
                Some(gimli::AttributeValue::UnitRef(_))
            ));
        }
    }
    assert_eq!(inline_count, 1);
}
