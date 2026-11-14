//! The program's checked product as per-item records under one persistent index.
//!
//! - A **body record** is one checked definition (`DiskCheck`), addressed by the
//!   digest of its envelope: that address is the product's content hash.
//! - The **index** is a 16-way trie over H(DefPath). Its rows are small: a live row
//!   holds the definition key's hash and the body record's address; a tombstone
//!   marks a path that left a file of the request. No row holds IR.
//! - A **file record** holds a module's source hash, its namespace map
//!   (name → DefPath) and its anchor map (item ordinal → DefPath, line base).
//! - The **root node** is the trie's top node plus the file records. One
//!   selection rename publishes root, rows, tombstones and maps together.
//!
//! Every lookup (by DefPath, by name, by anchor) treats a tombstone as not found.
//! A record that fails its digest or decode is a miss, never a hit.
use crate::store::Store;
use crate::{decode, encode};
use hirsel_core_ids::{ContentHash, DefPath};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;

/// The selection naming the current root node.
pub(crate) const SELECTION: &str = "program-index";
/// A leaf splits into an inner node above this many rows. With about 500
/// rows a changed row rewrites its leaf and the root: two nodes.
const LEAF_ROWS: usize = 48;

/// A module's identity across requests: package name, version and module names.
pub(crate) type ModuleKey = (String, String, Vec<String>);
pub(crate) fn module_key(path: &DefPath) -> ModuleKey {
    let p = &path.0;
    (
        p.package.name.clone(),
        p.package.version.clone(),
        p.modules.clone(),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum Row {
    Live {
        /// H(the canonical encoding of the `DefinitionKey`).
        key: ContentHash,
        /// The body record's address (its content hash).
        body: ContentHash,
    },
    /// The path left a file of the request: the file record that dropped it.
    Removed { file: Option<ContentHash> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileRecord {
    pub module: ModuleKey,
    pub source: ContentHash,
    pub names: BTreeMap<String, DefPath>,
    /// Item ordinal → (definition, zero-based line of the item's first byte).
    pub anchors: BTreeMap<u32, (DefPath, u32)>,
}

#[derive(Serialize, Deserialize)]
enum Node {
    Inner(Vec<(u8, ContentHash)>),
    Leaf(Vec<(DefPath, Row)>),
}
#[derive(Serialize, Deserialize)]
struct RootNode {
    files: Vec<(ModuleKey, ContentHash)>,
    children: Vec<(u8, ContentHash)>,
}

pub(crate) fn key_hash(key: &crate::DefinitionKey) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(
        "hirsel.definition-key.v1",
        [&encode(key)?[..]],
    ))
}
fn path_hash(path: &DefPath) -> io::Result<[u8; 32]> {
    Ok(ContentHash::of_parts("hirsel.program-index.path.v1", [&encode(path)?[..]]).0)
}
fn nibble(hash: &[u8; 32], depth: usize) -> u8 {
    let byte = hash[depth / 2];
    if depth % 2 == 0 { byte >> 4 } else { byte & 15 }
}

/// The prior publication as read at the start of a request.
#[derive(Default)]
pub(crate) struct ProgramIndex {
    pub root: Option<ContentHash>,
    rows: FxHashMap<DefPath, Row>,
    files: BTreeMap<ModuleKey, ContentHash>,
    /// Nodes and file records this publication already holds, verified or referenced.
    published: FxHashSet<ContentHash>,
}
impl ProgramIndex {
    /// Read the whole index: it holds no IR, only rows. A damaged or missing
    /// node is a miss for the rows below it.
    pub fn open(store: &Store) -> io::Result<Self> {
        let mut index = Self::default();
        let Some(root) = store.selection(SELECTION)? else {
            return Ok(index);
        };
        let Some(node) = store.get(root)?.and_then(|b| decode::<RootNode>(&b)) else {
            return Ok(index);
        };
        index.root = Some(root);
        index.published.insert(root);
        for (module, file) in node.files {
            index.published.insert(file);
            index.files.insert(module, file);
        }
        let mut pending: Vec<ContentHash> = node.children.into_iter().map(|c| c.1).collect();
        while let Some(hash) = pending.pop() {
            match store.get(hash)?.and_then(|b| decode::<Node>(&b)) {
                Some(Node::Inner(children)) => pending.extend(children.into_iter().map(|c| c.1)),
                Some(Node::Leaf(rows)) => index.rows.extend(rows),
                None => continue,
            }
            index.published.insert(hash);
        }
        Ok(index)
    }
    /// A live row, never a tombstone.
    pub fn live(&self, path: &DefPath) -> Option<(ContentHash, ContentHash)> {
        match self.rows.get(path)? {
            &Row::Live { key, body } => Some((key, body)),
            Row::Removed { .. } => None,
        }
    }
    /// The published file record of each module.
    #[cfg(test)]
    pub fn files(&self) -> Vec<(ModuleKey, ContentHash)> {
        self.files.iter().map(|(m, h)| (m.clone(), *h)).collect()
    }
    pub fn rows(&self) -> impl Iterator<Item = (&DefPath, &Row)> {
        self.rows.iter()
    }
    pub fn file(&self, store: &Store, module: &ModuleKey) -> io::Result<Option<FileRecord>> {
        let Some(&hash) = self.files.get(module) else {
            return Ok(None);
        };
        Ok(store.get(hash)?.and_then(|b| decode(&b)))
    }
    /// A name in a module's namespace, resolved to its live row.
    pub fn by_name(
        &self,
        store: &Store,
        module: &ModuleKey,
        name: &str,
    ) -> io::Result<Option<DefPath>> {
        Ok(self
            .file(store, module)?
            .and_then(|f| f.names.get(name).cloned())
            .filter(|p| self.live(p).is_some()))
    }
    /// An item ordinal of a module, resolved to its live row and line base.
    pub fn by_anchor(
        &self,
        store: &Store,
        module: &ModuleKey,
        ordinal: u32,
    ) -> io::Result<Option<(DefPath, u32)>> {
        Ok(self
            .file(store, module)?
            .and_then(|f| f.anchors.get(&ordinal).cloned())
            .filter(|(p, _)| self.live(p).is_some()))
    }
}

/// Records of one request, written before its single selection rename.
pub(crate) struct Publication {
    /// Body records framed when their body was checked.
    pub bodies: Vec<(ContentHash, Vec<u8>)>,
    pub rows: BTreeMap<DefPath, Row>,
    pub files: BTreeMap<ModuleKey, FileRecord>,
}
impl Publication {
    pub fn new() -> Self {
        Self {
            bodies: vec![],
            rows: BTreeMap::new(),
            files: BTreeMap::new(),
        }
    }
    /// Write what the prior publication does not hold, then rename the selection.
    /// Returns the root node's address, a deterministic function of the rows and files.
    pub fn publish(mut self, store: &Store, prior: &ProgramIndex) -> io::Result<ContentHash> {
        hirsel_core_front::cost_scope!("non-check", "program index publication");
        let mut writes: Vec<(ContentHash, Vec<u8>)> = std::mem::take(&mut self.bodies);
        let mut files = Vec::with_capacity(self.files.len());
        for (module, record) in &self.files {
            let (hash, bytes) = Store::frame(&encode(record)?);
            if !prior.published.contains(&hash) {
                writes.push((hash, bytes));
            }
            files.push((module.clone(), hash));
        }
        // Paths that left a file of this request leave a tombstone; earlier
        // tombstones stay while their file is still part of the request.
        for (path, row) in prior.rows() {
            if self.rows.contains_key(path) {
                continue;
            }
            let module = module_key(path);
            if self.files.contains_key(&module) {
                let removed = match row {
                    Row::Live { .. } => Row::Removed {
                        file: files
                            .binary_search_by(|f| f.0.cmp(&module))
                            .ok()
                            .map(|i| files[i].1),
                    },
                    removed => *removed,
                };
                self.rows.insert(path.clone(), removed);
            }
        }
        let mut hashed = Vec::with_capacity(self.rows.len());
        for (path, row) in self.rows {
            hashed.push((path_hash(&path)?, path, row));
        }
        hashed.sort_unstable_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let children = children(&hashed, 0, &mut writes, prior)?;
        let (root, bytes) = Store::frame(&encode(&RootNode { files, children })?);
        // Rechecked bodies are always written: a recheck after a damaged read
        // yields the same address, and the damaged bytes must be replaced.
        let changed = prior.root != Some(root);
        if changed {
            writes.push((root, bytes));
        }
        for (hash, bytes) in &writes {
            store.write_framed(*hash, bytes)?;
        }
        if changed {
            store.sync_directory()?;
            store.select(SELECTION, root)?;
        } else if !writes.is_empty() {
            store.sync_directory()?;
        }
        Ok(root)
    }
}
fn children(
    rows: &[([u8; 32], DefPath, Row)],
    depth: usize,
    writes: &mut Vec<(ContentHash, Vec<u8>)>,
    prior: &ProgramIndex,
) -> io::Result<Vec<(u8, ContentHash)>> {
    let mut out = vec![];
    let mut rest = rows;
    while let Some(first) = rest.first() {
        let n = nibble(&first.0, depth);
        let split = rest.partition_point(|r| nibble(&r.0, depth) == n);
        let (group, tail) = rest.split_at(split);
        rest = tail;
        let node = if group.len() <= LEAF_ROWS || depth + 1 == 64 {
            Node::Leaf(group.iter().map(|r| (r.1.clone(), r.2)).collect())
        } else {
            Node::Inner(children(group, depth + 1, writes, prior)?)
        };
        let (hash, bytes) = Store::frame(&encode(&node)?);
        if !prior.published.contains(&hash) {
            writes.push((hash, bytes));
        }
        out.push((n, hash));
    }
    Ok(out)
}
/// Republish the selected root with its file references and one-level leaves
/// edited: every record it writes is valid, only what references what changes.
#[cfg(test)]
pub(crate) fn forge(
    store: &Store,
    edit: impl FnOnce(&mut Vec<(ModuleKey, ContentHash)>, &mut Vec<(u8, Vec<(DefPath, Row)>)>),
) {
    let root = store.selection(SELECTION).unwrap().unwrap();
    let mut node: RootNode = decode(&store.get(root).unwrap().unwrap()).unwrap();
    let mut leaves: Vec<(u8, Vec<(DefPath, Row)>)> = node
        .children
        .iter()
        .map(
            |&(n, hash)| match decode(&store.get(hash).unwrap().unwrap()).unwrap() {
                Node::Leaf(rows) => (n, rows),
                Node::Inner(_) => panic!("a forged index has one level"),
            },
        )
        .collect();
    edit(&mut node.files, &mut leaves);
    node.children = leaves
        .into_iter()
        .map(|(n, rows)| {
            let (hash, bytes) = Store::frame(&encode(&Node::Leaf(rows)).unwrap());
            store.write_framed(hash, &bytes).unwrap();
            (n, hash)
        })
        .collect();
    node.children.sort_unstable();
    let (root, bytes) = Store::frame(&encode(&node).unwrap());
    store.write_framed(root, &bytes).unwrap();
    store.select(SELECTION, root).unwrap();
}
