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

/// A served port (NOTES 15): its calls by id, in issue order, each Queued
/// (not taken), Taken (the server holds its `Call`) or Settled.
#[derive(Default)]
struct Port {
    /// Cleaning the `Served` end seals the port: every unsettled and later
    /// call completes as `sealed` answers.
    sealed: bool,
    /// The outcome a sealed port gives: `Closed` for Dir and the streams.
    closed: Outcome,
    next_id: u64,
    calls: BTreeMap<u64, PortCall>,
    /// `next` waits: the token and whether quiescence answered it `None`.
    servers: Vec<(u64, bool)>,
}

struct PortCall {
    /// The caller's operation.
    token: u64,
    /// The caller's request, pinned until the caller collects the outcome
    /// or its abort settles the call; read only while the call is unsettled.
    request: *mut Request,
    /// The call's `Op` as scalars, copied at issue: tag, then fields.
    words: [u64; 4],
    taken: bool,
    /// The server holds this call's `Call`.
    held: bool,
    /// The reply, `Call` drop or seal that settled it, not yet collected.
    answer: Option<Outcome>,
    /// The caller collected the answer, or its abort settled the call.
    finished: bool,
}

impl Port {
    /// Settle an unsettled call; returns its caller's token to ready.
    fn settle(&mut self, id: u64, outcome: Outcome) -> Option<u64> {
        let call = self.calls.get_mut(&id)?;
        if call.answer.is_some() || call.finished {
            return None;
        }
        call.answer = Some(outcome);
        Some(call.token)
    }
    /// Forget a call once neither its caller nor a `Call` refers to it.
    fn prune(&mut self, id: u64) {
        if self.calls.get(&id).is_some_and(|c| c.finished && !c.held) {
            self.calls.remove(&id);
        }
    }
    fn by_token(&self, token: u64) -> Option<u64> {
        self.calls
            .iter()
            .find(|(_, c)| c.token == token && !c.finished)
            .map(|(&id, _)| id)
    }
    fn queued(&self) -> Option<u64> {
        self.calls
            .iter()
            .find(|(_, c)| !c.taken && !c.finished && c.answer.is_none())
            .map(|(&id, _)| id)
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
        let Some(id) = queued else {
            return done(Outcome::default());
        };
        let call = port.calls.get_mut(&id).expect("queued call");
        call.taken = true;
        call.held = true;
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
        None if abort => Outcome {
            status: 1,
            ..Outcome::default()
        },
        None => return None,
    };
    call.finished = true;
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
        (Cell::Port(port), STDOUT | STDERR) => {
            let mut p = lock(port);
            if p.sealed {
                outcome = p.closed;
            } else {
                // Scalars are copied now; the bytes stay the caller's until
                // `payload` copies them out of an unsettled call (NOTES 15).
                let id = p.next_id;
                p.next_id += 1;
                p.calls.insert(
                    id,
                    PortCall {
                        token,
                        request: &raw mut *request,
                        words: [0, request.buffer.len as u64, 0, 0],
                        taken: false,
                        held: false,
                        answer: None,
                        finished: false,
                    },
                );
                let servers = p.servers.iter().map(|&(t, _)| t).collect();
                drop(p);
                ready_all(servers);
                return pending();
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

/// `provide.stdout(bytes n)` and `provide.stderr(bytes n)`: a port with room
/// `n`, admitted with the port's own record and held until the run ends
/// (NOTES 18). `value` is the capability's grant and `written` the `Served`
/// key; a refusal answers as `Cells::add`.
/// # Safety
/// Context is running.
unsafe fn provide_stream(context: *mut Context, n: u64) -> Outcome {
    // SAFETY: forwarded.
    let files = unsafe { files(context) };
    let port = Port {
        closed: Outcome {
            error: IoError { tag: 4, code: 0 },
            ..Outcome::default()
        },
        ..Port::default()
    };
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
pub unsafe extern "C" fn hirsel_core_provide_stdout(
    context: *mut Context,
    n: u64,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { provide_stream(context, n) }
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
    unsafe { provide_stream(context, n) }
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
        value: lock(port).calls[&id].words[i.min(3) as usize],
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
    _field: u64,
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
        let from = (*c.request).buffer;
        let out = &mut *out;
        if out.capacity.saturating_sub(out.len) < from.len {
            causes::record(&mut *context, causes::Cause::new(causes::BOUNDS, site));
            return CANCELLED;
        }
        ptr::copy_nonoverlapping(from.data, out.data.add(out.len), from.len);
        out.len += from.len;
        Outcome {
            written: from.len as u64,
            ..Outcome::default()
        }
    }
}

/// `provide.reply`'s commit: settle the call with the reply (`tag` 0 Done; 1
/// WriteFailed with `a` written, `b` the error's provider tag and `c` its
/// code) and ready its caller; `value` 1 is Withdrawn: the call was settled
/// first, or its caller has a cancellation pending that it observes (NOTES
/// 16). A `Call` of another port is consumed as a dropped one and withdrawn
/// (NOTES 20).
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
    a: u64,
    b: u64,
    c: u64,
    _site: u64,
) -> Outcome {
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
    let live = p
        .calls
        .get(&id)
        .is_some_and(|call| !call.finished && call.answer.is_none());
    let outcome = if foreign || !live || crate::tasks::call_cancelled(p.calls[&id].token) {
        let closed = p.closed;
        let token = if foreign { p.settle(id, closed) } else { None };
        p.prune(id);
        drop(p);
        ready_all(token.into_iter().collect());
        return withdrawn;
    } else {
        match tag {
            0 => Outcome {
                written: p.calls[&id].words[1],
                ..Outcome::default()
            },
            _ => Outcome {
                error: IoError {
                    tag: b as u8,
                    code: c as u32,
                },
                written: a,
                ..Outcome::default()
            },
        }
    };
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
