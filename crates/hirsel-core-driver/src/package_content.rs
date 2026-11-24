use crate::package_dir::{module_path, package_name, path_rule};
use hirsel_core_ids::{ContentHash, PackageId};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

pub(crate) const CONTENT_LIMIT: usize = 64 << 20;
pub(crate) const ARCHIVE_LIMIT: usize = 512 << 20;
const FILE_LIMIT: usize = 65_536;
const PACKAGE_LIMIT: usize = 4096;
const ARCHIVE_TAG: &str = "hirsel.package-archive.v1";
const CHARGE: &str = "charge-v1";

/// The packages the installed toolchain supplies, each with the toolchain packages it binds.
/// `std@1` is the toolchain's std image; the others are package archives in
/// `core-packages/<name>-<major>.hpkg`.
const TOOLCHAIN: &[(&str, u32, &[&str])] = &[
    ("std", 1, &[]),
    ("store", 1, &["std"]),
    ("runner", 1, &["std", "store"]),
];

pub(crate) fn toolchain_major(name: &str) -> Option<u32> {
    TOOLCHAIN
        .iter()
        .find(|(supplied, _, _)| *supplied == name)
        .map(|(_, major, _)| *major)
}

/// The bindings of a toolchain package: each to another toolchain package. None for an
/// identity the toolchain does not supply.
pub(crate) fn toolchain_bindings(id: &Id) -> Option<BTreeMap<Id, Option<Root>>> {
    let (_, _, bound) = TOOLCHAIN
        .iter()
        .find(|(name, major, _)| *name == id.name && *major == id.major)?;
    Some(
        bound
            .iter()
            .map(|name| {
                (
                    Id {
                        name: (*name).into(),
                        major: toolchain_major(name).expect("a toolchain binding is supplied"),
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

    pub(crate) fn hex(self) -> String {
        hex(&self.0)
    }
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

pub(crate) fn parse_digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        *slot = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Some(bytes)
}

pub(crate) fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for byte in digest {
        write!(out, "{byte:02x}").unwrap();
    }
    out
}

#[derive(Clone)]
pub(crate) struct Source {
    pub(crate) module: Vec<String>,
    pub(crate) bytes: Arc<[u8]>,
}

pub(crate) use hirsel_core_resolve::{
    ForeignArchiveCapture as ArchiveRow, ForeignModuleCapture as ForeignModule,
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
    pub(crate) bindings: BTreeMap<Id, Option<Root>>,
    pub(crate) files: BTreeMap<String, Source>,
    pub(crate) foreign: BTreeMap<Vec<String>, ForeignModule>,
}

pub(crate) struct ContentInventory<'a>(&'a Content);

impl Content {
    pub(crate) fn inventory(&self) -> Result<ContentInventory<'_>, String> {
        let mut count = source_count(self.files.len())?;
        for module in self.foreign.values() {
            count = add_files(count, module.archives.len())?;
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
                part(&(foreign.archives.len() as u64).to_le_bytes())?;
                for (target, row) in &foreign.archives {
                    part(target.as_bytes())?;
                    part(&row.pin)?;
                    part(&(row.bytes.len() as u64).to_le_bytes())?;
                    part(&archive_digest(&row.bytes))?;
                }
            }
            Ok(())
        })
        .unwrap()
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut input = Reader::new(bytes, CONTENT_LIMIT)?;
        let version = input.number(u64::MAX)?;
        if version != 1 {
            return Err(format!("unknown version {version}"));
        }
        let id = input.id()?;
        if matches!(id.name.as_str(), "core" | "std") {
            return Err(format!("bad package name {:?}", id.name));
        }
        let charge = input.text()?;
        if charge != CHARGE {
            return Err(format!("unknown charge policy {charge:?}"));
        }
        let mut bindings = BTreeMap::new();
        let mut names = BTreeSet::new();
        for _ in 0..input.count()? {
            let binding = input.id()?;
            let tag = input.number(u64::MAX)?;
            let root = match tag {
                0 if toolchain_major(&binding.name) == Some(binding.major) => None,
                1 if !matches!(binding.name.as_str(), "core" | "std") => {
                    Some(Root(input.digest()?))
                }
                _ => return Err(format!("bad binding {binding}")),
            };
            if !names.insert(binding.name.clone()) {
                return Err(format!("two bindings of {}", binding.name));
            }
            ordered(
                bindings.last_key_value().map(|(key, _)| key),
                &binding,
                "bindings",
                &binding,
            )?;
            bindings.insert(binding, root);
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
            foreign.insert(module, ForeignModule { symbols, archives });
        }
        input.end()?;
        Ok(Self {
            id,
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
        out.number(1);
        out.text(&content.id.name);
        out.number(content.id.major as usize);
        out.text(CHARGE);
        out.number(content.bindings.len());
        for (binding, root) in &content.bindings {
            out.text(&binding.name);
            out.number(binding.major as usize);
            match root {
                None => out.number(0),
                Some(root) => {
                    out.number(1);
                    out.raw(&root.0);
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
            out.number(foreign.archives.len());
            for (target, row) in &foreign.archives {
                out.text(target);
                out.text(&row.path);
                out.raw(&row.pin);
                out.frame(&row.bytes);
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
        let start = self.offset;
        let mut value = 0u64;
        for shift in (0..=63).step_by(7) {
            let byte = self.raw(1)?[0];
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

    fn id(&mut self) -> Result<Id, String> {
        let name = self.text()?.to_owned();
        if !package_name(&name) {
            return Err(format!("bad package name {name:?}"));
        }
        let major = self.number(u32::MAX as u64)? as u32;
        Ok(Id { name, major })
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
    pub(crate) encoded: Arc<[u8]>,
}

pub(crate) struct PackageArchive {
    pub(crate) head: Root,
    pub(crate) entries: BTreeMap<Root, Library>,
}

impl PackageArchive {
    pub(crate) fn open(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;

        let read = || -> Result<Self, Box<dyn std::error::Error>> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            if !file.metadata()?.is_file() {
                return Err("expected a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take(ARCHIVE_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)?;
            let archive = Self::decode(&bytes)?;
            let id = &archive.entries[&archive.head].content.id;
            archive.verify(id, archive.head, "archive")?;
            Ok(archive)
        };
        read().map_err(|error| format!("{}: {error}", path.display()).into())
    }

    pub(crate) fn encode(head: Root, entries: &BTreeMap<Root, Library>) -> Result<Vec<u8>, String> {
        if entries.len() > PACKAGE_LIMIT {
            return Err(format!("over the limit of {PACKAGE_LIMIT} packages"));
        }
        let mut out = Writer::new(ARCHIVE_LIMIT);
        out.text(ARCHIVE_TAG);
        out.number(entries.len());
        out.frame(&entries[&head].encoded);
        for (root, library) in entries {
            if *root != head {
                out.frame(&library.encoded);
            }
        }
        out.finish()
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut input = Reader::new(bytes, ARCHIVE_LIMIT)?;
        if input.text()? != ARCHIVE_TAG {
            return Err("unknown package archive tag".into());
        }
        let count = input.count()?;
        if count == 0 {
            return Err("no packages".into());
        }
        if count > PACKAGE_LIMIT {
            return Err(format!("over the limit of {PACKAGE_LIMIT} packages"));
        }
        let mut head = None;
        let mut last = None;
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let encoded = input.frame()?;
            let content = Content::decode(encoded)?;
            let root = Root::of(encoded);
            if let Some(head) = head {
                if root == head {
                    return Err(format!("archive repeats {root}"));
                }
                ordered(last.as_ref(), &root, "packages", &root)?;
                last = Some(root);
            } else {
                head = Some(root);
            }
            entries.insert(
                root,
                Library {
                    content,
                    encoded: encoded.into(),
                },
            );
        }
        input.end()?;
        Ok(Self {
            head: head.unwrap(),
            entries,
        })
    }

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
            for (id, root) in &binder.bindings {
                let Some(root) = root else { continue };
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
