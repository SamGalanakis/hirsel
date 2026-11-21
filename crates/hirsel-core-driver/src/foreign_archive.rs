//! Validate a sealed archive's own objects before any use as a link input.
use crate::code_admission::AdmissionError;
use crate::code_store::ObjectKind;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::decode::DecodeBudget;
use hirsel_core_ir::release_recipe::{ForeignLeaf, ObjectRef};
use object::read::archive::{ArchiveFile, ArchiveKind};
use object::read::elf::{ElfFile64, FileHeader, Rel, Rela, SectionHeader, Sym};
use object::{LittleEndian, elf};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

const AR: &[u8] = b"!<arch>\n";
const THIN: &[u8] = b"!<thin>\n";
const LLVM_ADDRSIG: elf::SectionType = elf::SectionType(0x6fff_4c03);

pub(crate) struct SealedArchive {
    _file: File,
    indexed: BTreeMap<Vec<u8>, Vec<u64>>,
    members: BTreeMap<u64, Member>,
}

struct Member {
    name: String,
    symbols: Vec<Symbol>,
    abi: Vec<Option<Vec<u8>>>,
    bad_feature: Option<String>,
}

struct Symbol {
    name: Vec<u8>,
    definition: bool,
    strong_function: bool,
}

pub(crate) fn check_distinct_definitions<'a>(
    archives: impl IntoIterator<Item = (&'a SealedArchive, ObjectRef, &'a DefPath)>,
    budget: &mut DecodeBudget,
) -> Result<(), AdmissionError> {
    let mut seen = BTreeSet::new();
    let mut definitions = BTreeMap::<&[u8], (ContentHash, &DefPath)>::new();
    for (archive, object, module) in archives {
        budget.check()?;
        if !seen.insert(object.hash) {
            continue;
        }
        for member in archive.members.values() {
            for symbol in member.symbols.iter().filter(|symbol| symbol.definition) {
                budget.charge_slots(1)?;
                match definitions.entry(&symbol.name) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        budget.charge_bytes(size_of::<(&[u8], ContentHash, &DefPath)>())?;
                        entry.insert((object.hash, module));
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        let (previous, owner) = entry.get();
                        if *previous != object.hash {
                            return Err(AdmissionError::Refused(format!(
                                "FFI-3: symbol {} is defined by archive object {} of {} and by archive object {} of {}",
                                String::from_utf8_lossy(&symbol.name),
                                crate::call::hex(&previous.0),
                                owner.definition_identity(),
                                crate::call::hex(&object.hash.0),
                                module.definition_identity(),
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn refuse(context: &str, problem: impl std::fmt::Display) -> AdmissionError {
    AdmissionError::Refused(format!("FFI-3: {context}{problem}"))
}

fn context(archive: ObjectRef, module: &DefPath, target: &str) -> String {
    format!(
        "archive object {} of {} for {target}",
        crate::call::hex(&archive.hash.0),
        module.definition_identity(),
    )
}

impl SealedArchive {
    pub(crate) fn open(
        mut bytes: Vec<u8>,
        archive: ObjectRef,
        module: &DefPath,
        target: &str,
    ) -> Result<Self, AdmissionError> {
        let seal_error =
            || AdmissionError::Refused("FFI-3: this host cannot seal staged archives".into());
        let mut file = seal(&bytes).map_err(|_| seal_error())?;
        let context = context(archive, module, target);
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(0))?;
        let observed = if length == bytes.len() as u64 {
            file.read_exact(&mut bytes)?;
            ObjectKind::ForeignArchive.hash(&bytes)
        } else {
            ObjectKind::ForeignArchive
                .hash_stream(length, |hash| std::io::copy(&mut file, hash).map(|_| ()))?
        };
        if observed != archive.hash {
            return Err(refuse(
                &context,
                format!(
                    " does not match its pin: expected sha256 {}, found sha256 {}",
                    crate::call::hex(&archive.hash.0),
                    crate::call::hex(&observed.0),
                ),
            ));
        }
        if length != archive.len || length != bytes.len() as u64 {
            return Err(AdmissionError::Binding(
                "inconsistent length for a sealed foreign archive",
            ));
        }
        if bytes.starts_with(THIN) {
            return Err(refuse(
                &context,
                " is a thin archive; its members are not inside the pinned bytes",
            ));
        }
        let bad_ar = || refuse(&context, " is not an indexed ar archive");
        if !bytes.starts_with(AR) {
            return Err(bad_ar());
        }
        let ar = ArchiveFile::parse(bytes.as_slice()).map_err(|_| bad_ar())?;
        if !matches!(
            ar.kind(),
            ArchiveKind::Gnu | ArchiveKind::Gnu64 | ArchiveKind::Bsd | ArchiveKind::Bsd64
        ) {
            return Err(bad_ar());
        }
        let indexed = ar.symbols().map_err(|_| bad_ar())?.ok_or_else(bad_ar)?;
        let mut objects = Vec::new();
        for member in ar.members() {
            let member = member.map_err(|_| bad_ar())?;
            let name = member.name();
            if name.is_empty()
                || matches!(name, b"." | b"..")
                || name.iter().any(|b| matches!(b, b'/' | b'\\' | 0))
            {
                return Err(bad_ar());
            }
            let head = member.header().ok_or_else(bad_ar)?;
            let offset = std::ptr::from_ref(head)
                .addr()
                .checked_sub(bytes.as_ptr().addr())
                .ok_or_else(bad_ar)?;
            let data = member.data(bytes.as_slice()).map_err(|_| bad_ar())?;
            objects.push((
                offset as u64,
                String::from_utf8_lossy(name).into_owned(),
                data,
            ));
        }
        // An index names actual member headers, never a location inside an object.
        let mut symbols = BTreeMap::<Vec<u8>, Vec<u64>>::new();
        for entry in indexed {
            let entry = entry.map_err(|_| bad_ar())?;
            let offset = entry.offset().0;
            if objects
                .binary_search_by_key(&offset, |&(offset, _, _)| offset)
                .is_err()
            {
                return Err(bad_ar());
            }
            symbols
                .entry(entry.name().to_vec())
                .or_default()
                .push(offset);
        }
        let mut members = BTreeMap::new();
        for (offset, name, bytes) in objects {
            let bad_member = || {
                refuse(
                    &context,
                    format!(": member {name} is not a relocatable object for {target}"),
                )
            };
            let member = parse_member(bytes, &name, target).ok_or_else(bad_member)?;
            members.insert(offset, member);
        }
        Ok(Self {
            _file: file,
            indexed: symbols,
            members,
        })
    }

    pub(crate) fn check_headers(
        &self,
        archive: ObjectRef,
        leaf: &ForeignLeaf,
        target: &str,
        entry: &DefPath,
    ) -> Result<(), AdmissionError> {
        let context = context(archive, &leaf.module, target);
        let site = entry.definition_identity();
        for function in &leaf.functions {
            let symbol = format!("{}{}", leaf.symbols, function.identity.definition.0.name);
            let defines = self.indexed.get(symbol.as_bytes()).is_some_and(|rows| {
                rows.iter().all(|offset| {
                    self.members[offset].symbols.iter().any(|definition| {
                        definition.strong_function && definition.name == symbol.as_bytes()
                    })
                })
            });
            if !defines {
                return Err(refuse(
                    &context,
                    format!(" does not define {symbol}, declared at {site}"),
                ));
            }
        }
        for member in self.members.values() {
            if let Some(feature) = &member.bad_feature {
                return Err(refuse(
                    &context,
                    format!(
                        ": member {} has {feature}, which is not an admitted object feature",
                        member.name,
                    ),
                ));
            }
        }
        for member in self.members.values() {
            if let Some(symbol) = member
                .symbols
                .iter()
                .find(|s| s.definition && !s.name.starts_with(leaf.symbols.as_bytes()))
            {
                let symbol = String::from_utf8_lossy(&symbol.name);
                return Err(refuse(
                    &context,
                    format!(
                        ": member {} defines {symbol}, which does not start with the prefix {}",
                        member.name, leaf.symbols,
                    ),
                ));
            }
        }
        let mut abi = self.members.values().flat_map(|m| &m.abi);
        let [Some(Some(abi)), None] = [abi.next(), abi.next()] else {
            return Err(refuse(&context, " has no ABI text"));
        };
        let abi = std::str::from_utf8(abi).unwrap_or("");
        for function in &leaf.functions {
            let symbol = format!("{}{}", leaf.symbols, function.identity.definition.0.name);
            let expected = abi_line(&symbol, &function.header)
                .ok_or(AdmissionError::Binding("invalid foreign function header"))?;
            let mut lines = abi
                .split_inclusive('\n')
                .filter_map(|line| line.strip_suffix('\n'))
                .filter(|line| {
                    line.strip_prefix(&symbol)
                        .is_some_and(|rest| rest.starts_with(' '))
                });
            let actual = match (lines.next(), lines.next()) {
                (Some(line), None) => line,
                _ => "missing",
            };
            if actual != expected {
                return Err(refuse(
                    &context,
                    format!(
                        ": the ABI of {symbol} is {actual}, but its header declares {expected}, declared at {site}"
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn seal(bytes: &[u8]) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: the name is NUL-terminated and these open flags take no other arguments.
    let fd = unsafe {
        libc::memfd_create(
            c"hirsel-foreign".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a fresh owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    // SAFETY: the file is still open and F_ADD_SEALS takes a single integer argument.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(not(target_os = "linux"))]
fn seal(_: &[u8]) -> std::io::Result<File> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn parse_member(bytes: &[u8], name: &str, target: &str) -> Option<Member> {
    let (machine, unwind, irelative) = match target {
        "x86_64-unknown-linux-gnu" => (
            elf::EM_X86_64,
            elf::SHT_X86_64_UNWIND,
            elf::R_X86_64_IRELATIVE,
        ),
        "aarch64-unknown-linux-gnu" => (
            elf::EM_AARCH64,
            elf::SHT_AARCH64_ATTRIBUTES,
            elf::R_AARCH64_IRELATIVE,
        ),
        _ => return None,
    };
    let endian = LittleEndian;
    let file = ElfFile64::<LittleEndian>::parse(bytes).ok()?;
    let header = file.elf_header();
    if header.e_type(endian) != elf::ET_REL
        || header.e_machine(endian) != machine
        || !matches!(
            header.e_ident().os_abi,
            elf::ELFOSABI_NONE | elf::ELFOSABI_GNU
        )
    {
        return None;
    }
    let mut member = Member {
        name: name.into(),
        symbols: vec![],
        abi: vec![],
        bad_feature: None,
    };
    let sections = file.elf_section_table();
    let mut stack = false;
    let mut first_bad_symbol = None;
    for (index, section) in sections.enumerate().skip(1) {
        let kind = section.sh_type(endian);
        let name = sections.section_name(endian, section).ok()?;
        let content = section.data(endian, bytes).ok()?;
        if (!matches!(
            kind,
            elf::SHT_NULL
                | elf::SHT_PROGBITS
                | elf::SHT_NOBITS
                | elf::SHT_SYMTAB
                | elf::SHT_STRTAB
                | elf::SHT_RELA
                | elf::SHT_REL
                | elf::SHT_NOTE
                | elf::SHT_GROUP
        ) && kind != LLVM_ADDRSIG
            && kind != unwind)
            || forbidden_section(name)
            || (kind == elf::SHT_NOTE
                && !matches!(name, b".note.GNU-stack" | b".note.gnu.property"))
        {
            member.bad_feature.get_or_insert_with(|| {
                let section_type = if kind == LLVM_ADDRSIG {
                    "SHT_LLVM_ADDRSIG".to_owned()
                } else {
                    elf::machine_names(machine)
                        .sht
                        .name(kind)
                        .map_or_else(|| format!("{:#x}", kind.0), str::to_owned)
                };
                format!(
                    "section {} of type {section_type}",
                    String::from_utf8_lossy(name)
                )
            });
        }
        if name == b".note.GNU-stack" {
            stack = true;
            if section.sh_flags(endian).contains(elf::SHF_EXECINSTR) {
                member
                    .bad_feature
                    .get_or_insert_with(|| "an executable stack".into());
            }
        }
        if name == b".hirsel_abi" {
            // A non-PROGBITS section with this name cannot supply ABI text.
            member
                .abi
                .push((kind == elf::SHT_PROGBITS).then(|| content.to_vec()));
        }
        if matches!(kind, elf::SHT_SYMTAB | elf::SHT_DYNSYM) {
            let table = sections.symbol_table_by_index(endian, bytes, index).ok()?;
            for (index, symbol) in table.enumerate().skip(1) {
                let name = table.symbol_name(endian, symbol).ok()?.to_vec();
                let bind = symbol.st_bind();
                if first_bad_symbol.is_none() {
                    if symbol.st_type() == elf::STT_GNU_IFUNC {
                        first_bad_symbol = Some(format!(
                            "indirect function {}",
                            String::from_utf8_lossy(&name)
                        ));
                    } else if bind == elf::STB_GNU_UNIQUE {
                        first_bad_symbol = Some(format!(
                            "symbol {} with binding STB_GNU_UNIQUE",
                            String::from_utf8_lossy(&name)
                        ));
                    }
                }
                let definition = matches!(bind, elf::STB_GLOBAL | elf::STB_WEAK)
                    && symbol.st_shndx(endian) != elf::SHN_UNDEF;
                let strong_function = bind == elf::STB_GLOBAL
                    && symbol.st_type() == elf::STT_FUNC
                    && symbol.st_visibility() == elf::STV_DEFAULT
                    && table
                        .symbol_section(endian, symbol, index)
                        .ok()?
                        .is_some_and(|index| sections.section(index).is_ok());
                // Symbol refusals are decided only after all section checks.
                member.symbols.push(Symbol {
                    name,
                    definition,
                    strong_function,
                });
            }
        }
    }
    if !stack {
        member
            .bad_feature
            .get_or_insert_with(|| "no .note.GNU-stack".into());
    }
    member.bad_feature = member.bad_feature.or(first_bad_symbol);
    if member.bad_feature.is_none() {
        for section in sections.iter().skip(1) {
            let rel = section.rel(endian, bytes).ok()?;
            let rela = section.rela(endian, bytes).ok()?;
            if rel.is_some_and(|(rows, _)| rows.iter().any(|r| r.r_type(endian) == irelative))
                || rela.is_some_and(|(rows, _)| {
                    rows.iter().any(|r| r.r_type(endian, false) == irelative)
                })
            {
                member.bad_feature = Some("an IRELATIVE relocation".into());
                break;
            }
        }
    }
    Some(member)
}

fn forbidden_section(name: &[u8]) -> bool {
    let c_identifier = name.split_first().is_some_and(|(&first, tail)| {
        (first == b'_' || first.is_ascii_alphabetic())
            && tail.iter().all(|b| *b == b'_' || b.is_ascii_alphanumeric())
    });
    c_identifier
        || [
            b".init".as_slice(),
            b".fini",
            b".ctors",
            b".dtors",
            b".init_array",
            b".fini_array",
            b".preinit_array",
        ]
        .iter()
        .any(|prefix| {
            name.strip_prefix(*prefix)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(b"."))
        })
}

// PROG-9 writes the types in this header with aliases expanded and no parameter names.
fn abi_line(symbol: &str, header: &str) -> Option<String> {
    let (params, result) = header.strip_prefix('(')?.split_once(")->")?;
    let (result, mode) = result
        .strip_suffix("waits")
        .map_or((result, "stack"), |r| (r.trim_end(), "waits"));
    let mut params = params.split(',');
    let interface = params
        .next()?
        .strip_prefix("core@1/core/Foreign[")?
        .strip_suffix(']')?;
    definition_path(interface)?;
    let mut args = Vec::new();
    for param in params {
        let text = match param {
            "core@1/core/List[u8]" => "bytes".into(),
            "mut core@1/core/List[u8]" => "mut bytes".into(),
            scalar if ffi_scalar(scalar) => scalar.into(),
            param => {
                let (value, mode) = param
                    .strip_prefix("take ")
                    .map_or((param, "handle"), |argument| (argument, "take"));
                format!("{mode} {}", handle_kind(value)?)
            }
        };
        args.push(text);
    }
    let result = match result {
        "()" => "()".into(),
        scalar if ffi_scalar(scalar) => scalar.into(),
        result => format!(
            "new {}",
            handle_kind(
                result
                    .strip_prefix("core@1/core/Option[")?
                    .strip_suffix(']')?
            )?
        ),
    };
    Some(format!("{symbol} {mode} ({}) -> {result}", args.join(", ")))
}

fn ffi_scalar(text: &str) -> bool {
    matches!(
        text,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64"
    )
}

fn handle_kind(text: &str) -> Option<&str> {
    definition_path(
        text.strip_prefix("core@1/foreign/Ptr[")?
            .strip_suffix(']')?,
    )
}

fn definition_path(text: &str) -> Option<&str> {
    (text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_@/%".contains(&b))
        && DefPath::from_full(text).is_some())
    .then_some(text)
}
