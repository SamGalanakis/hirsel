use crate::cli::{Invocation, OptionId};
use crate::package_content::{ARCHIVE_LIMIT, Library, PackageArchive, Root};
use crate::package_dir::MANIFEST;
use crate::package_files::{Captured, open_directory, walk};
use crate::package_lock::{Lock, Mode, NAME as LOCK};
use crate::package_manifest::{Dependency, Manifest, SourceLocation};
use crate::package_resolve::Resolution;
use crate::package_store::{Entry, PackageStore, fetch_https};
use crate::package_workspace::{Edge, Workspace, archive_path, identity, read_manifest};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod edit;
mod github;

type Failure = Box<dyn std::error::Error>;

enum Source {
    Archive(String),
    Url(String),
    GitHub(github::Reference),
    Store(Root),
}

impl Source {
    fn parse(text: String) -> Result<Self, Failure> {
        if text.starts_with("https://") {
            Ok(Self::Url(text))
        } else if let Some(reference) = text.strip_prefix("github:") {
            Ok(Self::GitHub(github::Reference::parse(reference)?))
        } else if text.starts_with("sha256:") {
            Ok(Self::Store(Root::parse(&text).ok_or(
                "expected sha256: followed by 64 lowercase hex digits",
            )?))
        } else if text.ends_with(".hpkg") && !text.contains("://") && !text.starts_with("github:") {
            Ok(Self::Archive(text))
        } else {
            Err("add SOURCE requires an https:// URL, a github: reference, a .hpkg path, or a sha256: root".into())
        }
    }
}

struct Options {
    source: Source,
    expected: Option<Root>,
    store: PackageStore,
    update: bool,
    dry_run: bool,
    json: bool,
}

impl Options {
    fn read(invocation: Invocation) -> Result<Self, Failure> {
        let mut expected = None;
        let mut store = None;
        let mut update = false;
        let mut dry_run = false;
        let mut json = false;
        for (id, value) in invocation.options {
            match id {
                OptionId::Locked | OptionId::Frozen => {
                    let flag = if id == OptionId::Locked {
                        "--locked"
                    } else {
                        "--frozen"
                    };
                    return Err(
                        format!("hirsel.toml needs to be updated but {flag} was passed").into(),
                    );
                }
                OptionId::AddRoot => {
                    expected =
                        Some(value.as_deref().and_then(Root::parse).ok_or(
                            "--root requires sha256: followed by 64 lowercase hex digits",
                        )?);
                }
                OptionId::AddUpdate => update = true,
                OptionId::AddDryRun => dry_run = true,
                OptionId::Json => json = true,
                OptionId::PackageStore => store = value.map(PathBuf::from),
                _ => unreachable!(),
            }
        }
        let source = Source::parse(invocation.positional.ok_or("add requires SOURCE")?)?;
        if expected.is_some() && matches!(source, Source::Store(_)) {
            return Err("--root applies to a fetched archive".into());
        }
        Ok(Self {
            source,
            expected,
            store: PackageStore::new(store),
            update,
            dry_run,
            json,
        })
    }
}

#[derive(Serialize)]
struct ClosureRow {
    id: String,
    root: String,
    new: bool,
}

#[derive(Serialize)]
struct Report {
    result: &'static str,
    id: String,
    root: String,
    source: String,
    committed: Vec<PathBuf>,
    pending: Vec<PathBuf>,
    previous_root: Option<String>,
    dry_run: bool,
    cache: &'static str,
    fetched: BTreeSet<String>,
    closure: Vec<ClosureRow>,
}

impl Report {
    fn with_fetches(&self, error: Failure) -> Failure {
        if self.fetched.is_empty() {
            error
        } else {
            let sources = self.fetched.iter().cloned().collect::<Vec<_>>().join(", ");
            format!("{error}\n  fetched: {sources}").into()
        }
    }

    fn print(
        &self,
        options: &Options,
        target: &Path,
        archive_len: usize,
        lock_message: Option<&str>,
    ) {
        if options.json {
            println!(
                "{}",
                serde_json::to_string(self).expect("add report contains UTF-8 paths")
            );
            return;
        }
        let verb = match (self.result, self.dry_run) {
            ("added", true) => "would add",
            ("updated", true) => "would update",
            ("unchanged", _) => "unchanged",
            (verb, _) => verb,
        };
        let root = match &self.previous_root {
            Some(old) if self.result == "updated" => format!("{old} -> {}", self.root),
            _ => self.root.clone(),
        };
        println!("hirsel: {verb} {} {root} in {}", self.id, target.display());
        if archive_len != 0 {
            let source = self
                .source
                .split_once(':')
                .map_or(self.source.as_str(), |(_, value)| value);
            let new = self.closure.iter().filter(|row| row.new).count();
            println!(
                "        from {source} ({} packages, {} KiB, {new} new in the store)",
                self.closure.len(),
                archive_len.div_ceil(1024)
            );
        }
        if let Some(message) = lock_message {
            if self.dry_run {
                println!(
                    "hirsel: would {}",
                    message
                        .replacen("updated", "update", 1)
                        .replacen("created", "create", 1)
                );
            } else {
                println!("hirsel: {message}");
            }
        }
        for source in &self.fetched {
            println!("hirsel: fetched {source}");
        }
    }
}

fn fd_path(directory: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

fn lock_scope(cwd: &Path) -> Result<File, Failure> {
    let scope = open_directory(&fd_path(&Workspace::scope(cwd)?))?;
    loop {
        // SAFETY: scope is a live, readable descriptor for the workspace root.
        if unsafe { libc::flock(scope.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(scope);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

fn acquire(
    options: &Options,
    workspace: &Workspace,
    cwd: &Path,
) -> Result<(PackageArchive, Option<SourceLocation>, usize), Failure> {
    let (bytes, locator) = match &options.source {
        Source::Store(root) => {
            let library = match options.store.read(*root)? {
                Entry::Present(library) => library,
                Entry::Missing => {
                    return Err(format!("root is not in the package store\n  {root}").into());
                }
                Entry::Damaged => {
                    return Err(format!("root is damaged in the package store\n  {root}").into());
                }
            };
            return Ok((
                PackageArchive {
                    head: *root,
                    entries: BTreeMap::from([(*root, library)]),
                },
                None,
                0,
            ));
        }
        Source::Url(url) => (
            fetch_https(url).map_err(|error| format!("fetching {url} failed: {error}"))?,
            SourceLocation::Url(url.clone()),
        ),
        Source::GitHub(reference) => {
            let url = reference.locator()?;
            (reference.fetch(&url)?, SourceLocation::Url(url))
        }
        Source::Archive(path) => {
            let (from, relative) = if Path::new(path).is_absolute() {
                (
                    Path::new(""),
                    Path::new(path).strip_prefix(&workspace.directory)?,
                )
            } else {
                (cwd.strip_prefix(&workspace.directory)?, Path::new(path))
            };
            let normalized = relative
                .to_str()
                .and_then(|path| archive_path(from, path))
                .ok_or_else(|| format!("archive {path:?} is not inside the workspace root"))?;
            let (bytes, _) = walk(&workspace.root, Path::new(&normalized))
                .and_then(|file| file.read_vec(ARCHIVE_LIMIT))
                .map_err(|error| {
                    format!(
                        "archive {path:?} {}",
                        error.explain("workspace", "a regular file")
                    )
                })?;
            (bytes, SourceLocation::Archive(normalized))
        }
    };
    let len = bytes.len();
    let archive = PackageArchive::decode(bytes.into())
        .map_err(|error| format!("{locator} is not a package archive: {error}"))?;
    let head = &archive.entries[&archive.head].content.id;
    archive.verify(head, archive.head, &locator.to_string())?;
    if let Some(expected) = options.expected
        && expected != archive.head
    {
        let spelling = locator.to_string();
        return Err(format!(
            "fetched root differs from --root\n  fetched: {}\n  expected: {expected}\n  source: {}",
            archive.head,
            spelling.split_once(':').unwrap().1
        )
        .into());
    }
    Ok((archive, Some(locator), len))
}

fn check_source(
    workspace: &Workspace,
    source: &SourceLocation,
    fetched: Root,
) -> Result<(), Failure> {
    let check = |root: Root, locator: &SourceLocation| {
        if root != fetched && locator == source {
            let spelling = source.to_string();
            Err(format!(
                "{} is already bound to another root\n  bound: {root}\n  fetched: {fetched}\n  pass --update to accept the fetched root",
                spelling.split_once(':').unwrap().1
            ))
        } else {
            Ok(())
        }
    };
    for member in workspace.members.values() {
        for edge in member.edges.values() {
            if let Edge::Published(root, Some(locator)) = edge {
                check(*root, locator)?;
            }
        }
    }
    if let Some(root) = workspace.captured_manifest(&workspace.root)? {
        let manifest = Manifest::parse(&root.bytes, &workspace.directory.join(MANIFEST))?;
        if let Some(shared) = manifest.workspace {
            for dependency in shared.dependencies.values() {
                if let Dependency::Published(_, root, Some(locator)) = dependency {
                    if let SourceLocation::Archive(path) = locator {
                        if let Some(path) = archive_path(Path::new(""), path) {
                            check(*root, &SourceLocation::Archive(path))?;
                        }
                    } else {
                        check(*root, locator)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn stage(directory: &File, name: &str, contents: &[u8]) -> io::Result<tempfile::NamedTempFile> {
    let parent = fd_path(directory);
    let mut temp = tempfile::NamedTempFile::new_in(&parent)?;
    match fs::symlink_metadata(parent.join(name)) {
        Ok(metadata) if metadata.is_file() => {
            temp.as_file().set_permissions(fs::Permissions::from_mode(
                metadata.permissions().mode() & 0o777,
            ))?;
        }
        Ok(_) => {
            return Err(io::Error::other(format!(
                "{name} is not a regular directory entry"
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

fn publish(
    temp: tempfile::NamedTempFile,
    directory: &File,
    name: &str,
    path: PathBuf,
    report: &mut Report,
) -> Result<File, Failure> {
    let file = temp.persist(fd_path(directory).join(name))?;
    report.pending.retain(|pending| *pending != path);
    report.committed.push(path);
    directory.sync_all()?;
    Ok(file)
}

fn current_scope(cwd: &Path, scope: &File) -> Result<Workspace, Failure> {
    let workspace =
        Workspace::discover_for(cwd, false)?.ok_or("the workspace changed while hirsel add ran")?;
    if identity(&workspace.root)? != identity(scope)? {
        return Err("the workspace changed while hirsel add ran".into());
    }
    Ok(workspace)
}

struct ManifestWrite<'a> {
    directory: &'a File,
    relative: &'a Path,
    before: &'a Captured,
    proposed: &'a Captured,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Checkpoint {
    ArchiveCaptured,
    ManifestCommitted,
}

fn commit(
    cwd: &Path,
    scope: &File,
    manifest: ManifestWrite<'_>,
    trial: Resolution,
    report: &mut Report,
    checkpoint: impl FnOnce(Checkpoint),
) -> Result<Option<String>, Failure> {
    let ManifestWrite {
        directory: target,
        relative,
        before: captured,
        proposed: edited,
    } = manifest;
    let manifest_path = relative.join(MANIFEST);
    let workspace_path = trial.workspace.directory.clone();
    let changed = || format!("{} changed while hirsel add ran", manifest_path.display());
    let staged = (captured.bytes != edited.bytes)
        .then(|| stage(target, MANIFEST, &edited.bytes))
        .transpose()?;
    let dir = open_directory(&workspace_path.join(relative))
        .map_err(|_| format!("{}; nothing was written", changed()))?;
    if identity(&dir)? != identity(target)?
        || read_manifest(target, &workspace_path.join(relative))
            .ok()
            .flatten()
            .as_ref()
            != Some(captured)
        || (staged.is_some()
            && !fs::symlink_metadata(fd_path(target).join(MANIFEST)).is_ok_and(|m| m.is_file()))
    {
        return Err(format!("{}; nothing was written", changed()).into());
    }
    let published = if let Some(staged) = staged {
        let file = publish(staged, target, MANIFEST, manifest_path.clone(), report)?;
        checkpoint(Checkpoint::ManifestCommitted);
        Captured {
            bytes: edited.bytes.clone(),
            identity: identity(&file)?,
        }
    } else {
        captured.clone()
    };
    let actual = current_scope(cwd, scope)?;
    if actual.captured_manifest(target)? != Some(&published) {
        return Err(changed().into());
    }
    let resolved = Resolution::resolve_cached(
        actual,
        trial.store,
        false,
        trial.libraries,
        &mut report.fetched,
    )?;
    let prior = Lock::capture(&resolved.workspace)?;
    let lock = Lock::compare_captured(&resolved, Mode::Update, prior.as_ref())?;
    let Some((bytes, message)) = lock.pending() else {
        report.pending.retain(|path| path != Path::new(LOCK));
        return Ok(None);
    };
    let staged = stage(scope, LOCK, bytes.as_bytes())?;
    if Lock::capture(&resolved.workspace)? != prior {
        return Err("hirsel.lock changed while hirsel add ran".into());
    }
    publish(staged, scope, LOCK, LOCK.into(), report)?;
    Ok(Some(message.to_owned()))
}

pub(crate) fn run(invocation: Invocation) -> Result<i32, Failure> {
    run_at(invocation, &std::env::current_dir()?, |_| {})
}

pub(crate) fn run_at(
    invocation: Invocation,
    cwd: &Path,
    mut checkpoint: impl FnMut(Checkpoint),
) -> Result<i32, Failure> {
    let options = Options::read(invocation)?;
    let scope = lock_scope(cwd)?;
    let workspace = current_scope(cwd, &scope)?;
    let selected = workspace
        .member
        .as_ref()
        .ok_or("no package manifest here; pass --workspace to declare it for the workspace")?;
    let member = &workspace.members[selected];
    let relative = &member.directory;
    let target = open_directory(&fd_path(
        &walk(&workspace.root, relative)
            .and_then(|file| file.directory())
            .map_err(|error| error.explain("workspace", "a directory"))?,
    ))?;
    let captured = workspace
        .captured_manifest(&target)?
        .expect("the selected manifest was captured");
    let old_lock = Lock::capture(&workspace)?;

    let (archive, locator, len) = acquire(&options, &workspace, cwd)?;
    checkpoint(Checkpoint::ArchiveCaptured);
    let root = archive.head;
    let id = archive.entries[&root].content.id.clone();
    if !options.update
        && let Some(source) = &locator
    {
        check_source(&workspace, source, root)?;
    }

    let prior = member.package.dependencies.get(&id.name);
    let previous_root = member.edges.iter().find_map(|(bound, edge)| match edge {
        Edge::Published(root, _) if bound.name == id.name => Some(root.to_string()),
        _ => None,
    });
    let declared = match &locator {
        Some(SourceLocation::Archive(path)) => {
            let common = relative
                .components()
                .zip(Path::new(path).components())
                .take_while(|(left, right)| left == right)
                .count();
            let up = "../".repeat(relative.components().count() - common);
            let tail: PathBuf = Path::new(path).components().skip(common).collect();
            Some(SourceLocation::Archive(format!("{up}{}", tail.display())))
        }
        other => other.clone(),
    };
    let unchanged = matches!(prior, Some(Dependency::Published(..)))
        && matches!(member.edges.get(&id), Some(Edge::Published(old_root, old_source))
            if *old_root == root && *old_source == locator);

    let mut report = Report {
        result: if unchanged {
            "unchanged"
        } else if prior.is_some() {
            "updated"
        } else {
            "added"
        },
        id: id.to_string(),
        root: root.to_string(),
        source: locator
            .as_ref()
            .map_or_else(|| "store".into(), ToString::to_string),
        committed: Vec::new(),
        pending: Vec::new(),
        previous_root,
        dry_run: options.dry_run,
        cache: "complete",
        fetched: BTreeSet::new(),
        closure: Vec::new(),
    };
    let mut entries = BTreeMap::<Root, Library>::new();
    for (entry_root, library) in archive.entries {
        let new = if locator.is_some() {
            options
                .store
                .install_entry(entry_root, &library.encoded)
                .map_err(|error| format!("package store: {error}\n  cache: partial"))?
        } else {
            false
        };
        report.closure.push(ClosureRow {
            id: library.content.id.to_string(),
            root: entry_root.to_string(),
            new,
        });
        entries.insert(entry_root, library);
    }
    let edited = if unchanged {
        captured.clone()
    } else {
        Captured {
            bytes: edit::binding(&captured.bytes, &id, root, declared.as_ref())?
                .into_bytes()
                .into(),
            identity: captured.identity,
        }
    };
    let overlay = workspace
        .with_manifest(cwd, &target, edited.clone())?
        .ok_or("the workspace changed while hirsel add ran")?;
    let trial = Resolution::resolve_cached(
        overlay,
        options.store.clone(),
        false,
        entries,
        &mut report.fetched,
    )
    .map_err(|error| report.with_fetches(error))?;
    let manifest_path = relative.join(MANIFEST);
    if options.dry_run {
        let lock = Lock::compare_captured(&trial, Mode::Update, old_lock.as_ref())?;
        report.print(
            &options,
            &manifest_path,
            len,
            lock.pending().map(|(_, message)| message),
        );
        return Ok(0);
    }

    if !unchanged {
        report.pending.push(manifest_path.clone());
    }
    report.pending.push(LOCK.into());
    let manifest = ManifestWrite {
        directory: &target,
        relative,
        before: captured,
        proposed: &edited,
    };
    match commit(cwd, &scope, manifest, trial, &mut report, checkpoint) {
        Ok(message) => {
            report.print(&options, &manifest_path, len, message.as_deref());
            Ok(0)
        }
        Err(error) if report.committed.is_empty() => Err(report.with_fetches(error)),
        Err(error) => {
            report.result = "partial";
            let paths = |files: &[PathBuf]| {
                if files.is_empty() {
                    return "none".into();
                }
                files
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            eprintln!(
                "hirsel: {}\n  committed: {}\n  pending: {}\n  run the same hirsel add again to finish",
                report.with_fetches(error),
                paths(&report.committed),
                paths(&report.pending)
            );
            if options.json {
                report.print(&options, &manifest_path, len, None);
            }
            Ok(1)
        }
    }
}
