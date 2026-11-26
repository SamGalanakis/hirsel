use crate::fs::atomic_write;
use crate::package_content::{Id, Root};
use crate::package_files::{Captured, FileId, WalkError, open_directory, walk};
use crate::package_resolve::Resolution;
use crate::package_workspace::Workspace;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

pub(crate) const NAME: &str = "hirsel.lock";
const PREFIX: &str = "# hirsel.lock: written by hirsel; do not edit.\nversion = 1\n";

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum Mode {
    #[default]
    Update,
    Locked,
    Frozen,
}

impl Mode {
    pub(crate) fn flag(self) -> Option<&'static str> {
        match self {
            Self::Update => None,
            Self::Locked => Some("--locked"),
            Self::Frozen => Some("--frozen"),
        }
    }

    pub(crate) fn require_workspace(self, input: &str) -> Result<(), String> {
        if let Some(flag) = self.flag() {
            return Err(format!("{flag} needs a workspace lock; {input} has none"));
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq)]
struct Row {
    root: Option<Root>,
    source: String,
    edges: Vec<Id>,
}

type Rows = BTreeMap<Id, Row>;

fn std_row(rows: &mut Rows) {
    let toolchain = Id {
        name: "std".into(),
        major: 1,
    };
    if rows.values().any(|row| row.edges.contains(&toolchain)) {
        rows.insert(
            toolchain,
            Row {
                root: None,
                source: "toolchain".into(),
                edges: Vec::new(),
            },
        );
    }
}

fn make_rows(resolution: &Resolution) -> Rows {
    let mut rows = BTreeMap::new();
    for (id, member) in &resolution.workspace.members {
        let dir = if member.directory.as_os_str().is_empty() {
            "."
        } else {
            member.directory.to_str().unwrap()
        };
        rows.insert(
            id.clone(),
            Row {
                root: resolution.published.get(id).copied(),
                source: format!("member:{dir}"),
                edges: member.edges.keys().cloned().collect(),
            },
        );
    }
    for (id, root) in &resolution.published {
        if rows.contains_key(id) {
            continue;
        }
        rows.insert(
            id.clone(),
            Row {
                root: Some(*root),
                source: resolution.source(*root),
                edges: resolution.libraries[root]
                    .content
                    .bindings
                    .keys()
                    .cloned()
                    .collect(),
            },
        );
    }
    for id in &resolution.toolchain {
        rows.insert(
            id.clone(),
            Row {
                root: None,
                source: "toolchain".into(),
                edges: crate::package_content::toolchain_bindings(id)
                    .expect("a resolved toolchain package is supplied")
                    .into_keys()
                    .collect(),
            },
        );
    }
    std_row(&mut rows);
    rows
}

fn string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0}'..='\u{1f}' | '\u{7f}' => write!(out, "\\u{:04X}", u32::from(ch)).unwrap(),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn edges_text(edges: &[Id]) -> String {
    let items: Vec<_> = edges.iter().map(|id| string(&id.to_string())).collect();
    format!("[{}]", items.join(", "))
}

fn encode(rows: &Rows) -> String {
    let mut bytes = PREFIX.to_owned();
    for (id, row) in rows {
        writeln!(bytes, "\n[[package]]\nid = {}", string(&id.to_string())).unwrap();
        if let Some(root) = row.root {
            writeln!(bytes, "root = {}", string(&root.to_string())).unwrap();
        }
        writeln!(
            bytes,
            "source = {}\nedges = {}",
            string(&row.source),
            edges_text(&row.edges)
        )
        .unwrap();
    }
    bytes
}

pub(crate) fn scaffold() -> String {
    let mut rows = BTreeMap::from([(
        Id {
            name: "app".into(),
            major: 1,
        },
        Row {
            root: None,
            source: "member:.".into(),
            edges: vec![Id {
                name: "std".into(),
                major: 1,
            }],
        },
    )]);
    std_row(&mut rows);
    encode(&rows)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OldLock {
    version: u32,
    #[serde(default)]
    package: Vec<OldRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OldRow {
    id: String,
    root: Option<String>,
    source: String,
    edges: Vec<String>,
}

fn id(text: &str) -> Option<Id> {
    let (name, major) = text.split_once('@')?;
    if !crate::package_dir::package_name(name) || name == "core" {
        return None;
    }
    Some(Id {
        name: name.to_owned(),
        major: major.parse().ok()?,
    })
}

fn old_rows(bytes: &[u8]) -> Option<Rows> {
    let old = toml::from_str::<OldLock>(std::str::from_utf8(bytes).ok()?).ok()?;
    if old.version != 1 {
        return None;
    }
    let mut rows = BTreeMap::new();
    for row in old.package {
        let id = id(&row.id)?;
        let root = match row.root {
            Some(root) => Some(Root::parse(&root)?),
            None => None,
        };
        let edges = row
            .edges
            .iter()
            .map(|edge| self::id(edge))
            .collect::<Option<Vec<_>>>()?;
        if rows
            .insert(
                id,
                Row {
                    root,
                    source: row.source,
                    edges,
                },
            )
            .is_some()
        {
            return None;
        }
    }
    Some(rows)
}

/// Each package a lock file pins to a content root, as `(id, root, source)`; `None` when
/// the bytes are no version 1 lock.
pub(crate) fn pinned_roots(bytes: &[u8]) -> Option<Vec<(String, Root, String)>> {
    Some(
        old_rows(bytes)?
            .into_iter()
            .filter_map(|(id, row)| Some((id.to_string(), row.root?, row.source)))
            .collect(),
    )
}

fn differences(old: &[u8], now: &Rows) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(old) = old_rows(old) {
        let ids: BTreeSet<_> = old.keys().chain(now.keys()).collect();
        let root_text =
            |root: Option<Root>| root.map_or_else(|| "none".into(), |root| root.to_string());
        for id in ids {
            match (old.get(id), now.get(id)) {
                (None, Some(_)) => out.push(format!("  {id}: added")),
                (Some(_), None) => out.push(format!("  {id}: removed")),
                (Some(old), Some(new)) => {
                    if old.root != new.root {
                        out.push(format!(
                            "  {id}: root {} -> {}",
                            root_text(old.root),
                            root_text(new.root)
                        ));
                    }
                    if old.source != new.source {
                        out.push(format!("  {id}: source {} -> {}", old.source, new.source));
                    }
                    if old.edges != new.edges {
                        out.push(format!(
                            "  {id}: edges {} -> {}",
                            edges_text(&old.edges),
                            edges_text(&new.edges)
                        ));
                    }
                }
                (None, None) => unreachable!(),
            }
        }
    }
    if out.is_empty() {
        out.push("  hirsel.lock is not in the form this hirsel writes".into());
    }
    out
}

pub(crate) struct Lock {
    pending: Option<(String, String)>,
    identity: Option<FileId>,
}

impl Lock {
    pub(crate) fn compare(resolution: &Resolution, mode: Mode) -> Result<Self, String> {
        let captured = Self::capture(&resolution.workspace)?;
        Self::compare_captured(resolution, mode, captured.as_ref())
    }

    pub(crate) fn capture(workspace: &Workspace) -> Result<Option<Captured>, String> {
        match walk(&workspace.root, Path::new(NAME))
            .and_then(|object| object.read(crate::package_content::ARCHIVE_LIMIT))
        {
            Ok(captured) => Ok(Some(captured)),
            Err(WalkError::Missing) => Ok(None),
            Err(error) => Err(format!(
                "{NAME}: {}",
                error.explain("workspace", "a regular file")
            )),
        }
    }

    pub(crate) fn compare_captured(
        resolution: &Resolution,
        mode: Mode,
        captured: Option<&Captured>,
    ) -> Result<Self, String> {
        let rows = make_rows(resolution);
        let derived = encode(&rows);
        let identity = captured.map(|lock| lock.identity);
        let pending = match captured {
            Some(prior) if prior.bytes.as_ref() == derived.as_bytes() => None,
            Some(prior) => {
                let changes = differences(&prior.bytes, &rows).join("\n");
                if let Some(flag) = mode.flag() {
                    return Err(format!("{NAME} is stale under {flag}:\n{changes}"));
                }
                Some((derived, format!("updated {NAME}:\n{changes}")))
            }
            None => {
                if let Some(flag) = mode.flag() {
                    return Err(format!("{NAME} is missing under {flag}; run hirsel lock"));
                }
                Some((derived, format!("created {NAME}")))
            }
        };
        Ok(Self { pending, identity })
    }

    pub(crate) fn pending(&self) -> Option<(&str, &str)> {
        self.pending
            .as_ref()
            .map(|(text, message)| (text.as_str(), message.as_str()))
    }

    pub(crate) fn commit(self, workspace: &Workspace) -> std::io::Result<()> {
        if let Some((text, message)) = self.pending {
            let directory = crate::host::fd_path(&workspace.root)?;
            atomic_write(&directory.join(NAME), text.as_bytes())?;
            eprintln!("hirsel: {message}");
        }
        Ok(())
    }

    fn aliases(
        &self,
        workspace: &Workspace,
        name: &std::ffi::OsStr,
        parent: FileId,
        identity: Option<FileId>,
    ) -> bool {
        (self.identity.is_some() && self.identity == identity)
            || (name == NAME
                && workspace
                    .root
                    .metadata()
                    .is_ok_and(|root| FileId::of(&root) == parent))
    }
}

pub(crate) struct PackOutput {
    directory: std::fs::File,
    name: std::ffi::OsString,
}

impl PackOutput {
    fn path(&self) -> std::io::Result<PathBuf> {
        Ok(crate::host::fd_path(&self.directory)?.join(&self.name))
    }

    pub(crate) fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        atomic_write(&self.path()?, bytes)
    }

    pub(crate) fn stage(
        resolution: &Resolution,
        archive: &crate::package_content::PackageArchive,
        lock: &Lock,
        out: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let parent = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = out
            .file_name()
            .ok_or("--output needs a file name")?
            .to_owned();
        let directory = open_directory(parent)?;
        let parent = FileId::of(&directory.metadata()?);
        let output = Self { directory, name };
        let identity = output.path()?.metadata().ok().map(|meta| FileId::of(&meta));
        let store = &resolution.store;
        let existing_store_alias = identity
            .map(|id| store.identities_include(id))
            .transpose()?
            .unwrap_or(false);
        let new_store_alias = output
            .name
            .to_str()
            .and_then(crate::package_content::parse_digest)
            .map(Root)
            .is_some_and(|root| archive.entries.contains_key(&root))
            && store
                .path()
                .metadata()
                .is_ok_and(|dir| FileId::of(&dir) == parent);
        if identity.is_some_and(|id| resolution.workspace.input_alias(id))
            || lock.aliases(&resolution.workspace, &output.name, parent, identity)
            || existing_store_alias
            || new_store_alias
        {
            return Err(format!("--output {} is an input of this command", out.display()).into());
        }
        Ok(output)
    }
}
