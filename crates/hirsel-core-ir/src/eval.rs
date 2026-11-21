//! A test-only evaluator, written from the op documentation (SEM-1, SEM-2, LIR's op
//! docs; for the FORM v1 draft FAIL-8..14, MEM-19..29 and CONC-3..9) so the
//! samples' hand-derived run lines can be checked. Memory is blocks of bytes laid
//! out by the layout recipes. Limits keep exact `used` totals and hand over by
//! forwarding. A spawned child runs to its end at the spawn, so the event order is
//! the program order and nothing waits. A delivered `Causes` owns the child's used
//! cells until it is dropped. Compact data is region bytes read and written through
//! `byte` steps, as a producer lowers it. Each static is a read-only block made
//! at the start of the run: a store into one, or a `free` of one, stops the run.
//! Sequence storage is cleaned by its carrier; list and map operations are the
//! producer's code. Provider rows run against a modelled provider ([`World`]).

mod simd;

use crate::*;
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::{
    Body as Shaped, Cleanup, FieldTy, Handle, Layout, Recipe, Repr, TypeRef, charge, cleanup_class,
    compute, field_size, map_entry,
};
use hirsel_core_net::{Name, SecretGrant, SecretPolicy, SecretUse};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Returned(Vec<u64>),
    /// The run failed with this cause chain, primary cause first.
    Failed(Vec<String>),
    /// FAIL-15: the run ends every task without cleanup or a cause chain.
    Stopped(&'static str),
}

pub struct Report {
    pub outcome: Outcome,
    /// The root limit's `used` at the observation boundary (MEM-19).
    /// A host call observes the live task. An entry run retires the root task.
    pub used: u64,
    /// Root peak reservation (MEM-19), including bound/task/grant reservations.
    pub peak_reserved: u64,
    /// The first u64 field of each resource, in the order its cleanup started.
    pub trace: Vec<u64>,
    pub admissions: usize,
    /// Bytes the program wrote to its `Stdout` and `Stderr`.
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The site of each cause of a failed run, parallel to its chain (SITES.md):
    /// the anchor of the raising operation, or of the call that reached it when
    /// that operation is core's or unanchored; `None` when no such anchor exists.
    pub sites: Vec<Option<Anchor>>,
    /// The run's CONC-10 event list, in the native recorder's kinds and ids. Empty
    /// unless the run asked for events: a long run makes millions of them.
    pub events: Vec<Event>,
}

/// What the run's oracle checks covered (crate docs, "Loans").
#[derive(Debug, Default, PartialEq)]
pub struct Checked {
    /// Executed instructions and terminators, shared by every task of the run.
    pub instructions: u64,
    /// Lent places: each `view` and `take` place frozen for its call, each `mut`
    /// place and destination checked against the call's other places.
    pub loans: u64,
    /// Accesses through an address with a storage role, each checked against the
    /// bytes the activation's other roles touched (crate docs, "Roles").
    pub roles: u64,
    /// Executed copies of the size selected by [run_counting_copies].
    pub copies: u64,
    /// Spawned tasks' native stack high-water mark at evaluator call entry.
    /// Helper frames below that boundary use the reserved headroom.
    pub task_stack_peak: usize,
    /// Largest active evaluator activation chain in a spawned task.
    pub task_call_depth: usize,
}

/// The modelled provider (CAP-5), deterministic so runs can be derived by hand:
/// - `Args` is the run's argument table: each argument a run-owned `String`
///   carrier of capacity 0; the handle's value is not read.
/// - `stdout` and `stderr` writes are captured whole: `written` is the length.
/// - A clock read returns the number of clock reads so far, itself included,
///   both clocks alike: the host clock has started before the run (a law that
///   adds to a read relies on it), and a sleep jumps it to the deadline.
/// - Directory keys pair a pinned directory identity with a single basename.
///   Handles keep their file and independent cursor across namespace changes.
///   Listings snapshot direct entries in the model's deterministic order.
///   File mtime is the model clock at creation or last write. Directory
///   metadata is kind Directory, size and mtime zero.
///   No model operation introduces symbolic links.
///
/// - Nothing is ever cancelled.
#[derive(Default)]
struct World {
    launch_target: (u64, u64),
    launch: Option<hirsel_core_ids::launch::Metadata>,
    args: Vec<u64>,
    stdin: std::collections::VecDeque<u8>,
    env_patterns: Vec<hirsel_core_net::EnvPatterns>,
    env_entries: Vec<(String, u64, Option<u64>)>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    clock: u64,
    nodes: BTreeMap<DirKey, Node>,
    directories: u64,
    listings: Vec<Option<VecDeque<Vec<u8>>>>,
    files: Vec<Option<OpenFile>>,
    temporaries: u64,
    random: u8,
    network: Network,
    grant_charges: Vec<(Limit, u64)>,
    secrets: Vec<EvalSecret>,
    tls_writes: BTreeMap<u64, (Name, bool)>,
    /// `provide` cells: cell `i`'s capability and its hand or trigger are both
    /// `CELL_BASE + i` (native hirsel-core-rt `provide`).
    cells: Vec<VirtualCell>,
}
const CELL_BASE: u64 = 0x80_0000;
enum VirtualCell {
    Clock {
        mono: u64,
        wall: u64,
        /// Tasks waiting on a deadline of this clock, in registration order.
        timers: Vec<(u64, usize)>,
        /// Tasks in `clock.idle`, and whether quiescence has woken each.
        idle: Vec<(usize, bool)>,
    },
    Random(u64),
    Process {
        stopped: bool,
        waiters: Vec<usize>,
    },
}
/// A deadline on the host model's clock or on a virtual clock's own timers.
#[derive(Clone, Copy)]
enum Due {
    Host(u64),
    Cell(usize, u64),
}
#[derive(Clone)]
struct EvalSecret {
    policy: SecretPolicy,
    bytes: Arc<Zeroizing<Vec<u8>>>,
}

type DirKey = (u64, Vec<u8>);

enum Node {
    File(Arc<Mutex<ModelFile>>),
    Dir(u64),
}
struct OpenFile {
    file: Arc<Mutex<ModelFile>>,
    at: u64,
    locked: bool,
    mode: u8,
}
const FILE_PAGE: u64 = 4096;

#[derive(Default)]
struct ModelFile {
    len: u64,
    modified_ns: u64,
    pages: BTreeMap<u64, Box<[u8; FILE_PAGE as usize]>>,
}

impl ModelFile {
    fn read(&self, at: u64, count: usize) -> Vec<u8> {
        let mut bytes = vec![0; count];
        Self::visit(at, count, |page, from, to, pos| {
            if let Some(page) = self.pages.get(&page) {
                bytes[pos..pos + to - from].copy_from_slice(&page[from..to]);
            }
        });
        bytes
    }

    fn write(&mut self, at: u64, bytes: &[u8]) {
        Self::visit(at, bytes.len(), |page, from, to, pos| {
            let page = self
                .pages
                .entry(page)
                .or_insert_with(|| Box::new([0; FILE_PAGE as usize]));
            page[from..to].copy_from_slice(&bytes[pos..pos + to - from]);
        });
        self.len = self.len.max(at + bytes.len() as u64);
    }

    fn visit(at: u64, count: usize, mut f: impl FnMut(u64, usize, usize, usize)) {
        let mut pos = 0;
        while pos < count {
            let offset = at + pos as u64;
            let from = (offset % FILE_PAGE) as usize;
            let n = (count - pos).min(FILE_PAGE as usize - from);
            f(offset / FILE_PAGE, from, from + n, pos);
            pos += n;
        }
    }
}
/// The model host's largest block: an admitted allocation above it stops the run.
const HOST_BYTES: u64 = 1 << 32;
const FILE_BASE: u64 = 0x50_0000;
const DIR_BASE: u64 = 0x60_0000;
const LISTING_HANDLE: u64 = 1 << 63;

const NOT_FOUND: u64 = 1;
const DENIED: u64 = 2;
const EXISTS: u64 = 3;
const CLOSED: u64 = 4;
const OTHER: u64 = 5;

impl World {
    fn dir_key(&self, mut dir: u64, path: &[u8]) -> Result<DirKey, (u64, u64)> {
        let parts = path.split(|&b| b == b'/');
        for part in parts.clone() {
            if part.is_empty() || matches!(part, b"." | b"..") || part.contains(&0) {
                return Err((DENIED, 0));
            }
            if part.len() > 255 {
                return Err((OTHER, 36)); // ENAMETOOLONG
            }
        }
        let mut parts = parts.peekable();
        while let Some(part) = parts.next() {
            let key = (dir, part.to_vec());
            if parts.peek().is_none() {
                return Ok(key);
            }
            dir = match self.nodes.get(&key) {
                Some(Node::Dir(dir)) => *dir,
                Some(Node::File(_)) => return Err((OTHER, 20)), // ENOTDIR
                None => return Err((NOT_FOUND, 0)),
            };
        }
        unreachable!("split always supplies a final component")
    }

    fn new_dir(&mut self, key: DirKey) -> u64 {
        let dir = DIR_BASE + self.directories;
        self.directories += 1;
        self.nodes.insert(key, Node::Dir(dir));
        dir
    }

    fn children(&self, dir: u64) -> impl Iterator<Item = (&DirKey, &Node)> {
        self.nodes
            .range((dir, Vec::new())..)
            .take_while(move |((id, _), _)| *id == dir)
    }

    fn remove_children(&mut self, dir: u64) {
        let mut directories = vec![dir];
        while let Some(dir) = directories.pop() {
            let names: Vec<_> = self.children(dir).map(|(key, _)| key.clone()).collect();
            for key in names {
                if let Some(Node::Dir(child)) = self.nodes.remove(&key) {
                    directories.push(child);
                }
            }
        }
    }
}

const FN_BASE: u64 = 0x1000;
const REGION_BASE: u64 = 0x10_0000;
const TASK_BASE: u64 = 0x20_0000;
/// Sender of channel `i` is `CHAN_BASE + 2i`, its receiver `CHAN_BASE + 2i + 1`.
const CHAN_BASE: u64 = 0x30_0000;
/// Handle of `Latest` root `i` (CONC-13).
const LATEST_BASE: u64 = 0x40_0000;
/// Word 0 of an `own fn` environment whose captures are row `t`: the model's
/// name for that row's fixed descriptor.
const ENV_BASE: u64 = 0x70_0000;

/// One cause of a chain: its `CauseKind` case and, for `Fail`, the message's
/// `String` carrier `{data, length, capacity}`, which the cause owns (FAIL-10).
#[derive(Clone, Copy, Debug)]
struct Cause {
    kind: &'static str,
    message: [u64; 3],
    site: Option<Anchor>,
}
/// `CauseKind` in declaration order, its case ordinals.
const CAUSE_KINDS: [&str; 8] = [
    "Overflow",
    "Bounds",
    "Division",
    "Unwrap",
    "Fail",
    "Cancelled",
    "Capacity",
    "OutOfTime",
];
/// A cause cell: core's `Cause` (32 bytes: the `Fail` carrier at 0, `Site` at 24),
/// then the next cell's pointer. `CauseKind` is the carrier's capacity niche (FORM
/// #75): another kind `k` is the capacity `CAUSE_KIND_START + k`.
const CELL_NEXT: u64 = 32;
const CAUSE_CAPACITY: u64 = 16;
const CAUSE_SITE: u64 = 24;

const CAUSE_KIND_START: u64 = 1 << 63;

/// Why an instruction stopped: a new cause, or a callee's failure already chained.
#[derive(Debug)]
enum Stop {
    Cause(&'static str),
    Chained,
}
type X<T> = Result<T, Stop>;

struct Blk {
    bytes: Vec<u8>,
    live: bool,
    /// The limit the block is charged to; None for frame slots and engine records.
    limit: Option<Limit>,
    charge: u64,
    lent_size: Option<u64>,
    res: Option<Res>,
    /// A counted node's kind, fixed by its `alloc`; its operations must name it.
    counted: Option<Counted>,
    /// The cleanup class its `alloc` named (crate docs, "Cleanup classes"): `None`
    /// for a block no `alloc` row made; `Some(None)` for region storage.
    class: Option<Option<Cleanup>>,
    /// Static data (`static dN`): read, never written or freed.
    fixed: bool,
    /// The liveness shadow of `mark` (crate docs, "Slot kernels"): each marked
    /// slot's offset, whether it holds a value, and its size.
    marks: BTreeMap<usize, (bool, usize)>,
}
// A pointer's upper word is a monotonically assigned block identity, not a
// storage slot. Only live blocks occupy records. Absence is the tombstone.
// Completed empty index chunks are discarded; the top directory is bounded by
// the 32-bit identity space. One live block can pin one 4096-entry u32 chunk.
const BLOCK_CHUNK: usize = 4096;
struct BlockChunk {
    slots: Box<[u32; BLOCK_CHUNK]>,
    live: usize,
}
#[derive(Default)]
struct Blocks {
    next: usize,
    chunks: Vec<Option<Box<BlockChunk>>>,
    entries: Vec<(usize, Blk)>,
}
impl Blocks {
    fn insert(&mut self, block: Blk) -> usize {
        assert!(
            self.next < u32::MAX as usize,
            "evaluator block identity space exhausted (a run stop, not Capacity)"
        );
        let id = self.next;
        self.next += 1;
        let c = id / BLOCK_CHUNK;
        if c == self.chunks.len() {
            self.chunks.push(Some(Box::new(BlockChunk {
                slots: Box::new([0; BLOCK_CHUNK]),
                live: 0,
            })));
        }
        let chunk = self.chunks[c].as_mut().unwrap();
        chunk.slots[id % BLOCK_CHUNK] = (self.entries.len() + 1) as u32;
        chunk.live += 1;
        self.entries.push((id, block));
        id
    }
    fn slot(&self, id: usize) -> Option<usize> {
        let chunk = self.chunks.get(id / BLOCK_CHUNK)?.as_ref()?;
        let slot = chunk.slots[id % BLOCK_CHUNK];
        (slot != 0).then_some(slot.wrapping_sub(1) as usize)
    }
    fn remove(&mut self, id: usize) -> Blk {
        let slot = self.slot(id).expect("free of a non-block");
        let chunk = self.chunks[id / BLOCK_CHUNK].as_mut().unwrap();
        chunk.slots[id % BLOCK_CHUNK] = 0;
        chunk.live -= 1;
        if chunk.live == 0 && id / BLOCK_CHUNK < self.next / BLOCK_CHUNK {
            self.chunks[id / BLOCK_CHUNK] = None;
        }
        let (_, block) = self.entries.swap_remove(slot);
        if let Some(&(moved, _)) = self.entries.get(slot) {
            self.chunks[moved / BLOCK_CHUNK].as_mut().unwrap().slots[moved % BLOCK_CHUNK] =
                (slot + 1) as u32;
        }
        // Bound record storage by current liveness, not the historical peak.
        // Hysteresis keeps repeated allocation/free at the boundary amortized.
        if self.entries.capacity() > 256 && self.entries.len() < self.entries.capacity() / 4 {
            self.entries.shrink_to((2 * self.entries.len()).max(256));
        }
        block
    }
}
// Absent blocks retain only their liveness fact. No diagnostic depends on the
// retired bytes, size, limit or origin. Mutable indexing always follows a live
// check, and cannot create an absent entry.
static DEAD_BLOCK: Blk = Blk {
    bytes: Vec::new(),
    live: false,
    limit: None,
    charge: 0,
    lent_size: None,
    res: None,
    counted: None,
    class: None,
    fixed: false,
    marks: BTreeMap::new(),
};
impl std::ops::Index<usize> for Blocks {
    type Output = Blk;
    fn index(&self, id: usize) -> &Blk {
        self.slot(id)
            .map_or(&DEAD_BLOCK, |slot| &self.entries[slot].1)
    }
}
impl std::ops::IndexMut<usize> for Blocks {
    fn index_mut(&mut self, id: usize) -> &mut Blk {
        let slot = self.slot(id).expect("use of a freed block");
        &mut self.entries[slot].1
    }
}

#[cfg(test)]
mod block_table_tests {
    use super::*;

    fn block() -> Blk {
        Blk {
            bytes: vec![42; 8],
            live: true,
            limit: None,
            charge: 0,
            lent_size: None,
            res: None,
            counted: None,
            class: None,
            fixed: false,
            marks: BTreeMap::new(),
        }
    }

    #[test]
    fn block_storage_follows_liveness_and_never_reuses_an_identity() {
        let mut blocks = Blocks::default();
        let pinned = blocks.insert(block());
        let mut previous = pinned;
        for _ in 0..10 * BLOCK_CHUNK {
            let id = blocks.insert(block());
            assert!(id > previous);
            previous = id;
            assert_eq!(blocks[id].bytes, [42; 8]);
            blocks.remove(id);
            assert!(!blocks[id].live);
        }
        assert_eq!(blocks.entries.len(), 1);
        assert!(blocks.entries.capacity() <= 256);
        assert_eq!(blocks.chunks.iter().filter(|c| c.is_some()).count(), 2);
        assert_eq!(blocks[pinned].bytes, [42; 8]);
        blocks.remove(pinned);
        assert!(blocks.chunks[0].is_none());
        assert!(!blocks[pinned].live);
    }

    #[test]
    fn removing_a_record_preserves_the_identity_of_the_moved_record() {
        let mut blocks = Blocks::default();
        let ids: Vec<_> = (0..2 * BLOCK_CHUNK)
            .map(|_| blocks.insert(block()))
            .collect();
        for &id in &ids[..BLOCK_CHUNK] {
            blocks.remove(id);
        }
        assert!(blocks.chunks[0].is_none());
        for &id in &ids[BLOCK_CHUNK..] {
            assert_eq!(blocks[id].bytes, [42; 8]);
        }
        for &id in &ids[BLOCK_CHUNK..] {
            blocks.remove(id);
        }
        assert!(blocks.chunks.iter().all(Option::is_none));
        assert!(blocks.entries.capacity() <= 256);
    }

    #[test]
    fn block_identity_exhaustion_stops_instead_of_wrapping() {
        let mut blocks = Blocks {
            next: u32::MAX as usize,
            ..Blocks::default()
        };
        let fault =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| blocks.insert(block())))
                .unwrap_err();
        assert_eq!(
            fault.downcast_ref::<&str>().copied(),
            Some("evaluator block identity space exhausted (a run stop, not Capacity)")
        );
        assert_eq!(blocks.next, u32::MAX as usize);
        assert!(blocks.entries.is_empty());
        let last = ptr(u32::MAX as usize - 1, u32::MAX as u64);
        assert_eq!(split(last), (u32::MAX as usize - 1, u32::MAX as usize));
    }
}

struct Res {
    grant: u64,
    creator: usize,
    settled: bool,
    cell_used: bool,
}
/// A pin held by each scope, funded object and queued refund. Only the
/// evaluator baton changes the balance or links, but pins can be dropped by
/// different task threads when the scheduler tears down a run.
#[derive(Clone, Debug)]
struct Limit(Arc<LimitRecord>);

#[derive(Debug)]
struct LimitRecord {
    cap: u64,
    /// Scheduler index of the task whose admissions this limit serves.
    owner: usize,
    state: Mutex<LimitState>,
}

#[derive(Debug)]
struct LimitState {
    used: u64,
    parent: Option<Limit>,
    ended: bool,
}

impl Limit {
    fn new(cap: u64, parent: Option<Self>, owner: usize) -> Self {
        Self(Arc::new(LimitRecord {
            cap,
            owner,
            state: Mutex::new(LimitState {
                used: 0,
                parent,
                ended: false,
            }),
        }))
    }

    fn find(&self) -> Self {
        let mut limit = self.clone();
        loop {
            let forward = {
                let state = limit.0.state.lock().unwrap();
                if !state.ended {
                    break;
                }
                state.parent.clone().unwrap()
            };
            limit = forward;
        }
        limit
    }

    fn used(&self) -> u64 {
        self.0.state.lock().unwrap().used
    }

    fn subtract(&self, n: u64) {
        self.0.state.lock().unwrap().used -= n;
    }
}

impl Drop for LimitRecord {
    fn drop(&mut self) {
        // Release a uniquely owned forwarding chain without recursive drops.
        let mut parent = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .parent
            .take();
        while let Some(Limit(link)) = parent {
            let Some(mut record) = Arc::into_inner(link) else {
                break;
            };
            parent = record
                .state
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .parent
                .take();
        }
    }
}
/// A cause cell an obligation holds unused (FAIL-10): the limit and amount it
/// refunds to, and whether it is a resource's own cell, which its block's free
/// refunds.
struct Cell {
    limit: Limit,
    amount: u64,
    resource: bool,
}
/// A running `isolate`'s reservation (crate docs, "Isolation"): its cell's limit,
/// and the active chain it set aside, with that chain's cells.
struct Reservation {
    limit: Limit,
    chain: Vec<Cause>,
    cells: Vec<(Limit, u64)>,
}
struct StepBound {
    id: u64,
    left: u64,
}
struct Task {
    step_bounds: Vec<StepBound>,
    slice_steps: u64,
    limits: Vec<Limit>,
    /// The active chain: the causes of the failure in progress, primary first.
    chain: Vec<Cause>,
    /// The active chain's cells, with the limit and amount each refunds to.
    cells: Vec<(Limit, u64)>,
    /// The obligations that can take a cause: the task's primary first, then one
    /// per running finalizer, the innermost last. A cause goes into the last
    /// one's cell; `None` once it is used.
    obligations: Vec<Option<Cell>>,
    /// The running `isolate`s' reservations, the innermost last.
    reserved: Vec<Reservation>,
    /// The anchor each running activation of the task is at, innermost last, and
    /// whether its function inherits its caller's site (SITES.md 6(b)).
    at: Vec<(Option<Anchor>, bool)>,
    /// Its scheduler record: 0 for the root, `i + 1` for child `i`.
    k: usize,
    /// Cleanups running with cancellation shielded (CONC-8, CONC-9).
    shield: u32,
}
struct Child {
    status: u8,
    result: u64,
    r: TypeRef,
    charge: u64,
    n: u64,
    limit: Option<Limit>,
    joined: bool,
    /// The child's used cells: those of the chain it failed with.
    cells: Vec<(Limit, u64)>,
    /// Its primary cell when unused at its end, refunded when it is joined.
    primary: Option<Cell>,
    /// Its causes, primary first, while no `Causes` holds them.
    chain: Vec<Cause>,
}
impl Machine<'_> {
    fn next_id(&mut self) -> u64 {
        let id = self.sched.next;
        self.sched.next += 1;
        id
    }
}
struct Chan {
    id: u64,
    item: TypeRef,
    cap: u64,
    queue: VecDeque<Vec<u8>>,
    senders: u64,
    receiver: bool,
    charge: u64,
    limit: Option<Limit>,
}
/// A `Latest[T]` root: the current `Arc[T]` node, admitted at `LATEST_ROOT`.
struct Latest {
    id: u64,
    /// `Arc[T]`, whose drop releases the root's share at the last handle.
    item: TypeRef,
    root: u64,
    handles: u64,
    limit: Option<Limit>,
}

#[derive(Clone, Copy)]
enum At {
    Decl(TypeRef),
    Ptr(TypeRef),
    /// Holds an Rc/Arc node pointer; the payload is at node + 8.
    Counted(TypeRef),
    /// An inline array: its element type and length are `Machine::arrays[i]`.
    Array(usize),
    /// A `seq` field, by its element's logical size (MEM-24's charge unit).
    Seq(u64),
    /// A word of a `seq` field whose element has this logical size (FORM #113).
    Word(SeqWord, u64),
    Other,
}

struct Machine<'m> {
    m: &'m Module,
    recipes: Vec<Recipe>,
    mem: Blocks,
    root: Option<Limit>,
    next_step_bound: u64,
    peak_reserved: u64,
    regions: Vec<Vec<usize>>,
    children: Vec<Child>,
    chans: Vec<Chan>,
    latests: Vec<Latest>,
    /// Each live delivered `Causes`, by its head cell: the charged cells it owns.
    causes: HashMap<u64, Vec<(Limit, u64)>>,
    /// Every inline array field type of the module: element and length.
    arrays: Vec<(FieldTy, u64)>,
    trace: Vec<u64>,
    /// The last destination identity handed out (`rt.dest_id`).
    dests: u64,
    admissions: usize,
    instructions: u64,
    injection: Option<(usize, bool)>,
    world: World,
    /// The `view` places of the calls running now, as (block, offset, size).
    frozen: Vec<(usize, usize, usize)>,
    loans: u64,
    /// The module's derived parameter facts, and each function's roles, filled on
    /// its first call.
    facts: Arc<roles::Facts>,
    roles: Vec<Option<Arc<[Option<roles::Role>]>>>,
    /// Per running activation: its roles, and by block the byte spans each role
    /// touched (`lo -> (hi, role)`, disjoint, a role's adjacent spans merged).
    footprints: Vec<Footprint>,
    role_checks: u64,
    copy_size: Option<u64>,
    copies: u64,
    task_stack_peak: usize,
    task_call_depth: usize,
    /// Whether each function inherits its caller's site (`source::inherits_site`).
    inherits: Vec<bool>,
    /// By item, its first site word (`SourceMap::item_sites`), by default the
    /// module's own (`source::module_sites`).
    sites: Arc<[u64]>,
    /// [`Nans::Scrambled`]: the count of NaNs stored so far, which picks the next one's
    /// pattern; None in the canonical placement.
    scrambled: Option<u64>,
    sched: Sched,
    /// Each task's thread link, held here only while that task runs.
    links: Vec<Option<Link<'m>>>,
    /// By function, the address of its static descriptor (`Inst::DescriptorRef`),
    /// 0 for a function no body describes. Functions of one code identity share one.
    descriptors: Vec<u64>,
}

struct Footprint {
    roles: Arc<[Option<roles::Role>]>,
    spans: HashMap<usize, BTreeMap<usize, (usize, roles::Role)>>,
}

// ---- tasks (CONC-3..10)
//
// Each task runs on its own thread, and exactly one thread runs at a time: the
// `Machine` is a baton that moves by value to the task the scheduler picks and
// back when that task waits or ends. The scheduler is the native default
// scheduler's rule (hirsel-core-rt tasks.rs): a FIFO ready queue, a spawned child
// readied at the tail while its parent continues, every wake readying each
// blocked task newest first, and a poll queue that runs only when nothing else
// is ready. So the event order is the native order by construction.

use event::*;
/// The CONC-10 event kinds: the one table native and the evaluator share.
pub use hirsel_core_layout::event;
/// One CONC-10 event: its kind, the task that performed it, its object and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u8,
    pub task: u64,
    pub object: u64,
    pub value: u64,
    /// Caller anchor for a TRY_SEND, used to distinguish bodies from transport.
    pub source: Option<Anchor>,
}
/// Polls a task may perform before a peer that is ready gets a turn (CONC-10's
/// fairness; native `POLLS_PER_TURN`).
const POLLS_PER_TURN: u32 = 64;
/// Keep the root's existing stack. Spawned tasks need a bounded reservation:
/// the checked evaluator's deepest existing recursive corpus path is 27
/// activations / 27,520 bytes. Round twice that demand up to 64 KiB, and keep
/// another 64 KiB for helper calls, thread entry and run-stop unwinding.
const ROOT_STACK: usize = 128 << 20;
const TASK_STACK: usize = 128 << 10;
const STACK_HEADROOM: usize = 64 << 10;
thread_local! {
    static TASK_STACK_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(TASK_STACK) };
    static TASK_STACK_ROOM: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, usize::MAX)) };
}
/// Run a harness control with a smaller spawned-task stack. The root keeps
/// its existing stack, and a control cannot exceed the ordinary reservation.
pub fn with_task_stack<R>(bytes: usize, run: impl FnOnce() -> R) -> R {
    assert!(
        (STACK_HEADROOM..=TASK_STACK).contains(&bytes),
        "invalid evaluator task stack"
    );
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            TASK_STACK_BYTES.with(|size| size.set(self.0));
        }
    }
    let _restore = Restore(TASK_STACK_BYTES.with(|size| size.replace(bytes)));
    run()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ready,
    Running,
    Waiting(Condition),
    /// Parked in a provider operation's W (native `State::Provider`): only its
    /// timer, readiness or a cancellation that takes effect wakes it, never `wake`.
    Provider,
    Ended,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Condition {
    Join(usize),
    Send(usize),
    Receive(usize),
}
/// A task wait or a provider deadline.
#[derive(Clone, Copy, Debug)]
enum Wait {
    Stop,
    Task(Condition),
    /// A sleep until this clock value.
    Sleep(u64),
    Network(Option<u64>),
}
/// A task's scheduler record.
struct Rec {
    id: u64,
    state: State,
    /// A cancellation request not yet observed (CONC-7).
    cancel: bool,
    polls: u32,
    /// While parked: whether a cancellation request may
    /// take effect there (no shield, not finalizing; native `cancels_operation`).
    cancellable: bool,
    /// The operation it parked in failed Cancelled in W.
    cancelled: bool,
    /// A child not yet started: its task state, function and arguments.
    start: Option<(Task, FuncId, Vec<u64>)>,
    /// Refunds from other tasks to limits it owns, applied at its next event
    /// (MEM-20; native `drain_refunds` in `tasks::event`).
    refunds: Vec<(Limit, u64)>,
}
#[derive(Default)]
struct Sched {
    /// Index 0 is the root; child `i` (handle `TASK_BASE + i`) is `i + 1`.
    tasks: Vec<Rec>,
    ready: VecDeque<usize>,
    waiting: BTreeMap<Condition, VecDeque<usize>>,
    /// Tasks that used up their polls, run only when `ready` is empty.
    deferred: VecDeque<usize>,
    /// Sleeping tasks by deadline, ties in arrival order (native `insert_timer`).
    timers: Vec<(u64, usize)>,
    /// Cancellation requests against tasks parked in a provider operation, taken
    /// at the next turn (native `cancels`).
    cancels: Vec<usize>,
    /// The running task.
    current: usize,
    /// The next task or channel id: the root is 0 (native `Scheduler::id`).
    next: u64,
    /// The CONC-10 events, when the run records them.
    events: Option<Vec<Event>>,
}
impl Sched {
    /// One turn's wakes before its pop (native `take_wakes`): cancellations of
    /// provider-parked tasks, readiness, then expired timers in deadline order.
    fn take_wakes(&mut self, now: u64, readies: Vec<u64>) {
        for k in std::mem::take(&mut self.cancels) {
            let r = &mut self.tasks[k];
            if r.state == State::Provider && r.cancel && r.cancellable {
                r.cancelled = true;
                r.state = State::Ready;
                self.timers.retain(|&(_, j)| j != k);
                self.ready.push_back(k);
            }
        }
        for token in readies {
            let k = token as usize - 1;
            if self.tasks[k].state == State::Provider {
                self.tasks[k].state = State::Ready;
                self.timers.retain(|&(_, j)| j != k);
                self.ready.push_back(k);
            }
        }
        let expired = self.timers.partition_point(|&(d, _)| d <= now);
        for (_, k) in self.timers.drain(..expired) {
            self.tasks[k].state = State::Ready;
            self.ready.push_back(k);
        }
    }
}
/// A task thread's ends of its channels to the driver.
struct Link<'m> {
    resume: mpsc::Receiver<Resume<'m>>,
    reply: mpsc::Sender<Reply<'m>>,
}
enum Resume<'m> {
    Run(Box<Machine<'m>>),
    /// The run is over while this task waits: unwind its thread.
    Teardown,
}
enum Reply<'m> {
    Parked(Box<Machine<'m>>),
    /// A task ended; the root's end carries its result and chain.
    Ended(Box<Machine<'m>>, Option<(X<Vec<u64>>, Vec<Cause>)>),
    Panicked(Box<dyn std::any::Any + Send>),
    Stopped(Box<Machine<'m>>, &'static str),
}
/// The unwind payload of a torn-down task thread.
struct Teardown;
struct RunStop(&'static str);

fn collect_arrays(ty: &FieldTy, out: &mut Vec<(FieldTy, u64)>) {
    if let FieldTy::Array(e, n) = ty {
        if !out.iter().any(|(x, k)| x == &**e && k == n) {
            out.push(((**e).clone(), *n));
        }
        collect_arrays(e, out);
    }
}

pub fn run(m: &Module, f: FuncId, args: &[u64], limit: u64) -> Report {
    run_injected(m, f, args, limit, None)
}
/// Where a run's NaNs take their one observable pattern (FORM #97: only a `reinterpret`
/// from a float observes the pattern, and it gives the canonical one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nans {
    /// Every operation's NaN result is canonical: the placement most runs use.
    Canonical,
    /// For conformance harnesses only. Each float `store` of a NaN writes a different
    /// non-canonical quiet NaN: the sign and payload change from one store to the next.
    /// A producer that reads float-holding bytes at another type, or any observer other
    /// than `reinterpret`, then sees a pattern that a run in the canonical placement
    /// does not, so a hand-derived run line catches it.
    Scrambled,
}
/// A run whose NaNs follow `nans`, with what its oracle checks covered.
pub fn run_nans(m: &Module, f: FuncId, args: &[u64], limit: u64, nans: Nans) -> (Report, Checked) {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            nans,
            ..WorldInput::default()
        },
    )
}
/// Run a work-count case, counting executed explicit copies of `bytes` bytes.
pub fn run_counting_copies(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    nans: Nans,
    bytes: u64,
) -> (Report, Checked) {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            nans,
            copy_size: Some(bytes),
            ..WorldInput::default()
        },
    )
}

/// Each described instance's code revision, supplied by the program's reach.
pub type Revisions = std::collections::BTreeMap<ContentHash, ContentHash>;
/// A run whose descriptors' revision carriers hold the supplied revisions.
pub fn run_revisions(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    revisions: &Revisions,
) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            revisions: Some(revisions),
            ..WorldInput::default()
        },
    )
    .0
}
/// A host caller's return place and explicit instruction allowance. The place is
/// frame memory outside logical admission, just as a native caller's stack place.
/// Its pointer is supplied before `args`; its bytes are copied back after the run.
/// Its descriptors' revision carriers hold `revisions`.
#[allow(clippy::too_many_arguments)]
pub fn run_nans_into(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    nans: Nans,
    destination: Option<&mut [u8]>,
    instructions: u64,
    revisions: &Revisions,
) -> (Report, Checked) {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            nans,
            destination,
            instructions,
            revisions: Some(revisions),
            ..WorldInput::default()
        },
    )
}

/// Run an entry with the root task's startup charge and teardown, as the native
/// process entry does. The limit must cover startup. May supply a deterministic
/// network and record CONC-10 events.
/// A cause's site word is `sites[item] + start` (`SourceMap::item_sites` over the
/// module's items); an empty `sites` means the module's own (`source::module_sites`).
#[allow(clippy::too_many_arguments)]
pub fn run_entry(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    nans: Nans,
    destination: Option<&mut [u8]>,
    instructions: u64,
    revisions: &Revisions,
    network: Option<Network>,
    events: bool,
    sites: &[u64],
    launch: Option<&hirsel_core_ids::launch::Metadata>,
) -> (Report, Checked) {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            nans,
            destination,
            instructions,
            revisions: Some(revisions),
            network,
            events,
            process_entry: true,
            sites,
            launch,
            ..WorldInput::default()
        },
    )
}
/// A run whose `Args` table holds `argv`.
pub fn run_args(m: &Module, f: FuncId, args: &[u64], limit: u64, argv: &[&str]) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            argv,
            ..WorldInput::default()
        },
    )
    .0
}
/// Launch an evaluator with an artifact or embedder identity captured before entry.
pub fn run_launch(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    argv: &[&str],
    launch: &hirsel_core_ids::launch::Metadata,
) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            argv,
            launch: Some(launch),
            ..WorldInput::default()
        },
    )
    .0
}
/// Deterministic admission refusal for native/evaluator agreement fixtures.
pub fn run_injected(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    injection: Option<(usize, bool)>,
) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            injection,
            ..WorldInput::default()
        },
    )
    .0
}
/// Harness-owned socket descriptors and explicit rules. Never constructed by source code.
pub struct Network {
    pub grants: BTreeMap<u64, Vec<hirsel_core_net::GrantRule>>,
    pub sockets: hirsel_core_net::Sockets,
}
impl Default for Network {
    fn default() -> Self {
        Self {
            grants: BTreeMap::new(),
            sockets: hirsel_core_net::Sockets::with_model(Default::default()),
        }
    }
}
/// Run with the network harness's configured authority and owned descriptors.
pub fn run_network_into(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    network: Network,
    remaining: &mut Network,
) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            network: Some(network),
            network_out: Some(remaining),
            ..WorldInput::default()
        },
    )
    .0
}
/// Run with the network harness's configured authority and owned descriptors.
pub fn run_network(m: &Module, f: FuncId, args: &[u64], limit: u64, network: Network) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            network: Some(network),
            ..WorldInput::default()
        },
    )
    .0
}
/// Run against supplied input bytes and environment. The evaluator reads no host
/// process state. An environment supplied here has designation zero.
pub fn run_stdio(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    stdin: &[u8],
    env: Option<hirsel_core_net::EnvSnapshot>,
) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            stdin,
            env,
            ..WorldInput::default()
        },
    )
    .0
}
struct WorldInput<'a> {
    network: Option<Network>,
    network_out: Option<&'a mut Network>,
    launch: Option<&'a hirsel_core_ids::launch::Metadata>,
    revisions: Option<&'a Revisions>,
    injection: Option<(usize, bool)>,
    argv: &'a [&'a str],
    env: Option<hirsel_core_net::EnvSnapshot>,
    stdin: &'a [u8],
    nans: Nans,
    destination: Option<&'a mut [u8]>,
    instructions: u64,
    copy_size: Option<u64>,
    events: bool,
    process_entry: bool,
    sites: &'a [u64],
}
impl Default for WorldInput<'_> {
    fn default() -> Self {
        Self {
            network: None,
            network_out: None,
            launch: None,
            revisions: None,
            injection: None,
            argv: &[],
            env: None,
            stdin: &[],
            nans: Nans::Canonical,
            destination: None,
            instructions: u64::MAX,
            copy_size: None,
            events: false,
            process_entry: false,
            sites: &[],
        }
    }
}
fn run_world(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    mut input: WorldInput<'_>,
) -> (Report, Checked) {
    let recipes = compute(&m.types).expect("layouts");
    let native = crate::native_layout::resolve(m, &recipes);
    let m = native.as_ref();
    let mut mach = Machine::new(m, recipes, limit, &input);
    for s in &m.statics {
        let (_, bytes) = s.native(&m.types, &mach.recipes);
        let p = mach.block(bytes.len() as u64, None, 0, None);
        mach.mem[split(p).0].bytes.copy_from_slice(&bytes);
        mach.mem[split(p).0].fixed = true;
    }
    let target = mach
        .world
        .launch
        .as_ref()
        .unwrap()
        .target()
        .as_bytes()
        .to_vec();
    if !target.is_empty() {
        let target_data = mach.block(target.len() as u64, None, 0, None);
        mach.mem[split(target_data).0].bytes.clone_from(&target);
        mach.mem[split(target_data).0].fixed = true;
        mach.world.launch_target = (target_data, target.len() as u64);
    }
    for a in input.argv {
        let carrier = mach.host_view(a.as_bytes());
        mach.world.args.push(carrier);
    }
    mach.world.stdin.extend(input.stdin);
    if let Some(env) = input.env.take() {
        mach.world.env_patterns.push(env.patterns);
        for (name, value) in env.entries {
            let name_view = mach.host_view(name.as_bytes());
            let value_view = std::str::from_utf8(&value)
                .ok()
                .map(|s| mach.host_view(s.as_bytes()));
            mach.world.env_entries.push((name, name_view, value_view));
        }
    }
    // One descriptor per code identity, as the linker keeps one per program:
    // `{code, length, revision}` then the identity bytes, where `code` is a
    // function token and `revision` addresses its revision root's carrier
    // (`Module::revision_root`), `{bytes, 32, 0}` from `revisions` or an
    // intrinsic's revision, else `{0, 0, 0}`.
    let mut by_code = HashMap::new();
    let mut carriers = HashMap::new();
    for body in m.functions.iter().filter_map(|f| f.body.as_ref()) {
        for inst in &body.insts {
            let Inst::DescriptorRef {
                function, identity, ..
            } = *inst
            else {
                continue;
            };
            if mach.descriptors[function.index()] != 0 {
                continue;
            }
            let code = m.code_identity(function);
            let at = if let Some(&at) = by_code.get(&code) {
                at
            } else {
                let root = m.revision_root(function);
                let carrier = *carriers.entry(root.identity().digest()).or_insert_with(|| {
                    let at = mach.block(24, None, 0, None);
                    let revision = match &root {
                        RevisionRoot::Item(identity) => input
                            .revisions
                            .and_then(|r| r.get(&identity.digest()))
                            .copied(),
                        RevisionRoot::Intrinsic(identity) => Some(intrinsic_revision(identity)),
                    };
                    if let Some(revision) = revision {
                        let bytes = mach.block(32, None, 0, None);
                        mach.mem[split(bytes).0].bytes = revision.0.to_vec();
                        mach.mem[split(bytes).0].fixed = true;
                        mach.store(at, 8, bytes);
                        mach.store(at + 8, 8, 32);
                    }
                    mach.mem[split(at).0].fixed = true;
                    at
                });
                let bytes = m.statics[identity.index()].literal_bytes().unwrap();
                let mut block = Vec::with_capacity(24 + bytes.len());
                block.extend_from_slice(&(FN_BASE + u64::from(function.0)).to_le_bytes());
                block.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                block.extend_from_slice(&[0; 8]);
                block.extend_from_slice(bytes);
                let at = mach.block(block.len() as u64, None, 0, None);
                mach.mem[split(at).0].bytes = block;
                mach.store(at + 16, 8, carrier);
                mach.mem[split(at).0].fixed = true;
                by_code.insert(code, at);
                at
            };
            mach.descriptors[function.index()] = at;
        }
    }
    for shape in &m.types {
        let fields: Vec<&hirsel_core_layout::Field> = match &shape.body {
            Shaped::Record(f) => f.iter().collect(),
            Shaped::Variant(c) => c.iter().flat_map(|c| &c.fields).collect(),
        };
        for f in fields {
            collect_arrays(&f.ty, &mut mach.arrays);
        }
    }
    let root = mach.root().clone();
    let primary_charge = if input.process_entry {
        mach.admit(&root, charge::TASK_RECORD + charge::CAUSE_CELL)
            .expect("the entry limit covers its startup charge");
        charge::CAUSE_CELL
    } else {
        // A host call borrows a task with a primary cell outside its allowance.
        0
    };
    let task = Task {
        step_bounds: Vec::new(),
        slice_steps: 0,
        limits: vec![root.clone()],
        chain: Vec::new(),
        cells: Vec::new(),
        obligations: vec![Some(Cell {
            limit: root,
            amount: primary_charge,
            resource: false,
        })],
        reserved: Vec::new(),
        at: Vec::new(),
        k: 0,
        shield: 0,
    };
    if let Some(network) = input.network.take() {
        mach.world.network = network;
    }
    // A host caller still receives an aggregate when it only inspects the
    // status. Forwarding a return may lend this place before writing to it.
    let signature = &m.signatures[m.functions[f.index()].signature.index()];
    let destination = signature.dest.map(|ty| {
        let size = input
            .destination
            .as_ref()
            .map_or_else(|| mach.held(TypeRef(ty.0)), |bytes| bytes.len() as u64);
        let p = mach.block(size, None, 0, None);
        if let Some(bytes) = input.destination.as_ref() {
            mach.mem[split(p).0].bytes.copy_from_slice(bytes);
        }
        p
    });
    let arguments = destination
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    mach.sched.tasks.push(Rec {
        id: 0,
        state: State::Ready,
        cancel: false,
        polls: POLLS_PER_TURN,
        cancellable: false,
        cancelled: false,
        start: Some((task, f, arguments)),
        refunds: Vec::new(),
    });
    mach.sched.ready.push_back(0);
    let (mut mach, outcome, chain) = drive(mach, input.process_entry);
    if let Some(bytes) = input.destination.as_mut() {
        bytes.copy_from_slice(&mach.mem[split(destination.unwrap()).0].bytes);
    }
    for (l, n) in std::mem::take(&mut mach.world.grant_charges) {
        l.find().subtract(n);
    }
    // Refunds still pending when the run ends count toward its final use.
    for k in 0..mach.sched.tasks.len() {
        for (l, n) in std::mem::take(&mut mach.sched.tasks[k].refunds) {
            l.find().subtract(n);
        }
    }
    if let Some(remaining) = input.network_out {
        *remaining = std::mem::take(&mut mach.world.network);
    }
    let sites = chain.iter().map(|c| c.site).collect();
    let checked = Checked {
        instructions: input.instructions - mach.instructions,
        loans: mach.loans,
        roles: mach.role_checks,
        copies: mach.copies,
        task_stack_peak: mach.task_stack_peak,
        task_call_depth: mach.task_call_depth,
    };
    let report = Report {
        outcome,
        used: mach.root().used(),
        peak_reserved: mach.peak_reserved,
        trace: mach.trace,
        admissions: mach.admissions,
        stdout: mach.world.stdout,
        stderr: mach.world.stderr,
        sites,
        events: mach.sched.events.take().unwrap_or_default(),
    };
    (report, checked)
}

/// Run the root and every task it spawns to the root's end, by the native default
/// scheduler's turns: pop the head of the ready queue (the poll queue when it is
/// empty) and run that task until it waits, polls past its turn, or ends.
fn drive(mach: Machine<'_>, process_entry: bool) -> (Box<Machine<'_>>, Outcome, Vec<Cause>) {
    std::thread::scope(|scope| {
        let (reply_tx, reply_rx) = mpsc::channel();
        let mut resumes: Vec<Option<mpsc::Sender<Resume<'_>>>> = Vec::new();
        let mut baton = Box::new(mach);
        let teardown = |resumes: &mut Vec<Option<mpsc::Sender<Resume<'_>>>>,
                        baton: &Machine<'_>| {
            for (k, r) in resumes.iter_mut().enumerate() {
                if let Some(r) = r.take()
                    && baton.sched.tasks[k].state != State::Ended
                {
                    let _ = r.send(Resume::Teardown);
                }
            }
        };
        loop {
            let model = baton.world.network.sockets.model().cloned().unwrap();
            model.advance_to(baton.world.clock);
            let readies = model.poll(None, false);
            let now = model.now();
            baton.world.clock = now;
            let s = &mut baton.sched;
            s.take_wakes(now, readies);
            if s.ready.is_empty() {
                std::mem::swap(&mut s.ready, &mut s.deferred);
            }
            if s.ready.is_empty() {
                let deadline = s.timers.first().map(|&(at, _)| at);
                let readies = model.poll(deadline, true);
                baton.world.clock = model.now();
                s.take_wakes(model.now(), readies);
                if s.ready.is_empty() && model.now() != now {
                    // A delivery can move accepted bytes into transit without
                    // making the receiver ready yet. Continue to its next event.
                    continue;
                }
            }
            if s.ready.is_empty() && baton.quiesce() {
                continue;
            }
            let s = &mut baton.sched;
            let Some(k) = s.ready.pop_front() else {
                baton
                    .world
                    .stderr
                    .extend_from_slice(b"run stop: Deadlock\n");
                teardown(&mut resumes, &baton);
                return (baton, Outcome::Stopped("Deadlock"), Vec::new());
            };
            s.current = k;
            s.tasks[k].state = State::Running;
            if resumes.len() <= k {
                resumes.resize_with(k + 1, || None);
            }
            if let Some((task, f, args)) = s.tasks[k].start.take() {
                let (tx, rx) = mpsc::channel();
                resumes[k] = Some(tx);
                let link = Link {
                    resume: rx,
                    reply: reply_tx.clone(),
                };
                let stack = if k == 0 {
                    ROOT_STACK
                } else {
                    TASK_STACK_BYTES.with(std::cell::Cell::get)
                };
                std::thread::Builder::new()
                    .stack_size(stack)
                    .spawn_scoped(scope, move || {
                        task_thread(task, f, args, link, stack, process_entry)
                    })
                    .expect("a task thread");
            }
            let resume = resumes[k].as_ref().expect("a started task");
            resume.send(Resume::Run(baton)).expect("the task thread");
            match reply_rx.recv().expect("a task's reply") {
                Reply::Parked(b) => baton = b,
                Reply::Ended(b, root) => {
                    baton = b;
                    resumes[k] = None;
                    if let Some((result, chain)) = root {
                        teardown(&mut resumes, &baton);
                        let outcome = match result {
                            Ok(r) => Outcome::Returned(r),
                            Err(_) => {
                                Outcome::Failed(chain.iter().map(|c| c.kind.into()).collect())
                            }
                        };
                        return (baton, outcome, chain);
                    }
                }
                Reply::Stopped(mut machine, reason) => {
                    machine
                        .world
                        .stderr
                        .extend_from_slice(format!("run stop: {reason}\n").as_bytes());
                    teardown(&mut resumes, &machine);
                    return (machine, Outcome::Stopped(reason), Vec::new());
                }
                Reply::Panicked(payload) => {
                    // The panicking task's machine is gone with its stack; every
                    // other task is parked and is unwound before the panic goes on.
                    for r in resumes.iter_mut().enumerate().filter(|(j, _)| *j != k) {
                        if let Some(r) = r.1.take() {
                            let _ = r.send(Resume::Teardown);
                        }
                    }
                    std::panic::resume_unwind(payload)
                }
            }
        }
    })
}

/// A task thread: wait for the baton, run the task's body, hand the baton back.
/// The thread's link sits in the baton while the task runs (`Machine::park`).
fn task_thread<'m>(
    mut t: Task,
    f: FuncId,
    args: Vec<u64>,
    link: Link<'m>,
    stack: usize,
    process_entry: bool,
) {
    let origin = 0u8;
    TASK_STACK_ROOM.with(|room| {
        room.set((
            &origin as *const u8 as usize,
            if t.k == 0 {
                usize::MAX
            } else {
                stack.saturating_sub(STACK_HEADROOM)
            },
        ))
    });
    let Ok(Resume::Run(mut mach)) = link.resume.recv() else {
        return;
    };
    let (k, reply) = (t.k, link.reply.clone());
    mach.links[k] = Some(link);
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let root = if k == 0 {
            let r = mach.call(&mut t, f, &args);
            // The root ends as every task does (native `start`): END, failed or not.
            mach.event(END, 0, u64::from(r.is_err()));
            if process_entry {
                mach.retire_root(&mut t);
            }
            Some((r, std::mem::take(&mut t.chain)))
        } else {
            mach.child_main(&mut t, f, &args);
            None
        };
        mach.links[k] = None;
        root
    }));
    match run {
        Ok(root) => {
            let _ = reply.send(Reply::Ended(mach, root));
        }
        Err(payload) if payload.is::<Teardown>() => {}
        Err(payload) if payload.is::<RunStop>() => {
            let reason = payload.downcast::<RunStop>().unwrap().0;
            mach.links[k] = None;
            let _ = reply.send(Reply::Stopped(mach, reason));
        }
        Err(payload) => {
            let _ = reply.send(Reply::Panicked(payload));
        }
    }
}

fn ptr(blk: usize, off: u64) -> u64 {
    ((blk as u64 + 1) << 32) | off
}
fn split(p: u64) -> (usize, usize) {
    assert!(p >> 32 != 0, "{p:#x} is not a memory pointer");
    (((p >> 32) - 1) as usize, (p & 0xffff_ffff) as usize)
}
fn bytes_of(t: Ty) -> usize {
    (width(t) / 8) as usize
}
fn cause(r: RaiseCause) -> &'static str {
    match r {
        RaiseCause::Unwrap => "Unwrap",
        RaiseCause::Bounds => "Bounds",
        RaiseCause::Fail(_) => "Fail",
        RaiseCause::Capacity { .. } => "Capacity",
    }
}

impl<'m> Machine<'m> {
    fn new(m: &'m Module, recipes: Vec<Recipe>, limit: u64, input: &WorldInput<'_>) -> Self {
        Self {
            next_step_bound: 1,
            m,
            recipes,
            mem: Blocks::default(),
            peak_reserved: 0,
            root: Some(Limit::new(limit, None, 0)),
            regions: Vec::new(),
            children: Vec::new(),
            chans: Vec::new(),
            latests: Vec::new(),
            causes: HashMap::new(),
            arrays: Vec::new(),
            trace: Vec::new(),
            dests: 0,
            admissions: 0,
            instructions: input.instructions,
            injection: input.injection,
            world: World {
                launch: Some(input.launch.cloned().unwrap_or_else(|| {
                    hirsel_core_ids::launch::Metadata::source(
                        "",
                        hirsel_core_ids::launch::DEFAULT_N_PLACE,
                        false,
                    )
                })),
                ..World::default()
            },
            frozen: Vec::new(),
            loans: 0,
            facts: Arc::new(roles::Facts::new(m)),
            roles: vec![None; m.functions.len()],
            footprints: Vec::new(),
            role_checks: 0,
            copy_size: input.copy_size,
            copies: 0,
            task_stack_peak: 0,
            task_call_depth: 0,
            inherits: m
                .functions
                .iter()
                .map(|f| crate::source::inherits_site(&m.instances[f.instance.index()]))
                .collect(),
            sites: if input.sites.is_empty() {
                crate::source::module_sites(m).into()
            } else {
                input.sites.into()
            },
            scrambled: (input.nans == Nans::Scrambled).then_some(0),
            sched: Sched {
                next: 1,
                events: input.events.then(Vec::new),
                ..Sched::default()
            },
            links: vec![None],
            descriptors: vec![0; m.functions.len()],
        }
    }

    /// What a parked task's slot holds while its baton runs elsewhere.
    fn shell(&self) -> Machine<'m> {
        Machine {
            next_step_bound: 1,
            m: self.m,
            recipes: Vec::new(),
            mem: Blocks::default(),
            root: None,
            peak_reserved: 0,
            regions: Vec::new(),
            children: Vec::new(),
            chans: Vec::new(),
            latests: Vec::new(),
            causes: HashMap::new(),
            arrays: Vec::new(),
            trace: Vec::new(),
            dests: 0,
            admissions: 0,
            instructions: 0,
            injection: None,
            world: World::default(),
            frozen: Vec::new(),
            loans: 0,
            facts: self.facts.clone(),
            roles: Vec::new(),
            footprints: Vec::new(),
            role_checks: 0,
            copy_size: None,
            copies: 0,
            task_stack_peak: 0,
            task_call_depth: 0,
            inherits: Vec::new(),
            sites: self.sites.clone(),
            scrambled: None,
            sched: Sched::default(),
            links: Vec::new(),
            descriptors: Vec::new(),
        }
    }

    /// Switch to the scheduler. With a wait, the running task blocks until its
    /// condition changes or it can observe cancellation. Otherwise it goes to the poll
    /// queue's tail (native `poll_turn`). It continues when the driver hands the
    /// baton back; its frozen places and footprints stay with it meanwhile.
    fn park(&mut self, wait: Option<Wait>) {
        self.park_turn(wait, true);
    }
    fn park_turn(&mut self, wait: Option<Wait>, poll: bool) {
        let k = self.sched.current;
        match wait {
            Some(Wait::Sleep(deadline)) | Some(Wait::Network(Some(deadline))) => {
                self.sched.tasks[k].state = State::Provider;
                let s = &mut self.sched;
                let at = s.timers.partition_point(|&(d, _)| d <= deadline);
                s.timers.insert(at, (deadline, k));
            }
            Some(Wait::Network(None) | Wait::Stop) => self.sched.tasks[k].state = State::Provider,
            Some(Wait::Task(condition)) => {
                self.sched.tasks[k].state = State::Waiting(condition);
                self.sched
                    .waiting
                    .entry(condition)
                    .or_default()
                    .push_back(k);
            }
            None => {
                self.sched.tasks[k].state = State::Ready;
                if poll {
                    self.sched.deferred.push_back(k);
                } else {
                    self.sched.ready.push_back(k);
                }
            }
        }
        let link = self.links[k].take().expect("the running task's link");
        let frozen = std::mem::take(&mut self.frozen);
        let footprints = std::mem::take(&mut self.footprints);
        let shell = self.shell();
        let baton = std::mem::replace(self, shell);
        link.reply
            .send(Reply::Parked(Box::new(baton)))
            .expect("the driver");
        match link.resume.recv().expect("the driver") {
            Resume::Run(baton) => *self = *baton,
            Resume::Teardown => std::panic::resume_unwind(Box::new(Teardown)),
        }
        self.frozen = frozen;
        self.footprints = footprints;
        self.links[k] = Some(link);
    }
}

impl Machine<'_> {
    fn cell(&self, handle: u64) -> Option<usize> {
        handle
            .checked_sub(CELL_BASE)
            .map(|i| i as usize)
            .filter(|&i| i < self.world.cells.len())
    }
    /// An operation's optional `Until` triple at `a[i..i + 3]`: present, clock, at.
    fn until(&self, a: &[u64], i: usize) -> Option<Due> {
        (a[i] != 0).then(|| match self.cell(a[i + 1]) {
            Some(c) => Due::Cell(c, a[i + 2]),
            None => Due::Host(a[i + 2]),
        })
    }
    fn passed(&self, due: Due) -> bool {
        match due {
            Due::Host(at) => at <= self.world.clock,
            Due::Cell(c, at) => match self.world.cells[c] {
                VirtualCell::Clock { mono, .. } => at <= mono,
                _ => unreachable!("an Until names a Clock"),
            },
        }
    }
    fn cell_timer(&mut self, c: usize, at: u64, k: usize) {
        if let VirtualCell::Clock { timers, .. } = &mut self.world.cells[c]
            && !timers.iter().any(|&(_, j)| j == k)
        {
            timers.push((at, k));
        }
    }
    fn ready_provider(&mut self, k: usize) {
        if self.sched.tasks[k].state == State::Provider {
            self.sched.tasks[k].state = State::Ready;
            self.sched.ready.push_back(k);
        }
    }
    /// Park the running task on a cell until `done` holds; a cancellation that
    /// takes effect there fails the wait Cancelled (native W park).
    fn cell_wait(
        &mut self,
        t: &Task,
        c: usize,
        register: fn(&mut VirtualCell, usize),
        done: fn(&VirtualCell, usize) -> bool,
    ) -> X<()> {
        while !done(&self.world.cells[c], t.k) {
            register(&mut self.world.cells[c], t.k);
            self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
            self.park(Some(Wait::Network(None)));
            if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                self.check_cancel(t)?;
            }
        }
        Ok(())
    }
    /// The run is quiescent: wake its `clock.idle` waits after pruning deadlines
    /// no parked task waits on (native `provide::quiesce`).
    fn quiesce(&mut self) -> bool {
        let tasks = &self.sched.tasks;
        let parked = |k: usize| tasks[k].state == State::Provider;
        let mut woken = Vec::new();
        for cell in &mut self.world.cells {
            if let VirtualCell::Clock { timers, idle, .. } = cell {
                timers.retain(|&(_, k)| parked(k));
                for (k, flag) in idle.iter_mut() {
                    if !*flag && parked(*k) {
                        *flag = true;
                        woken.push(*k);
                    }
                }
            }
        }
        for &k in &woken {
            self.ready_provider(k);
        }
        !woken.is_empty()
    }

    /// The `provide` rows, and Clock, Random and Process operations on a cell.
    fn provide(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Option<X<Vec<u64>>> {
        let ok = |value: u64, written: u64| vec![value, 0, 0, written];
        if matches!(
            op,
            HostOp::ProvideClock | HostOp::ProvideRandom | HostOp::ProvideProcess
        ) {
            // A cell holds PROVIDE_CELL until the run ends, as a narrowed grant
            // (MEM-24); a refusal answers 0 with the available bytes.
            let payer = t.limits.last().unwrap();
            let charge = hirsel_core_layout::charge::PROVIDE_CELL;
            if let Err(available) = self.admit(payer, charge) {
                return Some(Ok(ok(0, available)));
            }
            self.world.grant_charges.push((payer.clone(), charge));
        }
        let new = |w: &mut World, cell| {
            w.cells.push(cell);
            CELL_BASE + w.cells.len() as u64 - 1
        };
        let reply = match op {
            HostOp::ProvideClock => {
                let h = new(
                    &mut self.world,
                    VirtualCell::Clock {
                        mono: a[0],
                        wall: a[1],
                        timers: Vec::new(),
                        idle: Vec::new(),
                    },
                );
                ok(h, h)
            }
            HostOp::ProvideRandom => ok(new(&mut self.world, VirtualCell::Random(a[0])), 0),
            HostOp::ProvideProcess => {
                let process = VirtualCell::Process {
                    stopped: false,
                    waiters: Vec::new(),
                };
                let h = new(&mut self.world, process);
                ok(h, h)
            }
            HostOp::ClockAdvance => {
                let c = self.cell(a[0]).unwrap();
                let VirtualCell::Clock { mono, timers, .. } = &mut self.world.cells[c] else {
                    unreachable!()
                };
                *mono = (*mono).max(a[1]);
                let now = *mono;
                let mut due: Vec<_> = timers
                    .iter()
                    .copied()
                    .filter(|&(at, _)| at <= now)
                    .collect();
                timers.retain(|&(at, _)| at > now);
                due.sort_by_key(|&(at, _)| at);
                for (_, k) in due {
                    self.ready_provider(k);
                }
                ok(0, 0)
            }
            HostOp::ClockSetWall => {
                let c = self.cell(a[0]).unwrap();
                if let VirtualCell::Clock { wall, .. } = &mut self.world.cells[c] {
                    *wall = a[1];
                }
                ok(0, 0)
            }
            HostOp::ProcessTrigger => {
                let c = self.cell(a[0]).unwrap();
                let VirtualCell::Process { stopped, waiters } = &mut self.world.cells[c] else {
                    unreachable!()
                };
                *stopped = true;
                for k in std::mem::take(waiters) {
                    self.ready_provider(k);
                }
                ok(0, 0)
            }
            HostOp::ClockIdle => {
                let c = self.cell(a[0]).unwrap();
                if let Err(cancelled) = self.check_cancel(t) {
                    return Some(Err(cancelled));
                }
                let waited = self.cell_wait(
                    t,
                    c,
                    |cell, k| {
                        if let VirtualCell::Clock { idle, .. } = cell
                            && !idle.iter().any(|&(j, _)| j == k)
                        {
                            idle.push((k, false));
                        }
                    },
                    |cell, k| match cell {
                        VirtualCell::Clock { idle, .. } => {
                            idle.iter().any(|&(j, woken)| j == k && woken)
                        }
                        _ => unreachable!(),
                    },
                );
                if let VirtualCell::Clock { idle, .. } = &mut self.world.cells[c] {
                    idle.retain(|&(j, _)| j != t.k);
                }
                if let Err(cancelled) = waited {
                    return Some(Err(cancelled));
                }
                let VirtualCell::Clock { timers, .. } = &self.world.cells[c] else {
                    unreachable!()
                };
                match timers.iter().map(|&(at, _)| at).min() {
                    Some(at) => ok(at, 1),
                    None => ok(0, 0),
                }
            }
            HostOp::WallMs
            | HostOp::MonoNs
            | HostOp::SleepUntil
            | HostOp::Random
            | HostOp::Stopped => {
                let c = self.cell(a[0])?;
                if !matches!(op, HostOp::WallMs | HostOp::MonoNs)
                    && let Err(cancelled) = self.check_cancel(t)
                {
                    return Some(Err(cancelled));
                }
                match op {
                    HostOp::WallMs | HostOp::MonoNs => {
                        let VirtualCell::Clock { mono, wall, .. } = self.world.cells[c] else {
                            unreachable!()
                        };
                        vec![if op == HostOp::MonoNs { mono } else { wall }]
                    }
                    HostOp::SleepUntil => {
                        let at = a[1];
                        loop {
                            let VirtualCell::Clock { mono, .. } = self.world.cells[c] else {
                                unreachable!()
                            };
                            if at <= mono {
                                break;
                            }
                            self.cell_timer(c, at, t.k);
                            self.sched.tasks[t.k].cancellable =
                                t.shield == 0 && t.obligations.len() == 1;
                            self.park(Some(Wait::Network(None)));
                            if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                                if let VirtualCell::Clock { timers, .. } = &mut self.world.cells[c]
                                {
                                    timers.retain(|&(_, k)| k != t.k);
                                }
                                if let Err(cancelled) = self.check_cancel(t) {
                                    return Some(Err(cancelled));
                                }
                            }
                        }
                        ok(0, 0)
                    }
                    HostOp::Random => {
                        let (_, data, len, cap) = self.carrier(a[1]);
                        let spare = cap.saturating_sub(len) as usize;
                        let VirtualCell::Random(state) = &mut self.world.cells[c] else {
                            unreachable!()
                        };
                        let mut bytes = Vec::with_capacity(spare);
                        while bytes.len() < spare {
                            // SplitMix64: eight little-endian bytes a step.
                            *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                            let mut z = *state;
                            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                            let step = (z ^ (z >> 31)).to_le_bytes();
                            let n = (spare - bytes.len()).min(8);
                            bytes.extend_from_slice(&step[..n]);
                        }
                        self.put(data + len, &bytes);
                        self.store(a[1] + 8, 8, len + spare as u64);
                        ok(0, spare as u64)
                    }
                    _ => {
                        let waited = self.cell_wait(
                            t,
                            c,
                            |cell, k| {
                                if let VirtualCell::Process { waiters, .. } = cell
                                    && !waiters.contains(&k)
                                {
                                    waiters.push(k);
                                }
                            },
                            |cell, _| matches!(cell, VirtualCell::Process { stopped: true, .. }),
                        );
                        if let VirtualCell::Process { waiters, .. } = &mut self.world.cells[c] {
                            waiters.retain(|&k| k != t.k);
                        }
                        if let Err(cancelled) = waited {
                            return Some(Err(cancelled));
                        }
                        vec![]
                    }
                }
            }
            _ => return None,
        };
        Some(Ok(reply))
    }

    fn park_task(&mut self, t: &Task, condition: Condition) {
        self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
        self.park(Some(Wait::Task(condition)));
    }

    fn wake(&mut self, condition: Condition) {
        if let Some(waiters) = self.sched.waiting.remove(&condition) {
            for k in waiters {
                self.sched.tasks[k].state = State::Ready;
                self.sched.ready.push_back(k);
            }
        }
    }

    /// Record an event of the running task (CONC-10). A poll uses one of the
    /// task's polls; the last one gives a ready peer the turn (native `poll_event`).
    fn event(&mut self, kind: u8, object: u64, value: u64) {
        let k = self.sched.current;
        if k >= self.sched.tasks.len() {
            return;
        }
        self.drain_refunds();
        let task = self.sched.tasks[k].id;
        if let Some(events) = &mut self.sched.events {
            events.push(Event {
                kind,
                task,
                object,
                value,
                source: None,
            });
        }
        if is_poll(kind, value) {
            let r = &mut self.sched.tasks[k];
            r.polls -= 1;
            if r.polls == 0 {
                r.polls = POLLS_PER_TURN;
                if !self.sched.ready.is_empty() || !self.sched.deferred.is_empty() {
                    self.park(None);
                }
            }
        }
    }
    /// A cancellation point (CONC-7): a pending request takes effect unless the
    /// task is in a finalizer or a shielded cleanup, and observing it fails the
    /// operation with `Cancelled` (native `cancel_takes_effect`).
    fn check_cancel(&mut self, t: &Task) -> X<()> {
        let r = &mut self.sched.tasks[t.k];
        if r.cancel && t.shield == 0 && t.obligations.len() == 1 {
            r.cancel = false;
            self.event(CANCELLED, 0, 0);
            return Err(Stop::Cause("Cancelled"));
        }
        Ok(())
    }
    /// Request a child's cancellation (CONC-7). A shielded child stays parked.
    fn cancel(&mut self, i: usize) {
        let r = &mut self.sched.tasks[i + 1];
        r.cancel = true;
        let id = r.id;
        match r.state {
            State::Provider => self.sched.cancels.push(i + 1),
            State::Waiting(condition) if r.cancellable => {
                let waiters = self.sched.waiting.get_mut(&condition).unwrap();
                waiters.retain(|&k| k != i + 1);
                if waiters.is_empty() {
                    self.sched.waiting.remove(&condition);
                }
                r.state = State::Ready;
                self.sched.ready.push_back(i + 1);
            }
            _ => {}
        }
        self.event(CANCEL, id, 0);
    }
    /// A child's body: run it, keep its result or chain for the join, then END
    /// and wake its joiner (native `start`).
    fn child_main(&mut self, t: &mut Task, f: FuncId, args: &[u64]) {
        let i = t.k - 1;
        let returned = self.call(t, f, args);
        let (rb, r) = (self.children[i].result, self.children[i].r);
        if let (Ok(values), Carried::Value(ty)) = (&returned, self.m.types[r.index()].carried) {
            self.store(rb, bytes_of(ty), values[0]);
        }
        self.end_limit(t.limits.pop().unwrap(), None);
        let status = match t.chain.first().map(|c| c.kind) {
            _ if returned.is_ok() => 0,
            Some("Cancelled") => 2,
            _ => 1,
        };
        let c = &mut self.children[i];
        c.status = status;
        c.cells = std::mem::take(&mut t.cells);
        c.chain = std::mem::take(&mut t.chain);
        c.primary = t.obligations[0].take();
        let id = self.sched.tasks[t.k].id;
        self.event(END, id, u64::from(status != 0));
        self.sched.tasks[t.k].state = State::Ended;
        self.wake(Condition::Join(i));
    }

    fn retire_root(&mut self, t: &mut Task) {
        // FAIL-10: refund the unused obligation and the cells in any failure
        // diagnosed at the process boundary. Keep the cause kinds and sites.
        if let Some(primary) = t.obligations[0].take() {
            self.refund(primary.limit, primary.amount);
        }
        for (cause, (payer, amount)) in t.chain.iter().zip(t.cells.drain(..)) {
            if cause.kind == "Fail" && cause.message[2] != 0 {
                self.free(cause.message[0]);
            }
            self.refund(payer, amount);
        }
        self.refund(t.limits[0].clone(), charge::TASK_RECORD);
    }

    /// The child task a handle names.
    fn child(&self, handle: u64) -> usize {
        handle.checked_sub(TASK_BASE).unwrap_or_else(|| {
            panic!(
                "a task handle that was never spawned ({handle:#x}): a refused \
                 spawn's null handle reached the operation"
            )
        }) as usize
    }
    fn chan_id(&self, i: usize) -> u64 {
        self.chans[i].id
    }
}

impl Machine<'_> {
    // ---- memory
    /// Rc and Arc counts differ in atomicity only, which one task cannot observe;
    /// what the evaluator checks is that every operation names its node's kind.
    /// A `String` or `Bytes` carrier's bytes, data pointer, length and capacity.
    fn carrier(&self, c: u64) -> (Vec<u8>, u64, u64, u64) {
        let (data, len, cap) = (self.load(c, 8), self.load(c + 8, 8), self.load(c + 16, 8));
        let bytes = match len {
            0 => Vec::new(),
            n => self.get(data, n as usize).to_vec(),
        };
        (bytes, data, len, cap)
    }
    fn host_view(&mut self, bytes: &[u8]) -> u64 {
        let len = bytes.len() as u64;
        let data = self.block(len, None, 0, None);
        self.mem[split(data).0].bytes.copy_from_slice(bytes);
        let carrier = self.block(24, None, 0, None);
        self.store(carrier, 8, data);
        self.store(carrier + 8, 8, len);
        self.mem[split(data).0].fixed = true;
        self.mem[split(carrier).0].fixed = true;
        carrier
    }

    fn env(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Vec<u64> {
        use hirsel_core_net::EnvPatterns;
        let patterns = self.world.env_patterns.get(a[0] as usize);
        let covered = |name: &str| patterns.is_some_and(|p| p.covers(name));
        let mut rows = self.world.env_entries.iter().filter(|row| covered(&row.0));
        let ok = |value| vec![value, 0, 0, 0];
        let error = |tag| vec![0, tag, 0, 0];
        match op {
            HostOp::EnvLen => ok(rows.count() as u64),
            HostOp::EnvName => ok(rows.nth(a[1] as usize).map_or(0, |row| row.1)),
            HostOp::EnvGet => {
                let name = std::str::from_utf8(self.carrier_bytes(a[1])).unwrap();
                if !covered(name) {
                    return error(1);
                }
                match self
                    .world
                    .env_entries
                    .binary_search_by(|row| row.0.as_str().cmp(name))
                {
                    Ok(i) => self.world.env_entries[i].2.map_or_else(|| error(2), ok),
                    Err(_) => ok(0),
                }
            }
            HostOp::EnvNarrow => {
                let text = std::str::from_utf8(self.carrier_bytes(a[1])).unwrap();
                let Some(filter) = EnvPatterns::parse(text) else {
                    return error(1);
                };
                let result = patterns.map_or_else(EnvPatterns::default, |p| p.intersect(&filter));
                let charge = result.charge();
                let payer = t.limits.last().unwrap();
                let requested = charge.unwrap_or(u64::MAX);
                if let Err(available) = self.admit_charge(payer, requested, charge.is_none()) {
                    return vec![requested, 2, 0, available];
                }
                let key = self.world.env_patterns.len() as u64;
                self.world.env_patterns.push(result);
                self.world.grant_charges.push((payer.clone(), requested));
                ok(key)
            }
            _ => unreachable!(),
        }
    }

    fn carrier_bytes(&self, carrier: u64) -> &[u8] {
        let len = self.load(carrier + 8, 8);
        if len == 0 {
            &[]
        } else {
            self.get(self.load(carrier, 8), len as usize)
        }
    }
    fn deny_describe_capability(&self) {
        if self
            .world
            .launch
            .as_ref()
            .is_some_and(|launch| launch.is_describe())
        {
            std::panic::resume_unwind(Box::new(RunStop("DescribeCapability")));
        }
    }
    fn network_host(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::model::{Address, Operation, Reply};
        let model = self.world.network.sockets.model().unwrap().clone();
        model.advance_to(self.world.clock);
        let address = || Address {
            family: a[1] as u8,
            hi: a[2],
            lo: a[3],
            port: a[4] as u16,
        };
        let operation = match op {
            HostOp::Connect | HostOp::Listen => {
                let address = address();
                let allowed = self.world.network.grants.get(&a[0]).is_some_and(|rules| {
                    rules.iter().any(|r| {
                        matches!(
                            r,
                            hirsel_core_net::GrantRule::Address(r) if r.permits(
                                u8::from(op == HostOp::Listen),
                                address.family,
                                address.hi,
                                address.lo,
                                address.port,
                            )
                        )
                    })
                });
                if !allowed {
                    let mut r = vec![0, 1, 0, 0];
                    if op == HostOp::Listen {
                        r.extend([0; 4]);
                    }
                    return Ok(r);
                }
                if op == HostOp::Connect {
                    Operation::Connect(address)
                } else {
                    Operation::Listen(address, a[5] as u16)
                }
            }
            HostOp::Accept => Operation::Accept(a[0]),
            HostOp::RecvBounded => Operation::Recv(a[0]),
            HostOp::SendPair => Operation::Send(a[0]),
            _ => unreachable!(),
        };
        let (input, data, len, cap) = if matches!(op, HostOp::RecvBounded | HostOp::SendPair) {
            self.carrier(a[1])
        } else {
            (vec![], 0, 0, 0)
        };
        let tail = if op == HostOp::SendPair {
            self.carrier(a[2]).0
        } else {
            vec![]
        };
        let until = match op {
            HostOp::Connect => 5,
            HostOp::Accept => 1,
            HostOp::SendPair | HostOp::RecvBounded => 3,
            _ => 2,
        };
        let deadline = if op == HostOp::Listen {
            None
        } else {
            self.until(a, until)
        };
        let spare = if op == HostOp::RecvBounded {
            cap.saturating_sub(len).min(a[2]) as usize
        } else {
            0
        };
        let reply = self.network_attempt(t, &model, deadline, |token| {
            model.attempt_parts(operation, [&input, &tail], spare, token)
        });
        let Reply {
            value,
            error,
            code,
            address,
            bytes,
            ..
        } = reply;
        if error == 0 {
            if matches!(op, HostOp::Connect | HostOp::Listen | HostOp::Accept) {
                self.world.network.sockets.adopt_model(&model, value);
            }
            if op == HostOp::RecvBounded {
                if !bytes.is_empty() {
                    self.put(data + len, &bytes);
                }
                self.store(a[1] + 8, 8, len + bytes.len() as u64);
            }
        }
        let mut result = vec![value, u64::from(error), u64::from(code), 0];
        if matches!(op, HostOp::Listen | HostOp::Accept) {
            result.extend([
                u64::from(address.family),
                address.hi,
                address.lo,
                u64::from(address.port),
            ]);
        }
        Ok(result)
    }
    fn network_resolve(&mut self, a: &[u64]) -> Vec<u64> {
        use hirsel_core_net::{GrantRule, Ip, Name};
        let Some(name) = std::str::from_utf8(self.carrier_bytes(a[1]))
            .ok()
            .and_then(Name::parse)
        else {
            return vec![0, 1, 0, 0];
        };
        if !self.world.network.grants.get(&a[0]).is_some_and(|rules| {
            rules
                .iter()
                .any(|r| matches!(r, GrantRule::Resolve(rule) if rule.permits(&name)))
        }) {
            return vec![0, 1, 0, 0];
        }
        if self.until(a, 3).is_some_and(|due| self.passed(due)) {
            return vec![0, 6, 0, 0];
        }
        let (data, len, cap) = (
            self.load(a[2], 8),
            self.load(a[2] + 8, 8),
            self.load(a[2] + 16, 8),
        );
        let model = self.world.network.sockets.model().unwrap();
        let addresses = model.resolve(&name, cap.saturating_sub(len) as usize);
        let count = addresses.len() as u64;
        for (index, address) in addresses.into_iter().enumerate() {
            let offset = (len + index as u64) * Ip::SIZE as u64;
            self.put(data + offset, &Ip::from(address).layout_bytes());
        }
        self.store(a[2] + 8, 8, len + count);
        vec![count, 0, 0, 0]
    }

    fn network_attempt(
        &mut self,
        t: &Task,
        model: &hirsel_core_net::model::Model,
        deadline: Option<Due>,
        mut attempt: impl FnMut(u64) -> Option<hirsel_core_net::model::Reply>,
    ) -> hirsel_core_net::model::Reply {
        let token = t.k as u64 + 1;
        let reply = loop {
            if let Some(reply) = attempt(token) {
                break reply;
            }
            if deadline.is_some_and(|due| self.passed(due)) {
                break model.abort(token, false);
            }
            self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
            let host = match deadline {
                Some(Due::Host(at)) => Some(at),
                Some(Due::Cell(c, at)) => {
                    self.cell_timer(c, at, t.k);
                    None
                }
                None => None,
            };
            self.park(Some(Wait::Network(host)));
            if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                break model.abort(token, true);
            }
        };
        self.world.clock = model.now();
        reply
    }

    fn tls_offer(&self, a: &[u64], port: u16) -> Result<Vec<Vec<u8>>, (u64, u64)> {
        use hirsel_core_net::{GrantRule, tls};
        let name = tls::verification_name(self.carrier_bytes(a[2])).ok_or((9, 1))?;
        let len = self.load(a[5] + 8, 8);
        if len > 16 {
            return Err((9, 2));
        }
        let data = self.load(a[5], 8);
        let protocols: Vec<_> = (0..len)
            .map(|i| self.carrier_bytes(data + i * 24))
            .collect();
        if !tls::valid_protocols(&protocols) {
            return Err((9, 2));
        }
        let pem = if a[3] == 1 {
            self.carrier_bytes(a[4])
        } else {
            &[]
        };
        let (trust, _) = tls::trust(a[3] as u8, pem).ok_or((9, 3))?;
        let allowed = self.world.network.grants.get(&a[0]).is_some_and(|rules| {
            rules.iter().any(
                |rule| matches!(rule, GrantRule::Tls(rule) if rule.permits(&name, port, trust)),
            )
        });
        if !allowed {
            return Err((1, 0));
        }
        Ok(protocols.into_iter().map(<[u8]>::to_vec).collect())
    }

    fn tls_connect(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        let key = a[1];
        let model = self.world.network.sockets.model().unwrap().clone();
        let offered = match model
            .connected_peer(key)
            .ok_or((1, 0))
            .and_then(|peer| self.tls_offer(a, peer.port))
        {
            Ok(offered) => offered,
            Err((tag, code)) => {
                self.world.network.sockets.release(key);
                return Ok(vec![0, tag, code, 0, 0, 0, 0, 0]);
            }
        };
        if let Err(cancelled) = self.check_cancel(t) {
            self.world.network.sockets.release(key);
            return Err(cancelled);
        }
        model.advance_to(self.world.clock);
        let deadline = self.until(a, 6);
        let reply = self.network_attempt(t, &model, deadline, |token| {
            model.attempt_tls(key, &offered, token)
        });
        if reply.error != 0 {
            self.world.network.sockets.release(key);
            return Ok(vec![
                0,
                reply.error.into(),
                reply.code.into(),
                0,
                0,
                0,
                0,
                0,
            ]);
        }
        self.world.network.sockets.split(key);
        let name = hirsel_core_net::tls::verification_name(self.carrier_bytes(a[2])).unwrap();
        self.world.tls_writes.insert(key & !3, (name, false));
        let protocol = if reply.bytes.is_empty() {
            0
        } else {
            offered.iter().position(|p| *p == reply.bytes).unwrap() as u64 + 1
        };
        let mut result = vec![reply.value, 0, 0, protocol];
        result.extend(hirsel_core_net::tls::peer_words(reply.peer));
        Ok(result)
    }

    fn narrow_net(&mut self, t: &Task, parent: u64, rule: hirsel_core_net::GrantRule) -> Vec<u64> {
        let grants = &self.world.network.grants;
        let intersections: Vec<_> = grants
            .get(&parent)
            .into_iter()
            .flatten()
            .filter_map(|r| r.intersect(&rule))
            .collect();
        let charge = intersections
            .iter()
            .try_fold(0u64, |total, r| total.checked_add(r.logical_size()))
            .and_then(|n| n.checked_add(7))
            .map(|n| n & !7)
            .expect("host-sized rule storage");
        let payer = t.limits.last().unwrap();
        if let Err(available) = self.admit(payer, charge) {
            return vec![charge, 2, 0, available];
        }
        let grants = &mut self.world.network.grants;
        let key = grants
            .last_key_value()
            .map_or(0, |(&key, _)| key)
            .checked_add(1)
            .expect("run-local handle counter");
        grants.insert(key, intersections);
        self.world.grant_charges.push((payer.clone(), charge));
        vec![key, 0, 0, 0]
    }

    fn secret_policy(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Vec<u64> {
        let Some(policy) = std::str::from_utf8(self.carrier_bytes(a[1]))
            .ok()
            .and_then(SecretPolicy::parse)
        else {
            return vec![0, 1, 0, 0];
        };
        let (policy, len) = if op == HostOp::SecretSeal {
            (policy, self.load(a[0] + 8, 8))
        } else {
            let Some(parent) = self.world.secrets.get(a[0] as usize) else {
                return vec![0, 1, 0, 0];
            };
            let Some(intersection) = parent.policy.intersect(&policy) else {
                return vec![0, 1, 0, 0];
            };
            (intersection, 0)
        };
        let size = SecretGrant::logical_size(&policy.patterns, len);
        let charge = size.unwrap_or(u64::MAX);
        let payer = t.limits.last().unwrap();
        if let Err(available) = self.admit_charge(payer, charge, size.is_none()) {
            return vec![charge, 2, 0, available];
        }
        let bytes = if op == HostOp::SecretSeal {
            let data = self.load(a[0], 8);
            let bytes = Zeroizing::new(self.carrier_bytes(a[0]).to_vec());
            if !bytes.is_empty() && self.load(a[0] + 16, 8) != 0 {
                let (block, offset) = split(data);
                use zeroize::Zeroize;
                self.mem[block].bytes[offset..offset + bytes.len()].zeroize();
            }
            Arc::new(bytes)
        } else {
            self.world.secrets[a[0] as usize].bytes.clone()
        };
        let key = self.world.secrets.len() as u64;
        self.world.secrets.push(EvalSecret { policy, bytes });
        self.world.grant_charges.push((payer.clone(), charge));
        vec![key, 0, 0, 0]
    }

    fn secret_mac(&mut self, op: HostOp, a: &[u64]) -> Vec<u64> {
        let secret = &self.world.secrets[a[0] as usize];
        let signing = op == HostOp::SecretHmac;
        let needed = if signing {
            SecretUse::Mac
        } else {
            SecretUse::Verify
        };
        if secret.policy.usage < needed {
            return vec![0, 1, 0, 0];
        }
        let data = self.carrier_bytes(a[1]);
        if signing {
            let tag = hirsel_core_net::hmac_sha256(&secret.bytes, data);
            for (i, byte) in tag.into_iter().enumerate() {
                self.store(a[2] + i as u64, 1, u64::from(byte));
            }
            vec![0, 0, 0, 0]
        } else {
            let verdict =
                hirsel_core_net::verify_hmac_sha256(&secret.bytes, data, self.carrier_bytes(a[2]));
            vec![u64::from(verdict), 0, 0, 0]
        }
    }

    fn send_secret(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        let key = a[0];
        let Some((name, spent)) = self.world.tls_writes.get(&(key & !3)) else {
            return Ok(vec![0, 1, 0, 0]);
        };
        if *spent {
            return Ok(vec![0, 5, 0, 0]);
        }
        let Some(secret) = self.world.secrets.get(a[2] as usize) else {
            return Ok(vec![0, 1, 0, 0]);
        };
        if !secret
            .policy
            .patterns
            .iter()
            .any(|pattern| pattern.covers(name))
        {
            return Ok(vec![0, 1, 0, 0]);
        }
        let bytes = secret.bytes.clone();
        let before = self.carrier_bytes(a[1]).to_vec();
        let after = self.carrier_bytes(a[3]).to_vec();
        self.check_cancel(t)?;
        let model = self.world.network.sockets.model().unwrap().clone();
        model.advance_to(self.world.clock);
        let deadline = self.until(a, 4);
        let mut committed = 0;
        let reply = self.network_attempt(t, &model, deadline, |token| {
            model.attempt_secret(
                key,
                [&before, bytes.as_slice(), &after],
                &mut committed,
                token,
            )
        });
        if reply.error != 0 && committed != 0 {
            self.world.tls_writes.get_mut(&(key & !3)).unwrap().1 = true;
        }
        Ok(vec![0, reply.error.into(), reply.code.into(), 0])
    }

    /// A provider row against [`World`]: an outcome `value, error, code, written`.
    fn host(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        if op == HostOp::SecretSeal {
            return Ok(self.secret_policy(t, op, a));
        }
        self.deny_describe_capability();
        if let Some(reply) = self.provide(t, op, a) {
            return reply;
        }
        match op {
            HostOp::TlsConnect => return self.tls_connect(t, a),
            HostOp::SendSecret => return self.send_secret(t, a),
            HostOp::SecretNarrow => return Ok(self.secret_policy(t, op, a)),
            HostOp::SecretHmac | HostOp::SecretVerifyHmac => return Ok(self.secret_mac(op, a)),
            HostOp::EnvGet | HostOp::EnvLen | HostOp::EnvName | HostOp::EnvNarrow => {
                return Ok(self.env(t, op, a));
            }
            HostOp::SendPair
                if self
                    .world
                    .tls_writes
                    .get(&(a[0] & !3))
                    .is_some_and(|record| record.1) =>
            {
                return Ok(vec![0, 5, 0, 0]);
            }
            _ => {}
        }
        let ok = |value: u64, written: u64| vec![value, 0, 0, written];
        let error = |e: u64| vec![0, e, 0, 0];
        let resolve = |(tag, code)| vec![0, tag, code, 0];
        let stat = |kind: u64, size: u64, time: u64| vec![size, 0, 0, time, kind];
        let stat_error = |(tag, code)| vec![0, tag, code, 0, 0];
        let file = |h: u64| (h - FILE_BASE) as usize;
        // Every operation but a clock read is a cancellation point at entry
        // (native provider `drive`, `cancellation_point`).
        if !matches!(
            op,
            HostOp::WallMs
                | HostOp::MonoNs
                | HostOp::NetNarrow
                | HostOp::NetNarrowTls
                | HostOp::NetNarrowResolve
                | HostOp::NetSplit
        ) {
            self.check_cancel(t)?;
        }
        Ok(match op {
            HostOp::Connect
            | HostOp::Listen
            | HostOp::Accept
            | HostOp::RecvBounded
            | HostOp::SendPair => {
                return self.network_host(t, op, a);
            }
            HostOp::TlsConnect
            | HostOp::SendSecret
            | HostOp::SecretSeal
            | HostOp::SecretNarrow
            | HostOp::SecretHmac
            | HostOp::SecretVerifyHmac
            | HostOp::EnvGet
            | HostOp::EnvLen
            | HostOp::EnvName
            | HostOp::EnvNarrow
            | HostOp::ProvideClock
            | HostOp::ProvideRandom
            | HostOp::ProvideProcess
            | HostOp::ClockAdvance
            | HostOp::ClockSetWall
            | HostOp::ClockIdle
            | HostOp::ProcessTrigger => {
                unreachable!()
            }
            HostOp::Resolve => self.network_resolve(a),
            HostOp::NetNarrowResolve => {
                use hirsel_core_net::{GrantRule, ResolveRule};
                match std::str::from_utf8(self.carrier_bytes(a[1]))
                    .ok()
                    .and_then(ResolveRule::new)
                {
                    Some(rule) => self.narrow_net(t, a[0], GrantRule::Resolve(rule)),
                    None => error(1),
                }
            }
            HostOp::NetNarrowTls => {
                use hirsel_core_net::{GrantRule, TlsRule, TlsTrust};
                let trust = match a[4] {
                    0 => Some(TlsTrust::System),
                    1 => self
                        .carrier_bytes(a[5])
                        .try_into()
                        .ok()
                        .map(TlsTrust::Roots),
                    _ => None,
                };
                let rule = trust.and_then(|trust| {
                    let patterns = std::str::from_utf8(self.carrier_bytes(a[1])).ok()?;
                    TlsRule::new(patterns, a[2] as u16, a[3] as u16, trust)
                });
                match rule {
                    Some(rule) => self.narrow_net(t, a[0], GrantRule::Tls(rule)),
                    None => error(1),
                }
            }
            HostOp::NetSplit => {
                let (read, write) = self.world.network.sockets.split(a[0]);
                vec![read, 0, 0, write]
            }
            HostOp::NetNarrow => {
                let r = hirsel_core_net::Rule {
                    dir: a[1] as u8,
                    family: a[2] as u8,
                    hi: a[3],
                    lo: a[4],
                    bits: a[5] as u8,
                    first: a[6] as u16,
                    last: a[7] as u16,
                };
                if r.valid() {
                    self.narrow_net(t, a[0], hirsel_core_net::GrantRule::Address(r))
                } else {
                    error(1)
                }
            }
            HostOp::StdinRead => {
                let (data, len, cap) = (
                    self.load(a[1], 8),
                    self.load(a[1] + 8, 8),
                    self.load(a[1] + 16, 8),
                );
                let n = (cap.saturating_sub(len) as usize)
                    .min(self.world.stdin.len())
                    .min(64 * 1024);
                if n != 0 {
                    let bytes: Vec<_> = self.world.stdin.drain(..n).collect();
                    self.put(data + len, &bytes);
                    self.store(a[1] + 8, 8, len + n as u64);
                }
                ok(n as u64, 0)
            }
            HostOp::Stopped => {
                // The model has no OS signal source. Cancellation still wakes this wait.
                self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
                self.park(Some(Wait::Stop));
                self.check_cancel(t)?;
                vec![]
            }
            HostOp::SleepUntil => {
                // Ready if the deadline has passed, else park in W until its
                // timer (os.rs `sleep_wait`); a cancellation that takes effect
                // there fails the sleep Cancelled.
                let deadline = a[1];
                if deadline > self.world.clock {
                    self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
                    self.park(Some(Wait::Sleep(deadline)));
                    if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                        self.check_cancel(t)?;
                    }
                }
                ok(0, 0)
            }
            HostOp::WallMs | HostOp::MonoNs => {
                // The host clock is monotonic (native reads CLOCK_MONOTONIC,
                // hirsel-core-rt os::monotonic_ns): a read after a sleep to the
                // maximum deadline stays there instead of wrapping (CONC-7).
                let model = self.world.network.sockets.model().unwrap();
                model.advance_to(self.world.clock);
                self.world.clock = model.clock_read();
                vec![self.world.clock]
            }
            HostOp::Stdout | HostOp::Stderr => {
                let (bytes, ..) = self.carrier(a[1]);
                let n = bytes.len() as u64;
                match op {
                    HostOp::Stdout => self.world.stdout.extend(bytes),
                    _ => self.world.stderr.extend(bytes),
                }
                ok(0, n)
            }
            HostOp::Random => {
                let (_, data, len, cap) = self.carrier(a[1]);
                let spare = cap.saturating_sub(len) as usize;
                let bytes = (0..spare)
                    .map(|i| self.world.random.wrapping_add(i as u8))
                    .collect::<Vec<_>>();
                self.world.random = self.world.random.wrapping_add(spare as u8);
                self.put(data + len, &bytes);
                self.store(a[1] + 8, 8, len + spare as u64);
                ok(0, spare as u64)
            }
            HostOp::Open => {
                let key = match self.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                let w = &mut self.world;
                let mode = a[2] as u8;
                let file = match (w.nodes.get(&key), mode) {
                    (Some(Node::Dir(_)), _) => return Ok(error(DENIED)),
                    (None, 0) => return Ok(error(NOT_FOUND)),
                    (Some(_), 3) => return Ok(error(EXISTS)),
                    (Some(Node::File(file)), _) => Arc::clone(file),
                    (None, _) => {
                        let file = Arc::new(Mutex::new(ModelFile {
                            modified_ns: w.clock.min(i64::MAX as u64),
                            ..ModelFile::default()
                        }));
                        w.nodes.insert(key, Node::File(Arc::clone(&file)));
                        file
                    }
                };
                if mode == 1 {
                    *file.lock().unwrap() = ModelFile {
                        modified_ns: w.clock.min(i64::MAX as u64),
                        ..ModelFile::default()
                    };
                }
                w.files.push(Some(OpenFile {
                    file,
                    at: 0,
                    locked: false,
                    mode,
                }));
                ok(FILE_BASE + w.files.len() as u64 - 1, 0)
            }
            HostOp::OpenDir => {
                let key = match self.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err((OTHER, 20)) => return Ok(error(DENIED)),
                    Err(e) => return Ok(resolve(e)),
                };
                match self.world.nodes.get(&key) {
                    Some(Node::Dir(dir)) => ok(*dir, 0),
                    Some(Node::File(_)) => error(DENIED),
                    None => error(NOT_FOUND),
                }
            }
            HostOp::Entries => {
                let records = self
                    .world
                    .children(a[0])
                    .map(|((_, name), node)| {
                        let kind = u8::from(matches!(node, Node::Dir(_)));
                        let mut record = vec![kind];
                        record.extend_from_slice(&(name.len() as u16).to_le_bytes());
                        record.extend_from_slice(name);
                        record
                    })
                    .collect();
                let h = LISTING_HANDLE | self.world.listings.len() as u64;
                self.world.listings.push(Some(records));
                ok(h, 0)
            }
            HostOp::NextEntries => {
                let (_, data, len, cap) = self.carrier(a[1]);
                let spare = cap.saturating_sub(len);
                if spare < 258 {
                    return Err(Stop::Cause("Bounds"));
                }
                let Some(records) = self.world.listings[(a[0] & !LISTING_HANDLE) as usize].as_mut()
                else {
                    return Ok(error(CLOSED));
                };
                let mut bytes = Vec::new();
                let mut count = 0;
                while records
                    .front()
                    .is_some_and(|r| bytes.len() as u64 + r.len() as u64 <= spare)
                {
                    bytes.extend(records.pop_front().unwrap());
                    count += 1;
                }
                if count != 0 {
                    self.put(data + len, &bytes);
                    self.store(a[1] + 8, 8, len + bytes.len() as u64);
                }
                ok(count, 0)
            }
            HostOp::Stat => {
                let key = match self.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(stat_error(e)),
                };
                match self.world.nodes.get(&key) {
                    Some(Node::Dir(_)) => stat(1, 0, 0),
                    Some(Node::File(f)) => {
                        let f = f.lock().unwrap();
                        stat(0, f.len, f.modified_ns)
                    }
                    None => stat_error((NOT_FOUND, 0)),
                }
            }
            HostOp::StatFile => {
                if let Some(f) = &self.world.files[file(a[0])] {
                    let f = f.file.lock().unwrap();
                    stat(0, f.len, f.modified_ns)
                } else {
                    stat_error((CLOSED, 0))
                }
            }
            HostOp::Read | HostOp::ReadAt => {
                let positioned = op == HostOp::ReadAt;
                let buffer = a[if positioned { 2 } else { 1 }];
                let (_, data, len, cap) = self.carrier(buffer);
                let i = file(a[0]);
                let Some(f) = self.world.files[i].as_ref() else {
                    return Ok(error(CLOSED));
                };
                if positioned && !matches!(f.mode, 0 | 4) {
                    return Ok(error(DENIED));
                }
                let at = if positioned { a[1] } else { f.at };
                let (n, bytes) = {
                    let c = f.file.lock().unwrap();
                    let n = cap.saturating_sub(len).min(c.len.saturating_sub(at)) as usize;
                    (n, c.read(at, n))
                };
                if n > 0 {
                    self.put(data + len, &bytes);
                    self.store(buffer + 8, 8, len + n as u64);
                }
                if !positioned {
                    self.world.files[i].as_mut().unwrap().at += n as u64;
                }
                ok(n as u64, 0)
            }
            HostOp::Write | HostOp::WriteAt => {
                let positioned = op == HostOp::WriteAt;
                let (bytes, ..) = self.carrier(a[if positioned { 2 } else { 1 }]);
                if positioned && a[1].checked_add(bytes.len() as u64).is_none() {
                    return Err(Stop::Cause("Bounds"));
                }
                let i = file(a[0]);
                let Some(f) = self.world.files[i].as_mut() else {
                    return Ok(error(CLOSED));
                };
                if positioned && !matches!(f.mode, 1 | 3 | 4) {
                    return Ok(error(DENIED));
                }
                if bytes.is_empty() {
                    return Ok(ok(0, 0));
                }
                let mut c = f.file.lock().unwrap();
                let at = if positioned {
                    a[1]
                } else if f.mode == 2 {
                    c.len
                } else {
                    f.at
                };
                let end = at + bytes.len() as u64;
                if end > i64::MAX as u64 {
                    return Ok(vec![0, 5, 27, 0]); // Other(EFBIG)
                }
                c.write(at, &bytes);
                c.modified_ns = self.world.clock.min(i64::MAX as u64);
                if !positioned {
                    f.at = end;
                }
                ok(0, bytes.len() as u64)
            }
            HostOp::Close => {
                let i = file(a[0]);
                self.world.files[i] = None;
                ok(0, 0)
            }
            HostOp::TryLock | HostOp::Unlock => {
                let i = file(a[0]);
                let open = self.world.files[i].as_ref().expect("an open file");
                let held = self.world.files.iter().enumerate().any(|(j, f)| {
                    j != i
                        && f.as_ref()
                            .is_some_and(|f| f.locked && Arc::ptr_eq(&f.file, &open.file))
                });
                let take = op == HostOp::TryLock;
                if take && held {
                    return Ok(ok(0, 0));
                }
                self.world.files[i].as_mut().unwrap().locked = take;
                ok(u64::from(take), 0)
            }
            HostOp::Mkdir => {
                let key = match self.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                if self.world.nodes.contains_key(&key) {
                    return Ok(error(EXISTS));
                }
                self.world.new_dir(key);
                ok(0, 0)
            }
            HostOp::Rename => {
                let from = match self.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                let to = match self.world.dir_key(a[0], self.carrier_bytes(a[2])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                // core.md: a regular file `from` takes the name `to` in the one `Dir`,
                // replacing a regular file there; anything else at either name is
                // `Denied` and renames nothing.
                match self.world.nodes.get(&from) {
                    None => return Ok(error(NOT_FOUND)),
                    Some(Node::Dir(_)) => return Ok(error(DENIED)),
                    Some(Node::File(_)) => {}
                }
                if let Some(Node::Dir(_)) = self.world.nodes.get(&to) {
                    return Ok(error(DENIED));
                }
                // The same name: removed and put back, unchanged.
                let node = self.world.nodes.remove(&from).unwrap();
                self.world.nodes.insert(to, node);
                ok(0, 0)
            }
            HostOp::Remove | HostOp::RemoveTree => {
                let key = match self.world.dir_key(a[0], self.carrier_bytes(a[1])) {
                    Ok(key) => key,
                    Err(e) => return Ok(resolve(e)),
                };
                if let Some(Node::Dir(dir)) = self.world.nodes.get(&key) {
                    let dir = *dir;
                    if op == HostOp::RemoveTree {
                        self.world.remove_children(dir);
                    } else if self.world.children(dir).next().is_some() {
                        return Ok(resolve((OTHER, 39))); // ENOTEMPTY
                    }
                }
                match self.world.nodes.remove(&key) {
                    Some(_) => ok(0, 0),
                    None => error(NOT_FOUND),
                }
            }
            HostOp::Mkdtemp => {
                let (prefix, data, len, cap) = self.carrier(a[1]);
                if cap.saturating_sub(len) < 32 {
                    return Err(Stop::Cause("Bounds"));
                }
                if prefix.iter().any(|&b| b == b'/' || b == 0) {
                    return Ok(error(DENIED));
                }
                for _ in 0..128 {
                    let k = self.world.temporaries;
                    self.world.temporaries += 1;
                    let suffix = format!("t{k}").into_bytes();
                    let full = [prefix.as_slice(), &suffix].concat();
                    if full.len() > 255 {
                        return Ok(resolve((OTHER, 36))); // ENAMETOOLONG
                    }
                    let key = (a[0], full);
                    if !self.world.nodes.contains_key(&key) {
                        self.put(data + len, &suffix);
                        self.store(a[1] + 8, 8, len + suffix.len() as u64);
                        return Ok(ok(self.world.new_dir(key), 0));
                    }
                }
                resolve((OTHER, 11)) // EAGAIN
            }
            HostOp::Sync | HostOp::SyncDir => ok(0, 0),
        })
    }
    fn kind(&self, p: u64, kind: Counted) {
        let made = self.mem[split(p).0].counted;
        assert_eq!(
            made,
            Some(kind),
            "a counted operation names another kind than its alloc"
        );
    }
    fn block(&mut self, size: u64, limit: Option<Limit>, charge: u64, res: Option<Res>) -> u64 {
        // An admitted block the host cannot provide is a run stop, as for
        // `hirsel_core_alloc_admitted`, never `Capacity`: a refusal is the limit's
        // (give the run `limit N`).
        assert!(
            size <= HOST_BYTES,
            "host out of memory: a block of {size} bytes was admitted (a run stop, not Capacity)"
        );
        let id = self.mem.insert(Blk {
            bytes: vec![0; size as usize],
            live: true,
            limit,
            charge,
            lent_size: None,
            res,
            counted: None,
            class: None,
            fixed: false,
            marks: BTreeMap::new(),
        });
        ptr(id, 0)
    }
    fn get(&self, p: u64, n: usize) -> &[u8] {
        let (b, o) = split(p);
        assert!(self.mem[b].live, "use of a freed block");
        &self.mem[b].bytes[o..o + n]
    }
    fn load(&self, p: u64, n: usize) -> u64 {
        let mut x = [0u8; 8];
        x[..n].copy_from_slice(self.get(p, n));
        u64::from_le_bytes(x)
    }
    fn put(&mut self, p: u64, bytes: &[u8]) {
        let (b, o) = split(p);
        assert!(self.mem[b].live, "use of a freed block");
        assert!(!self.mem[b].fixed, "a store into static data");
        if let Some(&(_, at, n)) = self
            .frozen
            .iter()
            .find(|&&(fb, fo, fn_)| fb == b && o < fo + fn_ && fo < o + bytes.len())
        {
            panic!(
                "a write of +{o}..+{} into a lent view or take place +{at}..+{} while its call runs",
                o + bytes.len(),
                at + n
            );
        }
        self.mem[b].bytes[o..o + bytes.len()].copy_from_slice(bytes);
    }
    fn store(&mut self, p: u64, n: usize, v: u64) {
        self.put(p, &v.to_le_bytes()[..n]);
    }
    /// A producer's read of `n` bytes at `p` (`read`, or `copy` from `p`) must not
    /// touch a slot marked empty. Marked slots do not overlap, so walking back from
    /// the read's end stops at the first slot that ends at or before its start.
    fn read_marked(&self, p: u64, n: usize) {
        let (b, o) = split(p);
        for (&at, &(live, size)) in self.mem[b].marks.range(..o + n).rev() {
            if at + size <= o {
                break;
            }
            assert!(
                live,
                "read of the empty slot at +{at}: a slot kernel read a slot it took"
            );
        }
    }
    /// `mark`: a put (`live`) needs a slot not marked live, a take (`empty`) one not
    /// marked empty. An unmarked slot may be either (storage a `List` handed over).
    /// A zero-size slot has no storage to tell it from its neighbours, so it is not
    /// shadowed.
    fn mark(&mut self, p: u64, live: bool, size: usize) {
        if size == 0 {
            return;
        }
        let (b, o) = split(p);
        assert!(self.mem[b].live, "mark of a freed block");
        let was = self.mem[b].marks.insert(o, (live, size)).map(|m| m.0);
        match (was, live) {
            (Some(true), true) => panic!("put over the live slot at +{o}"),
            (Some(false), false) => panic!("take of the empty slot at +{o}"),
            _ => {}
        }
    }
    /// The slot at `p` leaves the shadow in the state the kernel claims; a slot never
    /// marked may be either, so it passes.
    fn unmark(&mut self, p: u64, live: bool, size: usize) {
        if size == 0 {
            return;
        }
        let (b, o) = split(p);
        assert!(self.mem[b].live, "unmark of a freed block");
        match (self.mem[b].marks.remove(&o).map(|m| m.0), live) {
            (Some(false), true) => {
                panic!("unmark live of the empty slot at +{o}: a hole handed back as a value")
            }
            (Some(true), false) => {
                panic!("unmark empty of the live slot at +{o}: a value left behind")
            }
            _ => {}
        }
    }
    /// A `drop` of a value, or cleanup of a `List` element, at a marked slot is a
    /// take: the slot must be marked live, and is then marked empty. Cleanup that
    /// reaches a slot a kernel emptied is the bug this catches.
    fn drop_marked(&mut self, p: u64, size: usize) {
        if size == 0 {
            return;
        }
        let (b, o) = split(p);
        if let Some(m) = self.mem[b].marks.get_mut(&o) {
            assert!(
                m.0,
                "drop of the empty slot at +{o}: cleanup reached a slot a kernel took"
            );
            m.0 = false;
        }
    }
    /// The block an owning edge cleans: its native prefix was fixed by the class its
    /// `alloc` named, and cleanup through the edge reads the edge's class, so the two
    /// must agree (crate docs, "Cleanup classes").
    fn edge(&self, p: u64, edge: &FieldTy, what: &str) {
        if let Some(made) = self.mem[split(p).0].class {
            let cleaned = cleanup_class(&self.m.types, &self.recipes, edge);
            assert!(
                made == cleaned,
                "{what} was allocated as {made:?} storage, but its edge ({edge:?}) cleans \
                 {cleaned:?} storage: the block's prefix does not fit its cleanup"
            );
        }
    }
    /// Cleanup glue's free: native glue refunds the charge it computes from the
    /// recipe, not the one the block was admitted with, so a block allocated with
    /// another charge (or, given `size`, another native size) than the recipe's is a
    /// fault here, where the evaluator would otherwise refund what was admitted and
    /// hide the difference.
    fn free_as(&mut self, p: u64, charge: Option<u64>, size: Option<u64>, what: &str) {
        let blk = &self.mem[split(p).0];
        assert!(blk.live, "free of a non-block");
        let charge = charge.expect("a cleanup charge in range");
        assert_eq!(
            blk.charge, charge,
            "cleanup of {what} refunds {charge}, but its block was charged {}: \
             the allocation and the recipe disagree",
            blk.charge
        );
        if let Some(size) = size {
            assert_eq!(
                blk.lent_size.unwrap_or(blk.bytes.len() as u64),
                size,
                "cleanup of {what} frees a block of the recipe's {size} bytes, but it was \
                 allocated with {}",
                blk.bytes.len()
            );
        }
        self.free(p);
    }
    /// Free a block and refund its charge, less a settled grant and a used cause
    /// cell, which left with the finalizer's hand-over and with the chain.
    fn free(&mut self, p: u64) {
        let (b, o) = split(p);
        assert!(o == 0 && self.mem[b].live, "free of a non-block");
        assert!(
            !self.frozen.iter().any(|f| f.0 == b),
            "free of a block holding a lent view or take place while its call runs"
        );
        assert!(!self.mem[b].fixed, "a free of static data");
        if let Some((at, _)) = self.mem[b].marks.iter().find(|(_, m)| m.0) {
            panic!("free of a block whose slot at +{at} is live: a slot kernel left a value in it");
        }
        if self.mem[b].lent_size.take().is_some() {
            let slot = &mut self.mem[b];
            let charge = std::mem::take(&mut slot.charge);
            let payer = slot.limit.take().expect("a lent frame allocation's payer");
            slot.class = None;
            slot.marks.clear();
            self.refund(payer, charge);
            return;
        }
        let blk = self.mem.remove(b);
        let mut n = blk.charge;
        if let Some(r) = &blk.res {
            if !r.settled {
                let payer = blk.limit.as_ref().expect("a resource payer");
                let grant = Limit::new(r.grant, Some(payer.clone()), self.sched.current);
                self.end_limit(grant, None);
            }
            n -= r.grant;
            n -= if r.cell_used { charge::CAUSE_CELL } else { 0 };
        }
        if let Some(l) = blk.limit {
            self.refund(l, n);
        }
    }

    // ---- limits
    fn root(&self) -> &Limit {
        self.root.as_ref().unwrap()
    }

    /// Admit `n` against `l`, or Err(the room left). A successful admission reports
    /// no room: `available` is 0 then.
    fn admit(&mut self, l: &Limit, n: u64) -> Result<(), u64> {
        let mut state = l.0.state.lock().unwrap();
        let room = l.0.cap - state.used;
        self.admissions += 1;
        let refused = self
            .injection
            .is_some_and(|(nth, later)| self.admissions == nth || (later && self.admissions > nth));
        if n <= room && !refused {
            state.used += n;
            drop(state);
            self.peak_reserved = self.peak_reserved.max(self.root().used());
            Ok(())
        } else {
            drop(state);
            self.event(CAPACITY, n, room);
            Err(room)
        }
    }
    /// An `alloc` or `grow` admission of `n`, refused outright when its exact
    /// charge overflowed: Err(the room left).
    fn admit_charge(&mut self, l: &Limit, n: u64, overflowed: bool) -> Result<(), u64> {
        if overflowed {
            self.admissions += 1;
            let room = l.0.cap - l.used();
            self.event(CAPACITY, u64::MAX, room);
            return Err(room);
        }
        self.admit(l, n)
    }
    /// MEM-20: a refund to a limit the running task owns applies at once; one to
    /// another task's limit waits for that task's next event, so its admissions see
    /// exactly the refunds the event order puts before their task's latest event.
    fn refund(&mut self, l: Limit, n: u64) {
        let l = l.find();
        let owner = l.0.owner;
        if owner == self.sched.current || owner >= self.sched.tasks.len() {
            l.subtract(n);
        } else {
            if n != 0 {
                self.event(REFUND, self.sched.tasks[owner].id, n);
            }
            self.sched.tasks[owner].refunds.push((l, n));
        }
    }
    /// Apply the running task's pending refunds, each to the live limit its record
    /// now leads to (native `drain_refunds`).
    fn drain_refunds(&mut self) {
        let k = self.sched.current;
        if k < self.sched.tasks.len() && !self.sched.tasks[k].refunds.is_empty() {
            for (l, n) in std::mem::take(&mut self.sched.tasks[k].refunds) {
                self.refund(l, n);
            }
        }
    }
    /// MEM-20: the old owner folds before handing the limit to its parent.
    /// The recipient sees the unused reservation only upon its next event.
    fn end_limit(&mut self, l: Limit, creator: Option<usize>) {
        let current = self.sched.current;
        let (parent, unused) = {
            let state = l.0.state.lock().unwrap();
            (
                state.parent.as_ref().expect("not the root").find(),
                l.0.cap - state.used,
            )
        };
        let owner = parent.0.owner;
        if owner != current || creator.is_some_and(|creator| creator != current) {
            self.drain_refunds();
            let unused = l.0.cap - l.used();
            if owner < self.sched.tasks.len() {
                self.event(REFUND, self.sched.tasks[owner].id, unused);
            }
            if owner == current || owner >= self.sched.tasks.len() {
                parent.subtract(unused);
            } else {
                self.sched.tasks[owner]
                    .refunds
                    .push((parent.clone(), unused));
            }
        } else {
            parent.subtract(unused);
        }
        let mut state = l.0.state.lock().unwrap();
        state.parent = Some(parent);
        state.ended = true;
    }

    // ---- types
    fn shape(&self, t: TypeRef) -> &Shape {
        &self.m.types[t.index()]
    }
    /// The case of the variant of type `ty` at `base`.
    fn case_at(&self, base: u64, ty: TypeRef) -> u32 {
        let tag = self.fixed(ty).tag.expect("a variant");
        hirsel_core_layout::case_of(&tag, self.load(base + tag.offset, tag.width as usize))
    }
    fn fixed(&self, t: TypeRef) -> &hirsel_core_layout::Fixed {
        match &self.recipes[t.index()].layout {
            Layout::Fixed(f) => f,
            Layout::Compact(_) => panic!("compact values are not modelled"),
        }
    }
    fn is_resource(&self, t: TypeRef) -> bool {
        self.shape(t).repr == Repr::Resource
    }
    /// A value of `t` is held as a pointer to its block: a resource, or a stable
    /// value, which never moves once published (MEM-5).
    fn by_block(&self, t: TypeRef) -> bool {
        matches!(self.shape(t).repr, Repr::Resource | Repr::Region)
    }
    /// Bytes a holder spends on a value of `t`: a resource or region value is a
    /// pointer to its block.
    fn held(&self, t: TypeRef) -> u64 {
        if self.by_block(t) {
            8
        } else {
            self.fixed(t).size
        }
    }
    fn logical(&self, t: TypeRef) -> u64 {
        if self.is_resource(t) {
            8
        } else if self.by_block(t) {
            16
        } else {
            self.recipes[t.index()].logical_size.expect("fixed")
        }
    }
    fn at_of(&self, t: TypeRef) -> At {
        if self.by_block(t) {
            At::Ptr(t)
        } else {
            At::Decl(t)
        }
    }
    /// What a place of field type `f` holds, for the steps that follow it.
    fn at_field(&self, f: &FieldTy) -> At {
        match f {
            FieldTy::Inline(x) => self.at_of(*x),
            FieldTy::Box(x) => At::Ptr(*x),
            FieldTy::Rc(x) | FieldTy::Arc(x) => At::Counted(*x),
            FieldTy::Array(e, n) => At::Array(
                self.arrays
                    .iter()
                    .position(|(x, k)| x == &**e && k == n)
                    .expect("collected"),
            ),
            FieldTy::Seq(e) => At::Seq(field_size(&self.m.types, &self.recipes, e).2),
            _ => At::Other,
        }
    }
    /// FORM #113: an own `List` or `String` carrier reached through a typed word
    /// holds `len <= capacity <= charge::seq_len_bound(size)` for its element's
    /// logical size (SEM-4, MEM-24), which LLVM is told.
    /// Checked after every typed read and every typed len store; a capacity store
    /// checks its bound alone, since a carrier being built stores capacity first
    /// and its len word is not yet written.
    fn check_carrier(&self, carrier: u64, size: u64, stored: Option<SeqWord>) {
        let Some(bound) = hirsel_core_layout::charge::seq_len_bound(size) else {
            return;
        };
        let (len, cap) = (self.load(carrier + 8, 8), self.load(carrier + 16, 8));
        match stored {
            Some(SeqWord::Data) => {}
            Some(SeqWord::Cap) => assert!(
                cap <= bound,
                "FORM #113: a size-{size} carrier's capacity {cap} is above its bound {bound}"
            ),
            Some(SeqWord::Len) | None => assert!(
                len <= cap && cap <= bound,
                "FORM #113: a size-{size} carrier holds len {len}, capacity {cap}, bound {bound}"
            ),
        }
    }
    fn stride(&self, e: &FieldTy) -> u64 {
        field_size(&self.m.types, &self.recipes, e).0
    }
    fn place_size(&self, p: PlaceTy) -> u64 {
        match p {
            PlaceTy::Value(t) => bytes_of(t) as u64,
            PlaceTy::Declared(t) => self.held(TypeRef(t.0)),
        }
    }

    // ---- cleanup (FAIL-8, FAIL-12): each returns whether a finalizer failed
    fn drop_value(&mut self, t: &mut Task, at: u64, ty: TypeRef) -> bool {
        if self.recipes[ty.index()].drop.is_none() {
            return false;
        }
        if self.is_resource(ty) {
            let blk = self.load(at, 8);
            return self.drop_resource(t, blk, ty);
        }
        // An own row's core finalizer (crate docs, "Slot kernels") gets the value's
        // address first, with no grant and no cause cell. wf refuses one that may fail,
        // so it only moves slots; the fields' cleanup does the dropping.
        if let Some(f) = self.shape(ty).finalizer {
            let _ = self.call(t, FuncId(f.0), &[at]);
        }
        self.drop_fields(t, at, ty)
    }
    fn drop_fields(&mut self, t: &mut Task, base: u64, ty: TypeRef) -> bool {
        let row = match self.fixed(ty).tag {
            Some(_) => self.case_at(base, ty) as usize,
            None => 0,
        };
        let fields = self.recipes[ty.index()].drop.as_ref().unwrap().fields[row].clone();
        let mut failed = false;
        for f in fields {
            failed |= self.drop_field(t, base + f.offset, &f.ty);
        }
        failed
    }
    fn drop_field(&mut self, t: &mut Task, at: u64, ty: &FieldTy) -> bool {
        match ty {
            FieldTy::Scalar(_)
            | FieldTy::Fn
            | FieldTy::RegionRef(_)
            | FieldTy::Locator(_)
            | FieldTy::Borrow
            | FieldTy::View(_)
            | FieldTy::Handle(Handle::Capability) => false,
            FieldTy::Inline(x) => self.drop_value(t, at, *x),
            // SEM-9: an inline array is cleaned from index 0 up.
            FieldTy::Array(e, n) => {
                let stride = self.stride(e);
                let mut failed = false;
                for i in 0..*n {
                    failed |= self.drop_field(t, at + i * stride, e);
                }
                failed
            }
            FieldTy::Handle(Handle::Flow) => panic!("flow execution uses the native reference"),
            FieldTy::Handle(Handle::Socket) => {
                let h = self.load(at, 8);
                if h & 3 == 2 {
                    self.world.tls_writes.remove(&(h & !3));
                }
                self.world.network.sockets.release(h);
                false
            }
            // The provider closes the file and ignores the outcome (core.md,
            // MEM-24); the trace records the close by its handle.
            FieldTy::Handle(Handle::HostFile) => {
                let h = self.load(at, 8);
                if h & LISTING_HANDLE != 0 {
                    self.world.listings[(h & !LISTING_HANDLE) as usize] = None;
                    return false;
                }
                self.trace.push(h);
                if (CELL_BASE..LISTING_HANDLE).contains(&h) {
                    // A dropped ClockHand freezes its clock; a dropped
                    // StopTrigger never stops its process.
                    return false;
                }
                if let Some(file) = h
                    .checked_sub(FILE_BASE)
                    .and_then(|i| self.world.files.get_mut(i as usize))
                {
                    *file = None;
                }
                false
            }
            FieldTy::Box(x) => {
                let p = self.load(at, 8);
                self.edge(p, ty, "a box");
                let failed = self.drop_value(t, p, *x);
                let size = (!self.is_resource(*x)).then(|| self.held(*x));
                self.free_as(p, charge::boxed(self.logical(*x)), size, "a box");
                failed
            }
            FieldTy::Handle(Handle::Task) => {
                // CONC-8: cancel the child and wait for its end with cancellation
                // shielded, then clean its result or chain (native `abandon`).
                let i = self.child(self.load(at, 8));
                let id = self.sched.tasks[i + 1].id;
                t.shield += 1;
                self.cancel(i);
                while self.sched.tasks[i + 1].state != State::Ended {
                    self.park_task(t, Condition::Join(i));
                }
                self.event(JOIN, id, 2);
                let (status, result, r) = {
                    let c = &self.children[i];
                    (c.status, c.result, c.r)
                };
                // CONC-8: an abandoned handle cleans the result or the chain.
                let failed = status == 0 && self.drop_value(t, result, r);
                for c in std::mem::take(&mut self.children[i].chain) {
                    if c.message[2] != 0 {
                        self.free(c.message[0]);
                    }
                }
                for (l, n) in self.settle_child(i) {
                    self.refund(l, n);
                }
                t.shield -= 1;
                failed
            }
            FieldTy::FrozenBytes => {
                let capacity = self.load(at + 16, 8);
                if capacity != 0 {
                    let meta = self.load(at + 8, 8);
                    let block = if meta & 1 == 0 {
                        self.load(at, 8)
                    } else {
                        meta & !1
                    };
                    let size = (meta & 1 != 0).then_some(0);
                    self.free_as(block, charge::seq(capacity, 1), size, "frozen bytes");
                }
                false
            }
            FieldTy::Seq(element) => {
                let block = self.load(at, 8);
                let length = self.load(at + 8, 8);
                let capacity = self.load(at + 16, 8);
                let stride = self.stride(element);
                if stride == 0 {
                    // SEM-4: no element storage, nothing to clean (capacity 2^64 - 1).
                    return false;
                }
                if capacity != 0 {
                    self.edge(block, ty, "list storage");
                }
                let mut failed = false;
                for i in 0..length {
                    self.drop_marked(block + i * stride, stride as usize);
                    failed |= self.drop_field(t, block + i * stride, element);
                }
                // Capacity 0 has no storage: empty, or static data the carrier does
                // not own (core.md "Literals").
                if capacity != 0 {
                    let element = field_size(&self.m.types, &self.recipes, element).2;
                    self.free_as(block, charge::seq(capacity, element), None, "list storage");
                }
                failed
            }
            // SEM-5, SEM-9: live entries in insertion order, each key before its
            // value (`map_entry`); removed entries (state 0) were cleaned by
            // `map.remove`. Storage exists exactly when the capacity is not 0.
            FieldTy::Map(k, v) => {
                let block = self.load(at, 8);
                let slots = self.load(at + 8, 8);
                let capacity = self.load(at + 16, 8);
                let e = map_entry(&self.m.types, &self.recipes, k, v);
                if capacity != 0 {
                    self.edge(block, ty, "map storage");
                }
                let mut failed = false;
                for i in 0..slots {
                    let entry = block + i * e.stride;
                    if self.load(entry + e.state, 8) == 1 {
                        failed |= self.drop_field(t, entry + e.key, k);
                        failed |= self.drop_field(t, entry + e.value, v);
                    }
                }
                if capacity != 0 {
                    let key = field_size(&self.m.types, &self.recipes, k).2;
                    let value = field_size(&self.m.types, &self.recipes, v).2;
                    self.free_as(
                        block,
                        charge::map(capacity, key, value),
                        None,
                        "map storage",
                    );
                }
                failed
            }
            // The environment names its captures' row; they are cleaned as that
            // row from offset 8, then the block is freed (crate docs, "Callable
            // environments").
            FieldTy::OwnFn => {
                let environment = self.load(at + 8, 8);
                if environment == 0 {
                    return false;
                }
                self.edge(environment, ty, "an environment");
                let row = TypeRef(
                    u32::try_from(self.load(environment, 8) - ENV_BASE).expect("an environment"),
                );
                let failed = self.drop_value(t, environment + 8, row);
                let size = 8 + self.fixed(row).size.next_multiple_of(8);
                self.free_as(
                    environment,
                    charge::closure_env(self.logical(row)),
                    Some(size),
                    "an environment",
                );
                failed
            }
            FieldTy::Rc(x) | FieldTy::Arc(x) => {
                let node = self.load(at, 8);
                self.release(node, *x);
                false
            }
            FieldTy::Handle(Handle::Causes) => {
                let head = self.load(at, 8);
                let cells = self.causes.remove(&head).expect("Causes dropped twice");
                let mut cell = head;
                while cell != 0 {
                    let next = self.load(cell + CELL_NEXT, 8);
                    let capacity = self.load(cell + CAUSE_CAPACITY, 8);
                    if capacity != 0 && capacity < CAUSE_KIND_START {
                        let data = self.load(cell, 8);
                        self.free(data);
                    }
                    self.free(cell);
                    cell = next;
                }
                for (l, n) in cells {
                    self.refund(l, n);
                }
                false
            }
            FieldTy::Handle(Handle::Sender) => {
                let i = ((self.load(at, 8) - CHAN_BASE) / 2) as usize;
                self.chans[i].senders -= 1;
                let (id, senders) = (self.chans[i].id, self.chans[i].senders);
                self.event(SENDERS, id, senders);
                if senders == 0 {
                    self.event(CLOSE, id, 0);
                    self.wake(Condition::Receive(i));
                }
                self.chan_freed(i);
                false
            }
            FieldTy::Handle(Handle::Latest) => {
                let i = (self.load(at, 8) - LATEST_BASE) as usize;
                self.latests[i].handles -= 1;
                let (id, handles) = (self.latests[i].id, self.latests[i].handles);
                self.event(SENDERS, id, handles);
                if self.latests[i].handles != 0 {
                    return false;
                }
                let latest = &mut self.latests[i];
                let (item, root, limit) = (latest.item, latest.root, latest.limit.take().unwrap());
                self.release_latest_holder(t, root, item);
                self.refund(limit, charge::LATEST_ROOT);
                false
            }
            FieldTy::Handle(Handle::Receiver) => {
                let i = ((self.load(at, 8) - CHAN_BASE) / 2) as usize;
                self.chans[i].receiver = false;
                t.shield += 1;
                self.event(CLOSE, self.chans[i].id, 1);
                self.wake(Condition::Send(i));
                let mut failed = false;
                // CONC-9: last published first; a failure appends and the rest go on.
                while let Some(bytes) = self.chans[i].queue.pop_back() {
                    let tmp = self.block(bytes.len() as u64, None, 0, None);
                    self.put(tmp, &bytes);
                    failed |= self.drop_value(t, tmp, self.chans[i].item);
                }
                self.chan_freed(i);
                t.shield -= 1;
                failed
            }
            other => panic!("cleanup of {other:?} is not modelled"),
        }
    }
    fn drop_resource(&mut self, t: &mut Task, blk: u64, ty: TypeRef) -> bool {
        let (b, _) = split(blk);
        // Report::trace names a declared u64 field, never carrier/pointer bytes.
        if let Shaped::Record(fields) = &self.shape(ty).body
            && let Some(i) = fields
                .iter()
                .position(|f| f.ty == FieldTy::Scalar(Scalar::U64))
        {
            let offset = self.fixed(ty).offsets[0][i];
            self.trace.push(self.load(blk + offset, 8));
        }
        let mut failed = false;
        if let Some(f) = self.shape(ty).finalizer {
            let resource = self.mem[b].res.as_ref().expect("a resource block");
            let (grant, creator) = (resource.grant, resource.creator);
            let payer = self.mem[b].limit.clone().expect("charged");
            let g = Limit::new(grant, Some(payer.clone()), self.sched.current);
            t.limits.push(g);
            t.obligations.push(Some(Cell {
                limit: payer,
                amount: charge::CAUSE_CELL,
                resource: true,
            }));
            failed = self.call(t, FuncId(f.0), &[blk]).is_err();
            // FAIL-10: a used resource cell left with a chain; a cell an isolate
            // installed in its place (unused) is refunded now.
            let used = match t.obligations.pop().expect("the finalizer's obligation") {
                Some(c) if c.resource => false,
                Some(c) => {
                    self.refund(c.limit, c.amount);
                    true
                }
                None => true,
            };
            self.end_limit(t.limits.pop().unwrap(), Some(creator));
            let res = self.mem[b].res.as_mut().unwrap();
            res.settled = true;
            res.cell_used |= used;
        }
        failed |= self.drop_fields(t, blk, ty);
        let storage = self.recipes[ty.index()]
            .logical_size
            .expect("resource storage");
        let charge = charge::resource(storage, self.shape(ty).grant);
        self.free_as(blk, charge, None, "a resource");
        failed
    }
    /// Refund a joined or abandoned task's record, result slot and unused primary
    /// cell; the reserved `n` came back when the child's limit ended. The spawn's
    /// charge holds the child's first primary cell, which may have left with a
    /// chain: a used cell is among the cells returned to go with its `Causes`, or
    /// went with one the child caught.
    fn settle_child(&mut self, i: usize) -> Vec<(Limit, u64)> {
        let c = &mut self.children[i];
        assert!(!c.joined, "task handle used twice");
        c.joined = true;
        let (l, n) = (c.limit.take().unwrap(), c.charge - c.n - charge::CAUSE_CELL);
        let cells = std::mem::take(&mut c.cells);
        let primary = c.primary.take();
        self.refund(l, n);
        if let Some(p) = primary {
            self.refund(p.limit, p.amount);
        }
        cells
    }
    /// A cause's `Site` word (SITES.md), as a linked native unit's: the anchor
    /// item's first site word plus the anchor's start, 0 for none.
    fn site_word(&self, site: Option<Anchor>) -> u64 {
        site.and_then(|a| {
            let first = *self.sites.get(a.item.index())?;
            (first != 0).then(|| first + u64::from(a.start))
        })
        .unwrap_or(0)
    }
    /// A chain as the run time delivers it, a `Causes` owning `cells`: one cell per
    /// cause, each a `Cause` in core's layout then the next cell (crate docs,
    /// "Causes"). The head cell.
    fn deliver(&mut self, chain: Vec<Cause>, cells: Vec<(Limit, u64)>) -> u64 {
        let mut head = 0;
        for c in chain.iter().rev() {
            let cell = self.block(CELL_NEXT + 8, None, 0, None);
            let tag = CAUSE_KINDS.iter().position(|k| *k == c.kind).unwrap();
            if c.kind == "Fail" {
                for (j, w) in c.message.into_iter().enumerate() {
                    self.store(cell + 8 * j as u64, 8, w);
                }
            } else {
                if c.kind == "Capacity" {
                    // `Capacity(c)`: requested at 0, available at 8.
                    self.store(cell, 8, c.message[0]);
                    self.store(cell + 8, 8, c.message[1]);
                }
                self.store(cell + CAUSE_CAPACITY, 8, CAUSE_KIND_START + tag as u64);
            }
            let site = self.site_word(c.site);
            self.store(cell + CAUSE_SITE, 8, site);
            self.store(cell + CELL_NEXT, 8, head);
            head = cell;
        }
        self.causes.insert(head, cells);
        head
    }
    /// FAIL-10: record a cause in the innermost obligation's unused cell, which
    /// then leaves with the chain.
    fn record(&mut self, t: &mut Task, cause: Cause) {
        let cell = t
            .obligations
            .last_mut()
            .expect("a failure outside every obligation")
            .take()
            .expect("a failure whose obligation's cell is already used");
        t.cells.push((cell.limit, cell.amount));
        t.chain.push(cause);
    }
    fn release_latest_holder(&mut self, t: &mut Task, node: u64, item: TypeRef) {
        let place = self.block(8, None, 0, None);
        self.put(place, &node.to_le_bytes());
        assert!(
            !self.drop_value(t, place, item),
            "a shareable root's release failed"
        );
    }

    fn share_counted(&mut self, node: u64, kind: Counted) {
        self.kind(node, kind);
        let count = self.load(node, 8) + 1;
        self.store(node, 8, count);
        if kind == Counted::Arc {
            self.event(ARC_SHARE, 0, count);
        }
    }

    /// MEM-16: drop one holder; at zero clean the payload and free the node. The
    /// payload holds no resource, so nothing here fails or runs user code.
    fn release(&mut self, node: u64, ty: TypeRef) {
        self.edge(node, &FieldTy::Rc(ty), "a counted node");
        let count = self.load(node, 8) - 1;
        self.store(node, 8, count);
        if self.mem[split(node).0].counted == Some(Counted::Arc) {
            self.event(ARC_RELEASE, 0, u64::from(count == 0));
        }
        if count == 0 {
            let mut scratch = Task {
                step_bounds: Vec::new(),
                slice_steps: 0,
                limits: Vec::new(),
                chain: Vec::new(),
                cells: Vec::new(),
                obligations: Vec::new(),
                reserved: Vec::new(),
                at: Vec::new(),
                k: 0,
                shield: 0,
            };
            assert!(
                !self.drop_value(&mut scratch, node + 8, ty),
                "a counted payload failed"
            );
            let charge = charge::counted(self.recipes[ty.index()].logical_size.expect("fixed"));
            self.free_as(node, charge, None, "a counted node");
        }
    }
    /// Take channel `i`'s front item into `dest`, or return false when its queue is
    /// empty. The slot it frees wakes this channel's senders in park order.
    fn take(&mut self, i: usize, dest: u64) -> bool {
        let Some(bytes) = self.chans[i].queue.pop_front() else {
            return false;
        };
        self.put(dest, &bytes);
        self.event(RECEIVE, self.chan_id(i), 0);
        self.wake(Condition::Send(i));
        true
    }
    /// Put an item on channel `i` unless it is closed (2) or full (1): native
    /// `publish`, which wakes a receiver parked on this channel.
    fn publish(&mut self, i: usize, value: u64, item: TypeId) -> u64 {
        if !self.chans[i].receiver {
            return 2;
        }
        if self.chans[i].queue.len() as u64 == self.chans[i].cap {
            return 1;
        }
        let n = self.held(TypeRef(item.0)) as usize;
        let bytes = self.get(value, n).to_vec();
        self.chans[i].queue.push_back(bytes);
        self.event(PUBLISH, self.chan_id(i), 0);
        self.wake(Condition::Receive(i));
        0
    }
    fn chan_freed(&mut self, i: usize) {
        let c = &mut self.chans[i];
        if c.senders == 0 && !c.receiver {
            let (l, n) = (c.limit.take().unwrap(), c.charge);
            self.refund(l, n);
        }
    }

    fn step_bound(&mut self, left: u64) -> StepBound {
        let id = self.next_step_bound;
        self.next_step_bound += 1;
        StepBound { id, left }
    }

    fn step_leave(t: &mut Task) -> StepBound {
        let bound = t.step_bounds.pop().expect("an open step scope");
        if let Some(parent) = t.step_bounds.last_mut() {
            parent.left += bound.left;
        }
        bound
    }

    // ---- engine operations (CONC-3..6)
    fn rt(&mut self, t: &mut Task, op: RtOp, a: &[u64]) -> X<Vec<u64>> {
        let cur = t.limits.last().unwrap();
        Ok(match op {
            RtOp::StepEnter => {
                let n = a[0];
                if t.step_bounds.last().is_some_and(|parent| parent.left < n) {
                    vec![0]
                } else {
                    if let Some(parent) = t.step_bounds.last_mut() {
                        parent.left -= n;
                    }
                    t.step_bounds.push(self.step_bound(n));
                    vec![1]
                }
            }
            RtOp::StepLeave => {
                Self::step_leave(t);
                vec![]
            }
            RtOp::StepCatch => {
                let bound = Self::step_leave(t);
                let caught = !t.chain.is_empty()
                    && t.chain
                        .iter()
                        .all(|c| c.kind == "OutOfTime" && c.message[0] == bound.id);
                if caught {
                    t.chain.clear();
                    let mut cells = std::mem::take(&mut t.cells).into_iter();
                    if t.obligations.last().is_some_and(Option::is_none) {
                        let (limit, amount) = cells.next().expect("a cause's prepaid cell");
                        *t.obligations.last_mut().unwrap() = Some(Cell {
                            limit,
                            amount,
                            resource: t.obligations.len() > 1,
                        });
                    }
                    for (limit, amount) in cells {
                        self.refund(limit, amount);
                    }
                }
                vec![u64::from(caught)]
            }
            RtOp::Spawn { x, r } => {
                self.deny_describe_capability();
                let x = TypeRef(x.0);
                let r = TypeRef(r.0);
                let n = a[0];
                let charge = charge::task(self.logical(r), n).unwrap_or(u64::MAX);
                if let Err(room) = self.admit(cur, charge) {
                    return Ok(vec![0, room]); // x stays the caller's
                }
                // The child's limit serves the child, the next scheduler record.
                let lim = Limit::new(n, Some(cur.clone()), self.sched.tasks.len());
                let xb = self.block(self.held(x), None, 0, None);
                let bytes = self.get(a[1], self.held(x) as usize).to_vec();
                self.put(xb, &bytes);
                let rb = self.block(self.held(r), None, 0, None);
                // The child's primary cell is part of the spawn's charge to `cur`.
                let inherited = t
                    .step_bounds
                    .last()
                    .map(|bound| self.step_bound(bound.left));
                let mut child = Task {
                    step_bounds: inherited.into_iter().collect(),
                    slice_steps: 0,
                    limits: vec![lim],
                    chain: Vec::new(),
                    cells: Vec::new(),
                    obligations: vec![Some(Cell {
                        limit: cur.clone(),
                        amount: charge::CAUSE_CELL,
                        resource: false,
                    })],
                    reserved: Vec::new(),
                    at: Vec::new(),
                    k: 0,
                    shield: 0,
                };
                let f = FuncId((a[2] - FN_BASE) as u32);
                let xc = self.m.types[x.index()].carried;
                let rc = self.m.types[r.index()].carried;
                let mut args = Vec::with_capacity(3);
                if matches!(rc, Carried::Address) {
                    args.push(rb)
                }
                args.push(0); // A plain callable's environment.
                match xc {
                    Carried::Address => args.push(xb),
                    Carried::Value(t) => args.push(self.load(xb, bytes_of(t))),
                    Carried::Nothing => {}
                }
                let i = self.children.len();
                self.children.push(Child {
                    status: 0,
                    result: rb,
                    r,
                    charge,
                    n,
                    limit: Some(cur.clone()),
                    joined: false,
                    cells: Vec::new(),
                    chain: Vec::new(),
                    primary: None,
                });
                // CONC-3: the child is readied at the tail and the parent goes on.
                child.k = i + 1;
                let id = self.next_id();
                self.sched.tasks.push(Rec {
                    id,
                    state: State::Ready,
                    cancel: false,
                    polls: POLLS_PER_TURN,
                    cancellable: false,
                    cancelled: false,
                    start: Some((child, f, args)),
                    refunds: Vec::new(),
                });
                self.links.push(None);
                self.sched.ready.push_back(i + 1);
                self.event(SPAWN, id, n);
                vec![TASK_BASE + i as u64, 0]
            }
            RtOp::Join { r } => {
                let i = self.child(a[0]);
                let id = self.sched.tasks[i + 1].id;
                // CONC-6: a cancellation point at entry and at each wait.
                loop {
                    self.check_cancel(t)?;
                    if self.sched.tasks[i + 1].state == State::Ended {
                        break;
                    }
                    self.park_task(t, Condition::Join(i));
                }
                let status = self.children[i].status;
                if status == 0 {
                    let n = self.held(TypeRef(r.0)) as usize;
                    let bytes = self.get(self.children[i].result, n).to_vec();
                    self.put(a[1], &bytes);
                }
                self.event(JOIN, id, u64::from(status != 0));
                let cells = self.settle_child(i);
                if status == 0 {
                    vec![0, 0]
                } else {
                    let chain = std::mem::take(&mut self.children[i].chain);
                    vec![u64::from(status), self.deliver(chain, cells)]
                }
            }
            RtOp::CauseNext => vec![self.load(a[0] + CELL_NEXT, 8)],
            RtOp::CauseValue => vec![a[0]],
            RtOp::Cancel => {
                let i = self.child(a[0]);
                self.cancel(i);
                vec![]
            }
            RtOp::CheckCancel => {
                self.check_cancel(t)?;
                self.event(CHECK_CANCEL, 0, 0);
                vec![]
            }
            // Isolation (crate docs): a reservation on entry, set aside with the
            // active chain; refunded when the body returns; on a caught failure the
            // chain becomes a `Causes`, and the reserved cell takes the place of the
            // obligation cell the failure used, or is refunded when it used none.
            RtOp::IsolateEnter => {
                assert!(
                    t.obligations.last().is_some_and(Option::is_some),
                    "isolate entered with its obligation's cell used"
                );
                if let Err(room) = self.admit(cur, charge::CAUSE_CELL) {
                    return Ok(vec![0, room]);
                }
                let r = Reservation {
                    limit: cur.clone(),
                    chain: std::mem::take(&mut t.chain),
                    cells: std::mem::take(&mut t.cells),
                };
                t.reserved.push(r);
                vec![1, 0]
            }
            RtOp::IsolateLeave => {
                let r = t.reserved.pop().expect("isolate_leave with no reservation");
                assert!(t.chain.is_empty(), "isolate_leave during a failure");
                self.refund(r.limit, charge::CAUSE_CELL);
                (t.chain, t.cells) = (r.chain, r.cells);
                vec![]
            }
            RtOp::IsolateCatch => {
                let r = t.reserved.pop().expect("isolate_catch with no reservation");
                assert!(!t.chain.is_empty(), "isolate_catch with no failure");
                if t.chain.first().is_some_and(|cause| {
                    cause.kind == "Cancelled"
                        || (cause.kind == "OutOfTime"
                            && t.step_bounds
                                .iter()
                                .any(|bound| cause.message[0] == bound.id))
                }) {
                    // FAIL-16: not caught; the failure goes on past the isolate.
                    self.refund(r.limit, charge::CAUSE_CELL);
                    let (chain, cells) =
                        (std::mem::take(&mut t.chain), std::mem::take(&mut t.cells));
                    (t.chain, t.cells) = (r.chain, r.cells);
                    t.chain.extend(chain);
                    t.cells.extend(cells);
                    return Err(Stop::Chained);
                }
                let (chain, cells) = (std::mem::take(&mut t.chain), std::mem::take(&mut t.cells));
                (t.chain, t.cells) = (r.chain, r.cells);
                let head = self.deliver(chain, cells);
                let top = t.obligations.last_mut().expect("an obligation");
                if top.is_none() {
                    *top = Some(Cell {
                        limit: r.limit,
                        amount: charge::CAUSE_CELL,
                        resource: false,
                    });
                } else {
                    self.refund(r.limit, charge::CAUSE_CELL);
                }
                vec![head]
            }
            RtOp::DestId => {
                self.dests += 1;
                vec![self.dests]
            }
            RtOp::ArgsLen => vec![self.world.args.len() as u64],
            RtOp::ArgsGet => vec![self.world.args.get(a[1] as usize).copied().unwrap_or(0)],
            RtOp::LaunchWord => vec![self.world.launch.as_ref().unwrap().word(a[0], a[1])],
            RtOp::LaunchHandle => vec![0],
            RtOp::DescribeEmit => {
                if self.world.launch.as_ref().unwrap().is_describe() {
                    let (bytes, _, _, _) = self.carrier(a[0]);
                    self.world.stdout.extend_from_slice(&bytes);
                    vec![1]
                } else {
                    vec![0]
                }
            }
            RtOp::LaunchTarget { .. } => {
                let (data, len) = self.world.launch_target;
                self.store(a[0], 8, data);
                self.store(a[0] + 8, 8, len);
                self.store(a[0] + 16, 8, 0);
                vec![]
            }
            RtOp::Host(op) => self.host(t, op, a)?,
            RtOp::Flow(_) => panic!("flow execution uses the native reference"),
            RtOp::CompareBytes => {
                let range = |p: u64, n: u64| match n {
                    0 => Vec::new(),
                    n => self.get(p, n as usize).to_vec(),
                };
                let order = range(a[0], a[1]).cmp(&range(a[2], a[3]));
                vec![order as i8 as u8 as u64]
            }
            RtOp::HashBytes => {
                let bytes = match a[1] {
                    0 => Vec::new(),
                    n => self.get(a[0], n as usize).to_vec(),
                };
                vec![crate::hash_bytes(&bytes)]
            }
            RtOp::Xxh3Short => {
                let data = if a[1] == 0 {
                    &[]
                } else {
                    self.get(a[0], a[1] as usize)
                };
                vec![twox_hash::XxHash3_64::oneshot(data)]
            }
            RtOp::Xxh3Stripes => {
                let count = a[3] as usize;
                if count != 0 {
                    let mut acc = [0u64; 8];
                    for (lane, bytes) in acc.iter_mut().zip(self.get(a[0], 64).as_chunks::<8>().0) {
                        *lane = u64::from_le_bytes(*bytes);
                    }
                    let data = self.get(a[1], count * 64);
                    let secret = self.get(a[2], 64 + (count - 1) * 8);
                    for (i, stripe) in data.as_chunks::<64>().0.iter().enumerate() {
                        for (j, bytes) in stripe.as_chunks::<8>().0.iter().enumerate() {
                            let key_at = (i + j) * 8;
                            let key =
                                u64::from_le_bytes(secret[key_at..key_at + 8].try_into().unwrap());
                            let word = u64::from_le_bytes(*bytes);
                            let keyed = word ^ key;
                            acc[j ^ 1] = acc[j ^ 1].wrapping_add(word);
                            acc[j] = acc[j].wrapping_add((keyed & 0xffff_ffff) * (keyed >> 32));
                        }
                    }
                    let mut encoded = [0; 64];
                    for (word, bytes) in acc.into_iter().zip(encoded.as_chunks_mut::<8>().0) {
                        *bytes = word.to_le_bytes();
                    }
                    self.put(a[0], &encoded);
                }
                vec![]
            }
            RtOp::Sha1Blocks => {
                let mut h = [0u32; 5];
                for (word, bytes) in h.iter_mut().zip(self.get(a[0], 20).as_chunks::<4>().0) {
                    *word = u32::from_le_bytes(*bytes);
                }
                let data = if a[2] == 0 {
                    &[]
                } else {
                    self.get(a[1], a[2] as usize)
                };
                sha1_blocks(&mut h, data);
                let mut encoded = [0; 20];
                for (word, bytes) in h.into_iter().zip(encoded.as_chunks_mut::<4>().0) {
                    *bytes = word.to_le_bytes();
                }
                self.put(a[0], &encoded);
                vec![]
            }
            RtOp::Sha256Blocks => {
                let mut h = [0u32; 8];
                for (word, bytes) in h.iter_mut().zip(self.get(a[0], 32).as_chunks::<4>().0) {
                    *word = u32::from_le_bytes(*bytes);
                }
                let data = match a[2] {
                    0 => Vec::new(),
                    n => self.get(a[1], (n * 64) as usize).to_vec(),
                };
                sha256_blocks(&mut h, &data);
                self.put(
                    a[0],
                    &h.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<_>>(),
                );
                vec![]
            }
            RtOp::ChannelNew { item } => {
                let item = TypeRef(item.0);
                if a[0] == 0 {
                    return Err(Stop::Cause("Bounds"));
                }
                let charge = charge::channel(a[0], self.logical(item)).unwrap_or(u64::MAX);
                match self.admit(cur, charge) {
                    Err(room) => vec![0, 0, room],
                    Ok(()) => {
                        let id = self.next_id();
                        self.event(SENDERS, id, 1);
                        self.chans.push(Chan {
                            id,
                            item,
                            cap: a[0],
                            queue: VecDeque::new(),
                            senders: 1,
                            receiver: true,
                            charge,
                            limit: Some(cur.clone()),
                        });
                        let s = CHAN_BASE + 2 * (self.chans.len() as u64 - 1);
                        vec![s, s + 1, 0]
                    }
                }
            }
            RtOp::ChannelSender => {
                let c = &mut self.chans[((a[0] - CHAN_BASE) / 2) as usize];
                c.senders += 1;
                let (id, senders) = (c.id, c.senders);
                self.event(SENDERS, id, senders);
                vec![a[0]]
            }
            RtOp::Send { item } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                // CONC-5: a cancellation point at entry and at each wait.
                loop {
                    self.check_cancel(t)?;
                    match self.publish(i, a[1], item) {
                        0 => break vec![1],
                        2 => {
                            self.event(
                                PUBLISH,
                                self.chan_id(i),
                                hirsel_core_layout::event::CHANNEL_CLOSED,
                            );
                            break vec![0];
                        }
                        _ => self.park_task(t, Condition::Send(i)),
                    }
                }
            }
            RtOp::TrySend { item } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                let status = self.publish(i, a[1], item);
                let source = self.site(t);
                // The send's own event: the turn it may give up can record more after it.
                let event = self.sched.events.as_ref().map(Vec::len);
                self.event(TRY_SEND, self.chan_id(i), status);
                if let (Some(event), Some(events)) = (event, &mut self.sched.events) {
                    events[event].source = source;
                }
                vec![status]
            }
            RtOp::Receive { .. } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                loop {
                    self.check_cancel(t)?;
                    if self.take(i, a[1]) {
                        break vec![1];
                    }
                    if self.chans[i].senders == 0 {
                        self.event(
                            RECEIVE,
                            self.chan_id(i),
                            hirsel_core_layout::event::CHANNEL_CLOSED,
                        );
                        break vec![0];
                    }
                    self.park_task(t, Condition::Receive(i));
                }
            }
            RtOp::LatestNew { item } => match self.admit(cur, charge::LATEST_ROOT) {
                Err(room) => vec![0, room],
                Ok(()) => {
                    let id = self.next_id();
                    self.event(SENDERS, id, 1);
                    self.latests.push(Latest {
                        id,
                        item: TypeRef(item.0),
                        root: a[0],
                        handles: 1,
                        limit: Some(cur.clone()),
                    });
                    vec![LATEST_BASE + self.latests.len() as u64 - 1, 0]
                }
            },
            RtOp::LatestHandle => {
                let l = &mut self.latests[(a[0] - LATEST_BASE) as usize];
                l.handles += 1;
                let (id, handles) = (l.id, l.handles);
                self.event(SENDERS, id, handles);
                vec![a[0]]
            }
            RtOp::LatestPublish => {
                let l = &mut self.latests[(a[0] - LATEST_BASE) as usize];
                let (id, item, old) = (l.id, l.item, std::mem::replace(&mut l.root, a[1]));
                self.event(LATEST_PUBLISH, id, 0);
                self.release_latest_holder(t, old, item);
                vec![]
            }
            RtOp::LatestRead => {
                let l = &self.latests[(a[0] - LATEST_BASE) as usize];
                let (id, root) = (l.id, l.root);
                self.event(LATEST_READ, id, 0);
                self.share_counted(root, Counted::Arc);
                vec![root]
            }
            RtOp::TryReceive { .. } => {
                let i = ((a[0] - CHAN_BASE) / 2) as usize;
                let status = if self.take(i, a[1]) {
                    0
                } else if self.chans[i].senders == 0 {
                    2
                } else {
                    1
                };
                self.event(TRY_RECEIVE, self.chan_id(i), status);
                vec![status]
            }
        })
    }

    // ---- functions
    /// Check a call's loans (crate docs, "Loans") and freeze its `view` places
    /// until the caller truncates `frozen` back. The destination is exclusive.
    fn lend(&mut self, sig: &Signature, args: &[u64]) {
        let places: Vec<(usize, usize, usize, Loan)> = sig
            .dest
            .map(|ty| Param::Address {
                place: PlaceTy::Declared(ty),
                loan: Loan::Mut,
            })
            .into_iter()
            .chain(sig.params.iter().copied())
            .scan(0, |offset, param| {
                let a = *args.get(*offset)?;
                *offset += match param {
                    Param::Value(ty) => simd::word_count(ty),
                    Param::Address { .. } => 1,
                };
                Some((param, a))
            })
            .filter_map(|(p, a)| match p {
                Param::Address { place, loan } => {
                    let n = self.place_size(place) as usize;
                    (n > 0).then(|| {
                        let (b, o) = split(a);
                        (b, o, n, loan)
                    })
                }
                Param::Value(_) => None,
            })
            .collect();
        for (i, &(b, o, n, loan)) in places.iter().enumerate() {
            self.loans += 1;
            match loan {
                Loan::View | Loan::Take => {
                    self.frozen.push((b, o, n));
                    continue;
                }
                Loan::Mut => {}
            }
            for (j, &(b2, o2, n2, _)) in places.iter().enumerate() {
                assert!(
                    i == j || b != b2 || o + n <= o2 || o2 + n2 <= o,
                    "a {loan:?} place +{o}..+{} overlaps another argument's place +{o2}..+{}",
                    o + n,
                    o2 + n2
                );
            }
        }
    }
    fn call(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<Vec<u64>> {
        let frame = 0u8;
        let exhausted = TASK_STACK_ROOM.with(|room| {
            let (base, available) = room.get();
            base.abs_diff(&frame as *const u8 as usize) > available
        });
        if exhausted {
            // FAIL-15 bypasses every program cleanup and ends the entire run.
            std::panic::resume_unwind(Box::new(RunStop("Stack")));
        }
        if self.sched.current != 0 {
            let used =
                TASK_STACK_ROOM.with(|room| room.get().0.abs_diff(&frame as *const u8 as usize));
            self.task_stack_peak = self.task_stack_peak.max(used);
            self.task_call_depth = self.task_call_depth.max(t.at.len() + 1);
        }
        let m = self.m;
        let facts = &self.facts;
        let roles = self.roles[f.index()]
            .get_or_insert_with(|| facts.roles(m, f).into())
            .clone();
        self.footprints.push(Footprint {
            roles,
            spans: HashMap::new(),
        });
        let r = self.activation(t, f, args);
        self.footprints.pop();
        r
    }

    /// Note that an access through `a` touched `n` bytes at `p`, and stop the run
    /// when another role of this activation touched any of them (crate docs, "Roles").
    fn touch(&mut self, a: AddressId, p: u64, n: usize) {
        let Some(frame) = self.footprints.last_mut() else {
            return;
        };
        let Some(role) = frame.roles.get(a.index()).copied().flatten() else {
            return;
        };
        if n == 0 {
            return;
        }
        self.role_checks += 1;
        let (b, lo) = split(p);
        // Static bytes are never written, so any roles may read them together.
        if self.mem[b].fixed {
            return;
        }
        let hi = lo + n;
        let spans = frame.spans.entry(b).or_default();
        let near: Vec<(usize, usize, roles::Role)> = spans
            .range(..=hi)
            .rev()
            .take_while(|(_, (h, _))| *h >= lo)
            .map(|(&l, &(h, r))| (l, h, r))
            .collect();
        let (mut start, mut end) = (lo, hi);
        for (l, h, r) in near {
            if r == role {
                spans.remove(&l);
                (start, end) = (start.min(l), end.max(h));
            } else {
                assert!(
                    h <= lo || hi <= l,
                    "{role:?} touches +{}..+{} of a block that {r:?} touched",
                    lo.max(l),
                    hi.min(h)
                );
            }
        }
        spans.insert(start, (end, role));
    }

    /// Run one activation of `f`. Its slots are frame memory: they end when the
    /// activation does, by return or by failure, so a later use of an address into
    /// them is a stop (crate docs, "Values, slots and addresses").
    fn activation(&mut self, t: &mut Task, f: FuncId, args: &[u64]) -> X<Vec<u64>> {
        let b = self.m.functions[f.index()]
            .body
            .as_ref()
            .expect("external functions do not run here");
        let slots: Vec<u64> = b
            .slots
            .iter()
            .map(|s| self.block(self.place_size(*s), None, 0, None))
            .collect();
        let r = self.frame(t, b, args, &slots, self.inherits[f.index()]);
        for p in slots {
            self.mem.remove(split(p).0);
        }
        r
    }
    fn frame(
        &mut self,
        t: &mut Task,
        b: &Body,
        args: &[u64],
        slots: &[u64],
        inherits: bool,
    ) -> X<Vec<u64>> {
        t.at.push((None, inherits));
        let r = self.blocks(t, b, args, slots);
        t.at.pop();
        r
    }
    /// SITES.md: the innermost anchor of the task's activations that is not in a
    /// core@1 item. A core operation's cause, or an unanchored one's, takes the
    /// site of the call that reached it.
    ///
    /// Only a function that inherits (`source::inherits_site`) reads its caller's
    /// site; engines pass a body that does not inherit none. So a cause whose walk
    /// would leave such a body and still find an anchor stops the run: no producer
    /// may emit one (SITES.md 6(b)).
    fn site(&self, t: &Task) -> Option<Anchor> {
        let real = |a: &Option<Anchor>| {
            a.filter(|a| !self.m.items[a.item.index()].package_is("core", "1"))
        };
        let mut frames = t.at.iter().rev();
        while let Some((at, inherits)) = frames.next() {
            if let Some(a) = real(at) {
                return Some(a);
            }
            if !inherits {
                let beyond = frames.find_map(|(at, _)| real(at));
                assert!(
                    beyond.is_none(),
                    "a site falls back through a body that does not inherit its caller's site"
                );
                return None;
            }
        }
        None
    }
    fn blocks(&mut self, t: &mut Task, b: &Body, args: &[u64], slots: &[u64]) -> X<Vec<u64>> {
        let depth = t.limits.len();
        let reserved = t.reserved.len();
        let mut vals = vec![0u64; b.values.len()];
        let mut vectors = simd::Vectors::default();
        let mut places = vec![(0u64, At::Other); b.addresses.len()];
        let pool = |r: PoolRange<ValueId>| r.get(&b.pools.values).unwrap().to_vec();
        let mut block = BlockId(0);
        let mut incoming = args.to_vec();
        'blocks: loop {
            assert!(
                self.instructions != 0,
                "evaluator instruction budget exhausted"
            );
            self.instructions -= 1;
            let blk = &b.blocks[block.index()];
            vectors.unpack(b, &mut vals, &pool(blk.params), &incoming);
            let first = blk.insts.start as usize;
            for (k, inst) in blk.insts.get(&b.insts).unwrap().iter().enumerate() {
                if matches!(inst, Inst::BuiltinUse { .. }) {
                    continue;
                }
                assert!(
                    self.instructions != 0,
                    "evaluator instruction budget exhausted"
                );
                self.instructions -= 1;
                t.at.last_mut().unwrap().0 = b.inst_anchors[first + k];
                // Keep the large instruction dispatcher off the recursive call stack.
                let r = match *inst {
                    Inst::Call { call } => self.call_inst(
                        t,
                        b,
                        &mut vals,
                        &mut vectors,
                        &places,
                        &b.calls[call.index()],
                    ),
                    _ => self.inst(t, b, slots, &mut vals, &mut vectors, &mut places, inst),
                };
                if let Err(stop) = r {
                    if let Stop::Cause(c) = stop {
                        let site = self.site(t);
                        let cause = Cause {
                            kind: c,
                            message: [0; 3],
                            site,
                        };
                        self.record(t, cause);
                    }
                    match inst.fail(b) {
                        Some(fb) => {
                            (block, incoming) = (fb, Vec::new());
                            continue 'blocks;
                        }
                        None => break 'blocks,
                    }
                }
            }
            let take = |e: &EdgeId, vals: &[u64]| {
                let e = &b.edges[e.index()];
                (e.target, vectors.pack(b, vals, &pool(e.args)))
            };
            (block, incoming) = match &b.terminators[block.index()] {
                Terminator::Jump(e) => take(e, &vals),
                Terminator::Branch { condition, yes, no } => take(
                    if vals[condition.index()] != 0 {
                        yes
                    } else {
                        no
                    },
                    &vals,
                ),
                Terminator::Switch {
                    value,
                    arms,
                    otherwise,
                } => {
                    let x = vals[value.index()];
                    let arms = arms.get(&b.pools.arms).unwrap();
                    take(
                        arms.iter()
                            .find(|a| a.value == x)
                            .map_or(otherwise, |a| &a.edge),
                        &vals,
                    )
                }
                Terminator::Return(r) => {
                    assert_eq!(t.limits.len(), depth, "a return left a bound scope open");
                    assert_eq!(t.reserved.len(), reserved, "a return left an isolate open");
                    return Ok(vectors.pack(b, &vals, &pool(*r)));
                }
                Terminator::Raise { cause: c, cleanup } => {
                    let mut message = [0; 3];
                    match c {
                        RaiseCause::Fail(a) => {
                            let (p, _) = self.address(b, slots, &vals, &places, *a)?;
                            message = [0, 8, 16].map(|o| self.load(p + o, 8));
                        }
                        // `Capacity(c)`'s payload sits where `Fail`'s carrier does.
                        RaiseCause::Capacity {
                            requested,
                            available,
                        } => message = [vals[requested.index()], vals[available.index()], 0],
                        RaiseCause::Unwrap | RaiseCause::Bounds => {}
                    }
                    t.at.last_mut().unwrap().0 = blk.anchor;
                    let site = self.site(t);
                    let cause = Cause {
                        kind: cause(*c),
                        message,
                        site,
                    };
                    self.record(t, cause);
                    match cleanup {
                        Some(c) => (*c, Vec::new()),
                        None => break 'blocks,
                    }
                }
                Terminator::Propagate => break 'blocks,
                Terminator::Unreachable => panic!("an unreachable point was reached"),
            };
        }
        assert_eq!(t.limits.len(), depth, "a failure left a bound scope open");
        assert_eq!(t.reserved.len(), reserved, "a failure left an isolate open");
        Err(Stop::Chained)
    }

    fn address(
        &self,
        b: &Body,
        slots: &[u64],
        vals: &[u64],
        places: &[(u64, At)],
        a: AddressId,
    ) -> X<(u64, At)> {
        let info = &b.addresses[a.index()];
        let (mut p, mut at) = match info.base {
            AddressBase::Slot(s) => match b.slots[s.index()] {
                PlaceTy::Declared(t) => (slots[s.index()], self.at_of(TypeRef(t.0))),
                PlaceTy::Value(_) => (slots[s.index()], At::Other),
            },
            // A pointer value names the storage it points at: a typed address with
            // no steps is that value itself (a resource's block, a box payload).
            AddressBase::Value(v) => match info.ty {
                PlaceTy::Declared(t) if info.steps.len == 0 => {
                    (vals[v.index()], At::Decl(TypeRef(t.0)))
                }
                _ => (vals[v.index()], At::Other),
            },
            AddressBase::Address(x) => places[x.index()],
        };
        for step in info.steps.get(&b.pools.steps).unwrap() {
            let (case, field) = match *step {
                Step::Deref => {
                    (p, at) = match at {
                        At::Ptr(t) => (self.load(p, 8), At::Decl(t)),
                        At::Counted(t) => (self.load(p, 8) + 8, self.at_of(t)),
                        _ => panic!("deref of an untyped place"),
                    };
                    continue;
                }
                Step::Field(f) => (None, f),
                Step::CaseField { case, field } => (Some(case), field),
                Step::Element(i) => {
                    let At::Array(k) = at else {
                        panic!("element of a place that is not an inline array")
                    };
                    let (e, n) = &self.arrays[k];
                    let i = vals[i.index()];
                    if i >= *n {
                        return Err(Stop::Cause("Bounds"));
                    }
                    p += i * self.stride(e);
                    at = self.at_field(e);
                    continue;
                }
                Step::Byte(n) => {
                    p += vals[n.index()];
                    // A byte offset is untyped for raw buffers, but the front can
                    // also use it to address a declared inline value in a closure
                    // environment. Preserve that declared row so a following field
                    // step remains typed (LANG-14 by-value captures).
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.at_of(TypeRef(t.0)),
                        PlaceTy::Value(_) => At::Other,
                    };
                    continue;
                }
                Step::Seq(w) => {
                    let At::Seq(size) = at else {
                        panic!("seq word of a place that is not a seq field")
                    };
                    p += w.offset();
                    at = At::Word(w, size);
                    continue;
                }
                Step::Index(i) => {
                    // Raw storage of elements of the address's type.
                    p += vals[i.index()] * self.place_size(info.ty);
                    at = match info.ty {
                        PlaceTy::Declared(t) => self.at_of(TypeRef(t.0)),
                        PlaceTy::Value(_) => At::Other,
                    };
                    continue;
                }
            };
            let At::Decl(t) = at else {
                panic!("field of an untyped place")
            };
            let off = self.fixed(t).offsets[case.unwrap_or(0) as usize][field as usize];
            let fty = &self.shape(t).fields(case).unwrap()[field as usize].ty;
            at = self.at_field(fty);
            p += off;
        }
        Ok((p, at))
    }

    fn load_vector(&self, ptr: u64, len: usize) -> [u8; 32] {
        self.read_marked(ptr, len);
        let mut bytes = [0; 32];
        for (i, chunk) in bytes[..len].chunks_exact_mut(8).enumerate() {
            chunk.copy_from_slice(&self.load(ptr + 8 * i as u64, 8).to_le_bytes());
        }
        bytes
    }

    fn call_inst(
        &mut self,
        t: &mut Task,
        b: &Body,
        vals: &mut [u64],
        vectors: &mut simd::Vectors,
        places: &[(u64, At)],
        site: &CallSite,
    ) -> X<()> {
        let args = site.args.get(&b.pools.args).unwrap();
        let mut words = Vec::with_capacity(args.len());
        for arg in args {
            match *arg {
                Arg::Value(v) => vectors.push(b, vals, v, &mut words),
                Arg::Address(a) => words.push(places[a.index()].0),
            }
        }
        let args = words;
        let held = self.frozen.len();
        self.lend(&self.m.signatures[site.signature.index()], &args);
        let r = match site.callee {
            Callee::Direct(f) => self.call(t, f, &args),
            Callee::Indirect(v) => self.call(t, FuncId((vals[v.index()] - FN_BASE) as u32), &args),
            Callee::Runtime(op) => self.rt(t, op, &args),
        };
        self.frozen.truncate(held);
        let r = r?;
        vectors.unpack(b, vals, site.results.get(&b.pools.values).unwrap(), &r);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn inst(
        &mut self,
        t: &mut Task,
        b: &Body,
        slots: &[u64],
        vals: &mut [u64],
        vectors: &mut simd::Vectors,
        places: &mut [(u64, At)],
        inst: &Inst,
    ) -> X<()> {
        let ty = |v: ValueId| b.values[v.index()];
        match *inst {
            Inst::Const { result, bits } => vals[result.index()] = bits,
            Inst::Simd {
                result,
                op,
                first,
                second,
                ..
            } => {
                if matches!(op, SimdOp::From | SimdOp::Lanes) {
                    let ptr = vals[first.index()];
                    let len = bytes_of(ty(result));
                    vectors.set(result, self.load_vector(ptr, len));
                } else {
                    let vector = |id| match ty(id) {
                        Ty::Vector(_) => vectors.value(id),
                        _ => [0; 32],
                    };
                    let scalar = if op == SimdOp::Splat {
                        vals[first.index()]
                    } else {
                        second.map_or(0, |id| vals[id.index()])
                    };
                    match simd::apply(
                        op,
                        ty(result),
                        ty(first),
                        vector(first),
                        second.map_or([0; 32], vector),
                        scalar,
                    )? {
                        simd::ResultValue::Scalar(word) => vals[result.index()] = word,
                        simd::ResultValue::Vector(bytes) => vectors.set(result, bytes),
                    }
                }
            }
            Inst::PutLanes { to, vector, count } => {
                let ptr = places[to.index()].0;
                let bytes = vectors.value(vector);
                let count = vals[count.index()] as usize;
                self.touch(to, ptr, count);
                for (i, &byte) in bytes[..count].iter().enumerate() {
                    self.store(ptr + i as u64, 1, u64::from(byte));
                }
            }
            Inst::LayoutConst { .. } => unreachable!("engine-private operands are resolved"),
            Inst::FunctionRef { result, function } => {
                vals[result.index()] = FN_BASE + u64::from(function.0)
            }
            Inst::DescriptorRef {
                result, function, ..
            } => vals[result.index()] = self.descriptors[function.index()],
            // Statics are the first blocks, in module order.
            Inst::ReadStatic { result, data } => vals[result.index()] = ptr(data.index(), 0),
            Inst::CompactWidth { result, word, .. } => vals[result.index()] = vals[word.index()],
            Inst::Unary {
                result,
                op,
                operand,
                ..
            } => {
                let (t, x) = (ty(operand), vals[operand.index()]);
                vals[result.index()] = match op {
                    UnaryOp::Not => x ^ 1,
                    UnaryOp::BitNot => mask(t, !x),
                    UnaryOp::Neg if t.is_float() => bits(t, -float(t, x)),
                    UnaryOp::Neg => fit(t, -int(t, x)).ok_or(Stop::Cause("Overflow"))?,
                };
            }
            Inst::Binary {
                result,
                op,
                left,
                right,
                ..
            } => {
                let t = ty(left);
                let (a, c) = (vals[left.index()], vals[right.index()]);
                vals[result.index()] = if t.is_float() {
                    float_binary(t, op, a, c)
                } else {
                    let division = matches!(op, BinaryOp::Div | BinaryOp::Rem);
                    int_binary(t, op, a, c).ok_or(Stop::Cause(if division && c == 0 {
                        "Division"
                    } else {
                        "Overflow"
                    }))?
                };
            }
            Inst::Compare {
                result,
                op,
                left,
                right,
            } => {
                let t = ty(left);
                let (a, c) = (vals[left.index()], vals[right.index()]);
                let ord = if t.is_float() {
                    float(t, a).partial_cmp(&float(t, c))
                } else {
                    Some(int(t, a).cmp(&int(t, c)))
                };
                use std::cmp::Ordering::*;
                vals[result.index()] = u64::from(match op {
                    CompareOp::Eq => ord == Some(Equal),
                    CompareOp::Ne => ord != Some(Equal),
                    CompareOp::Lt => ord == Some(Less),
                    CompareOp::Le => matches!(ord, Some(Less | Equal)),
                    CompareOp::Gt => ord == Some(Greater),
                    CompareOp::Ge => matches!(ord, Some(Greater | Equal)),
                });
            }
            Inst::Convert {
                result,
                flag,
                operand,
                mode,
            } => {
                let (from, to, x) = (ty(operand), ty(result), vals[operand.index()]);
                let char_ty = Ty::Scalar(Scalar::Char);
                let (r, fl) = match mode {
                    ConvertMode::Widen if from == char_ty => (x, 0),
                    ConvertMode::TryNarrow if to == char_ty => {
                        let ok = (x < 0xD800) || (0xE000..=0x10FFFF).contains(&x);
                        (if ok { x } else { 0 }, u64::from(ok))
                    }
                    ConvertMode::Widen => (fit(to, int(from, x)).expect("widen fits"), 0),
                    ConvertMode::Truncate => (mask(to, x), 0),
                    ConvertMode::TryNarrow => {
                        (mask(to, x), u64::from(fit(to, int(from, x)).is_some()))
                    }
                    ConvertMode::FloatToInt => {
                        let f = float(from, x);
                        let (lo, hi) = range(to);
                        if f.is_nan() {
                            (0, 1)
                        } else if f.trunc() >= lo as f64 && f.trunc() < (hi + 1) as f64 {
                            (mask(to, f.trunc() as i128 as u64), 0)
                        } else {
                            (0, 2)
                        }
                    }
                    ConvertMode::IntToFloat => {
                        let v = int(from, x);
                        let r = if is_f32(to) {
                            u64::from((v as f32).to_bits())
                        } else {
                            (v as f64).to_bits()
                        };
                        (r, 0)
                    }
                    ConvertMode::Reinterpret if from.is_float() => (bits(from, float(from, x)), 0),
                    ConvertMode::Reinterpret => (bits(to, float(to, x)), 0),
                    ConvertMode::FloatResize => (bits(to, float(from, x)), 0),
                };
                vals[result.index()] = r;
                if let Some(f) = flag {
                    vals[f.index()] = fl;
                }
            }
            Inst::IntArith {
                result,
                flag,
                op,
                mode,
                left,
                right,
            } => {
                let t = ty(left);
                let (a, c) = (int(t, vals[left.index()]), int(t, vals[right.index()]));
                let exact = match op {
                    ArithOp::Add => Some(a + c),
                    ArithOp::Sub => Some(a - c),
                    ArithOp::Mul => Some(a.saturating_mul(c)),
                    ArithOp::Div => (c != 0).then(|| a / c),
                };
                let fits = exact.and_then(|e| fit(t, e));
                vals[result.index()] = match mode {
                    ArithMode::Checked if op == ArithOp::Div => fits.unwrap_or(0),
                    ArithMode::Wrapping | ArithMode::Checked if op == ArithOp::Mul => {
                        mask(t, vals[left.index()].wrapping_mul(vals[right.index()]))
                    }
                    ArithMode::Wrapping | ArithMode::Checked => mask(t, exact.unwrap() as u64),
                    ArithMode::Saturating => {
                        let (lo, hi) = range(t);
                        fit(t, exact.unwrap().clamp(lo, hi)).unwrap()
                    }
                };
                if let Some(f) = flag {
                    vals[f.index()] = u64::from(fits.is_none());
                }
            }
            Inst::MulAdd {
                result,
                left,
                right,
                addend,
            } => {
                let t = ty(result);
                let [a, c, d] = [left, right, addend].map(|v| vals[v.index()]);
                vals[result.index()] = if is_f32(t) {
                    let f = |x: u64| f32::from_bits(x as u32);
                    bits(t, f64::from(f(a).mul_add(f(c), f(d))))
                } else {
                    bits(
                        t,
                        f64::from_bits(a).mul_add(f64::from_bits(c), f64::from_bits(d)),
                    )
                };
            }
            Inst::ScalarUnary {
                result,
                op,
                operand,
            } => {
                let (t, x) = (ty(operand), vals[operand.index()]);
                let w = width(t);
                let f = float(t, x);
                vals[result.index()] = match op {
                    ScalarUnaryOp::Sqrt => bits(t, f.sqrt()),
                    ScalarUnaryOp::Floor => bits(t, f.floor()),
                    ScalarUnaryOp::Ceil => bits(t, f.ceil()),
                    ScalarUnaryOp::Trunc => bits(t, f.trunc()),
                    ScalarUnaryOp::Round => bits(t, f.round()),
                    ScalarUnaryOp::RoundEven => bits(t, f.round_ties_even()),
                    ScalarUnaryOp::Abs => bits(t, f.abs()),
                    ScalarUnaryOp::LeadingZeros => u64::from(x.leading_zeros() - (64 - w)),
                    ScalarUnaryOp::TrailingZeros => u64::from(x.trailing_zeros().min(w)),
                    ScalarUnaryOp::CountOnes => u64::from(x.count_ones()),
                    ScalarUnaryOp::SwapBytes => x.swap_bytes() >> (64 - w),
                    ScalarUnaryOp::ReverseBits => x.reverse_bits() >> (64 - w),
                };
            }
            Inst::Address { address, .. } => {
                places[address.index()] = self.address(b, slots, vals, places, address)?;
            }
            Inst::Case { result, address } => {
                let (p, At::Decl(ty)) = places[address.index()] else {
                    panic!("case of an untyped place")
                };
                vals[result.index()] = u64::from(self.case_at(p, ty));
            }
            Inst::SetCase { address, case } => {
                let (p, At::Decl(ty)) = places[address.index()] else {
                    panic!("set_case of an untyped place")
                };
                let tag = self.fixed(ty).tag.expect("a variant");
                // A niche variant's dataful case is made by its payload alone.
                if let Some(word) = hirsel_core_layout::case_word(&tag, case) {
                    self.store(p + tag.offset, tag.width as usize, word);
                }
            }
            Inst::Read {
                result, address, ..
            } => {
                let n = bytes_of(ty(result));
                self.touch(address, places[address.index()].0, n);
                if matches!(ty(result), Ty::Vector(_)) {
                    vectors.set(result, self.load_vector(places[address.index()].0, n));
                } else {
                    self.read_marked(places[address.index()].0, n);
                    vals[result.index()] = self.load(places[address.index()].0, n);
                }
                if let (p, At::Word(w, size)) = places[address.index()] {
                    self.check_carrier(p - w.offset(), size, None);
                }
            }
            Inst::Store { address, value } => {
                let (t, mut x) = (ty(value), vals[value.index()]);
                if let Ty::Vector(shape) = t {
                    let ptr = places[address.index()].0;
                    let n = shape.size() as usize;
                    self.touch(address, ptr, n);
                    let bytes = vectors.value(value);
                    for (i, chunk) in bytes[..n].chunks_exact(8).enumerate() {
                        self.store(
                            ptr + 8 * i as u64,
                            8,
                            u64::from_le_bytes(chunk.try_into().unwrap()),
                        );
                    }
                    return Ok(());
                }
                if let Some(k) = self.scrambled.as_mut()
                    && t.is_float()
                    && float(t, x).is_nan()
                {
                    x = scrambled(t, *k);
                    *k += 1;
                }
                let n = bytes_of(t);
                self.touch(address, places[address.index()].0, n);
                self.store(places[address.index()].0, n, x);
                if let (p, At::Word(w, size)) = places[address.index()] {
                    self.check_carrier(p - w.offset(), size, Some(w));
                }
            }
            // `StorageLive` begins the slot's storage unwritten, whether or not
            // it was live; after `StorageDead` a use of an address into it is a
            // stop until it begins again (crate docs, "Values, slots and
            // addresses").
            Inst::StorageLive { slot } => {
                let size = self.place_size(b.slots[slot.index()]) as usize;
                let blk = &mut self.mem[split(slots[slot.index()]).0];
                blk.bytes = vec![0; size];
                blk.marks.clear();
                blk.live = true;
            }
            // The record stays in the table until the activation ends, so a
            // later `StorageLive` revives the same identity; reads of it stop
            // while it is not live.
            Inst::StorageDead { slot } => {
                let blk = &mut self.mem[split(slots[slot.index()]).0];
                blk.live = false;
                blk.bytes = Vec::new();
                blk.marks.clear();
            }
            Inst::UpdateMask {
                result,
                mask: mv,
                field,
                initialized,
            } => {
                let x = vals[mv.index()];
                vals[result.index()] = if initialized {
                    x | (1 << field)
                } else {
                    x & !(1 << field)
                };
            }
            Inst::Call { .. } => unreachable!("calls dispatch from the block loop"),
            Inst::Alloc {
                result,
                available,
                region_entry,
                ref kind,
                place,
                charge,
                overflow,
                size,
                ..
            } => {
                let l = t.limits.last().unwrap().clone();
                if let Some(token) = region_entry {
                    vals[token.index()] = 0;
                }
                let n = vals[charge.index()];
                match self.admit_charge(&l, n, overflow.is_some_and(|v| vals[v.index()] != 0)) {
                    Err(room) => (vals[result.index()], vals[available.index()]) = (0, room),
                    Ok(()) if let AllocPlace::Frame(at) = place => {
                        (vals[result.index()], vals[available.index()]) = (vals[at.index()], 0);
                    }
                    Ok(()) if let AllocPlace::LentFrame(at) = place => {
                        let p = vals[at.index()];
                        let slot = &mut self.mem[split(p).0];
                        slot.limit = Some(l);
                        slot.charge = n;
                        slot.lent_size = Some(vals[size.index()]);
                        slot.class = Some(kind.cleanup(&self.m.types, &self.recipes));
                        (vals[result.index()], vals[available.index()]) = (p, 0);
                    }
                    Ok(())
                        if matches!(
                            kind,
                            AllocKind::Own {
                                edge: FieldTy::FrozenBytes
                            }
                        ) =>
                    {
                        let receipt = self.block(0, Some(l), n, None);
                        (vals[result.index()], vals[available.index()]) = (receipt | 1, 0);
                    }
                    Ok(()) => {
                        let res = match kind {
                            AllocKind::Resource { grant } => Some(Res {
                                grant: *grant,
                                creator: self.sched.current,
                                settled: false,
                                cell_used: false,
                            }),
                            _ => None,
                        };
                        let p = self.block(vals[size.index()], Some(l.clone()), n, res);
                        if let AllocKind::Environment { ty } = kind {
                            self.store(p, 8, ENV_BASE + u64::from(ty.0));
                        }
                        if let Some(count) = kind.counted() {
                            self.mem[split(p).0].counted = Some(count);
                            self.store(p, 8, 1);
                            if count == Counted::Arc {
                                self.event(ARC_NEW, 0, 1);
                            }
                        }
                        self.mem[split(p).0].class =
                            Some(kind.cleanup(&self.m.types, &self.recipes));
                        if let AllocPlace::Region(r) = place {
                            let region =
                                &mut self.regions[(vals[r.index()] - REGION_BASE) as usize];
                            if let Some(token) = region_entry {
                                vals[token.index()] = region.len() as u64;
                            }
                            region.push(split(p).0);
                        }
                        (vals[result.index()], vals[available.index()]) = (p, 0);
                    }
                }
            }
            Inst::Free { block, charge } => {
                let p = vals[block.index()];
                assert!(self.mem[split(p).0].live, "free of a non-block");
                assert_eq!(
                    self.mem[split(p).0].charge,
                    vals[charge.index()],
                    "free refunds another charge"
                );
                self.free(p);
            }
            Inst::PromoteFrame { carrier } => {
                let at = places[carrier.index()].0;
                if let Some(FieldTy::Seq(elem)) =
                    sequence_edge(&self.m.types, b.addresses[carrier.index()].ty)
                {
                    let old = self.load(at, 8);
                    if old != 0 && self.mem[split(old).0].lent_size.is_some() {
                        let bytes = (self.load(at + 8, 8) * self.stride(elem)) as usize;
                        let slot = &mut self.mem[split(old).0];
                        let size = slot.lent_size.take().unwrap();
                        let charge = std::mem::take(&mut slot.charge);
                        let payer = slot.limit.take();
                        let class = slot.class.take();
                        let marks = std::mem::take(&mut slot.marks);
                        let src = slot.bytes[..bytes].to_vec();
                        let data = self.block(size, payer, charge, None);
                        let heap = &mut self.mem[split(data).0];
                        heap.class = class;
                        heap.marks = marks;
                        self.put(data, &src);
                        self.store(at, 8, data);
                    }
                }
            }
            Inst::FrameRefund { charge } => {
                let l = t.limits.last().unwrap().clone();
                self.refund(l, vals[charge.index()]);
            }
            Inst::Pointer { result, address } => vals[result.index()] = places[address.index()].0,
            Inst::Trim {
                block,
                region,
                entry: _,
                previous,
                charge,
            } => {
                let (blk, o) = split(vals[block.index()]);
                let (n, old) = (vals[charge.index()], self.mem[blk].charge);
                assert_eq!(old, vals[previous.index()], "writer retained charge");
                assert!(self.regions[(vals[region.index()] - REGION_BASE) as usize].contains(&blk));
                assert!(
                    o == 0 && n <= old,
                    "trim of a non-block or above its charge"
                );
                self.mem[blk].charge = n;
                self.refund(
                    self.mem[blk].limit.clone().expect("a region block"),
                    old - n,
                );
            }
            Inst::RegionOpen { result } => {
                self.regions.push(Vec::new());
                vals[result.index()] = REGION_BASE + self.regions.len() as u64 - 1;
            }
            Inst::RegionClose { region } => {
                let blocks = std::mem::take(
                    &mut self.regions[(vals[region.index()] - REGION_BASE) as usize],
                );
                for blk in blocks {
                    self.free(ptr(blk, 0));
                }
            }
            Inst::Step { .. } => {
                if let Some(bound) = t.step_bounds.last_mut() {
                    if bound.left == 0 {
                        let id = bound.id;
                        let site = self.site(t);
                        self.record(
                            t,
                            Cause {
                                kind: "OutOfTime",
                                message: [id, 0, 0],
                                site,
                            },
                        );
                        return Err(Stop::Chained);
                    }
                    bound.left -= 1;
                    t.slice_steps += 1;
                    if t.slice_steps == 65_536 {
                        t.slice_steps = 0;
                        self.event(hirsel_core_layout::event::STEP_SLICE, 0, 65_536);
                        self.park_turn(None, false);
                    }
                }
            }
            Inst::LimitEnter {
                admitted,
                available,
                bytes,
            } => {
                let cur = t.limits.last().unwrap();
                let n = vals[bytes.index()];
                let (ok, room) = match self.admit(cur, n) {
                    Ok(()) => (1, 0),
                    Err(room) => (0, room),
                };
                if ok == 1 {
                    let l = Limit::new(n, Some(cur.clone()), self.sched.current);
                    t.limits.push(l);
                }
                (vals[admitted.index()], vals[available.index()]) = (ok, room);
            }
            Inst::BuiltinUse { .. } => {}
            Inst::LimitExit => {
                let l = t.limits.pop().unwrap();
                self.end_limit(l, None);
            }
            Inst::Drop { address, ty, .. } => {
                let at = places[address.index()].0;
                self.drop_marked(at, self.held(TypeRef(ty.0)) as usize);
                if self.drop_value(t, at, TypeRef(ty.0)) {
                    return Err(Stop::Chained);
                }
            }
            Inst::Copy { to, from, bytes } => {
                let bytes = vals[bytes.index()];
                self.copies += u64::from(self.copy_size == Some(bytes));
                let n = bytes as usize;
                if n == 0 {
                    // Zero bytes touch no storage; a zero-sized `List`'s null data is
                    // a valid operand (SEM-4).
                    return Ok(());
                }
                self.touch(from, places[from.index()].0, n);
                self.touch(to, places[to.index()].0, n);
                let (f, t) = (places[from.index()].0, places[to.index()].0);
                assert!(
                    f == t || split(f).0 != split(t).0 || f + n as u64 <= t || t + n as u64 <= f,
                    "copy of {n} bytes between overlapping ranges"
                );
                self.read_marked(f, n);
                let src = self.get(f, n).to_vec();
                self.put(places[to.index()].0, &src);
            }
            Inst::Grow {
                result,
                available,
                carrier,
                charge,
                overflow,
                size,
                capacity,
                moved,
                refund,
                ..
            } => {
                let at = places[carrier.index()].0;
                let edge = sequence_edge(&self.m.types, b.addresses[carrier.index()].ty)
                    .expect("wf: grow of a sequence carrier")
                    .clone();
                let FieldTy::Seq(elem) = &edge else {
                    unreachable!("a sequence edge")
                };
                self.touch(carrier, at, hirsel_core_layout::SEQ_CARRIER as usize);
                let (old, len, cap) = (
                    self.load(at, 8),
                    self.load(at + 8, 8),
                    self.load(at + 16, 8),
                );
                // Capacity 0 owns nothing (empty, or static bytes): its producer
                // allocates in the current limit and copies instead.
                assert!(
                    cap != 0,
                    "grow of a carrier with capacity 0, which owns no storage"
                );
                let ob = split(old).0;
                assert!(
                    split(old).1 == 0 && self.mem[ob].live && !self.mem[ob].fixed,
                    "grow of storage its carrier does not own"
                );
                self.edge(old, &edge, "grow's old storage");
                let l = t.limits.last().unwrap().clone();
                let n = vals[charge.index()];
                match self.admit_charge(&l, n, overflow.is_some_and(|v| vals[v.index()] != 0)) {
                    // Refused: the carrier, its block and the block's elements stay.
                    Err(room) => (vals[result.index()], vals[available.index()]) = (0, room),
                    Ok(()) => {
                        let (bytes, new_cap) = (vals[moved.index()], vals[capacity.index()]);
                        assert_eq!(
                            bytes,
                            len * self.stride(elem),
                            "grow moves another byte count than its carrier's length times the stride"
                        );
                        assert_eq!(
                            vals[refund.index()],
                            self.mem[ob].charge,
                            "grow refunds another charge than its old block's"
                        );
                        assert!(
                            len <= new_cap && bytes <= vals[size.index()],
                            "grow to a block that does not hold the carrier's elements"
                        );
                        let p = self.block(vals[size.index()], Some(l.clone()), n, None);
                        let nb = split(p).0;
                        self.mem[nb].class =
                            Some(cleanup_class(&self.m.types, &self.recipes, &edge));
                        // The elements move with their liveness shadow (crate docs, "Slot
                        // kernels"); what the old block marks past them is still its own.
                        let src = self.get(old, bytes as usize).to_vec();
                        self.put(p, &src);
                        let shadow: Vec<_> = self.mem[ob]
                            .marks
                            .range(..bytes as usize)
                            .map(|(&o, &m)| (o, m))
                            .collect();
                        for (o, m) in shadow {
                            self.mem[ob].marks.remove(&o);
                            self.mem[nb].marks.insert(o, m);
                        }
                        // The carrier takes the new block before the old one's refund.
                        self.store(at, 8, p);
                        self.store(at + 16, 8, new_cap);
                        self.free(old);
                        (vals[result.index()], vals[available.index()]) = (p, 0);
                    }
                }
            }
            Inst::Share { node, kind } => {
                self.share_counted(vals[node.index()], kind);
            }
            Inst::Release { node, ty, kind } => {
                self.kind(vals[node.index()], kind);
                self.release(vals[node.index()], TypeRef(ty.0))
            }
            Inst::Mark {
                address,
                count,
                live,
                ty,
            } => {
                let size = self.place_size(ty);
                for k in 0..vals[count.index()] {
                    self.mark(places[address.index()].0 + k * size, live, size as usize);
                }
            }
            Inst::Unmark {
                address,
                count,
                live,
                ty,
            } => {
                let size = self.place_size(ty);
                for k in 0..vals[count.index()] {
                    self.unmark(places[address.index()].0 + k * size, live, size as usize);
                }
            }
            Inst::TryUnwrap { result, node, kind } => {
                self.kind(vals[node.index()], kind);
                let unique = self.load(vals[node.index()], 8) == 1;
                vals[result.index()] = u64::from(unique);
                if kind == Counted::Arc {
                    self.event(ARC_UNWRAP, 0, u64::from(unique));
                }
            }
        }
        Ok(())
    }
}

fn range(t: Ty) -> (i128, i128) {
    let (signed, w) = t.int().unwrap();
    if signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    }
}

/// What a harness reading `x` as a result of type `t` observes: a float as
/// `reinterpret` gives its bits (a NaN as the canonical one, FORM #97), else `x`.
pub fn observed(t: Ty, x: u64) -> u64 {
    if t.is_float() {
        bits(t, float(t, x))
    } else {
        x
    }
}

/// A run-line literal of type `t`: decimal floats or the constant syntax.
pub fn scalar(t: Ty, s: &str) -> u64 {
    if t.is_float() && !s.starts_with("0x") {
        let f: f64 = s.parse().unwrap_or_else(|_| panic!("bad float `{s}`"));
        return if t == Ty::Scalar(Scalar::F32) {
            u64::from((f as f32).to_bits())
        } else {
            f.to_bits()
        };
    }
    crate::text::parse_constant(t, s).unwrap_or_else(|e| panic!("{e}"))
}

fn width(t: Ty) -> u32 {
    match t {
        Ty::Scalar(s) => s.size() as u32 * 8,
        Ty::Ptr => 64,
        Ty::Vector(v) => v.size() as u32 * 8,
    }
}
fn mask(t: Ty, x: u64) -> u64 {
    let w = width(t);
    if w == 64 { x } else { x & ((1 << w) - 1) }
}
/// The integer value of `x` read at `t`.
fn int(t: Ty, x: u64) -> i128 {
    match t.int() {
        Some((true, w)) => i128::from(((x << (64 - w)) as i64) >> (64 - w)),
        _ => i128::from(x),
    }
}
/// `v` as bits of `t`, or None when it does not fit.
fn fit(t: Ty, v: i128) -> Option<u64> {
    let (signed, w) = t.int()?;
    let (lo, hi) = if signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    };
    (lo..=hi).contains(&v).then(|| mask(t, v as u64))
}
fn is_f32(t: Ty) -> bool {
    t == Ty::Scalar(Scalar::F32)
}
fn float(t: Ty, x: u64) -> f64 {
    if is_f32(t) {
        f64::from(f32::from_bits(x as u32))
    } else {
        f64::from_bits(x)
    }
}
/// A float result as bits, a NaN as the kind's one observable NaN (SEM-2).
fn bits(t: Ty, f: f64) -> u64 {
    match (is_f32(t), f.is_nan()) {
        (true, true) => 0x7fc0_0000,
        (true, false) => u64::from((f as f32).to_bits()),
        (false, true) => 0x7ff8_0000_0000_0000,
        (false, false) => f.to_bits(),
    }
}

/// The `k`th NaN [`Nans::Scrambled`] stores: quiet, never canonical, its sign
/// alternating and its payload counting up from 1.
fn scrambled(t: Ty, k: u64) -> u64 {
    if is_f32(t) {
        0x7fc0_0000 | (k & 1) << 31 | (1 + k % ((1 << 22) - 1))
    } else {
        0x7ff8_0000_0000_0000 | (k & 1) << 63 | (1 + k % ((1 << 51) - 1))
    }
}

fn int_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> Option<u64> {
    let (x, y) = (int(t, a), int(t, c));
    let w = width(t);
    match op {
        BinaryOp::Add => fit(t, x + y),
        BinaryOp::Sub => fit(t, x - y),
        BinaryOp::Mul => fit(t, x.checked_mul(y)?),
        BinaryOp::Div => fit(t, x.checked_div(y)?),
        BinaryOp::Rem => (y != 0).then(|| mask(t, (x % y) as u64)),
        BinaryOp::Shl | BinaryOp::Shr if !(0..i128::from(w)).contains(&y) => None,
        BinaryOp::Shl => Some(mask(t, a << y)),
        BinaryOp::Shr => Some(mask(t, (x >> y) as u64)),
        BinaryOp::BitAnd => Some(a & c),
        BinaryOp::BitOr => Some(a | c),
        BinaryOp::BitXor => Some(a ^ c),
        BinaryOp::Rotl => {
            let (x, n) = (mask(t, a), mask(t, c) % u64::from(w));
            Some(mask(t, x << n | x.checked_shr(w - n as u32).unwrap_or(0)))
        }
        BinaryOp::Min => Some(if x <= y { a } else { c }),
        BinaryOp::Max => Some(if x >= y { a } else { c }),
        BinaryOp::MinNumber | BinaryOp::MaxNumber => unreachable!("float only"),
    }
}

fn float_binary(t: Ty, op: BinaryOp, a: u64, c: u64) -> u64 {
    let (x, y) = (float(t, a), float(t, c));
    let r = match op {
        BinaryOp::Add => x + y,
        BinaryOp::Sub => x - y,
        BinaryOp::Mul => x * y,
        BinaryOp::Div => x / y,
        BinaryOp::Min | BinaryOp::Max if x.is_nan() || y.is_nan() => f64::NAN,
        BinaryOp::Min | BinaryOp::MinNumber => x.minimum_or_number(y, op == BinaryOp::Min),
        BinaryOp::Max | BinaryOp::MaxNumber => -(-x).minimum_or_number(-y, op == BinaryOp::Max),
        _ => unreachable!("integer only"),
    };
    // f32 arithmetic done in f64 is exact before the one rounding in `bits` for
    // + - * (24-bit significands); `/` is not used on f32 by any sample.
    bits(t, r)
}

trait MinimumOrNumber {
    fn minimum_or_number(self, y: f64, propagate: bool) -> f64;
}
impl MinimumOrNumber for f64 {
    /// IEEE 754-2019 minimum (`propagate`) or minimumNumber; -0 < +0.
    fn minimum_or_number(self, y: f64, propagate: bool) -> f64 {
        let x = self;
        if x.is_nan() || y.is_nan() {
            return if propagate {
                f64::NAN
            } else if x.is_nan() {
                y
            } else {
                x
            };
        }
        if x == y {
            return if x.is_sign_negative() { x } else { y };
        }
        x.min(y)
    }
}

/// The SHA-256 round constants (FIPS 180-4 section 4.2.2).
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// RFC 3174 sections 4 and 6.1. The evaluator always uses scalar rounds.
fn sha1_blocks(h: &mut [u32; 5], data: &[u8]) {
    let (blocks, tail) = data.as_chunks::<64>();
    let mut padding = [0u8; 128];
    padding[..tail.len()].copy_from_slice(tail);
    padding[tail.len()] = 0x80;
    let end = if tail.len() < 56 { 64 } else { 128 };
    padding[end - 8..end].copy_from_slice(&((data.len() as u64) << 3).to_be_bytes());

    for block in blocks.iter().chain(padding[..end].as_chunks::<64>().0) {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *h;
        for (i, word) in w.into_iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((b & c) | (!b & d), 0x5a827999),
                20..40 => (b ^ c ^ d, 0x6ed9eba1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            (a, b, c, d, e) = (next, a, b.rotate_left(30), c, d);
        }
        for (word, update) in h.iter_mut().zip([a, b, c, d, e]) {
            *word = word.wrapping_add(update);
        }
    }
}

/// `rt.sha256_blocks` (crate docs, "SHA-256"): FIPS 180-4 section 6.2.2 over
/// each 64-byte block of `data`, in order. The evaluator's own portable
/// form; the run time's is `hirsel_core_sha256_blocks`.
fn sha256_blocks(h: &mut [u32; 8], data: &[u8]) {
    for block in data.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = *h;
        for i in 0..64 {
            let [a, b, c, d, e, f, g, hh] = v;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    }
}

#[cfg(test)]
#[test]
fn ended_byte_limits_release_their_records() {
    use hirsel_guard::admission::{Owner, OwnerKind};

    let module = Module::default();
    let mut machine = Machine::new(&module, vec![], 64, &WorldInput::default());
    let meter = Owner::new(OwnerKind::Ledger);
    let measure = meter.enter();
    let mut peak = 0;
    // MEM-20: each zero-byte scope has ended with no surviving reference.
    // Only the root and the current scope need records, regardless of count.
    let root = machine.root().clone();
    for _ in 0..4096 {
        machine.admit(&root, 0).unwrap();
        let scope = Limit::new(0, Some(root.clone()), 0);
        peak = peak.max(meter.bytes());
        machine.end_limit(scope, None);
    }
    assert_eq!(root.used(), 0);
    assert_eq!(meter.bytes(), 0);
    eprintln!("peak live byte-limit record allocations: {peak} bytes");
    assert!(peak <= 1024, "limit records used {peak} bytes");

    // A live allocation pins both ended records through hand-over. Freeing the
    // block refunds exactly its surviving charge to the root.
    drop(measure);
    machine.admit(&root, 64).unwrap();
    let outer = Limit::new(64, Some(root.clone()), 0);
    machine.admit(&outer, 48).unwrap();
    let inner = Limit::new(48, Some(outer.clone()), 0);
    machine.admit(&inner, 8).unwrap();
    let block = machine.block(8, Some(inner.clone()), 8, None);
    machine.end_limit(inner, None);
    machine.end_limit(outer, None);
    assert_eq!(root.used(), 8);
    machine.free(block);
    assert_eq!(root.used(), 0);
}
