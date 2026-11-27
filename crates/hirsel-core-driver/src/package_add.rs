use crate::cli::{AddOption, Request};
use crate::package_content::{ARCHIVE_LIMIT, Library, PackageArchive, Root, toolchain_major};
use crate::package_dir::MANIFEST;
use crate::package_files::{Captured, open_directory, walk};
use crate::package_git::{Budget, pack_listed};
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

pub(crate) mod edit;
pub(crate) mod git;
#[cfg(test)]
mod laws_registry;
pub(crate) mod options;
pub(crate) mod paths;
#[path = "registry_select.rs"]
pub(crate) mod registry_select;

use options::{Options, Source};
use paths::{find_member, relative_to, root_relative};

type Failure = Box<dyn std::error::Error>;

#[derive(Serialize)]
struct ClosureRow {
    id: String,
    root: String,
    new: bool,
}

#[derive(Serialize)]
struct Report {
    result: &'static str,
    alias: String,
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    registry: BTreeMap<String, registry_select::Finding>,
    /// The branch and commit taken for a git source given without a reference.
    #[serde(skip)]
    chose: Option<String>,
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
        for (id, finding) in &self.registry {
            finding.report(id);
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
        let id = if options.alias.is_some() {
            format!("{} = {}", self.alias, self.id)
        } else {
            self.id.clone()
        };
        println!("hirsel: {verb} {id} {root} in {}", target.display());
        if let Some(chose) = &self.chose {
            println!("hirsel: {chose}");
        }
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

fn fd_path(directory: &File) -> io::Result<PathBuf> {
    crate::host::fd_path(directory)
}

pub(crate) fn lock_scope(cwd: &Path) -> Result<File, Failure> {
    let scope = open_directory(&fd_path(&Workspace::scope(cwd)?)?)?;
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

/// A captured bundle: the archive, its source, its size, the sources a git commit's snapshot
/// made the tool contact, and the branch taken for a git source with no reference.
struct Capture {
    archive: PackageArchive,
    locator: Option<SourceLocation>,
    len: usize,
    fetched: Vec<String>,
    chose: Option<String>,
}

fn acquire(options: &Options, workspace: &Workspace, cwd: &Path) -> Result<Capture, Failure> {
    let (archive, locator, len, fetched, chose) =
        acquire_any(&options.source, &options.store, workspace, cwd)?;
    if let (Some(expected), Some(locator)) = (options.expected, &locator)
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
    Ok(Capture {
        archive,
        locator,
        len,
        fetched,
        chose,
    })
}

pub(crate) type Acquisition = (
    PackageArchive,
    Option<SourceLocation>,
    usize,
    Vec<String>,
    Option<String>,
);

pub(crate) fn acquire_any(
    source: &Source,
    store: &PackageStore,
    workspace: &Workspace,
    cwd: &Path,
) -> Result<Acquisition, Failure> {
    let (archive, len, locator) = match source {
        Source::Store(root) => {
            let library = match store.read(*root)? {
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
                Vec::new(),
                None,
            ));
        }
        Source::Url(url) => {
            let (archive, len) = fetch_https(&store.transport, url, &mut |_| Ok(()), "")?;
            (archive, len, SourceLocation::Url(url.clone()))
        }
        Source::Git(request) => {
            let mut budget = Budget::new(String::new());
            let (git, remote, chose) = request.resolve(&store.transport, &mut budget)?;
            let (_, acquired) = pack_listed(
                store,
                &store.session,
                &mut budget,
                None,
                None,
                &git,
                remote.as_ref(),
            )?;
            let len = acquired
                .archive
                .entries
                .values()
                .map(|library| library.encoded.len())
                .sum();
            return Ok((
                acquired.archive,
                Some(SourceLocation::Git(git)),
                len,
                acquired.fetched,
                chose,
            ));
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
            let len = bytes.len();
            let locator = SourceLocation::Archive(normalized);
            let archive = PackageArchive::decode_as(bytes.into(), false)
                .map_err(|error| format!("{locator} is not a package archive: {error}"))?;
            (archive, len, locator)
        }
        Source::Toolchain(_) | Source::Member(_) | Source::Registry(_) => {
            unreachable!("run_at binds the forms that fetch nothing without acquiring")
        }
    };
    let head = &archive.entries[&archive.head].content.id;
    archive.verify(head, archive.head, &locator.to_string())?;
    Ok((archive, Some(locator), len, Vec::new(), None))
}

pub(crate) fn stage(
    directory: &File,
    name: &str,
    contents: &[u8],
) -> io::Result<tempfile::NamedTempFile> {
    let parent = fd_path(directory)?;
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
    let file = temp.persist(fd_path(directory)?.join(name))?;
    report.pending.retain(|pending| *pending != path);
    if !report.committed.contains(&path) {
        report.committed.push(path);
    }
    directory.sync_all()?;
    Ok(file)
}

pub(crate) fn current_scope(cwd: &Path, scope: &File, command: &str) -> Result<Workspace, Failure> {
    let workspace = Workspace::discover_for(cwd, false)?
        .ok_or_else(|| format!("the workspace changed while hirsel {command} ran"))?;
    if identity(&workspace.root)? != identity(scope)? {
        return Err(format!("the workspace changed while hirsel {command} ran").into());
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
    let actual = current_scope(cwd, scope, "add")?;
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
            || !fs::symlink_metadata(fd_path(write.directory)?.join(MANIFEST))
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

pub(crate) fn run(invocation: Request<AddOption>) -> Result<i32, Failure> {
    run_at(invocation, &std::env::current_dir()?, |_| {})
}

pub(crate) fn run_at(
    invocation: Request<AddOption>,
    cwd: &Path,
    mut checkpoint: impl FnMut(Checkpoint),
) -> Result<i32, Failure> {
    let options = Options::read(invocation)?;
    let scope = lock_scope(cwd)?;
    let workspace = current_scope(cwd, &scope, "add")?;
    let mut registry = registry_select::Registry::new(&options.store)?;
    let mut authority_refusal = None;
    loop {
        match add(
            &options,
            cwd,
            &scope,
            &workspace,
            &mut registry,
            &mut checkpoint,
        ) {
            Ok(Attempt::Complete(code)) => return Ok(code),
            Ok(Attempt::Denied(root, error)) if registry.deny(root) => {
                authority_refusal = Some(error);
            }
            Ok(Attempt::Denied(_, error)) => return Err(error),
            Err(e) => {
                return Err(
                    if e.downcast_ref::<registry_select::NoEligible>().is_some() {
                        authority_refusal.unwrap_or(e)
                    } else {
                        e
                    },
                );
            }
        }
    }
}

enum Attempt {
    Complete(i32),
    Denied(Root, Failure),
}

fn add(
    options: &Options,
    cwd: &Path,
    scope: &File,
    workspace: &Workspace,
    registry: &mut registry_select::Registry,
    checkpoint: &mut dyn FnMut(Checkpoint),
) -> Result<Attempt, Failure> {
    let root_manifest = workspace.directory.join(MANIFEST);
    let root_captured = workspace
        .captured_manifest(scope)?
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
            )?)?)
        })
        .transpose()?;
    let old_lock = Lock::capture(&workspace)?;

    let (mut fetched_early, mut chose) = (Vec::new(), None);
    let (archive, locator, len, local) = match &options.source {
        Source::Registry(query) => {
            let chosen = registry.add_candidate(
                query,
                options.alias.as_deref(),
                options.workspace,
                workspace,
            )?;
            let root = chosen.root;
            let len = chosen.library.encoded.len();
            let locator = SourceLocation::Registry(chosen.origin);
            let archive = PackageArchive {
                head: root,
                entries: [(root, chosen.library)].into(),
            };
            (Some(archive), Some(locator), len, None)
        }
        Source::Toolchain(id) => {
            if toolchain_major(&id.name) != Some(id.major) {
                return Err(format!("the toolchain does not provide {id}").into());
            }
            (None, None, 0, Some((id.clone(), None)))
        }
        Source::Member(path) => {
            let (id, found) = find_member(workspace, cwd, path)?;
            (
                None,
                None,
                0,
                Some((id.clone(), Some(found.directory.clone()))),
            )
        }
        _ => {
            let capture = acquire(options, workspace, cwd)?;
            fetched_early = capture.fetched;
            chose = capture.chose;
            (Some(capture.archive), capture.locator, capture.len, None)
        }
    };
    checkpoint(Checkpoint::ArchiveCaptured);
    let root = archive.as_ref().map(|archive| archive.head);
    let id = match (&archive, &local) {
        (Some(archive), _) => archive.entries[&archive.head].content.id.clone(),
        (None, Some((id, _))) => id.clone(),
        (None, None) => unreachable!("every source names an archive or a local package"),
    };
    if let (Some(source), Some(root)) = (&locator, root) {
        edit::check_source(workspace, source, root, &id)?;
    }

    // A head with a local name is refused: only a workspace member has one (packages.md section 7).
    if local.is_none() && crate::package_manifest::name::local(&id.name) {
        return Err(format!("{id} has a local name; only a workspace member can have one").into());
    }
    let alias = options
        .alias
        .as_deref()
        .unwrap_or_else(|| hirsel_core_resolve::short_name(&id.name));
    if matches!(options.source, Source::Toolchain(_)) && alias != id.name {
        return Err(format!("a toolchain package must use its bare name {id}").into());
    }
    let prior = member.and_then(|member| member.package.dependencies.get(alias));
    let bound = member.and_then(|member| {
        let bound = member.aliases.get(alias)?;
        member.edges.get_key_value(bound)
    });
    if let Some((name, _)) = bound
        && let Some(member) = member
        && *name != id
    {
        return Err(edit::alias_taken(
            &member.manifest_path,
            "dependencies",
            alias,
            name,
        ));
    }
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
        let entry = shared.dependencies.get(alias);
        let same = match (entry, &edge) {
            (
                Some(Dependency::Published(declared, old_root, old_source, _)),
                Edge::Published(..),
            ) => {
                *declared == id
                    && Some(*old_root) == root
                    && old_source.as_ref().and_then(root_relative) == locator
            }
            (Some(Dependency::Toolchain(declared)), Edge::Toolchain)
            | (Some(Dependency::Member(declared, ..)), Edge::Member) => *declared == id,
            _ => false,
        };
        if let Some(entry) = entry {
            let existing = match entry {
                Dependency::Toolchain(id)
                | Dependency::Member(id, ..)
                | Dependency::Published(id, ..) => id,
                Dependency::Inherited => unreachable!("workspace rows cannot inherit"),
            };
            if *existing != id {
                return Err(edit::alias_taken(
                    &root_manifest,
                    "workspace.dependencies",
                    alias,
                    existing,
                ));
            }
        }
        let authority = |dependency: &Dependency| match dependency {
            Dependency::Published(_, _, _, mask) | Dependency::Member(_, _, mask) => *mask,
            _ => 0,
        };
        let own = prior.filter(|dependency| !matches!(dependency, Dependency::Inherited));
        if let (Some(dependency), Some(member), Some((name, old))) = (own, member, bound)
            && (old != &edge || authority(dependency) != entry.map_or(0, authority))
        {
            return Err(format!(
                "{}: dependencies.{} differs from the workspace binding; edit the local binding before inheriting it
  {} = {}
  {} = {}",
                member.manifest_path.display(),
                alias,
                member.manifest_path.display(),
                edit::spelling(name, old),
                root_manifest.display(),
                edit::spelling(&id, &edge),
            )
            .into());
        }
        let root_package = member.is_some_and(|member| member.directory.as_os_str().is_empty());
        let path = directory.map(|directory| relative_to(Path::new(""), directory));
        let proposed = if same && (!root_package || matches!(prior, Some(Dependency::Inherited))) {
            root_captured.clone()
        } else {
            Captured {
                bytes: edit::shared(
                    &root_captured.bytes,
                    alias,
                    &id,
                    &edit::form(path.as_deref(), root, locator.as_ref()),
                    root_package,
                )?
                .into_bytes()
                .into(),
                identity: root_captured.identity,
            }
        };
        writes.push(ManifestWrite {
            directory: scope,
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
                    bytes: edit::inherit(&before.bytes, alias)?.into_bytes().into(),
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
            Some(Dependency::Published(_, old_root, ..)) if !same => Some(*old_root),
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
        ) && bound.is_some_and(|(_, existing)| *existing == edge);
        if inherited && !unchanged {
            return Err(format!(
                "{}: dependencies.{} inherits the workspace binding; pass --workspace to change its declaration or run hirsel update {alias}",
                member.manifest_path.display(),
                alias
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
                    alias,
                    &id,
                    &edit::form(path.as_deref(), root, declared.as_ref()),
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
        alias: alias.to_owned(),
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
        registry: BTreeMap::new(),
        cache: "complete",
        fetched: fetched_early.into_iter().collect(),
        closure: Vec::new(),
        chose,
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
    if matches!(options.source, Source::Registry(_)) {
        let (findings, authority) =
            registry.check_add(&trial, &id, root, locator.as_ref(), shared.as_ref(), alias)?;
        report.registry = findings;
        if !authority.is_empty() {
            let error = authority.join("\n").into();
            if matches!(&options.source, Source::Registry(query) if query.exact.is_none()) {
                return Ok(Attempt::Denied(
                    root.expect("named registry binding"),
                    error,
                ));
            }
            return Err(error);
        }
    }
    let manifest_path = writes[0].path();
    if options.dry_run {
        let lock = Lock::compare_captured(&trial, Mode::Update, old_lock.as_ref())?;
        report.print(
            options,
            &manifest_path,
            len,
            lock.pending().map(|(_, message)| message),
        );
        return Ok(Attempt::Complete(0));
    }

    if matches!(options.source, Source::Registry(_)) {
        let lock = Lock::compare_captured(&trial, Mode::Update, old_lock.as_ref())?;
        let changes = writes.iter().filter(|w| w.changes());
        if let Some(failure) = crate::package_update::transaction::commit(
            "add",
            cwd,
            scope,
            workspace,
            changes
                .clone()
                .map(|w| (w.directory, &w.before, &w.proposed)),
            old_lock.as_ref(),
            &lock,
        )? {
            return Err(failure.into());
        }
        report.committed.extend(changes.map(ManifestWrite::path));
        if lock.pending().is_some() {
            report.committed.push(LOCK.into());
        }
        report.print(
            options,
            &manifest_path,
            len,
            lock.pending().map(|(_, message)| message),
        );
        return Ok(Attempt::Complete(0));
    }
    for write in writes.iter().filter(|write| write.changes()) {
        report.pending.push(write.path());
    }
    report.pending.push(LOCK.into());
    match commit(cwd, scope, &writes, trial, &mut report, checkpoint) {
        Ok(messages) => {
            let message = messages.join("\nhirsel: ");
            report.print(
                options,
                &manifest_path,
                len,
                (!messages.is_empty()).then_some(message.as_str()),
            );
            Ok(Attempt::Complete(0))
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
                report.print(options, &manifest_path, len, None);
            }
            Ok(Attempt::Complete(1))
        }
    }
}
