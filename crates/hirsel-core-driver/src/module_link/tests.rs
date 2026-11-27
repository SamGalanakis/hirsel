use super::*;
use hirsel_core_layout::Target;
use hirsel_mod::{Backend, Module};
use object::write::{Object, Relocation, SectionId, Symbol, SymbolId, SymbolSection as Home};
use object::{
    Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
};

const ENTRY: &str = "haaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn header() -> Header {
    Header {
        target: Target::X86_64Linux,
        backend: Backend::Cranelift,
        policy: 0,
        abi: ContentHash([1; 32]),
        compiler: ContentHash([2; 32]),
        coreir: ContentHash([3; 32]),
        text_align: 0,
        data_align: 0,
    }
}

fn section(
    object: &mut Object<'_>,
    name: &str,
    kind: SectionKind,
    bytes: &[u8],
    align: u64,
) -> SectionId {
    let id = object.add_section(vec![], name.as_bytes().to_vec(), kind);
    object.append_section_data(id, bytes, align);
    id
}

fn symbol(
    object: &mut Object<'_>,
    name: &str,
    home: Option<(SectionId, u64)>,
    global: bool,
) -> SymbolId {
    object.add_symbol(Symbol {
        name: name.as_bytes().to_vec(),
        value: 0,
        size: home.map_or(0, |(_, size)| size),
        kind: SymbolKind::Text,
        scope: if global {
            SymbolScope::Linkage
        } else {
            SymbolScope::Compilation
        },
        weak: false,
        section: home.map_or(Home::Undefined, |(id, _)| Home::Section(id)),
        flags: SymbolFlags::None,
    })
}

fn relocate(
    object: &mut Object<'_>,
    at: SectionId,
    offset: u64,
    symbol: SymbolId,
    r_type: elf::RelocationType,
    addend: i64,
) {
    let flags = RelocationFlags::Elf { r_type };
    object
        .add_relocation(
            at,
            Relocation {
                offset,
                symbol,
                addend,
                flags,
            },
        )
        .unwrap();
}

fn x86() -> Object<'static> {
    Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little)
}

fn root() -> [Root<'static>; 1] {
    [Root {
        identity: None,
        symbol: ENTRY,
        descriptor: None,
        signature: ContentHash([9; 32]),
        waits: false,
    }]
}

/// An entry whose one pointer load reads `import`, or whose one address word is in a
/// section of `kind`.
fn reaching(import: &str, kind: SectionKind) -> Vec<u8> {
    let mut object = x86();
    let code = section(
        &mut object,
        ".text.a",
        SectionKind::Text,
        &[0x48, 0x8b, 0x05, 0, 0, 0, 0, 0xc3],
        16,
    );
    symbol(&mut object, ENTRY, Some((code, 8)), true);
    let target = if kind == SectionKind::Text {
        symbol(&mut object, import, None, true)
    } else {
        let cell = section(&mut object, ".cell", kind, &[0; 8], 8);
        symbol(&mut object, "cell", Some((cell, 8)), false)
    };
    relocate(
        &mut object,
        code,
        3,
        target,
        elf::R_X86_64_REX_GOTPCRELX,
        -4,
    );
    object.write().unwrap()
}

/// Calls, pointer loads and address words resolve to the values the layout fixes.
/// Every expected number is derived by hand from the layout rules of PACK-2: text
/// sections in order at their alignment with trap fill, import slots first in data.
#[test]
fn a_link_resolves_each_reference_kind_to_its_laid_out_target() {
    let mut object = x86();
    #[rustfmt::skip]
    let a = [
        0xe8, 0, 0, 0, 0,             // 0: call b
        0x48, 0x8b, 0x05, 0, 0, 0, 0, // 5: mov rax, [rip + alloc slot]
        0x48, 0x8b, 0x0d, 0, 0, 0, 0, // 12: mov rcx, [rip + &k], taken as lea
        0xc3,                         // 19
    ];
    let code = section(&mut object, ".text.a", SectionKind::Text, &a, 16);
    let other = section(&mut object, ".text.b", SectionKind::Text, &[0xc3], 16);
    let mut words = [0x11; 16];
    words[8..].fill(0);
    let constant = section(
        &mut object,
        ".rodata.k",
        SectionKind::ReadOnlyData,
        &words,
        8,
    );
    // Nothing reaches this section, so its unknown import is not a refusal.
    let dead = section(
        &mut object,
        ".text.dead",
        SectionKind::Text,
        &[0xe8, 0, 0, 0, 0],
        16,
    );
    symbol(&mut object, ENTRY, Some((code, 20)), true);
    let b = symbol(&mut object, "b", Some((other, 1)), false);
    let k = symbol(&mut object, "k", Some((constant, 16)), false);
    let alloc = symbol(&mut object, "hirsel_core_alloc", None, true);
    let unknown = symbol(&mut object, "not_a_row", None, true);
    relocate(&mut object, code, 1, b, elf::R_X86_64_PLT32, -4);
    relocate(&mut object, code, 8, alloc, elf::R_X86_64_REX_GOTPCRELX, -4);
    relocate(&mut object, code, 15, k, elf::R_X86_64_REX_GOTPCRELX, -4);
    relocate(&mut object, constant, 8, b, elf::R_X86_64_64, 0);
    relocate(&mut object, dead, 1, unknown, elf::R_X86_64_PLT32, -4);
    let bytes = object.write().unwrap();

    let image = link(header(), &[&bytes], &root(), vec![]).unwrap().encode();
    let module = Module::parse(&image).unwrap();
    let text = module.text();
    // `.text.a` at 0, `.text.b` at the next multiple of 16 past its 20 bytes.
    assert_eq!(text.len(), 33);
    assert!(text[20..32].iter().all(|byte| *byte == 0xcc));
    // The call's field is at 1 and its instruction ends at 5: 32 - 5.
    assert_eq!(text[1..5], 27i32.to_le_bytes());
    assert_eq!(text[13], 0x8d);
    let alloc = hirsel_mod::row("hirsel_core_alloc").unwrap();
    assert_eq!(module.imports().collect::<Vec<_>>(), [Import::Row(alloc)]);
    // One import slot, then `k` at 8.
    assert_eq!(module.data().len(), 24);
    assert_eq!(module.data()[8..16], [0x11; 8]);
    assert_eq!(
        module.text_fixups().collect::<Vec<_>>(),
        [
            Fixup {
                site: 8,
                kind: Kind::Rel32Data,
                bias: 4,
                addend: 0
            },
            Fixup {
                site: 15,
                kind: Kind::Rel32Data,
                bias: 4,
                addend: 8
            },
        ]
    );
    assert_eq!(
        module.data_fixups().collect::<Vec<_>>(),
        [Fixup {
            site: 16,
            kind: Kind::Abs64Text,
            bias: 0,
            addend: 32
        }]
    );
    assert_eq!(
        module.entries().collect::<Vec<_>>(),
        [Entry {
            identity: ContentHash([0xaa; 32]),
            signature: ContentHash([9; 32]),
            text: 0,
            descriptor: NO_DESCRIPTOR,
            waits: false,
        }]
    );
    assert_eq!(
        module.spans().collect::<Vec<_>>(),
        [
            Span {
                start: 0,
                length: 20
            },
            Span {
                start: 32,
                length: 1
            }
        ]
    );
    assert_eq!((module.name(0), module.name(1)), (ENTRY, "b"));
}

/// The aarch64 forms: a `bl` is encoded in its 26-bit word field, and a GOT pair
/// becomes a page fixup and a load fixup against the slot it reads. Derived by hand:
/// `.text.a` is 24 bytes, `.text.b` follows at 32, one import slot then one pointer
/// slot then `k`.
#[test]
fn a_link_resolves_the_aarch64_reference_forms() {
    let mut object = Object::new(BinaryFormat::Elf, Architecture::Aarch64, Endianness::Little);
    let words: [u32; 6] = [
        0x9400_0000, // 0: bl b
        0x9000_0000, // 4: adrp x0, slot of alloc
        0xf940_0000, // 8: ldr x0, [x0, low 12 of the slot]
        0x9000_0001, // 12: adrp x1, slot of &k
        0xf940_0021, // 16: ldr x1, [x1, low 12 of the slot]
        0xd65f_03c0, // 20: ret
    ];
    let a: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let code = section(&mut object, ".text.a", SectionKind::Text, &a, 4);
    let other = section(
        &mut object,
        ".text.b",
        SectionKind::Text,
        &0xd65f_03c0u32.to_le_bytes(),
        16,
    );
    let constant = section(
        &mut object,
        ".rodata.k",
        SectionKind::ReadOnlyData,
        &[0x11; 8],
        8,
    );
    symbol(&mut object, ENTRY, Some((code, 24)), true);
    let b = symbol(&mut object, "b", Some((other, 4)), false);
    let k = symbol(&mut object, "k", Some((constant, 8)), false);
    let alloc = symbol(&mut object, "hirsel_core_alloc", None, true);
    relocate(&mut object, code, 0, b, elf::R_AARCH64_CALL26, 0);
    relocate(&mut object, code, 4, alloc, elf::R_AARCH64_ADR_GOT_PAGE, 0);
    relocate(
        &mut object,
        code,
        8,
        alloc,
        elf::R_AARCH64_LD64_GOT_LO12_NC,
        0,
    );
    relocate(&mut object, code, 12, k, elf::R_AARCH64_ADR_GOT_PAGE, 0);
    relocate(&mut object, code, 16, k, elf::R_AARCH64_LD64_GOT_LO12_NC, 0);
    let bytes = object.write().unwrap();

    let header = Header {
        target: Target::Aarch64Linux,
        ..header()
    };
    let image = link(header, &[&bytes], &root(), vec![]).unwrap().encode();
    let module = Module::parse(&image).unwrap();
    let text = module.text();
    assert_eq!(text.len(), 36);
    // 32 bytes ahead is 8 instructions.
    assert_eq!(text[0..4], 0x9400_0008u32.to_le_bytes());
    // The pointer loads stay loads, and the gap is two `brk #0`.
    assert_eq!(text[4..24], a[4..24]);
    assert_eq!(text[24..28], 0xd420_0000u32.to_le_bytes());
    assert_eq!(text[28..32], 0xd420_0000u32.to_le_bytes());
    assert_eq!(
        module.text_fixups().collect::<Vec<_>>(),
        [
            Fixup {
                site: 4,
                kind: Kind::PageData,
                bias: 0,
                addend: 0
            },
            Fixup {
                site: 8,
                kind: Kind::Low12LoadData,
                bias: 0,
                addend: 0
            },
            Fixup {
                site: 12,
                kind: Kind::PageData,
                bias: 0,
                addend: 8
            },
            Fixup {
                site: 16,
                kind: Kind::Low12LoadData,
                bias: 0,
                addend: 8
            },
        ]
    );
    assert_eq!(module.data().len(), 24);
    assert_eq!(
        module.data_fixups().collect::<Vec<_>>(),
        [Fixup {
            site: 8,
            kind: Kind::Abs64Data,
            bias: 0,
            addend: 16
        }]
    );
}

/// A release that reaches outside the closed row table, into a foreign row, or into
/// writable static data has no module (PACK-2, PACK-7).
#[test]
fn a_link_refuses_what_a_module_cannot_hold() {
    let stop = |objects: &[&[u8]]| link(header(), objects, &root(), vec![]).unwrap_err();
    let fine = reaching("hirsel_core_alloc", SectionKind::Text);
    link(header(), &[&fine], &root(), vec![]).unwrap();
    assert_eq!(
        stop(&[&reaching("not_a_row", SectionKind::Text)]),
        "import not_a_row is not in the row table"
    );
    assert_eq!(
        stop(&[&reaching("hirsel_core_foreign_cleanup", SectionKind::Text)]),
        "the release reaches hirsel_core_foreign_cleanup"
    );
    assert_eq!(
        stop(&[&reaching("hirsel_core_alloc", SectionKind::Data)]),
        "object 0 has writable data section .cell"
    );
    assert_eq!(
        stop(&[&reaching("hirsel_core_alloc", SectionKind::Tls)]),
        "thread-local section .cell"
    );
    assert_eq!(stop(&[&fine, &fine]), format!("{ENTRY} is defined twice"));
    let mut arm = Object::new(BinaryFormat::Elf, Architecture::Aarch64, Endianness::Little);
    let code = section(
        &mut arm,
        ".text.a",
        SectionKind::Text,
        &[0xc0, 0x03, 0x5f, 0xd6],
        4,
    );
    symbol(&mut arm, ENTRY, Some((code, 4)), true);
    assert_eq!(
        stop(&[&arm.write().unwrap()]),
        "a Elf Aarch64 object for x86_64-unknown-linux-gnu"
    );
}
