//! Virtual providers made by the program (core.md, Provide; CAP-2, CAP-5): the
//! clock, random and process cells. A cell is an ordinary provider behind a
//! dynamic grant, so callers reach it through the same entry, cancellation and
//! admission path as the OS provider. Its reads answer in `issue`, never switch
//! task and never allocate. Its waits (`sleep_until`, a network deadline on the
//! clock, `clock.idle`, `stopped`) park on the cell and are readied only by the
//! program: `clock.advance_to`, `process.trigger`, or the scheduler's
//! quiescence (`quiesce`). Host time never reaches a cell.
//!
//! Each cell admits `charge::PROVIDE_CELL` from the constructing payer, held
//! until teardown with the cell, as a narrowed grant's rule storage (MEM-24):
//! a capability is plain and copyable, so nothing earlier proves it gone.
use crate::{Context, causes, provider::*};
use hirsel_core_layout::charge::PROVIDE_CELL;
use std::{
    collections::BTreeMap,
    ptr,
    sync::{Mutex, MutexGuard},
};

enum Cell {
    Clock(Mutex<ClockCell>),
    Random(Mutex<u64>),
    Process(Mutex<ProcessCell>),
    Port(Mutex<Port>),
}

#[derive(Default)]
struct ClockCell {
    mono: u64,
    wall: u64,
    /// Waits on this clock's deadlines in registration order: a token stays
    /// until its deadline is reached; a stale one is pruned at quiescence.
    timers: Vec<(u64, u64)>,
    /// `clock.idle` waits: the token and whether quiescence has woken it.
    idle: Vec<(u64, bool)>,
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
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Vocab {
    #[default]
    Stream,
    Dir,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Entries,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Live,
    /// An explicit close was issued: the caller's handle is gone.
    Closing,
    /// Dropped: `next` delivers `Released(id)` in stamp order (FAIL-13).
    Released(u64),
}

#[derive(Clone, Copy)]
struct Handle {
    kind: Kind,
    state: State,
}

/// A live handle record's share of the room, with its release notice.
const HANDLE_ROOM: u64 = 32;
/// DirOp variants in declaration order (core.md, Provide).
const DIR_CLOSE: u64 = 18;
const DIR_RELEASED: u64 = 20;
/// DirReply and StreamReply variants as `Reply[Op]` tags.
const R_DONE: u64 = 0;
const R_FAILED: u64 = 1;
const R_WRITE_FAILED: u64 = 2;
const R_COUNT: u64 = 3;
const R_LOCKED: u64 = 4;
const R_STAT: u64 = 5;
const R_FILE: u64 = 6;
const R_DIR: u64 = 7;
const R_ENTRIES: u64 = 8;

struct PortCall {
    /// The caller's operation; 0 for a Released notice, which has none.
    token: u64,
    /// The caller's request, pinned until the caller collects the outcome
    /// or its abort settles the call; read only while the call is unsettled.
    request: *mut Request,
    /// The call's `Op` as scalars, copied at issue: tag, then fields.
    words: [u64; 8],
    taken: bool,
    /// The server holds this call's `Call`.
    held: bool,
    /// The reply, `Call` drop or seal that settled it, not yet collected.
    answer: Option<Outcome>,
    /// The caller collected the answer, or its abort settled the call.
    finished: bool,
    /// Reply bytes `fill` staged in the room.
    staged: Vec<u8>,
}

enum Queued {
    Call(u64),
    Released(u64),
}

impl Port {
    fn free_staged(&mut self, id: u64) {
        if let Some(call) = self.calls.get_mut(&id) {
            self.used -= call.staged.len() as u64;
            call.staged = Vec::new();
        }
    }
    /// A Close call ends: a server that took it knows the file is gone; one
    /// that never saw it learns by `Released`.
    fn closed_call(&mut self, id: u64) {
        let Some(call) = self.calls.get(&id) else {
            return;
        };
        if self.vocab != Vocab::Dir || call.words[0] != DIR_CLOSE {
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
            .filter_map(|(&h, r)| match r.state {
                State::Released(at) => Some((at, h)),
                _ => None,
            })
            .min();
        match (call, released) {
            (Some(c), Some((at, h))) if at < c => Some(Queued::Released(h)),
            (Some(c), _) => Some(Queued::Call(c)),
            (None, Some((_, h))) => Some(Queued::Released(h)),
            (None, None) => None,
        }
    }
    fn live(&self, handle: u64, kind: Kind) -> bool {
        self.handles
            .get(&handle)
            .is_some_and(|h| h.kind == kind && h.state == State::Live)
    }
    /// The call's words for a caller's request, or None when its handle is
    /// not one of this port's live objects of the operation's kind.
    fn words(&self, request: &Request) -> Option<[u64; 8]> {
        let h = request.handle;
        let b = request.buffer;
        let spare = (b.capacity.saturating_sub(b.len)) as u64;
        let path = request.path_len as u64;
        let mut w = [0; 8];
        let (kind, fields): (Kind, &[u64]) = match request.operation {
            OPEN => (Kind::Dir, &[0, h, path, request.mode as u64]),
            OPEN_DIR => (Kind::Dir, &[1, h, path]),
            MKDTEMP => (Kind::Dir, &[2, h, b.len as u64, spare]),
            STAT => (Kind::Dir, &[3, h, path]),
            MKDIR => (Kind::Dir, &[4, h, path]),
            REMOVE => (Kind::Dir, &[5, h, path]),
            REMOVE_TREE => (Kind::Dir, &[6, h, path]),
            RENAME => {
                let split = rename_split(request) as u64;
                w[..4].copy_from_slice(&[7, h, split, path.saturating_sub(split + 1)]);
                (Kind::Dir, &[])
            }
            SYNC_DIR => (Kind::Dir, &[8, h]),
            ENTRIES => (Kind::Dir, &[9, h]),
            READ => (Kind::File, &[10, h, spare]),
            READ_AT => {
                w[..4].copy_from_slice(&[11, h, request.offset(), spare]);
                (Kind::File, &[])
            }
            WRITE => (Kind::File, &[12, h, b.len as u64]),
            WRITE_AT => {
                w[..4].copy_from_slice(&[13, h, request.offset(), b.len as u64]);
                (Kind::File, &[])
            }
            STAT_FILE => (Kind::File, &[14, h]),
            SYNC => (Kind::File, &[15, h]),
            TRY_LOCK => (Kind::File, &[16, h]),
            UNLOCK => (Kind::File, &[17, h]),
            CLOSE => (Kind::File, &[DIR_CLOSE, h]),
            NEXT_ENTRIES => (Kind::Entries, &[19, h, spare]),
            _ => return None,
        };
        w[..fields.len()].copy_from_slice(fields);
        self.live(h, kind).then_some(w)
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
        let path = request_bytes(r.path, r.path_len);
        match (vocab, call.words[0], field) {
            (Vocab::Stream, 0, 0) => buffer,
            (Vocab::Dir, 0 | 1 | 3 | 4 | 5 | 6, 1) => path,
            (Vocab::Dir, 2 | 12, 1) | (Vocab::Dir, 13, 2) => buffer,
            (Vocab::Dir, 7, 1) => &path[..call.words[2] as usize],
            (Vocab::Dir, 7, 2) => &path[call.words[2] as usize + 1..],
            _ => &[],
        }
    }
}

/// The spare capacity a call offers for reply bytes.
fn call_room(vocab: Vocab, words: &[u64; 8]) -> u64 {
    match (vocab, words[0]) {
        (Vocab::Dir, 10 | 19) => words[2],
        (Vocab::Dir, 2 | 11) => words[3],
        _ => 0,
    }
}

/// Whether a reply variant answers the call's operation (core.md, Handles).
fn answers(vocab: Vocab, op: u64, reply: u64) -> bool {
    match vocab {
        Vocab::Stream => matches!(reply, R_DONE | R_WRITE_FAILED),
        Vocab::Dir => {
            (reply == R_FAILED && op != DIR_RELEASED)
                || match op {
                    0 => reply == R_FILE,
                    1 | 2 => reply == R_DIR,
                    3 | 14 => reply == R_STAT,
                    9 => reply == R_ENTRIES,
                    10 | 11 | 19 => reply == R_COUNT,
                    12 | 13 => matches!(reply, R_DONE | R_WRITE_FAILED),
                    16 => reply == R_LOCKED,
                    _ => reply == R_DONE,
                }
        }
    }
}

/// `next` tokens waiting anywhere in the process, with their port. A quiescent
/// run answers only the ones among its own parked operations.
static SERVERS: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());

/// The run's cells, each with the payer of its charge and the amount. Boxed
/// so each provider's state pointer stays put.
#[derive(Default)]
pub(crate) struct Cells(Mutex<Vec<(Box<Cell>, crate::limit::Payer, u64)>>);
// SAFETY: a payer designates a limit record the run pins until teardown,
// which refunds it under the run's ownership.
unsafe impl Send for Cells {}
// SAFETY: as Send; every access goes through the mutex.
unsafe impl Sync for Cells {}

/// `clock.idle` tokens waiting anywhere in the process, with their cell. A
/// quiescent run wakes only the ones among its own parked operations.
static IDLE: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe fn cell<'a>(state: *mut u8) -> &'a Cell {
    // SAFETY: the run pins every cell until teardown.
    unsafe { &*state.cast::<Cell>() }
}

const PROVIDER: Provider = Provider {
    state: ptr::null_mut(),
    wait,
    issue,
    resume,
    abort,
    deliver,
};

/// Whether `provider` is a cell's. Its waits are the program's to ready, so the
/// scheduler does not count them as host work still to come (FAIL-15).
pub(crate) fn owns(provider: &Provider) -> bool {
    std::ptr::fn_addr_eq(provider.wait, PROVIDER.wait)
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
            if at <= clock.mono {
                return Wait::default();
            }
            if !clock.timers.iter().any(|&(_, t)| t == token) {
                clock.timers.push((at, token));
            }
            park()
        }
        (Cell::Clock(c), CLOCK_IDLE) => {
            let mut clock = lock(c);
            if let Some(i) = clock.idle.iter().position(|&(t, _)| t == token) {
                if clock.idle[i].1 {
                    clock.idle.remove(i);
                    return Wait::default();
                }
            } else {
                clock.idle.push((token, false));
                lock(&IDLE).insert(token, state as usize);
            }
            park()
        }
        (Cell::Process(process), STOPPED) => {
            let mut process = lock(process);
            if process.stopped {
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
    let queued = port.queued();
    let quiesced = port.servers.iter().any(|&(t, q)| t == token && q);
    if queued.is_some() || quiesced {
        port.servers.retain(|&(t, _)| t != token);
        lock(&SERVERS).remove(&token);
        let id = match queued {
            None => return done(Outcome::default()),
            Some(Queued::Call(id)) => {
                let call = port.calls.get_mut(&id).expect("queued call");
                call.taken = true;
                call.held = true;
                id
            }
            Some(Queued::Released(handle)) => {
                // The notice's record was reserved with the handle: it leaves
                // the room now and becomes a Call that needs no reply.
                port.handles.remove(&handle);
                port.used -= HANDLE_ROOM;
                let id = port.next_id;
                port.next_id += 1;
                let mut words = [0; 8];
                words[0] = DIR_RELEASED;
                words[1] = handle;
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
                    },
                );
                id
            }
        };
        return done(Outcome {
            value: id,
            written: 1,
            ..Outcome::default()
        });
    }
    if !port.servers.iter().any(|&(t, _)| t == token) {
        port.servers.push((token, false));
        lock(&SERVERS).insert(token, state as usize);
    }
    pending()
}

/// A caller's resume or abort: collect the answer, or with `abort` settle the
/// unanswered call as cancelled (status 1, which `deliver` records).
fn collect(port: &Mutex<Port>, token: u64, abort: bool) -> Option<Outcome> {
    let mut port = lock(port);
    let id = port.by_token(token)?;
    let call = port.calls.get_mut(&id).expect("live call");
    let outcome = match call.answer {
        Some(answer) => answer,
        None if abort => {
            port.free_staged(id);
            port.closed_call(id);
            Outcome {
                status: 1,
                ..Outcome::default()
            }
        }
        None => return None,
    };
    port.calls.get_mut(&id).expect("live call").finished = true;
    port.prune(id);
    Some(outcome)
}

fn ready_all(tokens: Vec<u64>) {
    for token in tokens {
        crate::tasks::hirsel_core_ready(token);
    }
}

/// SplitMix64 (Steele, Lea and Flood 2014): eight little-endian bytes a step.
fn split_mix(state: &mut u64) -> [u8; 8] {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (z ^ (z >> 31)).to_le_bytes()
}

/// I: every cell operation finishes here.
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, token: u64) -> Step {
    // SAFETY: as `wait`; RANDOM's buffer owns its spare capacity.
    let (cell, request) = unsafe { (cell(state), &mut *request) };
    let mut outcome = Outcome::default();
    match (cell, request.operation) {
        (Cell::Clock(clock), MONO_NS) => outcome.value = lock(clock).mono,
        (Cell::Clock(clock), WALL_MS) => outcome.value = lock(clock).wall,
        (Cell::Clock(clock), CLOCK_IDLE) => {
            // The earliest deadline a task still waits on, if any.
            if let Some(at) = lock(clock).timers.iter().map(|&(at, _)| at).min() {
                outcome.value = at;
                outcome.written = 1;
            }
        }
        (Cell::Random(random), RANDOM) => {
            let mut random = lock(random);
            let spare = request.buffer.capacity.saturating_sub(request.buffer.len);
            let mut filled = 0;
            while filled < spare {
                let bytes = split_mix(&mut random);
                let n = (spare - filled).min(8);
                // SAFETY: bounded to the carrier's spare capacity.
                unsafe {
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        request.buffer.data.add(request.buffer.len + filled),
                        n,
                    )
                };
                filled += n;
            }
            request.buffer.len += filled;
            outcome.written = filled as u64;
        }
        (Cell::Port(port), PORT_NEXT) => return next_step(state, port, token),
        (Cell::Port(port), RELEASE) => {
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
            let mut p = lock(port);
            let words = match p.vocab {
                Vocab::Stream => {
                    let mut w = [0; 8];
                    w[1] = request.buffer.len as u64;
                    Some(w)
                }
                Vocab::Dir => p.words(request),
            };
            if p.sealed {
                outcome = p.closed;
            } else if let Some(words) = words {
                // Scalars are copied now; the bytes stay the caller's until
                // `payload` copies them out of an unsettled call (NOTES 15).
                if p.vocab == Vocab::Dir && words[0] == DIR_CLOSE {
                    if let Some(h) = p.handles.get_mut(&words[1]) {
                        h.state = State::Closing;
                    }
                }
                let id = p.next_id;
                p.next_id += 1;
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
                    },
                );
                let servers = p.servers.iter().map(|&(t, _)| t).collect();
                drop(p);
                ready_all(servers);
                return pending();
            } else {
                // Not this port's live object of the operation's kind.
                outcome.error = IoError { tag: 2, code: 0 };
            }
        }
        (Cell::Port(port), CLOSE_CALL) => {
            let mut p = lock(port);
            let id = request.handle;
            if let Some(call) = p.calls.get_mut(&id) {
                call.held = false;
            }
            let closed = p.closed;
            let token = p.settle(id, closed);
            p.prune(id);
            drop(p);
            ready_all(token.into_iter().collect());
        }
        (Cell::Port(port), CLOSE_SERVED) => {
            let mut p = lock(port);
            p.sealed = true;
            let closed = p.closed;
            let ids: Vec<u64> = p.calls.keys().copied().collect();
            let tokens = ids
                .into_iter()
                .filter_map(|id| p.settle(id, closed))
                .collect();
            drop(p);
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
        (Cell::Port(port), _) => collect(port, token, false).map_or_else(pending, done),
        _ => Step::default(),
    }
}

/// A: an answered call keeps its answer (the reply committed first); an
/// unanswered one, or a `next`, ends cancelled.
unsafe extern "C" fn abort(state: *mut u8, request: *mut Request, token: u64, _: u8) -> Outcome {
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
        (Cell::Port(port), _) => collect(port, token, true).unwrap_or(cancelled),
        _ => Outcome::default(),
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

impl Cells {
    /// Admit a cell's charge and pin it, or answer the refusal: value 0 and
    /// the available bytes in `written`, from which the caller raises Capacity.
    unsafe fn add(
        &self,
        context: *mut Context,
        cell: Cell,
        charge: u64,
    ) -> Result<Provider, Outcome> {
        // SAFETY: a running context has a current payer.
        let payer = unsafe { (*context).payer() };
        if let Err(capacity) = payer.admit(Some(charge)) {
            return Err(Outcome {
                written: capacity.available,
                ..Outcome::default()
            });
        }
        let mut cells = lock(&self.0);
        cells.push((Box::new(cell), payer, charge));
        Ok(Provider {
            state: (&raw const *cells.last().unwrap().0).cast_mut().cast(),
            ..PROVIDER
        })
    }
}

unsafe fn files<'a>(context: *mut Context) -> &'a crate::host_file::Files {
    // SAFETY: a running context pins its run's files.
    unsafe { &*(*context).host_files }
}

/// The cell a live hand or trigger designates.
unsafe fn held<'a>(context: *mut Context, key: u64) -> &'a Cell {
    // SAFETY: source typing supplies a live ClockHand or StopTrigger.
    unsafe { cell(files(context).registry.file_provider(key).state) }
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
        files.cells.add(
            context,
            Cell::Clock(Mutex::new(ClockCell {
                mono,
                wall,
                ..ClockCell::default()
            })),
            PROVIDE_CELL,
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant(provider, 0),
        written: files.registry.held(provider, 0, CLOSE_CELL),
        ..Outcome::default()
    }
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
        files
            .cells
            .add(context, Cell::Random(Mutex::new(seed)), PROVIDE_CELL)
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant(provider, 0),
        ..Outcome::default()
    }
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
        files.cells.add(
            context,
            Cell::Process(Mutex::new(ProcessCell::default())),
            PROVIDE_CELL,
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant(provider, 0),
        written: files.registry.held(provider, 0, CLOSE_CELL),
        ..Outcome::default()
    }
}

/// `clock.advance_to(mut h, mono_ns)`: time never moves back. Every wait whose
/// deadline is now reached is readied, earliest deadline first, ties in
/// registration order.
/// # Safety
/// Context is running and `hand` is a live ClockHand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_clock_advance_to(
    context: *mut Context,
    hand: u64,
    mono: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    let Cell::Clock(clock) = (unsafe { held(context, hand) }) else {
        unreachable!("a ClockHand designates a clock cell")
    };
    let mut due = Vec::new();
    {
        let mut clock = lock(clock);
        clock.mono = clock.mono.max(mono);
        let now = clock.mono;
        clock.timers.retain(|&(at, token)| {
            let reached = at <= now;
            if reached {
                due.push((at, token));
            }
            !reached
        });
    }
    due.sort_by_key(|&(at, _)| at);
    for (_, token) in due {
        crate::tasks::hirsel_core_ready(token);
    }
    Outcome::default()
}

/// `clock.set_wall(mut h, wall_ms)`: the wall clock may move either way.
/// # Safety
/// Context is running and `hand` is a live ClockHand.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_clock_set_wall(
    context: *mut Context,
    hand: u64,
    wall: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    let Cell::Clock(clock) = (unsafe { held(context, hand) }) else {
        unreachable!("a ClockHand designates a clock cell")
    };
    lock(clock).wall = wall;
    Outcome::default()
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
            handle: 0,
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
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded; the trigger's carrier is consumed, never closed.
    let (provider, _) = unsafe { files(context).registry.take_held(trigger) };
    // SAFETY: the run pins the cell.
    let Cell::Process(process) = (unsafe { cell(provider.state) }) else {
        unreachable!("a StopTrigger designates a process cell")
    };
    let waiters = {
        let mut process = lock(process);
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

/// `provide.stdout/stderr/dir(bytes n)`: a port with room `n`, admitted
/// with the port's own record and held until the run ends (NOTES 18). `value`
/// is the capability's grant (id 0, a Dir port's root) and `written` the
/// `Served` key; a refusal answers as `Cells::add`.
/// # Safety
/// Context is running.
unsafe fn provide_port(context: *mut Context, n: u64, vocab: Vocab) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    let mut port = Port {
        vocab,
        closed: Outcome {
            error: IoError { tag: 4, code: 0 },
            ..Outcome::default()
        },
        room: n,
        ..Port::default()
    };
    if vocab == Vocab::Dir {
        port.handles.insert(
            0,
            Handle {
                kind: Kind::Dir,
                state: State::Live,
            },
        );
    }
    let charge = PROVIDE_CELL.saturating_add(n);
    // SAFETY: forwarded.
    let provider = match unsafe {
        files
            .cells
            .add(context, Cell::Port(Mutex::new(port)), charge)
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant(provider, 0),
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
    unsafe { provide_port(context, n, Vocab::Dir) }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_stdout(
    context: *mut Context,
    n: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Stream) }
}

/// # Safety
/// Context is running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_stderr(
    context: *mut Context,
    n: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_port(context, n, Vocab::Stream) }
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
        value: lock(port).calls[&id].words[i.min(7) as usize],
        ..Outcome::default()
    }
}

/// `provide.payload(c, field, mut out)`: append the field's bytes while the
/// call is unsettled (a settled call's caller storage is gone: nothing); a
/// short spare capacity raises Bounds before copying anything.
/// # Safety
/// Context is running, `call` is a live `Call` key and `out` a live mutable
/// Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_call_payload(
    context: *mut Context,
    call: u64,
    field: u64,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded; an unsettled call's caller is parked, so its request
    // and the bytes it borrows are pinned.
    unsafe {
        let (port, id) = call_of(context, call);
        let port = lock(port);
        let c = &port.calls[&id];
        if c.finished || c.answer.is_some() {
            return Outcome::default();
        }
        let from = field_bytes(port.vocab, c, field);
        let out = &mut *out;
        if out.capacity.saturating_sub(out.len) < from.len() {
            causes::record(&mut *context, causes::Cause::new(causes::BOUNDS, site));
            return CANCELLED;
        }
        ptr::copy_nonoverlapping(from.as_ptr(), out.data.add(out.len), from.len());
        out.len += from.len();
        Outcome {
            written: from.len() as u64,
            ..Outcome::default()
        }
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
        if !ptr::eq(own, port) || c.finished || c.answer.is_some() {
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

/// The caller's outcome for a reply that answers its call, or None when the
/// reply breaks the port's rules (the server fails).
fn answer(p: &Port, call: &PortCall, tag: u64, w: [u64; 6]) -> Option<Outcome> {
    let op = call.words[0];
    if !answers(p.vocab, op, tag) {
        return None;
    }
    let io = |t: u64, code: u64| {
        ((1..=5).contains(&t)).then_some(IoError {
            tag: t as u8,
            code: code as u32,
        })
    };
    let mut out = Outcome::default();
    match tag {
        R_DONE => {
            out.written = match (p.vocab, op) {
                (Vocab::Stream, _) | (Vocab::Dir, 12) => {
                    call.words[2 - (p.vocab == Vocab::Stream) as usize]
                }
                (Vocab::Dir, 13) => call.words[3],
                _ => 0,
            }
        }
        R_FAILED => out.error = io(w[0], w[1])?,
        R_WRITE_FAILED => {
            let len = if op == 13 {
                call.words[3]
            } else {
                call.words[2 - (p.vocab == Vocab::Stream) as usize]
            };
            if w[0] > len {
                return None;
            }
            out.error = io(w[1], w[2])?;
            out.written = w[0];
        }
        R_COUNT => {
            // A read's count is the bytes it staged; a listing's, its entries.
            if op != 19 && w[0] != call.staged.len() as u64 {
                return None;
            }
            out.value = w[0];
        }
        R_LOCKED => out.value = (w[0] != 0) as u64,
        R_STAT => {
            out.kind = w[0].min(3) as u8;
            out.value = w[1];
            out.written = w[2];
        }
        _ => {
            // A new object: never the root, never an id already in use.
            if w[0] == 0 || p.handles.contains_key(&w[0]) {
                return None;
            }
            out.value = w[0];
        }
    }
    Some(out)
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
    let notice = !foreign && p.vocab == Vocab::Dir && p.calls[&id].words[0] == DIR_RELEASED;
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
        let closed = p.closed;
        let token = if foreign { p.settle(id, closed) } else { None };
        p.free_staged(id);
        p.prune(id);
        drop(p);
        ready_all(token.into_iter().collect());
        return withdrawn;
    }
    let closed = p.closed;
    let Some(outcome) = answer(&p, &p.calls[&id], tag, w) else {
        let token = p.settle(id, closed);
        drop(p);
        ready_all(token.into_iter().collect());
        // SAFETY: the server's running context records its failure.
        unsafe { causes::record(&mut *context, causes::Cause::fail(Default::default(), site)) };
        return CANCELLED;
    };
    let kind = match tag {
        R_FILE => Some(Kind::File),
        R_DIR => Some(Kind::Dir),
        R_ENTRIES => Some(Kind::Entries),
        _ => None,
    };
    if let Some(kind) = kind {
        let available = p.room - p.used;
        if available < HANDLE_ROOM {
            let token = p.settle(id, closed);
            drop(p);
            ready_all(token.into_iter().collect());
            return refused(HANDLE_ROOM, available);
        }
        p.used += HANDLE_ROOM;
        p.handles.insert(
            w[0],
            Handle {
                kind,
                state: State::Live,
            },
        );
    }
    let c = &p.calls[&id];
    if !c.staged.is_empty() {
        // The commit copies the staged bytes into the caller's spare
        // capacity, which `fill` bounded by the call's room.
        // SAFETY: the call is unsettled, so its caller's request is pinned.
        unsafe {
            let b = &mut (*c.request).buffer;
            ptr::copy_nonoverlapping(c.staged.as_ptr(), b.data.add(b.len), c.staged.len());
            b.len += c.staged.len();
        }
    }
    let token = p.settle(id, outcome);
    drop(p);
    ready_all(token.into_iter().collect());
    Outcome::default()
}

/// The scheduler finds its run quiescent (FAIL-15): wake the run's `clock.idle`
/// waits, after pruning the deadlines of operations no longer parked. Returns
/// whether it readied one; if not, the run stops with Deadlock.
pub(crate) fn quiesce(live: &dyn Fn(u64) -> bool) -> bool {
    let mut idle = lock(&IDLE);
    let woken: Vec<(u64, usize)> = idle
        .iter()
        .filter(|&(&token, _)| live(token))
        .map(|(&token, &cell)| (token, cell))
        .collect();
    for &(token, state) in &woken {
        idle.remove(&token);
        // SAFETY: a parked idle wait pins its run and so its cell.
        if let Cell::Clock(clock) = unsafe { cell(state as *mut u8) } {
            let mut clock = lock(clock);
            clock.timers.retain(|&(_, t)| live(t));
            if let Some(entry) = clock.idle.iter_mut().find(|(t, _)| *t == token) {
                entry.1 = true;
            }
        }
    }
    drop(idle);
    for &(token, _) in &woken {
        crate::tasks::hirsel_core_ready(token);
    }
    // No idle wait: a `next` with nothing queued answers None (NOTES 19).
    let served = if woken.is_empty() {
        let servers: Vec<(u64, usize)> = lock(&SERVERS)
            .iter()
            .filter(|&(&token, _)| live(token))
            .map(|(&token, &cell)| (token, cell))
            .collect();
        servers
            .into_iter()
            .filter(|&(token, state)| {
                // SAFETY: a parked `next` pins its run and so its port.
                let Cell::Port(port) = (unsafe { cell(state as *mut u8) }) else {
                    return false;
                };
                let mut port = lock(port);
                if port.queued().is_some() {
                    return false;
                }
                let entry = port.servers.iter_mut().find(|(t, _)| *t == token);
                entry.map(|e| e.1 = true).is_some()
            })
            .map(|(token, _)| token)
            .collect()
    } else {
        Vec::new()
    };
    let any = !woken.is_empty() || !served.is_empty();
    ready_all(served);
    any
}

/// Run teardown: forget the run's idle waits, free its cells and refund
/// their charges.
pub(crate) fn forget(cells: &mut Cells) {
    let cells = std::mem::take(cells.0.get_mut().unwrap_or_else(|e| e.into_inner()));
    let states: Vec<usize> = cells
        .iter()
        .map(|(c, _, _)| &raw const **c as usize)
        .collect();
    lock(&IDLE).retain(|_, cell| !states.contains(cell));
    lock(&SERVERS).retain(|_, cell| !states.contains(cell));
    for (_, payer, charge) in cells {
        payer.refund(charge);
    }
}

#[cfg(test)]
#[path = "provide_tests.rs"]
mod tests;
