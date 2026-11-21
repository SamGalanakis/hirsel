use crate::package_content::{
    ARCHIVE_LIMIT, ArchiveRow, Content, ForeignModule, Id, Root, Source, hex,
};
use crate::package_dir::MANIFEST;
use crate::package_files::{Captured, FileId, WalkError, open_directory, walk};
use crate::package_manifest::{Dependency, Manifest, Package, SourceLocation};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone)]
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
    pub(crate) content: Content,
    pub(crate) tests: BTreeMap<String, Source>,
}

pub(crate) struct Workspace {
    pub(crate) root: File,
    pub(crate) directory: PathBuf,
    pub(crate) member: Option<Id>,
    pub(crate) members: BTreeMap<Id, Member>,
    pub(crate) inputs: BTreeSet<FileId>,
}

struct Discovered {
    dir: File,
    path: PathBuf,
    manifest: Manifest,
    captured: Captured,
}

fn parent(directory: &File) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: directory is live and ".." is a static C string. This
        // ascent discovers workspace ownership; it is not a confined walk.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c"..".as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = directory;
        Err(io::ErrorKind::Unsupported.into())
    }
}

fn identity(directory: &File) -> io::Result<FileId> {
    directory.metadata().map(|metadata| FileId::of(&metadata))
}

fn file_error(path: &Path, field: &str, file: &str, error: &WalkError) -> String {
    format!(
        "{}: {field} {file:?} {}",
        path.display(),
        error.explain("package", "a regular file")
    )
}

fn read_manifest(dir: &File, path: &Path) -> Result<Option<Captured>, String> {
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
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
        Some(parent) if parent != path => parent.to_path_buf(),
        _ => path.join(".."),
    }
}

fn discover(dir: &Path) -> Result<Option<(Discovered, Discovered)>, Box<dyn std::error::Error>> {
    let mut directory = open_directory(dir)?;
    let mut path = dir.to_path_buf();
    let mut member: Option<Discovered> = None;
    let first;
    loop {
        if let Some(captured) = read_manifest(&directory, &path)? {
            let manifest = Manifest::parse(&captured.bytes, &path.join(MANIFEST))?;
            let workspace = manifest.workspace.is_some();
            let discovered = Discovered {
                dir: directory.try_clone()?,
                path: path.clone(),
                manifest,
                captured,
            };
            if workspace {
                first = discovered;
                break;
            }
            if member.is_none() {
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
                manifest: Manifest {
                    package: None,
                    workspace: None,
                },
                captured: Captured {
                    bytes: member.captured.bytes.clone(),
                    identity: member.captured.identity,
                },
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
            manifest: Manifest {
                package: None,
                workspace: None,
            },
            captured: Captured {
                bytes: first.captured.bytes.clone(),
                identity: first.captured.identity,
            },
        },
    };
    Ok(Some((first, requested)))
}

impl Workspace {
    pub(crate) fn discover(dir: &Path) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Some((mut root, mut selected)) = discover(dir)? else {
            return Ok(None);
        };
        let root_id = identity(&root.dir)?;
        let selected_id = identity(&selected.dir)?;
        let root_manifest = root.path.join(MANIFEST);
        let selected_manifest = selected.path.join(MANIFEST);
        let mut inputs = BTreeSet::from([root.captured.identity, selected.captured.identity]);
        let mut opened = BTreeMap::new();
        let mut drafts = BTreeMap::new();

        let workspace = root.manifest.workspace.take();
        let package = root.manifest.package.take().or_else(|| {
            if root_id == selected_id {
                selected.manifest.package.take()
            } else {
                None
            }
        });
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
            let mut selected = Some(selected);
            for listed in &config.members {
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
                if dir_id == root_id || opened.contains_key(&dir_id) {
                    return Err(format!(
                        "{}: workspace.members: {listed:?} is listed twice",
                        root_manifest.display()
                    )
                    .into());
                }
                let path = root.path.join(listed);
                let manifest_path = path.join(MANIFEST);
                let manifest = if dir_id == selected_id
                    && let Some(selected) = selected.take()
                {
                    selected.manifest
                } else {
                    let captured = read_manifest(&dir, &path)?
                        .ok_or_else(|| format!("{}: does not exist", manifest_path.display()))?;
                    inputs.insert(captured.identity);
                    Manifest::parse(&captured.bytes, &manifest_path)?
                };
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
                    if let Some(captured) = read_manifest(&ancestor, &at)? {
                        inputs.insert(captured.identity);
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
                let package = manifest
                    .package
                    .ok_or_else(|| format!("{}: expected [package]", manifest_path.display()))?;
                if drafts.contains_key(&package.id) {
                    return Err(format!(
                        "{}: workspace.members: {listed:?} repeats {}",
                        root_manifest.display(),
                        package.id
                    )
                    .into());
                }
                opened.insert(dir_id, package.id.clone());
                drafts.insert(package.id.clone(), (dir, relative, manifest_path, package));
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
        let mut edges_by_member = BTreeMap::new();
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
                    Dependency::Member(id, path) => {
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
                    Dependency::Published(id, root, source) => {
                        let source = match source {
                            Some(SourceLocation::Archive(path)) => {
                                let path = archive_path(from, path).ok_or_else(|| format!("{}: dependencies.{name}: archive {path:?} is not inside the workspace root", manifest.display()))?;
                                Some(SourceLocation::Archive(path))
                            }
                            other => other.clone(),
                        };
                        (id, Edge::Published(*root, source))
                    }
                };
                edges.insert(expected.clone(), edge);
            }
            edges_by_member.insert(id.clone(), edges);
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
                let mut archives = BTreeMap::new();
                for (target, row) in &spec.rows {
                    let bytes = read(&row.file, &format!("foreign.{}: archive", module.join(".")))?;
                    archives.insert(
                        target.clone(),
                        ArchiveRow {
                            path: row.file.clone(),
                            pin: row.pin,
                            bytes,
                        },
                    );
                }
                foreign.insert(
                    module.clone(),
                    ForeignModule {
                        symbols: spec.symbols.clone(),
                        archives,
                    },
                );
            }
            let content = Content {
                id: id.clone(),
                bindings: BTreeMap::new(),
                files,
                foreign,
            };
            let edges = edges_by_member.remove(&id).unwrap();
            members.insert(
                id,
                Member {
                    directory,
                    manifest_path,
                    package,
                    edges,
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
            inputs,
        }))
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
            for (target, row) in &foreign.archives {
                let digest = row.digest();
                if digest != row.pin {
                    return Err(format!(
                        "{}: foreign.{}: archive {:?} for {target} does not match its pin: expected sha256 {}, found sha256 {}",
                        self.manifest_path.display(),
                        module.join("."),
                        row.path,
                        hex(&row.pin),
                        hex(&digest)
                    ));
                }
            }
        }
        Ok(())
    }
}

fn archive_path(from: &Path, path: &str) -> Option<String> {
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

fn cycles(edges: &BTreeMap<Id, BTreeMap<Id, Edge>>) -> Result<(), String> {
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
