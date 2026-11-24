//! Validate a sealed archive's own objects before any use as a link input.
use crate::code_admission::AdmissionError;
use crate::code_store::ObjectKind;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::decode::{Debit, DecodeResult};
use hirsel_core_ir::release_recipe::{ForeignLeaf, ObjectRef};
use object::read::archive::{ArchiveFile, ArchiveKind};
use object::read::elf::{ElfFile64, FileHeader, SectionHeader, Sym};
use object::{LittleEndian, elf};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

const AR: &[u8] = b"!<arch>\n";
const THIN: &[u8] = b"!<thin>\n";
const LLVM_ADDRSIG: elf::SectionType = elf::SectionType(0x6fff_4c03);

pub(crate) struct SealedArchive {
    file: File,
    indexed: BTreeMap<Vec<u8>, IndexedSymbol>,
    members: Vec<Member>,
}

/// One sealed input per pin. The module and path are those of the first
/// selecting row and name the archive in collision diagnostics.
#[derive(Default)]
pub(crate) struct LinkArchives(BTreeMap<ContentHash, StagedArchive>);

struct StagedArchive {
    archive: SealedArchive,
    module: DefPath,
    object: ObjectRef,
    path: Option<String>,
}

impl LinkArchives {
    pub(crate) fn admit(
        &mut self,
        object: ObjectRef,
        leaf: &ForeignLeaf,
        target: &str,
        path: Option<&str>,
        budget: &Debit<'_>,
        bytes: impl FnOnce() -> Result<Vec<u8>, AdmissionError>,
    ) -> Result<(), AdmissionError> {
        let row = match self.0.entry(object.hash) {
            std::collections::btree_map::Entry::Occupied(at) => at.into_mut(),
            std::collections::btree_map::Entry::Vacant(at) => at.insert(StagedArchive {
                archive: SealedArchive::open(bytes()?, object, &leaf.module, target, path, budget)?,
                module: leaf.module.clone(),
                object,
                path: path.map(str::to_owned),
            }),
        };
        row.archive.check_headers(object, leaf, target, path)
    }

    pub(crate) fn pins(&self) -> impl Iterator<Item = ContentHash> + '_ {
        self.0.keys().copied()
    }

    pub(crate) fn contains(&self, pin: ContentHash) -> bool {
        self.0.contains_key(&pin)
    }

    pub(crate) fn definitions(
        &self,
        budget: &Debit<'_>,
    ) -> Result<ForeignDefinitions<'_>, AdmissionError> {
        let rows = budget.scratch();
        rows.charge_bytes(self.0.len().saturating_mul(size_of::<&StagedArchive>()))?;
        let mut sorted: Vec<_> = self.0.values().collect();
        sorted.sort_unstable_by(|a, b| a.module.cmp(&b.module));
        check_distinct_definitions(
            sorted.into_iter().map(|row| {
                (
                    &row.archive,
                    ArchiveOrigin {
                        module: &row.module,
                        object: row.object,
                        path: row.path.as_deref(),
                    },
                )
            }),
            budget,
        )
    }

    /// Check the actual generated inputs, then give LLD only the sealed
    /// descriptors. The owner lives across LLD's invocation.
    pub(crate) fn append_link_inputs(
        &self,
        inputs: &mut Vec<PathBuf>,
        bundle: Option<&crate::native::link_input::Bundle>,
        runtime: &Path,
        recipe: &crate::tool_inputs::LinkRecipe,
    ) -> Result<(), AdmissionError> {
        if self.0.is_empty() {
            return Ok(());
        }
        let symbols = self.definitions(&Debit::unlimited())?;
        symbols.check_tool_inputs(runtime, recipe)?;
        // The bundle among `inputs` is checked member by member.
        let bundled = bundle.map(|b| b.path());
        for input in inputs.iter().filter(|i| Some(*i) != bundled.as_ref()) {
            symbols.check_link_input(&std::fs::read(input)?, "the generated code")?;
        }
        if let Some(bundle) = bundle {
            for n in 0..bundle.len() {
                symbols.check_link_input(&bundle.member(n)?, "the generated code")?;
            }
        }
        let pid = std::process::id();
        inputs.extend(
            self.0.values().map(|row| {
                PathBuf::from(format!("/proc/{pid}/fd/{}", row.archive.file.as_raw_fd()))
            }),
        );
        Ok(())
    }
}

struct IndexedSymbol {
    offset: Option<u64>,
    /// The first definition matches the indexed member and is a global function.
    /// A second definition makes the symbol ambiguous.
    definition_matches: Option<bool>,
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

#[derive(Clone, Copy)]
pub(crate) struct ArchiveOrigin<'a> {
    pub object: ObjectRef,
    pub module: &'a DefPath,
    pub path: Option<&'a str>,
}

impl ArchiveOrigin<'_> {
    fn label(self) -> String {
        format!(
            "archive {} of {}",
            archive_name(self.object, self.path),
            self.module.definition_identity()
        )
    }
}

pub(crate) struct ForeignDefinitions<'a>(BTreeMap<&'a [u8], ArchiveOrigin<'a>>);

pub(crate) fn check_distinct_definitions<'a>(
    archives: impl IntoIterator<Item = (&'a SealedArchive, ArchiveOrigin<'a>)>,
    budget: &Debit<'_>,
) -> Result<ForeignDefinitions<'a>, AdmissionError> {
    let mut seen = BTreeSet::new();
    let mut definitions = BTreeMap::<&[u8], ArchiveOrigin<'_>>::new();
    for (archive, origin) in archives {
        budget.check()?;
        if !seen.insert(origin.object.hash) {
            continue;
        }
        for member in &archive.members {
            for symbol in member.symbols.iter().filter(|symbol| symbol.definition) {
                budget.charge_slots(1)?;
                let name = symbol.name.split(|byte| *byte == b'@').next().unwrap();
                match definitions.entry(name) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        budget.charge_bytes(size_of::<(&[u8], ArchiveOrigin<'_>)>())?;
                        entry.insert(origin);
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        let previous = *entry.get();
                        if previous.object.hash != origin.object.hash {
                            return Err(collision(name, previous, &origin.label()));
                        }
                    }
                }
            }
        }
    }
    Ok(ForeignDefinitions(definitions))
}

fn collision(symbol: &[u8], archive: ArchiveOrigin<'_>, other: &str) -> AdmissionError {
    AdmissionError::Refused(format!(
        "FFI-3: symbol {} is defined by {} and by {other}",
        String::from_utf8_lossy(symbol),
        archive.label()
    ))
}

impl ForeignDefinitions<'_> {
    pub(crate) fn check_tool_inputs(
        &self,
        runtime: &Path,
        recipe: &crate::tool_inputs::LinkRecipe,
    ) -> Result<(), AdmissionError> {
        self.check_link_input(&std::fs::read(runtime)?, "the runtime")?;
        let closure =
            crate::link_closure::closure(&recipe.args, crate::tool_inputs::INPUT_MARKER, true)?;
        for input in closure.binaries {
            self.check_link_input(
                &std::fs::read(&input)?,
                &format!("link input {}", input.display()),
            )?;
        }
        Ok(())
    }

    pub(crate) fn check_link_input(&self, bytes: &[u8], label: &str) -> Result<(), AdmissionError> {
        let unsupported =
            || AdmissionError::Refused(format!("FFI-3: cannot read symbol definitions of {label}"));
        let check = |bytes: &[u8]| -> Result<(), AdmissionError> {
            let file = ElfFile64::<LittleEndian>::parse(bytes).map_err(|_| unsupported())?;
            let sections = file.elf_section_table();
            let endian = LittleEndian;
            for (index, section) in sections.enumerate() {
                if !matches!(section.sh_type(endian), elf::SHT_SYMTAB | elf::SHT_DYNSYM) {
                    continue;
                }
                let table = sections
                    .symbol_table_by_index(endian, bytes, index)
                    .map_err(|_| unsupported())?;
                for symbol in table.iter() {
                    if !matches!(
                        symbol.st_bind(),
                        elf::STB_GLOBAL | elf::STB_WEAK | elf::STB_GNU_UNIQUE
                    ) || symbol.st_shndx(endian) == elf::SHN_UNDEF
                    {
                        continue;
                    }
                    let name = table
                        .symbol_name(endian, symbol)
                        .map_err(|_| unsupported())?;
                    let base = name.split(|byte| *byte == b'@').next().unwrap();
                    if let Some(&archive) = self.0.get(base) {
                        return Err(collision(base, archive, label));
                    }
                }
            }
            Ok(())
        };
        if bytes.starts_with(AR) {
            let archive = ArchiveFile::parse(bytes).map_err(|_| unsupported())?;
            for member in archive.members() {
                check(
                    member
                        .map_err(|_| unsupported())?
                        .data(bytes)
                        .map_err(|_| unsupported())?,
                )?;
            }
            Ok(())
        } else {
            check(bytes)
        }
    }
}

fn refuse(context: &str, problem: impl std::fmt::Display) -> AdmissionError {
    AdmissionError::Refused(format!("FFI-3: {context}{problem}"))
}

fn archive_name(archive: ObjectRef, path: Option<&str>) -> String {
    path.map_or_else(
        || format!("object {}", crate::call::hex(&archive.hash.0)),
        str::to_owned,
    )
}

fn context(archive: ObjectRef, module: &DefPath, target: &str, path: Option<&str>) -> String {
    let file = archive_name(archive, path);
    format!(
        "archive {file} of {} for {target}",
        module.definition_identity(),
    )
}

impl SealedArchive {
    pub(crate) fn open(
        mut bytes: Vec<u8>,
        archive: ObjectRef,
        module: &DefPath,
        target: &str,
        path: Option<&str>,
        budget: &Debit<'_>,
    ) -> Result<Self, AdmissionError> {
        let seal_error =
            || AdmissionError::Refused("FFI-3: this host cannot seal staged archives".into());
        let mut file = seal(&bytes).map_err(|_| seal_error())?;
        let context = context(archive, module, target, path);
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
        // The member list lives while the members parse; the index and the
        // parsed members are held by the sealed archive.
        let listing = budget.scratch();
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
            listing.reserve(&mut objects, 1)?;
            listing.charge_bytes(name.len())?;
            objects.push((
                offset as u64,
                String::from_utf8_lossy(name).into_owned(),
                data,
            ));
        }
        // An index names actual member headers, never a location inside an object.
        let mut symbols = BTreeMap::<Vec<u8>, IndexedSymbol>::new();
        for entry in indexed {
            let entry = entry.map_err(|_| bad_ar())?;
            let offset = entry.offset().0;
            if objects
                .binary_search_by_key(&offset, |&(offset, _, _)| offset)
                .is_err()
            {
                return Err(bad_ar());
            }
            if let Some(symbol) = symbols.get_mut(entry.name()) {
                symbol.offset = None;
            } else {
                // A B-tree node is at least half full, so an entry holds at
                // most two slots besides its name.
                budget.charge_bytes(
                    size_of::<(Vec<u8>, IndexedSymbol)>()
                        .saturating_mul(2)
                        .saturating_add(entry.name().len()),
                )?;
                symbols.insert(
                    entry.name().to_vec(),
                    IndexedSymbol {
                        offset: Some(offset),
                        definition_matches: None,
                    },
                );
            }
        }
        budget.allocation::<Member>(objects.len())?;
        let mut members = Vec::with_capacity(objects.len());
        for (offset, name, bytes) in objects {
            let bad_member = || {
                refuse(
                    &context,
                    format!(": member {name} is not a relocatable object for {target}"),
                )
            };
            let member = parse_member(bytes, &name, target, budget)?.ok_or_else(bad_member)?;
            for symbol in member.symbols.iter().filter(|symbol| symbol.definition) {
                if let Some(indexed) = symbols.get_mut(&symbol.name) {
                    indexed.definition_matches = Some(
                        indexed.definition_matches.is_none()
                            && indexed.offset == Some(offset)
                            && symbol.strong_function,
                    );
                }
            }
            members.push(member);
        }
        Ok(Self {
            file,
            indexed: symbols,
            members,
        })
    }

    pub(crate) fn check_headers(
        &self,
        archive: ObjectRef,
        leaf: &ForeignLeaf,
        target: &str,
        path: Option<&str>,
    ) -> Result<(), AdmissionError> {
        let context = context(archive, &leaf.module, target, path);
        for function in &leaf.functions {
            let symbol = format!("{}{}", leaf.symbols, function.identity.definition.0.name);
            let defines = self
                .indexed
                .get(symbol.as_bytes())
                .is_some_and(|row| row.definition_matches == Some(true));
            if !defines {
                let site = function.identity.definition.definition_identity();
                return Err(refuse(
                    &context,
                    format!(" does not define {symbol}, declared at {site}"),
                ));
            }
        }
        for member in &self.members {
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
        for member in &self.members {
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
        let mut abi = self.members.iter().flat_map(|m| &m.abi);
        let [Some(Some(abi)), None] = [abi.next(), abi.next()] else {
            return Err(refuse(&context, " has no ABI text"));
        };
        let abi = std::str::from_utf8(abi).unwrap_or("");
        for function in &leaf.functions {
            let symbol = format!("{}{}", leaf.symbols, function.identity.definition.0.name);
            let expected = crate::foreign_header::Abi::parse(&function.header)
                .ok_or(AdmissionError::Binding("invalid foreign function header"))?
                .line(&symbol);
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
                let site = function.identity.definition.definition_identity();
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

/// The member `bytes` holds, or `None` when it is not a relocatable object for
/// `target`. What the member keeps is charged to `budget`.
fn parse_member(
    bytes: &[u8],
    name: &str,
    target: &str,
    budget: &Debit<'_>,
) -> DecodeResult<Option<Member>> {
    macro_rules! or_none {
        ($e:expr) => {
            match $e {
                Ok(value) => value,
                Err(_) => return Ok(None),
            }
        };
    }
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
        _ => return Ok(None),
    };
    let endian = LittleEndian;
    let file = or_none!(ElfFile64::<LittleEndian>::parse(bytes));
    let header = file.elf_header();
    if header.e_type(endian) != elf::ET_REL
        || header.e_machine(endian) != machine
        || !matches!(
            header.e_ident().os_abi,
            elf::ELFOSABI_NONE | elf::ELFOSABI_GNU
        )
    {
        return Ok(None);
    }
    budget.charge_bytes(name.len())?;
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
        let name = or_none!(sections.section_name(endian, section));
        let content = or_none!(section.data(endian, bytes));
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
            let text = (kind == elf::SHT_PROGBITS).then(|| content.to_vec());
            budget.charge_bytes(text.as_ref().map_or(0, Vec::len))?;
            budget.reserve(&mut member.abi, 1)?;
            member.abi.push(text);
        }
        if matches!(kind, elf::SHT_SYMTAB | elf::SHT_DYNSYM) {
            let table = or_none!(sections.symbol_table_by_index(endian, bytes, index));
            for (index, symbol) in table.enumerate().skip(1) {
                let name = or_none!(table.symbol_name(endian, symbol)).to_vec();
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
                    && or_none!(table.symbol_section(endian, symbol, index))
                        .is_some_and(|index| sections.section(index).is_ok());
                // Symbol refusals are decided only after all section checks.
                budget.charge_bytes(name.len())?;
                budget.reserve(&mut member.symbols, 1)?;
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
    budget.charge_bytes(member.bad_feature.as_ref().map_or(0, String::len))?;
    if member.bad_feature.is_none() {
        for section in sections.iter().skip(1) {
            let rel = or_none!(section.rel(endian, bytes));
            let rela = or_none!(section.rela(endian, bytes));
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
    Ok(Some(member))
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
