//! The reach record: one per native request tuple, the walk's result over the
//! rows it consulted. A request whose consulted rows are all equal to the
//! record's takes the walk's result from it and reads no instance record; a
//! request with changed rows expands its unchanged instances from the record's
//! table and walks the rest as instance records do.
//!
//! The law. The walk is a function of the request tuple (the selection) and of
//! the rows it consulted, absent ones included. It reads the program index only
//! through [`WalkRows`], whose every accessor records the row it returns, so
//! the vector is complete by construction. The instance walk reads only the
//! rows of the definitions it instantiates, and each of those is a root of the
//! header walk, so the header walk records them. A record is used only if
//! every vector row equals the live row and the LANG-20 constant set is equal;
//! an unchanged row keeps its instance's table entry, a function of
//! `(env, identity, own)` by the instance record's rule.
use super::*;
use crate::instances::Location;
use crate::native::EntryAbi;
use crate::store::Bare;
use hirsel_core_ir::{Closure, Identity};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const REACH_SCHEMA: u32 = 2;

/// One consulted definition's row: its index identity, the request's body
/// record for it, and its header when the request owns that header. An image
/// definition's header and body are the image's, which the selection names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Row {
    id: Option<u32>,
    own: Option<ContentHash>,
    header: Option<crate::reuse::Header>,
}

/// A node of the header walk, a body or a declaration of a vector row, with
/// the nodes it reaches and whether its body is unavailable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Node {
    pub row: u32,
    pub body: bool,
    pub unavailable: bool,
    pub edges: Vec<(u32, bool)>,
}

/// The header walk's nodes, each with its edges and whether it is unavailable.
pub(crate) type Graph = BTreeMap<(DefPath, bool), (Vec<(DefPath, bool)>, bool)>;

/// One instance of the table, in link order: the instance record's fields
/// with its children and closures as indices, and its items as indices of
/// the record's paths.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub identity: Identity,
    pub own: Option<ContentHash>,
    pub location: Location,
    pub object: Bare,
    pub debug: Option<ContentHash>,
    pub children: Vec<u32>,
    pub items: Vec<u32>,
    pub closures: Vec<u32>,
}

/// An instance of a new record's table, before its paths are numbered.
#[derive(Clone, Debug)]
pub(crate) struct Fresh {
    pub identity: Identity,
    pub own: Option<ContentHash>,
    pub location: Location,
    pub object: Bare,
    pub debug: Option<ContentHash>,
    pub children: Vec<u32>,
    pub items: Vec<DefPath>,
    pub closures: Vec<u32>,
}

/// The entry's result cleanup object, in the request's store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Cleanup {
    pub identity: Identity,
    pub object: Bare,
    pub debug: Option<ContentHash>,
}

/// What the entry record gives the link: the ABI and the cleanup object. The
/// entry record is a function of the selection and the entry's row.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EntryLink {
    pub abi: EntryAbi,
    pub cleanup: Option<Cleanup>,
}

pub(crate) struct Reach {
    /// Every path the record names: the vector's first, sorted, so row `i`
    /// is of `paths[i]`; identities and items name theirs by index.
    pub paths: Vec<DefPath>,
    /// The vector's rows, in path order.
    pub rows: Vec<Row>,
    pub constants: Vec<DefPath>,
    /// The header walk's nodes, sorted by row and kind.
    pub graph: Vec<Node>,
    /// The reached instances in link order, which is identity order.
    pub instances: Vec<Entry>,
    pub closures: Vec<Arc<Closure>>,
    pub entry: EntryLink,
    /// [`sources_digest`] of the instances' items.
    pub sources: ContentHash,
    /// [`files_digest`] of the request that wrote it: a hit whose request
    /// files are these has these sources.
    pub files: ContentHash,
}

/// The record's bytes: each path once, every other path an index of it, in
/// bincode's variable-length integers. Each parameter is owned when read and
/// borrowed when written.
#[derive(Serialize, Deserialize)]
struct Stored<P, R, E, N, C, L> {
    schema: u32,
    paths: Vec<P>,
    rows: Vec<R>,
    constants: Vec<u32>,
    graph: N,
    instances: Vec<E>,
    closures: C,
    entry: L,
    sources: ContentHash,
    files: ContentHash,
}
/// One table instance as stored: its identity's definition by index.
#[derive(Serialize, Deserialize)]
struct StoredEntry<S, H, U> {
    definition: u32,
    dictionaries: S,
    closure_hashes: H,
    own: Option<ContentHash>,
    location: Location,
    object: Bare,
    debug: Option<ContentHash>,
    children: U,
    items: Vec<u32>,
    closures: U,
}
type Read = Stored<
    DefPath,
    Row,
    StoredEntry<Vec<String>, Vec<ContentHash>, Vec<u32>>,
    Vec<Node>,
    Vec<Arc<Closure>>,
    EntryLink,
>;
fn options() -> impl bincode::Options {
    use bincode::Options;
    bincode::DefaultOptions::new().with_varint_encoding()
}
pub(crate) fn decode_record(bytes: &[u8]) -> Option<Reach> {
    use bincode::Options;
    let read: Read = options()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()?;
    if read.schema != REACH_SCHEMA {
        return None;
    }
    crate::meter::count(crate::meter::Count::RecordRowsRead, read.rows.len() as u64);
    let path = |i: u32| read.paths.get(i as usize);
    let instances = read
        .instances
        .into_iter()
        .map(|e| {
            Some(Entry {
                identity: Identity {
                    definition: path(e.definition)?.clone(),
                    dictionaries: e.dictionaries,
                    closures: e.closure_hashes,
                },
                own: e.own,
                location: e.location,
                object: e.object,
                debug: e.debug,
                children: e.children,
                items: e.items,
                closures: e.closures,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let constants = read
        .constants
        .iter()
        .map(|&i| path(i).cloned())
        .collect::<Option<Vec<_>>>()?;
    let count = read.paths.len() as u32;
    let indices_in_range = read.rows.len() <= read.paths.len()
        && instances.iter().all(|e| e.items.iter().all(|&i| i < count));
    indices_in_range.then_some(Reach {
        paths: read.paths,
        rows: read.rows,
        constants,
        graph: read.graph,
        instances,
        closures: read.closures,
        entry: read.entry,
        sources: read.sources,
        files: read.files,
    })
}

/// The work counters of one request's walk and link, in the stage meter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReachCounters {
    /// Instances expanded from the reach record's table.
    pub expanded: u64,
    /// Instances on the instance-record path, hit or miss.
    pub instance_path: u64,
    /// Stored objects read and checked through their descriptor for a link.
    pub objects_verified: u64,
    /// Stored objects whose bytes failed their check: removed and rebuilt.
    pub objects_refused: u64,
    /// The bytes of the reach record read or written.
    pub record_bytes: u64,
    /// The vector's length.
    pub vector: u64,
}

/// The program-index view the walk reads through.
pub(crate) struct WalkRows<'a> {
    product: &'a CheckProduct,
    base: usize,
    /// Asked once per header the walk reads, so hashed; sorted once, by
    /// [`Draft::walked`].
    pub(crate) rows: rustc_hash::FxHashMap<DefPath, Row>,
}
impl<'a> WalkRows<'a> {
    pub(crate) fn new(product: &'a CheckProduct) -> Self {
        Self {
            product,
            base: product.index.definitions.base_len(),
            rows: Default::default(),
        }
    }
    fn record(&mut self, path: &DefPath) {
        if !self.rows.contains_key(path) {
            let row = live(self.product, self.base, path);
            self.rows.insert(path.clone(), row);
        }
    }
    pub(crate) fn header(&mut self, path: &DefPath) -> Option<&'a crate::reuse::Header> {
        self.record(path);
        self.product.headers.get(path)
    }
    pub(crate) fn defined(&mut self, path: &DefPath) -> bool {
        self.record(path);
        self.product.definitions.contains_key(path)
    }
}
fn live(product: &CheckProduct, base: usize, path: &DefPath) -> Row {
    let id = product.headers.id(path);
    Row {
        id: id.map(|id| id.0),
        own: product.definitions.get(path).map(|d| d.record),
        header: id
            .filter(|id| id.0 as usize >= base)
            .and_then(|id| product.headers.row(id).cloned()),
    }
}
/// Whether `row`, recorded for `path`, is the live row.
fn current(product: &CheckProduct, base: usize, path: &DefPath, row: &Row) -> bool {
    let id = match row.id {
        Some(id) => Some(hirsel_core_resolve::DefinitionId(id))
            .filter(|&id| product.headers.names_recorded(base, id, path)),
        None => product.headers.id(path),
    };
    id.map(|id| id.0) == row.id
        && product.definitions.get(path).map(|d| d.record) == row.own
        && match id.filter(|id| id.0 as usize >= base) {
            Some(id) => product.headers.row(id) == row.header.as_ref(),
            None => row.header.is_none(),
        }
}

impl Reach {
    /// Whether each vector row is not the live row.
    pub(crate) fn changed(&self, product: &CheckProduct) -> Vec<bool> {
        let base = product.index.definitions.base_len();
        self.rows
            .iter()
            .zip(&self.paths)
            .map(|(row, path)| !current(product, base, path, row))
            .collect()
    }
    /// The path of vector row `row`.
    pub(crate) fn row_path(&self, row: usize) -> &DefPath {
        &self.paths[row]
    }
    pub(crate) fn row(&self, path: &DefPath) -> Option<usize> {
        self.paths[..self.rows.len()].binary_search(path).ok()
    }
    /// The items of `entry`.
    pub(crate) fn items<'r>(&'r self, entry: &'r Entry) -> impl Iterator<Item = &'r DefPath> {
        entry.items.iter().map(|&i| &self.paths[i as usize])
    }
    pub(crate) fn instance(&self, identity: &Identity) -> Option<usize> {
        self.instances
            .binary_search_by(|e| e.identity.cmp(identity))
            .ok()
    }
    pub(crate) fn node(&self, path: &DefPath, body: bool) -> Option<&Node> {
        let row = self.row(path)? as u32;
        self.graph
            .binary_search_by(|n| (n.row, n.body).cmp(&(row, body)))
            .ok()
            .map(|i| &self.graph[i])
    }
    pub(crate) fn closures_of<'r>(
        &'r self,
        entry: &'r Entry,
    ) -> impl Iterator<Item = &'r Arc<Closure>> {
        entry.closures.iter().map(|&i| &self.closures[i as usize])
    }
    /// The definitions the table instantiates, glue excluded.
    pub(crate) fn definitions(&self) -> BTreeSet<&DefPath> {
        self.instances
            .iter()
            .map(|e| &e.identity.definition)
            .filter(|d| !hirsel_core_front::is_glue(d))
            .collect()
    }
    pub(crate) fn counts(&self) -> (usize, usize) {
        let bodies = self.graph.iter().filter(|n| n.body).count();
        (bodies, self.graph.len() - bodies)
    }
    /// A node's edges, as the walk names them.
    pub(crate) fn edges(&self, node: &Node) -> Vec<(DefPath, bool)> {
        node.edges
            .iter()
            .map(|&(row, body)| (self.row_path(row as usize).clone(), body))
            .collect()
    }
}

/// The walk's part of a new record: the vector, the constant set and the
/// header graph. A walk that reused the previous record's graph keeps its
/// vector with the changed rows replaced.
pub(crate) enum Draft {
    Walked {
        rows: Vec<(DefPath, Row)>,
        graph: Vec<Node>,
    },
    Patched {
        rows: Vec<(usize, Row)>,
    },
}
impl Draft {
    pub(crate) fn walked(rows: rustc_hash::FxHashMap<DefPath, Row>, graph: &Graph) -> Self {
        let mut rows: Vec<_> = rows.into_iter().collect();
        rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let index = |path: &DefPath| -> u32 {
            rows.binary_search_by(|(p, _)| p.cmp(path))
                .expect("the walk records every node it reaches") as u32
        };
        let graph = graph
            .iter()
            .map(|((path, body), (edges, unavailable))| Node {
                row: index(path),
                body: *body,
                unavailable: *unavailable,
                edges: edges.iter().map(|(p, b)| (index(p), *b)).collect(),
            })
            .collect();
        Draft::Walked { rows, graph }
    }
    /// The vector's length.
    pub(crate) fn len(&self, prior: Option<&Reach>) -> usize {
        match self {
            Draft::Walked { rows, .. } => rows.len(),
            Draft::Patched { .. } => prior.map_or(0, |p| p.rows.len()),
        }
    }
}

/// A Cranelift development request's use of its reach record.
pub(crate) struct Use {
    pub selection: ContentHash,
    pub prior: Option<Reach>,
    /// Every row is live and the constant set is equal: the walk's result is
    /// the record's, and nothing is written.
    pub hit: bool,
    /// The entry's row is live, so the record's entry link holds.
    pub entry_live: bool,
    pub constants: Vec<DefPath>,
    /// The new record's walk part, when the walk was accepted and complete.
    pub draft: Option<Draft>,
}

/// The patched vector of `prior`: each changed row replaced by the live row.
pub(crate) fn patched(product: &CheckProduct, prior: &Reach, changed: &[bool]) -> Draft {
    let base = product.index.definitions.base_len();
    Draft::Patched {
        rows: changed
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(|(i, _)| (i, live(product, base, prior.row_path(i))))
            .collect(),
    }
}

/// The selection: `hirsel.core-reach.v2` over the environment digest, the
/// image, the entry and its declaration part, whether it is a test, the
/// entry policy and the instance limit.
pub(crate) fn selection(
    product: &CheckProduct,
    records: &crate::instances::Records,
    entry: &DefPath,
    options: &NativeOptions,
) -> io::Result<Option<ContentHash>> {
    let decl = product
        .native_entry
        .as_ref()
        .filter(|(path, _)| path == entry)
        .map(|(_, decl)| (decl, false))
        .or_else(|| product.test_entries.get(entry).map(|decl| (decl, true)));
    let Some((decl, test)) = decl else {
        return Ok(None);
    };
    crate::native::key(
        "hirsel.core-reach.v2",
        &(
            records.digest,
            product.image.as_ref().map(|i| i.identity),
            entry,
            decl,
            test,
            crate::native_entry::policy(options),
            options.instance_limit,
        ),
    )
    .map(Some)
}

/// The record stored under `key`, and its size; a damaged or foreign one
/// is a miss.
pub(crate) fn load(store: &Store, key: ContentHash) -> io::Result<Option<(Reach, usize)>> {
    let Some(bytes) = store.get(key)? else {
        return Ok(None);
    };
    Ok(decode_record(&bytes).map(|r| (r, bytes.len())))
}

/// Write the record of `draft` with its table, entry link and sources under
/// `key`; its size.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    store: &Store,
    key: ContentHash,
    prior: Option<&Reach>,
    constants: &[DefPath],
    draft: &Draft,
    instances: &[Fresh],
    closures: &[Arc<Closure>],
    entry: &EntryLink,
    (sources, files): (ContentHash, ContentHash),
) -> io::Result<usize> {
    use bincode::Options;
    let (rows, graph): (Vec<(&DefPath, &Row)>, &[Node]) = match draft {
        Draft::Walked { rows, graph } => (rows.iter().map(|(p, r)| (p, r)).collect(), graph),
        Draft::Patched { rows: patch } => {
            let prior = prior.expect("a patched vector has its record");
            let mut rows: Vec<_> = (0..prior.rows.len())
                .map(|i| (prior.row_path(i), &prior.rows[i]))
                .collect();
            for (i, row) in patch {
                rows[*i].1 = row;
            }
            (rows, &prior.graph)
        }
    };
    // Paths are numbered in first-use order: the rows' (sorted and
    // distinct) by position, then each other path as it is first named.
    let mut paths: Vec<&DefPath> = rows.iter().map(|(p, _)| *p).collect();
    let mut numbers: rustc_hash::FxHashMap<&DefPath, u32> = Default::default();
    numbers.reserve(paths.len() + instances.len());
    numbers.extend(paths.iter().enumerate().map(|(i, p)| (*p, i as u32)));
    let mut index = |path| {
        *numbers.entry(path).or_insert_with(|| {
            paths.push(path);
            paths.len() as u32 - 1
        })
    };
    let constants: Vec<u32> = constants.iter().map(&mut index).collect();
    let entries: Vec<_> = instances
        .iter()
        .map(|e| StoredEntry {
            definition: index(&e.identity.definition),
            dictionaries: &e.identity.dictionaries,
            closure_hashes: &e.identity.closures,
            own: e.own,
            location: e.location,
            object: e.object,
            debug: e.debug,
            children: &e.children,
            items: e.items.iter().map(&mut index).collect(),
            closures: &e.closures,
        })
        .collect();
    let stored = Stored {
        schema: REACH_SCHEMA,
        rows: rows.iter().map(|(_, r)| *r).collect(),
        constants,
        graph,
        instances: entries,
        closures,
        entry,
        sources,
        files,
        paths,
    };
    let bytes = options().serialize(&stored).map_err(io::Error::other)?;
    store.put_keyed(key, &bytes)?;
    Ok(bytes.len())
}

/// What a source map rests on besides the image (which the record's key
/// names) and its items: each request file's module, name and text. Equal
/// files give each item the same module and offset.
pub(crate) fn files_digest(product: &CheckProduct) -> io::Result<ContentHash> {
    let files: Vec<_> = product
        .files
        .iter()
        .map(|f| (f.module, product.source_name(f.module), f.source))
        .collect();
    crate::native::key("hirsel.core-reach-files.v1", &files)
}

/// The digest of the source map `source_map(rows)` builds: each item's path,
/// file and offset, and each file's name and text. A request file's text is
/// its bytes' hash; an image file's is the image's.
pub(crate) fn sources_digest(
    product: &CheckProduct,
    rows: &[crate::native_sources::SourceRow<'_>],
) -> io::Result<ContentHash> {
    let mut modules = BTreeMap::new();
    for row in rows {
        modules.entry(row.module).or_insert_with(|| {
            let own = product.files.iter().find(|file| file.module == row.module);
            let text = match own {
                Some(file) => file.source,
                None => product.image.as_ref().unwrap().identity,
            };
            (product.source_name(row.module), own.is_some(), text)
        });
    }
    let rows: Vec<_> = rows
        .iter()
        .map(|r| (r.path, r.module, r.file_offset))
        .collect();
    crate::native::key(
        "hirsel.core-source-map.v1",
        &(rows, modules.into_iter().collect::<Vec<_>>()),
    )
}
