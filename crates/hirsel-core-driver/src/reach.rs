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
//!
//! The record is one keyed frame of fixed sections (see [`Section`]): a head
//! ([`Head`]) readable alone, then offset tables, read in place. A request
//! the request record answers never reads it; any other compares its rows
//! and constants against the live product. The bytes are a
//! function of the content, written in canonical order from the current
//! request, so a patched record is a fresh one byte for byte.
//!
//! A record is published only after its executable is stored, and only for
//! an accepted, complete walk: a refused, interrupted or damaged build
//! publishes none, and a failed output write leaves it in place. A changed
//! head is a new generation, the prior sections under the new head, written
//! by the store's one rename; no section is rewritten in place.
use super::*;
use crate::instances::{Adapter, CodeSignatures, Location};
use crate::native::EntryAbi;
use crate::store::Bare;
use hirsel_core_ir::{Closure, Identity};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

pub(crate) const REACH_SCHEMA: u32 = 13;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub row: u32,
    pub body: bool,
    pub unavailable: bool,
    pub edges: Vec<(u32, bool)>,
}

/// The header walk's nodes, each with its edges and whether it is unavailable.
pub(crate) type Graph = BTreeMap<(DefPath, bool), (Vec<(DefPath, bool)>, bool)>;

/// One instance of the table, in link order: the instance record's fields,
/// its children, closures and items (of the record's paths) as indices.
#[derive(Clone)]
pub(crate) struct Entry {
    pub identity: Identity,
    pub own: Option<ContentHash>,
    /// The instance's PROG-10 body hash.
    pub body: ContentHash,
    pub location: Location,
    pub object: Bare,
    pub debug: Option<Bare>,
    pub children: Vec<u32>,
    pub slots: Vec<Identity>,
    pub signatures: CodeSignatures,
    pub adapters: Vec<Adapter>,
    /// The instances it describes, as table indices.
    pub described: Vec<u32>,
    pub items: Vec<u32>,
    pub closures: Vec<u32>,
}

/// An instance of a new record's table, before its paths are numbered.
#[derive(Clone)]
pub(crate) struct Fresh {
    pub identity: Identity,
    pub own: Option<ContentHash>,
    /// The instance's PROG-10 body hash.
    pub body: ContentHash,
    pub location: Location,
    pub object: Bare,
    pub debug: Option<Bare>,
    pub children: Vec<u32>,
    pub slots: Vec<Identity>,
    pub signatures: CodeSignatures,
    pub adapters: Vec<Adapter>,
    /// The instances it describes, as table indices.
    pub described: Vec<u32>,
    pub items: Vec<DefPath>,
    pub closures: Vec<u32>,
}

/// The entry's result cleanup object, with its record's key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Cleanup {
    pub identity: Identity,
    pub key: ContentHash,
    pub object: Bare,
    pub debug: Option<Bare>,
}

/// What the entry record gives the link: the ABI and the cleanup object. The
/// entry record is a function of the selection and the entry's row.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EntryLink {
    pub abi: EntryAbi,
    pub cleanup: Option<Cleanup>,
}

/// One table instance as stored: its identity's definition by path index.
/// Each parameter is owned when read and borrowed when written.
#[derive(Serialize, Deserialize)]
struct StoredEntry<S, H, U, Slots, Signatures, Adapters> {
    definition: u32,
    dictionaries: S,
    closure_hashes: H,
    own: Option<ContentHash>,
    body: ContentHash,
    location: Location,
    object: Bare,
    debug: Option<Bare>,
    children: U,
    slots: Slots,
    signatures: Signatures,
    adapters: Adapters,
    described: U,
    items: U,
    closures: U,
}

/// The record's sections, in file order.
#[derive(Clone, Copy)]
enum Section {
    /// Every path the record names, as a vector: the vector's rows' paths
    /// first, sorted, so row `i` is of path `i`; the rest in first-use order.
    Paths,
    /// The vector's rows, as a vector of encoded [`Row`]s in path order.
    Rows,
    /// The LANG-20 constant set: fixed u32 path indices.
    Constants,
    /// The header walk's nodes sorted by row and kind, compressed rows:
    /// `n`, then per node `(row, flags, first edge)`, then the edge count and
    /// each edge as `row << 1 | body`.
    Graph,
    /// The reached instances in link order (identity order), as a vector of
    /// encoded [`Entry`]s: the object table and each instance's links.
    Instances,
    /// The closures the instances name, as a vector, in first-use order.
    Closures,
    /// The entry link, encoded.
    Link,
    /// Each path's source place in the link that wrote the record: fixed
    /// `(placed, module, file, file offset)` u32 quadruples by path index.
    Places,
    /// Counted foreign reach summary: definition path indices in identity order.
    Foreign,
}
const SECTIONS: usize = 9;
/// The bytes of one [`Section::Places`] row.
const PLACE: usize = 16;
/// The head: schema, [`Head`], and each section's offset and length in the
/// payload, u32 little-endian.
const HEAD: usize = HEAD_SECTIONS + SECTIONS * 8;
/// Where the head's section table starts.
pub(crate) const HEAD_SECTIONS: usize = 4 + HEAD_HASHES * 32 + 8;
const HEAD_HASHES: usize = 10;

/// What a record was built from and what it built, readable without its
/// sections. Its build acceptance is its existence: only an accepted,
/// complete walk publishes a record. The native selection's inputs are the
/// environment digest (compiler, runtime, target and options), the toolchain
/// and recipe identities, the child tools' environment, the entry policy and
/// the instance limit; the selection key holds the environment, the policy
/// and the limit, and the head holds them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    /// [`sources_digest`] of the instances' items.
    pub sources: ContentHash,
    /// [`files_digest`] of the request that wrote it: a hit whose request
    /// files are these has these sources.
    pub files: ContentHash,
    /// The check root the build was checked against.
    pub verified: ContentHash,
    /// The key of the executable it linked.
    pub executable: ContentHash,
    /// The instance records' environment digest.
    pub environment: ContentHash,
    /// The toolchain image and the link recipe's identity.
    pub toolchain: ContentHash,
    pub recipe: ContentHash,
    /// The keyed environment digest (the child tools' and the compiler's
    /// listed variables).
    pub tools: ContentHash,
    /// The entry policy's digest.
    pub policy: ContentHash,
    /// The entry's code address over the record's instances, so that a hit
    /// reports it without decoding them.
    pub address: ContentHash,
    pub instance_limit: u64,
}
impl Head {
    fn hashes(&self) -> [ContentHash; HEAD_HASHES] {
        [
            self.sources,
            self.files,
            self.verified,
            self.executable,
            self.environment,
            self.toolchain,
            self.recipe,
            self.tools,
            self.policy,
            self.address,
        ]
    }
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&REACH_SCHEMA.to_le_bytes());
        for hash in self.hashes() {
            out.extend_from_slice(&hash.0);
        }
        out.extend_from_slice(&self.instance_limit.to_le_bytes());
    }
    fn read(bytes: &[u8]) -> Self {
        let hash = |i: usize| ContentHash(bytes[4 + i * 32..4 + (i + 1) * 32].try_into().unwrap());
        let limit = 4 + HEAD_HASHES * 32;
        Head {
            sources: hash(0),
            files: hash(1),
            verified: hash(2),
            executable: hash(3),
            environment: hash(4),
            toolchain: hash(5),
            recipe: hash(6),
            tools: hash(7),
            policy: hash(8),
            address: hash(9),
            instance_limit: u64::from_le_bytes(bytes[limit..limit + 8].try_into().unwrap()),
        }
    }
}

/// A stored reach record, read in place: the head is read when it loads,
/// each element when it is first asked for. The store checks every byte of
/// the frame on the read, and the load checks every offset and index the
/// fixed sections hold, so an element that fails to decode is a writer's
/// fault, never damage.
pub(crate) struct Reach {
    bytes: Vec<u8>,
    sections: [(usize, usize); SECTIONS],
    pub head: Head,
    rows: usize,
    paths: Vec<OnceLock<DefPath>>,
    instances: Vec<OnceLock<Entry>>,
    closures: Vec<OnceLock<Arc<Closure>>>,
    link: OnceLock<EntryLink>,
    nodes: usize,
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn encode_element(value: &(impl Serialize + ?Sized)) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    encoding::write_compact(&mut bytes, value).expect("a reach element encodes");
    bytes
}
fn decode_element<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
    encoding::read_compact(bytes).expect("a checked reach record's element decodes")
}

impl Reach {
    /// The record in `bytes` when its schema, head and fixed sections are
    /// consistent; anything else is a miss.
    pub(crate) fn read(bytes: Vec<u8>) -> Option<Self> {
        if bytes.len() < HEAD || u32_at(&bytes, 0) != REACH_SCHEMA {
            return None;
        }
        let mut sections = [(0, 0); SECTIONS];
        let mut end = HEAD;
        for (i, section) in sections.iter_mut().enumerate() {
            let at = HEAD_SECTIONS + i * 8;
            let (offset, len) = (u32_at(&bytes, at) as usize, u32_at(&bytes, at + 4) as usize);
            if offset != end || offset.checked_add(len)? > bytes.len() {
                return None;
            }
            end = offset + len;
            *section = (offset, len);
        }
        if end != bytes.len() {
            return None;
        }
        let mut reach = Reach {
            head: Head::read(&bytes),
            sections,
            rows: 0,
            paths: vec![],
            instances: vec![],
            closures: vec![],
            link: OnceLock::new(),
            nodes: 0,
            bytes,
        };
        let paths = reach.vector_len(Section::Paths)?;
        let rows = reach.vector_len(Section::Rows)?;
        let instances = reach.vector_len(Section::Instances)?;
        let closures = reach.vector_len(Section::Closures)?;
        let section = |s| reach.section(s);
        let constants = section(Section::Constants);
        let foreign = section(Section::Foreign);
        if foreign.len() < 4
            || (foreign.len() - 4) % 4 != 0
            || (foreign.len() - 4) / 4 != u32_at(foreign, 0) as usize
            || (0..u32_at(foreign, 0) as usize)
                .any(|i| u32_at(foreign, 4 + i * 4) as usize >= paths)
        {
            return None;
        }
        let places = section(Section::Places);
        let graph = section(Section::Graph);
        let fixed = constants.len() % 4 == 0
            && (0..constants.len() / 4).all(|i| (u32_at(constants, i * 4) as usize) < paths)
            && places.len() == paths * PLACE
            && rows <= paths;
        if !fixed || graph.len() < 8 {
            return None;
        }
        let nodes = u32_at(graph, 0) as usize;
        let edges_at = 4 + nodes.checked_mul(12)?;
        if graph.len() < edges_at + 4 {
            return None;
        }
        let edges = u32_at(graph, edges_at) as usize;
        if graph.len() != edges_at + 4 + edges.checked_mul(4)? {
            return None;
        }
        let mut last = None;
        for n in 0..nodes {
            let at = 4 + n * 12;
            let (row, flags, first) = (
                u32_at(graph, at),
                u32_at(graph, at + 4),
                u32_at(graph, at + 8),
            );
            let next = match n + 1 < nodes {
                true => u32_at(graph, at + 20),
                false => edges as u32,
            };
            let key = (row, flags & 1);
            if row as usize >= rows || flags > 3 || first > next || last >= Some(key) {
                return None;
            }
            last = Some(key);
        }
        if (0..edges).any(|e| (u32_at(graph, edges_at + 4 + e * 4) >> 1) as usize >= rows) {
            return None;
        }
        reach.rows = rows;
        reach.nodes = nodes;
        reach.paths = (0..paths).map(|_| OnceLock::new()).collect();
        reach.instances = (0..instances).map(|_| OnceLock::new()).collect();
        reach.closures = (0..closures).map(|_| OnceLock::new()).collect();
        Some(reach)
    }

    pub(crate) fn foreign(&self) -> impl Iterator<Item = &DefPath> + '_ {
        let rows = self.section(Section::Foreign);
        (0..u32_at(rows, 0) as usize).map(|i| self.path(u32_at(rows, 4 + i * 4) as usize))
    }
    /// Each section's offset and length in the payload.
    #[cfg(test)]
    pub(crate) fn sections(&self) -> [(usize, usize); SECTIONS] {
        self.sections
    }
    fn section(&self, section: Section) -> &[u8] {
        let (offset, len) = self.sections[section as usize];
        &self.bytes[offset..offset + len]
    }
    /// A vector section's length, when its offsets are in order and in range.
    fn vector_len(&self, section: Section) -> Option<usize> {
        let bytes = self.section(section);
        if bytes.len() < 4 {
            return None;
        }
        let n = u32_at(bytes, 0) as usize;
        let blob = 4 + n.checked_add(1)?.checked_mul(4)?;
        if bytes.len() < blob {
            return None;
        }
        let mut last = 0;
        for i in 0..=n {
            let at = u32_at(bytes, 4 + i * 4) as usize;
            if at < last || blob + at > bytes.len() {
                return None;
            }
            last = at;
        }
        (blob + last == bytes.len()).then_some(n)
    }
    /// Element `i` of a vector section.
    fn element(&self, section: Section, i: usize) -> &[u8] {
        let bytes = self.section(section);
        let n = u32_at(bytes, 0) as usize;
        let blob = 4 + (n + 1) * 4;
        let (start, end) = (u32_at(bytes, 4 + i * 4), u32_at(bytes, 8 + i * 4));
        &bytes[blob + start as usize..blob + end as usize]
    }
    /// Path `i`, decoded when first asked for.
    pub(crate) fn path(&self, i: usize) -> &DefPath {
        self.paths[i].get_or_init(|| decode_element(self.element(Section::Paths, i)))
    }
    /// The path of vector row `row`.
    pub(crate) fn row_path(&self, row: usize) -> &DefPath {
        self.path(row)
    }
    /// The vector's length.
    pub(crate) fn rows(&self) -> usize {
        self.rows
    }
    pub(crate) fn row(&self, path: &DefPath) -> Option<usize> {
        let (mut low, mut high) = (0, self.rows);
        while low < high {
            let mid = (low + high) / 2;
            match self.path(mid).cmp(path) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
    /// Whether each vector row is not the live row.
    pub(crate) fn changed(&self, product: &CheckProduct) -> Vec<bool> {
        let base = product.index.definitions.base_len();
        crate::meter::count(crate::meter::Count::RecordRowsRead, self.rows as u64);
        (0..self.rows)
            .map(|i| {
                let row: Row = decode_element(self.element(Section::Rows, i));
                !current(product, base, self.path(i), &row)
            })
            .collect()
    }
    pub(crate) fn constants(&self) -> Vec<DefPath> {
        let bytes = self.section(Section::Constants);
        (0..bytes.len() / 4)
            .map(|i| self.path(u32_at(bytes, i * 4) as usize).clone())
            .collect()
    }
    /// The source place of path `i` in the link that wrote the record.
    pub(crate) fn place(&self, i: usize) -> Option<(ModuleId, SourceId, u32)> {
        let at = i * PLACE;
        let bytes = self.section(Section::Places);
        (u32_at(bytes, at) == 1).then(|| {
            (
                ModuleId(u32_at(bytes, at + 4)),
                SourceId(u32_at(bytes, at + 8)),
                u32_at(bytes, at + 12),
            )
        })
    }
    /// The table's length.
    pub(crate) fn len(&self) -> usize {
        self.instances.len()
    }
    /// Table entry `i`, decoded when first asked for; its indices are in range.
    pub(crate) fn entry(&self, i: usize) -> &Entry {
        self.instances[i].get_or_init(|| {
            let e: StoredEntry<
                Vec<String>,
                Vec<ContentHash>,
                Vec<u32>,
                Vec<Identity>,
                CodeSignatures,
                Vec<Adapter>,
            > = decode_element(self.element(Section::Instances, i));
            let in_range = (e.definition as usize) < self.paths.len()
                && e.children
                    .iter()
                    .chain(&e.described)
                    .all(|&c| (c as usize) < self.instances.len())
                && e.items.iter().all(|&p| (p as usize) < self.paths.len())
                && e.closures
                    .iter()
                    .all(|&c| (c as usize) < self.closures.len());
            assert!(in_range, "a checked reach record's indices are in range");
            Entry {
                identity: Identity {
                    definition: self.path(e.definition as usize).clone(),
                    dictionaries: e.dictionaries,
                    closures: e.closure_hashes,
                },
                own: e.own,
                body: e.body,
                location: e.location,
                object: e.object,
                debug: e.debug,
                children: e.children,
                slots: e.slots,
                signatures: e.signatures,
                adapters: e.adapters,
                described: e.described,
                items: e.items,
                closures: e.closures,
            }
        })
    }
    /// The items of `entry`.
    pub(crate) fn items<'r>(&'r self, entry: &'r Entry) -> impl Iterator<Item = &'r DefPath> {
        entry.items.iter().map(|&i| self.path(i as usize))
    }
    pub(crate) fn instance(&self, identity: &Identity) -> Option<usize> {
        let (mut low, mut high) = (0, self.len());
        while low < high {
            let mid = (low + high) / 2;
            match self.entry(mid).identity.cmp(identity) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
    pub(crate) fn closures_of<'r>(
        &'r self,
        entry: &'r Entry,
    ) -> impl Iterator<Item = &'r Arc<Closure>> {
        entry.closures.iter().map(|&i| {
            self.closures[i as usize]
                .get_or_init(|| decode_element(self.element(Section::Closures, i as usize)))
        })
    }
    /// The entry link.
    pub(crate) fn link(&self) -> &EntryLink {
        self.link
            .get_or_init(|| decode_element(self.section(Section::Link)))
    }
    /// The definitions the table instantiates, glue excluded.
    pub(crate) fn definitions(&self) -> BTreeSet<&DefPath> {
        (0..self.len())
            .map(|i| &self.entry(i).identity.definition)
            .filter(|d| !hirsel_core_front::is_glue(d))
            .collect()
    }
    /// The header walk's node `n`.
    pub(crate) fn node_at(&self, n: usize) -> Node {
        let graph = self.section(Section::Graph);
        let nodes = self.nodes;
        let at = 4 + n * 12;
        let edges_at = 4 + nodes * 12 + 4;
        let first = u32_at(graph, at + 8) as usize;
        let next = match n + 1 < nodes {
            true => u32_at(graph, at + 20) as usize,
            false => u32_at(graph, edges_at - 4) as usize,
        };
        let flags = u32_at(graph, at + 4);
        Node {
            row: u32_at(graph, at),
            body: flags & 1 == 1,
            unavailable: flags & 2 == 2,
            edges: (first..next)
                .map(|e| {
                    let edge = u32_at(graph, edges_at + e * 4);
                    (edge >> 1, edge & 1 == 1)
                })
                .collect(),
        }
    }
    /// The header walk's nodes, sorted by row and kind.
    pub(crate) fn nodes(&self) -> impl Iterator<Item = Node> + '_ {
        (0..self.nodes).map(|n| self.node_at(n))
    }
    pub(crate) fn node(&self, path: &DefPath, body: bool) -> Option<Node> {
        let key = (self.row(path)? as u32, body);
        let graph = self.section(Section::Graph);
        let (mut low, mut high) = (0, self.nodes);
        while low < high {
            let mid = (low + high) / 2;
            let at = 4 + mid * 12;
            match (u32_at(graph, at), u32_at(graph, at + 4) & 1 == 1).cmp(&key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(self.node_at(mid)),
            }
        }
        None
    }
    pub(crate) fn counts(&self) -> (usize, usize) {
        let graph = self.section(Section::Graph);
        let bodies = (0..self.nodes)
            .filter(|n| u32_at(graph, 4 + n * 12 + 4) & 1 == 1)
            .count();
        (bodies, self.nodes - bodies)
    }
    /// A node's edges, as the walk names them.
    pub(crate) fn edges(&self, node: &Node) -> Vec<(DefPath, bool)> {
        node.edges
            .iter()
            .map(|&(row, body)| (self.row_path(row as usize).clone(), body))
            .collect()
    }
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
    /// Units this build lowered for its release std. Zero when it took the
    /// std from a store, as every build over a prebuilt std product should.
    pub std_lowered: u64,
    /// One when a resident compiler linked this build by patching its last
    /// executable in place (`incremental_link`), zero when lld linked it.
    pub link_patched: u64,
    /// One when a resident compiler made this build's source map by
    /// rebasing its last one (`source_delta`), zero otherwise.
    pub source_map_rebased: u64,
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
    pub(crate) fn is_current(&self, path: &DefPath, row: &Row) -> bool {
        row.own != Some(ContentHash::default()) && current(self.product, self.base, path, row)
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
            Draft::Patched { .. } => prior.map_or(0, Reach::rows),
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
    /// The new record's walk part: when the walk was accepted and complete,
    /// or for a hit whose head moved.
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

/// The selection: `hirsel.core-reach.v7` over the environment digest, the
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
        "hirsel.core-reach.v7",
        &(
            records.digest,
            product.environment,
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
    let len = bytes.len();
    Ok(Reach::read(bytes).map(|r| (r, len)))
}

/// A vector section being written: each element's end offset, then the
/// elements' bytes.
#[derive(Default)]
struct Vector {
    ends: Vec<u32>,
    blob: Vec<u8>,
}
/// A record whose payload does not fit u32 offsets: none is written.
struct Overflow;
impl Vector {
    fn push(&mut self, bytes: &[u8]) -> Result<(), Overflow> {
        self.blob.extend_from_slice(bytes);
        self.ends
            .push(u32::try_from(self.blob.len()).map_err(|_| Overflow)?);
        Ok(())
    }
    fn encode<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Overflow> {
        self.push(&encode_element(value))
    }
    fn finish(self) -> Result<Vec<u8>, Overflow> {
        let n = u32::try_from(self.ends.len()).map_err(|_| Overflow)?;
        let mut out = Vec::with_capacity(4 + 4 * (self.ends.len() + 1) + self.blob.len());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for end in self.ends {
            out.extend_from_slice(&end.to_le_bytes());
        }
        out.extend_from_slice(&self.blob);
        Ok(out)
    }
}

/// The record's bytes and their section lengths, written in canonical order
/// from the current request: rows in path order, then every other path in
/// first use, instances in identity order. Equal content gives equal bytes,
/// so a patched record is the fresh one; a patch copies the prior record's
/// unchanged row and path bytes and its graph, which equal the fresh ones.
#[allow(clippy::too_many_arguments)]
fn encode_record<'a>(
    prior: Option<&'a Reach>,
    head: &Head,
    constants: &'a [DefPath],
    draft: &'a Draft,
    instances: &'a [Fresh],
    closures: &[Arc<Closure>],
    entry: &EntryLink,
    places: &[crate::native_sources::SourceRow<'a>],
    foreign: &'a BTreeSet<Identity>,
) -> Result<Vec<u8>, Overflow> {
    let mut path_bytes = Vector::default();
    let mut row_bytes = Vector::default();
    let mut paths: Vec<&'a DefPath> = vec![];
    let graph: Vec<u8> = match draft {
        Draft::Walked { rows, graph } => {
            for (path, row) in rows {
                path_bytes.encode(path)?;
                row_bytes.encode(row)?;
                paths.push(path);
            }
            let mut out = vec![];
            let edges: usize = graph.iter().map(|n| n.edges.len()).sum();
            let n = u32::try_from(graph.len()).map_err(|_| Overflow)?;
            out.extend_from_slice(&n.to_le_bytes());
            let mut first = 0u32;
            for node in graph {
                let flags = node.body as u32 | (node.unavailable as u32) << 1;
                for word in [node.row, flags, first] {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                first += node.edges.len() as u32;
            }
            let m = u32::try_from(edges).map_err(|_| Overflow)?;
            out.extend_from_slice(&m.to_le_bytes());
            for node in graph {
                for &(row, body) in &node.edges {
                    out.extend_from_slice(&(row << 1 | body as u32).to_le_bytes());
                }
            }
            out
        }
        Draft::Patched { rows: patch } => {
            let prior = prior.expect("a patched vector has its record");
            let mut patch = patch.iter().peekable();
            for i in 0..prior.rows() {
                path_bytes.push(prior.element(Section::Paths, i))?;
                paths.push(prior.path(i));
                match patch.next_if(|(at, _)| *at == i) {
                    Some((_, row)) => row_bytes.encode(row)?,
                    None => row_bytes.push(prior.element(Section::Rows, i))?,
                }
            }
            prior.section(Section::Graph).to_vec()
        }
    };
    // Every other path is numbered as it is first named.
    let mut numbers: rustc_hash::FxHashMap<&'a DefPath, u32> = Default::default();
    numbers.reserve(paths.len() + instances.len());
    numbers.extend(paths.iter().enumerate().map(|(i, p)| (*p, i as u32)));
    let mut index = |path: &'a DefPath| -> Result<u32, Overflow> {
        let next = u32::try_from(paths.len()).map_err(|_| Overflow)?;
        Ok(*numbers.entry(path).or_insert_with(|| {
            paths.push(path);
            next
        }))
    };
    let mut constant_bytes = vec![];
    for path in constants {
        constant_bytes.extend_from_slice(&index(path)?.to_le_bytes());
    }
    let mut instance_bytes = Vector::default();
    for e in instances {
        let definition = index(&e.identity.definition)?;
        let items = e
            .items
            .iter()
            .map(&mut index)
            .collect::<Result<Vec<_>, _>>()?;
        instance_bytes.encode(&StoredEntry {
            definition,
            dictionaries: &e.identity.dictionaries,
            closure_hashes: &e.identity.closures,
            own: e.own,
            body: e.body,
            location: e.location,
            object: e.object,
            debug: e.debug,
            children: &e.children,
            slots: &e.slots,
            signatures: &e.signatures,
            adapters: &e.adapters,
            described: &e.described,
            items: &items,
            closures: &e.closures,
        })?;
    }
    let mut placed = vec![];
    for row in places {
        placed.push((index(row.path)?, row));
    }
    let mut foreign_bytes = u32::try_from(foreign.len())
        .map_err(|_| Overflow)?
        .to_le_bytes()
        .to_vec();
    for identity in foreign {
        foreign_bytes.extend_from_slice(&index(&identity.definition)?.to_le_bytes());
    }
    for path in &paths[path_bytes.ends.len()..] {
        path_bytes.encode(*path)?;
    }
    let mut place_bytes = vec![0u8; paths.len() * PLACE];
    for (i, row) in placed {
        write_place(&mut place_bytes, i as usize, row);
    }
    let mut closure_bytes = Vector::default();
    for closure in closures {
        closure_bytes.encode(closure)?;
    }
    let link = encode_element(entry);
    let sections: [Vec<u8>; SECTIONS] = [
        path_bytes.finish()?,
        row_bytes.finish()?,
        constant_bytes,
        graph,
        instance_bytes.finish()?,
        closure_bytes.finish()?,
        link,
        place_bytes,
        foreign_bytes,
    ];
    let total = HEAD + sections.iter().map(Vec::len).sum::<usize>();
    u32::try_from(total).map_err(|_| Overflow)?;
    let mut out = Vec::with_capacity(total);
    head.encode(&mut out);
    let mut offset = HEAD;
    for section in &sections {
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        out.extend_from_slice(&(section.len() as u32).to_le_bytes());
        offset += section.len();
    }
    for section in &sections {
        out.extend_from_slice(section);
    }
    Ok(out)
}

/// What a build publishes under its selection once its executable is stored.
pub(crate) enum Content<'a> {
    /// A walk's record: `draft` with its table, entry link and source places.
    Drafted {
        constants: &'a [DefPath],
        draft: &'a Draft,
        instances: &'a [Fresh],
        closures: &'a [Arc<Closure>],
        entry: &'a EntryLink,
        places: &'a [crate::native_sources::SourceRow<'a>],
        foreign: &'a BTreeSet<Identity>,
    },
    /// A hit: the prior record's sections, under a new head if it changed.
    /// When the request files changed, `places` are the hit's source places,
    /// which replace the prior link's.
    Held {
        places: Option<&'a [crate::native_sources::SourceRow<'a>]>,
    },
}

/// Write the record under `key` with `head`, as one keyed frame: the
/// selection and its record commit in one rename. A hit whose head is the
/// prior one writes nothing. Its size; a record too large for its offsets is
/// not written.
pub(crate) fn publish(
    store: &Store,
    key: ContentHash,
    prior: Option<&Reach>,
    head: &Head,
    content: Content<'_>,
) -> io::Result<usize> {
    let bytes = match content {
        Content::Drafted {
            constants,
            draft,
            instances,
            closures,
            entry,
            places,
            foreign,
        } => match encode_record(
            prior, head, constants, draft, instances, closures, entry, places, foreign,
        ) {
            Ok(bytes) => bytes,
            Err(Overflow) => return Ok(0),
        },
        Content::Held { places } => {
            let prior = prior.expect("a hit holds its record");
            if prior.head == *head {
                return Ok(0);
            }
            let mut bytes = Vec::with_capacity(prior.bytes.len());
            head.encode(&mut bytes);
            bytes.extend_from_slice(&prior.bytes[HEAD_SECTIONS..]);
            if let Some(places) = places {
                // A hit's places name its instances' items, which the record
                // already numbers; one it does not is not written.
                let Some(section) = place_section(prior, places) else {
                    return Ok(0);
                };
                let (offset, len) = prior.sections[Section::Places as usize];
                if section.len() != len {
                    return Ok(0);
                }
                bytes[offset..offset + len].copy_from_slice(&section);
            }
            bytes
        }
    };
    store.put_keyed(key, &bytes)?;
    Ok(bytes.len())
}

/// The Places section for `places` over the prior record's path numbering,
/// as [`encode_record`] lays it out; `None` when a place's path is unnumbered.
fn place_section(
    prior: &Reach,
    places: &[crate::native_sources::SourceRow<'_>],
) -> Option<Vec<u8>> {
    let n = prior.paths.len();
    let numbers: rustc_hash::FxHashMap<&DefPath, usize> =
        (0..n).map(|i| (prior.path(i), i)).collect();
    let mut out = vec![0u8; n * PLACE];
    for row in places {
        write_place(&mut out, *numbers.get(row.path)?, row);
    }
    Some(out)
}
/// Row `i` of a Places section: placed, module, file, file offset.
fn write_place(out: &mut [u8], i: usize, row: &crate::native_sources::SourceRow<'_>) {
    let at = i * PLACE;
    for (k, word) in [1, row.module.0, row.source.0, row.file_offset]
        .into_iter()
        .enumerate()
    {
        out[at + k * 4..at + k * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
}

/// What a source map rests on besides the image (which the record's key
/// names) and its items: each request file's occurrence, module, name and
/// text. Equal files give each item the same file and offset.
pub(crate) fn files_digest(product: &CheckProduct) -> io::Result<ContentHash> {
    let files: Vec<_> = product
        .files
        .iter()
        .map(|f| (f.file, f.module, product.file_name(f), f.source))
        .collect();
    crate::native::key("hirsel.core-reach-files.v2", &files)
}

/// The digest of the source map `source_map(rows)` builds: each item's path,
/// file and offset, and each file's name and text. A request file's text is
/// its bytes' hash; an image file's is the image's.
pub(crate) fn sources_digest(
    product: &CheckProduct,
    rows: &[crate::native_sources::SourceRow<'_>],
) -> io::Result<ContentHash> {
    let mut files = BTreeMap::new();
    for row in rows {
        if files.contains_key(&row.source) {
            continue;
        }
        let file = match product.files.iter().find(|file| file.file == row.source) {
            Some(file) => (product.file_name(file), true, file.source),
            None => {
                let image = product.image.as_ref().unwrap();
                let name = image.source_name(row.source)?;
                let name = name.unwrap_or_else(|| product.module_name(row.module));
                (name, false, image.identity)
            }
        };
        files.insert(row.source, file);
    }
    let rows: Vec<_> = rows
        .iter()
        .map(|r| (r.path, r.source, r.file_offset))
        .collect();
    crate::native::key(
        "hirsel.core-source-map.v2",
        &(rows, files.into_iter().collect::<Vec<_>>()),
    )
}
