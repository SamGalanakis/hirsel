use super::cache::{Cache, Check};
use super::diagnostics::{self, Diagnostic, InputOrigins, Origin};
use super::documents::{Document, SourceOverlay, Text};
use super::snapshot::{EditorSnapshot, immutable_path};
use crate::ContentHash;
use crate::package_content::Id;
use crate::package_dir::{MANIFEST, PackageDir};
use crate::package_lock::{self, Lock, Mode};
use crate::package_resolve::Resolution;
use crate::package_store::PackageStore;
use crate::package_workspace::{Member, Workspace};
use crate::{CheckRequest, CoreImage, Driver, Meter, PackageProduct};
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{CapturedSource, ModuleSource, NameIndex, PackageManifest};
use hirsel_core_syntax::ParseMode;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(super) struct EditorInputs {
    pub(super) generation: u64,
    pub(super) folders: Option<BTreeSet<PathBuf>>,
    pub(super) overlay: SourceOverlay,
    pub(super) detached: BTreeSet<PathBuf>,
}

impl EditorInputs {
    pub(super) fn owns(&self, path: &Path) -> bool {
        self.detached.contains(path)
            || self
                .folders
                .as_ref()
                .is_none_or(|folders| folders.iter().any(|folder| path.starts_with(folder)))
    }

    pub(super) fn open(&mut self, path: PathBuf, document: Document) -> bool {
        if !self.owns(&path)
            && path.extension().is_some_and(|ext| ext == "hirsel")
            && manifest_directory(&path).is_none()
        {
            self.detached.insert(path.clone());
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = self.overlay.entry(path) {
            entry.insert(document);
            return true;
        }
        false
    }
}

pub(super) enum Project<'a> {
    Workspace(&'a Resolution),
    Detached(&'a Path),
}

impl Project<'_> {
    pub(super) fn identity(&self) -> &Path {
        match self {
            Self::Workspace(resolved) => &resolved.workspace.directory,
            Self::Detached(path) => path,
        }
    }
}

impl EditorSnapshot {
    fn syntax(&mut self, path: &Path) {
        let mode = self.modes.get(path).copied().unwrap_or(ParseMode::Source);
        self.diagnostics
            .extend(diagnostics::syntax(path, &self.origins[path], mode));
    }

    fn configuration(&mut self, path: &Path, error: impl ToString) {
        self.diagnostics
            .push(Diagnostic::configuration(path, error.to_string()));
    }

    fn project_problem<'a>(
        &mut self,
        configuration: &Path,
        error: impl ToString,
        paths: impl IntoIterator<Item = &'a PathBuf>,
    ) {
        let message = error.to_string();
        self.configuration(configuration, &message);
        for path in paths {
            if self.inputs.overlay.contains_key(path) {
                self.configuration(path, &message);
            }
            self.syntax(path);
        }
    }
}

#[derive(Default)]
pub(super) struct EditorSession {
    driver: Driver,
    cache: Cache,
    std_image: Option<Arc<CoreImage>>,
    scratch: BTreeMap<PathBuf, tempfile::TempDir>,
}

impl EditorSession {
    pub(super) fn check(
        &mut self,
        inputs: EditorInputs,
        obsolete: impl Fn() -> bool,
    ) -> EditorSnapshot {
        let mut snapshot = EditorSnapshot::new(inputs);
        let mut groups = BTreeMap::<Option<PathBuf>, BTreeSet<PathBuf>>::new();
        for path in snapshot.origins.keys() {
            groups
                .entry(manifest_directory(path))
                .or_default()
                .insert(path.clone());
        }
        if let Some(paths) = groups.remove(&None) {
            for path in paths {
                if obsolete() {
                    return snapshot;
                }
                if let Err(error) = self.check_detached(&path, &mut snapshot) {
                    snapshot.configuration(&path, error);
                    snapshot.syntax(&path);
                }
            }
        }
        for (directory, mut paths) in groups {
            if obsolete() {
                break;
            }
            let directory = directory.expect("standalone paths were handled");
            let manifest = directory.join(MANIFEST);
            let workspace = match Workspace::discover_for(&directory, false) {
                Ok(Some(workspace)) => workspace,
                Ok(None) => {
                    for path in &paths {
                        snapshot.syntax(path);
                    }
                    continue;
                }
                Err(error) => {
                    snapshot.project_problem(&manifest, error, &paths);
                    continue;
                }
            };
            let mut by_member = BTreeMap::new();
            for (id, member) in &workspace.members {
                let owned: BTreeSet<_> = paths
                    .extract_if(.., |path| {
                        let Some(relative) = path
                            .strip_prefix(member_directory(member))
                            .ok()
                            .and_then(Path::to_str)
                        else {
                            return false;
                        };
                        let Some(source) = member
                            .content
                            .files
                            .get(relative)
                            .or_else(|| member.tests.get(relative))
                        else {
                            return false;
                        };
                        let mode = if member.content.foreign.contains_key(&source.module) {
                            ParseMode::Foreign
                        } else {
                            ParseMode::Source
                        };
                        snapshot.modes.insert(path.clone(), mode);
                        true
                    })
                    .collect();
                if !owned.is_empty() {
                    by_member.insert(id.clone(), owned);
                }
            }
            for path in &paths {
                snapshot.syntax(path);
            }
            if by_member.is_empty() {
                continue;
            }
            let root = workspace.directory.clone();
            // Resolve and validate the lock before any overlay bytes enter a check request.
            let mode = Mode::Frozen;
            let lock_path = root.join(package_lock::NAME);
            let remedy = format!(
                "Run hirsel lock in {} to refresh package analysis.",
                root.display()
            );
            let lock = match Lock::capture(&workspace) {
                Ok(Some(lock)) => lock,
                Ok(None) => {
                    let message = format!("hirsel.lock is missing. {remedy}");
                    snapshot.project_problem(&lock_path, message, by_member.values().flatten());
                    continue;
                }
                Err(error) => {
                    let message = format!("{error}\n{remedy}");
                    snapshot.project_problem(&lock_path, message, by_member.values().flatten());
                    continue;
                }
            };
            let store = PackageStore::with_transport(None, mode.transport());
            let resolved = Resolution::resolve(workspace, store, mode == Mode::Frozen);
            let resolved = match resolved {
                Ok(resolved) => resolved,
                Err(error) => {
                    let message = format!(
                        "{error}\nAfter fixing the dependency error, run hirsel lock in {}.",
                        root.display()
                    );
                    snapshot.project_problem(
                        &root.join(MANIFEST),
                        message,
                        by_member.values().flatten(),
                    );
                    continue;
                }
            };
            if let Err(error) = Lock::compare_captured(&resolved, mode, Some(&lock)) {
                let message = format!("{error}\n{remedy}");
                snapshot.project_problem(&lock_path, message, by_member.values().flatten());
                continue;
            }
            for (member, paths) in by_member {
                if obsolete() {
                    return snapshot;
                }
                if let Err(error) = self.check_member(&resolved, &member, &paths, &mut snapshot) {
                    snapshot.project_problem(
                        &resolved.workspace.members[&member].manifest_path,
                        error,
                        &paths,
                    );
                }
            }
        }
        snapshot
    }

    fn check_detached(&mut self, path: &Path, snapshot: &mut EditorSnapshot) -> io::Result<()> {
        let directory = path
            .parent()
            .expect("a source file has a containing directory");
        let scratch = self.scratch(&snapshot.inputs, directory)?;
        let sources = scratch.join("sources");
        let mut request = CheckRequest::library_request(None, &scratch.join("detached"))?;
        let package = crate::package("local/app");
        let module = path
            .file_stem()
            .expect("a source file has a stem")
            .to_string_lossy();
        if module == "main" {
            request.entry = DefPath::from_full("local/app@1/main/main");
        }
        let name = path
            .file_name()
            .expect("a source file has a name")
            .to_string_lossy()
            .into_owned();
        let text = snapshot.origins[path].clone();
        request.inputs.packages.push(PackageManifest {
            package: package.clone(),
            sources: vec![ModuleSource {
                file: path.to_owned(),
                modules: vec![module.into_owned()],
                signatures: false,
                foreign: None,
                captured: Some(CapturedSource {
                    bytes: text.bytes.clone(),
                    name: Some(name.clone()),
                    test: false,
                }),
            }],
            dependencies: vec![request.inputs.core.clone(), crate::package("std")],
            namespace: None,
            toolchain: false,
        });
        let origins = InputOrigins::from([(
            (package, name),
            Origin {
                path: path.to_owned(),
                text,
            },
        )]);
        let refusal = self.check_request(
            request,
            origins,
            &BTreeSet::from([path.to_owned()]),
            Project::Detached(path),
            &sources,
            snapshot,
        )?;
        if let Some(refusal) = refusal {
            snapshot.configuration(path, refusal);
        }
        Ok(())
    }

    fn scratch(&mut self, inputs: &EditorInputs, directory: &Path) -> io::Result<&Path> {
        let scratch = match self.scratch.entry(directory.to_owned()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(private_store(inputs, directory)?)
            }
        };
        Ok(scratch.path())
    }

    fn check_member(
        &mut self,
        resolved: &Resolution,
        member: &Id,
        wanted: &BTreeSet<PathBuf>,
        snapshot: &mut EditorSnapshot,
    ) -> io::Result<()> {
        let app = PackageDir::from_resolution(resolved, member, true)?;
        let workspace = &resolved.workspace;
        let scratch = self.scratch(&snapshot.inputs, &workspace.directory)?;
        let store = scratch
            .join("check")
            .join(ContentHash::of_bytes(member.to_string().as_bytes()).to_string());
        let sources = scratch.join("sources");
        // Registered image opens may sweep their store. Bundled library sources
        // keep the editor's analysis outside the regular build caches.
        let mut request = CheckRequest::from_app(app, None, &store)?;

        let mut origins = InputOrigins::new();
        for (id, local) in &workspace.members {
            let Some(package) = request
                .inputs
                .packages
                .iter_mut()
                .find(|package| package.package == id.compiler_id())
            else {
                continue;
            };
            let directory = member_directory(local);
            let prefix = format!("{id}/");
            for source in &mut package.sources {
                let Some(capture) = &mut source.captured else {
                    continue;
                };
                let Some(name) = &capture.name else { continue };
                let file = if id == member {
                    name.as_str()
                } else {
                    name.strip_prefix(&prefix)
                        .expect("a dependency source has a package prefix")
                };
                let path = directory.join(file);
                let text = match snapshot.inputs.overlay.get(&path) {
                    Some(document) if snapshot.inputs.owns(&path) => {
                        capture.bytes = document.text.bytes.clone();
                        document.text.clone()
                    }
                    _ => Text::new(capture.bytes.clone()),
                };
                origins.insert(
                    (package.package.clone(), name.clone()),
                    Origin { path, text },
                );
            }
        }
        let refusal = self.check_request(
            request,
            origins,
            wanted,
            Project::Workspace(resolved),
            &sources,
            snapshot,
        )?;
        if let Some(refusal) = refusal {
            snapshot.configuration(&workspace.members[member].manifest_path, refusal);
        }
        Ok(())
    }

    fn check_request(
        &mut self,
        mut request: CheckRequest,
        mut origins: InputOrigins,
        wanted: &BTreeSet<PathBuf>,
        project: Project<'_>,
        sources: &Path,
        snapshot: &mut EditorSnapshot,
    ) -> io::Result<Option<String>> {
        let mut check = Check::default();
        check.rename_base = self.bundled_headers(&mut request, sources)?;
        for package in &mut request.inputs.packages {
            let root = package
                .namespace
                .as_ref()
                .and_then(|namespace| namespace.root)
                .unwrap_or_else(|| {
                    ContentHash::of_parts(
                        "hirsel.editor-supplied-source.v1",
                        [
                            package.package.name.as_bytes(),
                            package.package.version.as_bytes(),
                        ],
                    )
                });
            for source in &mut package.sources {
                let mode = source.mode();
                let Some(capture) = &mut source.captured else {
                    continue;
                };
                let name = capture
                    .name
                    .get_or_insert_with(|| source.file.to_string_lossy().into_owned());
                let key = (package.package.clone(), name.clone());
                if origins.contains_key(&key) {
                    continue;
                }
                let Some(path) =
                    immutable_path(sources, root, ContentHash::of_bytes(&capture.bytes), name)
                else {
                    continue;
                };
                snapshot.modes.insert(path.clone(), mode);
                snapshot.immutable.insert(path.clone());
                origins.insert(
                    key,
                    Origin {
                        path,
                        text: Text::new(capture.bytes.clone()),
                    },
                );
            }
        }
        let product = self
            .driver
            .check_for_editor(request, &mut self.cache, &mut check)?;
        let stats = check.stats(&self.cache);
        snapshot.stats.parse_hits += stats.parse_hits;
        snapshot.stats.parse_misses += stats.parse_misses;
        snapshot.stats.body_hits += stats.body_hits;
        snapshot.stats.body_checks += stats.body_checks;
        snapshot.stats.retained_bytes = stats.retained_bytes;
        snapshot.diagnostics.extend(diagnostics::checked(
            &product,
            &origins,
            wanted,
            &snapshot.immutable,
        ));
        snapshot.origins.extend(
            origins
                .values()
                .map(|origin| (origin.path.clone(), origin.text.clone())),
        );
        snapshot.retain(&product, &check, &origins, wanted, project, sources);
        Ok(product.authority_refusal.clone())
    }

    fn bundled_headers(
        &mut self,
        request: &mut CheckRequest,
        sources: &Path,
    ) -> io::Result<Option<Arc<NameIndex>>> {
        if !request
            .inputs
            .packages
            .iter()
            .any(|p| p.package == crate::package("std"))
        {
            return Ok(None);
        }
        // Check toolchain bodies when building the library product. Editor facts
        // are collected only for the sources in the current request's overlay.
        if self.std_image.is_none() {
            self.std_image = Some(
                PackageProduct::bundled_std(
                    &sources.with_file_name("std"),
                    &mut Meter::new(false),
                )?
                .image,
            );
        }
        let image = Arc::clone(self.std_image.as_ref().unwrap());
        let directory = image.directory()?;
        if !request.use_image(image)? {
            return Err(io::Error::other(
                "toolchain image does not match the requested libraries",
            ));
        }

        // Keep the image's headers and the current module topology. Rename
        // reindexes the writable overlay against that same immutable base.
        let mut index = directory.index.as_ref().clone();
        let mut symbols = index.symbols.as_ref().clone();
        for package in &request.inputs.packages {
            package.intern(&mut symbols);
        }
        index.symbols = Arc::new(symbols);
        let mut manifests = request.inputs.packages.clone();
        manifests.extend(
            directory
                .inputs
                .packages
                .iter()
                .cloned()
                .map(|mut package| {
                    package.sources.clear();
                    package
                }),
        );
        index.add_manifests(&manifests);
        Ok(Some(Arc::new(index)))
    }
}

fn manifest_directory(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|directory| directory.join(MANIFEST).symlink_metadata().is_ok())
        .map(Path::to_owned)
}

fn member_directory(member: &Member) -> &Path {
    member
        .manifest_path
        .parent()
        .expect("a manifest has a containing directory")
}

fn private_store(inputs: &EditorInputs, workspace: &Path) -> io::Result<tempfile::TempDir> {
    let cache = crate::PackageProduct::default_cache();
    let cache = cache.parent().unwrap_or(&cache);
    for candidate in [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/dev/shm"),
    ] {
        let Ok(directory) = candidate.canonicalize() else {
            continue;
        };
        let contains = |root: &Path| {
            directory.starts_with(root)
                || root
                    .canonicalize()
                    .is_ok_and(|root| directory.starts_with(root))
        };
        if !contains(workspace)
            && !contains(cache)
            && inputs
                .folders
                .as_ref()
                .is_none_or(|folders| !folders.iter().any(|folder| contains(folder)))
        {
            return tempfile::Builder::new()
                .prefix("hirsel-lsp-")
                .tempdir_in(directory);
        }
    }
    Err(io::Error::other(
        "no temporary directory outside the workspace and build cache",
    ))
}
