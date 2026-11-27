//! What a commit yields (GIT-2): the package at `dir`, its workspace found inside the commit,
//! the members it reaches read from the snapshot, and the bundle `hirsel pack` writes for
//! it, packed by the resolver's own encoding and closure and verified before anything is kept.

use super::snapshot::{Dir, Missed, Snapshot};
use super::transport::{self, Remote};
use super::{Budget, Failure, Session};
use crate::package_content::{
    ARCHIVE_LIMIT_MIB, ArchiveRow, Content, ForeignCode, ForeignModule, Id, PackageArchive, Root,
    Source,
};
use crate::package_dir::MANIFEST;
use crate::package_manifest::{
    Dependency, GitSource, Manifest, Package, Selections, SourceLocation,
};
use crate::package_resolve::{Libraries, Members, pack_members};
use crate::package_store::{PackageStore, decode, fetch_https};
use crate::package_workspace::{Edge, Member, archive_path, cycles};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A verified bundle that nothing has kept yet, and every source contacted for it.
pub(crate) struct Acquired {
    pub(crate) archive: PackageArchive,
    pub(crate) fetched: Vec<String>,
}

/// The pack of commit `git.rev`, from this command's session or fetched once.
fn commit_pack(
    store: &PackageStore,
    session: &Session,
    budget: &mut Budget,
    git: &GitSource,
    remote: Option<&Remote>,
) -> Result<Arc<super::pack::Pack>, Failure> {
    if let Some(pack) = session.get(git) {
        return Ok(pack);
    }
    budget.fetch()?;
    let pack = match remote {
        Some(remote) => transport::fetch(remote, &git.rev, budget)?,
        None => {
            let remote = transport::connect(&store.transport, &git.url, budget)?;
            transport::fetch(&remote, &git.rev, budget)?
        }
    };
    let pack = Arc::new(pack);
    session.put(git, pack.clone());
    Ok(pack)
}

/// Packs `git` for the binding `id` pinned to `pin` and verifies the bundle (GIT-2 to GIT-6).
/// Nothing is kept: the caller admits the bundle once the outermost source has passed.
pub(crate) fn pack_commit(
    store: &PackageStore,
    session: &Session,
    budget: &mut Budget,
    id: &Id,
    pin: Root,
    git: &GitSource,
) -> Result<Acquired, String> {
    let (_, acquired) = pack_listed(store, session, budget, Some(id), Some(pin), git, None)?;
    Ok(acquired)
}

/// `pack_commit` for `hirsel add`: the id is the package's own when `expected` is `None`, the
/// remote its reference listing opened is reused, and with no pin the head's own root is
/// taken and returned.
pub(crate) fn pack_listed(
    store: &PackageStore,
    session: &Session,
    budget: &mut Budget,
    expected: Option<&Id>,
    pin: Option<Root>,
    git: &GitSource,
    remote: Option<&Remote>,
) -> Result<(Root, Acquired), String> {
    let spelling = SourceLocation::Git(git.clone()).to_string();
    let who = expected.map_or_else(String::new, |id| format!("{id}: "));
    let explain = |failure: Failure| failure.explain_after(&who, &spelling);
    let pack = commit_pack(store, session, budget, git, remote).map_err(explain)?;
    if let Some(named) = &git.reference
        && let Some((kind, _)) =
            pack.header(&super::Oid::parse(&git.rev).expect("rev is a commit id"))
        && kind != super::pack::Kind::Commit
    {
        return Err(format!(
            "{}: {named} names a {}, not a commit",
            git.url,
            kind.name()
        ));
    }
    let snapshot = Snapshot::open(pack, &git.rev).map_err(explain)?;
    let graph = Graph {
        snapshot: &snapshot,
        commit: git.commit(),
        expected,
        who: &who,
        spelling: &spelling,
    };
    let (root_dir, members, selections, id) = graph.read(git.dir.as_deref())?;
    let id = &id;
    if budget.outer.is_empty() {
        budget.outer = format!("{id}: {spelling}");
    }
    let mut fetched = Vec::new();
    let mut fetch = |binder: &Id, root: Root, source: &SourceLocation| {
        let source_text = source.to_string();
        let libraries: Libraries = match source {
            SourceLocation::Archive(path) => {
                let bytes = snapshot
                    .file(&root_dir, &root_dir, path)
                    .map_err(|missed| {
                        graph_missed(
                            missed,
                            &format!("{binder}: {source}"),
                            "workspace",
                            "a regular file",
                            &format!("{binder}: "),
                            &source_text,
                        )
                    })?;
                decode(binder, root, &source_text, bytes.to_vec())?.entries
            }
            SourceLocation::Url(url) => {
                budget
                    .fetch()
                    .map_err(|failure| failure.explain(binder, &source_text))?;
                let (archive, _) = fetch_https(
                    &store.transport,
                    url,
                    &mut |n| budget.receive(n).map_err(|failure| failure.to_string()),
                    &format!("{binder}: "),
                )?;
                archive.verify(binder, root, &source_text)?;
                fetched.push(source_text.clone());
                archive.entries
            }
            SourceLocation::Git(nested) => {
                let acquired = pack_commit(store, session, budget, binder, root, nested)?;
                fetched.push(source_text.clone());
                fetched.extend(acquired.fetched);
                acquired.archive.entries
            }
        };
        Ok(libraries)
    };
    let (_, archive) = pack_members(
        &members,
        store,
        &selections,
        &graph.manifest_path(&root_dir),
        &mut fetch,
        id,
    )
    .map_err(|error| {
        let text = error.to_string();
        if text.starts_with("over the limit of") && text.ends_with("bytes") {
            format!("{id}: {spelling}: over the limit of {ARCHIVE_LIMIT_MIB} MiB")
        } else {
            text
        }
    })?;
    let pin = pin.unwrap_or(archive.head);
    archive.verify(id, pin, &spelling)?;
    Ok((pin, Acquired { archive, fetched }))
}

fn graph_missed(
    missed: Missed,
    prefix: &str,
    boundary: &str,
    object: &str,
    who: &str,
    spelling: &str,
) -> String {
    match missed.explain(boundary, object) {
        Ok(text) => format!("{prefix} {text}"),
        Err(failure) => failure.explain_after(who, spelling),
    }
}

struct Graph<'s> {
    snapshot: &'s Snapshot,
    commit: String,
    /// The id the binding declares, `None` for `hirsel add`, which takes the package's own.
    expected: Option<&'s Id>,
    /// `<id>: ` in front of a refusal, or nothing for `hirsel add`.
    who: &'s str,
    spelling: &'s str,
}

/// One reached member before its files are read.
struct Draft {
    dir: Dir,
    relative: String,
    manifest_path: PathBuf,
    package: Package,
}

impl Graph<'_> {
    fn manifest_path(&self, dir: &Dir) -> PathBuf {
        if dir.names.is_empty() {
            PathBuf::from(format!("{}/{MANIFEST}", self.commit))
        } else {
            PathBuf::from(format!("{}/{}/{MANIFEST}", self.commit, dir.path()))
        }
    }

    fn missed(&self, missed: Missed, prefix: &str, boundary: &str, object: &str) -> String {
        graph_missed(missed, prefix, boundary, object, self.who, self.spelling)
    }

    /// The manifest bytes in `dir`, read within `dir` as section 3 reads one.
    fn manifest_bytes(&self, dir: &Dir) -> Result<Option<Arc<Vec<u8>>>, String> {
        let path = self.manifest_path(dir);
        self.snapshot.manifest(dir).map_err(|missed| {
            self.missed(
                missed,
                &format!("{}:", path.display()),
                "package",
                "a regular file",
            )
        })
    }

    /// GIT-2 steps 3 and 4: the package, its workspace root and the members it reaches.
    /// Returns the workspace root and the reached members, each read as section 4 reads one.
    fn read(&self, dir: Option<&str>) -> Result<(Dir, Members, Selections, Id), String> {
        let (who, spelling) = (self.who, self.spelling);
        let top = self.snapshot.top();
        let no_manifest = || format!("{who}{spelling} has no hirsel.toml");
        let package_dir = match dir {
            None => top.clone(),
            Some(dir) => match self.snapshot.dir(&top, &top, dir) {
                Ok(found) => found,
                Err(Missed::Missing | Missed::WrongType) => return Err(no_manifest()),
                Err(missed) => {
                    return Err(self.missed(
                        missed,
                        &format!("{who}{spelling}: dir {dir:?}"),
                        "commit",
                        "a directory",
                    ));
                }
            },
        };
        let package_path = self.manifest_path(&package_dir);
        let bytes = self.manifest_bytes(&package_dir)?.ok_or_else(no_manifest)?;
        let (workspace, manifest) = Manifest::classify(&bytes, &package_path);
        let manifest = manifest?;
        let Some(package) = &manifest.package else {
            return Err(format!(
                "{who}{spelling} is a workspace root, not a package"
            ));
        };
        if let Some(id) = self.expected
            && package.id != *id
        {
            return Err(format!("{who}{spelling} is {}", package.id));
        }
        let id = package.id.clone();

        // Section 3 with the top of the tree as the highest directory searched.
        let mut found = workspace.then(|| package_dir.clone());
        if found.is_none() {
            for depth in (0..package_dir.names.len()).rev() {
                let above = package_dir.ancestor(depth);
                if let Some(bytes) = self.manifest_bytes(&above)? {
                    let path = self.manifest_path(&above);
                    let (workspace, parsed) = Manifest::classify(&bytes, &path);
                    parsed?;
                    if workspace {
                        found = Some(above);
                        break;
                    }
                }
            }
        }
        let root = found.unwrap_or_else(|| package_dir.clone());
        let root_path = self.manifest_path(&root);
        let mut root_manifest = if root == package_dir {
            manifest
        } else {
            let bytes = self
                .manifest_bytes(&root)?
                .expect("the root holds a manifest");
            Manifest::parse(&bytes, &root_path)?
        };
        let mut root_package = root_manifest.package.take();
        let root_id = root_package.as_ref().map(|package| package.id.clone());
        let relative = |dir: &Dir| dir.names[root.names.len()..].join("/");

        // The listed members, walked but not read.
        let config = root_manifest.workspace.as_ref();
        let mut listed: Vec<(String, Result<Dir, String>)> = Vec::new();
        for entry in config.map(|config| &config.members[..]).unwrap_or(&[]) {
            let walked = self.snapshot.dir(&root, &root, entry).map_err(|missed| {
                self.missed(
                    missed,
                    &format!("{}: workspace.members: {entry:?}", root_path.display()),
                    "workspace",
                    "a directory",
                )
            });
            let walked = match walked {
                Ok(dir)
                    if dir == root
                        || listed
                            .iter()
                            .any(|(_, seen)| seen.as_ref().is_ok_and(|seen| *seen == dir)) =>
                {
                    Err(format!(
                        "{}: workspace.members: {entry:?} is listed twice",
                        root_path.display()
                    ))
                }
                other => other,
            };
            listed.push((entry.clone(), walked));
        }
        // The member a directory is, by identity, or the refusal of a listed spelling.
        let member_at =
            |target: Option<&Dir>, lexical: Option<String>| -> Result<Option<String>, String> {
                if let Some(target) = target {
                    if *target == root && root_id.is_some() {
                        return Ok(Some(String::new()));
                    }
                    if let Some((entry, _)) = listed
                        .iter()
                        .find(|(_, dir)| dir.as_ref().is_ok_and(|dir| dir == target))
                    {
                        return Ok(Some(entry.clone()));
                    }
                }
                for (entry, walked) in &listed {
                    if let Err(refusal) = walked
                        && archive_path(Path::new(""), entry) == lexical
                    {
                        return Err(refusal.clone());
                    }
                }
                Ok(None)
            };
        if root != package_dir {
            let lexical = Some(relative(&package_dir));
            if member_at(Some(&package_dir), lexical)?.is_none() {
                return Err(format!(
                    "{} is inside the workspace at {} but is not one of its members",
                    package_path.display(),
                    root_path.display()
                ));
            }
        }

        // The reached set, from the package along `path` bindings.
        let mut drafts: BTreeMap<Id, Draft> = BTreeMap::new();
        let mut edges_by_member = BTreeMap::new();
        let mut aliases_by_member = BTreeMap::new();
        let mut queue = VecDeque::from([package_dir.clone()]);
        let mut reached: Vec<Dir> = Vec::new();
        while let Some(dir) = queue.pop_front() {
            if reached.contains(&dir) {
                continue;
            }
            reached.push(dir.clone());
            let manifest_path = self.manifest_path(&dir);
            let entry = if dir == root {
                String::new()
            } else {
                member_at(Some(&dir), None)?.unwrap_or_default()
            };
            let package = if dir == root {
                root_package.take()
            } else {
                let bytes = self
                    .manifest_bytes(&dir)?
                    .ok_or_else(|| format!("{}: does not exist", manifest_path.display()))?;
                let manifest = Manifest::parse(&bytes, &manifest_path)?;
                if manifest.selections.is_some() {
                    return Err(format!(
                        "{}: selections: only the workspace root's manifest selects",
                        manifest_path.display()
                    ));
                }
                if manifest.workspace.is_some() {
                    return Err(format!(
                        "{}: workspace.members: {entry:?} belongs to the workspace at {}",
                        root_path.display(),
                        manifest_path.display()
                    ));
                }
                for depth in (root.names.len() + 1..dir.names.len()).rev() {
                    let above = dir.ancestor(depth);
                    if let Some(bytes) = self.manifest_bytes(&above)? {
                        let path = self.manifest_path(&above);
                        if Manifest::parse(&bytes, &path)?.workspace.is_some() {
                            return Err(format!(
                                "{}: workspace.members: {entry:?} belongs to the workspace at {}",
                                root_path.display(),
                                path.display()
                            ));
                        }
                    }
                }
                manifest.package
            };
            let package = package
                .ok_or_else(|| format!("{}: expected [package]", manifest_path.display()))?;
            if drafts.contains_key(&package.id) {
                return Err(format!(
                    "{}: workspace.members: {entry:?} repeats {}",
                    root_path.display(),
                    package.id
                ));
            }
            if package.inherit_charge && config.and_then(|config| config.charge.as_ref()).is_none()
            {
                return Err(format!(
                    "{}: package.charge: the workspace declares no charge",
                    manifest_path.display()
                ));
            }
            let from = relative(&dir);
            let mut edges = BTreeMap::new();
            let mut aliases = BTreeMap::new();
            let mut targets = Vec::new();
            for (name, dependency) in &package.dependencies {
                let (dependency, from, start) = match dependency {
                    Dependency::Inherited => (
                        config
                            .and_then(|config| config.dependencies.get(name))
                            .ok_or_else(|| {
                                format!(
                                    "{}: dependencies.{name}: the workspace declares no {name}",
                                    manifest_path.display()
                                )
                            })?,
                        String::new(),
                        &root,
                    ),
                    other => (other, from.clone(), &dir),
                };
                let (expected, edge) = match dependency {
                    Dependency::Inherited => unreachable!(),
                    Dependency::Toolchain(id) => (id, Edge::Toolchain),
                    Dependency::Member(member, path) => {
                        let target = self.snapshot.dir(&root, start, path).ok();
                        let lexical = archive_path(Path::new(&from), path);
                        let not_member = || {
                            format!(
                                "{}: dependencies.{name}: {path} is not a member of the workspace at {}",
                                manifest_path.display(),
                                root_path.display()
                            )
                        };
                        member_at(target.as_ref(), lexical)?.ok_or_else(not_member)?;
                        let target = target.ok_or_else(not_member)?;
                        let found = match &root_id {
                            Some(root_id) if target == root => root_id.clone(),
                            _ => self.member_id(&target)?,
                        };
                        if *member != found {
                            return Err(format!(
                                "{}: dependencies.{name}: {path} is {found}, not {member}",
                                manifest_path.display()
                            ));
                        }
                        targets.push(target);
                        (member, Edge::Member)
                    }
                    Dependency::Published(id, pin, source) => {
                        let source = match source {
                            Some(SourceLocation::Archive(path)) => {
                                let path = archive_path(Path::new(&from), path).ok_or_else(|| format!(
                                    "{}: dependencies.{name}: archive {path:?} is not inside the workspace root. To share an archive across packages, add a parent hirsel.toml with [workspace] and members listing the package directories, then use a relative path within that workspace",
                                    manifest_path.display()
                                ))?;
                                Some(SourceLocation::Archive(path))
                            }
                            other => other.clone(),
                        };
                        (id, Edge::Published(*pin, source))
                    }
                };
                if edges
                    .insert(expected.clone(), edge.clone())
                    .is_some_and(|earlier| earlier != edge)
                {
                    return Err(format!(
                        "{}: dependencies.{name}: bindings of {expected} differ",
                        manifest_path.display()
                    ));
                }
                aliases.insert(name.clone(), expected.clone());
            }
            queue.extend(targets);
            edges_by_member.insert(package.id.clone(), edges);
            aliases_by_member.insert(package.id.clone(), aliases);
            drafts.insert(
                package.id.clone(),
                Draft {
                    dir: dir.clone(),
                    relative: from,
                    manifest_path,
                    package,
                },
            );
        }
        cycles(&edges_by_member)?;

        // GIT-2 step 5's inputs: each reached member's listed files, read from the snapshot.
        let mut members = BTreeMap::new();
        for (member_id, draft) in drafts {
            let read = |file: &str, field: &str| -> Result<Arc<[u8]>, String> {
                let file_bytes = self.snapshot.file(&draft.dir, &draft.dir, file);
                file_bytes
                    .map(|bytes| Arc::from(bytes.as_slice()))
                    .map_err(|missed| {
                        self.missed(
                            missed,
                            &format!("{}: {field} {file:?}", draft.manifest_path.display()),
                            "package",
                            "a regular file",
                        )
                    })
            };
            let mut files = BTreeMap::new();
            for (file, module) in &draft.package.sources {
                files.insert(
                    file.clone(),
                    Source {
                        module: module.clone(),
                        bytes: read(file, "sources:")?,
                    },
                );
            }
            let mut foreign = BTreeMap::new();
            for (module, spec) in &draft.package.foreign {
                let field = format!("foreign.{}", module.join("."));
                let mut row = |row: &crate::package_manifest::Archive, what: &str| {
                    Ok::<_, String>(ArchiveRow {
                        path: row.file.clone(),
                        pin: row.pin,
                        bytes: read(&row.file, &format!("{field}: {what}"))?,
                    })
                };
                let code = match &spec.code {
                    crate::package_manifest::Code::Native(rows) => ForeignCode::Native(
                        rows.iter()
                            .map(|(target, archive)| Ok((target.clone(), row(archive, "archive")?)))
                            .collect::<Result<_, String>>()?,
                    ),
                    crate::package_manifest::Code::Wasm(module) => {
                        ForeignCode::Wasm(row(module, "module")?)
                    }
                };
                foreign.insert(
                    module.clone(),
                    ForeignModule {
                        symbols: spec.symbols.clone(),
                        code,
                    },
                );
            }
            let content = Content {
                id: member_id.clone(),
                version: draft.package.version.clone(),
                bindings: BTreeMap::new(),
                files,
                foreign,
            };
            let edges = edges_by_member.remove(&member_id).unwrap();
            let aliases = aliases_by_member.remove(&member_id).unwrap();
            members.insert(
                member_id,
                Member {
                    directory: PathBuf::from(draft.relative),
                    manifest_path: draft.manifest_path,
                    package: draft.package,
                    edges,
                    aliases,
                    content,
                    tests: BTreeMap::new(),
                },
            );
        }
        let selections = root_manifest.selections.unwrap_or_default();
        if let Some(id) = selections.keys().find(|id| members.contains_key(*id)) {
            return Err(format!(
                "{}: selections: {id} is a member of this workspace",
                root_path.display()
            ));
        }
        Ok((root, members, selections, id))
    }

    /// The package id the manifest in member directory `dir` declares.
    fn member_id(&self, dir: &Dir) -> Result<Id, String> {
        let path = self.manifest_path(dir);
        let bytes = self
            .manifest_bytes(dir)?
            .ok_or_else(|| format!("{}: does not exist", path.display()))?;
        Manifest::parse(&bytes, &path)?
            .package
            .map(|package| package.id)
            .ok_or_else(|| format!("{}: expected [package]", path.display()))
    }
}
