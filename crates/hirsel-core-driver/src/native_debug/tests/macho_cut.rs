//! The Mach-O arm64 per-part cut, linked by the driver's Mach-O link: each
//! part keeps its ARM64_RELOC_ADDEND / PAGE21 / PAGEOFF12 pairs, its
//! `__compact_unwind` entry and its `__eh_frame` FDE, and is a function of
//! its own definition only.
use super::{compact_unwind, link_macho};
use hirsel_core_ids::ContentHash;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use std::path::{Path, PathBuf};

/// `_first` loads `_table[2]` through a PAGE21/PAGEOFF12 pair with addend
/// 16 (frameless); `_second` keeps a frame, calls `_first`, addresses a
/// local C string and calls `_odd`, so the link's dead strip keeps it;
/// `_odd`'s CFA is x1+32, which no compact encoding holds, so it unwinds
/// through DWARF. `extra` goes after `_second`'s calls. Every definition is
/// 16-aligned, as a split module generates it (`parts.cpp`).
fn source(extra: &str, odd: bool) -> String {
    let odd = if odd {
        ".globl _odd\n.p2align 4\n_odd:\n.cfi_startproc\n.cfi_def_cfa x1, 32\nret\n.cfi_endproc\n"
    } else {
        ""
    };
    format!(
        r#"
.section __TEXT,__text,regular,pure_instructions
.build_version macos, 26, 0
.globl _first
.p2align 4
_first:
.cfi_startproc
adrp x0, _table@PAGE+16
add x0, x0, _table@PAGEOFF+16
ldr x0, [x0]
ret
.cfi_endproc
.globl _second
.p2align 4
_second:
.cfi_startproc
stp x29, x30, [sp, #-16]!
mov x29, sp
.cfi_def_cfa w29, 16
.cfi_offset w30, -8
.cfi_offset w29, -16
bl _first
adrp x1, l_.str@PAGE
add x1, x1, l_.str@PAGEOFF
bl _odd
{extra}
ldp x29, x30, [sp], #16
ret
.cfi_endproc
{odd}
.section __TEXT,__cstring,cstring_literals
l_.str:
.asciz "hi"
.section __DATA,__const
.globl _table
.p2align 4
_table:
.quad 1, 2, 3, 4
.subsections_via_symbols
"#
    )
}

fn bin() -> PathBuf {
    PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin")
}

fn assemble(dir: &Path, name: &str, source: &str) -> Vec<u8> {
    let (assembly, object) = (dir.join(format!("{name}.s")), dir.join(format!("{name}.o")));
    std::fs::write(&assembly, source).unwrap();
    let status = std::process::Command::new(bin().join("clang"))
        .args(["--target=arm64-apple-macos26.0", "-c"])
        .arg(&assembly)
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(object).unwrap()
}

fn cut(object: &[u8], names: &[&'static str]) -> Vec<Vec<u8>> {
    let parts: Vec<_> = names
        .iter()
        .map(|&name| (name, ContentHash::of_bytes(name.as_bytes())))
        .collect();
    hirsel_llvm_lazy::core::split_native_object(object, &parts, false, None)
        .unwrap()
        .into_iter()
        .map(|part| part.bytes)
        .collect()
}

fn word(image: &object::File<'_>, address: u64) -> u32 {
    let bytes = read(image, address, 4);
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn read<'a>(image: &'a object::File<'_>, address: u64, len: u64) -> &'a [u8] {
    image
        .sections()
        .find_map(|s| {
            let offset = address.checked_sub(s.address())?;
            (offset + len <= s.size())
                .then(|| &s.data().unwrap()[offset as usize..][..len as usize])
        })
        .unwrap()
}

/// The address an `adrp; add` pair at `at` forms, decoded as the Arm ARM
/// encodes ADRP (immhi:immlo pages from the instruction's page) and ADD
/// (immediate, imm12 unshifted).
fn adrp_add(image: &object::File<'_>, at: u64) -> u64 {
    let (adrp, add) = (word(image, at), word(image, at + 4));
    assert_eq!(adrp & 0x9f00_0000, 0x9000_0000, "not ADRP at {at:#x}");
    assert_eq!(add & 0xffc0_0000, 0x9100_0000, "not ADD #imm at {at:#x}");
    let pages = ((adrp >> 5 & 0x7_ffff) << 2 | adrp >> 29 & 3) as i64;
    let pages = pages << 43 >> 43;
    let page = (at & !0xfff).wrapping_add_signed(pages << 12);
    page + u64::from(add >> 10 & 0xfff)
}

/// The image of the three parts cut from the probe, linked by the driver.
#[test]
fn macho_parts_keep_their_page_pairs_and_unwind() {
    use gimli::UnwindSection as _;
    use gimli::{BaseAddresses, CfaRule, EhFrame, LittleEndian, Register, UnwindContext};

    let dir = tempfile::tempdir().unwrap();
    let object = assemble(dir.path(), "whole", &source("", true));
    let parts = cut(&object, &["first", "second", "odd"]);
    let image = dir.path().join("image");
    let objects = parts.into_iter().map(Into::into).collect();
    link_macho(&bin().join("ld.lld"), objects, "_second", &image);
    let bytes = std::fs::read(&image).unwrap();
    let macho = object::File::parse(bytes.as_slice()).unwrap();
    let address = |name: &str| macho.symbol_by_name(name).unwrap().address();
    let (first, second, odd) = (address("_first"), address("_second"), address("_odd"));
    // `_first` addresses `_table` + 16, whose quad is the third: 3.
    assert_eq!(adrp_add(&macho, first), address("_table") + 16);
    assert_eq!(read(&macho, address("_table") + 16, 8), 3u64.to_le_bytes());
    // `_second`: BL (imm26 words from itself) to `_first`, then "hi".
    let bl = word(&macho, second + 8);
    assert_eq!(bl >> 26, 0b100101, "not BL");
    let words = i64::from((bl & 0x3ff_ffff) as i32) << 38 >> 38;
    assert_eq!((second + 8).wrapping_add_signed(words * 4), first);
    assert_eq!(read(&macho, adrp_add(&macho, second + 12), 3), b"hi\0");
    // Compact unwind (compact_unwind_encoding.h): UNWIND_ARM64_MODE_FRAMELESS,
    // _FRAME, and _DWARF for `_odd`, each at its own function.
    let (entries, dump) = compact_unwind(&bin(), &image);
    let text = macho
        .segments()
        .find(|segment| object::ObjectSegment::name(segment) == Ok(Some("__TEXT")))
        .map(|segment| object::ObjectSegment::address(&segment))
        .unwrap();
    for (address, mode) in [
        (first, 0x0200_0000),
        (second, 0x0400_0000),
        (odd, 0x0300_0000),
    ] {
        let (start, encoding) = entries.range(..=address - text).next_back().unwrap();
        assert_eq!(
            (*start, encoding & 0x0f00_0000),
            (address - text, mode),
            "{address:#x}:\n{dump}"
        );
    }
    // `_odd`'s FDE: CFA = x1 + 32.
    let section = macho.section_by_name("__eh_frame").unwrap();
    let bases = BaseAddresses::default().set_eh_frame(section.address());
    let frame = EhFrame::new(section.data().unwrap(), LittleEndian);
    let mut context = UnwindContext::new();
    let row = frame
        .unwind_info_for_address(&bases, &mut context, odd, EhFrame::cie_from_offset)
        .unwrap();
    assert_eq!(
        *row.cfa(),
        CfaRule::RegisterAndOffset {
            register: Register(1),
            offset: 32
        }
    );
}

/// A part is a function of its definition alone: `_first` cut from an
/// object whose neighbours differ (a longer `_second`, no `_odd`) is the
/// same bytes.
#[test]
fn macho_part_does_not_depend_on_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let one = assemble(dir.path(), "one", &source("", true));
    let two = assemble(dir.path(), "two", &source("nop\nnop\nnop", false));
    let one = cut(&one, &["first", "second", "odd"]);
    let two = cut(&two, &["first", "second"]);
    assert_eq!(one[0], two[0]);
}

/// LLVM's own Darwin release objects cut per part with their debug rows, as
/// `release_native` cuts them: a switch wide enough for a jump table, which
/// Mach-O addresses from its section's start with no length (so a split
/// module generates none); a 13-byte constant followed, beside, by a
/// neighbour's 16-aligned one, since Mach-O records no atom's size or
/// alignment (so a split module aligns each definition to 16); a shared constant (a weak
/// definition each part that reaches it carries),
/// a private constant whose pointers are UNSIGNED relocations, a C string;
/// the parts link, and a part cut from a module with other neighbours is
/// the same bytes.
#[test]
fn macho_llvm_release_parts_cut_link_and_stand_alone() {
    use hirsel_core_layout::Target;
    use hirsel_llvm_lazy::{Lend, Unheld, Unit, emit_program_parts};

    let ir = |neighbour: &str| {
        format!(
            r#"
            @hirsel_zero = linkonce_odr constant [61 x i8] zeroinitializer, align 8
            @hirsel_ptrs = private constant [2 x ptr] [ptr @hirsel_zero, ptr @hirsel_live], align 8
            @hirsel_msg = private unnamed_addr constant [3 x i8] c"hi\00"
            @hirsel_odd = private unnamed_addr constant [13 x i8] c"abcdefghijkl\01"
            define i64 @hirsel_live(ptr %p) noinline {{
                %v = load volatile i64, ptr %p
                ret i64 %v
            }}
            define i64 @hirsel_pick(i64 %k) noinline {{
                switch i64 %k, label %d [ i64 0, label %a
                                          i64 1, label %b
                                          i64 2, label %c
                                          i64 3, label %e3
                                          i64 4, label %e4
                                          i64 5, label %e5
                                          i64 6, label %e6
                                          i64 7, label %e7
                                          i64 8, label %e8
                                          i64 9, label %e9
                                          i64 10, label %e10
                                          i64 11, label %e11
                                          i64 12, label %e12
                                          i64 13, label %e13 ]
            a:
                %x = call i64 @hirsel_live(ptr @hirsel_zero)
                ret i64 %x
            b:
                ret i64 7
            c:
                ret i64 ptrtoint (ptr @hirsel_msg to i64)
            e3:
                %r3 = call i64 @hirsel_live(ptr @hirsel_msg)
                ret i64 %r3
            e4:
                %r4 = call i64 @hirsel_live(ptr @hirsel_ptrs)
                ret i64 %r4
            e5:
                ret i64 5000011
            e6:
                %r6 = call i64 @hirsel_live(ptr @hirsel_zero)
                %s6 = mul i64 %r6, 6
                ret i64 %s6
            e7:
                ret i64 7000003
            e8:
                %r8 = call i64 @hirsel_live(ptr @hirsel_msg)
                %s8 = add i64 %r8, 8
                ret i64 %s8
            e9:
                %b9 = load volatile i8, ptr getelementptr (i8, ptr @hirsel_odd, i64 12)
                %r9 = zext i8 %b9 to i64
                ret i64 %r9
            e10:
                %r10 = call i64 @hirsel_live(ptr @hirsel_odd)
                ret i64 %r10
            e11:
                ret i64 11000027
            e12:
                %r12 = call i64 @hirsel_live(ptr @hirsel_zero)
                %s12 = xor i64 %r12, 12
                ret i64 %s12
            e13:
                ret i64 13000049
            d:
                %i = and i64 %k, 1
                %slot = getelementptr [2 x ptr], ptr @hirsel_ptrs, i64 0, i64 %i
                %f = load ptr, ptr %slot
                %y = call i64 %f(ptr @hirsel_msg)
                ret i64 %y
            }}
            define i64 @hirsel_peek() noinline {{
                %b = load volatile i8, ptr getelementptr (i8, ptr @hirsel_odd, i64 12)
                %r = zext i8 %b to i64
                ret i64 %r
            }}
            define i64 @hirsel_sum(ptr %p, i64 %n) hot noinline {{
            top:
                br label %loop
            loop:
                %i = phi i64 [ 0, %top ], [ %j, %loop ]
                %t = phi i64 [ 0, %top ], [ %u, %loop ]
                %q = getelementptr i64, ptr %p, i64 %i
                %v = load volatile i64, ptr %q
                %u = add i64 %t, %v
                %j = add i64 %i, 1
                %done = icmp eq i64 %j, %n
                br i1 %done, label %out, label %loop
            out:
                ret i64 %u
            }}
            {neighbour}
            define i64 @hirsel_entry(i64 %k) {{
                %v = call i64 @hirsel_pick(i64 %k)
                ret i64 %v
            }}"#
        )
    };
    let neighbour =
        "@hirsel_wide = private unnamed_addr constant [2 x i128] [i128 5, i128 9], align 16
            define i64 @hirsel_other(ptr %p) noinline {
                %q = getelementptr i8, ptr @hirsel_wide, i64 16
                %c = load volatile i64, ptr %q
                store volatile i64 %c, ptr %p
                %v = load volatile i64, ptr %p
                %w = mul i64 %v, 3
                store volatile i64 %w, ptr %p
                ret i64 %w
            }";
    let mut cut = |ir: &str, names: &[&str]| {
        let exports: Vec<String> = names.iter().map(|n| format!("hirsel_{n}")).collect();
        let program = emit_program_parts(
            &[Unit::Text(ir)],
            &exports,
            Target::Aarch64Darwin,
            0,
            &mut Unheld,
            Lend::None,
        )
        .unwrap();
        let parts: Vec<_> = program
            .parts
            .iter()
            .map(|p| (p.name.as_str(), ContentHash::of_bytes(p.name.as_bytes())))
            .collect();
        assert_eq!(parts.len(), names.len(), "the module stayed whole");
        let [whole] = program.objects.as_slice() else {
            panic!("no jobs generates one object");
        };
        let objects = hirsel_llvm_lazy::core::split_native_object(whole, &parts, true, None);
        let objects = objects.unwrap();
        for (object, (name, _)) in objects.iter().zip(&parts) {
            let debug = object
                .debug
                .as_ref()
                .expect("a release part keeps its rows");
            assert_eq!(debug.sections.len(), 1, "{name}");
            assert_eq!(debug.sections[0].symbol, *name);
        }
        let named: std::collections::BTreeMap<String, Vec<u8>> = parts
            .iter()
            .zip(objects)
            .map(|((name, _), object)| (name.to_string(), object.bytes))
            .collect();
        named
    };
    // Each part generated alone on its own thread's machine, as the release
    // std's are (`generate_parts`), and cut alone.
    let threaded = |ir: &str, names: &[&str]| {
        let exports: Vec<String> = names.iter().map(|n| format!("hirsel_{n}")).collect();
        let program = emit_program_parts(
            &[Unit::Text(ir)],
            &exports,
            Target::Aarch64Darwin,
            2,
            &mut Unheld,
            Lend::None,
        )
        .unwrap();
        let named: std::collections::BTreeMap<String, Vec<u8>> = program
            .parts
            .iter()
            .zip(&program.objects)
            .map(|(part, object)| {
                let key = ContentHash::of_bytes(part.name.as_bytes());
                let cut = hirsel_llvm_lazy::core::split_native_object(
                    object,
                    &[(&part.name, key)],
                    true,
                    None,
                );
                let [cut] = <[_; 1]>::try_from(cut.unwrap()).ok().unwrap();
                (part.name.clone(), cut.bytes)
            })
            .collect();
        named
    };
    let names = ["live", "pick", "peek", "sum", "entry"];
    let with = ["live", "pick", "peek", "sum", "other", "entry"];
    let alone = cut(&ir(""), &names);
    let beside = cut(&ir(neighbour), &with);
    let lent = threaded(&ir(""), &names);
    let lent_beside = threaded(&ir(neighbour), &with);
    // `peek`'s one constant, 13 bytes, ends its section alone and runs into
    // the neighbour's padding beside; `sum`, hot and looping, is aligned past
    // 16 by the release pipeline unless the split lowers it.
    for name in [
        "hirsel_live",
        "hirsel_pick",
        "hirsel_peek",
        "hirsel_sum",
        "hirsel_entry",
    ] {
        assert_eq!(
            alone[name], beside[name],
            "{name} depends on its neighbours"
        );
        assert_eq!(
            lent[name], lent_beside[name],
            "threaded {name} depends on its neighbours"
        );
    }
    // `pick`'s copy of the private table points at the zero-filled global,
    // then at `live`, through the relocations the IR's initialiser implies.
    let pick = object::File::parse(beside["hirsel_pick"].as_slice()).unwrap();
    let table = pick
        .sections()
        .find(|s| s.name() == Ok("__const") && s.segment_name() == Ok(Some("__DATA")))
        .unwrap();
    let mut targets: Vec<_> = table
        .relocations()
        .map(|(offset, r)| match r.target() {
            object::RelocationTarget::Symbol(i) => (
                offset,
                pick.symbol_by_index(i).unwrap().name().unwrap().to_owned(),
            ),
            other => panic!("{other:?}"),
        })
        .collect();
    targets.sort();
    assert_eq!(
        targets,
        [
            (0, "_hirsel_zero".to_owned()),
            (8, "_hirsel_live".to_owned())
        ]
    );
    // The shared constant travels whole with the part that reaches it, as a
    // weak definition the link keeps one copy of: 61 zero bytes.
    let zero = pick.symbol_by_name("_hirsel_zero").unwrap();
    assert!(zero.is_definition() && zero.is_weak());
    let section = pick
        .section_by_index(zero.section_index().unwrap())
        .unwrap();
    let at = (zero.address() - section.address()) as usize;
    assert_eq!(section.data().unwrap()[at..at + 61], [0; 61]);
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    let objects = beside.into_values().map(Into::into).collect();
    link_macho(&bin().join("ld.lld"), objects, "_hirsel_entry", &image);
}
