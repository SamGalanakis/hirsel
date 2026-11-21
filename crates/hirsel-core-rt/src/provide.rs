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
use crate::{Context, provider::*};
use std::{
    collections::BTreeMap,
    ptr,
    sync::{Mutex, MutexGuard},
};

enum Cell {
    Clock(Mutex<ClockCell>),
    Random(Mutex<u64>),
    Process(Mutex<ProcessCell>),
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

/// The run's cells, each with the payer of its charge. Boxed so each
/// provider's state pointer stays put.
#[derive(Default)]
pub(crate) struct Cells(Mutex<Vec<(Box<Cell>, crate::limit::Payer)>>);
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
        _ => Wait::default(),
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
unsafe extern "C" fn issue(state: *mut u8, request: *mut Request, _: u64) -> Step {
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
        _ => {}
    }
    Step {
        outcome,
        ..Step::default()
    }
}

unsafe extern "C" fn resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
    Step::default()
}

unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    Outcome::default()
}

unsafe extern "C" fn deliver(_: *mut u8, _: *mut Request, outcome: Outcome) -> Outcome {
    outcome
}

impl Cells {
    /// Admit a cell's charge and pin it, or answer the refusal: value 0 and
    /// the available bytes in `written`, from which the caller raises Capacity.
    unsafe fn add(&self, context: *mut Context, cell: Cell) -> Result<Provider, Outcome> {
        // SAFETY: a running context has a current payer.
        let payer = unsafe { (*context).payer() };
        if let Err(capacity) = payer.admit(Some(hirsel_core_layout::charge::PROVIDE_CELL)) {
            return Err(Outcome {
                written: capacity.available,
                ..Outcome::default()
            });
        }
        let mut cells = lock(&self.0);
        cells.push((Box::new(cell), payer));
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
        )
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant(provider, 0),
        written: files.registry.cell(provider),
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
    let provider = match unsafe { files.cells.add(context, Cell::Random(Mutex::new(seed))) } {
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
        files
            .cells
            .add(context, Cell::Process(Mutex::new(ProcessCell::default())))
    } {
        Ok(provider) => provider,
        Err(refused) => return refused,
    };
    Outcome {
        value: files.registry.grant(provider, 0),
        written: files.registry.cell(provider),
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
    let provider = unsafe { files(context).registry.take_cell(trigger) };
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
    !woken.is_empty()
}

/// Run teardown: forget the run's idle waits, free its cells and refund
/// their charges.
pub(crate) fn forget(cells: &mut Cells) {
    let cells = std::mem::take(cells.0.get_mut().unwrap_or_else(|e| e.into_inner()));
    let states: Vec<usize> = cells.iter().map(|(c, _)| &raw const **c as usize).collect();
    lock(&IDLE).retain(|_, cell| !states.contains(cell));
    for (_, payer) in cells {
        payer.refund(hirsel_core_layout::charge::PROVIDE_CELL);
    }
}
