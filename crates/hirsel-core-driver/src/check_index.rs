//! The program's checked product as per-item records under one persistent index.
//!
//! - A **body record** is one checked definition (`DiskCheck`), addressed by the
//!   digest of its envelope: that address is the product's content hash.
//! - The **index** is a 16-way trie over H(DefPath). Its rows are small: each
//!   holds the definition key's hash and the body record's address of a live
//!   definition of the request. No row holds IR, and a path that left the
//!   request has no row, so the root is a function of the live rows and files:
//!   an incremental publication is a fresh one's.
//! - A **file record** holds one source file's module, source hash, namespace
//!   map (name → DefPath) and anchor map (item ordinal → DefPath, line base).
//!   The root lists them in the request's normalised source order, so two
//!   files of one module are two records.
//! - The **root node** is the trie's top node plus the file records and the
//!   key shape its rows' keys were derived under. One selection rename
//!   publishes root, rows and maps together.
//!
//! A record that fails its digest or decode is a miss, never a hit.
use crate::store::Store;
use crate::{decode, encode};
use hirsel_core_ids::{ContentHash, DefPath};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;

/// The selection naming the current root node.
/// `v2`: rows are live definitions only, with no tombstones.
pub(crate) const SELECTION: &str = "program-index-v2";
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
pub(crate) struct Row {
    /// H(the canonical encoding of the `DefinitionKey`).
    pub key: ContentHash,
    /// The body record's address (its content hash).
    pub body: ContentHash,
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
    /// Every row's body record holds its key under this shape.
    shape: ContentHash,
    files: Vec<(ModuleKey, ContentHash)>,
    children: Vec<(u8, ContentHash)>,
}

pub(crate) fn key_hash(key: &crate::DefinitionKey) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(
        "hirsel.definition-key.v1",
        [&encode(key)?[..]],
    ))
}
/// The address of the pointer from a definition's key to its body record.
/// The index holds the rows of the last request alone, so a body another
/// request checked (`run` without test sources, `test` without an entry) is
/// found by its key, whichever request published last.
pub(crate) fn key_pointer(path: &DefPath, key: ContentHash) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(
        "hirsel.definition-key-record.v1",
        [&encode(path)?[..], &key.0[..]],
    ))
}
/// The body record a request checked a body into, read through the body's
/// [`key_pointer`]. The caller verifies the record's own path and key; a
/// damaged pointer is a miss.
pub(crate) fn keyed_record(store: &Store, pointer: ContentHash) -> io::Result<Option<ContentHash>> {
    Ok(store
        .get(pointer)?
        .and_then(|bytes| Some(ContentHash(bytes.try_into().ok()?))))
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
    /// The key shape of the selected root.
    pub shape: Option<ContentHash>,
    rows: FxHashMap<DefPath, Row>,
    files: Vec<(ModuleKey, ContentHash)>,
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
        index.shape = Some(node.shape);
        index.published.insert(root);
        for (module, file) in node.files {
            index.published.insert(file);
            index.files.push((module, file));
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
    /// The row of a live definition.
    pub fn live(&self, path: &DefPath) -> Option<(ContentHash, ContentHash)> {
        self.rows.get(path).map(|row| (row.key, row.body))
    }
    /// The published file record of each source file, in source order.
    #[cfg(test)]
    pub fn files(&self) -> Vec<(ModuleKey, ContentHash)> {
        self.files.clone()
    }
    pub fn rows(&self) -> impl Iterator<Item = (&DefPath, &Row)> {
        self.rows.iter()
    }
}

/// Records of one request, written before its single selection rename.
pub(crate) struct Publication {
    /// The key shape every row's key was derived under.
    pub shape: ContentHash,
    /// Body records framed when their body was checked.
    pub bodies: Vec<(ContentHash, Vec<u8>)>,
    pub rows: BTreeMap<DefPath, Row>,
    /// Each body checked by this request: its [`key_pointer`] and its record.
    pub keyed: Vec<(ContentHash, ContentHash)>,
    /// One record per source file, in normalised source order.
    pub files: Vec<FileRecord>,
}
impl Publication {
    pub fn new() -> Self {
        Self {
            shape: ContentHash::default(),
            bodies: vec![],
            rows: BTreeMap::new(),
            keyed: vec![],
            files: vec![],
        }
    }
    /// Write what the prior publication does not hold, then rename the selection.
    /// Returns the root node's address, a deterministic function of this
    /// request's rows and files alone.
    pub fn publish(mut self, store: &Store, prior: &ProgramIndex) -> io::Result<ContentHash> {
        hirsel_core_front::cost_scope!("non-check", "program index publication");
        let mut writes: Vec<(ContentHash, Vec<u8>)> = std::mem::take(&mut self.bodies);
        let mut files = Vec::with_capacity(self.files.len());
        for record in &self.files {
            let (hash, bytes) = Store::frame(&encode(record)?);
            if !prior.published.contains(&hash) {
                writes.push((hash, bytes));
            }
            files.push((record.module.clone(), hash));
        }
        let mut hashed = Vec::with_capacity(self.rows.len());
        for (path, row) in self.rows {
            hashed.push((path_hash(&path)?, path, row));
        }
        hashed.sort_unstable_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let children = children(&hashed, 0, &mut writes, prior)?;
        let (root, bytes) = Store::frame(&encode(&RootNode {
            shape: self.shape,
            files,
            children,
        })?);
        // Rechecked bodies are always written: a recheck after a damaged read
        // yields the same address, and the damaged bytes must be replaced.
        let changed = prior.root != Some(root);
        if changed {
            writes.push((root, bytes));
        }
        for (hash, bytes) in &writes {
            store.write_framed(*hash, bytes)?;
        }
        // After the records they name.
        for (pointer, record) in &self.keyed {
            store.put_keyed(*pointer, &record.0)?;
        }
        if changed {
            store.select(SELECTION, root)?;
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
