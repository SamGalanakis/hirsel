use crate::package_dir::{module_path, path_rule};
use crate::package_manifest::name::{self as names, Version};
use crate::product::bundled::TOOLCHAIN;
use bytes::Bytes;
use hirsel_core_ids::{ContentHash, PackageId};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

pub(crate) const CONTENT_LIMIT: usize = 64 << 20;
pub(crate) const ARCHIVE_LIMIT_MIB: usize = 512;
pub(crate) const ARCHIVE_LIMIT: usize = ARCHIVE_LIMIT_MIB << 20;
const FILE_LIMIT: usize = 65_536;
const PACKAGE_LIMIT: usize = 4096;
mod archive;
const CHARGE: &str = "charge-v1";

pub(crate) fn toolchain_major(name: &str) -> Option<u32> {
    TOOLCHAIN
        .iter()
        .find(|((supplied, _), _)| *supplied == name)
        .map(|((_, major), _)| *major)
}

/// The bindings of a toolchain package: each to another toolchain package. None for an
/// identity the toolchain does not supply.
pub(crate) fn toolchain_bindings(id: &Id) -> Option<BTreeMap<Id, Option<Root>>> {
    let (_, bound) = TOOLCHAIN
        .iter()
        .find(|(package, _)| *package == (id.name.as_str(), id.major))?;
    Some(
        bound
            .iter()
            .map(|&(name, major)| {
                (
                    Id {
                        name: name.into(),
                        major,
                    },
                    None,
                )
            })
            .collect(),
    )
}

/// The toolchain packages, other than std, that `bound` reaches through toolchain bindings,
/// `bound` included.
pub(crate) fn toolchain_closure<'a>(bound: impl IntoIterator<Item = &'a Id>) -> BTreeSet<Id> {
    let mut closure = BTreeSet::new();
    let mut pending: Vec<Id> = bound.into_iter().cloned().collect();
    while let Some(id) = pending.pop() {
        if id.name == "std" || !closure.insert(id.clone()) {
            continue;
        }
        if let Some(bindings) = toolchain_bindings(&id) {
            pending.extend(bindings.into_keys());
        }
    }
    closure
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Id {
    pub(crate) name: String,
    pub(crate) major: u32,
}

impl Id {
    pub(crate) fn compiler_id(&self) -> PackageId {
        PackageId {
            name: self.name.clone(),
            version: self.major.to_string(),
        }
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.major)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(crate) struct Root(pub(crate) [u8; 32]);

impl Root {
    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self(ContentHash::of_parts("hirsel.library-root.v1", [bytes]).0)
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(Self(parse_digest(text.strip_prefix("sha256:")?)?))
    }
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", ContentHash(self.0))
    }
}

pub(crate) fn parse_digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || text.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    hirsel_core_ids::hex::decode(text)?.try_into().ok()
}

/// Canonical library content's encoding version (packages section 1). Version 3
/// adds the package's version and binds each dependency by alias, exactly or by floor.
const CONTENT_VERSION: u64 = 3;

/// How a binding of `C` binds its dependency (packages section 1, item 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    /// Tag 0: the toolchain supplies the package.
    Toolchain,
    /// Tag 1: exactly this root.
    Exact(Root),
    /// Tag 2: this tested root, or a root of the identity at least `MAJOR.minor.patch`.
    Floor {
        tested: Root,
        minor: u32,
        patch: u32,
    },
}

impl Binding {
    /// The root the binding was checked against; none for a toolchain package.
    pub(crate) fn root(&self) -> Option<Root> {
        match *self {
            Self::Toolchain => None,
            Self::Exact(root) | Self::Floor { tested: root, .. } => Some(root),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Source {
    pub(crate) module: Vec<String>,
    pub(crate) bytes: Arc<[u8]>,
}

pub(crate) use hirsel_core_resolve::{
    ForeignArchiveCapture as ArchiveRow, ForeignCode, ForeignModuleCapture as ForeignModule,
};

pub(crate) fn archive_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[derive(Default)]
pub(crate) struct Sha256Writer(Sha256);

impl Sha256Writer {
    pub(crate) fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

impl std::io::Write for Sha256Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Content {
    pub(crate) id: Id,
    pub(crate) version: Version,
    /// Each binding by its alias.
    pub(crate) bindings: BTreeMap<String, (Id, Binding)>,
    pub(crate) files: BTreeMap<String, Source>,
    pub(crate) foreign: BTreeMap<Vec<String>, ForeignModule>,
}

pub(crate) struct ContentInventory<'a>(&'a Content);

impl Content {
    /// The identities the bindings bind, each with the root it was checked against. Two
    /// aliases of one identity are one edge.
    pub(crate) fn edges(&self) -> BTreeMap<Id, Option<Root>> {
        self.bindings
            .values()
            .map(|(id, binding)| (id.clone(), binding.root()))
            .collect()
    }

    pub(crate) fn inventory(&self) -> Result<ContentInventory<'_>, String> {
        let mut count = source_count(self.files.len())?;
        for module in self.foreign.values() {
            count = add_files(count, module.archives().map_or(1, BTreeMap::len))?;
        }
        Ok(ContentInventory(self))
    }

    pub(crate) fn foreign_stamp(&self) -> ContentHash {
        ContentHash::of_parts_stream("hirsel.foreign-stamp.v1", |hash| {
            let mut part =
                |bytes: &[u8]| hash.part_with(bytes.len() as u64, |writer| writer.write_all(bytes));
            part(&(self.foreign.len() as u64).to_le_bytes())?;
            for (module, foreign) in &self.foreign {
                part(&(module.len() as u64).to_le_bytes())?;
                for component in module {
                    part(component.as_bytes())?;
                }
                part(foreign.symbols.as_bytes())?;
                match &foreign.code {
                    ForeignCode::Native(archives) => {
                        part(&[0])?;
                        part(&(archives.len() as u64).to_le_bytes())?;
                        for (target, row) in archives {
                            part(target.as_bytes())?;
                            part(&row.pin)?;
                            part(&(row.bytes.len() as u64).to_le_bytes())?;
                            part(&archive_digest(&row.bytes))?;
                        }
                    }
                    ForeignCode::Wasm(row) => {
                        part(&[1])?;
                        part(&row.pin)?;
                        part(&(row.bytes.len() as u64).to_le_bytes())?;
                        part(&archive_digest(&row.bytes))?;
                    }
                }
            }
            Ok(())
        })
        .unwrap()
    }

    /// Decodes a published `C` (packages section 1), whose own name is scoped.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        Self::decode_as(bytes, false)
    }

    /// Decodes `C`, admitting a toolchain package's own bare name when `toolchain` is set:
    /// the toolchain's installed archives and `hirsel pack` of a toolchain package's
    /// manifest, which section 2 lets carry a bare name.
    pub(crate) fn decode_as(bytes: &[u8], toolchain: bool) -> Result<Self, String> {
        let mut input = Reader::new(bytes, CONTENT_LIMIT)?;
        let version = input.number(u64::MAX)?;
        if version != CONTENT_VERSION {
            return Err(format!("unknown version {version}"));
        }
        let name = input.text()?.to_owned();
        if !(names::scoped(&name) || toolchain && toolchain_major(&name).is_some()) {
            return Err(format!("bad package name {name:?}"));
        }
        let major = input.number(u32::MAX as u64)? as u32;
        let minor = input.number(u32::MAX as u64)? as u32;
        let patch = input.number(u32::MAX as u64)? as u32;
        let pre = input.text()?.to_owned();
        let version = Version {
            major,
            minor,
            patch,
            pre,
        };
        if !version.pre.is_empty() && !names::prerelease(&version.pre) {
            return Err(format!("bad version {:?}", version.to_string()));
        }
        let id = Id { name, major };
        let charge = input.text()?;
        if charge != CHARGE {
            return Err(format!("unknown charge policy {charge:?}"));
        }
        let mut bindings: BTreeMap<String, (Id, Binding)> = BTreeMap::new();
        let mut identities: BTreeMap<Id, Binding> = BTreeMap::new();
        for _ in 0..input.count()? {
            let alias = input.text()?.to_owned();
            if !names::alias(&alias) {
                return Err(format!("bad alias {alias:?}"));
            }
            ordered(
                bindings.last_key_value().map(|(key, _)| key),
                &alias,
                "bindings",
                &alias,
            )?;
            let name = input.text()?.to_owned();
            if name != "core" && !names::scoped(&name) && toolchain_major(&name).is_none() {
                return Err(format!("bad package name {name:?}"));
            }
            let bound = Id {
                name,
                major: input.number(u32::MAX as u64)? as u32,
            };
            let refused = || format!("bad binding {alias} = {bound}");
            let bare = !names::scoped(&bound.name);
            if bound.name == "core" || bound == id {
                return Err(refused());
            }
            let binding = match input.number(u64::MAX)? {
                0 if toolchain_major(&bound.name) == Some(bound.major) => Binding::Toolchain,
                1 if !bare => Binding::Exact(Root(input.digest()?)),
                2 if !bare => Binding::Floor {
                    tested: Root(input.digest()?),
                    minor: input.number(u32::MAX as u64)? as u32,
                    patch: input.number(u32::MAX as u64)? as u32,
                },
                _ => return Err(refused()),
            };
            if identities
                .insert(bound.clone(), binding)
                .is_some_and(|earlier| earlier != binding)
            {
                return Err(format!("bindings of {bound} differ"));
            }
            bindings.insert(alias, (bound, binding));
        }
        let mut files = BTreeMap::new();
        let mut paths = PathTree::default();
        let mut modules = BTreeMap::new();
        let mut count = source_count(input.count()?)?;
        for _ in 0..count {
            let path = input.text()?.to_owned();
            ordered(
                files.last_key_value().map(|(key, _)| key),
                &path,
                "files",
                &path,
            )?;
            admit_path(&mut paths, &path, ".hirsel")?;
            let module = input.module()?;
            *modules.entry(module.clone()).or_insert(0usize) += 1;
            let bytes = input.frame()?.into();
            files.insert(path, Source { module, bytes });
        }
        let mut foreign = BTreeMap::new();
        let mut prefixes = BTreeSet::new();
        for _ in 0..input.count()? {
            let module = input.module()?;
            let name = module.join(".");
            ordered(
                foreign.last_key_value().map(|(key, _)| key),
                &module,
                "foreign modules",
                &name,
            )?;
            let files = modules.get(&module).copied().unwrap_or(0);
            if files != 1 {
                return Err(format!("foreign module {name} is formed by {files} files"));
            }
            let symbols = input.text()?.to_owned();
            if !symbol_prefix(&symbols) || !prefixes.insert(symbols.clone()) {
                return Err(format!("bad symbol prefix {symbols:?} of {name}"));
            }
            let code = match input.number(1)? {
                0 => {
                    let rows = input.count()?;
                    count = add_files(count, rows)?;
                    let mut archives = BTreeMap::new();
                    for _ in 0..rows {
                        let target = input.text()?.to_owned();
                        ordered(
                            archives.last_key_value().map(|(key, _)| key),
                            &target,
                            "archive rows",
                            &target,
                        )?;
                        if !target_name(&target) {
                            return Err(format!("bad target {target:?} of {name}"));
                        }
                        let path = input.text()?.to_owned();
                        admit_path(&mut paths, &path, ".a")?;
                        let pin = input.digest()?;
                        let bytes = input.frame()?.into();
                        let row = ArchiveRow { path, pin, bytes };
                        if archive_digest(&row.bytes) != pin {
                            return Err(format!(
                                "archive {:?} of {name} for {target} does not match its pin",
                                row.path
                            ));
                        }
                        archives.insert(target, row);
                    }
                    ForeignCode::Native(archives)
                }
                _ => {
                    count = add_files(count, 1)?;
                    let path = input.text()?.to_owned();
                    admit_path(&mut paths, &path, ".wasm")?;
                    let pin = input.digest()?;
                    let bytes = input.frame()?.into();
                    let row = ArchiveRow { path, pin, bytes };
                    if archive_digest(&row.bytes) != pin {
                        return Err(format!(
                            "module {:?} of {name} does not match its pin",
                            row.path
                        ));
                    }
                    ForeignCode::Wasm(row)
                }
            };
            foreign.insert(module, ForeignModule { symbols, code });
        }
        input.end()?;
        Ok(Self {
            id,
            version,
            bindings,
            files,
            foreign,
        })
    }
}

impl ContentInventory<'_> {
    pub(crate) fn encode(self) -> Result<(Root, Vec<u8>), String> {
        let content = self.0;
        let mut out = Writer::new(CONTENT_LIMIT);
        out.number(CONTENT_VERSION as usize);
        out.text(&content.id.name);
        out.number(content.version.major as usize);
        out.number(content.version.minor as usize);
        out.number(content.version.patch as usize);
        out.text(&content.version.pre);
        out.text(CHARGE);
        out.number(content.bindings.len());
        for (alias, (bound, binding)) in &content.bindings {
            out.text(alias);
            out.text(&bound.name);
            out.number(bound.major as usize);
            match binding {
                Binding::Toolchain => out.number(0),
                Binding::Exact(root) => {
                    out.number(1);
                    out.raw(&root.0);
                }
                Binding::Floor {
                    tested,
                    minor,
                    patch,
                } => {
                    out.number(2);
                    out.raw(&tested.0);
                    out.number(*minor as usize);
                    out.number(*patch as usize);
                }
            }
        }
        out.number(content.files.len());
        for (path, file) in &content.files {
            out.text(path);
            out.module(&file.module);
            out.frame(&file.bytes);
        }
        out.number(content.foreign.len());
        for (module, foreign) in &content.foreign {
            out.module(module);
            out.text(&foreign.symbols);
            match &foreign.code {
                ForeignCode::Native(archives) => {
                    out.number(0);
                    out.number(archives.len());
                    for (target, row) in archives {
                        out.text(target);
                        out.text(&row.path);
                        out.raw(&row.pin);
                        out.frame(&row.bytes);
                    }
                }
                ForeignCode::Wasm(row) => {
                    out.number(1);
                    out.text(&row.path);
                    out.raw(&row.pin);
                    out.frame(&row.bytes);
                }
            }
        }
        let encoded = out.finish()?;
        Ok((Root::of(&encoded), encoded))
    }
}

fn ordered<T: Ord>(
    previous: Option<&T>,
    current: &T,
    part: &str,
    item: &dyn fmt::Display,
) -> Result<(), String> {
    if previous.is_some_and(|last| last >= current) {
        return Err(format!("{part} out of order at {item}"));
    }
    Ok(())
}

pub(crate) fn symbol_prefix(text: &str) -> bool {
    let mut bytes = text.bytes();
    text.len() <= 32
        && bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(crate) fn target_name(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

fn source_count(files: usize) -> Result<usize, String> {
    if files == 0 {
        return Err("no files".into());
    }
    add_files(0, files)
}

fn add_files(count: usize, added: usize) -> Result<usize, String> {
    count
        .checked_add(added)
        .filter(|&total| total <= FILE_LIMIT)
        .ok_or_else(|| format!("over the limit of {FILE_LIMIT} files and archive rows"))
}

#[derive(Default)]
pub(crate) struct PathTree {
    // Each key is a folded component prefix. The first complete path that
    // introduced it supplies the spelling for any diagnostic.
    prefixes: BTreeMap<String, (String, String, bool)>,
}

impl PathTree {
    pub(crate) fn insert(&mut self, path: &str) -> Result<(), String> {
        let mut prefix = String::new();
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(&component.to_ascii_lowercase());
            let file = components.peek().is_none();
            if let Some((spelling, first, prior_file)) = self.prefixes.get(&prefix) {
                match (*prior_file, file) {
                    (true, true) if path == first => {
                        return Err(format!("{path:?} is named twice"));
                    }
                    (true, true) => {
                        return Err(format!("{first:?} and {path:?} differ only in case"));
                    }
                    (true, false) => {
                        return Err(format!(
                            "{first:?} is a file and also a directory of {path:?}"
                        ));
                    }
                    (false, true) => {
                        return Err(format!(
                            "{path:?} is a file and also a directory of {first:?}"
                        ));
                    }
                    (false, false) if spelling != component => {
                        return Err(format!(
                            "{first:?} and {path:?} spell directory {spelling:?} differently"
                        ));
                    }
                    _ => {}
                }
            } else {
                self.prefixes
                    .insert(prefix.clone(), (component.into(), path.into(), file));
            }
        }
        Ok(())
    }
}

fn admit_path(paths: &mut PathTree, path: &str, suffix: &str) -> Result<(), String> {
    path_rule(path, suffix)
        .and_then(|()| paths.insert(path))
        .map_err(|rule| format!("bad path {path:?}: {rule}"))
}

struct Writer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl Writer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.exceeded |= bytes.len() > self.limit - self.bytes.len();
        if !self.exceeded {
            self.bytes.extend_from_slice(bytes);
        }
    }

    fn number(&mut self, mut number: usize) {
        while number >= 128 {
            self.raw(&[(number as u8 & 127) | 128]);
            number >>= 7;
        }
        self.raw(&[number as u8]);
    }

    fn frame(&mut self, bytes: &[u8]) {
        self.number(bytes.len());
        self.raw(bytes);
    }

    fn text(&mut self, text: &str) {
        self.frame(text.as_bytes());
    }

    fn module(&mut self, parts: &[String]) {
        self.number(parts.len());
        for part in parts {
            self.text(part);
        }
    }

    fn finish(self) -> Result<Vec<u8>, String> {
        if self.exceeded {
            Err(format!("over the limit of {} bytes", self.limit))
        } else {
            Ok(self.bytes)
        }
    }
}

fn read_number(
    start: usize,
    max: u64,
    mut byte: impl FnMut() -> Result<u8, String>,
) -> Result<u64, String> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = byte()?;
        if shift == 63 && byte > 1 {
            break;
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            if (shift != 0 && byte == 0) || value > max {
                break;
            }
            return Ok(value);
        }
    }
    Err(format!("bad number at byte {start}"))
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], limit: usize) -> Result<Self, String> {
        if data.len() > limit {
            return Err(format!("over the limit of {limit} bytes"));
        }
        Ok(Self { data, offset: 0 })
    }

    fn raw(&mut self, len: usize) -> Result<&'a [u8], String> {
        if len > self.data.len() - self.offset {
            return Err(format!("truncated at {}", self.offset));
        }
        let start = self.offset;
        self.offset += len;
        Ok(&self.data[start..self.offset])
    }

    fn number(&mut self, max: u64) -> Result<u64, String> {
        read_number(self.offset, max, || Ok(self.raw(1)?[0]))
    }

    fn count(&mut self) -> Result<usize, String> {
        Ok(self.number(usize::MAX as u64)? as usize)
    }

    fn frame(&mut self) -> Result<&'a [u8], String> {
        let len = self.count()?;
        self.raw(len)
    }

    fn text(&mut self) -> Result<&'a str, String> {
        let len = self.count()?;
        let start = self.offset;
        let bytes = self.raw(len)?;
        std::str::from_utf8(bytes).map_err(|_| format!("bad text at byte {start}"))
    }

    fn module(&mut self) -> Result<Vec<String>, String> {
        let mut parts = Vec::new();
        for _ in 0..self.count()? {
            parts.push(self.text()?.to_owned());
        }
        let name = parts.join(".");
        if module_path(&name).as_ref() != Some(&parts) {
            return Err(format!("bad path {name:?}: invalid module path"));
        }
        Ok(parts)
    }

    fn digest(&mut self) -> Result<[u8; 32], String> {
        Ok(self.raw(32)?.try_into().unwrap())
    }

    fn end(&self) -> Result<(), String> {
        if self.offset != self.data.len() {
            return Err(format!("trailing bytes at {}", self.offset));
        }
        Ok(())
    }
}

pub(crate) struct Library {
    pub(crate) content: Content,
    pub(crate) encoded: Bytes,
}

pub(crate) struct PackageArchive {
    pub(crate) head: Root,
    pub(crate) entries: BTreeMap<Root, Library>,
}

impl PackageArchive {
    pub(crate) fn verify(&self, expected: &Id, pin: Root, source: &str) -> Result<(), String> {
        if self.head != pin {
            return Err(format!(
                "{expected}: {source} holds {}, but the binding pins {pin}",
                self.head
            ));
        }
        check_identity(expected, pin, &self.entries[&self.head].content.id)?;
        let mut reached = BTreeSet::new();
        let mut queued = vec![self.head];
        let mut missing = BTreeSet::new();
        let mut wrong = BTreeSet::new();
        while let Some(root) = queued.pop() {
            if !reached.insert(root) {
                continue;
            }
            let binder = &self.entries[&root].content;
            for (id, binding) in binder.bindings.values() {
                let Some(root) = binding.root() else {
                    continue;
                };
                let root = &root;
                match self.entries.get(root) {
                    Some(entry) if entry.content.id == *id => queued.push(*root),
                    Some(entry) => {
                        wrong.insert((&binder.id, id, *root, &entry.content.id));
                    }
                    None => {
                        missing.insert((&binder.id, id, *root));
                    }
                }
            }
        }
        if let Some((binder, id, root)) = missing.first() {
            return Err(format!(
                "{expected}: {source} lacks {root}, which {binder} binds as {id}"
            ));
        }
        if let Some((_, id, root, found)) = wrong.first() {
            return check_identity(id, *root, found);
        }
        if let Some(extra) = self.entries.keys().find(|root| !reached.contains(root)) {
            return Err(format!(
                "{expected}: {source} holds {extra}, which nothing in it binds"
            ));
        }
        Ok(())
    }
}

pub(crate) fn check_identity(expected: &Id, root: Root, found: &Id) -> Result<(), String> {
    if expected != found {
        return Err(format!("{expected}: {root} is {found}"));
    }
    Ok(())
}
