use super::diagnostics::{self, Diagnostic, InputOrigins, Origin};
use super::documents::{SourceOverlay, Text};
use crate::package_content::Id;
use crate::package_dir::{MANIFEST, PackageDir};
use crate::package_lock::{self, Lock, Mode};
use crate::package_resolve::Resolution;
use crate::package_store::PackageStore;
use crate::package_workspace::{Member, Workspace};
use crate::{CheckRequest, Driver};
use hirsel_core_syntax::ParseMode;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct EditorInputs {
    pub(super) generation: u64,
    pub(super) folders: Option<BTreeSet<PathBuf>>,
    pub(super) overlay: SourceOverlay,
}

impl EditorInputs {
    pub(super) fn owns(&self, path: &Path) -> bool {
        self.folders
            .as_ref()
            .is_none_or(|folders| folders.iter().any(|folder| path.starts_with(folder)))
    }
}

pub(super) struct EditorSnapshot {
    pub(super) inputs: EditorInputs,
    pub(super) origins: BTreeMap<PathBuf, Text>,
    pub(super) diagnostics: Vec<Diagnostic>,
}

impl EditorSnapshot {
    fn new(inputs: EditorInputs) -> Self {
        let origins = inputs
            .overlay
            .iter()
            .filter(|(path, _)| inputs.owns(path))
            .map(|(path, document)| (path.clone(), document.text.clone()))
            .collect();
        Self {
            inputs,
            origins,
            diagnostics: Vec::new(),
        }
    }

    fn syntax(&mut self, paths: &BTreeSet<PathBuf>, modes: &BTreeMap<PathBuf, ParseMode>) {
        for path in paths {
            let mode = modes.get(path).copied().unwrap_or(ParseMode::Source);
            self.diagnostics
                .extend(diagnostics::syntax(path, &self.origins[path], mode));
        }
    }

    fn configuration(&mut self, path: &Path, error: impl ToString) {
        self.diagnostics
            .push(Diagnostic::configuration(path, error.to_string()));
    }
}

#[derive(Default)]
pub(super) struct EditorSession {
    driver: Driver,
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
            snapshot.syntax(&paths, &BTreeMap::new());
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
                    snapshot.syntax(&paths, &BTreeMap::new());
                    continue;
                }
                Err(error) => {
                    snapshot.configuration(&manifest, error);
                    snapshot.syntax(&paths, &BTreeMap::new());
                    continue;
                }
            };
            let mut by_member = BTreeMap::new();
            let mut modes = BTreeMap::new();
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
                        modes.insert(path.clone(), mode);
                        true
                    })
                    .collect();
                if !owned.is_empty() {
                    by_member.insert(id.clone(), owned);
                }
            }
            snapshot.syntax(&paths, &modes);
            if by_member.is_empty() {
                continue;
            }
            let root = workspace.directory.clone();
            // Resolve and validate the lock before any overlay bytes enter a check request.
            let mode = Mode::Frozen;
            let store = PackageStore::with_transport(None, mode.transport());
            let resolved = Resolution::resolve(workspace, store, mode == Mode::Frozen);
            let resolved = match resolved {
                Ok(resolved) => resolved,
                Err(error) => {
                    snapshot.configuration(&root.join(MANIFEST), error);
                    for paths in by_member.values() {
                        snapshot.syntax(paths, &modes);
                    }
                    continue;
                }
            };
            if let Err(error) = Lock::compare(&resolved, mode) {
                snapshot.configuration(&root.join(package_lock::NAME), error);
                for paths in by_member.values() {
                    snapshot.syntax(paths, &modes);
                }
                continue;
            }
            for (member, paths) in by_member {
                if obsolete() {
                    return snapshot;
                }
                if let Err(error) = self.check_member(&resolved, &member, &paths, &mut snapshot) {
                    snapshot
                        .configuration(&resolved.workspace.members[&member].manifest_path, error);
                    snapshot.syntax(&paths, &modes);
                }
            }
        }
        snapshot
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
        let scratch = private_store(&snapshot.inputs, &workspace.directory)?;
        // Registered image opens may sweep their store. Bundled library
        // sources keep all check artifacts in the editor's private store.
        let mut request = CheckRequest::from_app(app, None, scratch.path())?;

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
        let product = self.driver.check_for_authority(request)?;
        snapshot
            .diagnostics
            .extend(diagnostics::checked(&product, &origins, wanted));
        snapshot.origins.extend(
            origins
                .into_values()
                .map(|origin| (origin.path, origin.text)),
        );
        if let Some(refusal) = &product.authority_refusal {
            snapshot.configuration(&workspace.members[member].manifest_path, refusal);
        }
        Ok(())
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
