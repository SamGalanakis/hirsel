//! The package's generated law lock. The static inventory uses the checked
//! source graph and the manifest's test launches without executing a test.

use crate::package_dir::TestRuns;
use crate::test_run::{LawReport, LawRow};
use crate::{CheckProduct, NativeOptions};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_syntax::Item;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::Path;

mod comparison;
pub(crate) use comparison::Baseline;

const NAME: &str = "hirsel.laws";
const HEADER: &str = "hirsel.laws 1";

type TestFiles = BTreeSet<(PackageId, String)>;

pub(crate) fn test_files(request: &crate::CheckRequest) -> TestFiles {
    request
        .inputs
        .packages
        .iter()
        .flat_map(|package| {
            package.sources.iter().filter_map(|source| {
                let captured = source.captured.as_ref()?;
                if captured.test {
                    Some((package.package.clone(), captured.name.clone()?))
                } else {
                    None
                }
            })
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Columns {
    src: String,
    sig: String,
    run: String,
    bug: String,
    oracle: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Line {
    columns: Columns,
    rows: String,
}

#[derive(Default)]
pub(crate) struct Lock(BTreeMap<String, Line>);

pub(crate) struct Inventory(BTreeMap<String, Columns>);

impl Inventory {
    pub(crate) fn of(
        product: &CheckProduct,
        tests: &[DefPath],
        files: &TestFiles,
        runs: &TestRuns,
    ) -> io::Result<Self> {
        let options = NativeOptions::development(
            String::new(),
            Default::default(),
            crate::Link::Driver(Default::default()),
            Default::default(),
            Default::default(),
        );
        let mut result = BTreeMap::new();
        for test in tests {
            let (source, signatures) = definition_inputs(product, test, files)?;
            let mut run = runs.options(test, &options);
            if let Some(Ok(entry)) = product.test_entries.get(test) {
                crate::test_run::grant_parameters(entry, &mut run);
            }
            let header = product.headers.get(test).ok_or_else(|| missing(test))?;
            let item = source_item(product, header).ok_or_else(|| missing(test))?;
            let docs = &item.text[item.range.owned.start as usize..item.file_offset as usize];
            let field = |name| {
                let text = docs
                    .lines()
                    .filter_map(|line| line.trim_start().strip_prefix(name))
                    .map(str::trim)
                    .collect::<Vec<_>>();
                if text.is_empty() {
                    "-".into()
                } else {
                    bytes(&text.join("\n"))
                }
            };
            let (limit, mask, net, credentials, env, dirs, foreign, exec, ..) =
                crate::native_entry::policy(&run);
            let config =
                serde_json::to_string(&(limit, mask, net, credentials, env, dirs, foreign, exec))?;
            result.insert(
                test.to_string(),
                Columns {
                    src: source,
                    sig: signatures,
                    run: bytes(&config),
                    bug: field("/// bug:"),
                    oracle: field("/// oracle:"),
                },
            );
        }
        Ok(Self(result))
    }
}

fn missing(test: &DefPath) -> io::Error {
    io::Error::other(format!("law source is missing for {test}"))
}

fn source_item<'a>(product: &'a CheckProduct, header: &crate::reuse::Header) -> Option<Item<'a>> {
    let (_, source, item) = header.anchor;
    let file = product.files.iter().find(|file| file.file == source)?;
    Some(file.syntax.item(item as usize))
}

fn definition_inputs(
    product: &CheckProduct,
    test: &DefPath,
    files: &TestFiles,
) -> io::Result<(String, String)> {
    let root = product.headers.id(test).ok_or_else(|| missing(test))?;
    let mut seen = BTreeSet::new();
    let mut pending = vec![root];
    let mut source = BTreeMap::new();
    let mut signatures = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let path = product.index.definition_path(id);
        let Some(header) = product.headers.row(id) else {
            continue;
        };
        let test_source = id == root || product.test_entries.contains_key(&path) || {
            let (_, src, _) = header.anchor;
            product
                .files
                .iter()
                .find(|file| file.file == src)
                .and_then(|file| file.name.as_ref())
                .is_some_and(|name| files.contains(&(path.0.package.clone(), name.clone())))
        };
        if test_source {
            let item = source_item(product, header).ok_or_else(|| missing(&path))?;
            source.insert(path.to_string(), item);
            pending.extend(header.reads.iter().copied());
            match resolved_constructors(product, &path) {
                Some(types) => {
                    pending.extend(
                        header
                            .body_reads
                            .iter()
                            .filter(|id| !header.body_candidates.contains(id))
                            .copied(),
                    );
                    pending.extend(types);
                }
                None => pending.extend(header.body_reads.iter().copied()),
            }
        } else {
            signatures.insert(
                path.to_string(),
                (path.definition_identity(), header.digest.0),
            );
        }
    }
    let mut src = Sha256::new();
    for (path, item) in source {
        part(&mut src, path.as_bytes());
        part(&mut src, without_docs(&item).as_bytes());
    }
    let mut sig = Sha256::new();
    for (identity, digest) in signatures.into_values() {
        part(&mut sig, identity.as_bytes());
        part(&mut sig, &digest);
    }
    Ok((
        short(src.finalize().as_ref()),
        short(sig.finalize().as_ref()),
    ))
}

/// The types a checked body's contextual constructors resolved to, which stand
/// in for their candidate sets: an unused same-named constructor elsewhere is
/// not a dependency. None keeps every candidate: the body was not built, or a
/// constructor was resolved by searching its whole candidate set.
fn resolved_constructors(
    product: &CheckProduct,
    path: &DefPath,
) -> Option<Vec<hirsel_core_resolve::DefinitionId>> {
    let check = &product.definitions.get(path)?.check;
    if check.function.is_none() || !check.refusals.is_empty() || check.constructor_candidates {
        return None;
    }
    check
        .constructor_types
        .iter()
        .map(|ty| product.headers.id(ty))
        .collect()
}

/// An item's source text with its `///` doc comments removed, so a doc edit
/// changes no `src`.
fn without_docs(item: &Item<'_>) -> String {
    let text = item.source_text();
    let mut from = 0;
    let mut content = String::with_capacity(text.len());
    for token in &item.tree.tokens[item.range.tokens.start as usize..item.range.tokens.end as usize]
    {
        if !token.kind.is_trivia()
            || token.span.start < item.file_offset
            || token.span.end > item.range.end
        {
            continue;
        }
        let start = (token.span.start - item.file_offset) as usize;
        let end = (token.span.end - item.file_offset) as usize;
        if text[start..end].starts_with("///") {
            content.push_str(&text[from..start]);
            from = end;
        }
    }
    content.push_str(&text[from..]);
    content
}

/// A source function's `src` (docs/coverage.md, "The site map"): the short
/// hash of its source without doc comments, framed by its length.
pub(crate) fn function_src(product: &CheckProduct, path: &DefPath) -> Option<String> {
    let header = product.headers.row(product.headers.id(path)?)?;
    let mut src = Sha256::new();
    part(
        &mut src,
        without_docs(&source_item(product, header)?).as_bytes(),
    );
    Some(short(src.finalize().as_ref()))
}

pub(crate) fn part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

pub(crate) fn short(hash: &[u8]) -> String {
    let mut text = String::with_capacity(16);
    for byte in &hash[..8] {
        write!(&mut text, "{byte:02x}").unwrap();
    }
    text
}

fn bytes(text: &str) -> String {
    short(Sha256::digest(text.as_bytes()).as_ref())
}

impl Lock {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let invalid = || io::Error::other(format!("{}: invalid {NAME}", path.display()));
        Self::parse(&text).ok_or_else(invalid)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let mut records = BTreeMap::new();
        for line in lines {
            let mut fields = line.split_ascii_whitespace();
            let definition = fields.next()?.to_owned();
            DefPath::from_full(&definition)?;
            let mut field = |name| fields.next()?.strip_prefix(name).map(str::to_owned);
            let columns = Columns {
                src: field("src=")?,
                sig: field("sig=")?,
                run: field("run=")?,
                bug: field("bug=")?,
                oracle: field("oracle=")?,
            };
            let rows = field("rows=")?;
            if fields.next().is_some()
                || records
                    .last_key_value()
                    .is_some_and(|(last, _)| last >= &definition)
            {
                return None;
            }
            records.insert(definition, Line { columns, rows });
        }
        Some(Self(records))
    }

    fn text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for (definition, line) in &self.0 {
            let Columns {
                src,
                sig,
                run,
                bug,
                oracle,
            } = &line.columns;
            writeln!(
                out,
                "{definition} src={src} sig={sig} run={run} bug={bug} oracle={oracle} rows={}",
                line.rows
            )
            .unwrap();
        }
        out
    }

    pub(crate) fn differences(&self, current: &Inventory) -> Vec<String> {
        let mut changed = Vec::new();
        for (test, now) in &current.0 {
            let Some(old) = self.0.get(test).map(|line| &line.columns) else {
                changed.push(format!("{test}: missing"));
                continue;
            };
            for (name, was, is) in [
                ("src", &old.src, &now.src),
                ("sig", &old.sig, &now.sig),
                ("run", &old.run, &now.run),
                ("bug", &old.bug, &now.bug),
                ("oracle", &old.oracle, &now.oracle),
            ] {
                if is != was {
                    changed.push(format!("{test}: {name} {was} -> {is}"));
                }
            }
        }
        for test in self.0.keys().filter(|name| !current.0.contains_key(*name)) {
            changed.push(format!("{test}: removed"));
        }
        changed
    }

    pub(crate) fn refresh(&mut self, current: Inventory) {
        self.0 = current
            .0
            .into_iter()
            .map(|(test, columns)| {
                let rows = self
                    .0
                    .remove(&test)
                    .filter(|old| old.columns.src == columns.src && old.columns.sig == columns.sig)
                    .map_or_else(|| "pending".into(), |old| old.rows);
                (test, Line { columns, rows })
            })
            .collect();
    }

    pub(crate) fn record(
        &mut self,
        current: &Inventory,
        observations: &BTreeMap<String, LawReport>,
    ) {
        for (name, report) in observations {
            let Some(columns) = current.0.get(name) else {
                continue;
            };
            let rows = encode_rows(name, report.rows());
            self.0
                .entry(name.clone())
                .and_modify(|line| line.rows.clone_from(&rows))
                .or_insert_with(|| Line {
                    columns: columns.clone(),
                    rows,
                });
        }
    }
}

pub(crate) fn read(directory: &Path) -> io::Result<Lock> {
    Lock::read(&directory.join(NAME))
}

pub(crate) fn update(directory: &Path, update: impl FnOnce(&mut Lock)) -> io::Result<()> {
    let internal = directory.join(".hirsel-core");
    std::fs::create_dir_all(&internal)?;
    let guard = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(internal.join("law-lock-writer"))?;
    guard.lock()?;
    let path = directory.join(NAME);
    let mut lock = Lock::read(&path)?;
    update(&mut lock);
    crate::package_store::atomic_write(&path, lock.text().as_bytes())
}

fn site_key(test: &str, row: &LawRow) -> String {
    let (owner, ordinal) = row.site.rsplit_once('#').expect("compiled law site");
    if owner == test {
        format!("{}#{ordinal}", row.callee)
    } else {
        format!("{}@{owner}#{ordinal}", row.callee)
    }
}

fn label_key(label: &str) -> String {
    let mut out = String::new();
    for byte in label.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
            out.push(byte as char);
        } else {
            write!(&mut out, "%{byte:02X}").unwrap();
        }
    }
    out
}

fn encode_rows(test: &str, rows: &[LawRow]) -> String {
    let mut locked = Vec::new();
    for row in rows {
        if row.callee == "sometimes" {
            locked.extend(
                row.witnessed
                    .iter()
                    .filter(|(_, count)| **count != 0)
                    .map(|(label, _)| format!("sometimes:{}", label_key(label))),
            );
        } else {
            let site = site_key(test, row);
            let mut class = row.class.replace(' ', "-");
            if let Some(kind) = row.kind {
                write!(&mut class, "/{kind}").unwrap();
            }
            let numbers = if row.per_build {
                "per-build".into()
            } else {
                row.numbers
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join("/")
            };
            locked.push(format!("{site}:{class}:{numbers}"));
        }
    }
    if locked.is_empty() {
        "-".into()
    } else {
        locked.join(";")
    }
}
