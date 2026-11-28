use crate::package_content::{Content, Id, Root, archive_digest};
use crate::package_files::Captured;
use crate::package_lock::{Lock, NAME as LOCK, Row, make_rows, old_rows};
use crate::package_manifest::{Dependency, Manifest};
use crate::package_resolve::Resolution;
use crate::package_workspace::Workspace;
use crate::reachability::authority::{Cards, names};
use hirsel_core_resolve::DefinitionKind;
use hirsel_core_syntax::{NodeKind, ParseMode, Symbols, parse_bytes};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::report::Diff;

#[derive(Serialize)]
pub(crate) struct Plan {
    workspace: PathBuf,
    workspace_identity: (String, String),
    member: Option<String>,
    pub(crate) files: Vec<FileChange>,
    bindings: Vec<BindingChange>,
    selections: Vec<Change<Selection>>,
    locks: Vec<Change<Selection>>,
    exports: BTreeMap<String, BTreeMap<String, ExportChange>>,
    authority: BTreeMap<String, Diff>,
    pub(crate) registry: Vec<serde_json::Value>,
    #[serde(skip)]
    pub(crate) checks_authority: bool,
}

#[derive(Serialize)]
pub(crate) struct FileChange {
    pub(crate) path: PathBuf,
    pub(crate) before: Option<String>,
    pub(crate) after: String,
}

#[derive(Serialize)]
struct BindingChange {
    manifest: PathBuf,
    table: &'static str,
    alias: String,
    before: Option<Binding>,
    after: Option<Binding>,
}

#[derive(Eq, PartialEq, Serialize)]
struct Binding {
    kind: &'static str,
    id: Option<String>,
    root: Option<String>,
    source: Option<String>,
    authority: Option<Vec<&'static str>>,
}

impl From<&Dependency> for Binding {
    fn from(dependency: &Dependency) -> Self {
        let (kind, id, root, source, authority) = match dependency {
            Dependency::Published(id, root, source, ceiling) => (
                "published",
                Some(id),
                Some(root.to_string()),
                source.as_ref().map(ToString::to_string),
                Some(*ceiling),
            ),
            Dependency::Member(id, path, ceiling) => {
                ("path", Some(id), None, Some(path.clone()), Some(*ceiling))
            }
            Dependency::Toolchain(id) => ("toolchain", Some(id), None, None, Some(0)),
            Dependency::Inherited => ("inherit", None, None, None, None),
        };
        Self {
            kind,
            id: id.map(ToString::to_string),
            root,
            source,
            authority: authority.map(|mask| names(mask.into())),
        }
    }
}

#[derive(Serialize)]
struct Change<T> {
    id: String,
    before: Option<T>,
    after: Option<T>,
}

#[derive(Serialize)]
struct Selection {
    root: Option<String>,
    version: Option<String>,
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tested: Option<Vec<String>>,
}

impl From<&Row> for Selection {
    fn from(row: &Row) -> Self {
        Self {
            root: row.root.map(|root| root.to_string()),
            version: row.version.clone(),
            source: Some(row.source.clone()),
            tested: Some(row.tested.clone()),
        }
    }
}

#[derive(Serialize)]
struct ExportChange {
    change: &'static str,
    before: Option<String>,
    after: Option<String>,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    Root(archive_digest(bytes)).to_string()
}

fn file(path: PathBuf, before: Option<&[u8]>, after: &[u8]) -> FileChange {
    FileChange {
        path,
        before: before.map(digest),
        after: digest(after),
    }
}

impl Plan {
    pub(crate) fn new<'a>(
        before: &Workspace,
        trial: &Resolution,
        edits: impl IntoIterator<Item = (PathBuf, &'a Captured, &'a Captured)>,
        prior_lock: Option<&Captured>,
        lock: &Lock,
        baseline: Option<&Resolution>,
    ) -> Result<Self, String> {
        let mut files = Vec::new();
        let mut bindings = Vec::new();
        for (path, old, new) in edits {
            if old.bytes == new.bytes {
                continue;
            }
            let at = before.directory.join(&path);
            let old_config = Manifest::parse(&old.bytes, &at)?;
            let new_config = Manifest::parse(&new.bytes, &at)?;
            for (table, old, new) in [
                (
                    "dependencies",
                    old_config.package.map(|p| p.dependencies),
                    new_config.package.map(|p| p.dependencies),
                ),
                (
                    "workspace.dependencies",
                    old_config.workspace.map(|w| w.dependencies),
                    new_config.workspace.map(|w| w.dependencies),
                ),
            ] {
                let old = old.unwrap_or_default();
                let new = new.unwrap_or_default();
                for alias in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
                    let left = old.get(alias).map(Binding::from);
                    let right = new.get(alias).map(Binding::from);
                    if left != right {
                        bindings.push(BindingChange {
                            manifest: path.clone(),
                            table,
                            alias: alias.clone(),
                            before: left,
                            after: right,
                        });
                    }
                }
            }
            files.push(file(path, Some(&old.bytes), &new.bytes));
        }
        let old_rows = prior_lock
            .and_then(|lock| old_rows(&lock.bytes))
            .unwrap_or_default();
        let new_rows = make_rows(trial);
        let locks = old_rows
            .keys()
            .chain(new_rows.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let old = old_rows.get(id);
                let new = new_rows.get(id);
                (old != new).then(|| Change {
                    id: id.to_string(),
                    before: old.map(Selection::from),
                    after: new.map(Selection::from),
                })
            })
            .collect();
        if let Some((bytes, _)) = lock.pending() {
            files.push(file(
                LOCK.into(),
                prior_lock.map(|lock| lock.bytes.as_ref()),
                bytes.as_bytes(),
            ));
        }

        let proposed = &trial.workspace.selections;
        let selections = before
            .selections
            .keys()
            .chain(proposed.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let old = before.selections.get(id);
                let new = proposed.get(id);
                let selected =
                    |(root, source): &(Root, Option<crate::package_manifest::SourceLocation>),
                     resolution: Option<&Resolution>| Selection {
                        root: Some(root.to_string()),
                        version: resolution
                            .and_then(|res| res.libraries.get(root))
                            .map(|lib| lib.content.version.to_string()),
                        source: source.as_ref().map(ToString::to_string),
                        tested: None,
                    };
                (old != new).then(|| Change {
                    id: id.to_string(),
                    before: old.map(|row| selected(row, baseline)),
                    after: new.map(|row| selected(row, Some(trial))),
                })
            })
            .collect();

        let previous = |id: &Id| {
            old_rows
                .get(id)
                .and_then(|row| row.root)
                .or_else(|| baseline.and_then(|res| res.published.get(id).copied()))
        };
        let mut checks_authority = false;
        let exports = old_rows
            .keys()
            .chain(trial.published.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let old = previous(id);
                let new = trial.published.get(id).copied();
                if old == new {
                    return None;
                }
                let old_interface = old.and_then(|root| {
                    baseline
                        .and_then(|res| res.libraries.get(&root))
                        .or_else(|| trial.libraries.get(&root))
                        .map(|library| interface(&library.content))
                        .or_else(|| match trial.store.read(root).ok()? {
                            crate::package_store::Entry::Present(library)
                                if library.content.id == *id =>
                            {
                                Some(interface(&library.content))
                            }
                            _ => None,
                        })
                });
                let new_interface =
                    new.and_then(|root| trial.libraries.get(&root))
                        .map(|library| {
                            checks_authority |= library
                                .content
                                .files
                                .values()
                                .any(|file| !file.bytes.iter().all(u8::is_ascii_whitespace));
                            interface(&library.content)
                        });
                let unavailable = old.is_some() && old_interface.is_none();
                let delta = export_changes(
                    old_interface.unwrap_or_default(),
                    new_interface.unwrap_or_default(),
                    unavailable,
                );
                Some((id.to_string(), delta))
            })
            .collect();

        use std::os::unix::fs::MetadataExt;
        let meta = before.root.metadata().map_err(|error| error.to_string())?;
        Ok(Self {
            workspace: before.directory.clone(),
            workspace_identity: (meta.dev().to_string(), meta.ino().to_string()),
            member: before.member.as_ref().map(ToString::to_string),
            files,
            bindings,
            selections,
            locks,
            exports,
            authority: BTreeMap::new(),
            registry: Vec::new(),
            checks_authority,
        })
    }

    pub(crate) fn authority(&mut self, prior: Option<&Cards>, checked: &Cards) -> bool {
        let mut expanded = false;
        for (id, after) in checked {
            let name = format!("{}@{}", id.name, id.version);
            if !self.exports.contains_key(&name) {
                continue;
            }
            let before = prior.and_then(|cards| cards.get(id));
            let diff = Diff::between(before, after);
            expanded |= after.reached.kinds & !before.map_or(0, |card| card.reached.kinds) != 0;
            self.authority.insert(name, diff);
        }
        expanded
    }

    pub(crate) fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(|change| change.path.as_path())
    }
}

fn interface(content: &Content) -> BTreeMap<String, String> {
    let mut exports = BTreeMap::new();
    let mut symbols = Symbols::default();
    for source in content.files.values() {
        if source.bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let parsed = parse_bytes(&source.bytes, ParseMode::Signatures, &mut symbols);
        for item in parsed.syntax.items().filter(|item| !item.range.has_errors) {
            let root = item.tree.node(item.root);
            let kind = match root.kind {
                NodeKind::Function => DefinitionKind::Function,
                NodeKind::Constant => DefinitionKind::Constant,
                NodeKind::TypeDecl | NodeKind::ResourceType | NodeKind::ScopedType => {
                    DefinitionKind::Type
                }
                _ => continue,
            };
            let mut modifiers = item
                .tree
                .children(item.root)
                .iter()
                .map(|&id| item.tree.node(id).kind);
            if !modifiers.clone().any(|kind| kind == NodeKind::Public) {
                continue;
            }
            if let Some(token) = root.token {
                let name = symbols.text(item.symbol(token));
                let path = source
                    .module
                    .iter()
                    .map(String::as_str)
                    .chain([name])
                    .collect::<Vec<_>>()
                    .join("/");
                let opaque = modifiers.any(|kind| kind == NodeKind::Opaque);
                exports.insert(
                    path,
                    crate::inspect::signature(item, kind, opaque).to_owned(),
                );
            }
        }
    }
    exports
}

fn export_changes(
    old: BTreeMap<String, String>,
    new: BTreeMap<String, String>,
    unavailable: bool,
) -> BTreeMap<String, ExportChange> {
    old.keys()
        .chain(new.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let before = old.get(name);
            let after = new.get(name);
            let change = match (before, after) {
                _ if unavailable => "previous_unavailable",
                (None, Some(_)) => "added",
                (Some(_), None) => "removed",
                _ if before != after => "changed",
                _ => return None,
            };
            Some((
                name.clone(),
                ExportChange {
                    change,
                    before: before.cloned(),
                    after: after.cloned(),
                },
            ))
        })
        .collect()
}
