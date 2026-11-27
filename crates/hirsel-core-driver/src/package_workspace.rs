use crate::package_content::{
    ARCHIVE_LIMIT, ArchiveRow, Content, ForeignCode, ForeignModule, Id, Root, Source,
    archive_digest,
};
use crate::package_dir::MANIFEST;
use crate::package_files::{Captured, FileId, WalkError, open_directory, walk};
use crate::package_manifest::{Dependency, Manifest, Package, SourceLocation};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq)]
pub(crate) enum Edge {
    Toolchain,
    Member,
    Published(Root, Option<SourceLocation>),
}

pub(crate) struct Member {
    pub(crate) directory: PathBuf,
    pub(crate) manifest_path: PathBuf,
    pub(crate) package: Package,
    pub(crate) edges: BTreeMap<Id, Edge>,
    /// Per-consumer policy, outside the content graph and its keys.
    pub(crate) ceilings: BTreeMap<String, u16>,
    /// Each dependency's alias, after inherited declarations are expanded.
    pub(crate) aliases: BTreeMap<String, Id>,
    pub(crate) content: Content,
    pub(crate) tests: BTreeMap<String, Source>,
}

pub(crate) struct Workspace {
    pub(crate) root: File,
    pub(crate) directory: PathBuf,
    pub(crate) member: Option<Id>,
    pub(crate) members: BTreeMap<Id, Member>,
    pub(crate) selections: crate::package_manifest::Selections,
    inputs: BTreeSet<FileId>,
    manifests: Manifests,
}

struct Discovered {
    dir: File,
    path: PathBuf,
    manifest: Result<Manifest, String>,
}

fn parent(directory: &File) -> io::Result<File> {
    // This ascent discovers workspace ownership; it is not a confined walk.
    crate::host::open_parent(directory)
}

pub(crate) fn identity(directory: &File) -> io::Result<FileId> {
    directory.metadata().map(|metadata| FileId::of(&metadata))
}

fn file_error(path: &Path, field: &str, file: &str, error: &WalkError) -> String {
    format!(
        "{}: {field} {file:?} {}",
        path.display(),
        error.explain("package", "a regular file")
    )
}

type Manifests = BTreeMap<FileId, Option<Captured>>;

fn capture_manifest(
    manifests: &mut Manifests,
    directory: &File,
    path: &Path,
) -> Result<Option<Captured>, Box<dyn std::error::Error>> {
    let id = identity(directory)?;
    Ok(match manifests.entry(id) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.get().clone(),
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(read_manifest(directory, path)?).clone()
        }
    })
}

pub(crate) fn read_manifest(dir: &File, path: &Path) -> Result<Option<Captured>, String> {
    let manifest = path.join(MANIFEST);
    let file = match walk(dir, Path::new(MANIFEST)) {
        Ok(file) => file,
        Err(WalkError::Missing) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{}: {}",
                manifest.display(),
                error.explain("package", "a regular file")
            ));
        }
    };
    file.read(ARCHIVE_LIMIT).map(Some).map_err(|error| {
        format!(
            "{}: {}",
            manifest.display(),
            error.explain("package", "a regular file")
        )
    })
}

fn ancestor_spelling(path: &Path) -> PathBuf {
    if path.ends_with("..") {
        return path.join("..");
    }
    if path == Path::new(".") {
        return PathBuf::from("..");
    }
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
        Some(parent) if parent != path => parent.to_path_buf(),
        _ => path.join(".."),
    }
}

fn discover(
    dir: &Path,
    manifests: &mut Manifests,
) -> Result<Option<(Discovered, Discovered)>, Box<dyn std::error::Error>> {
    let mut directory = open_directory(dir)?;
    let mut path = dir.to_path_buf();
    let mut member: Option<Discovered> = None;
    let first;
    loop {
        if let Some(captured) = capture_manifest(manifests, &directory, &path)? {
            let (workspace, manifest) = Manifest::classify(&captured.bytes, &path.join(MANIFEST));
            if !workspace && member.is_some() {
                manifest?;
            } else {
                let discovered = Discovered {
                    dir: directory.try_clone()?,
                    path: path.clone(),
                    manifest,
                };
                if workspace {
                    first = discovered;
                    break;
                }
                member = Some(discovered);
            }
        }
        let above = parent(&directory)?;
        if identity(&above)? == identity(&directory)? {
            let Some(member) = member else {
                return Ok(None);
            };
            let same = Discovered {
                dir: member.dir.try_clone()?,
                path: member.path.clone(),
                manifest: Ok(Manifest {
                    selections: None,
                    package: None,
                    workspace: None,
                }),
            };
            return Ok(Some((same, member)));
        }
        directory = above;
        path = ancestor_spelling(&path);
    }
    let requested = match member {
        Some(member) => member,
        None => Discovered {
            dir: first.dir.try_clone()?,
            path: first.path.clone(),
            manifest: Ok(Manifest {
                selections: None,
                package: None,
                workspace: None,
            }),
        },
    };
    Ok(Some((first, requested)))
}

impl Workspace {
    pub(crate) fn discover(dir: &Path) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        Self::discover_for(dir, true)
    }

    pub(crate) fn discover_for(
        dir: &Path,
        needs_package: bool,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        Self::discover_with(dir, needs_package, BTreeMap::new())
    }

    pub(crate) fn scope(dir: &Path) -> Result<File, Box<dyn std::error::Error>> {
        let (root, _) = discover(dir, &mut BTreeMap::new())?
            .ok_or("no package manifest here; pass --workspace to declare it for the workspace")?;
        Ok(root.dir)
    }

    pub(crate) fn with_manifests(
        &self,
        dir: &Path,
        edits: &[(&File, &Captured)],
        needs_package: bool,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let mut manifests = self.manifests.clone();
        for (target, manifest) in edits {
            manifests.insert(identity(target)?, Some((*manifest).clone()));
        }
        Self::discover_with(dir, needs_package, manifests)
    }

    fn discover_with(
        dir: &Path,
        needs_package: bool,
        mut manifests: Manifests,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Some((root, selected)) = discover(dir, &mut manifests)? else {
            return Ok(None);
        };
        let root_id = identity(&root.dir)?;
        let selected_id = identity(&selected.dir)?;
        let root_manifest = root.path.join(MANIFEST);
        let selected_manifest = selected.path.join(MANIFEST);
        let mut root_config = root.manifest?;
        let mut selected_config = Some(selected.manifest);
        if root_id == selected_id {
            let same = selected_config.take().unwrap()?;
            root_config.selections = root_config.selections.or(same.selections);
            root_config.package = root_config.package.or(same.package);
        }
        if needs_package && root_id == selected_id && root_config.package.is_none() {
            return Err(format!(
                "{} is a workspace root, not a package; name a member directory",
                root_manifest.display()
            )
            .into());
        }
        let mut inputs = BTreeSet::new();
        let mut opened = BTreeMap::new();
        let mut drafts = BTreeMap::new();

        let workspace = root_config.workspace;
        let package = root_config.package;
        if let Some(package) = package {
            opened.insert(root_id, package.id.clone());
            drafts.insert(
                package.id.clone(),
                (
                    root.dir.try_clone()?,
                    PathBuf::new(),
                    root_manifest.clone(),
                    package,
                ),
            );
        }
        if let Some(config) = &workspace {
            let mut refusals = Vec::new();
            let mut listed_ids = BTreeSet::from([root_id]);
            for listed in &config.members {
                let result = (|| -> Result<_, Box<dyn std::error::Error>> {
                    let opened_member = walk(&root.dir, Path::new(listed)).map_err(|error| {
                        format!(
                            "{}: workspace.members: {listed:?} {}",
                            root_manifest.display(),
                            error.explain("workspace", "a directory")
                        )
                    })?;
                    let relative = opened_member.path.clone();
                    if relative.to_str().is_none() {
                        return Err(format!(
                        "{}: workspace.members: {listed:?} resolves to a non-UTF-8 directory name",
                        root_manifest.display()
                    )
                    .into());
                    }
                    let dir = opened_member.directory().map_err(|error| {
                        format!(
                            "{}: workspace.members: {listed:?} {}",
                            root_manifest.display(),
                            error.explain("workspace", "a directory")
                        )
                    })?;
                    let dir_id = identity(&dir)?;
                    if !listed_ids.insert(dir_id) {
                        return Err(format!(
                            "{}: workspace.members: {listed:?} is listed twice",
                            root_manifest.display()
                        )
                        .into());
                    }
                    let path = root.path.join(listed);
                    let manifest_path = path.join(MANIFEST);
                    let manifest = if dir_id == selected_id
                        && let Some(manifest) = selected_config.take()
                    {
                        manifest?
                    } else {
                        let captured =
                            capture_manifest(&mut manifests, &dir, &path)?.ok_or_else(|| {
                                format!("{}: does not exist", manifest_path.display())
                            })?;
                        Manifest::parse(&captured.bytes, &manifest_path)?
                    };
                    if manifest.selections.is_some() {
                        return Err(format!(
                            "{}: selections: only the workspace root's manifest selects",
                            manifest_path.display()
                        )
                        .into());
                    }
                    if manifest.workspace.is_some() {
                        return Err(format!(
                            "{}: workspace.members: {listed:?} belongs to the workspace at {}",
                            root_manifest.display(),
                            manifest_path.display()
                        )
                        .into());
                    }
                    let mut ancestor_path = relative.parent();
                    let mut ancestor = parent(&dir)?;
                    loop {
                        if identity(&ancestor)? == root_id {
                            break;
                        }
                        let Some(relative) = ancestor_path else {
                            return Err(format!(
                                "{}: workspace.members: {listed:?} resolves outside the workspace",
                                root_manifest.display()
                            )
                            .into());
                        };
                        let at = root.path.join(relative);
                        if let Some(captured) = capture_manifest(&mut manifests, &ancestor, &at)? {
                            if Manifest::parse(&captured.bytes, &at.join(MANIFEST))?
                                .workspace
                                .is_some()
                            {
                                return Err(format!(
                                "{}: workspace.members: {listed:?} belongs to the workspace at {}",
                                root_manifest.display(),
                                at.join(MANIFEST).display()
                            )
                            .into());
                            }
                        }
                        let above = parent(&ancestor)?;
                        if identity(&above)? == identity(&ancestor)? {
                            return Err(format!(
                                "{}: workspace.members: {listed:?} resolves outside the workspace",
                                root_manifest.display()
                            )
                            .into());
                        }
                        ancestor_path = relative.parent();
                        ancestor = above;
                    }
                    let package = manifest.package.ok_or_else(|| {
                        format!("{}: expected [package]", manifest_path.display())
                    })?;
                    if drafts.contains_key(&package.id) {
                        return Err(format!(
                            "{}: workspace.members: {listed:?} repeats {}",
                            root_manifest.display(),
                            package.id
                        )
                        .into());
                    }
                    Ok((dir_id, dir, relative, manifest_path, package))
                })();
                match result {
                    Ok((dir_id, dir, relative, manifest_path, package)) => {
                        opened.insert(dir_id, package.id.clone());
                        drafts.insert(package.id.clone(), (dir, relative, manifest_path, package));
                    }
                    Err(error) => refusals.push(error.to_string()),
                }
            }
            if let Some(Err(error)) = selected_config {
                refusals.push(error);
            }
            if !refusals.is_empty() {
                return Err(refusals.join("\nhirsel: ").into());
            }
        }
        let member = opened.get(&selected_id).cloned();
        if member.is_none() && root_id != selected_id {
            return Err(format!(
                "{} is inside the workspace at {} but is not one of its members",
                selected_manifest.display(),
                root_manifest.display()
            )
            .into());
        }
        let selections = root_config.selections.unwrap_or_default();
        if let Some(id) = selections.keys().find(|id| drafts.contains_key(*id)) {
            return Err(format!(
                "{}: selections: {id} is a member of this workspace",
                root_manifest.display()
            )
            .into());
        }
        let mut edges_by_member = BTreeMap::new();
        let mut policies = BTreeMap::new();
        for (id, (_, relative, manifest, package)) in &drafts {
            if package.inherit_charge
                && workspace
                    .as_ref()
                    .and_then(|config| config.charge.as_ref())
                    .is_none()
            {
                return Err(format!(
                    "{}: package.charge: the workspace declares no charge",
                    manifest.display()
                )
                .into());
            }
            let mut edges = BTreeMap::new();
            let mut aliases = BTreeMap::new();
            let mut ceilings = BTreeMap::new();
            for (name, dependency) in &package.dependencies {
                let (dependency, from) = match dependency {
                    Dependency::Inherited => (
                        workspace
                            .as_ref()
                            .and_then(|config| config.dependencies.get(name))
                            .ok_or_else(|| {
                                format!(
                                    "{}: dependencies.{name}: the workspace declares no {name}",
                                    manifest.display()
                                )
                            })?,
                        Path::new(""),
                    ),
                    other => (other, relative.as_path()),
                };
                let (expected, edge) = match dependency {
                    Dependency::Inherited => unreachable!(),
                    Dependency::Toolchain(id) => (id, Edge::Toolchain),
                    Dependency::Member(id, path, ceiling) => {
                        ceilings.insert(name.clone(), *ceiling);
                        let found = if Path::new(path).is_absolute() {
                            None
                        } else {
                            walk(&root.dir, &from.join(path))
                                .ok()
                                .and_then(|opened| opened.directory().ok())
                                .and_then(|file| identity(&file).ok())
                                .and_then(|id| opened.get(&id))
                        };
                        let found = found.ok_or_else(|| format!("{}: dependencies.{name}: {path} is not a member of the workspace at {}", manifest.display(), root_manifest.display()))?;
                        if id != found {
                            return Err(format!(
                                "{}: dependencies.{name}: {path} is {found}, not {id}",
                                manifest.display()
                            )
                            .into());
                        }
                        (id, Edge::Member)
                    }
                    Dependency::Published(id, root, source, ceiling) => {
                        ceilings.insert(name.clone(), *ceiling);
                        let source = match source {
                            Some(SourceLocation::Archive(path)) => {
                                let path = archive_path(from, path).ok_or_else(|| format!(
                                    "{}: dependencies.{name}: archive {path:?} is not inside the workspace root. To share an archive across packages, add a parent hirsel.toml with [workspace] and members listing the package directories, then use a relative path within that workspace",
                                    manifest.display()
                                ))?;
                                Some(SourceLocation::Archive(path))
                            }
                            other => other.clone(),
                        };
                        (id, Edge::Published(*root, source))
                    }
                };
                if edges
                    .insert(expected.clone(), edge.clone())
                    .is_some_and(|earlier| earlier != edge)
                {
                    return Err(format!(
                        "{}: dependencies.{name}: bindings of {expected} differ",
                        manifest.display()
                    )
                    .into());
                }
                aliases.insert(name.clone(), expected.clone());
            }
            edges_by_member.insert(id.clone(), edges);
            policies.insert(id.clone(), (aliases, ceilings));
        }
        cycles(&edges_by_member)?;
        let mut members = BTreeMap::new();
        for (id, (dir, directory, manifest_path, package)) in drafts {
            let mut read = |file: &str, field: &str| {
                let result = walk(&dir, Path::new(file))
                    .and_then(|file| file.read(ARCHIVE_LIMIT))
                    .map_err(|error| file_error(&manifest_path, field, file, &error))?;
                inputs.insert(result.identity);
                Ok::<_, String>(result.bytes)
            };
            let mut sources = |listed: &BTreeMap<String, Vec<String>>, field| {
                listed
                    .iter()
                    .map(|(file, module)| {
                        Ok((
                            file.clone(),
                            Source {
                                module: module.clone(),
                                bytes: read(file, field)?,
                            },
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, String>>()
            };
            let files = sources(&package.sources, "sources:")?;
            let tests = sources(&package.test_sources, "tests.sources:")?;
            let mut foreign = BTreeMap::new();
            for (module, spec) in &package.foreign {
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
                id: id.clone(),
                version: package.version.clone(),
                bindings: BTreeMap::new(),
                files,
                foreign,
            };
            let edges = edges_by_member.remove(&id).unwrap();
            let (aliases, ceilings) = policies.remove(&id).unwrap();
            members.insert(
                id,
                Member {
                    directory,
                    manifest_path,
                    package,
                    edges,
                    aliases,
                    ceilings,
                    content,
                    tests,
                },
            );
        }
        Ok(Some(Self {
            root: root.dir,
            directory: root.path,
            member,
            members,
            selections,
            inputs,
            manifests,
        }))
    }

    pub(crate) fn input_alias(&self, identity: FileId) -> bool {
        self.inputs.contains(&identity)
            || self
                .manifests
                .values()
                .flatten()
                .any(|manifest| manifest.identity == identity)
    }

    pub(crate) fn captured_manifest(&self, directory: &File) -> io::Result<Option<&Captured>> {
        Ok(self
            .manifests
            .get(&identity(directory)?)
            .and_then(Option::as_ref))
    }

    pub(crate) fn record_input(&mut self, identity: FileId) {
        self.inputs.insert(identity);
    }

    /// The selected member, which `pack` publishes with every member it reaches: a local
    /// name never is (packages.md sections 1 and 6, `pack, local`).
    pub(crate) fn packed(&self) -> Result<&Id, String> {
        let selected = self.selected()?;
        let mut reached = BTreeSet::from([selected]);
        let mut pending = vec![selected];
        while let Some(id) = pending.pop() {
            if crate::package_manifest::name::local(&id.name) {
                return Err(format!(
                    "{}: {} is a local name and is never packed; give it a scoped name to publish it",
                    self.members[id].manifest_path.display(),
                    id.name
                ));
            }
            for (next, edge) in &self.members[id].edges {
                if *edge == Edge::Member && reached.insert(next) {
                    pending.push(next);
                }
            }
        }
        Ok(selected)
    }

    pub(crate) fn selected(&self) -> Result<&Id, String> {
        self.member.as_ref().ok_or_else(|| {
            format!(
                "{} is a workspace root, not a package; name a member directory",
                self.directory.join(MANIFEST).display()
            )
        })
    }
}

impl Member {
    pub(crate) fn check_pins(&self) -> Result<(), String> {
        for (module, foreign) in &self.content.foreign {
            let rows: Vec<(String, &ArchiveRow)> = match &foreign.code {
                ForeignCode::Native(archives) => archives
                    .iter()
                    .map(|(target, row)| (format!("archive {:?} for {target}", row.path), row))
                    .collect(),
                ForeignCode::Wasm(row) => vec![(format!("module {:?}", row.path), row)],
            };
            for (what, row) in rows {
                let digest = archive_digest(&row.bytes);
                if digest != row.pin {
                    // GIT-5: the cause is plain when the bytes are a Git LFS pointer.
                    let pointer = row
                        .bytes
                        .starts_with(b"version https://git-lfs.github.com/spec/v1\n");
                    return Err(format!(
                        "{}: foreign.{}: {what} does not match its pin: expected sha256 {}, found sha256 {}{}",
                        self.manifest_path.display(),
                        module.join("."),
                        hirsel_core_ids::hex::encode(&row.pin),
                        hirsel_core_ids::hex::encode(&digest),
                        if pointer {
                            "; it is a Git LFS pointer, and hirsel does not fetch LFS content"
                        } else {
                            ""
                        }
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn archive_path(from: &Path, path: &str) -> Option<String> {
    if path.is_empty() || path.starts_with('/') {
        return None;
    }
    let mut components: Vec<_> = from.iter().map(|c| c.to_str()).collect::<Option<_>>()?;
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            normal => components.push(normal),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

pub(crate) fn cycles(edges: &BTreeMap<Id, BTreeMap<Id, Edge>>) -> Result<(), String> {
    let mut finished = BTreeSet::new();
    for start in edges.keys() {
        if finished.contains(start) {
            continue;
        }
        let member_edges = |id: &Id| {
            edges[id]
                .iter()
                .filter_map(|(id, edge)| matches!(edge, Edge::Member).then_some(id))
        };
        let mut stack = vec![(start, member_edges(start))];
        let mut visiting = BTreeMap::from([(start, 0)]);
        while let Some((id, outgoing)) = stack.last_mut() {
            let Some(next) = outgoing.next() else {
                finished.insert(*id);
                visiting.remove(id);
                stack.pop();
                continue;
            };
            if finished.contains(next) {
                continue;
            }
            if let Some(&first) = visiting.get(next) {
                let mut cycle: Vec<_> = stack[first..].iter().map(|(id, _)| *id).collect();
                let smallest = cycle
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, id)| *id)
                    .unwrap()
                    .0;
                cycle.rotate_left(smallest);
                cycle.push(cycle[0]);
                return Err(format!(
                    "the members' path bindings form a cycle: {}",
                    cycle
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ));
            }
            visiting.insert(next, stack.len());
            stack.push((next, member_edges(next)));
        }
    }
    Ok(())
}
