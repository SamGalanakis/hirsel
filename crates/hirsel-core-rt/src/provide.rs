//! Virtual providers made by the program (core.md, Provide; CAP-2, CAP-5): the
//! clock, random and process cells. A cell is an ordinary provider behind a
//! dynamic grant, so callers reach it through the same entry, cancellation and
//! admission path as the OS provider. Its reads answer in `issue`, never switch
//! task and never allocate. Its waits (`sleep_until`, a network deadline on the
//! clock, `clock.idle`, `stopped`) park on the cell and are readied only by the
//! program: `clock.advance_to`, `process.trigger`, or the scheduler's
//! quiescence (`quiesce`). Host time never reaches a cell.
//!
//! Each cell admits `charge::PROVIDE_CELL` from the limit current at its
//! construction (MEM-24) and is withdrawn when that limit ends (`cells`).
mod cells;
mod clock;
mod net;
mod payload;
mod quiet;
pub(crate) use cells::{Cells, RETIRED, pinned, withdraw_limit};
use cells::{WITHDRAWN, cell, live};
pub(crate) use quiet::Scope;
use quiet::{IDLE, SERVERS, register};
pub(crate) mod random;
mod replies;
mod udp;
use net::net_port;
pub(crate) use net::{exec_port, join_socket, release_socket, split_socket};
use replies::{answer, unix_reply};
pub(crate) use udp::release_udp;

use crate::exec_runtime::{Call, ProcCall, SpawnCall};
use crate::unix_net::{
    SERVED_DATAGRAM_RECV, SERVED_UNIX_LISTEN, UnixField, UnixHandle, UnixOperation, UnixReply,
    UnixRequest,
};
use crate::{Context, causes, provider::*};
#[cfg(test)]
pub(crate) use clock::hirsel_core_clock_advance_to;
pub(crate) use clock::{ChannelClock, retire_deadline};
use clock::{ClockCell, call_until};
use hirsel_core_ids::dir_port::DirOp;
use hirsel_core_layout::charge::PROVIDE_CELL;
use hirsel_core_net::{
    exec::{ExecRules, served as exec},
    random_stream::RandomStream,
};
use std::{
    collections::BTreeMap,
    mem::ManuallyDrop,
    ptr,
    sync::{Mutex, MutexGuard, OnceLock},
};

enum Cell {
    Clock(Mutex<ClockCell>),
    Random(Mutex<RandomStream>),
    Process(Mutex<ProcessCell>),
    Port(Box<Mutex<Port>>),
}

#[derive(Default)]
struct ProcessCell {
    stopped: bool,
    waiters: Vec<u64>,
}

/// A served port (NOTES 15, 21): its calls by id, in issue order, each Queued
/// (not taken), Taken (the server holds its `Call`) or Settled.
#[derive(Default)]
struct Port {
    vocab: Vocab,
    /// Cleaning the `Served` end seals the port: every unsettled and later
    /// call completes as `sealed` answers.
    sealed: bool,
    /// Its limit ended (MEM-24): sealed, every call settled and every
    /// operation but a close failing with Bounds.
    withdrawn: bool,
    /// The outcome a sealed port gives: `Closed` for Dir and the streams.
    closed: Outcome,
    /// The order of calls and Released notices: a call's id is its stamp.
    next_id: u64,
    calls: BTreeMap<u64, PortCall>,
    /// `next` waits: the token and whether quiescence answered it `None`.
    servers: Vec<(u64, bool)>,
    /// The server's live objects by id (id 0 is the root Dir).
    handles: BTreeMap<u64, Handle>,
    /// The room admitted at construction and the bytes of it in use.
    room: u64,
    used: u64,
    /// A Net port's caller socket and listener keys (direction bits clear)
    /// and the server ids they name (NOTES 31).
    keys: BTreeMap<u64, u64>,
    udp_retired: BTreeMap<u64, crate::udp_net::served::Retirement>,
    /// A Net port's root rules: every address, pinned with the port, so
    /// `net.narrow` intersects them as any Net grant's.
    rules: Option<crate::net::RulesStorage>,
    exec_grant: Option<Box<crate::exec::Grant>>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Vocab {
    #[default]
    Stream,
    Dir,
    Net,
    Exec,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Entries,
    Socket,
    Listener,
    UnixListener,
    Datagram,
    Udp,
    Proc,
    PipeRead,
    PipeWrite,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Live,
    /// An explicit close was issued: the caller's handle is gone.
    Closing,
    /// Dropped: `next` delivers `Released(id)` in stamp order (FAIL-13).
    Released(u64),
    /// A split socket half whose `Released` the server has taken.
    Gone,
}

#[derive(Clone, Copy)]
struct Handle {
    kind: Kind,
    /// The object's state, or a split socket's read half.
    state: State,
    /// A split socket's write half; None until `net.split`.
    write: Option<State>,
}

impl Handle {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            state: State::Live,
            write: None,
        }
    }
    /// Every part's notice has been taken: the record leaves the room.
    fn done(&self) -> bool {
        self.state == State::Gone && self.write.is_none_or(|w| w == State::Gone)
    }
}

/// A live handle record's share of the room, with its release notice.
const HANDLE_ROOM: u64 = 32;
/// NetOp variants in declaration order.
const NET_CONNECT: u64 = 0;
const NET_RESOLVE: u64 = 1;
const NET_LISTEN: u64 = 2;
const NET_ACCEPT: u64 = 3;
const NET_RECV: u64 = 4;
const NET_SEND: u64 = 5;
const NET_HANDSHAKE: u64 = 6;
const NET_SEND_SECRET: u64 = 7;
const NET_RELEASED: u64 = 8;
/// After the Unix variants (9 to 17).
const NET_READY: u64 = 18;
const NET_LOCAL_ADDR: u64 = 19;
const NET_PEER_ADDR: u64 = 20;
/// After the UDP variants (21 to 25): TLS-9's server handshake.
const NET_HELLO: u64 = 26;
const NET_SERVE: u64 = 27;
/// DirReply and StreamReply variants as `Reply[Op]` tags; NetReply shares
/// Done, Failed and Count.
use hirsel_core_ids::dir_port::reply::{
    COUNT as R_COUNT, DIR as R_DIR, DONE as R_DONE, ENTRIES as R_ENTRIES, FAILED as R_FAILED,
    FILE as R_FILE, LOCKED as R_LOCKED, STAT as R_STAT, WRITE_FAILED as R_WRITE_FAILED,
};
/// NetReply's own variants after the shared Done, Failed and Count.
const R_SOCKET: u64 = 9;
const R_LISTENER: u64 = 10;
const R_SECURED: u64 = 11;
const R_UNIX_SOCKET: u64 = 12;
const R_UNIX_LISTENING: u64 = 13;
const R_DATAGRAM: u64 = 14;
const R_PACKET: u64 = 15;
const R_ADDRESS: u64 = 16;
/// After UdpSockets (17) and UdpPackets (18).
const R_ACCEPTED: u64 = 19;

/// A port caller's socket and listener keys: the registry's generation-0
/// band with bit 33 set, so they never meet a registry key (generation 1 and
/// up) or a model key (small and sequential). Process-wide, so two ports'
/// keys never meet either (NOTES 31).
const PORT_KEYS: u64 = 1 << 33;
static NEXT_PORT_KEY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct PortCall {
    /// The caller's operation; 0 for a Released notice, which has none.
    token: u64,
    /// The caller's request, pinned until the caller collects the outcome
    /// or its abort settles the call; read only while the call is unsettled.
    request: *mut Request,
    /// The call's `Op` as scalars, copied at issue: tag, then fields.
    words: [u64; 9],
    taken: bool,
    /// The server holds this call's `Call`.
    held: bool,
    /// The reply, `Call` drop or seal that settled it, not yet collected.
    answer: Option<Outcome>,
    /// The caller collected the answer, or its abort settled the call.
    finished: bool,
    /// Reply bytes `fill` staged in the room.
    staged: Vec<u8>,
    staged_packets: Vec<hirsel_core_net::udp::Packet>,
    /// A SendSecret's Secrets, narrowed to the stream's name at issue.
    secret: Option<Vec<crate::secret::Secret>>,
}

enum Queued {
    Call(u64),
    /// A handle's notice: its id, and whether it is a split socket's write half.
    Released(u64, bool),
}

/// The words of a Released notice for `id`'s `part` (Whole 0, Read 1, Write 2).
fn notice(vocab: Vocab, id: u64, part: u64) -> [u64; 9] {
    let mut words = [0; 9];
    match vocab {
        Vocab::Net => words[..3].copy_from_slice(&[NET_RELEASED, id, part]),
        Vocab::Exec => words[..3].copy_from_slice(&[exec::RELEASED, id, part]),
        _ => words[..2].copy_from_slice(&[DirOp::Released.word(), id]),
    }
    words
}

fn is_notice(vocab: Vocab, op: u64) -> bool {
    match vocab {
        Vocab::Stream => false,
        Vocab::Dir => op == DirOp::Released.word(),
        Vocab::Net => op == NET_RELEASED,
        Vocab::Exec => op == exec::RELEASED,
    }
}

impl Port {
    fn closed_for(&self, op: u64) -> Outcome {
        if self.vocab == Vocab::Exec && exec::exec_error_op(op) {
            crate::exec_runtime::failure(4, 0)
        } else {
            self.closed
        }
    }
    fn close_call(&mut self, id: u64) -> Option<u64> {
        let op = self.calls.get(&id)?.words[0];
        self.settle(id, self.closed_for(op))
    }
    fn issue_key(&mut self, id: u64) -> u64 {
        let seq = NEXT_PORT_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = PORT_KEYS | (seq << 2);
        self.keys.insert(key, id);
        key
    }
    fn free_staged(&mut self, id: u64) {
        if let Some(call) = self.calls.get_mut(&id) {
            self.used -= call.staged.len() as u64 + udp::packet_charge(call.staged_packets.len());
            call.staged = Vec::new();
            call.staged_packets = Vec::new();
        }
    }
    /// A Close call ends: a server that took it knows the file is gone; one
    /// that never saw it learns by `Released`.
    fn closed_call(&mut self, id: u64) {
        let Some(call) = self.calls.get(&id) else {
            return;
        };
        if self.vocab != Vocab::Dir || call.words[0] != DirOp::Close.word() {
            return;
        }
        let (taken, file) = (call.taken, call.words[1]);
        if taken {
            if self.handles.remove(&file).is_some() {
                self.used -= HANDLE_ROOM;
            }
        } else if let Some(h) = self.handles.get_mut(&file) {
            h.state = State::Released(self.next_id);
            self.next_id += 1;
        }
    }
    /// Settle an unsettled call; returns its caller's token to ready.
    fn settle(&mut self, id: u64, outcome: Outcome) -> Option<u64> {
        let call = self.calls.get_mut(&id)?;
        if call.answer.is_some() || call.finished {
            return None;
        }
        call.answer = Some(outcome);
        let token = call.token;
        self.free_staged(id);
        self.closed_call(id);
        Some(token)
    }
    /// Forget a call once neither its caller nor a `Call` refers to it.
    fn prune(&mut self, id: u64) {
        if self.calls.get(&id).is_some_and(|c| c.finished && !c.held) {
            self.free_staged(id);
            self.calls.remove(&id);
        }
    }
    fn by_token(&self, token: u64) -> Option<u64> {
        self.calls
            .iter()
            .find(|(_, c)| c.token == token && c.token != 0 && !c.finished)
            .map(|(&id, _)| id)
    }
    /// The oldest queued call or Released notice.
    fn queued(&self) -> Option<Queued> {
        let call = self
            .calls
            .iter()
            .find(|(_, c)| !c.taken && !c.finished && c.answer.is_none())
            .map(|(&id, _)| id);
        let released = self
            .handles
            .iter()
            .flat_map(|(&h, r)| {
                let read = match r.state {
                    State::Released(at) => Some((at, h, false)),
                    _ => None,
                };
                let write = match r.write {
                    Some(State::Released(at)) => Some((at, h, true)),
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
    fn live(&self, handle: u64, kind: Kind) -> bool {
        self.handles
            .get(&handle)
            .is_some_and(|h| h.kind == kind && h.state == State::Live)
    }
    /// A Net caller's socket or listener: its server id, when the key's part
    /// (whole, read or write half) is live and of the operation's kind.
    fn socket(&self, key: u64, kind: Kind) -> Option<u64> {
        let id = *self.keys.get(&(key & !3))?;
        let h = self.handles.get(&id)?;
        let state = match key & 3 {
            0 if h.write.is_none() => h.state,
            1 if h.write.is_some() || kind == Kind::PipeRead => h.state,
            2 if kind == Kind::PipeWrite => h.state,
            2 => h.write?,
            _ => return None,
        };
        (h.kind == kind && state == State::Live).then_some(id)
    }
    /// A Net call's words: an address operation on the port's Net (its rules
    /// already permitted the address), or an operation on a live key.
    fn exec_words(&self, request: &Request) -> Option<[u64; 9]> {
        let mut words = [0; 9];
        let b = request.buffer;
        match request.operation {
            EXEC_SPAWN => {
                // SAFETY: the spawn adapter exclusively lends the typed call.
                let call =
                    unsafe { &mut (*request.path.cast_mut().cast::<Call<SpawnCall>>()).data };
                let launch = &call.launch;
                call.served_payloads = launch.served_payloads();
                words = [
                    exec::SPAWN,
                    launch.program.len() as u64,
                    call.served_payloads[0].len() as u64,
                    call.served_payloads[1].len() as u64,
                    launch.dir.len() as u64,
                    launch.path.len() as u64,
                    u64::from(launch.piped[0]),
                    u64::from(launch.piped[1]),
                    u64::from(launch.piped[2]),
                ];
            }
            EXEC_WAIT | EXEC_SIGNAL if self.live(request.handle, Kind::Proc) => {
                let op = if request.operation == EXEC_WAIT {
                    exec::AWAIT
                } else {
                    exec::SIGNAL
                };
                words[..3].copy_from_slice(&[op, request.handle, u64::from(request.mode)]);
            }
            RECV_BOUNDED | SEND_PAIR => {
                // SAFETY: the pipe I/O adapter pins its request and any tail.
                let net = unsafe { &*request.path.cast::<crate::net::NetworkRequest>() };
                if request.operation == RECV_BOUNDED {
                    let id = self.socket(request.handle, Kind::PipeRead)?;
                    let room = b.capacity.saturating_sub(b.len) as u64;
                    words[..4].copy_from_slice(&[exec::RECV, id, room, net.max_receive]);
                } else {
                    let id = self.socket(request.handle, Kind::PipeWrite)?;
                    let tail = unsafe { (*net.send_tail).len };
                    words[..4].copy_from_slice(&[exec::SEND, id, b.len as u64, tail as u64]);
                }
            }
            _ => return None,
        }
        Some(words)
    }

    /// The call's words for a caller's request, or None when its handle is
    /// not one of this port's live objects of the operation's kind.
    fn words(&self, request: &Request) -> Option<[u64; 9]> {
        match self.vocab {
            Vocab::Net => return self.net_words(request),
            Vocab::Exec => return self.exec_words(request),
            _ => {}
        }
        let h = request.handle;
        let b = request.buffer;
        let spare = (b.capacity.saturating_sub(b.len)) as u64;
        let path = request.path_len as u64;
        let op = DirOp::from_host(request.operation)?;
        let (kind, fields) = match op {
            DirOp::Open => (Kind::Dir, [h, path, request.mode as u64]),
            DirOp::OpenDir
            | DirOp::Stat
            | DirOp::Mkdir
            | DirOp::Remove
            | DirOp::RemoveTree
            | DirOp::Entries => (Kind::Dir, [h, path, 0]),
            DirOp::Mkdtemp | DirOp::Mkstemp => (Kind::Dir, [h, b.len as u64, spare]),
            DirOp::Rename => {
                let split = rename_split(request) as u64;
                (Kind::Dir, [h, split, path.saturating_sub(split + 1)])
            }
            DirOp::SyncDir | DirOp::ZeroExtension => (Kind::Dir, [h, 0, 0]),
            DirOp::Read => (Kind::File, [h, spare, 0]),
            DirOp::ReadAt => (Kind::File, [h, request.offset(), spare]),
            DirOp::Write => (Kind::File, [h, b.len as u64, 0]),
            DirOp::WriteAt => (Kind::File, [h, request.offset(), b.len as u64]),
            DirOp::ZeroAt => (Kind::File, [h, request.offset(), request.zero_len()]),
            DirOp::StatFile
            | DirOp::Sync
            | DirOp::TryLock
            | DirOp::TryLockShared
            | DirOp::Unlock
            | DirOp::Close => (Kind::File, [h, 0, 0]),
            DirOp::NextEntries => (Kind::Entries, [h, spare, 0]),
            // A notice is queued at a drop, never issued by a caller.
            DirOp::Released => return None,
        };
        let [f1, f2, f3] = fields;
        self.live(h, kind)
            .then_some([op.word(), f1, f2, f3, 0, 0, 0, 0, 0])
    }
}

/// Rename's combined path is `from`, NUL, `to` (host_ops.rs).
fn rename_split(request: &Request) -> usize {
    // SAFETY: the caller's adapter keeps its combined path across `drive`.
    let path = unsafe { request_bytes(request.path, request.path_len) };
    path.iter().position(|&b| b == 0).unwrap_or(path.len())
}

unsafe fn request_bytes<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: the caller pins its request views while the call is unsettled.
        unsafe { std::slice::from_raw_parts(data, len) }
    }
}

/// A data field of an unsettled call: the bytes the caller's request holds.
/// # Safety
/// The call is unsettled, so its caller is parked inside `drive`.
unsafe fn field_bytes<'a>(vocab: Vocab, call: &PortCall, field: u64) -> &'a [u8] {
    // SAFETY: forwarded.
    unsafe {
        let r = &*call.request;
        let buffer = request_bytes(r.buffer.data, r.buffer.len);
        if vocab == Vocab::Net
            && let Some(op) = UnixOperation::describe(r.operation)
        {
            let unix = &*r.path.cast::<UnixRequest>();
            return match usize::try_from(field).ok().and_then(|i| op.fields.get(i)) {
                Some(UnixField::Root(_)) => unix.root.as_bytes(),
                Some(UnixField::Path) => unix.path.as_bytes(),
                Some(UnixField::Bytes) => buffer,
                _ => &[],
            };
        }
        if vocab == Vocab::Dir {
            let path = request_bytes(r.path, r.path_len);
            return match (DirOp::from_word(call.words[0]), field) {
                (
                    Some(
                        DirOp::Open
                        | DirOp::OpenDir
                        | DirOp::Stat
                        | DirOp::Mkdir
                        | DirOp::Remove
                        | DirOp::RemoveTree
                        | DirOp::Entries,
                    ),
                    1,
                ) => path,
                (Some(DirOp::Mkdtemp | DirOp::Write | DirOp::Mkstemp), 1)
                | (Some(DirOp::WriteAt), 2) => buffer,
                (Some(DirOp::Rename), 1) => &path[..call.words[2] as usize],
                (Some(DirOp::Rename), 2) => &path[call.words[2] as usize + 1..],
                _ => &[],
            };
        }
        match (vocab, call.words[0], field) {
            (Vocab::Stream, 0, 0) => buffer,
            (Vocab::Net, NET_RESOLVE, 0) => {
                crate::tls::bytes(&(*r.path.cast::<crate::resolve::ResolveRequest>()).raw_name)
            }
            (Vocab::Net, udp::SEND, _) => crate::udp_net::served::payload(r, field),
            (Vocab::Exec, exec::SPAWN, _) => {
                let call = &(*r.path.cast::<Call<SpawnCall>>()).data;
                match field {
                    0 => call.launch.program.as_bytes(),
                    1 => &call.served_payloads[0],
                    2 => &call.served_payloads[1],
                    3 => call.launch.dir.as_bytes(),
                    4 => call.launch.path.as_bytes(),
                    _ => &[],
                }
            }
            (Vocab::Exec, exec::SEND, 1) | (Vocab::Net, NET_SEND | NET_SEND_SECRET, 1) => buffer,
            (Vocab::Exec, exec::SEND, 2) | (Vocab::Net, NET_SEND | NET_SEND_SECRET, 2) => {
                let n = &*r.path.cast::<crate::net::NetworkRequest>();
                request_bytes((*n.send_tail).data, (*n.send_tail).len)
            }
            // A handshake's canonical name and its offer as ALPN wire
            // bytes (NOTES 32).
            (Vocab::Net, NET_HANDSHAKE, 1) => {
                crate::tls::view_bytes(&(*r.path.cast::<crate::tls::TlsRequest>()).name)
            }
            (Vocab::Net, NET_HANDSHAKE, 2) => {
                crate::tls::view_bytes(&(*r.path.cast::<crate::tls::TlsRequest>()).alpn)
            }
            // A server handshake's chain run and its ALPN wire bytes; the
            // key stays with the run (TLS-9).
            (Vocab::Net, NET_SERVE, 1) => crate::tls::view_bytes(
                &(*r.path.cast::<crate::tls_server::TlsAcceptRequest>()).chain,
            ),
            (Vocab::Net, NET_SERVE, 2) => crate::tls::view_bytes(
                &(*r.path.cast::<crate::tls_server::TlsAcceptRequest>()).alpn,
            ),
            _ => &[],
        }
    }
}

/// The spare capacity a call offers for reply bytes.
fn call_room(vocab: Vocab, words: &[u64; 9]) -> u64 {
    if vocab == Vocab::Dir {
        return match DirOp::from_word(words[0]) {
            Some(DirOp::Read | DirOp::NextEntries) => words[2],
            Some(DirOp::Mkdtemp | DirOp::ReadAt | DirOp::Mkstemp) => words[3],
            _ => 0,
        };
    }
    match (vocab, words[0]) {
        (Vocab::Exec, exec::RECV) => words[2].min(words[3]),
        (Vocab::Net, NET_RECV | NET_RESOLVE | NET_HELLO | SERVED_DATAGRAM_RECV | udp::RECV) => {
            words[2]
        }
        _ => 0,
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const PROVIDER: Provider = Provider {
    state: ptr::null_mut(),
    wait,
    issue,
    resume,
    abort,
    deliver,
};

struct SchedulerHooks {
    wait: unsafe extern "C" fn(*mut u8, *mut Request, u64) -> Wait,
    quiesce: fn(Scope) -> bool,
    forget: fn(&mut Cells),
    drop_cells: fn(&mut Cells),
}

// A ordinary provider run has no cell callbacks. The first cell installs the
// hooks before publishing its provider, so a native program that constructs no
// cell does not link their wait, quiescence or cleanup paths through startup.
static SCHEDULER_HOOKS: OnceLock<SchedulerHooks> = OnceLock::new();

/// Whether `provider` is a cell's. Its waits are the program's to ready, so the
/// scheduler does not count them as host work still to come (FAIL-15).
pub(crate) fn owns(provider: &Provider) -> bool {
    SCHEDULER_HOOKS
        .get()
        .is_some_and(|hooks| std::ptr::fn_addr_eq(provider.wait, hooks.wait))
}

fn deadline(request: &Request) -> u64 {
    if request.buffer.len < 8 {
        return 0;
    }
    // SAFETY: SLEEP_UNTIL carries its eight-byte deadline in the buffer.
    unsafe { request.buffer.data.cast::<u64>().read_unaligned() }
}

fn park() -> Wait {
    Wait {
        kind: WAIT_PARK,
        deadline_ns: 0,
    }
}

/// W: a deadline that is reached, a woken idle wait and a stopped process are
/// ready; any other wait registers its token and parks on the cell.
unsafe extern "C" fn wait(state: *mut u8, request: *mut Request, token: u64) -> Wait {
    // SAFETY: the scheduler passes this cell's pinned state and request.
    let (cell, request) = unsafe { (cell(state), &*request) };
    match (cell, request.operation) {
        (Cell::Clock(clock), SLEEP_UNTIL) => {
            let at = deadline(request);
            let mut clock = lock(clock);
            if at <= clock.mono || !live(state, request.handle) {
                return Wait::default();
            }
            if !clock.timers.iter().any(|&(_, _, t)| t == token) {
                let registration = clock.next_registration();
                clock.timers.push((at, registration, token));
            }
            park()
        }
        (Cell::Clock(clock), SLEEP_UNTIL_WALL) => {
            let at = deadline(request);
            let mut clock = lock(clock);
            if at <= clock.wall || !live(state, request.handle) {
                return Wait::default();
            }
            if !clock.wall_timers.iter().any(|&(_, t)| t == token) {
                clock.wall_timers.push((at, token));
            }
            park()
        }
        (Cell::Clock(c), CLOCK_IDLE) => {
            let mut clock = lock(c);
            if !live(state, request.handle) {
                return Wait::default();
            }
            if let Some(i) = clock.idle.iter().position(|&(t, _)| t == token) {
                if clock.idle[i].1 {
                    clock.idle.remove(i);
                    return Wait::default();
                }
            } else {
                clock.idle.push((token, false));
                register(&IDLE, token, state);
            }
            park()
        }
        (Cell::Process(process), STOPPED) => {
            let mut process = lock(process);
            if process.stopped || !live(state, request.handle) {
                return Wait::default();
            }
            if !process.waiters.contains(&token) {
                process.waiters.push(token);
            }
            park()
        }
        // Port operations wait in I: a caller's issue queues its call, and
        // `next` parks in issue until a call is queued or the run quiesces.
        _ => Wait::default(),
    }
}

fn pending() -> Step {
    Step {
        kind: STEP_PENDING,
        ..Step::default()
    }
}

/// NetError TimedOut (tag 6).
const TIMED_OUT: Outcome = Outcome {
    value: 0,
    error: IoError { tag: 6, code: 0 },
    written: 0,
    status: 0,
    kind: 0,
};

/// Wait for a queued call's answer until its deadline, or time it out.
/// # Safety
/// `request` is the caller's pinned request on this port.
unsafe fn await_call(port: &Mutex<Port>, request: &Request, token: u64) -> Step {
    // SAFETY: this call pins its adapter and clock.
    let until = unsafe { call_until(lock(port).vocab, request) };
    match unsafe { crate::os_net::deadline(until, token) } {
        crate::os_net::Due::Passed => {
            unsafe { collect(port, request, token, Some(timed_out(request.operation))) }
                .map_or_else(pending, done)
        }
        due => Step {
            kind: STEP_PENDING,
            deadline_ns: due.host().unwrap_or(0),
            ..Step::default()
        },
    }
}

fn timed_out(operation: u8) -> Outcome {
    if operation == EXEC_WAIT {
        crate::exec_runtime::failure(2, 0)
    } else {
        TIMED_OUT
    }
}

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

/// `next`: take the oldest queued call (`written` 1, `value` its id), answer
/// `None` once quiescence woke this wait, or register and stay pending.
fn next_step(state: *mut u8, port: &Mutex<Port>, token: u64) -> Step {
    let mut port = lock(port);
    if port.withdrawn {
        return done(WITHDRAWN);
    }
    let queued = port.queued();
    let quiesced = port.servers.iter().any(|&(t, q)| t == token && q);
    if queued.is_some() || quiesced {
        port.servers.retain(|&(t, _)| t != token);
        lock(&SERVERS).remove(&token);
        let mut retired = None;
        let id = match queued {
            None => return done(Outcome::default()),
            Some(Queued::Call(id)) => {
                let call = port.calls.get_mut(&id).expect("queued call");
                call.taken = true;
                call.held = true;
                id
            }
            Some(Queued::Released(handle, write)) => {
                // The notice's record was reserved with the handle: it leaves
                // the room with the last part's notice, and becomes a Call
                // that needs no reply.
                let h = port.handles.get_mut(&handle).expect("released handle");
                let part = match (h.kind, h.write, write) {
                    (Kind::PipeRead, _, _) | (_, Some(_), false) => 1,
                    (Kind::PipeWrite, _, _) | (_, Some(_), true) => 2,
                    _ => 0,
                };
                if write {
                    h.write = Some(State::Gone);
                } else {
                    h.state = State::Gone;
                }
                if h.done() {
                    port.handles.remove(&handle);
                    port.used -= HANDLE_ROOM;
                    retired = port.udp_retired.remove(&handle);
                }
                let id = port.next_id;
                port.next_id += 1;
                let words = notice(port.vocab, handle, part);
                port.calls.insert(
                    id,
                    PortCall {
                        token: 0,
                        request: ptr::null_mut(),
                        words,
                        taken: true,
                        held: true,
                        answer: None,
                        finished: true,
                        staged: Vec::new(),
                        staged_packets: Vec::new(),
                        secret: None,
                    },
                );
                id
            }
        };
        drop(port);
        drop(retired);
        return done(Outcome {
            value: id,
            written: 1,
            ..Outcome::default()
        });
    }
    if !port.servers.iter().any(|&(t, _)| t == token) {
        port.servers.push((token, false));
        register(&SERVERS, token, state);
    }
    pending()
}

/// A caller's resume or abort: collect the answer, or with `end` settle the
/// unanswered call with it: cancelled (status 1, which `deliver` records) or
/// timed out. A reply to a call settled so is Withdrawn.
/// # Safety
/// `request` is the caller's pinned request on this port.
unsafe fn collect(
    port: &Mutex<Port>,
    request: &Request,
    token: u64,
    end: Option<Outcome>,
) -> Option<Outcome> {
    let mut port = lock(port);
    let id = port.by_token(token)?;
    let call = port.calls.get_mut(&id).expect("live call");
    let outcome = match (call.answer, end) {
        (Some(answer), _) => answer,
        (None, Some(end)) => {
            port.free_staged(id);
            port.closed_call(id);
            end
        }
        (None, None) => return None,
    };
    port.calls.get_mut(&id).expect("live call").finished = true;
    port.prune(id);
    let vocab = port.vocab;
    drop(port);
    // SAFETY: the call still pins its adapter and clock through delivery.
    unsafe { crate::os_net::retire_deadline(call_until(vocab, request), token) };
    Some(outcome)
}

fn ready_all(tokens: Vec<u64>) {
    for token in tokens {
        crate::tasks::hirsel_core_ready(token);
    }
}

/// I: every cell operation finishes here.
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: as `wait`; RANDOM's buffer owns its spare capacity.
    let (cell, request) = unsafe { (cell(state), &mut *request) };
    let mut outcome = Outcome::default();
    let handle = request.handle;
    match (cell, request.operation) {
        (Cell::Clock(clock), MONO_NS | BOOT_NS | WALL_MS | CLOCK_IDLE) => {
            // A withdrawn cell's reads fail under the lock its withdrawal holds.
            let clock = lock(clock);
            if !live(state, handle) {
                return done(WITHDRAWN);
            }
            match request.operation {
                MONO_NS => outcome.value = clock.mono,
                BOOT_NS => outcome.value = clock.boot,
                WALL_MS => outcome.value = clock.wall,
                // The earliest deadline a task still waits on, if any.
                _ => {
                    if let Some(at) = clock.next_deadline() {
                        outcome.value = at;
                        outcome.written = 1;
                    }
                }
            }
        }
        (Cell::Random(random), RANDOM) => {
            let mut random = lock(random);
            if !live(state, handle) {
                return done(WITHDRAWN);
            }
            // SAFETY: the request pins its exclusively writable spare.
            outcome = unsafe { random::draw_request(&mut random, &mut request.buffer) };
        }
        // A sleep or stop wait that ended because its cell was withdrawn. A
        // dropped hand's close never fails.
        (Cell::Clock(clock), SLEEP_UNTIL | SLEEP_UNTIL_WALL) => {
            let _held = lock(clock);
            if !live(state, handle) {
                return done(WITHDRAWN);
            }
        }
        (Cell::Process(process), STOPPED) => {
            let _held = lock(process);
            if !live(state, handle) {
                return done(WITHDRAWN);
            }
        }
        (Cell::Port(port), PORT_NEXT) => return next_step(state, port, token),
        (Cell::Port(port), RELEASE | CLOSE)
            if request.operation == RELEASE || lock(port).vocab == Vocab::Exec =>
        {
            // A dropped derived handle (FAIL-13): mark it, allocating nothing;
            // `next` delivers the notice. A sealed port's server never asks.
            let mut p = lock(port);
            let stamp = p.next_id;
            let live = match p.handles.get_mut(&request.handle) {
                Some(h) if h.state == State::Live => {
                    h.state = State::Released(stamp);
                    true
                }
                _ => false,
            };
            if live {
                p.next_id += 1;
                let servers = p.servers.iter().map(|&(t, _)| t).collect();
                drop(p);
                ready_all(servers);
            }
        }
        (Cell::Port(port), operation) if operation != CLOSE_CALL && operation != CLOSE_SERVED => {
            // SAFETY: a caller's operation pins its request through delivery.
            let until = unsafe { call_until(lock(port).vocab, request) };
            let passed =
                unsafe { crate::os_net::deadline(until, token) } == crate::os_net::Due::Passed;
            let mut p = lock(port);
            let words = match p.vocab {
                Vocab::Stream => {
                    let mut w = [0; 9];
                    w[1] = request.buffer.len as u64;
                    Some(w)
                }
                Vocab::Dir | Vocab::Net | Vocab::Exec => p.words(request),
            };
            if p.sealed {
                outcome = match request.operation {
                    CLOSE if p.withdrawn => Outcome::default(),
                    _ if p.withdrawn => WITHDRAWN,
                    EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL => crate::exec_runtime::failure(4, 0),
                    _ => p.closed,
                };
            } else if words.is_some() && passed {
                // A deadline already reached times out before queueing.
                outcome = timed_out(request.operation);
            } else if let Some(words) = words {
                // Scalars are copied now; the bytes stay the caller's until
                // `payload` copies them out of an unsettled call (NOTES 15).
                if p.vocab == Vocab::Dir && words[0] == DirOp::Close.word() {
                    if let Some(h) = p.handles.get_mut(&words[1]) {
                        h.state = State::Closing;
                    }
                }
                let id = p.next_id;
                p.next_id += 1;
                let secret = if p.vocab == Vocab::Net && words[0] == NET_SEND_SECRET {
                    // SAFETY: SEND_SECRET pins its SendRequest in `path`.
                    unsafe {
                        (*request.path.cast_mut().cast::<crate::secret::SendRequest>())
                            .narrowed
                            .take()
                    }
                } else {
                    None
                };
                p.calls.insert(
                    id,
                    PortCall {
                        token,
                        request: &raw mut *request,
                        words,
                        taken: false,
                        held: false,
                        answer: None,
                        finished: false,
                        staged: Vec::new(),
                        staged_packets: Vec::new(),
                        secret,
                    },
                );
                let servers = p.servers.iter().map(|&(t, _)| t).collect();
                drop(p);
                ready_all(servers);
                // SAFETY: as above.
                return unsafe { await_call(port, request, token) };
            } else {
                // Not this port's live object of the operation's kind: Denied
                // (IoError tag 2, NetError tag 1).
                let tag = if matches!(p.vocab, Vocab::Net | Vocab::Exec) {
                    1
                } else {
                    2
                };
                outcome.error = IoError { tag, code: 0 };
            }
            drop(p);
            // The deadline check can arm the clock even if no call was queued.
            // SAFETY: the request still pins the clock.
            unsafe { crate::os_net::retire_deadline(until, token) };
        }
        (Cell::Port(port), CLOSE_CALL) => {
            let mut p = lock(port);
            let id = request.handle;
            if let Some(call) = p.calls.get_mut(&id) {
                call.held = false;
            }
            let token = p.close_call(id);
            p.prune(id);
            drop(p);
            ready_all(token.into_iter().collect());
        }
        (Cell::Port(port), CLOSE_SERVED) => {
            let mut p = lock(port);
            p.sealed = true;
            let ids: Vec<u64> = p.calls.keys().copied().collect();
            let tokens = ids.into_iter().filter_map(|id| p.close_call(id)).collect();
            let retired = std::mem::take(&mut p.udp_retired);
            for id in retired.keys() {
                p.handles.remove(id);
                p.used -= HANDLE_ROOM;
            }
            drop(p);
            drop(retired);
            ready_all(tokens);
        }
        _ => {}
    }
    done(outcome)
}

/// R: a caller collects its answer; a readied `next` looks again.
unsafe extern "C" fn resume(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: as `wait`.
    let (cell, request) = unsafe { (cell(state), &*request) };
    match (cell, request.operation) {
        (Cell::Port(port), PORT_NEXT) => next_step(state, port, token),
        // SAFETY: the caller's request is pinned through delivery.
        (Cell::Port(port), _) => match unsafe { collect(port, request, token, None) } {
            Some(outcome) => done(outcome),
            // SAFETY: the caller's request stays pinned while it waits.
            None => unsafe { await_call(port, request, token) },
        },
        _ => Step::default(),
    }
}

/// A: an answered call keeps its answer (the reply committed first); an
/// unanswered one ends timed out at its deadline and cancelled otherwise, and
/// a `next` ends cancelled.
unsafe extern "C" fn abort(
    state: *mut u8,
    request: *mut Request,
    token: u64,
    reason: u8,
) -> Outcome {
    // SAFETY: as `wait`.
    let (cell, request) = unsafe { (cell(state), &*request) };
    let cancelled = Outcome {
        status: 1,
        ..Outcome::default()
    };
    match (cell, request.operation) {
        (Cell::Port(port), PORT_NEXT) => {
            lock(port).servers.retain(|&(t, _)| t != token);
            lock(&SERVERS).remove(&token);
            cancelled
        }
        (Cell::Port(port), _) => {
            // A host deadline that fired times the call out (NOTES 35).
            let end = if reason == ABORT_DEADLINE {
                timed_out(request.operation)
            } else {
                match (lock(port).vocab, request.operation) {
                    (Vocab::Exec, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) => {
                        crate::exec_runtime::failure(3, 0)
                    }
                    (Vocab::Net | Vocab::Exec, _) => crate::os_net::signal(7),
                    _ => cancelled,
                }
            };
            // SAFETY: the caller's request is pinned through delivery.
            unsafe { collect(port, request, token, Some(end)) }.unwrap_or(end)
        }
        (cell, _) => {
            quiet::forget_wait(cell, token);
            Outcome::default()
        }
    }
}

/// D: a port operation the scheduler cancelled in I records its cause.
unsafe extern "C" fn deliver(_: *mut u8, request: *mut Request, outcome: Outcome) -> Outcome {
    if outcome.status == 1 {
        // SAFETY: deliver runs in the task, which owns its context.
        unsafe {
            let observed =
                crate::tasks::check_cancel(crate::tasks::current_context(), (*request).site);
            debug_assert_eq!(observed, 1);
        }
    }
    outcome
}

fn install_hooks() {
    SCHEDULER_HOOKS.get_or_init(|| SchedulerHooks {
        wait: PROVIDER.wait,
        quiesce: quiet::wake_quiescent,
        forget: cells::forget_cells,
        drop_cells: cells::drop_cells,
    });
}

unsafe fn files<'a>(context: *mut Context) -> &'a crate::host_file::Files {
    // SAFETY: a running context pins its run's files.
    unsafe { &*(*context).host_files }
}

/// The cell a live hand or trigger designates.
/// The cell a live hand designates, its state and the hand's generation.
/// None once a withdrawal forgot the hand.
unsafe fn held<'a>(context: *mut Context, key: u64) -> Option<(&'a Cell, *mut u8, u64)> {
    // SAFETY: source typing supplies a live ClockHand or StopTrigger.
    unsafe {
        let registry = &files(context).registry;
        let state = registry.file_provider(key).state;
        (!state.is_null()).then(|| (cell(state), state, registry.file_handle(key)))
    }
}

/// A hand's operation on a withdrawn cell fails with `Bounds` (MEM-24).
unsafe fn withdrawn(context: *mut Context, site: u64) -> Outcome {
    // SAFETY: a running context records the cause.
    unsafe { causes::record(&mut *context, causes::Cause::new(causes::BOUNDS, site)) };
    CANCELLED
}

/// `provide.clock(start_mono_ns, start_wall_ms) -> (Clock, ClockHand)`.
/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_clock(
    context: *mut Context,
    mono: u64,
    wall: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    // SAFETY: forwarded.
    let provider = match unsafe {
        files.registry.run_grants().cells.add(
            context,
            || {
                Cell::Clock(Mutex::new(ClockCell {
                    mono,
                    boot: mono,
                    wall,
                    ..ClockCell::default()
                }))
            },
            PROVIDE_CELL,
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    files
        .registry
        .run_grants()
        .cells
        .designate(files, provider, Some(CLOSE_CELL))
}

/// `provide.random(seed) -> Random`.
/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_random(
    context: *mut Context,
    seed: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    // SAFETY: forwarded.
    let provider = match unsafe {
        files.registry.run_grants().cells.add(
            context,
            || Cell::Random(Mutex::new(RandomStream::Seeded(seed))),
            PROVIDE_CELL,
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    files
        .registry
        .run_grants()
        .cells
        .designate(files, provider, None)
}

/// `provide.process() -> (Process, StopTrigger)`.
/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_process(context: *mut Context, _site: u64) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    // SAFETY: forwarded.
    let provider = match unsafe {
        files.registry.run_grants().cells.add(
            context,
            || Cell::Process(Mutex::new(ProcessCell::default())),
            PROVIDE_CELL,
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    files
        .registry
        .run_grants()
        .cells
        .designate(files, provider, Some(CLOSE_CELL))
}

/// `clock.idle(mut h) -> Option[u64] waits`: parks until the run is quiescent,
/// then answers the earliest deadline still waited on (`written` 1), or none.
/// # Safety
/// Context is running and `hand` is a live ClockHand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_clock_idle(
    context: *mut Context,
    hand: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded; the hand's provider is pinned for the run.
    unsafe {
        let provider = files(context).registry.file_provider(hand);
        let mut request = Request {
            operation: CLOCK_IDLE,
            mode: 0,
            handle: files(context).registry.file_handle(hand),
            path: ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: ptr::null_mut(),
                len: 0,
                capacity: 0,
            },
            site,
        };
        drive(context, &provider, &mut request, None)
    }
}

/// `process.trigger(take t)`: the process is stopped from now on, and every
/// `stopped` wait on it is readied.
/// # Safety
/// Context is running and `trigger` is a live StopTrigger, consumed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_process_trigger(
    context: *mut Context,
    trigger: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded; the trigger's carrier is consumed, never closed.
    let (provider, handle) = unsafe { files(context).registry.take_held(trigger) };
    if provider.state.is_null() {
        // SAFETY: forwarded.
        return unsafe { withdrawn(context, site) };
    }
    // SAFETY: the run pins the cell.
    let Cell::Process(process) = (unsafe { cell(provider.state) }) else {
        unreachable!("a StopTrigger designates a process cell")
    };
    let waiters = {
        let mut process = lock(process);
        if !live(provider.state, handle) {
            drop(process);
            // SAFETY: forwarded.
            return unsafe { withdrawn(context, site) };
        }
        process.stopped = true;
        std::mem::take(&mut process.waiters)
    };
    for token in waiters {
        crate::tasks::hirsel_core_ready(token);
    }
    Outcome::default()
}

fn empty_request(operation: u8, handle: u64, site: u64) -> Request {
    Request {
        operation,
        mode: 0,
        handle,
        path: ptr::null(),
        path_len: 0,
        buffer: Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
        site,
    }
}

/// `provide.stdout/stderr/dir(bytes n)`: a port with room `n`, admitted with
/// its cell's charge from the current limit until that limit ends. `value`
/// is the capability's grant (id 0, a Dir port's root) and `written` the
/// `Served` key; a refusal answers as `Cells::add`.
/// # Safety
/// Context is running.
unsafe fn provide_port(
    context: *mut Context,
    n: u64,
    vocab: Vocab,
    terminal: crate::host_file::Terminal,
) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    // A sealed port answers Closed (IoError tag 4), or for a Net Reset
    // (NetError tag 5).
    let closed = if matches!(vocab, Vocab::Net | Vocab::Exec) {
        5
    } else {
        4
    };
    let mut port = Port {
        vocab,
        closed: Outcome {
            error: IoError {
                tag: closed,
                code: 0,
            },
            ..Outcome::default()
        },
        room: n,
        ..Port::default()
    };
    if vocab == Vocab::Dir {
        port.handles.insert(0, Handle::new(Kind::Dir));
    }
    let mut root = 0;
    if vocab == Vocab::Net {
        // Every address in each direction and family (NOTES 31), a
        // handshake to any name and port under any trust (NOTES 32), and a
        // lookup of any name (NOTES 34).
        let addresses = hirsel_core_net::GrantRule::served_addresses();
        let tls = hirsel_core_net::GrantRule::Tls(hirsel_core_net::TlsRule::served());
        let resolve = hirsel_core_net::GrantRule::Resolve(hirsel_core_net::ResolveRule::served());
        let unix = hirsel_core_net::UnixDirection::ALL
            .into_iter()
            .map(|dir| hirsel_core_net::GrantRule::Unix(hirsel_core_net::UnixRule::served(dir)));
        let rules = crate::net::RulesStorage::new(addresses.chain([tls, resolve]).chain(unix));
        root = rules.handle();
        port.rules = Some(rules);
    }
    if vocab == Vocab::Exec {
        let grant = Box::new(crate::exec::Grant::Served {
            rules: ExecRules::universal(),
        });
        root = std::ptr::from_ref(&*grant) as u64;
        port.exec_grant = Some(grant);
    }
    let charge = PROVIDE_CELL.saturating_add(n);
    // SAFETY: forwarded.
    let provider = match unsafe {
        files.registry.run_grants().cells.add(
            context,
            || Cell::Port(Box::new(Mutex::new(port))),
            charge,
        )
    } {
        Ok((provider, _)) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant_with_terminal(provider, root, terminal),
        written: files.registry.held(provider, 0, CLOSE_SERVED),
        ..Outcome::default()
    }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_dir(
    context: *mut Context,
    n: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Dir, crate::host_file::Terminal::No) }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_net(
    context: *mut Context,
    n: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Net, crate::host_file::Terminal::No) }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_exec(
    context: *mut Context,
    n: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Exec, crate::host_file::Terminal::No) }
}

/// A virtual stream's terminal from its word (`provide.size_word`): zero is
/// not a terminal; otherwise bit 32 is set over `cols | rows << 16`.
fn fixed(size: u64) -> crate::host_file::Terminal {
    if size == 0 {
        crate::host_file::Terminal::No
    } else {
        crate::host_file::Terminal::Fixed(size)
    }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_stdout(
    context: *mut Context,
    n: u64,
    size: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Stream, fixed(size)) }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_stderr(
    context: *mut Context,
    n: u64,
    size: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Stream, fixed(size)) }
}

/// `provide.next(mut s) -> Option[Call[Op]] waits`: `written` 1 and `value`
/// the new `Call` key, or `written` 0 at quiescence.
/// # Safety
/// Context is running and `served` is a live `Served` key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_port_next(
    context: *mut Context,
    served: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded; the port is pinned for the run.
    unsafe {
        let registry = &files(context).registry;
        let provider = registry.file_provider(served);
        let mut request = empty_request(PORT_NEXT, 0, site);
        let mut outcome = drive(context, &provider, &mut request, None);
        if outcome.status == 0 && outcome.written == 1 {
            outcome.value = registry.held(provider, outcome.value, CLOSE_CALL);
        }
        outcome
    }
}

/// The port and call id a live `Call` key designates.
unsafe fn call_of<'a>(context: *mut Context, call: u64) -> (&'a Mutex<Port>, u64) {
    // SAFETY: source typing supplies a live Call.
    unsafe {
        let registry = &files(context).registry;
        let Cell::Port(port) = cell(registry.file_provider(call).state) else {
            unreachable!("a Call designates a port")
        };
        (port, registry.file_handle(call))
    }
}

/// A private word of a taken call's `Op`: 0 is its tag, then its fields.
/// # Safety
/// Context is running and `call` is a live `Call` key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_call_word(
    context: *mut Context,
    call: u64,
    i: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    let (port, id) = unsafe { call_of(context, call) };
    Outcome {
        value: lock(port).calls[&id]
            .words
            .get(i as usize)
            .copied()
            .unwrap_or(0),
        ..Outcome::default()
    }
}

/// `provide.secret(c, i)`: a SendSecret call's Secret at index `i`, narrowed
/// to the stream's name, as a new designation charged to the server's limit
/// (error tag 2 and the room, from which it raises Capacity). Any other call,
/// or an index past the call's secrets, fails the server.
/// # Safety
/// Context is running and `call` is a live `Call` key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_call_secret(
    context: *mut Context,
    call: u64,
    i: u64,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe {
        let (port, id) = call_of(context, call);
        let secret = lock(port).calls[&id]
            .secret
            .as_ref()
            .and_then(|secrets| secrets.get(i as usize).cloned());
        let Some(secret) = secret else {
            causes::record(&mut *context, causes::Cause::fail(Default::default(), site));
            return CANCELLED;
        };
        crate::secret::designate(context, secret)
    }
}

/// A Capacity refusal of `requested` room bytes: error tag 1, `written` the
/// bytes available, from which the server raises Capacity (MEM-21).
fn refused(requested: u64, available: u64) -> Outcome {
    Outcome {
        value: requested,
        error: IoError { tag: 1, code: 0 },
        written: available,
        ..Outcome::default()
    }
}

/// `provide.fill(mut s, mut c, b)`: stage the first bytes of `b` in the room,
/// at most what the call's `room` still offers; returns how many it took. A
/// room too short for them raises Capacity in the server.
/// # Safety
/// Context is running, `served` a live `Served` key, `call` a live `Call` key
/// and `b` a live Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_port_fill(
    context: *mut Context,
    served: u64,
    call: u64,
    b: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe {
        let (port, id) = call_of(context, call);
        let registry = &files(context).registry;
        let Cell::Port(own) = cell(registry.file_provider(served).state) else {
            unreachable!("a Served designates a port")
        };
        let mut p = lock(port);
        let vocab = p.vocab;
        let c = &p.calls[&id];
        if !ptr::eq(&**own, port) || c.finished || c.answer.is_some() {
            return Outcome::default();
        }
        let bytes = request_bytes((*b).data, (*b).len);
        let offered = call_room(vocab, &c.words).saturating_sub(c.staged.len() as u64);
        let take = offered.min(bytes.len() as u64);
        let available = p.room - p.used;
        if take > available {
            return refused(take, available);
        }
        p.used += take;
        let c = p.calls.get_mut(&id).expect("live call");
        c.staged.extend_from_slice(&bytes[..take as usize]);
        Outcome {
            value: take,
            ..Outcome::default()
        }
    }
}

/// The entries of ALPN wire bytes, each a length byte then its protocol.
fn alpn_count(mut wire: &[u8]) -> u64 {
    let mut count = 0;
    while let Some((&len, rest)) = wire.split_first() {
        wire = rest.get(usize::from(len)..).unwrap_or_default();
        count += 1;
    }
    count
}

/// `provide.reply`'s commit: settle the call with the reply's tag and words
/// and ready its caller. `value` 1 is Withdrawn: the call was settled first,
/// or its caller has a cancellation pending that it observes (NOTES 16). A
/// `Call` of another port is consumed as a dropped one and withdrawn (NOTES
/// 20). A reply that does not answer the call's operation, or names an id it
/// may not issue, settles the caller as sealed and fails the server (`Fail`);
/// a new handle the room cannot hold is a Capacity refusal.
/// # Safety
/// Context is running, `served` is a live `Served` key and `call` a live
/// `Call` key, consumed here.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn hirsel_core_port_reply(
    context: *mut Context,
    served: u64,
    call: u64,
    tag: u64,
    w0: u64,
    w1: u64,
    w2: u64,
    w3: u64,
    w4: u64,
    w5: u64,
    site: u64,
) -> Outcome {
    let w = [w0, w1, w2, w3, w4, w5];
    // SAFETY: forwarded; the Call's carrier is consumed, never closed.
    let (registry, (provider, id)) = unsafe {
        let registry = &files(context).registry;
        (registry, registry.take_held(call))
    };
    // SAFETY: the run pins the port.
    let Cell::Port(port) = (unsafe { cell(provider.state) }) else {
        unreachable!("a Call designates a port")
    };
    let withdrawn = Outcome {
        value: 1,
        ..Outcome::default()
    };
    let mut p = lock(port);
    if let Some(call) = p.calls.get_mut(&id) {
        call.held = false;
    }
    let foreign = registry.file_provider(served).state != provider.state;
    let notice = !foreign && is_notice(p.vocab, p.calls[&id].words[0]);
    if notice {
        // A Released notice needs no reply; `Done` acknowledges it.
        p.prune(id);
        if tag == R_DONE {
            return Outcome::default();
        }
        drop(p);
        // SAFETY: the server's running context records its failure.
        unsafe { causes::record(&mut *context, causes::Cause::fail(Default::default(), site)) };
        return CANCELLED;
    }
    let live = p
        .calls
        .get(&id)
        .is_some_and(|call| !call.finished && call.answer.is_none());
    if foreign || !live || crate::tasks::call_cancelled(p.calls[&id].token) {
        let token = if foreign { p.close_call(id) } else { None };
        p.free_staged(id);
        p.prune(id);
        drop(p);
        ready_all(token.into_iter().collect());
        return withdrawn;
    }
    let unix = if p.vocab == Vocab::Net {
        // SAFETY: a live call's request stays pinned until it settles.
        UnixOperation::describe(unsafe { (*p.calls[&id].request).operation })
            .map(|op| unix_reply(op.reply))
    } else {
        None
    };
    let Some(outcome) = answer(&p, &p.calls[&id], unix.map(|(reply, _)| reply), tag, w) else {
        let token = p.close_call(id);
        drop(p);
        ready_all(token.into_iter().collect());
        // SAFETY: the server's running context records its failure.
        unsafe { causes::record(&mut *context, causes::Cause::fail(Default::default(), site)) };
        return CANCELLED;
    };
    if p.vocab == Vocab::Net && udp::operation(p.calls[&id].words[0]) {
        return udp::reply(p, id, tag, w, outcome);
    }
    let mut outcome = outcome;
    let kind = match (p.vocab, tag) {
        (Vocab::Dir, R_FILE) => Some(Kind::File),
        (Vocab::Dir, R_DIR) => Some(Kind::Dir),
        (Vocab::Dir, R_ENTRIES) => Some(Kind::Entries),
        (Vocab::Net, R_SOCKET) => Some(Kind::Socket),
        (Vocab::Net, R_LISTENER) => Some(Kind::Listener),
        (Vocab::Net, tag) => unix
            .filter(|&(reply, _)| tag == reply)
            .and_then(|(_, kind)| kind),
        _ => None,
    };
    if let Some(kind) = kind {
        let available = p.room - p.used;
        if available < HANDLE_ROOM {
            let token = p.close_call(id);
            drop(p);
            ready_all(token.into_iter().collect());
            return refused(HANDLE_ROOM, available);
        }
        p.used += HANDLE_ROOM;
        p.handles.insert(w[0], Handle::new(kind));
        if p.vocab == Vocab::Net {
            // The caller's key, and the address it learns: a listener's
            // bound one, an accepted socket's peer.
            outcome.value = p.issue_key(w[0]);
        }
    }
    if p.vocab == Vocab::Net && matches!(tag, R_SOCKET | R_LISTENER | R_ADDRESS) {
        // SAFETY: the unsettled IP operation pins its address output.
        unsafe {
            let n = (*p.calls[&id].request)
                .path
                .cast_mut()
                .cast::<crate::net::NetworkRequest>();
            (*n).address = crate::net::Address {
                family: w[1] as u8,
                hi: w[2],
                lo: w[3],
                port: w[4] as u16,
            };
        }
    }
    if (p.vocab, tag) == (Vocab::Exec, exec::STARTED) {
        let needed = HANDLE_ROOM * w[..4].iter().filter(|&&id| id != 0).count() as u64;
        let available = p.room - p.used;
        if available < needed {
            let token = p.close_call(id);
            drop(p);
            ready_all(token.into_iter().collect());
            return refused(needed, available);
        }
        p.used += needed;
        p.handles.insert(w[0], Handle::new(Kind::Proc));
        let mut pipes = [0; 3];
        for (i, &pipe) in w[1..4].iter().enumerate().filter(|&(_, id)| *id != 0) {
            let (kind, part) = if i == 0 {
                (Kind::PipeWrite, 2)
            } else {
                (Kind::PipeRead, 1)
            };
            p.handles.insert(pipe, Handle::new(kind));
            pipes[i] = p.issue_key(pipe) | part;
        }
        // SAFETY: the caller is unsettled and lends SpawnCall through delivery.
        unsafe {
            let request = &*p.calls[&id].request;
            (*request.path.cast_mut().cast::<Call<SpawnCall>>())
                .data
                .pipes = pipes;
        }
    }
    if (p.vocab, tag) == (Vocab::Net, R_SECURED) {
        // The chosen protocol and the peer digest (big-endian words) go to
        // the caller's outputs; the stream's write half is now live.
        // SAFETY: the call is unsettled, so its caller's request is pinned.
        unsafe {
            let c = &p.calls[&id];
            let tls = &*(*c.request).path.cast::<crate::tls::TlsRequest>();
            let protocol = &mut *tls.protocol_out;
            protocol.len = 0;
            if w[0] == 1 {
                let mut wire = crate::tls::view_bytes(&tls.alpn);
                for _ in 0..w[1] {
                    wire = &wire[1 + usize::from(wire[0])..];
                }
                let chosen = &wire[1..1 + usize::from(wire[0])];
                ptr::copy_nonoverlapping(chosen.as_ptr(), protocol.data, chosen.len());
                protocol.len = chosen.len();
            }
            let peer = &mut *tls.peer_out;
            for (i, word) in w[2..6].iter().enumerate() {
                peer[i * 8..i * 8 + 8].copy_from_slice(&word.to_be_bytes());
            }
            let key = (*c.request).handle;
            if let Some(&h) = p.keys.get(&(key & !3)) {
                p.handles.get_mut(&h).expect("live socket").write = Some(State::Live);
            }
        }
    }
    if (p.vocab, tag) == (Vocab::Net, R_ACCEPTED) {
        // The chosen protocol's index goes to the caller; the stream's
        // write half is now live.
        // SAFETY: the call is unsettled, so its caller's request is pinned.
        unsafe {
            let c = &p.calls[&id];
            let accept = &*(*c.request)
                .path
                .cast::<crate::tls_server::TlsAcceptRequest>();
            *accept.protocol_out = if w[0] == 1 { w[1] } else { u64::MAX };
            let key = (*c.request).handle;
            if let Some(&h) = p.keys.get(&(key & !3)) {
                p.handles.get_mut(&h).expect("live socket").write = Some(State::Live);
            }
        }
    }
    let c = &p.calls[&id];
    if !c.staged.is_empty()
        && !((p.vocab == Vocab::Exec
            || (p.vocab, c.words[0]) == (Vocab::Net, SERVED_DATAGRAM_RECV)
            || (p.vocab, c.words[0]) == (Vocab::Dir, DirOp::Mkstemp.word()))
            && outcome.error != IoError::OK)
    {
        // The commit copies the staged bytes into the caller's spare
        // capacity, which `fill` bounded by the call's room.
        // SAFETY: the call is unsettled, so its caller's request is pinned.
        unsafe {
            let b = &mut (*c.request).buffer;
            if (p.vocab, c.words[0]) == (Vocab::Net, NET_HELLO) {
                // The name goes to the Hello's record, never the buffer.
                let hello = &*(*c.request).path.cast::<crate::tls_server::HelloRequest>();
                let out = &mut *hello.name_out;
                ptr::copy_nonoverlapping(c.staged.as_ptr(), out.data, c.staged.len());
                out.len = c.staged.len();
            } else if (p.vocab, c.words[0]) == (Vocab::Net, NET_RESOLVE) {
                // Records become the caller's Ip elements; `answer` checked them.
                let addresses = hirsel_core_net::ip_records(&c.staged).expect("checked records");
                crate::resolve::append(b, &addresses);
            } else {
                ptr::copy_nonoverlapping(c.staged.as_ptr(), b.data.add(b.len), c.staged.len());
                b.len += c.staged.len();
            }
        }
    }
    let token = p.settle(id, outcome);
    drop(p);
    ready_all(token.into_iter().collect());
    Outcome::default()
}

/// The scheduler finds `scope` quiescent (FAIL-15): wake its `clock.idle`
/// waits. Returns whether it readied one; if not, the run stops with Deadlock.
pub(crate) fn quiesce(scope: Scope) -> bool {
    SCHEDULER_HOOKS
        .get()
        .is_some_and(|hooks| (hooks.quiesce)(scope))
}

/// Run teardown: forget the run's idle waits, free its cells and refund
/// their charges.
pub(crate) fn forget(cells: &mut Cells) {
    if let Some(hooks) = SCHEDULER_HOOKS.get() {
        (hooks.forget)(cells);
    }
}

#[cfg(test)]
#[path = "provide_tests.rs"]
mod tests;
