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

mod conn;
mod map;
mod simd;

pub use map::run_map_hash;
#[cfg(feature = "map-observation")]
pub use map::run_map_observed;

use crate::*;
use hirsel_core_ids::ContentHash;
use hirsel_core_ids::host_op::{DIR_READ_ONLY, needs_writable_dir};
use hirsel_core_layout::{
    Body as Shaped, Cleanup, FieldTy, Handle, Layout, RecipeRow, Repr, TypeRef, charge,
    cleanup_class, compute, field_size, map_entry,
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
    /// Executed source step charges.
    pub steps: u64,
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
    #[cfg(feature = "map-observation")]
    pub maps: Option<MapObservation>,
}

#[cfg(feature = "map-observation")]
#[derive(Debug, Default, PartialEq)]
pub struct MapObservation {
    pub fast_peak: u64,
    pub strong_peak: u64,
    pub strong_searches: u64,
    pub strong_groups: u64,
    pub promotions: u64,
    pub rebuilds: u64,
    pub rebuild_promotions: u64,
    pub resumed_installs: u64,
    pub last_word_strong: bool,
    groups: u64,
    rebuilding: bool,
    promoted_in_rebuild: bool,
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
    map_hash_key: [u8; 16],
    map_fast_salt: [u64; 2],
    map_strong_key: [u8; 16],
    #[cfg(feature = "map-observation")]
    maps: Option<MapObservation>,
    launch_target: (u64, u64),
    launch: Option<hirsel_core_ids::launch::Metadata>,
    args: Vec<u64>,
    stdin: std::collections::VecDeque<u8>,
    terminals: [bool; 3],
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
    /// Secret designations by key; a released one is `None` (TLS-5).
    secrets: Vec<Option<EvalSlot>>,
    /// Each sealed secret's charge, refunded once no designation holds its token.
    seals: Vec<(std::sync::Weak<()>, Limit, u64)>,
    tls_writes: BTreeMap<u64, (Name, bool, hirsel_core_net::scram::Binding)>,
    next_scram: u64,
    scrams: BTreeMap<u64, ([u8; 32], Limit)>,
    /// `provide` cells: cell `i`'s capability and its hand or trigger are both
    /// `CELL_BASE + i` (native hirsel-core-rt `provide`).
    cells: Vec<VirtualCell>,
    /// A served Dir's derived handles: handle `PORT_HANDLE + i` is the port
    /// and server id at `i`.
    port_handles: Vec<(usize, u64)>,
    /// A served Net's grants (in `network.grants`, so `net.narrow` works on
    /// them as on any Net) and their port.
    net_ports: BTreeMap<u64, usize>,
    exec_grants: Vec<(usize, hirsel_core_net::exec::ExecRules)>,
    exec_handles: BTreeMap<u64, Limit>,
    /// A served Net caller's socket and listener keys (direction bits clear):
    /// port, server id and the peer's port, which a handshake's rule checks
    /// (native `provide::Port::keys`, `Files::connected`).
    port_keys: BTreeMap<u64, (usize, u64, u16)>,
    /// A datagram retains the rule list of the grant that created it.
    datagrams: BTreeMap<u64, DatagramState>,
    next_port_key: u64,
}
const CELL_BASE: u64 = 0x80_0000;
const SCRAM_BASE: u64 = 1 << 60;
const PORT_HANDLE: u64 = 1 << 61;
const EXEC_GRANT: u64 = 1 << 58;
/// A port's `Served` end is `SERVED_BASE + i` for cell `i`; its call `id` is
/// `CALL_BASE + (i << 32) + id`.
const SERVED_BASE: u64 = 0x90_0000;
const CALL_BASE: u64 = 1 << 62;
/// A served Net caller's keys (native `provide::PORT_KEYS`): clear of the
/// host registry keys.
const PORT_KEYS: u64 = 1 << 33;
/// One call on a served port (native `provide::PortCall`). The caller's bytes
/// are copied at issue: they cannot change while it waits, and `payload`
/// answers them only while the call is unsettled, as natively.
struct PortCall {
    caller: usize,
    words: [u64; 9],
    /// The `Op`'s data fields by field number.
    fields: Vec<Vec<u8>>,
    /// The caller's carrier that receives the staged reply bytes.
    out: Option<u64>,
    staged: Vec<u8>,
    taken: bool,
    held: bool,
    answer: Option<Vec<u64>>,
    finished: bool,
    /// A SendSecret's Secrets, narrowed to the stream's name at issue.
    secret: Option<Vec<EvalSecret>>,
}
/// A served object's state (native `provide::State`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PortState {
    Live,
    Closing,
    Released(u64),
    Gone,
}
/// A served object (native `provide::Handle`): kind 0 Dir, 1 File, 2
/// Entries, 3 Socket, 4 Listener; a split socket's read half is `state`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct PortHandle {
    kind: u8,
    state: PortState,
    write: Option<PortState>,
    read_only_file: bool,
    termination: Option<u64>,
}
impl PortHandle {
    fn new(kind: u8) -> Self {
        Self {
            kind,
            state: PortState::Live,
            write: None,
            read_only_file: false,
            termination: None,
        }
    }
    fn done(&self) -> bool {
        self.state == PortState::Gone && self.write.is_none_or(|w| w == PortState::Gone)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Vocab {
    Stream,
    Dir,
    Net,
    Exec,
}
enum Queued {
    Call(u64),
    Released(u64, bool),
}
const DIR_CLOSE: u64 = 18;
const DIR_RELEASED: u64 = 20;
const NET_RELEASED: u64 = 8;
const NET_UNIX_CONNECT: u64 = 9;
const NET_UNIX_LISTEN: u64 = 10;
const NET_UNIX_ACCEPT: u64 = 11;
const NET_DATAGRAM_OPEN: u64 = 12;
const NET_DATAGRAM_BIND: u64 = 13;
const NET_DATAGRAM_CONNECT: u64 = 14;
const NET_DATAGRAM_SEND: u64 = 15;
const NET_DATAGRAM_SEND_TO: u64 = 16;
const NET_DATAGRAM_RECV: u64 = 17;
const NET_READY: u64 = 18;
const PORT_SOCKET: u8 = 3;
const PORT_UNIX_SOCKET: u8 = 5;
const PORT_UNIX_LISTENER: u8 = 6;
const PORT_DATAGRAM: u8 = 7;
const PORT_PROC: u8 = 8;
const PORT_PIPE_READ: u8 = 9;
const PORT_PIPE_WRITE: u8 = 10;
use hirsel_core_net::exec::served as exec;

#[derive(Clone, Copy)]
struct DatagramState {
    grant: u64,
    bound: bool,
    connected: bool,
}
fn is_notice(vocab: Vocab, op: u64) -> bool {
    match vocab {
        Vocab::Stream => false,
        Vocab::Dir => op == DIR_RELEASED,
        Vocab::Net => op == NET_RELEASED,
        Vocab::Exec => op == exec::RELEASED,
    }
}
const HANDLE_ROOM: u64 = 32;
enum VirtualCell {
    Clock {
        mono: u64,
        wall: u64,
        /// Tasks waiting on a deadline of this clock, in registration order.
        timers: Vec<(u64, usize)>,
        /// Tasks waiting on this clock's wall time, as `timers`.
        wall_timers: Vec<(u64, usize)>,
        /// Tasks in `clock.idle`, and whether quiescence has woken each.
        idle: Vec<(usize, bool)>,
    },
    Random(u64),
    Process {
        stopped: bool,
        waiters: Vec<usize>,
    },
    Port {
        vocab: Vocab,
        terminal: bool,
        sealed: bool,
        /// The order of calls and Released notices.
        next: u64,
        calls: BTreeMap<u64, PortCall>,
        /// Tasks in `next`, and whether quiescence answered each None.
        servers: Vec<(usize, bool)>,
        handles: BTreeMap<u64, PortHandle>,
        room: u64,
        used: u64,
    },
}
impl VirtualCell {
    fn port(
        &mut self,
    ) -> (
        &mut BTreeMap<u64, PortCall>,
        &mut BTreeMap<u64, PortHandle>,
        &mut u64,
        &mut u64,
        Vocab,
    ) {
        let VirtualCell::Port {
            calls,
            handles,
            used,
            next,
            vocab,
            ..
        } = self
        else {
            unreachable!("a port")
        };
        (calls, handles, used, next, *vocab)
    }
    /// The oldest queued call or Released notice.
    fn queued(&self) -> Option<Queued> {
        let VirtualCell::Port { calls, handles, .. } = self else {
            unreachable!("a port")
        };
        let call = calls
            .iter()
            .find(|(_, c)| !c.taken && !c.finished && c.answer.is_none())
            .map(|(&id, _)| id);
        let released = handles
            .iter()
            .flat_map(|(&h, r)| {
                let read = match r.state {
                    PortState::Released(at) => Some((at, h, false)),
                    _ => None,
                };
                let write = match r.write {
                    Some(PortState::Released(at)) => Some((at, h, true)),
                    _ => None,
                };
                read.into_iter().chain(write)
            })
            .min();
        match (call, released) {
            (Some(c), Some((at, h, w))) if at < c => Some(Queued::Released(h, w)),
            (Some(c), _) => Some(Queued::Call(c)),
            (None, Some((_, h, w))) => Some(Queued::Released(h, w)),
            (None, None) => None,
        }
    }
    fn free_staged(&mut self, id: u64) {
        let (calls, _, used, _, _) = self.port();
        if let Some(call) = calls.get_mut(&id) {
            *used -= call.staged.len() as u64;
            call.staged = Vec::new();
        }
    }
    /// A Close call ends: a server that took it knows the file is gone; one
    /// that never saw it learns by `Released`.
    fn closed_call(&mut self, id: u64) {
        let (calls, handles, used, next, vocab) = self.port();
        let Some(call) = calls.get(&id) else {
            return;
        };
        if vocab != Vocab::Dir || call.words[0] != DIR_CLOSE {
            return;
        }
        let file = call.words[1];
        if call.taken {
            if handles.remove(&file).is_some() {
                *used -= HANDLE_ROOM;
            }
        } else if let Some(h) = handles.get_mut(&file) {
            h.state = PortState::Released(*next);
            *next += 1;
        }
    }
    /// Settle an unsettled call; returns its caller.
    fn settle(&mut self, id: u64, answer: Vec<u64>) -> Option<usize> {
        let (calls, ..) = self.port();
        let call = calls.get_mut(&id)?;
        if call.answer.is_some() || call.finished {
            return None;
        }
        call.answer = Some(answer);
        let caller = call.caller;
        self.free_staged(id);
        self.closed_call(id);
        Some(caller)
    }
    fn prune(&mut self, id: u64) {
        let (calls, ..) = self.port();
        if calls.get(&id).is_some_and(|c| c.finished && !c.held) {
            self.free_staged(id);
            self.port().0.remove(&id);
        }
    }
}
/// A sealed port's answer: `Closed`, or a Net's `Reset`.
fn port_closed(vocab: Vocab, op: u64) -> [u64; 5] {
    let error = match vocab {
        Vocab::Net => 5,
        Vocab::Exec if !exec::exec_error_op(op) => 5,
        _ => 4,
    };
    [0, error, 0, 0, 0]
}
/// A port outcome shaped as the host model's row for the call's operation:
/// a Stat's carries the kind, a Net Listen's or Accept's an address.
fn port_outcome(vocab: Vocab, op: u64, v: [u64; 5]) -> Vec<u64> {
    match (vocab, op) {
        (Vocab::Dir, 3 | 14) => v.to_vec(),
        (Vocab::Net, 2 | 3 | 6) => [&v[..4], &[0; 4]].concat(),
        (Vocab::Exec, exec::SPAWN) => [&v[..4], &[0; 3]].concat(),
        _ => v[..4].to_vec(),
    }
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
    /// The seal's token, shared by every designation of the bytes.
    seal: Arc<()>,
}

struct EvalSlot {
    secret: EvalSecret,
    holders: u64,
    charge: (Limit, u64),
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
    read_only_dir: bool,
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
        dir &= !DIR_READ_ONLY;
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
        let dir = dir & !DIR_READ_ONLY;
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

    /// Charge what a crossing brings in, already checked against the cap.
    fn add(&self, n: u64) {
        self.0.state.lock().unwrap().used += n;
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
    unit_owner: Option<usize>,
    end_taken: bool,
    crossed_result: Option<conn::Item>,
    /// A helper's own limit from its end until its owner's end take.
    unit_limit: Option<Limit>,
    /// What a helper's end take kept of the spawn's charge for its join:
    /// `size(R)` and the result's content.
    end_kept: u64,
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
    recipes: Vec<RecipeRow>,
    mem: Blocks,
    root: Option<Limit>,
    next_step_bound: u64,
    peak_reserved: u64,
    regions: Vec<Vec<usize>>,
    children: Vec<Child>,
    chans: Vec<Chan>,
    conns: Vec<Conn>,
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
    steps: u64,
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

use conn::{CONN_BASE, Conn};
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
    ConnSend(usize),
    ConnReceive(usize),
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
    unit: usize,
    notices: VecDeque<conn::UnitNotice>,
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
    /// provider-parked tasks, then expired timers in deadline order.
    fn take_wakes(&mut self, now: u64) {
        for k in std::mem::take(&mut self.cancels) {
            let r = &mut self.tasks[k];
            if r.state == State::Provider && r.cancel && r.cancellable {
                r.cancelled = true;
                r.state = State::Ready;
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
/// process entry does. The limit must cover startup. May record CONC-10 events.
/// A cause's site word is `sites[item] + start` (`SourceMap::item_sites` over the
/// module's items); an empty `sites` means the module's own (`source::module_sites`).
/// With `error_text`, a returned `Error(e)` is rendered as PROG-3 says.
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
    events: bool,
    sites: &[u64],
    launch: Option<&hirsel_core_ids::launch::Metadata>,
    error_text: Option<ErrorText<'_>>,
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
            events,
            process_entry: true,
            sites,
            launch,
            error_text,
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
#[derive(Default)]
pub struct Network {
    pub grants: BTreeMap<u64, Vec<hirsel_core_net::GrantRule>>,
    pub sockets: hirsel_core_net::Sockets,
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
/// Evaluate a host launch with supplied input and environment snapshots. The
/// granted standard descriptors are probed once, before the evaluator starts.
pub fn run_host_stdio(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    stdin: &[u8],
    env: Option<hirsel_core_net::EnvSnapshot>,
    granted: [bool; 3],
) -> Report {
    use std::io::IsTerminal;
    let terminals = [
        granted[0] && std::io::stdin().is_terminal(),
        granted[1] && std::io::stdout().is_terminal(),
        granted[2] && std::io::stderr().is_terminal(),
    ];
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            stdin,
            env,
            terminals,
            ..WorldInput::default()
        },
    )
    .0
}

struct WorldInput<'a> {
    map_hash_key: Option<[u8; 16]>,
    #[cfg(feature = "map-observation")]
    observe_maps: bool,
    network: Option<Network>,
    network_out: Option<&'a mut Network>,
    launch: Option<&'a hirsel_core_ids::launch::Metadata>,
    revisions: Option<&'a Revisions>,
    injection: Option<(usize, bool)>,
    argv: &'a [&'a str],
    env: Option<hirsel_core_net::EnvSnapshot>,
    stdin: &'a [u8],
    terminals: [bool; 3],
    nans: Nans,
    destination: Option<&'a mut [u8]>,
    instructions: u64,
    copy_size: Option<u64>,
    events: bool,
    process_entry: bool,
    sites: &'a [u64],
    /// PROG-3: render `main`'s `Error(e)` with this program, before the run's
    /// memory is released, into the slot.
    error_text: Option<ErrorText<'a>>,
}
/// The entry's text program for `Error(e)` and where its text goes (PROG-3).
pub type ErrorText<'a> = (&'a hirsel_core_layout::show::Show, &'a mut Option<String>);
impl Default for WorldInput<'_> {
    fn default() -> Self {
        Self {
            network: None,
            map_hash_key: None,
            #[cfg(feature = "map-observation")]
            observe_maps: false,
            network_out: None,
            launch: None,
            revisions: None,
            injection: None,
            argv: &[],
            env: None,
            stdin: &[],
            terminals: [false; 3],
            nans: Nans::Canonical,
            destination: None,
            instructions: u64::MAX,
            copy_size: None,
            events: false,
            process_entry: false,
            sites: &[],
            error_text: None,
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
    mach.world.terminals = input.terminals;
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
        unit: 0,
        notices: VecDeque::new(),
    });
    mach.sched.ready.push_back(0);
    let (mut mach, outcome, chain) = drive(mach, input.process_entry);
    if let (Some((show, text)), Some(at), Outcome::Returned(_)) =
        (input.error_text.take(), destination, &outcome)
        && let Some(dest) = signature.dest
        && let Layout::Fixed(fixed) = &mach.recipes[dest.index()].layout
        && let Some(tag) = fixed.tag
        && hirsel_core_layout::case_of(&tag, mach.load(at + tag.offset, tag.width as usize)) == 1
    {
        struct Blocks<'b, 'm>(&'b Machine<'m>);
        impl hirsel_core_layout::show::Memory for Blocks<'_, '_> {
            fn read(&self, at: u64, out: &mut [u8]) {
                out.copy_from_slice(self.0.get(at, out.len()));
            }
        }
        *text = Some(show.render(at, &Blocks(&mach)));
    }
    if let Some(bytes) = input.destination.as_mut() {
        bytes.copy_from_slice(&mach.mem[split(destination.unwrap()).0].bytes);
    }
    for (l, n) in std::mem::take(&mut mach.world.grant_charges) {
        l.find().subtract(n);
    }
    for slot in std::mem::take(&mut mach.world.secrets)
        .into_iter()
        .flatten()
    {
        slot.charge.0.find().subtract(slot.charge.1);
    }
    for (_, l, n) in std::mem::take(&mut mach.world.seals) {
        l.find().subtract(n);
    }
    // Refunds still pending when the run ends count toward its final use.
    for k in 0..mach.sched.tasks.len() {
        for (l, n) in std::mem::take(&mut mach.sched.tasks[k].refunds) {
            l.find().subtract(n);
        }
        for notice in std::mem::take(&mut mach.sched.tasks[k].notices) {
            if let conn::UnitNotice::Refund(l, n) = notice {
                l.find().subtract(n);
            }
        }
    }
    if let Some(remaining) = input.network_out {
        *remaining = std::mem::take(&mut mach.world.network);
    }
    let sites = chain.iter().map(|c| c.site).collect();
    let checked = Checked {
        steps: mach.steps,
        instructions: input.instructions - mach.instructions,
        loans: mach.loans,
        roles: mach.role_checks,
        copies: mach.copies,
        task_stack_peak: mach.task_stack_peak,
        task_call_depth: mach.task_call_depth,
        #[cfg(feature = "map-observation")]
        maps: mach.world.maps.take(),
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
            let s = &mut baton.sched;
            s.take_wakes(baton.world.clock);
            if s.ready.is_empty() {
                std::mem::swap(&mut s.ready, &mut s.deferred);
            }
            if s.ready.is_empty()
                && let Some(&(at, _)) = s.timers.first()
            {
                baton.world.clock = at;
                s.take_wakes(at);
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
            let start = s.tasks[k].start.take();
            baton.take_unit_notices();
            if resumes.len() <= k {
                resumes.resize_with(k + 1, || None);
            }
            if let Some((task, f, args)) = start {
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
    fn new(m: &'m Module, recipes: Vec<RecipeRow>, limit: u64, input: &WorldInput<'_>) -> Self {
        let map_hash_key = input.map_hash_key.unwrap_or_else(map::fresh_key);
        let (s0, s1, map_strong_key) = hirsel_core_layout::map_index::derive(&map_hash_key);
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
            conns: Vec::new(),
            latests: Vec::new(),
            causes: HashMap::new(),
            arrays: Vec::new(),
            trace: Vec::new(),
            dests: 0,
            admissions: 0,
            instructions: input.instructions,
            steps: 0,
            injection: input.injection,
            world: World {
                map_hash_key,
                map_fast_salt: [s0, s1],
                map_strong_key,
                #[cfg(feature = "map-observation")]
                maps: input.observe_maps.then(MapObservation::default),
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
            conns: Vec::new(),
            latests: Vec::new(),
            causes: HashMap::new(),
            arrays: Vec::new(),
            trace: Vec::new(),
            dests: 0,
            admissions: 0,
            instructions: 0,
            steps: 0,
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
    fn remove_cell_timer(&mut self, c: usize, k: usize) {
        if let VirtualCell::Clock { timers, .. } = &mut self.world.cells[c] {
            timers.retain(|&(_, task)| task != k);
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
            if let VirtualCell::Clock {
                timers,
                wall_timers,
                idle,
                ..
            } = cell
            {
                timers.retain(|&(_, k)| parked(k));
                wall_timers.retain(|&(_, k)| parked(k));
                for (k, flag) in idle.iter_mut() {
                    if !*flag && parked(*k) {
                        *flag = true;
                        woken.push(*k);
                    }
                }
            }
        }
        if woken.is_empty() {
            // No idle wait: a `next` with nothing queued answers None.
            for cell in &mut self.world.cells {
                if let VirtualCell::Port { .. } = cell
                    && cell.queued().is_none()
                    && let VirtualCell::Port { servers, .. } = cell
                {
                    for (k, flag) in servers.iter_mut() {
                        if !*flag && parked(*k) {
                            *flag = true;
                            woken.push(*k);
                        }
                    }
                }
            }
        }
        for &k in &woken {
            self.ready_provider(k);
        }
        !woken.is_empty()
    }

    /// The port and call id a `Call` handle designates.
    fn call_of(h: u64) -> (usize, u64) {
        let i = h - CALL_BASE;
        ((i >> 32) as usize, i & 0xffff_ffff)
    }

    /// A caller's operation on a served port: queue it, ready the servers and
    /// wait for its answer. A cancellation that takes effect before the
    /// answer settles the call and fails Cancelled. `live` is whether the
    /// call names one of the port's live objects of its operation's kind.
    fn port_call(
        &mut self,
        t: &Task,
        c: usize,
        words: [u64; 9],
        fields: Vec<Vec<u8>>,
        out: Option<u64>,
        live: bool,
    ) -> X<Vec<u64>> {
        self.port_call_with(t, c, words, fields, out, live, &mut None, None)
    }

    /// `port_call` for a Net call, with its `until` (NOTES 35) and, for a
    /// SendSecret, its narrowed Secret. A deadline already reached times out
    /// before queueing; one reached while the call waits settles it TimedOut,
    /// and a later reply is Withdrawn.
    #[allow(clippy::too_many_arguments)]
    fn port_call_with(
        &mut self,
        t: &Task,
        c: usize,
        words: [u64; 9],
        fields: Vec<Vec<u8>>,
        out: Option<u64>,
        live: bool,
        secret: &mut Option<Vec<EvalSecret>>,
        deadline: Option<Due>,
    ) -> X<Vec<u64>> {
        let VirtualCell::Port { vocab, sealed, .. } = &self.world.cells[c] else {
            unreachable!()
        };
        let vocab = *vocab;
        if *sealed {
            return Ok(port_outcome(vocab, words[0], port_closed(vocab, words[0])));
        }
        if !live {
            // Not this port's live object of the operation's kind: Denied
            // (IoError tag 2, NetError tag 1).
            let denied = if matches!(vocab, Vocab::Net | Vocab::Exec) {
                1
            } else {
                DENIED
            };
            return Ok(port_outcome(vocab, words[0], [0, denied, 0, 0, 0]));
        }
        let exec_error = vocab == Vocab::Exec && exec::exec_error_op(words[0]);
        let deadline_error = if exec_error { 2 } else { 6 };
        if deadline.is_some_and(|due| self.passed(due)) {
            return Ok(port_outcome(vocab, words[0], [0, deadline_error, 0, 0, 0]));
        }
        let VirtualCell::Port {
            next,
            calls,
            servers,
            handles,
            ..
        } = &mut self.world.cells[c]
        else {
            unreachable!()
        };
        if vocab == Vocab::Dir && words[0] == DIR_CLOSE {
            if let Some(h) = handles.get_mut(&words[1]) {
                h.state = PortState::Closing;
            }
        }
        let id = *next;
        *next += 1;
        calls.insert(
            id,
            PortCall {
                caller: t.k,
                words,
                fields,
                out,
                staged: Vec::new(),
                taken: false,
                held: false,
                answer: None,
                finished: false,
                secret: secret.take(),
            },
        );
        let servers: Vec<usize> = servers.iter().map(|&(k, _)| k).collect();
        for k in servers {
            self.ready_provider(k);
        }
        loop {
            let (calls, ..) = self.world.cells[c].port();
            let call = calls.get_mut(&id).unwrap();
            if let Some(answer) = call.answer.clone() {
                call.finished = true;
                self.world.cells[c].prune(id);
                return Ok(answer);
            }
            if deadline.is_some_and(|due| self.passed(due)) {
                let cell = &mut self.world.cells[c];
                let call = cell.port().0.get_mut(&id).unwrap();
                call.finished = true;
                cell.free_staged(id);
                cell.closed_call(id);
                cell.prune(id);
                return Ok(port_outcome(vocab, words[0], [0, deadline_error, 0, 0, 0]));
            }
            self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
            let host = match deadline {
                Some(Due::Host(at)) => Some(at),
                Some(Due::Cell(clock, at)) => {
                    self.cell_timer(clock, at, t.k);
                    None
                }
                None => None,
            };
            self.park(Some(Wait::Network(host)));
            if let Some(Due::Cell(clock, _)) = deadline {
                self.remove_cell_timer(clock, t.k);
            }
            if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                let cell = &mut self.world.cells[c];
                let call = cell.port().0.get_mut(&id).unwrap();
                if call.answer.is_none() {
                    call.finished = true;
                    cell.free_staged(id);
                    cell.closed_call(id);
                    cell.prune(id);
                    if matches!(vocab, Vocab::Net | Vocab::Exec) {
                        let aborted = if exec_error { 3 } else { 7 };
                        return Ok(port_outcome(vocab, words[0], [0, aborted, 0, 0, 0]));
                    }
                    self.check_cancel(t)?;
                }
            }
        }
    }

    /// A served Dir's port and server id for a Dir, HostFile or HostEntries
    /// handle: the root is the cell's own capability.
    fn port_handle(&self, h: u64) -> Option<(usize, u64)> {
        if (PORT_HANDLE..CALL_BASE).contains(&h) {
            return Some(self.world.port_handles[(h - PORT_HANDLE) as usize]);
        }
        let c = self.cell(h)?;
        matches!(
            self.world.cells[c],
            VirtualCell::Port {
                vocab: Vocab::Dir,
                ..
            }
        )
        .then_some((c, 0))
    }

    /// A host Dir, file or listing operation on a served Dir's handle: its
    /// words and data fields (native `Port::words`, `field_bytes`).
    fn dir_call(&mut self, t: &Task, op: HostOp, c: usize, h: u64, a: &[u64]) -> X<Vec<u64>> {
        self.check_cancel(t).inspect_err(|_| {
            // A close cancelled before issue still ends the file: the server
            // learns by Released (native `host_file` CLOSE under a shield).
            if op == HostOp::Close {
                self.port_release(a[0]);
            }
        })?;
        if matches!(op, HostOp::Write | HostOp::WriteAt)
            && let VirtualCell::Port { handles, .. } = &self.world.cells[c]
            && handles
                .get(&h)
                .is_some_and(|file| file.read_only_file && file.state == PortState::Live)
        {
            return Ok(vec![0, DENIED, 0, 0]);
        }
        let spare = |m: &Self, carrier: u64| {
            let (_, _, len, cap) = m.carrier(carrier);
            cap.saturating_sub(len)
        };
        // The caller-side checks of every provider's entry (native `drive`).
        match op {
            HostOp::Mkdtemp if spare(self, a[1]) < 32 => return Err(Stop::Cause("Bounds")),
            HostOp::NextEntries if spare(self, a[1]) < 258 => return Err(Stop::Cause("Bounds")),
            HostOp::WriteAt
                if a[1]
                    .checked_add(self.carrier_bytes(a[2]).len() as u64)
                    .is_none() =>
            {
                return Err(Stop::Cause("Bounds"));
            }
            _ => {}
        }
        let path = |m: &Self, i: usize| m.carrier_bytes(a[i]).to_vec();
        let (kind, words, fields, out): (u8, Vec<u64>, Vec<Vec<u8>>, Option<u64>) = match op {
            HostOp::Open => {
                let p = path(self, 1);
                (0, vec![0, h, p.len() as u64, a[2]], vec![vec![], p], None)
            }
            HostOp::OpenDir
            | HostOp::Stat
            | HostOp::Mkdir
            | HostOp::Remove
            | HostOp::RemoveTree => {
                let tag = match op {
                    HostOp::OpenDir => 1,
                    HostOp::Stat => 3,
                    HostOp::Mkdir => 4,
                    HostOp::Remove => 5,
                    _ => 6,
                };
                let p = path(self, 1);
                (0, vec![tag, h, p.len() as u64], vec![vec![], p], None)
            }
            HostOp::Mkdtemp => {
                let (prefix, ..) = self.carrier(a[1]);
                let room = spare(self, a[1]);
                (
                    0,
                    vec![2, h, prefix.len() as u64, room],
                    vec![vec![], prefix],
                    Some(a[1]),
                )
            }
            HostOp::Rename => {
                let (from, to) = (path(self, 1), path(self, 2));
                (
                    0,
                    vec![7, h, from.len() as u64, to.len() as u64],
                    vec![vec![], from, to],
                    None,
                )
            }
            HostOp::SyncDir => (0, vec![8, h], vec![], None),
            HostOp::Entries => (0, vec![9, h], vec![], None),
            HostOp::Read => (1, vec![10, h, spare(self, a[1])], vec![], Some(a[1])),
            HostOp::ReadAt => (1, vec![11, h, a[1], spare(self, a[2])], vec![], Some(a[2])),
            HostOp::Write => {
                let (b, ..) = self.carrier(a[1]);
                (1, vec![12, h, b.len() as u64], vec![vec![], b], None)
            }
            HostOp::WriteAt => {
                let (b, ..) = self.carrier(a[2]);
                (
                    1,
                    vec![13, h, a[1], b.len() as u64],
                    vec![vec![], vec![], b],
                    None,
                )
            }
            HostOp::StatFile => (1, vec![14, h], vec![], None),
            HostOp::Sync => (1, vec![15, h], vec![], None),
            HostOp::TryLock => (1, vec![16, h], vec![], None),
            HostOp::Unlock => (1, vec![17, h], vec![], None),
            HostOp::Close => (1, vec![DIR_CLOSE, h], vec![], None),
            HostOp::NextEntries => (2, vec![19, h, spare(self, a[1])], vec![], Some(a[1])),
            _ => unreachable!("a Dir, file or listing operation"),
        };
        let VirtualCell::Port { handles, .. } = &self.world.cells[c] else {
            unreachable!()
        };
        let live = handles.get(&h).is_some_and(|handle| {
            handle.kind == kind && handle.state == PortState::Live && handle.write.is_none()
        });
        let mut w = [0; 9];
        w[..words.len()].copy_from_slice(&words);
        let mut answer = self.port_call(t, c, w, fields, out, live)?;
        if a[0] & DIR_READ_ONLY != 0 && answer[1] == 0 {
            if matches!(op, HostOp::OpenDir | HostOp::Mkdtemp) {
                answer[0] |= DIR_READ_ONLY;
            } else if op == HostOp::Open {
                let (_, file) = self.port_handle(answer[0]).expect("a registered file");
                let (_, handles, ..) = self.world.cells[c].port();
                handles.get_mut(&file).unwrap().read_only_file = true;
            }
        }
        Ok(answer)
    }

    /// A dropped HostFile or HostEntries of a served Dir (FAIL-13): marked
    /// Released, allocating nothing; `next` delivers the notice.
    fn port_release(&mut self, h: u64) {
        let Some((c, id)) = self.port_handle(h) else {
            return;
        };
        let VirtualCell::Port {
            next,
            handles,
            servers,
            ..
        } = &mut self.world.cells[c]
        else {
            unreachable!()
        };
        let Some(handle) = handles.get_mut(&id).filter(|h| h.state == PortState::Live) else {
            return;
        };
        handle.state = PortState::Released(*next);
        *next += 1;
        let servers: Vec<usize> = servers.iter().map(|&(k, _)| k).collect();
        for k in servers {
            self.ready_provider(k);
        }
    }

    /// `provide.next`: the oldest queued call, or None once quiescence answers.
    fn port_next(&mut self, t: &Task, c: usize) -> X<Vec<u64>> {
        self.check_cancel(t)?;
        loop {
            let queued = self.world.cells[c].queued();
            let VirtualCell::Port {
                vocab,
                calls,
                servers,
                handles,
                used,
                next,
                ..
            } = &mut self.world.cells[c]
            else {
                unreachable!()
            };
            let quiesced = servers.iter().any(|&(k, q)| k == t.k && q);
            if queued.is_some() || quiesced {
                servers.retain(|&(k, _)| k != t.k);
                let id = match queued {
                    None => return Ok(vec![0, 0, 0, 0]),
                    Some(Queued::Call(id)) => {
                        let call = calls.get_mut(&id).unwrap();
                        call.taken = true;
                        call.held = true;
                        id
                    }
                    Some(Queued::Released(h, write)) => {
                        // The notice's record leaves the room with the last
                        // part's notice, and becomes a Call that needs no reply.
                        let handle = handles.get_mut(&h).unwrap();
                        let part = match (handle.kind, handle.write, write) {
                            (PORT_PIPE_READ, _, _) | (_, Some(_), false) => 1,
                            (PORT_PIPE_WRITE, _, _) | (_, Some(_), true) => 2,
                            _ => 0,
                        };
                        if write {
                            handle.write = Some(PortState::Gone);
                        } else {
                            handle.state = PortState::Gone;
                        }
                        if handle.done() {
                            handles.remove(&h);
                            *used -= HANDLE_ROOM;
                        }
                        let id = *next;
                        *next += 1;
                        let mut words = [0; 9];
                        match *vocab {
                            Vocab::Net => words[..3].copy_from_slice(&[NET_RELEASED, h, part]),
                            Vocab::Exec => words[..3].copy_from_slice(&[exec::RELEASED, h, part]),
                            _ => words[..2].copy_from_slice(&[DIR_RELEASED, h]),
                        }
                        calls.insert(
                            id,
                            PortCall {
                                caller: t.k,
                                words,
                                fields: Vec::new(),
                                out: None,
                                staged: Vec::new(),
                                taken: true,
                                held: true,
                                answer: None,
                                finished: true,
                                secret: None,
                            },
                        );
                        id
                    }
                };
                return Ok(vec![CALL_BASE + ((c as u64) << 32) + id, 0, 0, 1]);
            }
            if !servers.iter().any(|&(k, _)| k == t.k) {
                servers.push((t.k, false));
            }
            self.sched.tasks[t.k].cancellable = t.shield == 0 && t.obligations.len() == 1;
            self.park(Some(Wait::Network(None)));
            if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                if let VirtualCell::Port { servers, .. } = &mut self.world.cells[c] {
                    servers.retain(|&(k, _)| k != t.k);
                }
                self.check_cancel(t)?;
            }
        }
    }

    /// A `Call` dropped without a reply, or the `Served` end cleaned (sealing
    /// the port): settle as sealed.
    fn port_drop(&mut self, h: u64) {
        let mut callers = Vec::new();
        if h >= CALL_BASE {
            let (c, id) = Self::call_of(h);
            let cell = &mut self.world.cells[c];
            if let Some(call) = cell.port().0.get_mut(&id) {
                call.held = false;
            }
            let op = cell.port().0.get(&id).map_or(0, |call| call.words[0]);
            let vocab = cell.port().4;
            callers.extend(cell.settle(id, port_outcome(vocab, op, port_closed(vocab, op))));
            cell.prune(id);
        } else {
            let c = (h - SERVED_BASE) as usize;
            let cell = &mut self.world.cells[c];
            let VirtualCell::Port {
                sealed,
                calls,
                vocab,
                ..
            } = cell
            else {
                unreachable!()
            };
            *sealed = true;
            let vocab = *vocab;
            let ids: Vec<(u64, u64)> = calls.iter().map(|(&id, c)| (id, c.words[0])).collect();
            for (id, op) in ids {
                callers.extend(cell.settle(id, port_outcome(vocab, op, port_closed(vocab, op))));
            }
        }
        for k in callers {
            self.ready_provider(k);
        }
    }

    /// The caller's outcome for a reply that answers its call, and the kind
    /// of the object it issues; None when the reply breaks the port's rules
    /// (native `provide::answer`).
    fn port_answer(
        &self,
        c: usize,
        id: u64,
        tag: u64,
        w: &[u64],
    ) -> Option<([u64; 5], Option<u8>)> {
        let VirtualCell::Port {
            vocab,
            calls,
            handles,
            ..
        } = &self.world.cells[c]
        else {
            unreachable!()
        };
        let call = &calls[&id];
        let op = call.words[0];
        if *vocab == Vocab::Net {
            return Self::net_answer(call, handles, tag, w);
        }
        if *vocab == Vocab::Exec {
            let [value, error, code, written] =
                exec::answer(&call.words, call.staged.len() as u64, tag, w, |id| {
                    handles.contains_key(&id)
                })?;
            return Some(([value, error, code, written, 0], None));
        }
        let dir = *vocab == Vocab::Dir;
        let answers = if !dir {
            matches!(tag, 0 | 2)
        } else {
            (tag == 1 && op != DIR_RELEASED)
                || match op {
                    0 => tag == 6,
                    1 | 2 => tag == 7,
                    3 | 14 => tag == 5,
                    9 => tag == 8,
                    10 | 11 | 19 => tag == 3,
                    12 | 13 => matches!(tag, 0 | 2),
                    16 => tag == 4,
                    _ => tag == 0,
                }
        };
        if !answers {
            return None;
        }
        let io = |t: u64| (1..=5).contains(&t).then_some(t);
        let len = match (dir, op) {
            (false, _) => call.words[1],
            (true, 12) => call.words[2],
            (true, 13) => call.words[3],
            _ => 0,
        };
        Some(match tag {
            0 => ([0, 0, 0, len, 0], None),
            1 => ([0, io(w[0])?, w[1] & 0xffff_ffff, 0, 0], None),
            2 => {
                if w[0] > len {
                    return None;
                }
                ([0, io(w[1])?, w[2] & 0xffff_ffff, w[0], 0], None)
            }
            3 => {
                if op != 19 && w[0] != call.staged.len() as u64 {
                    return None;
                }
                ([w[0], 0, 0, 0, 0], None)
            }
            4 => ([u64::from(w[0] != 0), 0, 0, 0, 0], None),
            5 => ([w[1], 0, 0, w[2], w[0].min(3)], None),
            _ => {
                if w[0] == 0 || handles.contains_key(&w[0]) {
                    return None;
                }
                (
                    [w[0], 0, 0, 0, 0],
                    Some(match tag {
                        6 => 1,
                        7 => 0,
                        _ => 2,
                    }),
                )
            }
        })
    }

    /// A served Net's answer (native `provide::answer`): the row and the kind
    /// of a new socket (3) or listener (4).
    fn net_answer(
        call: &PortCall,
        handles: &BTreeMap<u64, PortHandle>,
        tag: u64,
        w: &[u64],
    ) -> Option<([u64; 5], Option<u8>)> {
        let op = call.words[0];
        let answers = (tag == 1 && op != NET_RELEASED)
            || match op {
                0 | 3 => tag == 9,
                2 => tag == 10,
                1 | 4 | 5 => tag == 3,
                6 => tag == 11,
                7 | NET_DATAGRAM_CONNECT | NET_DATAGRAM_SEND | NET_DATAGRAM_SEND_TO | NET_READY => {
                    tag == 0
                }
                NET_UNIX_CONNECT | NET_UNIX_ACCEPT => tag == 12,
                NET_UNIX_LISTEN => tag == 13,
                NET_DATAGRAM_OPEN | NET_DATAGRAM_BIND => tag == 14,
                NET_DATAGRAM_RECV => tag == 15,
                _ => false,
            };
        if !answers {
            return None;
        }
        Some(match tag {
            0 => ([0; 5], None),
            11 => {
                // Some(i) names one of the offered protocols; None is (0, 0).
                let mut wire = &call.fields[2][..];
                let mut offered = 0;
                while let Some((&len, rest)) = wire.split_first() {
                    wire = rest.get(usize::from(len)..).unwrap_or_default();
                    offered += 1;
                }
                if !matches!((w[0], w[1]), (0, 0)) && !(w[0] == 1 && w[1] < offered) {
                    return None;
                }
                ([0, 0, 0, w[0] + w[1], 0], None)
            }
            1 => {
                let max = if op == NET_UNIX_LISTEN { 10 } else { 9 };
                (1..=max).contains(&w[0]).then_some(())?;
                ([0, w[0], w[1] & 0xffff_ffff, 0, 0], None)
            }
            3 => {
                let fits = if op == 5 {
                    w[0] <= call.words[2] + call.words[3]
                } else if op == 1 {
                    // Well-formed, distinct address records (NOTES 34).
                    hirsel_core_net::ip_records(&call.staged)
                        .is_some_and(|records| w[0] == records.len() as u64)
                } else {
                    w[0] == call.staged.len() as u64
                };
                fits.then_some(())?;
                ([w[0], 0, 0, 0, 0], None)
            }
            15 => {
                let room = call.words[2];
                if w[1] != u64::from(w[0] > room) || w[0].min(room) != call.staged.len() as u64 {
                    return None;
                }
                ([w[0], 0, 0, w[1], 0], None)
            }
            12..=14 => {
                if w[0] == 0 || handles.contains_key(&w[0]) {
                    return None;
                }
                let kind = match tag {
                    12 => PORT_UNIX_SOCKET,
                    13 => PORT_UNIX_LISTENER,
                    _ => PORT_DATAGRAM,
                };
                ([w[0], 0, 0, 0, 0], Some(kind))
            }
            _ => {
                let v4 = w[1] == 0 && w[2] == 0 && w[3] <= u64::from(u32::MAX);
                if w[0] == 0 || handles.contains_key(&w[0]) || !(v4 || w[1] == 1) || w[4] > 0xffff {
                    return None;
                }
                ([w[0], 0, 0, 0, 0], Some(if tag == 9 { 3 } else { 4 }))
            }
        })
    }

    /// `provide.reply`'s commit (native `hirsel_core_port_reply`): value 1 is
    /// Withdrawn (NOTES 16, 20); a reply that breaks the port's rules settles
    /// the caller as sealed and fails the server; a new handle the room
    /// cannot hold is a Capacity refusal (error tag 1).
    fn port_reply(&mut self, a: &[u64]) -> X<Vec<u64>> {
        let ok = |value: u64| vec![value, 0, 0, 0];
        let (c, id) = Self::call_of(a[1]);
        if c != (a[0] - SERVED_BASE) as usize {
            self.port_drop(a[1]);
            return Ok(ok(1));
        }
        let cell = &mut self.world.cells[c];
        let (calls, _, _, _, vocab) = cell.port();
        let call = calls.get_mut(&id).unwrap();
        call.held = false;
        let (op, caller, dialed) = (call.words[0], call.caller, call.words[4]);
        if is_notice(vocab, op) {
            // A Released notice needs no reply; `Done` acknowledges it.
            cell.prune(id);
            return if a[2] == 0 {
                Ok(ok(0))
            } else {
                Err(Stop::Cause("Fail"))
            };
        }
        let live = !call.finished && call.answer.is_none();
        let task = &self.sched.tasks[caller];
        let pending = task.state == State::Provider && task.cancel && task.cancellable;
        if !live || pending {
            cell.free_staged(id);
            cell.prune(id);
            return Ok(ok(1));
        }
        let Some((answer, kind)) = self.port_answer(c, id, a[2], &a[3..9]) else {
            let cell = &mut self.world.cells[c];
            let k = cell.settle(id, port_outcome(vocab, op, port_closed(vocab, op)));
            k.into_iter().for_each(|k| self.ready_provider(k));
            return Err(Stop::Cause("Fail"));
        };
        let mut answer = answer;
        if let Some(kind) = kind {
            let VirtualCell::Port {
                room,
                used,
                handles,
                ..
            } = &mut self.world.cells[c]
            else {
                unreachable!()
            };
            let available = *room - *used;
            if available < HANDLE_ROOM {
                let cell = &mut self.world.cells[c];
                let k = cell.settle(id, port_outcome(vocab, op, port_closed(vocab, op)));
                k.into_iter().for_each(|k| self.ready_provider(k));
                return Ok(vec![HANDLE_ROOM, 1, 0, available]);
            }
            *used += HANDLE_ROOM;
            handles.insert(answer[0], PortHandle::new(kind));
            if vocab == Vocab::Net {
                let key = PORT_KEYS | (self.world.next_port_key << 2);
                self.world.next_port_key += 1;
                // A connected socket's peer is the dialed address; an
                // accepted one's, the reply's.
                let peer = if op == 0 { dialed } else { a[7] };
                self.world
                    .port_keys
                    .insert(key, (c, answer[0], peer as u16));
                answer[0] = key;
            } else {
                self.world.port_handles.push((c, answer[0]));
                answer[0] = PORT_HANDLE + self.world.port_handles.len() as u64 - 1;
            }
        }
        let mut pipes = [0; 3];
        if (vocab, a[2]) == (Vocab::Exec, exec::STARTED) {
            let needed = HANDLE_ROOM * a[3..7].iter().filter(|&&id| id != 0).count() as u64;
            let VirtualCell::Port {
                room,
                used,
                handles,
                ..
            } = &mut self.world.cells[c]
            else {
                unreachable!()
            };
            let available = *room - *used;
            if available < needed {
                let k =
                    self.world.cells[c].settle(id, port_outcome(vocab, op, port_closed(vocab, op)));
                k.into_iter().for_each(|k| self.ready_provider(k));
                return Ok(vec![needed, 1, 0, available]);
            }
            *used += needed;
            for (i, &id) in a[3..7].iter().enumerate().filter(|&(_, id)| *id != 0) {
                let (kind, part) = match i {
                    0 => (PORT_PROC, 0),
                    1 => (PORT_PIPE_WRITE, 2),
                    _ => (PORT_PIPE_READ, 1),
                };
                handles.insert(id, PortHandle::new(kind));
                let key = PORT_KEYS | (self.world.next_port_key << 2);
                self.world.next_port_key += 1;
                self.world.port_keys.insert(key, (c, id, 0));
                if i == 0 {
                    answer[0] = key;
                } else {
                    pipes[i - 1] = key | part;
                }
            }
        }
        let (calls, ..) = self.world.cells[c].port();
        let call = &calls[&id];
        if let (Some(out), false) = (call.out, call.staged.is_empty())
            && !((vocab == Vocab::Exec || (vocab, op) == (Vocab::Net, NET_DATAGRAM_RECV))
                && answer[1] != 0)
        {
            // The commit copies the staged bytes into the caller's capacity.
            let (op, staged) = (call.words[0], call.staged.clone());
            let (_, data, len, _) = self.carrier(out);
            if (vocab, op) == (Vocab::Net, 1) {
                // A resolve's records become the caller's Ip elements.
                let addresses = hirsel_core_net::ip_records(&staged).expect("checked records");
                for (i, address) in addresses.iter().enumerate() {
                    let at = (len + i as u64) * hirsel_core_net::Ip::SIZE as u64;
                    self.put(
                        data + at,
                        &hirsel_core_net::Ip::from(*address).layout_bytes(),
                    );
                }
                self.store(out + 8, 8, len + addresses.len() as u64);
            } else {
                self.put(data + len, &staged);
                self.store(out + 8, 8, len + staged.len() as u64);
            }
        }
        let mut row = port_outcome(vocab, op, answer);
        if (vocab, op) == (Vocab::Exec, exec::SPAWN) {
            row[4..7].copy_from_slice(&pipes);
        }
        if vocab == Vocab::Net && matches!(op, 2 | 3) && row[1] == 0 {
            // A listener's bound address, an accepted socket's peer.
            row[4..8].copy_from_slice(&a[4..8]);
        }
        if vocab == Vocab::Net && op == 6 && row[1] == 0 {
            // The peer digest's words.
            row[4..8].copy_from_slice(&a[5..9]);
        }
        let k = self.world.cells[c].settle(id, row);
        k.into_iter().for_each(|k| self.ready_provider(k));
        Ok(ok(0))
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
        if matches!(
            op,
            HostOp::ProvideStdout
                | HostOp::ProvideStderr
                | HostOp::ProvideDir
                | HostOp::ProvideNet
                | HostOp::ProvideExec
        ) {
            // A port's room with its own record, held until the run ends.
            let payer = t.limits.last().unwrap();
            let charge = hirsel_core_layout::charge::PROVIDE_CELL.saturating_add(a[0]);
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
                        wall_timers: Vec::new(),
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
            HostOp::ProvideStdout
            | HostOp::ProvideStderr
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec => {
                let vocab = match op {
                    HostOp::ProvideDir => Vocab::Dir,
                    HostOp::ProvideNet => Vocab::Net,
                    HostOp::ProvideExec => Vocab::Exec,
                    _ => Vocab::Stream,
                };
                let port = VirtualCell::Port {
                    vocab,
                    terminal: vocab == Vocab::Stream && a[1] != 0,
                    sealed: false,
                    next: 0,
                    calls: BTreeMap::new(),
                    servers: Vec::new(),
                    handles: if vocab == Vocab::Dir {
                        BTreeMap::from([(0, PortHandle::new(0))])
                    } else {
                        BTreeMap::new()
                    },
                    room: a[0],
                    used: 0,
                };
                let h = new(&mut self.world, port);
                let served = h - CELL_BASE + SERVED_BASE;
                if vocab == Vocab::Exec {
                    let key = EXEC_GRANT + self.world.exec_grants.len() as u64;
                    self.world.exec_grants.push((
                        (h - CELL_BASE) as usize,
                        hirsel_core_net::exec::ExecRules::universal(),
                    ));
                    return Some(Ok(ok(key, served)));
                }
                if vocab != Vocab::Net {
                    return Some(Ok(ok(h, served)));
                }
                // A served Net is an ordinary grant of every address, handshake
                // and name (native `provide_port`), so `net.narrow`
                // intersects it as any Net's.
                let tls = hirsel_core_net::GrantRule::Tls(hirsel_core_net::TlsRule::served());
                let resolve =
                    hirsel_core_net::GrantRule::Resolve(hirsel_core_net::ResolveRule::served());
                let rules = (0..2)
                    .flat_map(|dir| {
                        (0..2).map(move |family| {
                            hirsel_core_net::GrantRule::Address(hirsel_core_net::Rule {
                                dir,
                                family,
                                hi: 0,
                                lo: 0,
                                bits: 0,
                                first: 0,
                                last: u16::MAX,
                            })
                        })
                    })
                    .chain([tls, resolve])
                    .chain(hirsel_core_net::UnixDirection::ALL.into_iter().map(|dir| {
                        hirsel_core_net::GrantRule::Unix(hirsel_core_net::UnixRule::served(dir))
                    }))
                    .collect();
                let grants = &mut self.world.network.grants;
                let key = grants
                    .last_key_value()
                    .map_or(0, |(&key, _)| key)
                    .checked_add(1)
                    .expect("run-local handle counter");
                grants.insert(key, rules);
                self.world.net_ports.insert(key, (h - CELL_BASE) as usize);
                ok(key, served)
            }
            HostOp::StreamTerminal => {
                let terminal = match self.cell(a[0]) {
                    Some(c) => match self.world.cells[c] {
                        VirtualCell::Port { terminal, .. } => terminal,
                        _ => unreachable!(),
                    },
                    None => self.world.terminals[a[1] as usize],
                };
                ok(u64::from(terminal), 0)
            }
            HostOp::PortNext => {
                let c = (a[0] - SERVED_BASE) as usize;
                return Some(self.port_next(t, c));
            }
            HostOp::CallWord => {
                let (c, id) = Self::call_of(a[0]);
                let (calls, ..) = self.world.cells[c].port();
                ok(calls[&id].words.get(a[1] as usize).copied().unwrap_or(0), 0)
            }
            HostOp::CallSecret => {
                // A new designation charged as `secret.narrow`; any call but
                // a SendSecret fails the server (native `call_secret`).
                let (c, id) = Self::call_of(a[0]);
                let (calls, ..) = self.world.cells[c].port();
                let Some(secret) = calls[&id]
                    .secret
                    .as_ref()
                    .and_then(|secrets| secrets.get(a[1] as usize).cloned())
                else {
                    return Some(Err(Stop::Cause("Fail")));
                };
                let size = SecretGrant::logical_size(&secret.policy.send, 0);
                let charge = size.unwrap_or(u64::MAX);
                let payer = t.limits.last().unwrap();
                if let Err(available) = self.admit_charge(payer, charge, size.is_none()) {
                    return Some(Ok(vec![charge, 2, 0, available]));
                }
                let key = self.designate(secret, (payer.clone(), charge));
                ok(key, 0)
            }
            HostOp::CallPayload => {
                let (c, id) = Self::call_of(a[0]);
                let (calls, ..) = self.world.cells[c].port();
                let call = &calls[&id];
                if call.finished || call.answer.is_some() {
                    return Some(Ok(ok(0, 0)));
                }
                let bytes = call.fields.get(a[1] as usize).cloned().unwrap_or_default();
                let (_, data, len, cap) = self.carrier(a[2]);
                if cap.saturating_sub(len) < bytes.len() as u64 {
                    return Some(Err(Stop::Cause("Bounds")));
                }
                self.put(data + len, &bytes);
                self.store(a[2] + 8, 8, len + bytes.len() as u64);
                ok(0, bytes.len() as u64)
            }
            HostOp::PortFill => {
                // Stage reply bytes in the room, at most the call's offer; a
                // room too short refuses with error tag 1 (MEM-21).
                let (c, id) = Self::call_of(a[1]);
                let (bytes, ..) = self.carrier(a[2]);
                if c != (a[0] - SERVED_BASE) as usize {
                    return Some(Ok(ok(0, 0)));
                }
                let VirtualCell::Port {
                    vocab,
                    calls,
                    room,
                    used,
                    ..
                } = &mut self.world.cells[c]
                else {
                    unreachable!()
                };
                let call = calls.get_mut(&id).unwrap();
                if call.finished || call.answer.is_some() {
                    return Some(Ok(ok(0, 0)));
                }
                let offer = match (*vocab, call.words[0]) {
                    (Vocab::Dir, 10 | 19) | (Vocab::Net, 1 | 4 | NET_DATAGRAM_RECV) => {
                        call.words[2]
                    }
                    (Vocab::Dir, 2 | 11) => call.words[3],
                    (Vocab::Exec, exec::RECV) => call.words[2].min(call.words[3]),
                    _ => 0,
                };
                let take = offer
                    .saturating_sub(call.staged.len() as u64)
                    .min(bytes.len() as u64);
                let available = *room - *used;
                if take > available {
                    return Some(Ok(vec![take, 1, 0, available]));
                }
                *used += take;
                call.staged.extend_from_slice(&bytes[..take as usize]);
                ok(take, 0)
            }
            HostOp::PortReply => {
                return Some(self.port_reply(a));
            }
            HostOp::ClockSetWall => {
                let c = self.cell(a[0]).unwrap();
                let VirtualCell::Clock {
                    wall, wall_timers, ..
                } = &mut self.world.cells[c]
                else {
                    unreachable!()
                };
                *wall = a[1];
                let now = *wall;
                let mut due: Vec<_> = wall_timers
                    .iter()
                    .copied()
                    .filter(|&(at, _)| at <= now)
                    .collect();
                wall_timers.retain(|&(at, _)| at > now);
                due.sort_by_key(|&(at, _)| at);
                for (_, k) in due {
                    self.ready_provider(k);
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
            HostOp::Stdout | HostOp::Stderr => {
                let c = self.cell(a[0])?;
                if let Err(cancelled) = self.check_cancel(t) {
                    return Some(Err(cancelled));
                }
                let (bytes, ..) = self.carrier(a[1]);
                let mut words = [0; 9];
                words[1] = bytes.len() as u64;
                return Some(self.port_call(t, c, words, vec![bytes], None, true));
            }
            HostOp::Open
            | HostOp::OpenDir
            | HostOp::Mkdtemp
            | HostOp::Stat
            | HostOp::Mkdir
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::Rename
            | HostOp::SyncDir
            | HostOp::Entries
            | HostOp::Read
            | HostOp::ReadAt
            | HostOp::Write
            | HostOp::WriteAt
            | HostOp::StatFile
            | HostOp::Sync
            | HostOp::TryLock
            | HostOp::Unlock
            | HostOp::Close
            | HostOp::NextEntries => {
                let (c, h) = self.port_handle(a[0] & !DIR_READ_ONLY)?;
                return Some(self.dir_call(t, op, c, h, a));
            }
            HostOp::WallMs
            | HostOp::MonoNs
            | HostOp::SleepUntil
            | HostOp::SleepUntilWall
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
                    HostOp::SleepUntilWall => {
                        let at = a[1];
                        loop {
                            let VirtualCell::Clock { wall, .. } = self.world.cells[c] else {
                                unreachable!()
                            };
                            if at <= wall {
                                break;
                            }
                            if let VirtualCell::Clock { wall_timers, .. } = &mut self.world.cells[c]
                                && !wall_timers.iter().any(|&(_, j)| j == t.k)
                            {
                                wall_timers.push((at, t.k));
                            }
                            self.sched.tasks[t.k].cancellable =
                                t.shield == 0 && t.obligations.len() == 1;
                            self.park(Some(Wait::Network(None)));
                            if std::mem::take(&mut self.sched.tasks[t.k].cancelled) {
                                if let VirtualCell::Clock { wall_timers, .. } =
                                    &mut self.world.cells[c]
                                {
                                    wall_timers.retain(|&(_, k)| k != t.k);
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
        let limit = t.limits.pop().unwrap();
        let unit_owner = self.children[i].unit_owner;
        if unit_owner.is_some() {
            if returned.is_ok() {
                let owners = self.cross_owners(r);
                self.check_cross_sockets(rb, &owners);
                self.children[i].crossed_result = Some(self.cross_take(rb, r, &owners));
            }
            self.children[i].unit_limit = Some(limit);
        } else {
            self.end_limit(limit, None);
        }
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
        if let Some(owner) = unit_owner {
            self.notify(owner, conn::UnitNotice::HelperEnd(i));
        }
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
    /// A served Net caller's socket or listener (native `Port::socket`): its
    /// port, server id and whether the key's part (whole, read or write half)
    /// is live and of kind `kind`.
    fn port_socket(&self, key: u64, kind: u8) -> Option<(usize, u64, bool)> {
        let &(c, id, _) = self.world.port_keys.get(&(key & !3))?;
        let VirtualCell::Port { handles, .. } = &self.world.cells[c] else {
            unreachable!()
        };
        let live = handles.get(&id).is_some_and(|h| {
            let state = match (key & 3, h.write) {
                (0, None) | (1, Some(_)) => Some(h.state),
                (1, None) if kind == PORT_PIPE_READ => Some(h.state),
                (2, None) if kind == PORT_PIPE_WRITE => Some(h.state),
                (2, write) => write,
                _ => None,
            };
            h.kind == kind && state == Some(PortState::Live)
        });
        Some((c, id, live))
    }

    /// A Net operation on a served Net or its socket: its words and data
    /// fields (native `Port::net_words`, `field_bytes`).
    fn net_call(
        &mut self,
        t: &Task,
        op: HostOp,
        c: usize,
        id: u64,
        live: bool,
        a: &[u64],
    ) -> X<Vec<u64>> {
        self.check_cancel(t)?;
        let served_exec = matches!(
            self.world.cells[c],
            VirtualCell::Port {
                vocab: Vocab::Exec,
                ..
            }
        );
        let (words, fields, out): (Vec<u64>, Vec<Vec<u8>>, Option<u64>) = match op {
            HostOp::Connect => (vec![0, a[1], a[2], a[3], a[4]], vec![], None),
            HostOp::Listen => (vec![2, a[1], a[2], a[3], a[4], a[5]], vec![], None),
            HostOp::Accept => (vec![3, id], vec![], None),
            HostOp::Resolve => {
                // The name as written and room for the caller's spare Ip
                // slots as address records (NOTES 34).
                let name = self.carrier_bytes(a[1]).to_vec();
                let (_, _, len, cap) = self.carrier(a[2]);
                let room = cap
                    .saturating_sub(len)
                    .saturating_mul(hirsel_core_net::IP_RECORD_LEN as u64);
                (vec![1, name.len() as u64, room], vec![name], Some(a[2]))
            }
            HostOp::RecvBounded => {
                let (_, _, len, cap) = self.carrier(a[1]);
                let room = cap.saturating_sub(len);
                let (tag, room) = if served_exec {
                    (exec::RECV, room)
                } else {
                    (4, room.min(a[2]))
                };
                (vec![tag, id, room, a[2]], vec![], Some(a[1]))
            }
            HostOp::RecvReady => (vec![NET_READY, id], vec![], None),
            HostOp::SendPair => {
                let prefix = self.carrier(a[1]).0;
                let tail = self.carrier(a[2]).0;
                (
                    vec![
                        if served_exec { exec::SEND } else { 5 },
                        id,
                        prefix.len() as u64,
                        tail.len() as u64,
                    ],
                    vec![vec![], prefix, tail],
                    None,
                )
            }
            _ => unreachable!("a TCP operation"),
        };
        let mut w = [0; 9];
        w[..words.len()].copy_from_slice(&words);
        // Each operation's `until` operand (Listen has none).
        let deadline = match op {
            HostOp::Connect => self.until(a, 5),
            HostOp::Accept | HostOp::RecvReady => self.until(a, 1),
            HostOp::Resolve | HostOp::RecvBounded | HostOp::SendPair => self.until(a, 3),
            _ => None,
        };
        self.port_call_with(t, c, w, fields, out, live, &mut None, deadline)
    }

    /// `net.join` on a served Net socket's live halves (native
    /// `provide::join_socket`): whole again, the server asked nothing.
    fn port_join(&mut self, key: u64) -> bool {
        let Some(&(c, id, _)) = self.world.port_keys.get(&(key & !3)) else {
            return false;
        };
        let VirtualCell::Port { handles, .. } = &mut self.world.cells[c] else {
            unreachable!()
        };
        match handles.get_mut(&id) {
            Some(h)
                if h.kind == PORT_SOCKET
                    && h.state == PortState::Live
                    && h.write == Some(PortState::Live) =>
            {
                h.write = None;
                true
            }
            _ => false,
        }
    }

    /// `net.split` or a dropped socket, half or listener of a served Net
    /// (native `split_socket`, `release_socket`); false when `key` is not one.
    fn port_socket_part(&mut self, key: u64, split: bool) -> bool {
        let Some(&(c, id, _)) = self.world.port_keys.get(&(key & !3)) else {
            return false;
        };
        let VirtualCell::Port {
            next,
            handles,
            servers,
            ..
        } = &mut self.world.cells[c]
        else {
            unreachable!()
        };
        let Some(h) = handles.get_mut(&id) else {
            self.world.port_keys.remove(&(key & !3));
            self.world.datagrams.remove(&key);
            return true;
        };
        if split {
            h.write = Some(PortState::Live);
            return true;
        }
        let slot = if key & 3 == 2 && h.kind != PORT_PIPE_WRITE {
            h.write.as_mut()
        } else {
            Some(&mut h.state)
        };
        let marked = match slot {
            Some(s) if *s == PortState::Live => {
                *s = PortState::Released(*next);
                true
            }
            _ => false,
        };
        if h.state != PortState::Live && h.write.is_none_or(|w| w != PortState::Live) {
            self.world.port_keys.remove(&(key & !3));
            self.world.datagrams.remove(&key);
        }
        if marked {
            *next += 1;
            let servers: Vec<usize> = servers.iter().map(|&(k, _)| k).collect();
            for k in servers {
                self.ready_provider(k);
            }
        }
        true
    }

    fn exec_call(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::exec::LogicalLaunch;
        self.check_cancel(t)?;
        if op == HostOp::ExecSpawn {
            let Some((c, rules)) = a[0]
                .checked_sub(EXEC_GRANT)
                .and_then(|i| self.world.exec_grants.get(i as usize))
            else {
                std::panic::resume_unwind(Box::new(RunStop("NativeExec")));
            };
            let string = |at| std::str::from_utf8(self.carrier_bytes(at)).unwrap();
            let list = |at| (self.load(at, 8), self.load(at + 8, 8));
            let (args, argc) = list(a[2]);
            let (env, envc) = list(a[3]);
            let mut env: Vec<_> = (0..envc)
                .map(|i| {
                    let at = env + i * 48;
                    (string(at), self.carrier_bytes(at + 24))
                })
                .collect();
            env.sort_unstable_by_key(|(key, _)| *key);
            let launch = LogicalLaunch {
                program: string(a[1]),
                args: (0..argc)
                    .map(|i| self.carrier_bytes(args + i * 24))
                    .collect(),
                env,
                dir: string(a[4]),
                path: string(a[5]),
                piped: [a[6] != 0, a[7] != 0, a[8] != 0],
            };
            if !launch.permitted(rules) {
                return Ok(vec![0, 1, 0, 0, 0, 0, 0]);
            }
            let [args, env] = launch.served_payloads();
            let fields = vec![
                launch.program.as_bytes().to_vec(),
                args,
                env,
                launch.dir.as_bytes().to_vec(),
                launch.path.as_bytes().to_vec(),
            ];
            let mut words = [0; 9];
            for (word, field) in words[1..6].iter_mut().zip(&fields) {
                *word = field.len() as u64;
            }
            words[6..].copy_from_slice(&a[6..9]);
            let c = *c;
            let payer = t.limits.last().unwrap();
            let charge = charge::EXEC_HANDLE * (1 + words[6..].iter().sum::<u64>());
            if let Err(available) = self.admit(payer, charge) {
                return Ok(vec![charge, 5, 0, available, 0, 0, 0]);
            }
            let answer = self.port_call(t, c, words, fields, None, true)?;
            if answer[1] == 0 {
                for key in [answer[0], answer[4], answer[5], answer[6]]
                    .into_iter()
                    .filter(|&key| key != 0)
                {
                    self.world.exec_handles.insert(key, payer.clone());
                }
            } else {
                self.refund(payer.clone(), charge);
            }
            return Ok(answer);
        }

        let Some((c, id, live)) = self.port_socket(a[0], PORT_PROC) else {
            std::panic::resume_unwind(Box::new(RunStop("NativeExec")));
        };
        let wait = op == HostOp::ExecWait;
        let cached = self.world.cells[c].port().1[&id].termination;
        if let Some(value) = cached {
            return Ok(vec![if wait { value } else { 0 }, 0, 0, 0]);
        }
        let mut words = [0; 9];
        words[..2].copy_from_slice(&[if wait { exec::AWAIT } else { exec::SIGNAL }, id]);
        let deadline = if wait {
            self.until(a, 1)
        } else {
            words[2] = a[1];
            None
        };
        let answer =
            self.port_call_with(t, c, words, Vec::new(), None, live, &mut None, deadline)?;
        if wait && answer[1] == 0 {
            self.world.cells[c]
                .port()
                .1
                .get_mut(&id)
                .unwrap()
                .termination = Some(answer[0]);
        }
        Ok(answer)
    }

    fn exec_narrow(&mut self, t: &Task, a: &[u64]) -> Vec<u64> {
        use hirsel_core_net::exec::{ArgToken, ExecRule, Form, Names};
        let Some((c, parent)) = a[0]
            .checked_sub(EXEC_GRANT)
            .and_then(|i| self.world.exec_grants.get(i as usize))
        else {
            std::panic::resume_unwind(Box::new(RunStop("NativeExec")));
        };
        let c = *c;
        let text = |at| std::str::from_utf8(self.carrier_bytes(at)).unwrap();
        let name = text(a[2]);
        let names = || Names::parse(text(a[3]));
        let rule = match a[1] {
            0 => Names::parse(name).map(ExecRule::Programs),
            1 => {
                let data = self.load(a[3], 8);
                let len = self.load(a[3] + 8, 8);
                let tokens = (0..len)
                    .map(|i| {
                        let at = data + i * 24;
                        match self.load(at + 16, 8) {
                            0x8000_0000_0000_0000 => ArgToken::Any,
                            0x8000_0000_0000_0001 => ArgToken::Rest,
                            _ => ArgToken::Lit(self.carrier_bytes(at).to_vec()),
                        }
                    })
                    .collect();
                Form::new(tokens).map(|form| ExecRule::Form(name.to_owned(), form))
            }
            2 => names().map(|keys| ExecRule::EnvKeys(name.to_owned(), keys)),
            3 => names().map(|labels| ExecRule::Dirs(name.to_owned(), labels)),
            _ => unreachable!(),
        };
        let Some(rules) = rule.and_then(|rule| parent.narrow(&rule)) else {
            return vec![0, 1, 0, 0];
        };
        let size = rules.charge();
        let charge = size.unwrap_or(u64::MAX);
        let payer = t.limits.last().unwrap();
        if let Err(available) = self.admit_charge(payer, charge, size.is_none()) {
            return vec![charge, 2, 0, available];
        }
        let key = EXEC_GRANT + self.world.exec_grants.len() as u64;
        self.world.exec_grants.push((c, rules));
        self.world.grant_charges.push((payer.clone(), charge));
        vec![key, 0, 0, 0]
    }

    fn network_host(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        match op {
            HostOp::Connect | HostOp::Listen => {
                let grants = self.world.network.grants.get(&a[0]);
                let allowed = grants.is_some_and(|rules| {
                    rules.iter().any(|rule| {
                        matches!(
                            rule,
                            hirsel_core_net::GrantRule::Address(rule) if rule.permits(
                                u8::from(op == HostOp::Listen),
                                a[1] as u8,
                                a[2],
                                a[3],
                                a[4] as u16,
                            )
                        )
                    })
                });
                if allowed && let Some(&c) = self.world.net_ports.get(&a[0]) {
                    return self.net_call(t, op, c, 0, true, a);
                }
            }
            HostOp::Accept | HostOp::RecvBounded | HostOp::RecvReady | HostOp::SendPair => {
                let stream = op != HostOp::Accept;
                let kind = if stream { PORT_SOCKET } else { 4 };
                let original = self.port_socket(a[0], kind);
                let socket = if stream && original.is_some_and(|(_, _, live)| !live) {
                    self.port_socket(a[0], PORT_UNIX_SOCKET)
                } else {
                    original
                };
                if let Some((c, id, mut live)) = socket {
                    if stream
                        && matches!(
                            self.world.cells[c],
                            VirtualCell::Port {
                                vocab: Vocab::Exec,
                                ..
                            }
                        )
                    {
                        let kind = if op == HostOp::SendPair {
                            PORT_PIPE_WRITE
                        } else {
                            PORT_PIPE_READ
                        };
                        (_, _, live) = self.port_socket(a[0], kind).unwrap();
                        if op == HostOp::RecvReady {
                            // ExecOp has no readiness call: a served pipe is
                            // ready at once and its receive waits (NET-1).
                            self.check_cancel(t)?;
                            return Ok(vec![0, 0, 0, 0]);
                        }
                    }
                    return self.net_call(t, op, c, id, live, a);
                }
            }
            _ => unreachable!(),
        }
        let mut denied = vec![0, 1, 0, 0];
        if matches!(op, HostOp::Listen | HostOp::Accept) {
            denied.extend([0; 4]);
        }
        Ok(denied)
    }

    fn unix_host(&mut self, t: &Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::{GrantRule, UnixDirection, valid_relative_path};

        let denied = || Ok(vec![0, 1, 0, 0]);
        let key = a[0];
        let creates_datagram = matches!(op, HostOp::DatagramOpen | HostOp::DatagramBind);
        let (grant, c, id, live) = match op {
            HostOp::UnixConnect
            | HostOp::UnixListen
            | HostOp::DatagramOpen
            | HostOp::DatagramBind => {
                let Some(&c) = self.world.net_ports.get(&key) else {
                    return denied();
                };
                (key, c, 0, true)
            }
            HostOp::UnixAccept => {
                let Some((c, id, live)) = self.port_socket(key, PORT_UNIX_LISTENER) else {
                    return denied();
                };
                (0, c, id, live)
            }
            _ => {
                let Some(state) = self.world.datagrams.get(&key) else {
                    return denied();
                };
                if (op == HostOp::DatagramRecv && !state.bound)
                    || (op == HostOp::DatagramSend && !state.connected)
                {
                    return denied();
                }
                let Some((c, id, live)) = self.port_socket(key, PORT_DATAGRAM) else {
                    return denied();
                };
                (state.grant, c, id, live)
            }
        };
        let direction = match op {
            HostOp::UnixConnect => Some(UnixDirection::Connect),
            HostOp::UnixListen => Some(UnixDirection::Listen),
            HostOp::DatagramBind => Some(UnixDirection::Bind),
            HostOp::DatagramConnect | HostOp::DatagramSendTo => Some(UnixDirection::Send),
            _ => None,
        };
        let mut fields = Vec::new();
        if let Some(dir) = direction {
            let root = self.carrier_bytes(a[1]);
            let path = self.carrier_bytes(a[2]);
            if root.is_empty() || path.len() > hirsel_core_net::unix::MAX_PATH_BYTES || !valid_relative_path(path)
                || !self.world.network.grants[&grant].iter().any(|rule| {
                    matches!(rule, GrantRule::Unix(rule) if rule.permits(
                        dir, std::str::from_utf8(root).unwrap(), std::str::from_utf8(path).unwrap()))
                })
            {
                return denied();
            }
            if matches!(op, HostOp::DatagramConnect | HostOp::DatagramSendTo) {
                fields.push(Vec::new());
            }
            fields.extend([root.to_vec(), path.to_vec()]);
        }
        let (root, path) = if direction.is_some() {
            let i = fields.len() - 2;
            (fields[i].len() as u64, fields[i + 1].len() as u64)
        } else {
            (0, 0)
        };
        let mut w = [0; 9];
        let (out, deadline) = match op {
            HostOp::UnixConnect => {
                w[..3].copy_from_slice(&[NET_UNIX_CONNECT, root, path]);
                (None, self.until(a, 3))
            }
            HostOp::UnixListen => {
                w[..4].copy_from_slice(&[NET_UNIX_LISTEN, root, path, a[3]]);
                (None, None)
            }
            HostOp::UnixAccept => {
                w[..2].copy_from_slice(&[NET_UNIX_ACCEPT, id]);
                (None, self.until(a, 1))
            }
            HostOp::DatagramOpen => {
                w[0] = NET_DATAGRAM_OPEN;
                (None, None)
            }
            HostOp::DatagramBind => {
                w[..3].copy_from_slice(&[NET_DATAGRAM_BIND, root, path]);
                (None, None)
            }
            HostOp::DatagramConnect => {
                w[..4].copy_from_slice(&[NET_DATAGRAM_CONNECT, id, root, path]);
                (None, None)
            }
            HostOp::DatagramSend => {
                let data = self.carrier_bytes(a[1]);
                w[..3].copy_from_slice(&[NET_DATAGRAM_SEND, id, data.len() as u64]);
                fields.extend([Vec::new(), data.to_vec()]);
                (None, self.until(a, 2))
            }
            HostOp::DatagramSendTo => {
                let data = self.carrier_bytes(a[3]);
                w[..5].copy_from_slice(&[NET_DATAGRAM_SEND_TO, id, root, path, data.len() as u64]);
                fields.push(data.to_vec());
                (None, self.until(a, 4))
            }
            HostOp::DatagramRecv => {
                let (_, _, len, cap) = self.carrier(a[1]);
                w[..3].copy_from_slice(&[NET_DATAGRAM_RECV, id, cap.saturating_sub(len)]);
                (Some(a[1]), self.until(a, 2))
            }
            _ => unreachable!(),
        };
        let result = self.port_call_with(t, c, w, fields, out, live, &mut None, deadline)?;
        if result[1] == 0 {
            if creates_datagram {
                self.world.datagrams.insert(
                    result[0],
                    DatagramState {
                        grant,
                        bound: op == HostOp::DatagramBind,
                        connected: false,
                    },
                );
            } else if op == HostOp::DatagramConnect {
                self.world.datagrams.get_mut(&key).unwrap().connected = true;
            }
        }
        Ok(result)
    }

    fn network_resolve(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::{GrantRule, Name};
        let Some(name) = std::str::from_utf8(self.carrier_bytes(a[1]))
            .ok()
            .and_then(Name::parse)
        else {
            return Ok(vec![0, 1, 0, 0]);
        };
        let allowed = self.world.network.grants.get(&a[0]).is_some_and(|rules| {
            rules
                .iter()
                .any(|r| matches!(r, GrantRule::Resolve(rule) if rule.permits(&name)))
        });
        if allowed && let Some(&c) = self.world.net_ports.get(&a[0]) {
            return self.net_call(t, HostOp::Resolve, c, 0, true, a);
        }
        Ok(vec![0, 1, 0, 0])
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
        let served = self.world.net_ports.get(&a[0]).copied();
        if let Some(&(c, _, peer)) = self.world.port_keys.get(&(key & !3)) {
            return self.port_tls(t, a, c, peer, served);
        }
        self.world.network.sockets.release(key);
        Ok(vec![0, 1, 0, 0, 0, 0, 0, 0])
    }

    /// `tls.connect` on a served Net's socket (native `tls::connect` through
    /// the port): the handshake is a call its server answers with Secured,
    /// and only through that Net (NOTES 32). The Socket is consumed on every
    /// return: a failure releases it whole.
    fn port_tls(
        &mut self,
        t: &Task,
        a: &[u64],
        c: usize,
        peer: u16,
        served: Option<usize>,
    ) -> X<Vec<u64>> {
        let key = a[1];
        let failed = |s: &mut Self, tag: u64, code: u64| {
            s.port_socket_part(key, false);
            Ok(vec![0, tag, code, 0, 0, 0, 0, 0])
        };
        if served != Some(c)
            || !self
                .port_socket(key, PORT_SOCKET)
                .is_some_and(|(_, _, live)| live)
        {
            return failed(self, 1, 0);
        }
        let offered = match self.tls_offer(a, peer) {
            Ok(offered) => offered,
            Err((tag, code)) => return failed(self, tag, code),
        };
        let name = hirsel_core_net::tls::verification_name(self.carrier_bytes(a[2])).unwrap();
        let canonical = match &name {
            hirsel_core_net::Name::Dns(name) => name.as_bytes().to_vec(),
            hirsel_core_net::Name::Ip(ip) => ip.to_string().into_bytes(),
        };
        let wire: Vec<u8> = offered
            .iter()
            .flat_map(|p| std::iter::once(p.len() as u8).chain(p.iter().copied()))
            .collect();
        let (_, id, live) = self.port_socket(key, 3).unwrap();
        let mut w = [0; 9];
        w[..5].copy_from_slice(&[
            6,
            id,
            canonical.len() as u64,
            wire.len() as u64,
            u64::from(a[3] == 0),
        ]);
        let row = match self.check_cancel(t).and_then(|()| {
            let deadline = self.until(a, 6);
            let fields = vec![vec![], canonical, wire];
            self.port_call_with(t, c, w, fields, None, live, &mut None, deadline)
        }) {
            Ok(row) => row,
            Err(stop) => {
                self.port_socket_part(key, false);
                return Err(stop);
            }
        };
        if row[1] != 0 {
            return failed(self, row[1], row[2]);
        }
        self.port_socket_part(key, true);
        self.world
            .tls_writes
            .insert(key & !3, (name, false, Default::default()));
        Ok([&[key, 0, 0], &row[3..8]].concat())
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
        if let Some(&c) = self.world.net_ports.get(&parent) {
            // A narrowed served Net keeps its port (native `narrow` keeps
            // the provider).
            self.world.net_ports.insert(key, c);
        }
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
            let Some(parent) = self.secret(a[0]) else {
                return vec![0, 1, 0, 0];
            };
            let Some(intersection) = parent.policy.intersect(&policy) else {
                return vec![0, 1, 0, 0];
            };
            (intersection, 0)
        };
        let size = SecretGrant::logical_size(&policy.send, len);
        let charge = size.unwrap_or(u64::MAX);
        let payer = t.limits.last().unwrap();
        if let Err(available) = self.admit_charge(payer, charge, size.is_none()) {
            return vec![charge, 2, 0, available];
        }
        let (bytes, seal) = if op == HostOp::SecretSeal {
            let data = self.load(a[0], 8);
            let bytes = Zeroizing::new(self.carrier_bytes(a[0]).to_vec());
            if !bytes.is_empty() && self.load(a[0] + 16, 8) != 0 {
                let (block, offset) = split(data);
                use zeroize::Zeroize;
                self.mem[block].bytes[offset..offset + bytes.len()].zeroize();
            }
            let key = self.store_sealed(policy, bytes, payer.clone(), charge);
            return vec![key, 0, 0, 0];
        } else {
            let parent = self.secret(a[0]).unwrap();
            (parent.bytes.clone(), parent.seal.clone())
        };
        let payer = payer.clone();
        let key = self.designate(
            EvalSecret {
                policy,
                bytes,
                seal,
            },
            (payer, charge),
        );
        vec![key, 0, 0, 0]
    }

    fn store_sealed(
        &mut self,
        policy: SecretPolicy,
        bytes: Zeroizing<Vec<u8>>,
        payer: Limit,
        charge: u64,
    ) -> u64 {
        let seal = Arc::new(());
        self.world
            .seals
            .push((Arc::downgrade(&seal), payer.clone(), charge));
        self.designate(
            EvalSecret {
                policy,
                bytes: Arc::new(bytes),
                seal,
            },
            (payer, 0),
        )
    }

    fn secret_derive(&mut self, t: &Task, a: &[u64]) -> Vec<u64> {
        let parent = self.secret(a[0]).unwrap();
        let policy = std::str::from_utf8(self.carrier_bytes(a[3]))
            .ok()
            .and_then(|uses| parent.policy.derived(uses));
        let Some(policy) = policy else {
            return vec![0, 1, 0, 0];
        };
        let charge = SecretGrant::logical_size(&policy.send, 32).unwrap();
        let payer = t.limits.last().unwrap();
        if let Err(available) = self.admit_charge(payer, charge, false) {
            return vec![charge, 2, 0, available];
        }
        let parent = self.secret(a[0]).unwrap();
        let bytes = hirsel_core_net::derive_hmac_sha256(
            &parent.bytes,
            self.carrier_bytes(a[1]),
            self.carrier_bytes(a[2]),
        );
        let key = self.store_sealed(policy, bytes, payer.clone(), charge);
        vec![key, 0, 0, 0]
    }

    fn secret(&self, key: u64) -> Option<&EvalSecret> {
        let slot = self.world.secrets.get(key as usize)?.as_ref()?;
        Some(&slot.secret)
    }

    /// A new designation with one holder (native `Secrets::push`).
    fn designate(&mut self, secret: EvalSecret, charge: (Limit, u64)) -> u64 {
        self.world.secrets.push(Some(EvalSlot {
            secret,
            holders: 1,
            charge,
        }));
        self.world.secrets.len() as u64 - 1
    }

    fn secret_share(&mut self, key: u64) -> u64 {
        self.world.secrets[key as usize].as_mut().unwrap().holders += 1;
        key
    }

    /// Release one holder; the last refunds the designation's charge, and the
    /// last designation of sealed bytes refunds the seal (TLS-5).
    fn secret_drop(&mut self, key: u64) {
        let slot = self.world.secrets[key as usize].as_mut().unwrap();
        slot.holders -= 1;
        if slot.holders != 0 {
            return;
        }
        let slot = self.world.secrets[key as usize].take().unwrap();
        let (l, n) = slot.charge;
        self.refund(l, n);
        drop(slot.secret);
        let (released, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.world.seals)
            .into_iter()
            .partition(|(token, ..)| token.strong_count() == 0);
        self.world.seals = held;
        for (_, l, n) in released {
            self.refund(l, n);
        }
    }

    fn secret_mac(&mut self, op: HostOp, a: &[u64]) -> Vec<u64> {
        let secret = self.secret(a[0]).unwrap();
        let signing = op == HostOp::SecretHmac;
        let needed = if signing {
            SecretUse::MAC
        } else {
            SecretUse::VERIFY
        };
        if !secret.policy.uses.permits(needed) {
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

    /// `secret.wrap`, `unwrap` and `sign` (native `secret.rs` key
    /// operations): error tag 1 Denied, 2 Invalid, 3 Tampered, 4 Capacity.
    /// Every evaluator Secret is sealed, so no KEK or secret is opaque.
    fn secret_key(&mut self, t: &Task, op: HostOp, a: &[u64]) -> Vec<u64> {
        use hirsel_core_net::key_wrap::{self, KeyFailure, SignAlg};
        let fail = |f: KeyFailure| {
            let tag = match f {
                KeyFailure::Denied => 1,
                KeyFailure::Invalid => 2,
                KeyFailure::Tampered => 3,
            };
            vec![0, tag, 0, 0]
        };
        let key = self.secret(a[0]).unwrap().clone();
        let needed = match op {
            HostOp::SecretSign => SignAlg::from_case(a[1]).expect("a SignAlg case").needed(),
            _ => SecretUse::WRAP,
        };
        if !key.policy.uses.permits(needed) {
            return fail(KeyFailure::Denied);
        }
        let (produced, out) = match op {
            HostOp::SecretWrap => {
                let s = self.secret(a[1]).unwrap();
                let blob = key_wrap::wrap_with_salt(
                    &key.bytes,
                    &s.policy.text(),
                    &s.bytes,
                    key_wrap::salt(),
                );
                (blob, a[2])
            }
            HostOp::SecretSign => {
                let alg = SignAlg::from_case(a[1]).unwrap();
                (
                    key_wrap::sign(alg, &key.bytes, self.carrier_bytes(a[2])),
                    a[3],
                )
            }
            _ => {
                let Some(requested) = std::str::from_utf8(self.carrier_bytes(a[2]))
                    .ok()
                    .and_then(SecretPolicy::parse)
                else {
                    return fail(KeyFailure::Invalid);
                };
                let blob = self.carrier_bytes(a[1]).to_vec();
                let len = key_wrap::unwrapped_len(&blob).unwrap_or(0) as u64;
                let size = SecretGrant::logical_size(&requested.send, len);
                let charge = size.unwrap_or(u64::MAX);
                let payer = t.limits.last().unwrap().clone();
                if let Err(available) = self.admit_charge(&payer, charge, size.is_none()) {
                    return vec![charge, 4, 0, available];
                }
                let mut plain = Zeroizing::new(Vec::new());
                if let Err(f) = key_wrap::unwrap_into(&key.bytes, &blob, &mut plain)
                    .and_then(|bound| key_wrap::within(&requested, bound))
                {
                    self.refund(payer, charge);
                    return fail(f);
                }
                let seal = Arc::new(());
                self.world
                    .seals
                    .push((Arc::downgrade(&seal), payer.clone(), charge));
                let key = self.designate(
                    EvalSecret {
                        policy: requested,
                        bytes: Arc::new(plain),
                        seal,
                    },
                    (payer, 0),
                );
                return vec![key, 0, 0, 0];
            }
        };
        let bytes = match produced {
            Ok(bytes) => bytes,
            Err(f) => return fail(f),
        };
        let (_, data, len, cap) = self.carrier(out);
        let spare = cap.saturating_sub(len);
        let n = bytes.len() as u64;
        if n > spare {
            return vec![n, 4, 0, spare];
        }
        self.put(data + len, &bytes);
        self.store(out + 8, 8, len + n);
        vec![n, 0, 0, 0]
    }

    fn send_secret(&mut self, t: &Task, a: &[u64]) -> X<Vec<u64>> {
        let key = a[0];
        let words = |this: &Self, carrier: u64| -> Vec<u64> {
            let (data, len) = (this.load(carrier, 8), this.load(carrier + 8, 8));
            (0..len).map(|i| this.load(data + 8 * i, 8)).collect()
        };
        let wire = self.carrier_bytes(a[1]).to_vec();
        let (handles, marks) = (words(self, a[2]), words(self, a[3]));
        // A fault of the program, as an out-of-bounds index (TLS-5).
        if !hirsel_core_net::marks::well_formed(&marks, wire.len(), handles.len()) {
            return Err(Stop::Cause("Bounds"));
        }
        let Some((name, spent, _)) = self.world.tls_writes.get(&(key & !3)) else {
            return Ok(vec![0, 1, 0, 0]);
        };
        if *spent {
            return Ok(vec![0, 5, 0, 0]);
        }
        // Every secret is checked before any byte is sent; every evaluator
        // Secret is sealed, with no entry grants (TLS-5).
        let mut narrowed = Vec::with_capacity(handles.len());
        for &handle in &handles {
            let Some(secret) = self.secret(handle) else {
                return Ok(vec![0, 1, 0, 0]);
            };
            if !secret
                .policy
                .send
                .iter()
                .any(|pattern| pattern.covers(name))
            {
                return Ok(vec![0, 1, 0, 0]);
            }
            narrowed.push(EvalSecret {
                policy: SecretPolicy::exactly(name),
                bytes: secret.bytes.clone(),
                seal: secret.seal.clone(),
            });
        }
        if let Some((c, id, live)) = self.port_socket(key, 3) {
            // Its server receives the Secrets, never their bytes (NOTES 6).
            if key & 3 != 2 {
                return Ok(vec![0, 1, 0, 0]);
            }
            let mut w = [0; 9];
            w[..5].copy_from_slice(&[
                7,
                id,
                wire.len() as u64,
                handles.len() as u64,
                marks.len() as u64,
            ]);
            let marks = marks.iter().flat_map(|word| word.to_le_bytes()).collect();
            let fields = vec![vec![], wire, marks];
            self.check_cancel(t)?;
            let deadline = self.until(a, 4);
            let mut secret = Some(narrowed);
            let row = self.port_call_with(t, c, w, fields, None, live, &mut secret, deadline);
            if secret.is_none() && !matches!(&row, Ok(row) if row[1] == 0) {
                self.world.tls_writes.get_mut(&(key & !3)).unwrap().1 = true;
            }
            return row;
        }
        Ok(vec![0, 1, 0, 0])
    }

    fn scram_send(&mut self, t: &mut Task, args: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::scram::Transcript;
        let key = args[0];
        let Some((name, spent, binding)) = self.world.tls_writes.get(&(key & !3)) else {
            return Ok(vec![0, 1, 0, 0]);
        };
        let secret = self.secret(args[1]).unwrap();
        if !secret
            .policy
            .send
            .iter()
            .any(|pattern| pattern.covers(name))
        {
            return Ok(vec![0, 1, 0, 0]);
        }
        let (spent, binding, password) = (*spent, *binding, secret.bytes.clone());
        let client_first = self.carrier_bytes(args[2]).to_vec();
        let server_first = self.carrier_bytes(args[3]).to_vec();
        let transcript = match Transcript::parse(&client_first, &server_first, &binding) {
            Ok(transcript) => transcript,
            Err(error) => return Ok(vec![0, error as u64, 0, 0]),
        };
        self.take_steps(t, transcript.iterations().into())?;
        let payer = t.limits.last().unwrap().clone();
        const CHARGE: u64 = 32;
        if let Err(available) = self.admit(&payer, CHARGE) {
            return Ok(vec![CHARGE, 6, 0, available]);
        }
        if spent {
            self.refund(payer, CHARGE);
            return Ok(vec![0, 5, 0, 5]);
        }
        if let Err(cancelled) = self.check_cancel(t) {
            self.refund(payer, CHARGE);
            return Err(cancelled);
        }
        let before = self.carrier_bytes(args[4]).to_vec();
        let after = self.carrier_bytes(args[5]).to_vec();
        let (message, signature) = transcript.derive(&password);
        if let Some((c, id, live)) = self.port_socket(key, 3) {
            // Only Send calls reach its server, until it took every byte
            // (native `served_proof`); a failure spends the half.
            let joined = Zeroizing::new([&before[..], &message, &after].concat());
            let mut at = 0;
            while at < joined.len() {
                let mut w = [0; 9];
                w[..4].copy_from_slice(&[5, id, (joined.len() - at) as u64, 0]);
                let fields = vec![vec![], joined[at..].to_vec(), vec![]];
                let deadline = self.until(args, 6);
                let row =
                    match self.port_call_with(t, c, w, fields, None, live, &mut None, deadline) {
                        Ok(row) => row,
                        Err(stop) => {
                            self.refund(payer, CHARGE);
                            self.world.tls_writes.get_mut(&(key & !3)).unwrap().1 = true;
                            return Err(stop);
                        }
                    };
                if row[1] != 0 {
                    self.refund(payer, CHARGE);
                    self.world.tls_writes.get_mut(&(key & !3)).unwrap().1 = true;
                    return Ok(vec![0, 5, row[2], row[1]]);
                }
                at += (row[0] as usize).min(joined.len() - at);
            }
            let id = SCRAM_BASE + self.world.next_scram;
            self.world.next_scram += 1;
            self.world.scrams.insert(id, (signature, payer));
            return Ok(vec![id, 0, 0, 0]);
        }
        self.refund(payer, CHARGE);
        Ok(vec![0, 1, 0, 0])
    }

    fn scram_final_len(&self, args: &[u64]) -> Vec<u64> {
        use hirsel_core_net::scram::Transcript;
        let Some((_, _, binding)) = self.world.tls_writes.get(&(args[0] & !3)) else {
            return vec![0, 1, 0, 0];
        };
        match Transcript::parse(
            self.carrier_bytes(args[1]),
            self.carrier_bytes(args[2]),
            binding,
        ) {
            Ok(transcript) => vec![transcript.final_len() as u64, 0, 0, 0],
            Err(error) => vec![0, error as u64, 0, 0],
        }
    }

    fn scram_verify(&mut self, args: &[u64]) -> Vec<u64> {
        let (signature, payer) = self.world.scrams.remove(&args[0]).unwrap();
        let result = hirsel_core_net::scram::verify(&signature, self.carrier_bytes(args[1]));
        self.refund(payer, 32);
        vec![u64::from(result), 0, 0, 0]
    }

    /// A provider row against [`World`]: an outcome `value, error, code, written`.
    fn host(&mut self, t: &mut Task, op: HostOp, a: &[u64]) -> X<Vec<u64>> {
        if op == HostOp::SecretSeal {
            return Ok(self.secret_policy(t, op, a));
        }
        self.deny_describe_capability();
        let mode = if op == HostOp::Open { a[2] as u8 } else { 0 };
        if needs_writable_dir(op as u8, mode) && a[0] & DIR_READ_ONLY != 0 {
            self.check_cancel(t)?;
            return Ok(vec![0, DENIED, 0, 0]);
        }
        if let Some(reply) = self.provide(t, op, a) {
            return reply;
        }
        match op {
            HostOp::TlsConnect => return self.tls_connect(t, a),
            HostOp::SendSecret => return self.send_secret(t, a),
            HostOp::ScramSha256 => return self.scram_send(t, a),
            HostOp::ScramFinalLen => return Ok(self.scram_final_len(a)),
            HostOp::ScramVerify => return Ok(self.scram_verify(a)),
            HostOp::SecretNarrow => return Ok(self.secret_policy(t, op, a)),
            HostOp::SecretDeriveHmac => return Ok(self.secret_derive(t, a)),
            HostOp::SecretHmac | HostOp::SecretVerifyHmac => return Ok(self.secret_mac(op, a)),
            HostOp::SecretWrap | HostOp::SecretUnwrap | HostOp::SecretSign => {
                return Ok(self.secret_key(t, op, a));
            }
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
                | HostOp::ExecNarrow
                | HostOp::NetNarrow
                | HostOp::NetNarrowTls
                | HostOp::NetNarrowResolve
                | HostOp::NetNarrowUnix
                | HostOp::NetSplit
                | HostOp::NetJoin
        ) {
            self.check_cancel(t)?;
        }
        Ok(match op {
            HostOp::Connect
            | HostOp::Listen
            | HostOp::Accept
            | HostOp::RecvBounded
            | HostOp::RecvReady
            | HostOp::SendPair => {
                return self.network_host(t, op, a);
            }
            HostOp::TlsConnect
            | HostOp::SendSecret
            | HostOp::ScramSha256
            | HostOp::ScramFinalLen
            | HostOp::ScramVerify
            | HostOp::SecretSeal
            | HostOp::SecretNarrow
            | HostOp::SecretDeriveHmac
            | HostOp::SecretHmac
            | HostOp::SecretVerifyHmac
            | HostOp::SecretWrap
            | HostOp::SecretUnwrap
            | HostOp::SecretSign
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
            | HostOp::ProcessTrigger
            | HostOp::ProvideStdout
            | HostOp::ProvideStderr
            | HostOp::PortNext
            | HostOp::CallWord
            | HostOp::CallSecret
            | HostOp::CallPayload
            | HostOp::PortReply
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec
            | HostOp::PortFill
            | HostOp::StreamTerminal => {
                unreachable!()
            }
            // The model world grants no Cred (TLS-7), so no call reaches here.
            HostOp::CredFetch => unreachable!("a model world has no Cred"),
            HostOp::ExecSpawn | HostOp::ExecWait | HostOp::ExecSignal => {
                return self.exec_call(t, op, a);
            }
            HostOp::ExecNarrow => self.exec_narrow(t, a),
            HostOp::Resolve => return self.network_resolve(t, a),
            HostOp::UnixConnect
            | HostOp::UnixListen
            | HostOp::UnixAccept
            | HostOp::DatagramOpen
            | HostOp::DatagramBind
            | HostOp::DatagramConnect
            | HostOp::DatagramSend
            | HostOp::DatagramSendTo
            | HostOp::DatagramRecv => {
                return self.unix_host(t, op, a);
            }
            HostOp::NetNarrowUnix => {
                use hirsel_core_net::{GrantRule, UnixDirection, UnixRule};
                let rule = UnixDirection::from_tag(a[1] as u8).and_then(|dir| {
                    let root = std::str::from_utf8(self.carrier_bytes(a[2])).ok()?;
                    let path = std::str::from_utf8(self.carrier_bytes(a[3])).ok()?;
                    UnixRule::new(dir, root, path)
                });
                match rule {
                    Some(rule) => self.narrow_net(t, a[0], GrantRule::Unix(rule)),
                    None => error(1),
                }
            }
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
            HostOp::NetSplit if self.port_socket_part(a[0], true) => {
                vec![a[0] | 1, 0, 0, a[0] | 2]
            }
            HostOp::NetSplit => {
                let (read, write) = self.world.network.sockets.split(a[0]);
                vec![read, 0, 0, write]
            }
            HostOp::NetJoin
                if (a[0] & !3) == (a[1] & !3)
                    && !self.world.tls_writes.contains_key(&(a[0] & !3))
                    && self.port_join(a[0]) =>
            {
                ok(a[0] & !3, 0)
            }
            HostOp::NetJoin => vec![a[0], 1, 0, a[1]],
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
            HostOp::SleepUntil | HostOp::SleepUntilWall => {
                // Ready if the deadline has passed, else park in W until its
                // timer (os.rs `sleep_wait`); a cancellation that takes effect
                // there fails the sleep Cancelled. The model's wall and
                // monotonic readings are one counter, so a wall wait is this
                // same wait.
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
                self.world.clock = self.world.clock.saturating_add(1);
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
                    read_only_dir: a[0] & DIR_READ_ONLY != 0,
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
                    Some(Node::Dir(dir)) => ok(*dir | (a[0] & DIR_READ_ONLY), 0),
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
                let i = file(a[0]);
                if self.world.files[i]
                    .as_ref()
                    .is_some_and(|f| f.read_only_dir)
                {
                    return Ok(error(DENIED));
                }
                if positioned && a[1].checked_add(bytes.len() as u64).is_none() {
                    return Err(Stop::Cause("Bounds"));
                }
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
    fn load_map_hash(&self, p: u64) -> hirsel_core_layout::map_hash::State {
        hirsel_core_layout::map_hash::State(std::array::from_fn(|i| self.load(p + i as u64 * 8, 8)))
    }

    fn store_map_hash(&mut self, p: u64, state: hirsel_core_layout::map_hash::State) {
        for (i, word) in state.0.into_iter().enumerate() {
            self.store(p + i as u64 * 8, 8, word);
        }
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
    fn socket_drop(&mut self, key: u64) {
        if let Some(payer) = self.world.exec_handles.remove(&key) {
            self.refund(payer, charge::EXEC_HANDLE);
        }
        if key & 3 == 2 {
            self.world.tls_writes.remove(&(key & !3));
        }
        if !self.port_socket_part(key, false) {
            self.world.network.sockets.release(key);
        }
    }

    fn child_end_taken(&mut self, child: usize) -> bool {
        if self.children[child].unit_owner.is_none() {
            return self.sched.tasks[child + 1].state == State::Ended;
        }
        if !self.children[child].end_taken {
            self.take_unit_notices();
        }
        self.children[child].end_taken
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
                self.socket_drop(self.load(at, 8));
                false
            }
            FieldTy::Handle(Handle::Secret) => {
                self.secret_drop(self.load(at, 8));
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
                if (SCRAM_BASE..PORT_HANDLE).contains(&h) {
                    let (_, payer) = self.world.scrams.remove(&h).unwrap();
                    self.refund(payer, 32);
                    return false;
                }
                self.trace.push(h);
                if (PORT_HANDLE..CALL_BASE).contains(&h) {
                    // A served Dir's file or listing: the server learns by
                    // Released (FAIL-13).
                    self.port_release(h);
                    return false;
                }
                if h >= CALL_BASE || (SERVED_BASE..SERVED_BASE + 0x10_0000).contains(&h) {
                    // A dropped Call settles its caller as sealed; a cleaned
                    // Served end seals its port.
                    self.port_drop(h);
                    return false;
                }
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
                while !self.child_end_taken(i) {
                    self.park_task(t, Condition::Join(i));
                }
                self.event(JOIN, id, 2);
                let (status, result, r) = {
                    let c = &self.children[i];
                    (c.status, c.result, c.r)
                };
                // CONC-8: an abandoned handle cleans the result or the chain.
                let failed = if let Some(item) = self.children[i].crossed_result.take() {
                    self.cross_discard(&item, &self.cross_owners(r));
                    false
                } else {
                    status == 0 && self.drop_value(t, result, r)
                };
                for c in std::mem::take(&mut self.children[i].chain) {
                    if c.message[2] != 0 {
                        self.free(c.message[0]);
                    }
                }
                for (l, n) in self.settle_child(i, 0) {
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
                    if self.load(entry + e.meta, 8) != 0 {
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
            FieldTy::Handle(Handle::Client) => {
                self.client_drop(self.load(at, 8));
                false
            }
            FieldTy::Handle(Handle::Inbox) => {
                t.shield += 1;
                self.inbox_drop(self.load(at, 8));
                t.shield -= 1;
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
    /// went with one the child caught. A helper's end take returned all but
    /// `size(R)`, the result's content and what it left behind; the join
    /// returns the first two less the content `moved` into the result.
    fn settle_child(&mut self, i: usize, moved: u64) -> Vec<(Limit, u64)> {
        let c = &mut self.children[i];
        assert!(!c.joined, "task handle used twice");
        c.joined = true;
        let n = if c.unit_owner.is_some() {
            c.end_kept - moved
        } else {
            c.charge - c.n - charge::CAUSE_CELL
        };
        let l = c.limit.take().unwrap();
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

    fn take_steps(&mut self, t: &mut Task, mut count: u64) -> X<()> {
        while count != 0 {
            let Some(bound) = t.step_bounds.last_mut() else {
                self.steps += count;
                break;
            };
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
            let amount = count.min(bound.left).min(65_536 - t.slice_steps);
            bound.left -= amount;
            count -= amount;
            t.slice_steps += amount;
            self.steps += amount;
            if t.slice_steps == 65_536 {
                t.slice_steps = 0;
                self.event(hirsel_core_layout::event::STEP_SLICE, 0, 65_536);
                self.park_turn(None, false);
            }
        }
        Ok(())
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
            RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r } => {
                self.deny_describe_capability();
                let unit = matches!(op, RtOp::SpawnUnit { .. });
                let x = TypeRef(x.0);
                let r = TypeRef(r.0);
                let n = a[0];
                let charge = charge::task(self.logical(r), n).unwrap_or(u64::MAX);
                if let Err(room) = self.admit(cur, charge) {
                    // x stays the caller's
                    return Ok(if unit {
                        vec![0, charge, room]
                    } else {
                        vec![0, room]
                    });
                }
                // CONC-15: then x's movable charge against n.
                let owners = if unit {
                    self.cross_owners(x)
                } else {
                    Vec::new()
                };
                let moved = self.cross_charge(a[1], &owners);
                if unit && moved > n {
                    self.refund(cur.clone(), charge);
                    return Ok(vec![0, moved, n]);
                }
                // The child's limit serves the child, the next scheduler record.
                let lim = Limit::new(n, (!unit).then(|| cur.clone()), self.sched.tasks.len());
                let xb = self.block(self.held(x), None, 0, None);
                if unit {
                    self.check_cross_sockets(a[1], &owners);
                    let item = self.cross_take(a[1], x, &owners);
                    lim.add(moved);
                    self.cross_put(xb, &item, &owners, &lim);
                } else {
                    let bytes = self.get(a[1], self.held(x) as usize).to_vec();
                    self.put(xb, &bytes);
                }
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
                    unit_owner: unit.then(|| self.sched.tasks[t.k].unit),
                    end_taken: false,
                    crossed_result: None,
                    unit_limit: None,
                    end_kept: 0,
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
                    unit: if unit {
                        i + 1
                    } else {
                        self.sched.tasks[t.k].unit
                    },
                    notices: VecDeque::new(),
                });
                self.links.push(None);
                self.sched.ready.push_back(i + 1);
                self.event(SPAWN, id, n);
                if unit {
                    vec![TASK_BASE + i as u64, 0, 0]
                } else {
                    vec![TASK_BASE + i as u64, 0]
                }
            }
            RtOp::Join { r } => {
                let i = self.child(a[0]);
                let id = self.sched.tasks[i + 1].id;
                // CONC-6: a cancellation point at entry and at each wait.
                loop {
                    self.check_cancel(t)?;
                    if self.child_end_taken(i) {
                        break;
                    }
                    self.park_task(t, Condition::Join(i));
                }
                let status = self.children[i].status;
                let mut moved = 0;
                if status == 0 {
                    if let Some(item) = self.children[i].crossed_result.take() {
                        let payer = self.children[i].limit.as_ref().unwrap().clone();
                        self.cross_put(a[1], &item, &self.cross_owners(TypeRef(r.0)), &payer);
                        moved = item.charge;
                    } else {
                        let n = self.held(TypeRef(r.0)) as usize;
                        let bytes = self.get(self.children[i].result, n).to_vec();
                        self.put(a[1], &bytes);
                    }
                }
                self.event(JOIN, id, u64::from(status != 0));
                let cells = self.settle_child(i, moved);
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
            RtOp::SecretShare => vec![self.secret_share(a[0])],
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
            RtOp::MapHashBytes => {
                let bytes = if a[1] == 0 {
                    &[]
                } else {
                    self.get(a[0], a[1] as usize)
                };
                vec![hirsel_core_layout::map_hash::bytes(
                    &self.world.map_hash_key,
                    bytes,
                )]
            }
            RtOp::MapHashWord => vec![hirsel_core_layout::map_hash::word(
                &self.world.map_hash_key,
                a[0],
            )],
            RtOp::MapHashStart => {
                let state = hirsel_core_layout::map_hash::State::new(&self.world.map_hash_key);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapPlaceWord => {
                #[cfg(feature = "map-observation")]
                if let Some(stats) = &mut self.world.maps {
                    stats.last_word_strong = false;
                }
                vec![hirsel_core_layout::map_index::fast_word(
                    a[0],
                    self.world.map_fast_salt[0],
                )]
            }
            RtOp::MapPlaceBytes | RtOp::MapStrongBytes => {
                let bytes = if a[1] == 0 {
                    &[]
                } else {
                    self.get(a[0], a[1] as usize)
                };
                let [s0, s1] = self.world.map_fast_salt;
                vec![if op == RtOp::MapPlaceBytes {
                    hirsel_core_layout::map_index::fast_bytes(bytes, s0, s1)
                } else {
                    hirsel_core_layout::map_hash::bytes(&self.world.map_strong_key, bytes)
                }]
            }
            RtOp::MapStrongWord => {
                #[cfg(feature = "map-observation")]
                if let Some(stats) = &mut self.world.maps {
                    stats.last_word_strong = true;
                }
                vec![hirsel_core_layout::map_hash::word(
                    &self.world.map_strong_key,
                    a[0],
                )]
            }
            RtOp::MapStrongStart => {
                let state = hirsel_core_layout::map_hash::State::new(&self.world.map_strong_key);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapHashAddWord => {
                let mut state = self.load_map_hash(a[0]);
                state.word(a[1]);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapHashAddBytes => {
                let mut state = self.load_map_hash(a[0]);
                let bytes = if a[2] == 0 {
                    &[]
                } else {
                    self.get(a[1], a[2] as usize)
                };
                state.bytes(bytes);
                self.store_map_hash(a[0], state);
                vec![]
            }
            RtOp::MapHashFinish => vec![self.load_map_hash(a[0]).finish()],
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
            RtOp::ZeroBytes => {
                if a[2] != 0 {
                    self.put(a[1], &vec![0; a[2] as usize]);
                }
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
            RtOp::Crc32c | RtOp::Crc64Nvme => {
                let (poly, mask) = if op == RtOp::Crc32c {
                    (0x82f63b78, u64::from(u32::MAX))
                } else {
                    (0x9a6c9329ac4bc9b5, u64::MAX)
                };
                let mut crc = a[0] ^ mask;
                if a[2] != 0 {
                    for &byte in self.get(a[1], a[2] as usize) {
                        crc = crc_zero_byte(crc ^ u64::from(byte), poly);
                    }
                }
                vec![crc ^ mask]
            }
            RtOp::Crc32cCombine => vec![crc_combine::<32>(a[0], a[1], a[2], 0x82f63b78)],
            RtOp::Crc64NvmeCombine => vec![crc_combine::<64>(a[0], a[1], a[2], 0x9a6c9329ac4bc9b5)],
            RtOp::Ed25519Verify | RtOp::P256Verify | RtOp::P256VerifyDer => {
                use hirsel_core_net::SignatureAlgorithm;
                let algorithm = match op {
                    RtOp::Ed25519Verify => SignatureAlgorithm::Ed25519,
                    RtOp::P256Verify => SignatureAlgorithm::P256Fixed,
                    RtOp::P256VerifyDer => SignatureAlgorithm::P256Der,
                    _ => unreachable!(),
                };
                let range = |data, len| {
                    if len == 0 {
                        &[]
                    } else {
                        self.get(data, len as usize)
                    }
                };
                let valid = hirsel_core_net::verify_signature(
                    algorithm,
                    range(a[0], a[1]),
                    range(a[2], a[3]),
                    range(a[4], a[5]),
                );
                vec![u64::from(valid)]
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
            RtOp::ConnNew { item } => self.conn_new(&cur.clone(), item, a[0], a[1])?,
            RtOp::ConnClient => self.conn_client(a[0]),
            RtOp::ConnSend { .. } | RtOp::ConnTrySend { .. } => {
                let wait = matches!(op, RtOp::ConnSend { .. });
                self.conn_send(t, wait, a[0], a[1])?
            }
            RtOp::ConnReceive { .. } | RtOp::ConnTryReceive { .. } => {
                let wait = matches!(op, RtOp::ConnReceive { .. });
                let cur = cur.clone();
                self.conn_receive(t, &cur, wait, a[0], a[1])?
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
            // Only the release lowering of a native program forms these
            // (returnable.rs); the evaluator runs the module before it.
            RtOp::FrameRecord
            | RtOp::Returnable(_)
            | RtOp::ReceiveRec { .. }
            | RtOp::Transfer { .. } => {
                unreachable!("returnable waits are native-only")
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
        #[cfg(feature = "map-observation")]
        let (probe, rebuild, promotes_rebuild, strong) = if let Some(stats) = &mut self.world.maps {
            let definition = &m.instances[m.functions[f.index()].instance.index()].definition;
            let probe = definition.is_core("map.probe");
            let rebuild = definition.is_core("map.rebuild");
            if probe {
                stats.groups = 0;
            }
            if rebuild {
                stats.rebuilding = true;
                stats.rebuilds += 1;
            }
            (
                probe,
                rebuild,
                stats.rebuilding && definition.is_core("map.promote"),
                stats.last_word_strong,
            )
        } else {
            (false, false, false, false)
        };
        let r = self.activation(t, f, args);
        #[cfg(feature = "map-observation")]
        if let Some(stats) = &mut self.world.maps {
            if probe {
                if strong {
                    stats.strong_searches += 1;
                    stats.strong_groups += stats.groups;
                    stats.strong_peak = stats.strong_peak.max(stats.groups);
                } else {
                    stats.fast_peak = stats.fast_peak.max(stats.groups);
                }
            }
            if rebuild {
                stats.rebuilding = false;
                stats.promoted_in_rebuild = false;
            }
            if promotes_rebuild {
                stats.promoted_in_rebuild = true;
            }
        }
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
                    #[cfg(feature = "map-observation")]
                    if let Inst::BuiltinUse { definition } = inst
                        && let Some(stats) = &mut self.world.maps
                    {
                        if definition.is_core("map.group_match") {
                            stats.groups += 1;
                        } else if definition.is_core("map.set_strong") {
                            stats.promotions += 1;
                            stats.rebuild_promotions += u64::from(stats.rebuilding);
                        } else if stats.promoted_in_rebuild && definition.is_core("map.install") {
                            stats.resumed_installs += 1;
                        }
                    }
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
            Inst::Step { .. } => self.take_steps(t, 1)?,
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
            Inst::Move { to, from, bytes } => {
                let n = vals[bytes.index()] as usize;
                if n == 0 {
                    return Ok(());
                }
                self.touch(from, places[from.index()].0, n);
                self.touch(to, places[to.index()].0, n);
                let f = places[from.index()].0;
                self.read_marked(f, n);
                // Read the whole source first, so an overlapping target sees the old bytes.
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
fn crc_zero_byte(mut remainder: u64, poly: u64) -> u64 {
    for _ in 0..8 {
        remainder = (remainder >> 1) ^ (poly & (remainder & 1).wrapping_neg());
    }
    remainder
}

fn crc_combine<const WIDTH: usize>(mut a: u64, b: u64, mut len_b: u64, poly: u64) -> u64 {
    // Each column describes what a zero byte does to one bit of the CRC.
    fn apply<const WIDTH: usize>(columns: &[u64; WIDTH], mut value: u64) -> u64 {
        let mut result = 0;
        while value != 0 {
            result ^= columns[value.trailing_zeros() as usize];
            value &= value - 1;
        }
        result
    }

    let mut power: [u64; WIDTH] = std::array::from_fn(|bit| crc_zero_byte(1 << bit, poly));
    while len_b != 0 {
        if len_b & 1 != 0 {
            a = apply(&power, a);
        }
        len_b >>= 1;
        if len_b != 0 {
            power = power.map(|column| apply(&power, column));
        }
    }
    a ^ b
}

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
