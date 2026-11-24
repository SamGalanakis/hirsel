use crate::cli::{Invocation, OptionId};
use crate::package_content::{ARCHIVE_LIMIT, Id, Library, PackageArchive, Root, toolchain_major};
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
mod paths;

use paths::{find_member, relative_to, root_relative};

type Failure = Box<dyn std::error::Error>;

enum Source {
    Archive(String),
    Url(String),
    GitHub(github::Reference),
    Store(Root),
    Toolchain(Id),
    Member(String),
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
        } else if text.contains("://") {
            Err("add SOURCE requires an https:// URL, a github: reference, a .hpkg path, a sha256: root, NAME@MAJOR, or a member directory".into())
        } else if text.ends_with(".hpkg") {
            Ok(Self::Archive(text))
        } else if let Some((name, major)) = text.split_once('@')
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && !major.is_empty()
            && major.bytes().all(|b| b.is_ascii_digit())
        {
            let major = major
                .parse()
                .map_err(|_| format!("{text}: the major does not fit in 32 bits"))?;
            Ok(Self::Toolchain(Id {
                name: name.to_owned(),
                major,
            }))
        } else {
            Ok(Self::Member(text))
        }
    }
}

struct Options {
    source: Source,
    expected: Option<Root>,
    store: PackageStore,
    update: bool,
    workspace: bool,
    dry_run: bool,
    json: bool,
}

impl Options {
    fn read(invocation: Invocation) -> Result<Self, Failure> {
        let mut expected = None;
        let mut store = None;
        let mut update = false;
        let mut workspace = false;
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
                OptionId::AddWorkspace => workspace = true,
                OptionId::AddDryRun => dry_run = true,
                OptionId::Json => json = true,
                OptionId::PackageStore => store = value.map(PathBuf::from),
                _ => unreachable!(),
            }
        }
        let source = Source::parse(invocation.positional.ok_or("add requires SOURCE")?)?;
        if expected.is_some()
            && matches!(
                source,
                Source::Store(_) | Source::Toolchain(_) | Source::Member(_)
            )
        {
            return Err("--root applies to a fetched archive".into());
        }
        Ok(Self {
            source,
            expected,
            store: PackageStore::new(store),
            update,
            workspace,
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
    root: Option<String>,
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
        let bound = self.root.clone().unwrap_or_else(|| self.source.clone());
        let root = match &self.previous_root {
            Some(old) if self.result == "updated" => format!("{old} -> {bound}"),
            _ => bound,
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
        Source::Toolchain(_) | Source::Member(_) => {
            unreachable!("run_at binds the forms that fetch nothing without acquiring")
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
                if let Dependency::Published(_, root, Some(locator)) = dependency
                    && let Some(locator) = root_relative(locator)
                {
                    check(*root, &locator)?;
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
    if !report.committed.contains(&path) {
        report.committed.push(path);
    }
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

/// One manifest operation: the bytes captured before the edit and the bytes
/// to publish. Each operation leaves a valid workspace by itself.
struct ManifestWrite<'a> {
    directory: &'a File,
    relative: PathBuf,
    before: Captured,
    proposed: Captured,
}

impl ManifestWrite<'_> {
    fn path(&self) -> PathBuf {
        self.relative.join(MANIFEST)
    }

    fn changes(&self) -> bool {
        self.before.bytes != self.proposed.bytes
    }

    /// Whether the directory and manifest on disk are still the captured ones.
    fn current(&self, workspace: &Path) -> bool {
        let at = workspace.join(&self.relative);
        open_directory(&at)
            .ok()
            .and_then(|dir| identity(&dir).ok())
            .is_some_and(|dir| identity(self.directory).ok() == Some(dir))
            && read_manifest(self.directory, &at).ok().flatten().as_ref() == Some(&self.before)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Checkpoint {
    ArchiveCaptured,
    ManifestCommitted,
}

/// Derives `hirsel.lock` from the committed manifests and commits it.
fn settle(
    cwd: &Path,
    scope: &File,
    published: Option<(&File, &Captured, &Path)>,
    store: PackageStore,
    libraries: BTreeMap<Root, Library>,
    report: &mut Report,
) -> Result<(PackageStore, BTreeMap<Root, Library>, Option<String>), Failure> {
    let actual = current_scope(cwd, scope)?;
    if let Some((target, published, path)) = published
        && actual.captured_manifest(target)? != Some(published)
    {
        return Err(format!("{} changed while hirsel add ran", path.display()).into());
    }
    let resolved =
        Resolution::resolve_cached(actual, store, false, libraries, &mut report.fetched)?;
    let prior = Lock::capture(&resolved.workspace)?;
    let lock = Lock::compare_captured(&resolved, Mode::Update, prior.as_ref())?;
    let message = match lock.pending() {
        None => {
            report.pending.retain(|path| path != Path::new(LOCK));
            None
        }
        Some((bytes, message)) => {
            let staged = stage(scope, LOCK, bytes.as_bytes())?;
            if Lock::capture(&resolved.workspace)? != prior {
                return Err("hirsel.lock changed while hirsel add ran".into());
            }
            publish(staged, scope, LOCK, LOCK.into(), report)?;
            Some(message.to_owned())
        }
    };
    Ok((resolved.store, resolved.libraries, message))
}

/// Publishes each changed manifest in order, deriving the lock after each.
fn commit(
    cwd: &Path,
    scope: &File,
    writes: &[ManifestWrite<'_>],
    trial: Resolution,
    report: &mut Report,
    checkpoint: &mut dyn FnMut(Checkpoint),
) -> Result<Vec<String>, Failure> {
    let workspace_path = trial.workspace.directory.clone();
    for write in writes {
        if !write.current(&workspace_path) {
            return Err(format!(
                "{} changed while hirsel add ran; nothing was written",
                write.path().display()
            )
            .into());
        }
    }
    let (mut store, mut libraries) = (trial.store, trial.libraries);
    let mut messages = Vec::new();
    let changing: Vec<_> = writes.iter().filter(|write| write.changes()).collect();
    if changing.is_empty() {
        let (_, _, message) = settle(cwd, scope, None, store, libraries, report)?;
        messages.extend(message);
        return Ok(messages);
    }
    for (index, write) in changing.iter().enumerate() {
        let path = write.path();
        let staged = stage(write.directory, MANIFEST, &write.proposed.bytes)?;
        if !write.current(&workspace_path)
            || !fs::symlink_metadata(fd_path(write.directory).join(MANIFEST))
                .is_ok_and(|metadata| metadata.is_file())
        {
            let written = if report.committed.is_empty() {
                "; nothing was written"
            } else {
                ""
            };
            return Err(format!("{} changed while hirsel add ran{written}", path.display()).into());
        }
        let file = publish(staged, write.directory, MANIFEST, path.clone(), report)?;
        checkpoint(Checkpoint::ManifestCommitted);
        let published = Captured {
            bytes: write.proposed.bytes.clone(),
            identity: identity(&file)?,
        };
        let message;
        (store, libraries, message) = settle(
            cwd,
            scope,
            Some((write.directory, &published, &path)),
            store,
            libraries,
            report,
        )?;
        messages.extend(message);
        if index + 1 < changing.len() && !report.pending.iter().any(|p| p == Path::new(LOCK)) {
            report.pending.push(LOCK.into());
        }
    }
    Ok(messages)
}

/// The binding a manifest writes: a member path, a published root, or a toolchain name.
fn form<'a>(
    path: Option<&'a str>,
    root: Option<Root>,
    source: Option<&'a SourceLocation>,
) -> edit::Form<'a> {
    match (path, root) {
        (Some(path), _) => edit::Form::Member(path),
        (None, Some(root)) => edit::Form::Published(root, source),
        (None, None) => edit::Form::Toolchain,
    }
}

fn spelling(id: &crate::package_content::Id, edge: &Edge) -> String {
    match edge {
        Edge::Published(root, _) => format!("{id} {root}"),
        Edge::Member => format!("{id} member"),
        Edge::Toolchain => format!("{id} toolchain"),
    }
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
    let root_manifest = workspace.directory.join(MANIFEST);
    let root_captured = workspace
        .captured_manifest(&scope)?
        .cloned()
        .ok_or("the workspace changed while hirsel add ran")?;
    let selected = workspace.member.clone();
    if selected.is_none() && !options.workspace {
        return Err(format!(
            "{}: no package manifest here; pass --workspace to declare it for the workspace",
            root_manifest.display()
        )
        .into());
    }
    let member = selected.as_ref().map(|id| &workspace.members[id]);
    let shared = if options.workspace {
        let declared = Manifest::parse(&root_captured.bytes, &root_manifest)?;
        Some(declared.workspace.ok_or_else(|| {
            format!(
                "{}: no [workspace]; hirsel add does not create a workspace boundary",
                member
                    .map_or(&root_manifest, |member| &member.manifest_path)
                    .display()
            )
        })?)
    } else {
        None
    };
    let member_target = member
        .map(|member| -> Result<File, Failure> {
            Ok(open_directory(&fd_path(
                &walk(&workspace.root, &member.directory)
                    .and_then(|file| file.directory())
                    .map_err(|error| error.explain("workspace", "a directory"))?,
            ))?)
        })
        .transpose()?;
    let old_lock = Lock::capture(&workspace)?;

    let (archive, locator, len, local) = match &options.source {
        Source::Toolchain(id) => {
            if toolchain_major(&id.name) != Some(id.major) {
                return Err(format!("the toolchain does not provide {id}").into());
            }
            (None, None, 0, Some((id.clone(), None)))
        }
        Source::Member(path) => {
            let (id, found) = find_member(&workspace, cwd, path)?;
            (
                None,
                None,
                0,
                Some((id.clone(), Some(found.directory.clone()))),
            )
        }
        _ => {
            let (archive, locator, len) = acquire(&options, &workspace, cwd)?;
            (Some(archive), locator, len, None)
        }
    };
    checkpoint(Checkpoint::ArchiveCaptured);
    let root = archive.as_ref().map(|archive| archive.head);
    let id = match (&archive, &local) {
        (Some(archive), _) => archive.entries[&archive.head].content.id.clone(),
        (None, Some((id, _))) => id.clone(),
        (None, None) => unreachable!("every source names an archive or a local package"),
    };
    if !options.update
        && let (Some(source), Some(root)) = (&locator, root)
    {
        check_source(&workspace, source, root)?;
    }

    let prior = member.and_then(|member| member.package.dependencies.get(&id.name));
    let bound =
        member.and_then(|member| member.edges.iter().find(|(bound, _)| bound.name == id.name));
    let bound_root = bound.and_then(|(_, edge)| match edge {
        Edge::Published(root, _) => Some(*root),
        _ => None,
    });
    let directory = local
        .as_ref()
        .and_then(|(_, directory)| directory.as_deref());
    let edge = match (directory, root) {
        (Some(_), _) => Edge::Member,
        (None, Some(root)) => Edge::Published(root, locator.clone()),
        (None, None) => Edge::Toolchain,
    };
    let mut writes = Vec::new();
    let (updated, previous_root, needs_package);
    if let Some(shared) = &shared {
        let entry = shared.dependencies.get(&id.name);
        let same = match (entry, &edge) {
            (Some(Dependency::Published(declared, old_root, old_source)), Edge::Published(..)) => {
                *declared == id
                    && Some(*old_root) == root
                    && old_source.as_ref().and_then(root_relative) == locator
            }
            (Some(Dependency::Toolchain(declared)), Edge::Toolchain)
            | (Some(Dependency::Member(declared, _)), Edge::Member) => *declared == id,
            _ => false,
        };
        let own = prior.filter(|dependency| !matches!(dependency, Dependency::Inherited));
        if let (Some(_), Some(member), Some((name, old))) = (own, member, bound)
            && !options.update
        {
            return Err(format!(
                "{}: dependencies.{} is bound here and in the workspace; pass --update to replace the local binding with {{ workspace = true }}\n  {} = {}\n  {} = {}",
                member.manifest_path.display(),
                id.name,
                member.manifest_path.display(),
                spelling(name, old),
                root_manifest.display(),
                spelling(&id, &edge),
            )
            .into());
        }
        if let Some(entry) = entry
            && !same
            && !options.update
        {
            let consumers: Vec<String> = workspace
                .members
                .iter()
                .filter(|(other, sibling)| {
                    Some(*other) != selected.as_ref()
                        && matches!(
                            sibling.package.dependencies.get(&id.name),
                            Some(Dependency::Inherited)
                        )
                })
                .map(|(other, _)| other.to_string())
                .collect();
            if !consumers.is_empty() {
                let declared = match entry {
                    Dependency::Toolchain(declared)
                    | Dependency::Member(declared, _)
                    | Dependency::Published(declared, ..) => declared.to_string(),
                    Dependency::Inherited => id.name.clone(),
                };
                return Err(format!(
                    "the workspace binds {declared} for {}; pass --update to change it",
                    consumers.join(", ")
                )
                .into());
            }
        }
        let root_package = member.is_some_and(|member| member.directory.as_os_str().is_empty());
        let path = directory.map(|directory| relative_to(Path::new(""), directory));
        let proposed = if same && (!root_package || matches!(prior, Some(Dependency::Inherited))) {
            root_captured.clone()
        } else {
            Captured {
                bytes: edit::shared(
                    &root_captured.bytes,
                    &id,
                    &form(path.as_deref(), root, locator.as_ref()),
                    root_package,
                )?
                .into_bytes()
                .into(),
                identity: root_captured.identity,
            }
        };
        writes.push(ManifestWrite {
            directory: &scope,
            relative: PathBuf::new(),
            before: root_captured.clone(),
            proposed,
        });
        if let (Some(member), Some(target)) = (member, &member_target)
            && !root_package
        {
            let before = workspace
                .captured_manifest(target)?
                .expect("the selected manifest was captured")
                .clone();
            let proposed = if matches!(prior, Some(Dependency::Inherited)) {
                before.clone()
            } else {
                Captured {
                    bytes: edit::inherit(&before.bytes, &id.name)?.into_bytes().into(),
                    identity: before.identity,
                }
            };
            writes.push(ManifestWrite {
                directory: target,
                relative: member.directory.clone(),
                before,
                proposed,
            });
        }
        let replaced = match entry {
            Some(Dependency::Published(_, old_root, _)) if !same => Some(*old_root),
            _ => None,
        };
        previous_root = replaced.or(own.and(bound_root));
        updated = (entry.is_some() && !same) || own.is_some();
        needs_package = member.is_some();
    } else {
        let member = member.expect("a package was selected");
        let target = member_target.as_ref().expect("a package was selected");
        let relative = &member.directory;
        let declared = match &locator {
            Some(SourceLocation::Archive(path)) => Some(SourceLocation::Archive(relative_to(
                relative,
                Path::new(path),
            ))),
            other => other.clone(),
        };
        let path = directory.map(|directory| relative_to(relative, directory));
        let inherited = matches!(prior, Some(Dependency::Inherited));
        let unchanged = matches!(
            (prior, &edge),
            (Some(Dependency::Inherited), _)
                | (Some(Dependency::Published(..)), Edge::Published(..))
                | (Some(Dependency::Toolchain(_)), Edge::Toolchain)
                | (Some(Dependency::Member(..)), Edge::Member)
        ) && member.edges.get(&id) == Some(&edge);
        if inherited && !unchanged && !options.update {
            return Err(format!(
                "{}: dependencies.{} inherits the workspace binding; pass --workspace to change it for the workspace, or --update to bind it here",
                member.manifest_path.display(),
                id.name
            )
            .into());
        }
        let before = workspace
            .captured_manifest(target)?
            .expect("the selected manifest was captured")
            .clone();
        let proposed = if unchanged {
            before.clone()
        } else {
            Captured {
                bytes: edit::binding(
                    &before.bytes,
                    &id,
                    &form(path.as_deref(), root, declared.as_ref()),
                )?
                .into_bytes()
                .into(),
                identity: before.identity,
            }
        };
        writes.push(ManifestWrite {
            directory: target,
            relative: relative.clone(),
            before,
            proposed,
        });
        previous_root = bound_root;
        updated = prior.is_some();
        needs_package = true;
    }
    let unchanged = writes.iter().all(|write| !write.changes());

    let mut report = Report {
        result: if unchanged {
            "unchanged"
        } else if updated {
            "updated"
        } else {
            "added"
        },
        id: id.to_string(),
        root: root.map(|root| root.to_string()),
        source: match (&locator, &local) {
            (Some(locator), _) => locator.to_string(),
            (None, Some((_, None))) => "toolchain".into(),
            (None, Some((_, Some(directory)))) => {
                let dir = directory.to_str().ok_or("member directory is not UTF-8")?;
                format!("member:{}", if dir.is_empty() { "." } else { dir })
            }
            (None, None) => "store".into(),
        },
        committed: Vec::new(),
        pending: Vec::new(),
        previous_root: previous_root.map(|root| root.to_string()),
        dry_run: options.dry_run,
        cache: "complete",
        fetched: BTreeSet::new(),
        closure: Vec::new(),
    };
    let mut entries = BTreeMap::<Root, Library>::new();
    for (entry_root, library) in archive.into_iter().flat_map(|archive| archive.entries) {
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
    let edits: Vec<(&File, &Captured)> = writes
        .iter()
        .filter(|write| write.changes())
        .map(|write| (write.directory, &write.proposed))
        .collect();
    let overlay = workspace
        .with_manifests(cwd, &edits, needs_package)?
        .ok_or("the workspace changed while hirsel add ran")?;
    let trial = Resolution::resolve_cached(
        overlay,
        options.store.clone(),
        false,
        entries,
        &mut report.fetched,
    )
    .map_err(|error| report.with_fetches(error))?;
    let manifest_path = writes[0].path();
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

    for write in writes.iter().filter(|write| write.changes()) {
        report.pending.push(write.path());
    }
    report.pending.push(LOCK.into());
    match commit(cwd, &scope, &writes, trial, &mut report, &mut checkpoint) {
        Ok(messages) => {
            let message = messages.join("\nhirsel: ");
            report.print(
                &options,
                &manifest_path,
                len,
                (!messages.is_empty()).then_some(message.as_str()),
            );
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
