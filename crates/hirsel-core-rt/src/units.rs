//! The unit pool (CONC-15, CONC-16). `HIRSEL_UNTIED=W` runs a program's root
//! unit, and every helper unit `task.spawn_unit` makes, on W worker threads;
//! without it a helper runs on its own OS thread (units_lang.rs). A unit is one
//! order of events: its entry chain, scheduler, tasks, reactor and budget live
//! on its own stack and heap records. Units meet only through connections
//! (units_lang.rs over the ledger, units_ledger.rs). A socket can move as an
//! owned item over a connection.
//!
//! A unit recording (not built). A unit's events are a function of its inputs,
//! and the only inputs that come from another unit are arrivals, which it takes
//! at take positions (CONC-16). A recording of unit u would therefore hold u's
//! own host tape, as a root's run tape does, plus: a helper's entry value at its
//! content; and at each take position, in order, how many arrivals it took and
//! each one (the connection, the item's bytes with its owners' content, a
//! retirement notice and its refund, a helper's end with its result or causes).
//! Sends, and every effect on other units, are outputs and are not recorded.
//! A take that attaches a socket would record its identity, direction and
//! already observed protocol and readiness state. Later takes would record
//! the new registration's edges and the outcomes of its host operations.
//! Replaying u alone answers its take positions from the recording, so each unit
//! replays without the others. Today a recorded run (HIRSEL_RECORD) records its
//! root alone, and its helpers are not recorded.
#[path = "unit_memory.rs"]
mod memory;
pub(crate) use memory::Memory;
use memory::UnitStack;
#[cfg(test)]
#[path = "units_check.rs"]
mod check;
#[path = "units_sync.rs"]
pub(crate) mod sync;
#[path = "units_ledger.rs"]
pub(crate) mod ledger;
use std::{
    cell::Cell,
    collections::VecDeque,
    ptr,
    sync::{Arc, OnceLock, atomic::Ordering},
};
use sync::{AtomicU64, Mutex};

/// The running pool unit's id, or NONE outside the pool.
#[inline(never)]
pub(crate) fn current() -> u32 {
    let u = if untied() { cur() } else { std::ptr::null_mut() };
    // SAFETY: a running unit's record is never freed.
    if u.is_null() { NONE } else { unsafe { (*u).id } }
}

pub(crate) fn spin() -> bool {
    static SPIN: OnceLock<bool> = OnceLock::new();
    *SPIN.get_or_init(|| std::env::var_os("HIRSEL_XSPIN").is_some())
}

// ================================================================== untied
//
// The pool: `HIRSEL_UNTIED=W` runs the root unit, and every helper unit made
// later, on W worker threads. A unit is one order of events: its whole run (entry chain,
// scheduler, tasks, reactor, budget) lives on its own stack and heap records,
// and a worker runs it until it has nothing runnable or its slice ends. Then
// the unit parks (switches back to the worker's host stack) or is requeued.
// At most one worker is inside a unit (checked on every entry). An idle
// worker steals a whole unit from another worker's queue.
//
// Wakes: a connection send posts to the receiver's port and makes it
// runnable (no syscall unless a worker must be woken). A sync helper's ready is routed to its issuing unit only. Socket
// readiness arrives through one pool poller that holds every unit's sockets
// (edge-triggered, tagged with the owning unit): an idle worker blocks in it,
// or a busy or searching worker polls it when no worker is blocked there.
// Each event joins the owning unit's arrival queue in the order it reached the
// unit, and the unit takes the queue at its next switch. Timers of parked
// units sit in one shared heap.
//
// Idle workers: a searcher watches the queued count for a few microseconds,
// then parks on its own handle. A wake hands one parked worker the searching
// role, so a burst of newly runnable units wakes one worker at a time.
use std::cell::{RefCell as URefCell, UnsafeCell};
use std::collections::BTreeSet;
use sync::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize};

/// mc-units-untied: a thread-local Cell whose every access computes the slot
/// address inside a call that is never inlined, so no per-thread address
/// computed before a unit moves to another worker is used after it.
pub(crate) struct Moved<T: 'static>(pub(crate) &'static std::thread::LocalKey<Cell<T>>);
impl<T: Copy> Moved<T> {
    #[inline(never)]
    pub(crate) fn get(&'static self) -> T {
        self.0.get()
    }
}
impl<T> Moved<T> {
    #[inline(never)]
    pub(crate) fn set(&'static self, v: T) {
        self.0.set(v)
    }
    #[inline(never)]
    pub(crate) fn replace(&'static self, v: T) -> T {
        self.0.replace(v)
    }
}
impl<T: Default> Moved<T> {
    #[inline(never)]
    pub(crate) fn take(&'static self) -> T {
        self.0.take()
    }
}
macro_rules! moved_tls {
    ($vis:vis static $name:ident: $t:ty = $init:expr;) => {
        $vis static $name: $crate::units::Moved<$t> = {
            thread_local! { static INNER: std::cell::Cell<$t> = const { std::cell::Cell::new($init) }; }
            $crate::units::Moved(&INNER)
        };
    };
}
pub(crate) use moved_tls;

pub(crate) const NONE: u32 = u32::MAX;
/// Unit ids end below this; helpers take theirs from HELPER_BASE up.
const ID_END: u32 = 0xffff;
const WAKE_TAG: u64 = u64::MAX;

static UNTIED_ON: AtomicBool = AtomicBool::new(false);
#[inline]
pub(crate) fn untied() -> bool {
    UNTIED_ON.load(Ordering::Relaxed)
}
/// `HIRSEL_UNTIED=W`: run the root unit, and every helper unit it makes, on W
/// worker threads.
pub(crate) fn workers() -> Option<usize> {
    std::env::var("HIRSEL_UNTIED")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&w: &usize| w >= 1)
}
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Counts from outside the worker pool and from workers that have exited.
pub(crate) static USTATS: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
thread_local! {
    static WORKER_STATS: [Cell<u64>; 16] = [const { Cell::new(0) }; 16];
}
const S_RESUMES: usize = 0;
const S_PARKS: usize = 1;
const S_YIELDS: usize = 2;
const S_STEALS: usize = 3;
const S_OS_WAKES: usize = 4;
const S_GPOLLS: usize = 5;
const S_GPOLL_BLOCKS: usize = 6;
const S_TIMER_FIRES: usize = 8;
const S_CREATED: usize = 9;
const S_CREATE_NS: usize = 10;
const S_READY_TO_PARK_NS: usize = 11;
const S_GPOLL_EVENTS: usize = 12;
const S_NOTIFIED_REQUEUE: usize = 13;
const S_LIFO: usize = 14;
fn bump(i: usize) {
    add_stat(i, 1);
}

// A unit may resume on another worker. Resolve this worker's TLS in the
// callee so the caller cannot cache its address across a stack switch.
#[inline(never)]
fn add_stat(i: usize, n: u64) {
    if worker_ix() == usize::MAX {
        USTATS[i].fetch_add(n, Ordering::Relaxed);
    } else {
        WORKER_STATS.with(|stats| stats[i].set(stats[i].get().wrapping_add(n)));
    }
}

const PARKED: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;
const NOTIFIED: u8 = 3;
const DONE: u8 = 4;

#[derive(Clone, Copy)]
enum After {
    Run,
    Wait(Option<u64>),
    Yield,
    Done,
}

pub(crate) struct Unit {
    id: u32,
    sp: UnsafeCell<*mut u8>,
    /// The running worker's host stack slot.
    host: UnsafeCell<*mut *mut u8>,
    stack: UnsafeCell<Option<UnitStack>>,
    state: AtomicU8,
    inside: AtomicBool,
    after: Cell<After>,
    sync_cells: URefCell<Vec<*mut crate::os_sync::SyncCell>>,
    /// Readiness for this unit's keys, as (reactor identity, epoll flags):
    /// crossing events and the shared poller's socket edges. The unit's
    /// scheduler takes them at its next turn, in this order.
    xready: Mutex<Vec<(u64, u32)>>,
    /// Set with the lock held whenever `xready` gains an entry, so an empty
    /// take costs one load.
    xpending: AtomicBool,
    /// The running unit's batch buffer, swapped with `xready` at a take.
    xspare: UnsafeCell<Vec<(u64, u32)>>,
    status: AtomicU8,
    stop_fd: Cell<i32>,
    slice_start: Cell<Option<u64>>,
    created_ns: u64,
    first_park: Cell<bool>,
    /// The deadline of this unit's one live timer registration, or
    /// `u64::MAX`. Set when the unit parks with a deadline; cleared, and the
    /// set entry removed, when the park ends.
    timer_at: AtomicU64,
    /// The unit's scheduler-less waiter (replaces the thread's ALONE).
    pub(crate) alone: std::sync::OnceLock<Arc<crate::tasks::Readies>>,
    pub(crate) alone_depth: Cell<usize>,
    config: crate::entry::OsConfig,
    /// A helper unit's body (CONC-15), run in place of main.
    job: Cell<Option<Job>>,
    /// A helper unit's record is reused by a later helper once it ends.
    helper: bool,
}
/// A helper unit's body.
pub(crate) type Job = Box<dyn FnOnce() + Send>;
// SAFETY: the Cells are touched only by the one worker inside the unit;
// entry and exit are ordered by the state word and the queue locks.
unsafe impl Sync for Unit {}
unsafe impl Send for Unit {}

static TABLE: [AtomicPtr<Unit>; 65536] = [const { AtomicPtr::new(std::ptr::null_mut()) }; 65536];
fn unit_ref(id: u32) -> Option<&'static Unit> {
    let p = TABLE.get(id as usize)?.load(Ordering::Acquire);
    // SAFETY: unit records are leaked; a published pointer stays valid.
    unsafe { p.as_ref() }
}

thread_local! {
    static CUR: Cell<*mut Unit> = const { Cell::new(std::ptr::null_mut()) };
    static WORKER_IX: Cell<usize> = const { Cell::new(usize::MAX) };
    // Turns this worker has taken since it last looked at the injector first.
    static TURNS: Cell<u32> = const { Cell::new(0) };
}
/// The unit running on this worker. Never inlined: a unit resumes on any
/// worker, so no caller may keep this thread's slot address across a park.
#[inline(never)]
pub(crate) fn cur() -> *mut Unit {
    CUR.get()
}
#[inline(never)]
fn worker_ix() -> usize {
    WORKER_IX.get()
}
/// A per-unit identity for code that asks "is this my own domain" by thread.
#[inline(never)]
pub(crate) fn identity() -> usize {
    cur().addr()
}

#[derive(Clone, Copy)]
struct UP(*const Unit);
// SAFETY: unit records are leaked and shared by design.
unsafe impl Send for UP {}

struct Pool {
    w: usize,
    locals: Vec<Mutex<VecDeque<UP>>>,
    /// Per worker: the unit a unit running there last woke. Not stealable;
    /// run next by its worker, at most LIFO_RUNS times in a row (as Tokio's
    /// LIFO slot), so a request and its reply stay on one core.
    lifo: Vec<AtomicPtr<Unit>>,
    lifo_runs: Vec<AtomicU32>,
    lifo_on: bool,
    injector: Mutex<VecDeque<UP>>,
    memory: Memory,
    /// Units in the local queues and the injector. The LIFO slots are not
    /// counted: only their own worker takes them.
    queued: AtomicUsize,
    searching: AtomicUsize,
    /// Parked workers, most recent last, and their count for a lock-free check.
    idle: Mutex<Vec<usize>>,
    nsleep: AtomicUsize,
    /// Per worker: its thread, and whether a wake handed it the search role.
    threads: Vec<OnceLock<sync::thread::Thread>>,
    handed: Vec<AtomicBool>,
    poll: Mutex<()>,
    /// Set while a worker holds `poll`, for a probe or a blocking wait. A
    /// worker parks on its own handle only after seeing it set, and the
    /// holder checks for sleepers after clearing it (see `keep_poller`).
    poll_held: AtomicBool,
    poller_blocked: AtomicBool,
    poll_until: AtomicU64,
    gep: i32,
    gwake: i32,
    _shared: Arc<hirsel_core_net::Poller>,
    /// The shared poller has been handed to a socket registry.
    net_used: AtomicBool,
    /// Live park deadlines as (deadline, unit id): at most one per unit.
    timers: Mutex<BTreeSet<(u64, u32)>>,
    next_timer: AtomicU64,
    /// Units admitted and not yet ended. The initial units count from the
    /// start; any later unit is admitted with `admit`. Reaching zero is the
    /// decision to stop and is final: `admit` refuses from zero.
    live: AtomicUsize,
    stop: AtomicBool,
    last_poll: AtomicU64,
    spin_ns: u64,
    connection_spins: u64,
    slice_ns: u64,
    poll_every_ns: u64,
    spin_poll_ns: u64,
}
static POOL: OnceLock<Pool> = OnceLock::new();
fn pool() -> &'static Pool {
    POOL.get().unwrap()
}
pub(crate) fn memory() -> Option<&'static Memory> {
    if !untied() {
        return None;
    }
    let p = pool();
    (!p.stop.load(Ordering::Acquire)).then_some(&p.memory)
}
struct RunFn(&'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync));
static RUN: OnceLock<RunFn> = OnceLock::new();
struct Base(crate::entry::OsConfig);
// SAFETY: the launcher pins the configuration for the whole launch.
unsafe impl Send for Base {}
unsafe impl Sync for Base {}
static BASE: OnceLock<Base> = OnceLock::new();
static WORST: AtomicU8 = AtomicU8::new(0);

fn now() -> u64 {
    sync::monotonic_ns()
}

/// Count one more live unit, unless the pool has decided to stop. Admission
/// and the stop decision are the same count, so an admitted unit runs to its
/// end before any worker leaves, and a refused one never starts.
fn admit(p: &Pool) -> bool {
    p.live
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n > 0).then_some(n + 1))
        .is_ok()
}

/// Make unit `id`, admitted already, and queue it. The record is made here;
/// its stack is taken and its program starts on its first run.
fn create_unit(id: u32) -> &'static Unit {
    create_unit_with(id, None)
}

/// Helper units take ids from here up, recycled with their records.
const HELPER_BASE: u32 = 0x8000;
static HELPER_NEXT: AtomicU32 = AtomicU32::new(HELPER_BASE);
/// Ended helper units, whose records and ids the next helpers reuse.
static SPARE_HELPERS: std::sync::Mutex<Vec<UP>> = std::sync::Mutex::new(Vec::new());

/// Run `job` as a new unit on the pool, beside the calling unit (CONC-15).
/// Hands `job` back when no pool runs: the caller places the helper itself.
pub(crate) fn spawn_job(job: Job) -> Result<(), Job> {
    if !untied() || cur().is_null() {
        return Err(job);
    }
    let p = pool();
    // The running spawner is live, so the pool cannot have decided to stop.
    if !admit(p) {
        crate::process::stop(b"UnitAfterStop");
    }
    let spare = SPARE_HELPERS.lock().unwrap_or_else(|e| e.into_inner()).pop();
    if let Some(up) = spare {
        // SAFETY: an ended helper's leaked record; no worker is inside it and
        // its stack was returned. A late wake of its old run is a spurious
        // turn of the new one.
        let u = unsafe { &*up.0 };
        u.after.set(After::Run);
        u.status.store(0, Ordering::Relaxed);
        u.stop_fd.set(2);
        u.first_park.set(false);
        u.timer_at.store(u64::MAX, Ordering::Relaxed);
        u.alone_depth.set(0);
        u.job.set(Some(job));
        u.state.store(QUEUED, Ordering::SeqCst);
        bump(S_CREATED);
        enqueue(p, u);
        return Ok(());
    }
    let id = HELPER_NEXT.fetch_add(1, Ordering::Relaxed);
    if id >= ID_END {
        crate::process::stop(b"UnitIdSpace");
    }
    create_unit_with(id, Some(job));
    Ok(())
}

fn create_unit_with(id: u32, job: Option<Job>) -> &'static Unit {
    let t0 = now();
    let p = pool();
    let config = BASE.get().unwrap().0;
    let u: &'static Unit = Box::leak(Box::new(Unit {
        id,
        sp: UnsafeCell::new(std::ptr::null_mut()),
        host: UnsafeCell::new(std::ptr::null_mut()),
        stack: UnsafeCell::new(None),
        state: AtomicU8::new(QUEUED),
        inside: AtomicBool::new(false),
        after: Cell::new(After::Run),
        sync_cells: URefCell::new(Vec::new()),
        xready: Mutex::new(Vec::new()),
        xpending: AtomicBool::new(false),
        xspare: UnsafeCell::new(Vec::new()),
        status: AtomicU8::new(0),
        stop_fd: Cell::new(2),
        slice_start: Cell::new(None),
        created_ns: t0,
        first_park: Cell::new(false),
        timer_at: AtomicU64::new(u64::MAX),
        alone: std::sync::OnceLock::new(),
        alone_depth: Cell::new(0),
        config,
        helper: job.is_some(),
        job: Cell::new(job),
    }));
    if !TABLE[id as usize]
        .compare_exchange(std::ptr::null_mut(), (u as *const Unit).cast_mut(), Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        crate::process::stop(b"UnitIdTaken");
    }
    bump(S_CREATED);
    add_stat(S_CREATE_NS, now() - t0);
    enqueue(p, u);
    u
}

/// Give a unit about to run for the first time its stack and initial frame.
/// Done by the worker that runs it, so a cache miss's round trip to the
/// memory thread never holds the poll turn or the unit's creator.
/// # Safety
/// `u` is dequeued by this worker and has never run.
unsafe fn start_stack(p: &Pool, u: &Unit) {
    let stack = p.memory.take_unit_stack();
    // SAFETY: the frame is written at the top of the unit's own stack, which
    // this worker alone owns until the unit runs.
    unsafe {
        let top = stack.top();
        let words = hirsel_stack::initial_frame(unit_entry, (u as *const Unit).cast_mut().cast(), std::ptr::null_mut());
        let start = top.sub(16 + size_of_val(&words));
        std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), start, size_of_val(&words));
        *u.sp.get() = start;
        *u.stack.get() = Some(stack);
    }
}

unsafe extern "C" fn unit_entry(a: *mut u8, _b: *mut u8) -> ! {
    // SAFETY: the record outlives its stack; see create_unit.
    unsafe {
        let u = &*(a as *const Unit);
        let status = match u.job.take() {
            Some(job) => {
                job();
                0
            }
            None => {
                let run = RUN.get().unwrap().0;
                let status = run(&u.config);
                status
            }
        };
        u.status.store(status, Ordering::Relaxed);
        u.after.set(After::Done);
        hirsel_stack::switch(u.sp.get(), **u.host.get());
        std::process::abort()
    }
}

/// Switch from the running unit back to its worker. Returns when a worker
/// (any) resumes the unit.
fn leave(u: &Unit, after: After) {
    // SAFETY: called on the unit's own stack while it runs.
    unsafe {
        crate::tasks::check_idle_tls();
        u.after.set(after);
        hirsel_stack::switch(u.sp.get(), **u.host.get());
    }
}

/// The running unit has nothing runnable: park until a wake or `deadline`.
pub(crate) fn park(deadline: Option<u64>) {
    bump(S_PARKS);
    // SAFETY: called on the unit's own stack while it runs.
    let u = unsafe { &*cur() };
    if !u.first_park.replace(true) {
        add_stat(S_READY_TO_PARK_NS, now() - u.created_ns);
    }
    leave(u, After::Wait(deadline));
}

/// The running unit's record, if untied and inside a unit.
pub(crate) fn running() -> Option<&'static Unit> {
    if !untied() {
        return None;
    }
    let u = cur();
    // SAFETY: a live unit record (records are never freed).
    (!u.is_null()).then(|| unsafe { &*u })
}
pub(crate) fn unit_id(u: &Unit) -> u32 {
    u.id
}

/// Record an in-flight sync cell on the running pooled unit so unit teardown
/// detaches it if the unit ends before completion. Returns `true` if tracked.
pub(crate) fn track_sync(cell: *mut crate::os_sync::SyncCell) -> bool {
    if !untied() {
        return false;
    }
    let u = cur();
    if u.is_null() {
        return false;
    }
    // SAFETY: only the running unit's worker touches its `sync_cells`.
    unsafe { (*u).sync_cells.borrow_mut().push(cell) };
    true
}

/// Remove a completed sync cell from the running pooled unit's tracking list.
pub(crate) fn untrack_sync(cell: *mut crate::os_sync::SyncCell) {
    if !untied() {
        return;
    }
    let u = cur();
    if u.is_null() {
        return;
    }
    // SAFETY: only the running unit's worker touches its `sync_cells`.
    let mut cells = unsafe { (*u).sync_cells.borrow_mut() };
    if let Some(i) = cells.iter().position(|&c| c == cell) {
        cells.swap_remove(i);
    }
}

fn work_waiting(p: &Pool, ix: usize) -> bool {
    p.queued.load(Ordering::Relaxed) != 0 || !p.lifo[ix].load(Ordering::Relaxed).is_null()
}

pub(crate) fn connection_spinner() -> *mut Unit {
    if untied() && pool().connection_spins != 0 {
        cur()
    } else {
        ptr::null_mut()
    }
}

/// Spin only while a peer can make progress on another worker. Check work
/// on every iteration, including the private slot that queued excludes.
pub(crate) fn spin_connection(peer: &Unit, mut ready: impl FnMut() -> bool) -> bool {
    let p = pool();
    if p.connection_spins == 0
        || p.w == 1
        || !matches!(peer.state.load(Ordering::Relaxed), RUNNING | NOTIFIED)
        || ptr::eq(peer, cur())
    {
        return false;
    }
    let ix = worker_ix();
    for _ in 0..p.connection_spins {
        if work_waiting(p, ix) || !matches!(peer.state.load(Ordering::Relaxed), RUNNING | NOTIFIED)
        {
            break;
        }
        if ready() {
            return true;
        }
        std::hint::spin_loop();
    }
    false
}

/// Requeue the running unit when another unit waits and its slice is over.
#[inline]
pub(crate) fn maybe_yield() {
    let p = pool();
    if !work_waiting(p, worker_ix()) {
        return;
    }
    // SAFETY: called on the unit's own stack while it runs.
    let u = unsafe { &*cur() };
    // A turn that started without competition did not claim a timed slice.
    // If work has arrived since then, yield at this checkpoint.
    if u.slice_start
        .get()
        .is_some_and(|start| now().saturating_sub(start) < p.slice_ns)
    {
        return;
    }
    bump(S_YIELDS);
    leave(u, After::Yield);
}

/// Publish readiness to the next entry even when the unit already has a
/// pending entry. Only the PARKED transition publishes a new queue item.
pub(crate) fn wake(u: &Unit) {
    let prior = u.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
        match state {
            PARKED => Some(QUEUED),
            RUNNING => Some(NOTIFIED),
            QUEUED | NOTIFIED => Some(state),
            _ => None,
        }
    });
    if prior == Ok(PARKED) {
        enqueue(pool(), u);
    }
}
pub(crate) fn wake_id(id: u32) {
    if let Some(u) = unit_ref(id) {
        wake(u);
    }
}

fn push_arrival(u: &Unit, id: u64, flags: u32) {
    let mut q = u.xready.lock().unwrap();
    q.push((id, flags));
    u.xpending.store(true, Ordering::Release);
}
/// Readiness queued for the running unit, as (reactor identity, flags), in
/// the order it reached the unit. A push after the check wakes the unit, so
/// it is taken at the unit's next switch.
pub(crate) fn take_xready(mut each: impl FnMut(u64, u32)) {
    // SAFETY: called by the running unit's scheduler.
    let u = unsafe { &*cur() };
    if !u.xpending.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: only the running unit touches its spare buffer.
    let batch = unsafe { &mut *u.xspare.get() };
    {
        let mut q = u.xready.lock().unwrap();
        u.xpending.store(false, Ordering::Relaxed);
        std::mem::swap(&mut *q, batch);
    }
    for (id, flags) in batch.drain(..) {
        each(id, flags);
    }
}

/// The running unit's id, for the shared poller's tags.
fn shared_unit() -> Option<u32> {
    let u = cur();
    if u.is_null() {
        return None;
    }
    // This callback runs before the registry can add a descriptor.
    pool().net_used.store(true, Ordering::Relaxed);
    // SAFETY: a live unit record.
    Some(unsafe { (*u).id })
}

const LIFO_RUNS: u32 = 3;
const INJECTOR_EVERY: u32 = 61;
fn enqueue(p: &Pool, u: &Unit) {
    let ix = worker_ix();
    if ix < p.w && p.lifo_on && !cur().is_null() {
        // Woken by the unit running on this worker: it runs here next.
        // Only this worker accesses its private slot.
        let prev = p.lifo[ix].load(Ordering::Relaxed);
        p.lifo[ix].store(ptr::from_ref(u).cast_mut(), Ordering::Relaxed);
        if !prev.is_null() {
            publish(p, &p.locals[ix], UP(prev));
        }
        return;
    }
    publish(p, if ix < p.w { &p.locals[ix] } else { &p.injector }, UP(u));
}

/// The running unit is about to make a host call that may block its worker
/// (a write to a full pipe, a file operation). A unit in this worker's private
/// slot would wait that long, since only this worker takes it: queue it where
/// any worker can take it, and tell the pool.
pub(crate) fn before_blocking() {
    if !untied() {
        return;
    }
    let p = pool();
    let ix = worker_ix();
    if ix >= p.w || p.lifo[ix].load(Ordering::Relaxed).is_null() {
        return;
    }
    let l = p.lifo[ix].load(Ordering::Relaxed);
    if !l.is_null() {
        p.lifo[ix].store(ptr::null_mut(), Ordering::Relaxed);
        publish(p, &p.locals[ix], UP(l));
    }
}

/// Put `up` on queue `q`, count it and tell the pool. Every unit that becomes
/// counted work comes through here, so a sleeping worker or a poller blocked
/// with no deadline always hears of it.
fn publish(p: &Pool, q: &Mutex<VecDeque<UP>>, up: UP) {
    q.lock().unwrap().push_back(up);
    p.queued.fetch_add(1, Ordering::SeqCst);
    notify_worker(p);
}

/// Wake a sleeping worker unless the workers already searching cover the
/// queued units. A handed worker counts as searching before the OS runs it,
/// so a burst wakes several workers at once instead of one wake at a time.
fn notify_worker(p: &Pool) {
    let covered = |p: &Pool| {
        let s = p.searching.load(Ordering::SeqCst);
        s > 0 && p.queued.load(Ordering::SeqCst) <= s
    };
    if covered(p) {
        return;
    }
    if p.nsleep.load(Ordering::SeqCst) > 0 {
        let idle = p.idle.lock().unwrap();
        if !covered(p) && hand_search(p, idle) {
            return;
        }
    }
    if p.searching.load(Ordering::SeqCst) == 0 && p.poller_blocked.load(Ordering::SeqCst) {
        write_wake(p);
    }
}

/// Wake the most recently parked worker, if any, with the search role.
fn hand_search(p: &Pool, mut idle: sync::MutexGuard<'_, Vec<usize>>) -> bool {
    let Some(w) = idle.pop() else { return false };
    p.nsleep.fetch_sub(1, Ordering::SeqCst);
    // The woken worker searches on this count.
    p.searching.fetch_add(1, Ordering::SeqCst);
    p.handed[w].store(true, Ordering::Release);
    drop(idle);
    p.threads[w].get().unwrap().unpark();
    bump(S_OS_WAKES);
    true
}

/// Called by a worker about to run a unit. Parked workers sleep with no
/// timeout, so while any sleeps, some worker must hold the poll turn or be
/// searching (a searcher ends by polling or by coming here). If none does,
/// wake a sleeper: it searches, then blocks in the poller until the earliest
/// deadline.
fn keep_poller(p: &Pool) {
    let unwatched = |p: &Pool| !p.poll_held.load(Ordering::SeqCst) && p.searching.load(Ordering::SeqCst) == 0;
    if p.nsleep.load(Ordering::SeqCst) > 0 && unwatched(p) {
        let idle = p.idle.lock().unwrap();
        if unwatched(p) {
            hand_search(p, idle);
        }
    }
}

/// The poll turn: `poll` held, with `poll_held` set for the sleepers' check.
struct PollTurn<'a> {
    p: &'a Pool,
    _lock: sync::MutexGuard<'a, ()>,
}
impl<'a> PollTurn<'a> {
    fn take(p: &'a Pool) -> Option<Self> {
        let lock = p.poll.try_lock().ok()?;
        p.poll_held.store(true, Ordering::SeqCst);
        Some(PollTurn { p, _lock: lock })
    }
}
impl Drop for PollTurn<'_> {
    fn drop(&mut self) {
        // Cleared before the lock is released; a holder that then runs a
        // unit calls `keep_poller`, which reads the sleepers after this.
        self.p.poll_held.store(false, Ordering::SeqCst);
    }
}
fn write_wake(p: &Pool) {
    bump(S_OS_WAKES);
    let one = 1u64;
    // SAFETY: eight-byte write to a live eventfd.
    unsafe { sync::write(p.gwake, (&one as *const u64).cast(), 8) };
}

/// Register the parking unit's deadline. `next_timer` is the set's minimum,
/// stored under the set's lock.
fn add_timer(p: &Pool, at: u64, u: &Unit) {
    {
        let mut set = p.timers.lock().unwrap();
        u.timer_at.store(at, Ordering::SeqCst);
        set.insert((at, u.id));
        p.next_timer.store(set.first().map_or(u64::MAX, |e| e.0), Ordering::SeqCst);
    }
    // The poller publishes `poll_until` and then reloads `next_timer`, so
    // either it sees this deadline or this load sees its bound.
    if p.poller_blocked.load(Ordering::SeqCst) && at < p.poll_until.load(Ordering::SeqCst) {
        write_wake(p);
    }
}
/// End the unit's park: drop its timer registration if it still has one.
fn clear_timer(p: &Pool, u: &Unit) {
    // A running unit's timer can only be cleared by another worker.
    if u.timer_at.load(Ordering::Relaxed) == u64::MAX {
        return;
    }
    let at = u.timer_at.swap(u64::MAX, Ordering::SeqCst);
    if at == u64::MAX {
        return;
    }
    let mut set = p.timers.lock().unwrap();
    set.remove(&(at, u.id));
    p.next_timer.store(set.first().map_or(u64::MAX, |e| e.0), Ordering::SeqCst);
}
fn fire_timers(p: &Pool) {
    let next = p.next_timer.load(Ordering::SeqCst);
    if next == u64::MAX {
        return;
    }
    let t = now();
    if next > t {
        return;
    }
    let mut due = Vec::new();
    {
        let mut set = p.timers.lock().unwrap();
        while let Some(&(at, id)) = set.first() {
            if at > t {
                break;
            }
            set.pop_first();
            // The registration is live unless its park already ended.
            if let Some(u) = unit_ref(id)
                && u.timer_at.compare_exchange(at, u64::MAX, Ordering::SeqCst, Ordering::SeqCst).is_ok()
            {
                due.push(u);
            }
        }
        p.next_timer.store(set.first().map_or(u64::MAX, |e| e.0), Ordering::SeqCst);
    }
    for u in due {
        bump(S_TIMER_FIRES);
        wake(u);
    }
}

fn poll_global(p: &Pool, timeout: i32) {
    let mut evs: [libc::epoll_event; 128] = [libc::epoll_event { events: 0, u64: 0 }; 128];
    bump(S_GPOLLS);
    if timeout != 0 {
        bump(S_GPOLL_BLOCKS);
    }
    // SAFETY: a live epoll and a writable event array.
    let n = unsafe { sync::epoll_wait(p.gep, evs.as_mut_ptr(), evs.len() as i32, timeout) };
    p.last_poll.store(now(), Ordering::Relaxed);
    for e in evs.iter().take(n.max(0) as usize) {
        let tag = e.u64;
        if tag == WAKE_TAG {
            let mut v = 0u64;
            // SAFETY: nonblocking eventfd read.
            unsafe { sync::read(p.gwake, (&mut v as *mut u64).cast(), 8) };
        } else {
            bump(S_GPOLL_EVENTS);
            let unit = (tag >> hirsel_core_net::SHARED_UNIT_SHIFT) as u32;
            if let Some(u) = unit_ref(unit) {
                let ident = tag & ((1 << hirsel_core_net::SHARED_UNIT_SHIFT) - 1);
                push_arrival(u, ident, e.events);
                wake(u);
            }
        }
    }
    fire_timers(p);
}

/// The next unit for worker `ix`, taken off the queues and the count.
fn find(p: &Pool, ix: usize) -> Option<UP> {
    // A worker kept busy by its private slot or its own queue still takes
    // from the injector every INJECTOR_EVERY turns (Go's schedtick % 61), so
    // yielded and outside-woken units are not held back behind it for ever.
    let turns = TURNS.get() + 1;
    TURNS.set(turns);
    if turns >= INJECTOR_EVERY {
        TURNS.set(0);
        if let Some(u) = p.injector.lock().unwrap().pop_front() {
            p.queued.fetch_sub(1, Ordering::SeqCst);
            return Some(u);
        }
    }
    let l = p.lifo[ix].load(Ordering::Relaxed);
    if !l.is_null() {
        p.lifo[ix].store(ptr::null_mut(), Ordering::Relaxed);
        let runs = p.lifo_runs[ix].load(Ordering::Relaxed);
        if runs < LIFO_RUNS {
            p.lifo_runs[ix].store(runs + 1, Ordering::Relaxed);
            bump(S_LIFO);
            return Some(UP(l));
        }
        // Out of private runs. Take the slot only when this worker's
        // queue is empty, as it would come first there.
        if p.locals[ix].lock().unwrap().is_empty() {
            p.lifo_runs[ix].store(0, Ordering::Relaxed);
            return Some(UP(l));
        }
        publish(p, &p.locals[ix], UP(l));
    }
    p.lifo_runs[ix].store(0, Ordering::Relaxed);
    if p.queued.load(Ordering::SeqCst) == 0 {
        fire_timers(p);
        if p.queued.load(Ordering::SeqCst) == 0 {
            return None;
        }
    }
    let taken = |u: UP| {
        p.queued.fetch_sub(1, Ordering::SeqCst);
        Some(u)
    };
    if let Some(u) = p.locals[ix].lock().unwrap().pop_front() {
        return taken(u);
    }
    if let Some(u) = p.injector.lock().unwrap().pop_front() {
        return taken(u);
    }
    for j in 1..p.w {
        let v = (ix + j) % p.w;
        if let Ok(mut q) = p.locals[v].try_lock()
            && let Some(u) = q.pop_back()
        {
            drop(q);
            bump(S_STEALS);
            return taken(u);
        }
    }
    None
}

/// Socket registrations and armed timers can make units runnable at any
/// point in the turn. They keep the timed slice and existing poll cadence.
fn has_poll_sources(p: &Pool) -> bool {
    p.net_used.load(Ordering::Relaxed) || p.next_timer.load(Ordering::SeqCst) != u64::MAX
}

/// Poll the shared poller without blocking if no worker is blocked in it
/// and the pool has not polled for `every` nanoseconds.
fn poll_due(p: &Pool, t: u64, every: u64) {
    if !p.poller_blocked.load(Ordering::Relaxed)
        && t.saturating_sub(p.last_poll.load(Ordering::Relaxed)) > every
        && let Some(_turn) = PollTurn::take(p)
    {
        poll_global(p, 0);
    }
}

/// Run one queued unit until it parks, yields or ends.
unsafe fn run_unit(p: &Pool, ix: usize, up: UP, host: &mut *mut u8) {
    // SAFETY: the dequeued record is live; this worker alone dequeued it.
    unsafe {
        let u = &*up.0;
        if (*u.sp.get()).is_null() {
            start_stack(p, u);
        }
        // Acquire readiness published while this entry was already queued.
        u.state.swap(RUNNING, Ordering::SeqCst);
        // Law 1: never two workers inside one unit.
        if u.inside.swap(true, Ordering::SeqCst) {
            eprintln!("untied: law 1 violated: unit {} entered twice", u.id);
            std::process::abort();
        }
        bump(S_RESUMES);
        // Any park this entry ends is over: its deadline no longer applies.
        clear_timer(p, u);
        CUR.set(up.0.cast_mut());
        let prev_fd = crate::fiber::set_stop_fd(u.stop_fd.get());
        *u.host.get() = host as *mut *mut u8;
        let timed = has_poll_sources(p) || work_waiting(p, ix);
        u.slice_start.set(timed.then(now));
        hirsel_stack::switch(host as *mut *mut u8, *u.sp.get());
        u.stop_fd.set(crate::fiber::set_stop_fd(prev_fd));
        CUR.set(std::ptr::null_mut());
        let after = u.after.replace(After::Run);
        u.inside.store(false, Ordering::SeqCst);
        match after {
            After::Done => {
                WORST.fetch_max(u.status.load(Ordering::Relaxed), Ordering::Relaxed);
                u.state.store(DONE, Ordering::SeqCst);
                for cell in u.sync_cells.borrow_mut().drain(..) {
                    // SAFETY: unit has ended; detach any sync still in flight so
                    // the helper frees the cell on completion and delivers nothing.
                    unsafe { crate::os_sync::SyncCell::detach(cell) };
                }
                // The terminal switch returned to the host stack. DONE
                // rejects further wakes, and nothing can resume this stack.
                p.memory.put_unit_stack((*u.stack.get()).take().unwrap());
                *u.sp.get() = std::ptr::null_mut();
                if u.helper {
                    SPARE_HELPERS.lock().unwrap_or_else(|e| e.into_inner()).push(up);
                }
                if p.live.fetch_sub(1, Ordering::SeqCst) == 1 {
                    p.stop.store(true, Ordering::SeqCst);
                    let parked = std::mem::take(&mut *p.idle.lock().unwrap());
                    for w in parked {
                        p.nsleep.fetch_sub(1, Ordering::SeqCst);
                        p.threads[w].get().unwrap().unpark();
                    }
                    write_wake(p);
                }
            }
            After::Yield => {
                u.state.swap(QUEUED, Ordering::SeqCst);
                publish(p, &p.injector, up);
            }
            After::Wait(deadline) => {
                if let Some(at) = deadline {
                    add_timer(p, at, u);
                }
                if u
                    .state
                    .compare_exchange(RUNNING, PARKED, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    // Woken while it ran: run it again.
                    bump(S_NOTIFIED_REQUEUE);
                    u.state.swap(QUEUED, Ordering::SeqCst);
                    publish(p, &p.locals[ix], up);
                }
            }
            After::Run => crate::process::stop(b"UnitLeftWithoutReason"),
        }
        // Busy workers poll now and then when no idle worker is blocked in
        // the poller.
        if !p.poller_blocked.load(Ordering::Relaxed) && has_poll_sources(p) {
            poll_due(p, now(), p.poll_every_ns);
        }
    }
}

fn worker_main(p: &'static Pool, ix: usize) {
    crate::fiber::arm();
    WORKER_IX.set(ix);
    let mut host: *mut u8 = std::ptr::null_mut();
    let _ = p.threads[ix].set(sync::thread::current());
    // A wake handed this worker the search role (already counted).
    let mut handed = false;
    loop {
        if !handed && let Some(u) = find(p, ix) {
            keep_poller(p);
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if p.stop.load(Ordering::SeqCst) && p.queued.load(Ordering::SeqCst) == 0 {
            if handed {
                p.searching.fetch_sub(1, Ordering::SeqCst);
            }
            break;
        }
        // Search: at most half the workers at once. A searcher watches the
        // queued count without taking queue locks, and polls only when no
        // worker is blocked in the poller.
        let mut got = None;
        if std::mem::take(&mut handed) || 2 * p.searching.fetch_add(1, Ordering::SeqCst) < p.w {
            let t0 = now();
            let mut i = 0u32;
            loop {
                if p.queued.load(Ordering::SeqCst) > 0
                    && let Some(u) = find(p, ix)
                {
                    got = Some(u);
                    break;
                }
                i = i.wrapping_add(1);
                if i % 64 == 0 {
                    let t = now();
                    if t - t0 > p.spin_ns || p.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    poll_due(p, t, p.spin_poll_ns);
                }
                std::hint::spin_loop();
            }
        }
        p.searching.fetch_sub(1, Ordering::SeqCst);
        if let Some(u) = got {
            // The last searcher to find work wakes another if work remains,
            // or if workers sleep and nobody watches the poller.
            if p.queued.load(Ordering::SeqCst) > 0 {
                notify_worker(p);
            } else {
                keep_poller(p);
            }
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if p.stop.load(Ordering::SeqCst) && p.queued.load(Ordering::SeqCst) == 0 {
            break;
        }
        match PollTurn::take(p) {
            Some(_turn) => {
                p.poller_blocked.store(true, Ordering::SeqCst);
                if p.queued.load(Ordering::SeqCst) == 0 && !p.stop.load(Ordering::SeqCst) {
                    // Publish the bound, then reload: a deadline registered
                    // before the store is seen here, and one after it sees
                    // this bound and writes the wake.
                    let nt = loop {
                        let nt = p.next_timer.load(Ordering::SeqCst);
                        p.poll_until.store(nt, Ordering::SeqCst);
                        if p.next_timer.load(Ordering::SeqCst) >= nt {
                            break nt;
                        }
                    };
                    let timeout = if nt == u64::MAX {
                        -1
                    } else {
                        nt.saturating_sub(now()).div_ceil(1_000_000).min(i32::MAX as u64) as i32
                    };
                    poll_global(p, timeout);
                }
                p.poller_blocked.store(false, Ordering::SeqCst);
            }
            None => {
                let mut idle = p.idle.lock().unwrap();
                p.nsleep.fetch_add(1, Ordering::SeqCst);
                // Sleep only while another worker holds the poll turn: it
                // reads the sleepers after it lets the turn go.
                if p.queued.load(Ordering::SeqCst) != 0
                    || p.stop.load(Ordering::SeqCst)
                    || !p.poll_held.load(Ordering::SeqCst)
                {
                    p.nsleep.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                idle.push(ix);
                drop(idle);
                loop {
                    sync::thread::park();
                    if p.handed[ix].swap(false, Ordering::AcqRel) {
                        handed = true;
                        break;
                    }
                    if p.stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    }
    // The launch joins the workers before reading the totals.
    WORKER_STATS.with(|stats| {
        for (total, local) in USTATS.iter().zip(stats) {
            total.fetch_add(local.get(), Ordering::Relaxed);
        }
    });
}

/// Run `run` as `k` root units (a program has one; the reference bench two) on
/// `w` workers; the worst status wins.
pub(crate) fn launch(
    k: usize,
    w: usize,
    input: &crate::entry::OsConfig,
    run: &(dyn Fn(&crate::entry::OsConfig) -> u8 + Sync),
) -> u8 {
    // One poller for every unit's descriptors; its wake eventfd is WAKE_TAG.
    let shared = hirsel_core_net::share_poller(shared_unit).unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
    let (gep, gwake) = (shared.raw_fd(), shared.wake_fd());
    let _ = POOL.set(Pool {
        w,
        locals: (0..w).map(|_| Mutex::new(VecDeque::new())).collect(),
        lifo: (0..w).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(),
        lifo_runs: (0..w).map(|_| AtomicU32::new(0)).collect(),
        lifo_on: env_u64("HIRSEL_UNTIED_LIFO", 1) != 0,
        injector: Mutex::new(VecDeque::new()),
        memory: Memory::new(),
        queued: AtomicUsize::new(0),
        searching: AtomicUsize::new(0),
        idle: Mutex::new(Vec::with_capacity(w)),
        nsleep: AtomicUsize::new(0),
        threads: (0..w).map(|_| OnceLock::new()).collect(),
        handed: (0..w).map(|_| AtomicBool::new(false)).collect(),
        poll: Mutex::new(()),
        poll_held: AtomicBool::new(false),
        poller_blocked: AtomicBool::new(false),
        poll_until: AtomicU64::new(u64::MAX),
        gep,
        gwake,
        _shared: shared,
        net_used: AtomicBool::new(false),
        timers: Mutex::new(BTreeSet::new()),
        next_timer: AtomicU64::new(u64::MAX),
        live: AtomicUsize::new(k),
        stop: AtomicBool::new(false),
        last_poll: AtomicU64::new(0),
        connection_spins: env_u64("HIRSEL_UNTIED_CONN_SPINS", 16).min(128),
        spin_ns: env_u64("HIRSEL_UNTIED_SPIN_US", 5) * 1000,
        spin_poll_ns: env_u64("HIRSEL_UNTIED_SPIN_POLL_US", 20) * 1000,
        slice_ns: env_u64("HIRSEL_UNTIED_SLICE_US", 500) * 1000,
        poll_every_ns: env_u64("HIRSEL_UNTIED_POLL_US", 100) * 1000,
    });
    // SAFETY: the launch joins every worker before `run` goes out of scope.
    let run: &'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync) = unsafe { std::mem::transmute(run) };
    let _ = RUN.set(RunFn(run));
    let _ = BASE.set(Base(*input));
    UNTIED_ON.store(true, Ordering::SeqCst);
    let p = pool();
    let memory_thread = p.memory.start();
    for i in 0..k {
        create_unit(i as u32);
    }
    std::thread::scope(|scope| {
        for ix in 0..w {
            std::thread::Builder::new()
                .name(format!("hirsel-w{ix}"))
                .stack_size(1 << 20)
                .spawn_scoped(scope, move || worker_main(p, ix))
                .unwrap();
        }
    });
    drop(memory_thread);
    // Every recording block is in its file before the run reports.
    #[cfg(feature = "record")]
    crate::tape::writer::flush();
    if std::env::var_os("HIRSEL_XSTATS").is_some() {
        p.memory.report();
        let g = |i: usize| USTATS[i].load(Ordering::Relaxed);
        #[cfg(feature = "record")]
        let tape_stats = format!(
            " tape_writes={} tape_bytes={} tape_payload={} tape_skipped={} tape_worker_ns={} tape_truncated={} tape_writer_ns={} tape_writer_calls={} tape_write_failed={}",
            crate::tape::TAPE_COUNTS[0].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[1].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[2].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[3].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[4].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[5].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[6].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[7].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[8].load(Ordering::Relaxed),
        );
        #[cfg(not(feature = "record"))]
        let tape_stats = "";
        eprintln!(
            "untied-stats workers={} units={} resumes={} parks={} yields={} steals={} os_wakes={} gpolls={} gpoll_blocks={} gpoll_events={} timer_fires={} notified_requeue={} create_ns_avg={} ready_to_park_ns_avg={} token_acquires={} unit_epoll_waits={} lifo={}{tape_stats}",
            w,
            g(S_CREATED),
            g(S_RESUMES),
            g(S_PARKS),
            g(S_YIELDS),
            g(S_STEALS),
            g(S_OS_WAKES),
            g(S_GPOLLS),
            g(S_GPOLL_BLOCKS),
            g(S_GPOLL_EVENTS),
            g(S_TIMER_FIRES),
            g(S_NOTIFIED_REQUEUE),
            g(S_CREATE_NS) / g(S_CREATED).max(1),
            g(S_READY_TO_PARK_NS) / g(S_CREATED).max(1),
            crate::sequencer::ACQUIRES.load(Ordering::Relaxed),
            hirsel_core_net::EPOLL_WAITS.load(Ordering::Relaxed),
            g(S_LIFO),
        );
    }
    if crate::sequencer::ACQUIRES.load(Ordering::Relaxed) != 0 {
        eprintln!("untied: law 4 violated: the cross-worker ordering token was taken");
        return 101;
    }
    WORST.load(Ordering::Relaxed)
}
