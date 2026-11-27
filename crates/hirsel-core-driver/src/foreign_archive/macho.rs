//! FFI-3's Mach-O rows and the definitions of a Mach-O link input.
use super::{Member, Symbol, c_name};
use crate::code_admission::AdmissionError;
use hirsel_core_ir::decode::{Debit, DecodeResult};
use hirsel_core_layout::AppleRecord;

// Mach-O, as LLVM's `BinaryFormat/MachO.h` names it.
pub(super) const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_OBJECT: u32 = 0x1;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const CPU_SUBTYPE_ARM64_ALL: u32 = 0;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_BUILD_VERSION: u32 = 0x32;
const SECTION_TYPE: u32 = 0xff;
const S_REGULAR: u32 = 0x0;
const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x400;
const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_EXT: u8 = 0x01;
const N_UNDF: u8 = 0x0;
const N_ABS: u8 = 0x2;
const N_INDR: u8 = 0xa;
const N_PBUD: u8 = 0xc;
const N_SECT: u8 = 0xe;
const N_SYMBOL_RESOLVER: u16 = 0x100;

/// The load commands FFI-3 admits, and the names of those it refuses.
const LOAD_COMMANDS: &[(u32, &str, bool)] = &[
    (0x1, "LC_SEGMENT", false),
    (LC_SYMTAB, "LC_SYMTAB", true),
    (0x3, "LC_SYMSEG", false),
    (0x4, "LC_THREAD", false),
    (0x5, "LC_UNIXTHREAD", false),
    (0x6, "LC_LOADFVMLIB", false),
    (0x7, "LC_IDFVMLIB", false),
    (0x8, "LC_IDENT", false),
    (0x9, "LC_FVMFILE", false),
    (0xa, "LC_PREPAGE", false),
    (0xb, "LC_DYSYMTAB", true),
    (0xc, "LC_LOAD_DYLIB", false),
    (0xd, "LC_ID_DYLIB", false),
    (0xe, "LC_LOAD_DYLINKER", false),
    (0xf, "LC_ID_DYLINKER", false),
    (0x10, "LC_PREBOUND_DYLIB", false),
    (0x11, "LC_ROUTINES", false),
    (0x12, "LC_SUB_FRAMEWORK", false),
    (0x13, "LC_SUB_UMBRELLA", false),
    (0x14, "LC_SUB_CLIENT", false),
    (0x15, "LC_SUB_LIBRARY", false),
    (0x16, "LC_TWOLEVEL_HINTS", false),
    (0x17, "LC_PREBIND_CKSUM", false),
    (0x8000_0018, "LC_LOAD_WEAK_DYLIB", false),
    (LC_SEGMENT_64, "LC_SEGMENT_64", true),
    (0x1a, "LC_ROUTINES_64", false),
    (0x1b, "LC_UUID", false),
    (0x8000_001c, "LC_RPATH", false),
    (0x1d, "LC_CODE_SIGNATURE", false),
    (0x1e, "LC_SEGMENT_SPLIT_INFO", false),
    (0x8000_001f, "LC_REEXPORT_DYLIB", false),
    (0x20, "LC_LAZY_LOAD_DYLIB", false),
    (0x21, "LC_ENCRYPTION_INFO", false),
    (0x22, "LC_DYLD_INFO", false),
    (0x8000_0022, "LC_DYLD_INFO_ONLY", false),
    (0x8000_0023, "LC_LOAD_UPWARD_DYLIB", false),
    (0x24, "LC_VERSION_MIN_MACOSX", false),
    (0x25, "LC_VERSION_MIN_IPHONEOS", false),
    (0x26, "LC_FUNCTION_STARTS", false),
    (0x27, "LC_DYLD_ENVIRONMENT", false),
    (0x8000_0028, "LC_MAIN", false),
    (0x29, "LC_DATA_IN_CODE", true),
    (0x2a, "LC_SOURCE_VERSION", false),
    (0x2b, "LC_DYLIB_CODE_SIGN_DRS", false),
    (0x2c, "LC_ENCRYPTION_INFO_64", false),
    (0x2d, "LC_LINKER_OPTION", false),
    (0x2e, "LC_LINKER_OPTIMIZATION_HINT", true),
    (0x2f, "LC_VERSION_MIN_TVOS", false),
    (0x30, "LC_VERSION_MIN_WATCHOS", false),
    (0x31, "LC_NOTE", false),
    (LC_BUILD_VERSION, "LC_BUILD_VERSION", true),
    (0x8000_0033, "LC_DYLD_EXPORTS_TRIE", false),
    (0x8000_0034, "LC_DYLD_CHAINED_FIXUPS", false),
    (0x35, "LC_FILESET_ENTRY", false),
    (0x36, "LC_ATOM_INFO", false),
];

/// The section types (the low byte of a section's flags), and whether FFI-3 admits each.
const SECTION_TYPES: &[(&str, bool)] = &[
    ("S_REGULAR", true),
    ("S_ZEROFILL", true),
    ("S_CSTRING_LITERALS", true),
    ("S_4BYTE_LITERALS", true),
    ("S_8BYTE_LITERALS", true),
    ("S_LITERAL_POINTERS", true),
    ("S_NON_LAZY_SYMBOL_POINTERS", false),
    ("S_LAZY_SYMBOL_POINTERS", false),
    ("S_SYMBOL_STUBS", false),
    ("S_MOD_INIT_FUNC_POINTERS", false),
    ("S_MOD_TERM_FUNC_POINTERS", false),
    ("S_COALESCED", true),
    ("S_GB_ZEROFILL", false),
    ("S_INTERPOSING", false),
    ("S_16BYTE_LITERALS", true),
    ("S_DTRACE_DOF", false),
    ("S_LAZY_DYLIB_SYMBOL_POINTERS", false),
    ("S_THREAD_LOCAL_REGULAR", true),
    ("S_THREAD_LOCAL_ZEROFILL", true),
    ("S_THREAD_LOCAL_VARIABLES", true),
    ("S_THREAD_LOCAL_VARIABLE_POINTERS", true),
    ("S_THREAD_LOCAL_INIT_FUNCTION_POINTERS", false),
    ("S_INIT_FUNC_OFFSETS", false),
];

fn le<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    le(bytes, at).map(u32::from_le_bytes)
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    le(bytes, at).map(u64::from_le_bytes)
}

fn fixed_name(bytes: &[u8]) -> &[u8] {
    bytes.split(|byte| *byte == 0).next().unwrap_or(bytes)
}

/// A 64-bit little-endian Mach-O file: its header and load commands.
struct MachO<'a> {
    bytes: &'a [u8],
    commands: &'a [u8],
    count: u32,
}

struct MachSection<'a> {
    segment: &'a [u8],
    name: &'a [u8],
    flags: u32,
    offset: u32,
    size: u64,
}

struct MachSymbol<'a> {
    name: &'a [u8],
    kind: u8,
    section: u8,
    desc: u16,
    value: u64,
}

impl MachSymbol<'_> {
    fn debug(&self) -> bool {
        self.kind & N_STAB != 0
    }

    /// An external definition: in a section, absolute, indirect, or common
    /// (undefined with a size).
    fn defines(&self) -> bool {
        !self.debug()
            && self.kind & N_EXT != 0
            && match self.kind & N_TYPE {
                N_SECT | N_ABS | N_INDR => true,
                N_UNDF => self.value != 0,
                _ => false,
            }
    }

    fn imports(&self) -> bool {
        !self.debug()
            && match self.kind & N_TYPE {
                N_UNDF => self.kind & N_EXT != 0 && self.value == 0,
                N_PBUD => true,
                _ => false,
            }
    }
}

impl<'a> MachO<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        if u32_at(bytes, 0)? != MH_MAGIC_64 {
            return None;
        }
        let size = usize::try_from(u32_at(bytes, 20)?).ok()?;
        Some(Self {
            bytes,
            commands: bytes.get(32..32usize.checked_add(size)?)?,
            count: u32_at(bytes, 16)?,
        })
    }

    fn header(&self, at: usize) -> u32 {
        u32_at(self.bytes, at).unwrap_or(0)
    }

    /// Each load command's type and bytes, or `None` for a malformed table.
    fn commands(&self) -> Option<Vec<(u32, &'a [u8])>> {
        let mut at = 0usize;
        let mut commands = Vec::new();
        for _ in 0..self.count {
            let kind = u32_at(self.commands, at)?;
            let size = usize::try_from(u32_at(self.commands, at + 4)?).ok()?;
            if size < 8 || size % 8 != 0 {
                return None;
            }
            commands.push((kind, self.commands.get(at..at.checked_add(size)?)?));
            at += size;
        }
        Some(commands)
    }

    /// Every section of every `LC_SEGMENT_64`, in order: `n_sect` counts from one.
    fn sections(&self) -> Option<Vec<MachSection<'a>>> {
        let mut sections = Vec::new();
        for (kind, command) in self.commands()? {
            if kind != LC_SEGMENT_64 {
                continue;
            }
            let count = usize::try_from(u32_at(command, 64)?).ok()?;
            for index in 0..count {
                let at = 72usize.checked_add(index.checked_mul(80)?)?;
                let row = command.get(at..at.checked_add(80)?)?;
                sections.push(MachSection {
                    name: fixed_name(&row[..16]),
                    segment: fixed_name(&row[16..32]),
                    size: u64_at(row, 40)?,
                    offset: u32_at(row, 48)?,
                    flags: u32_at(row, 64)?,
                });
            }
        }
        Some(sections)
    }

    /// The symbol table's entries, or `None` for a malformed or repeated `LC_SYMTAB`.
    fn symbols(&self) -> Option<impl Iterator<Item = Option<MachSymbol<'a>>>> {
        let mut tables = self
            .commands()?
            .into_iter()
            .filter(|(kind, _)| *kind == LC_SYMTAB);
        let (table, strings) = match (tables.next(), tables.next()) {
            (None, _) => (&[][..], &[][..]),
            (Some((_, command)), None) => {
                let span = |offset: usize, len: usize| {
                    let start = usize::try_from(u32_at(command, offset)?).ok()?;
                    let len = usize::try_from(u32_at(command, len)?).ok()?;
                    self.bytes.get(start..start.checked_add(len)?)
                };
                let count = usize::try_from(u32_at(command, 12)?).ok()?;
                let start = usize::try_from(u32_at(command, 8)?).ok()?;
                let table = self
                    .bytes
                    .get(start..start.checked_add(count.checked_mul(16)?)?)?;
                (table, span(16, 20)?)
            }
            (Some(_), Some(_)) => return None,
        };
        Some(table.chunks_exact(16).map(move |row| {
            let name = strings.get(usize::try_from(u32_at(row, 0)?).ok()?..)?;
            Some(MachSymbol {
                name: fixed_name(name),
                kind: row[4],
                section: row[5],
                desc: u16::from_le_bytes([row[6], row[7]]),
                value: u64_at(row, 8)?,
            })
        }))
    }
}

impl<'a> MachSection<'a> {
    fn kind(&self) -> u32 {
        self.flags & SECTION_TYPE
    }

    fn content(&self, bytes: &'a [u8]) -> Option<&'a [u8]> {
        let start = usize::try_from(self.offset).ok()?;
        bytes.get(start..start.checked_add(usize::try_from(self.size).ok()?)?)
    }
}

/// A relocatable arm64 Mach-O member for this Apple platform and floor.
pub(super) fn parse_member(
    bytes: &[u8],
    name: &str,
    apple: &AppleRecord,
    budget: &Debit<'_>,
) -> DecodeResult<Option<Member>> {
    let Some(file) = MachO::parse(bytes) else {
        return Ok(None);
    };
    let (Some(commands), Some(sections), Some(symbols)) =
        (file.commands(), file.sections(), file.symbols())
    else {
        return Ok(None);
    };
    let mut builds = commands
        .iter()
        .filter(|(kind, _)| *kind == LC_BUILD_VERSION)
        .map(|(_, command)| (u32_at(command, 8), u32_at(command, 12)));
    let build = match (builds.next(), builds.next()) {
        (Some((Some(platform), Some(minos))), None) => {
            platform == apple.macho_platform && minos <= apple.macho_minos
        }
        _ => false,
    };
    if file.header(4) != CPU_TYPE_ARM64
        || file.header(8) != CPU_SUBTYPE_ARM64_ALL
        || file.header(12) != MH_OBJECT
        || !build
    {
        return Ok(None);
    }
    budget.charge_bytes(name.len())?;
    let mut member = Member {
        name: name.into(),
        symbols: vec![],
        imports: vec![],
        abi: vec![],
        bad_feature: None,
    };
    // Load commands in order, a segment's sections where it stands.
    for (kind, _) in &commands {
        if member.bad_feature.is_some() {
            break;
        }
        match LOAD_COMMANDS.iter().find(|(code, _, _)| code == kind) {
            Some((_, _, true)) => {}
            Some((_, name, false)) => member.bad_feature = Some(format!("load command {name}")),
            None => member.bad_feature = Some(format!("load command {kind:#x}")),
        }
        if *kind == LC_SEGMENT_64 && member.bad_feature.is_none() {
            member.bad_feature = sections.iter().find_map(|section| {
                let kind = section.kind();
                let admitted = SECTION_TYPES
                    .get(kind as usize)
                    .is_some_and(|(_, admitted)| *admitted);
                (!admitted || section.segment == b"__LLVM").then(|| {
                    let kind = SECTION_TYPES
                        .get(kind as usize)
                        .map_or_else(|| format!("{kind:#x}"), |(name, _)| (*name).to_owned());
                    format!(
                        "section {},{} of type {kind}",
                        String::from_utf8_lossy(section.segment),
                        String::from_utf8_lossy(section.name),
                    )
                })
            });
        }
    }
    for section in &sections {
        if section.segment == b"__TEXT" && section.name == b"__hirsel_abi" {
            // A zero-fill or literal section cannot supply ABI text.
            let text = if section.kind() == S_REGULAR {
                let Some(content) = section.content(bytes) else {
                    return Ok(None);
                };
                Some(content.to_vec())
            } else {
                None
            };
            budget.charge_bytes(text.as_ref().map_or(0, Vec::len))?;
            budget.reserve(&mut member.abi, 1)?;
            member.abi.push(text);
        }
    }
    let mut first_bad_symbol = None;
    for symbol in symbols {
        let Some(symbol) = symbol else {
            return Ok(None);
        };
        if symbol.debug() {
            continue;
        }
        let in_section = symbol.kind & N_TYPE == N_SECT;
        let section = in_section
            .then(|| sections.get(usize::from(symbol.section).checked_sub(1)?))
            .flatten();
        if in_section && section.is_none() {
            return Ok(None);
        }
        if first_bad_symbol.is_none() && in_section && symbol.desc & N_SYMBOL_RESOLVER != 0 {
            first_bad_symbol = Some(format!("symbol resolver {}", c_name(symbol.name, true)));
        }
        let name = symbol.name.to_vec();
        if symbol.imports() && !name.is_empty() {
            budget.charge_bytes(name.len())?;
            budget.reserve(&mut member.imports, 1)?;
            member.imports.push(name.clone());
        }
        let strong_function = symbol.kind & N_EXT != 0
            && section.is_some_and(|section| {
                section.flags & (S_ATTR_SOME_INSTRUCTIONS | S_ATTR_PURE_INSTRUCTIONS) != 0
            });
        budget.charge_bytes(name.len())?;
        budget.reserve(&mut member.symbols, 1)?;
        member.symbols.push(Symbol {
            name,
            definition: symbol.defines(),
            strong_function,
        });
    }
    member.bad_feature = member.bad_feature.or(first_bad_symbol);
    budget.charge_bytes(member.bad_feature.as_ref().map_or(0, String::len))?;
    Ok(Some(member))
}

/// Call `defined` with each symbol a Mach-O link input defines, or `None`
/// when `bytes` is not a well-formed Mach-O file.
pub(super) fn definitions(
    bytes: &[u8],
    defined: &mut dyn FnMut(&[u8]) -> Result<(), AdmissionError>,
) -> Option<Result<(), AdmissionError>> {
    let file = MachO::parse(bytes)?;
    for symbol in file.symbols()? {
        let symbol = symbol?;
        if symbol.defines() {
            if let Err(error) = defined(symbol.name) {
                return Some(Err(error));
            }
        }
    }
    Some(Ok(()))
}

#[cfg(test)]
mod tests {
    /// FFI-3's Mach-O rows and its import check on Apple targets.
    /// Each expected line is the spec's diagnostic (docs/spec/foreign.md, FFI-3,
    /// "Mach-O members" and "Imports") filled in by hand: the C name without
    /// Mach-O's leading `_`, the feature named as LLVM's `MachO.h` names it.
    /// Bugs: a member admitted for the wrong CPU (arm64e) or a later macOS than
    /// the floor; a load command that loads a library (`LC_LINKER_OPTION`) or an
    /// initializer section passing as an object feature; the `_` of a Mach-O
    /// symbol breaking the definition or prefix rows; and an archive whose
    /// member calls `fsync`, which the libSystem allowlist leaves out, reaching
    /// the linker.
    #[test]
    fn macho_foreign_archives_check_the_mach_o_rows_and_imports() {
        use crate::code_store::ObjectKind;
        use crate::foreign_archive::{LinkArchives, SealedArchive};
        use crate::tool_inputs::{LinkRecipe, RecipeFile, RecipeStat, Trust};
        use hirsel_core_ids::ContentHash;
        use hirsel_core_ids::DefPath;
        use hirsel_core_ir::Identity;
        use hirsel_core_ir::decode::Debit;
        use hirsel_core_ir::release_recipe::{ForeignFunction, ForeignLeaf, ObjectRef};
        use std::path::PathBuf;
        use std::process::Command;

        let target = "aarch64-apple-darwin";
        let module = DefPath::from_full("ffi_laws@1/native").unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
        let abi = r#"
int hfl_touch(void) { return 0; }
__attribute__((used, section("__TEXT,__hirsel_abi")))
static const char abi[sizeof("hfl_touch stack () -> i32\n") - 1] = "hfl_touch stack () -> i32\n";
"#;
        let run = |command: &mut Command| {
            let output = command.current_dir(fixture.path()).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        // One member, laws.o, compiled for `triple` from `source`.
        let compile = |triple: &str, source: &str| {
            std::fs::write(fixture.path().join("laws.c"), source).unwrap();
            let _ = std::fs::remove_file(fixture.path().join("laws.a"));
            run(Command::new(llvm.join("clang"))
                .arg(format!("--target={triple}"))
                .args(["-std=c11", "-O2", "-c", "laws.c", "-o", "laws.o"]));
            run(Command::new(llvm.join("llvm-ar")).args([
                "--format=darwin",
                "rcs",
                "laws.a",
                "laws.o",
            ]));
            let bytes = std::fs::read(fixture.path().join("laws.a")).unwrap();
            let object = ObjectRef {
                hash: ObjectKind::ForeignArchive.hash(&bytes),
                len: bytes.len() as u64,
            };
            (bytes, object)
        };
        let build = |triple: &str, extra: &str| compile(triple, &format!("{abi}{extra}"));
        let leaf = ForeignLeaf {
            module: module.clone(),
            symbols: "hfl_".into(),
            code: hirsel_core_ir::release_recipe::ForeignCode::Native(vec![]),
            functions: vec![ForeignFunction {
                identity: Identity::plain(DefPath::from_full("ffi_laws@1/native/touch").unwrap()),
                header: "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->i32".into(),
            }],
        };
        let prefix_for = |target: &str, object: ObjectRef| {
            format!(
                "FFI-3: archive object {} of ffi_laws@1/native for {target}",
                object.hash
            )
        };
        let prefix = |object| prefix_for(target, object);
        let admit_for = |target: &str, (bytes, object): (Vec<u8>, ObjectRef)| {
            SealedArchive::open(bytes, object, &module, target, None, &Debit::unlimited())
                .and_then(|archive| archive.check_headers(object, &leaf, target, None))
                .map_err(|error| error.to_string())
        };
        let admit = |built| admit_for(target, built);
        use hirsel_core_layout::Target;
        for (target, other, earlier) in [
            (
                Target::Aarch64Ios,
                Target::Aarch64IosSim,
                "arm64-apple-ios17.0",
            ),
            (
                Target::Aarch64IosSim,
                Target::Aarch64Ios,
                "arm64-apple-ios17.0-simulator",
            ),
        ] {
            let triple = target.triple();
            let apple = target.apple().unwrap();
            let later = apple
                .llvm_triple
                .replacen(apple.floor, &format!("{}.1", apple.floor), 1);
            admit_for(triple, build(apple.llvm_triple, "")).unwrap();
            admit_for(triple, build(earlier, "")).unwrap();
            for rejected in [other.llvm_triple(), &later] {
                let archive = build(rejected, "");
                let expected = format!(
                    "{}: member laws.o is not a relocatable object for {triple}",
                    prefix_for(triple, archive.1),
                );
                assert_eq!(
                    admit_for(triple, archive).unwrap_err(),
                    expected,
                    "{rejected}"
                );
            }
        }
        let apple = Target::Aarch64Darwin.apple().unwrap();
        let arm64 = apple.llvm_triple;
        let arm64e = arm64.replacen("arm64", "arm64e", 1);
        let x86 = arm64.replacen("arm64", "x86_64", 1);
        let later = format!("{arm64}.1");
        admit(build(arm64, "")).unwrap();
        for (triple, extra, feature) in [
            (arm64e.as_str(), "", None),
            (later.as_str(), "", None),
            (x86.as_str(), "", None),
            (
                arm64,
                "__asm__(\".linker_option \\\"-lz\\\"\");\n",
                Some("load command LC_LINKER_OPTION"),
            ),
            (
                arm64,
                "static volatile int flag;\n\
                 __attribute__((constructor)) static void initialize(void) { flag = 1; }\n",
                Some("section __DATA,__mod_init_func of type S_MOD_INIT_FUNC_POINTERS"),
            ),
            (
                arm64,
                "__asm__(\".text\\n.globl _hfl_pick\\n.symbol_resolver _hfl_pick\\n_hfl_pick: ret\\n\");\n",
                Some("symbol resolver hfl_pick"),
            ),
            (
                arm64,
                "__asm__(\".section __LLVM,__bitcode\\n.byte 0\\n\");\n",
                Some("section __LLVM,__bitcode of type S_REGULAR"),
            ),
        ] {
            let built = build(triple, extra);
            let object = built.1;
            let expected = match feature {
                None => format!(
                    "{}: member laws.o is not a relocatable object for {target}",
                    prefix(object)
                ),
                Some(feature) => format!(
                    "{}: member laws.o has {feature}, which is not an admitted object feature",
                    prefix(object)
                ),
            };
            assert_eq!(admit(built).unwrap_err(), expected, "{triple} {extra}");
        }
        let built = build(arm64, "int other(void) { return 1; }\n");
        let object = built.1;
        assert_eq!(
            admit(built).unwrap_err(),
            format!(
                "{}: member laws.o defines other, which does not start with the prefix hfl_",
                prefix(object)
            )
        );
        // ABI text in a section other than __TEXT,__hirsel_abi is no ABI text.
        let built = compile(
            arm64,
            "int hfl_touch(void) { return 0; }\n\
             __attribute__((used, section(\"__DATA,__hirsel_abi\")))\n\
             static const char abi[sizeof(\"hfl_touch stack () -> i32\\n\") - 1] = \"hfl_touch stack () -> i32\\n\";\n",
        );
        let object = built.1;
        assert_eq!(
            admit(built).unwrap_err(),
            format!("{} has no ABI text", prefix(object))
        );

        // Imports: the recipe's text stub is the only system library.
        let stub = fixture.path().join("libSystem.tbd");
        let tbd_target = format!("arm64-{}", apple.platform_version_name);
        std::fs::write(
            &stub,
            format!(
                "--- !tapi-tbd\ntbd-version: 4\ntargets: [ {tbd_target} ]\n\
             install-name: '/usr/lib/libSystem.B.dylib'\ncurrent-version: 1\nexports:\n  \
             - targets: [ {tbd_target} ]\n    symbols: [ '_fcntl' ]\n\
             undefineds:\n  - targets: [ {tbd_target} ]\n    symbols: [ '_fsync' ]\n...\n"
            ),
        )
        .unwrap();
        let runtime = fixture.path().join("runtime.o");
        std::fs::write(fixture.path().join("runtime.c"), "int hirsel_runtime;\n").unwrap();
        run(Command::new(llvm.join("clang"))
            .arg(format!("--target={arm64}"))
            .args(["-c", "runtime.c", "-o", "runtime.o"]));
        let recorded = |path: &std::path::Path| RecipeFile {
            path: path.to_owned(),
            content: ContentHash::default(),
            stat: RecipeStat {
                dev: 0,
                ino: 0,
                len: 0,
                mode: 0,
                mtime: 0,
                mtime_nsec: 0,
                ctime: 0,
                ctime_nsec: 0,
            },
            trust: Trust::Settled,
        };
        let plan = LinkRecipe {
            schema: 0,
            target: target.into(),
            linker: llvm.join("ld.lld"),
            args: vec![],
            files: vec![recorded(&llvm.join("ld.lld")), recorded(&stub)],
            source_files: vec![],
            absent: vec![],
            directories: vec![],
            binaries: vec![stub.clone()],
            identity: ContentHash::default(),
        };
        let link = |extra: &str| {
            let (bytes, object) = build(arm64, extra);
            let mut staged = LinkArchives::default();
            staged
                .admit(
                    object,
                    &leaf,
                    target,
                    Some("laws.a"),
                    &Debit::unlimited(),
                    || Ok(bytes),
                )
                .unwrap();
            let mut inputs = vec![];
            staged
                .append_link_inputs(&mut inputs, &[], &runtime, &plan)
                .map_err(|error| error.to_string())
        };
        link("extern int fcntl(int, int, ...);\nint hfl_sync(int fd) { return fcntl(fd, 51); }\n")
            .unwrap();
        assert_eq!(
            link("extern int fsync(int);\nint hfl_sync(int fd) { return fsync(fd); }\n")
                .unwrap_err(),
            format!(
                "FFI-3: archive laws.a of ffi_laws@1/native for {target}: member laws.o imports fsync, which no admitted input defines"
            )
        );
    }
}
