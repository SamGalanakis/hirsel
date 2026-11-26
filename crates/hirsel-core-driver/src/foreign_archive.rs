//! Validate a sealed archive's own objects before any use as a link input.
use crate::code_admission::AdmissionError;
use crate::code_store::ObjectKind;
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::decode::{Debit, DecodeResult};
use hirsel_core_ir::release_recipe::{ForeignCode, ForeignLeaf, ForeignTarget, ObjectRef};
use hirsel_core_resolve::{ForeignArchiveCapture, ForeignModuleCapture};
use object::read::archive::{ArchiveFile, ArchiveKind};
use object::read::elf::{ElfFile64, FileHeader, SectionHeader, Sym};
use object::{LittleEndian, elf};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const AR: &[u8] = b"!<arch>\n";
const THIN: &[u8] = b"!<thin>\n";
const LLVM_ADDRSIG: elf::SectionType = elf::SectionType(0x6fff_4c03);
const TBD: &[u8] = b"--- !tapi-tbd";

pub(crate) mod contained;
mod macho;

/// A sealed archive or module as the release recipe names it.
pub(crate) fn archive_ref(row: &ForeignArchiveCapture) -> ObjectRef {
    ObjectRef {
        hash: ContentHash(row.pin),
        len: row.bytes.len() as u64,
    }
}

/// A module's code as the release recipe records it: its archives by target,
/// or its one contained module (FFI-10).
pub(crate) fn recipe_code(capture: &ForeignModuleCapture) -> ForeignCode {
    match &capture.code {
        hirsel_core_resolve::ForeignCode::Native(archives) => ForeignCode::Native(
            archives
                .iter()
                .map(|(target, row)| ForeignTarget {
                    target: target.clone(),
                    archive: archive_ref(row),
                })
                .collect(),
        ),
        hirsel_core_resolve::ForeignCode::Wasm(row) => ForeignCode::Wasm(archive_ref(row)),
    }
}

/// FFI-3: a linked module's archive for `target`.
pub(crate) fn select<'a>(
    module: &DefPath,
    capture: &'a ForeignModuleCapture,
    target: &str,
) -> Result<&'a ForeignArchiveCapture, AdmissionError> {
    let archives = capture.archives().ok_or_else(contained::not_built)?;
    archives.get(target).ok_or_else(|| {
        let targets = if archives.is_empty() {
            "none".into()
        } else {
            archives
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        AdmissionError::Refused(format!(
            "FFI-3: {} has no archive for target {target}; its archives are for {targets}",
            module.definition_identity()
        ))
    })
}

pub(crate) struct SealedArchive {
    file: crate::host::SealedFile,
    /// Its members are Mach-O objects, whose symbols carry a leading `_`.
    macho: bool,
    indexed: BTreeMap<Vec<u8>, IndexedSymbol>,
    members: Vec<Member>,
}

/// One sealed input per pin. The module and path are those of the first
/// selecting row and name the archive in collision diagnostics.
/// Beside them, each contained module's precompiled artifact by its pin
/// (FFI-15), which the entry object holds and no linker input does.
#[derive(Default)]
pub(crate) struct LinkArchives(
    BTreeMap<ContentHash, StagedArchive>,
    BTreeMap<ContentHash, Vec<u8>>,
);

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
        self.0.keys().chain(self.1.keys()).copied()
    }

    pub(crate) fn contain(&mut self, pin: ContentHash, artifact: Vec<u8>) {
        self.1.insert(pin, artifact);
    }

    /// The precompiled artifact of the contained module pinned `pin`.
    pub(crate) fn artifact(&self, pin: ContentHash) -> Option<&[u8]> {
        self.1.get(&pin).map(Vec::as_slice)
    }

    pub(crate) fn contains(&self, pin: ContentHash) -> bool {
        self.0.contains_key(&pin) || self.1.contains_key(&pin)
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

    /// Check the actual generated inputs, the files `inputs` and the object
    /// bytes `members` the link bundles, then give LLD only the sealed
    /// descriptors. The owner lives across LLD's invocation.
    pub(crate) fn append_link_inputs(
        &self,
        inputs: &mut Vec<PathBuf>,
        members: &[std::sync::Arc<[u8]>],
        runtime: &Path,
        recipe: &crate::tool_inputs::LinkRecipe,
    ) -> Result<(), AdmissionError> {
        if self.0.is_empty() {
            return Ok(());
        }
        let mut symbols = self.definitions(&Debit::unlimited())?;
        symbols.check_tool_inputs(runtime, recipe)?;
        for input in inputs.iter() {
            symbols.check_link_input(&std::fs::read(input)?, "the generated code")?;
        }
        for member in members {
            symbols.check_link_input(member, "the generated code")?;
        }
        symbols.check_imports(&recipe.target)?;
        for row in self.0.values() {
            inputs.push(row.archive.file.path()?);
        }
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
    /// The global and weak symbols the member leaves undefined, in symbol order.
    imports: Vec<Vec<u8>>,
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

/// What the admitted archives define, and what they import that no archive
/// defines. Symbol names are the object file's own: on Mach-O they carry the
/// platform's leading `_`, which diagnostics leave out.
pub(crate) struct ForeignDefinitions<'a> {
    defined: BTreeMap<&'a [u8], ArchiveOrigin<'a>>,
    /// Each archive's imports that no archive defines, in module, member and
    /// symbol order.
    imports: Vec<(ArchiveOrigin<'a>, &'a str, &'a [u8])>,
    /// The imports no input read so far defines.
    unresolved: BTreeSet<&'a [u8]>,
    macho: bool,
}

/// `symbol` as C names it: Mach-O symbols carry a leading `_`.
fn c_name(symbol: &[u8], macho: bool) -> std::borrow::Cow<'_, str> {
    let symbol = if macho {
        symbol.strip_prefix(b"_").unwrap_or(symbol)
    } else {
        symbol
    };
    String::from_utf8_lossy(symbol)
}

pub(crate) fn check_distinct_definitions<'a>(
    archives: impl IntoIterator<Item = (&'a SealedArchive, ArchiveOrigin<'a>)>,
    budget: &Debit<'_>,
) -> Result<ForeignDefinitions<'a>, AdmissionError> {
    let mut seen = BTreeSet::new();
    let mut distinct = Vec::new();
    let mut definitions = BTreeMap::<&[u8], ArchiveOrigin<'_>>::new();
    for (archive, origin) in archives {
        budget.check()?;
        if !seen.insert(origin.object.hash) {
            continue;
        }
        budget.reserve(&mut distinct, 1)?;
        distinct.push((archive, origin));
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
                            return Err(collision(name, previous, &origin.label(), archive.macho));
                        }
                    }
                }
            }
        }
    }
    let mut imports = Vec::new();
    let mut unresolved = BTreeSet::new();
    for &(archive, origin) in &distinct {
        for member in &archive.members {
            for symbol in &member.imports {
                if definitions.contains_key(symbol.as_slice()) {
                    continue;
                }
                budget.charge_slots(1)?;
                budget.reserve(&mut imports, 1)?;
                imports.push((origin, member.name.as_str(), symbol.as_slice()));
                if unresolved.insert(symbol.as_slice()) {
                    budget.charge_bytes(size_of::<&[u8]>().saturating_mul(2))?;
                }
            }
        }
    }
    Ok(ForeignDefinitions {
        defined: definitions,
        imports,
        unresolved,
        macho: distinct.iter().any(|(archive, _)| archive.macho),
    })
}

fn collision(
    symbol: &[u8],
    archive: ArchiveOrigin<'_>,
    other: &str,
    macho: bool,
) -> AdmissionError {
    AdmissionError::Refused(format!(
        "FFI-3: symbol {} is defined by {} and by {other}",
        c_name(symbol, macho),
        archive.label()
    ))
}

impl ForeignDefinitions<'_> {
    /// Read the runtime and the toolchain's own link inputs: the libc and system
    /// libraries of an ELF recipe's closure, or a Mach-O recipe's text stubs,
    /// which are its files besides the linker.
    pub(crate) fn check_tool_inputs(
        &mut self,
        runtime: &Path,
        recipe: &crate::tool_inputs::LinkRecipe,
    ) -> Result<(), AdmissionError> {
        self.check_link_input(&std::fs::read(runtime)?, "the runtime")?;
        for input in recipe_binaries(recipe, self.macho)? {
            self.check_link_input(
                &std::fs::read(&input)?,
                &format!("link input {}", input.display()),
            )?;
        }
        Ok(())
    }

    /// Refuse a symbol `bytes` defines that an archive also defines, and count
    /// the archives' imports it defines as resolved.
    pub(crate) fn check_link_input(
        &mut self,
        bytes: &[u8],
        label: &str,
    ) -> Result<(), AdmissionError> {
        let unsupported =
            || AdmissionError::Refused(format!("FFI-3: cannot read symbol definitions of {label}"));
        let macho = self.macho;
        let defined = &self.defined;
        let unresolved = &mut self.unresolved;
        input_definitions(bytes, &mut |name| {
            let base = name.split(|byte| *byte == b'@').next().unwrap();
            if let Some(&archive) = defined.get(base) {
                return Err(collision(base, archive, label, macho));
            }
            unresolved.remove(base);
            Ok(())
        })
        .ok_or_else(unsupported)?
    }

    /// Refuse the first import, in module, member and symbol order, that no
    /// archive and no input read since defines.
    pub(crate) fn check_imports(&self, target: &str) -> Result<(), AdmissionError> {
        let Some(&(origin, member, symbol)) = self
            .imports
            .iter()
            .find(|(_, _, symbol)| self.unresolved.contains(symbol))
        else {
            return Ok(());
        };
        Err(refuse(
            &context(origin.object, origin.module, target, origin.path),
            format!(
                ": member {member} imports {}, which no admitted input defines",
                c_name(symbol, self.macho)
            ),
        ))
    }
}

/// The toolchain's own link inputs a recipe names: the libc and system
/// libraries of an ELF recipe's closure, or a Mach-O recipe's text stubs,
/// which are its files besides the linker.
pub(crate) fn recipe_binaries(
    recipe: &crate::tool_inputs::LinkRecipe,
    macho: bool,
) -> std::io::Result<Vec<PathBuf>> {
    Ok(if macho {
        recipe
            .files
            .iter()
            .map(|file| file.path.clone())
            .filter(|path| *path != recipe.linker)
            .collect()
    } else {
        crate::link_closure::closure(&recipe.args, crate::tool_inputs::INPUT_MARKER, true)?.binaries
    })
}

/// Visit each global or weak symbol `bytes` defines: an ELF object or shared
/// object, a Mach-O object, an `ar` archive of objects, or a TBD text stub.
/// `None` when the bytes are none of these.
pub(crate) fn input_definitions(
    bytes: &[u8],
    defined: &mut dyn FnMut(&[u8]) -> Result<(), AdmissionError>,
) -> Option<Result<(), AdmissionError>> {
    if bytes.starts_with(AR) {
        let archive = ArchiveFile::parse(bytes).ok()?;
        for member in archive.members() {
            let data = member.ok()?.data(bytes).ok()?;
            if let Err(error) = input_definitions(data, defined)? {
                return Some(Err(error));
            }
        }
        return Some(Ok(()));
    }
    if bytes.starts_with(TBD) {
        return tbd_definitions(std::str::from_utf8(bytes).ok()?, defined);
    }
    if bytes.starts_with(&macho::MH_MAGIC_64.to_le_bytes()) {
        return macho::definitions(bytes, defined);
    }
    let file = ElfFile64::<LittleEndian>::parse(bytes).ok()?;
    let sections = file.elf_section_table();
    let endian = LittleEndian;
    for (index, section) in sections.enumerate() {
        if !matches!(section.sh_type(endian), elf::SHT_SYMTAB | elf::SHT_DYNSYM) {
            continue;
        }
        let table = sections.symbol_table_by_index(endian, bytes, index).ok()?;
        for symbol in table.iter() {
            if !matches!(
                symbol.st_bind(),
                elf::STB_GLOBAL | elf::STB_WEAK | elf::STB_GNU_UNIQUE
            ) || symbol.st_shndx(endian) == elf::SHN_UNDEF
            {
                continue;
            }
            if let Err(error) = defined(table.symbol_name(endian, symbol).ok()?) {
                return Some(Err(error));
            }
        }
    }
    Some(Ok(()))
}

/// The symbols a TBD stub exports: each `symbols`, `weak-symbols` and
/// `thread-local-symbols` list in its `exports` and `reexports`.
fn tbd_definitions(
    text: &str,
    defined: &mut dyn FnMut(&[u8]) -> Result<(), AdmissionError>,
) -> Option<Result<(), AdmissionError>> {
    // A top-level key starts a line; `undefineds` lists what the stub imports.
    let mut blocks = Vec::new();
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if !line.starts_with([' ', '\t', '-']) && !line.trim().is_empty() {
            if let Some(start) = start.take() {
                blocks.push(&text[start..offset]);
            }
            if line.starts_with("exports:") || line.starts_with("reexports:") {
                start = Some(offset);
            }
        }
        offset += line.len();
    }
    blocks.extend(start.map(|start| &text[start..]));
    for block in blocks {
        if let Err(error) = tbd_block(block, defined)? {
            return Some(Err(error));
        }
    }
    Some(Ok(()))
}

fn tbd_block(
    text: &str,
    defined: &mut dyn FnMut(&[u8]) -> Result<(), AdmissionError>,
) -> Option<Result<(), AdmissionError>> {
    let mut rest = text;
    while let Some(at) = rest.find("symbols:") {
        let key_start = rest[..at]
            .rfind(|c: char| c.is_whitespace())
            .map_or(0, |at| at + 1);
        let key = &rest[key_start..at];
        rest = rest[at + "symbols:".len()..].trim_start();
        if !matches!(key, "" | "weak-" | "thread-local-") {
            continue;
        }
        let list = rest.strip_prefix('[')?;
        let end = list.find(']')?;
        for name in list[..end].split(',') {
            let name = name.trim().trim_matches('\'');
            if !name.is_empty() {
                if let Err(error) = defined(name.as_bytes()) {
                    return Some(Err(error));
                }
            }
        }
        rest = &list[end + 1..];
    }
    Some(Ok(()))
}

fn refuse(context: &str, problem: impl std::fmt::Display) -> AdmissionError {
    AdmissionError::Refused(format!("FFI-3: {context}{problem}"))
}

fn archive_name(archive: ObjectRef, path: Option<&str>) -> String {
    path.map_or_else(|| format!("object {}", archive.hash), str::to_owned)
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
        let mut sealed = seal(&bytes).map_err(|_| seal_error())?;
        let context = context(archive, module, target, path);
        let file = sealed.file_mut();
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
                    archive.hash, observed,
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
            file: sealed,
            macho: is_macho(target),
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
        // Mach-O names a C symbol with a leading `_`.
        let mangle = if self.macho { "_" } else { "" };
        for function in &leaf.functions {
            let symbol = format!("{}{}", leaf.symbols, function.identity.definition.0.name);
            let defines = self
                .indexed
                .get(format!("{mangle}{symbol}").as_bytes())
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
        let prefix = format!("{mangle}{}", leaf.symbols);
        for member in &self.members {
            if let Some(symbol) = member
                .symbols
                .iter()
                .find(|s| s.definition && !s.name.starts_with(prefix.as_bytes()))
            {
                let symbol = c_name(&symbol.name, self.macho);
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

pub(crate) fn seal(bytes: &[u8]) -> std::io::Result<crate::host::SealedFile> {
    let mut sealed = crate::host::SealedFile::new(c"hirsel-foreign")?;
    sealed.file_mut().write_all(bytes)?;
    sealed.seal()?;
    Ok(sealed)
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
    let (machine, unwind, irelative) = match hirsel_core_layout::Target::from_triple(target) {
        Some(hirsel_core_layout::Target::X86_64Linux) => (
            elf::EM_X86_64,
            elf::SHT_X86_64_UNWIND,
            elf::R_X86_64_IRELATIVE,
        ),
        Some(hirsel_core_layout::Target::Aarch64Linux) => (
            elf::EM_AARCH64,
            elf::SHT_AARCH64_ATTRIBUTES,
            elf::R_AARCH64_IRELATIVE,
        ),
        Some(hirsel_core_layout::Target::Aarch64Darwin) => {
            return macho::parse_member(bytes, name, budget);
        }
        None => return Ok(None),
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
        imports: vec![],
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
                let external = matches!(bind, elf::STB_GLOBAL | elf::STB_WEAK);
                if external && symbol.st_shndx(endian) == elf::SHN_UNDEF && !name.is_empty() {
                    budget.charge_bytes(name.len())?;
                    budget.reserve(&mut member.imports, 1)?;
                    member.imports.push(name.clone());
                }
                let definition = external && symbol.st_shndx(endian) != elf::SHN_UNDEF;
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

fn is_macho(target: &str) -> bool {
    use hirsel_core_layout::ObjectFormat;
    hirsel_core_layout::Target::from_triple(target).is_some_and(|target| {
        match target.object_format() {
            ObjectFormat::MachO => true,
            ObjectFormat::Elf => false,
        }
    })
}
