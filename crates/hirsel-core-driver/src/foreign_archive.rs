//! Validate a sealed archive's own objects before any use as a link input.
//! Linux archives hold ELF objects; macOS archives hold Mach-O objects, whose
//! C symbols carry a leading underscore that is dropped from every name here.
use crate::code_admission::AdmissionError;
use crate::code_store::ObjectKind;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::decode::DecodeBudget;
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
    file: Sealed,
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
        bytes: impl FnOnce() -> Result<Vec<u8>, AdmissionError>,
    ) -> Result<(), AdmissionError> {
        let row = match self.0.entry(object.hash) {
            std::collections::btree_map::Entry::Occupied(at) => at.into_mut(),
            std::collections::btree_map::Entry::Vacant(at) => at.insert(StagedArchive {
                archive: SealedArchive::open(bytes()?, object, &leaf.module, target, path)?,
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
        budget: &mut DecodeBudget,
    ) -> Result<ForeignDefinitions<'_>, AdmissionError> {
        budget.charge_bytes(self.0.len().saturating_mul(size_of::<&StagedArchive>()))?;
        let mut rows: Vec<_> = self.0.values().collect();
        rows.sort_unstable_by(|a, b| a.module.cmp(&b.module));
        check_distinct_definitions(
            rows.into_iter().map(|row| {
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
        runtime: &Path,
        recipe: &crate::tool_inputs::LinkRecipe,
    ) -> Result<(), AdmissionError> {
        if self.0.is_empty() {
            return Ok(());
        }
        let symbols = self.definitions(&mut DecodeBudget::unlimited())?;
        symbols.check_tool_inputs(runtime, recipe)?;
        for input in &*inputs {
            symbols.check_link_input(&std::fs::read(input)?, "the generated code")?;
        }
        inputs.extend(self.0.values().map(|row| row.archive.file.link_path()));
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
    budget: &mut DecodeBudget,
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
        // macOS system libraries are text stubs; only the runtime is a binary input.
        if hirsel_core_layout::Target::from_triple(&recipe.target).is_some_and(|t| t.is_darwin()) {
            return Ok(());
        }
        let closure = crate::link_closure::closure(
            &recipe.args,
            crate::tool_inputs::INPUT_MARKER,
            crate::tool_inputs::SUPPORT_LIBS,
            true,
        )?;
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
            if bytes.starts_with(&MACHO_MAGIC) {
                for (name, definition) in macho_symbols(bytes).ok_or_else(unsupported)? {
                    if definition && let Some(&archive) = self.0.get(name.as_slice()) {
                        return Err(collision(&name, archive, label));
                    }
                }
                return Ok(());
            }
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
    ) -> Result<Self, AdmissionError> {
        let seal_error =
            || AdmissionError::Refused("FFI-3: this host cannot seal staged archives".into());
        let mut sealed = seal(&bytes).map_err(|_| seal_error())?;
        let file = &mut sealed.file;
        let context = context(archive, module, target, path);
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(0))?;
        let observed = if length == bytes.len() as u64 {
            file.read_exact(&mut bytes)?;
            ObjectKind::ForeignArchive.hash(&bytes)
        } else {
            ObjectKind::ForeignArchive
                .hash_stream(length, |hash| std::io::copy(file, hash).map(|_| ()))?
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
        let darwin = hirsel_core_layout::Target::from_triple(target).is_some_and(|t| t.is_darwin());
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
            let name = if darwin {
                c_name(entry.name())
            } else {
                entry.name()
            };
            symbols
                .entry(name.to_vec())
                .and_modify(|symbol| symbol.offset = None)
                .or_insert(IndexedSymbol {
                    offset: Some(offset),
                    definition_matches: None,
                });
        }
        let mut members = Vec::with_capacity(objects.len());
        for (offset, name, bytes) in objects {
            let bad_member = || {
                refuse(
                    &context,
                    format!(": member {name} is not a relocatable object for {target}"),
                )
            };
            let member = parse_member(bytes, &name, target).ok_or_else(bad_member)?;
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
            file: sealed,
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

/// The sealed copy of an archive and the path the linker reads it by.
struct Sealed {
    file: File,
    /// macOS has no sealed memory files: a private directory holds a
    /// read-only, user-immutable copy, removed with the archive.
    #[cfg(target_os = "macos")]
    dir: PathBuf,
}

impl Sealed {
    fn link_path(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        return self.dir.join("archive.a");
        #[cfg(target_os = "linux")]
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.file.as_raw_fd()
        ))
    }
}

#[cfg(target_os = "macos")]
impl Drop for Sealed {
    fn drop(&mut self) {
        // SAFETY: a live descriptor and integer flags.
        unsafe { libc::fchflags(self.file.as_raw_fd(), 0) };
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(target_os = "linux")]
fn seal(bytes: &[u8]) -> std::io::Result<Sealed> {
    use std::os::fd::FromRawFd;
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
    Ok(Sealed { file })
}

#[cfg(target_os = "macos")]
fn seal(bytes: &[u8]) -> std::io::Result<Sealed> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = tempfile::Builder::new()
        .prefix("hirsel-foreign-")
        .tempdir()?
        .keep();
    let sealed = (|| {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let path = dir.join("archive.a");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        // SAFETY: a live descriptor and integer flags.
        if unsafe { libc::fchflags(file.as_raw_fd(), libc::UF_IMMUTABLE) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
    })();
    match sealed {
        Ok(file) => Ok(Sealed { file, dir }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(error)
        }
    }
}

/// Mach-O constants by value, from `<mach-o/loader.h>` and `<mach-o/nlist.h>`.
mod macho_raw {
    pub const MH_OBJECT: u32 = 0x1;
    pub const CPU_TYPE_ARM64: u32 = 0x0100_000c;
    pub const N_STAB: u8 = 0xe0;
    pub const N_PEXT: u8 = 0x10;
    pub const N_TYPE: u8 = 0x0e;
    pub const N_EXT: u8 = 0x01;
    pub const N_UNDF: u8 = 0x0;
    pub const N_SECT: u8 = 0xe;
    pub const N_WEAK_DEF: u16 = 0x0080;
    pub const N_SYMBOL_RESOLVER: u16 = 0x0100;
    pub const SECTION_TYPE: u32 = 0xff;
    pub const S_REGULAR: u32 = 0x0;
    pub const S_ZEROFILL: u32 = 0x1;
    pub const S_CSTRING_LITERALS: u32 = 0x2;
    pub const S_4BYTE_LITERALS: u32 = 0x3;
    pub const S_8BYTE_LITERALS: u32 = 0x4;
    pub const S_LITERAL_POINTERS: u32 = 0x5;
    pub const S_COALESCED: u32 = 0xb;
    pub const S_GB_ZEROFILL: u32 = 0xc;
    pub const S_16BYTE_LITERALS: u32 = 0xe;
    pub const S_THREAD_LOCAL_REGULAR: u32 = 0x11;
    pub const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
    pub const S_THREAD_LOCAL_VARIABLES: u32 = 0x13;
    pub const S_THREAD_LOCAL_VARIABLE_POINTERS: u32 = 0x14;
    pub const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
    pub const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x0000_0400;
}

const MACHO_MAGIC: [u8; 4] = 0xfeed_facf_u32.to_le_bytes();

/// A Mach-O C symbol name without its leading underscore.
fn c_name(name: &[u8]) -> &[u8] {
    name.strip_prefix(b"_").unwrap_or(name)
}

/// Each external symbol of a 64-bit Mach-O object, and whether it defines it.
fn macho_symbols(bytes: &[u8]) -> Option<Vec<(Vec<u8>, bool)>> {
    use object::read::macho::{MachOFile64, Nlist};
    let file = MachOFile64::<LittleEndian>::parse(bytes).ok()?;
    let table = file.macho_symbol_table();
    let strings = table.strings();
    let mut symbols = Vec::new();
    for symbol in table.iter() {
        let kind = symbol.n_type().0;
        if kind & macho_raw::N_STAB != 0 || kind & macho_raw::N_EXT == 0 {
            continue;
        }
        let name = symbol.name(LittleEndian, strings).ok()?;
        let defined = match kind & macho_raw::N_TYPE {
            macho_raw::N_UNDF => symbol.n_value(LittleEndian) != 0,
            _ => true,
        };
        symbols.push((c_name(name).to_vec(), defined));
    }
    Some(symbols)
}

/// A Mach-O relocatable object for arm64 macOS, held to the same features as
/// an ELF member: no constructors, destructors, interposing or resolver
/// functions, and ABI text in `__TEXT,__hirsel_abi`.
fn parse_macho_member(bytes: &[u8], name: &str) -> Option<Member> {
    use object::read::macho::{MachHeader, MachOFile64, Nlist, Section as _};
    let endian = LittleEndian;
    let file = MachOFile64::<LittleEndian>::parse(bytes).ok()?;
    let header = file.macho_header();
    if header.filetype(endian).0 as u32 != macho_raw::MH_OBJECT
        || header.cputype(endian).0 as u32 != macho_raw::CPU_TYPE_ARM64
    {
        return None;
    }
    let mut member = Member {
        name: name.into(),
        symbols: vec![],
        abi: vec![],
        bad_feature: None,
    };
    // Section kinds an ordinary C object holds; anything else is refused.
    let admitted = |kind: u32| {
        matches!(
            kind,
            macho_raw::S_REGULAR
                | macho_raw::S_ZEROFILL
                | macho_raw::S_CSTRING_LITERALS
                | macho_raw::S_4BYTE_LITERALS
                | macho_raw::S_8BYTE_LITERALS
                | macho_raw::S_16BYTE_LITERALS
                | macho_raw::S_LITERAL_POINTERS
                | macho_raw::S_COALESCED
                | macho_raw::S_GB_ZEROFILL
                | macho_raw::S_THREAD_LOCAL_REGULAR
                | macho_raw::S_THREAD_LOCAL_ZEROFILL
                | macho_raw::S_THREAD_LOCAL_VARIABLES
                | macho_raw::S_THREAD_LOCAL_VARIABLE_POINTERS
        )
    };
    let mut code = Vec::new();
    for section in object::Object::sections(&file) {
        let section = section.macho_section();
        let flags = section.flags(endian).0;
        let kind = flags & macho_raw::SECTION_TYPE;
        let segment = section.segment_name();
        let sectname = section.name();
        let label = || {
            format!(
                "section {},{} of type {kind:#x}",
                String::from_utf8_lossy(trim(segment)),
                String::from_utf8_lossy(trim(sectname))
            )
        };
        if !admitted(kind) {
            member.bad_feature.get_or_insert_with(label);
        }
        if trim(segment) == b"__TEXT" && trim(sectname) == b"__hirsel_abi" {
            // Only a regular section can supply ABI text.
            member.abi.push(
                (kind == macho_raw::S_REGULAR)
                    .then(|| {
                        section
                            .data(endian, bytes, u64::from(section.offset(endian)))
                            .ok()
                            .map(<[u8]>::to_vec)
                    })
                    .flatten(),
            );
        }
        code.push(
            flags & (macho_raw::S_ATTR_PURE_INSTRUCTIONS | macho_raw::S_ATTR_SOME_INSTRUCTIONS)
                != 0,
        );
    }
    let table = file.macho_symbol_table();
    let strings = table.strings();
    let mut first_bad_symbol = None;
    for symbol in table.iter() {
        let kind = symbol.n_type().0;
        if kind & macho_raw::N_STAB != 0 || kind & macho_raw::N_EXT == 0 {
            continue;
        }
        let raw = symbol.name(endian, strings).ok()?;
        let name = c_name(raw).to_vec();
        let desc = symbol.n_desc(endian).0;
        if desc & macho_raw::N_SYMBOL_RESOLVER != 0 && first_bad_symbol.is_none() {
            first_bad_symbol = Some(format!(
                "resolver function {}",
                String::from_utf8_lossy(&name)
            ));
        }
        let section = (kind & macho_raw::N_TYPE == macho_raw::N_SECT)
            .then(|| usize::from(symbol.n_sect()).checked_sub(1))
            .flatten();
        let definition = match kind & macho_raw::N_TYPE {
            macho_raw::N_UNDF => symbol.n_value(endian) != 0,
            _ => true,
        };
        let strong_function = kind & macho_raw::N_PEXT == 0
            && desc & macho_raw::N_WEAK_DEF == 0
            && section.is_some_and(|index| code.get(index) == Some(&true));
        member.symbols.push(Symbol {
            name,
            definition,
            strong_function,
        });
    }
    member.bad_feature = member.bad_feature.or(first_bad_symbol);
    Some(member)
}

/// A fixed-width Mach-O name without its NUL padding.
fn trim(name: &[u8]) -> &[u8] {
    name.split(|&b| b == 0).next().unwrap_or(name)
}

fn parse_member(bytes: &[u8], name: &str, target: &str) -> Option<Member> {
    let (machine, unwind, irelative) = match target {
        "aarch64-apple-darwin" => return parse_macho_member(bytes, name),
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
