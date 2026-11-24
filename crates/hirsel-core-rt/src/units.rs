//! The unit pool (CONC-15). For a program that can reach `task.spawn_unit`,
//! `HIRSEL_UNTIED=W` runs its root and helper units on W worker threads;
//! without it a helper runs on its own OS thread (units_lang.rs). A unit is one
//! order of events: its entry chain, scheduler, tasks, reactor and budget live
//! on its own stack and heap records. Units meet only through connections
//! (units_lang.rs over the ledger, units_ledger.rs). A socket can move as an
//! owned item over a connection.
//!
//! A unit recording (not built). A unit's events are a function of its inputs,
//! and the only inputs that come from another unit are arrivals, which it takes
//! at take positions (not yet specified). A recording of unit u would therefore hold u's
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
#[cfg(feature = "testing")]
#[path = "units_observe.rs"]
pub(crate) mod observe;
use memory::UnitStack;
pub(crate) use memory::{Memory, SharedMemory};
#[cfg(test)]
#[path = "units_check.rs"]
mod check;
#[path = "units_ledger.rs"]
pub(crate) mod ledger;
#[path = "units_reply.rs"]
mod reply;
pub(crate) use reply::{Reply, file_request, uses_file_ring};
#[path = "units_report.rs"]
mod report;
#[path = "units_sync.rs"]
pub(crate) mod sync;
#[path = "units_sync_cells.rs"]
mod sync_cells;
use std::{
    cell::Cell,
    collections::VecDeque,
    ptr,
    sync::{Arc, OnceLock, atomic::Ordering},
};
use sync::{AtomicU64, Mutex};
pub(crate) use sync_cells::{track_sync, untrack_sync};

/// The running pool unit's id, or NONE outside the pool.
#[inline(never)]
pub(crate) fn current() -> u32 {
    let u = if untied() {
        cur()
    } else {
        std::ptr::null_mut()
    };
    // SAFETY: a running unit's record is never freed.
    if u.is_null() {
        NONE
    } else {
        unsafe { (*u).id }
    }
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
/// computed before a unit moves to another worker is used after it. Each
/// static has its own key type, so each gets its own accessors with a direct
/// thread-local read (no shared accessor called through `LocalKey`).
pub(crate) trait MovedKey: 'static {
    type T: 'static;
    fn key() -> &'static std::thread::LocalKey<Cell<Self::T>>;
}
pub(crate) struct Moved<K>(pub(crate) std::marker::PhantomData<K>);
impl<K: MovedKey<T: Copy>> Moved<K> {
    #[inline(never)]
    pub(crate) fn get(&'static self) -> K::T {
        K::key().get()
    }
    /// A read on code that cannot cross a stack switch before using it.
    #[inline(always)]
    pub(crate) fn get_here(&'static self) -> K::T {
        K::key().get()
    }
}
impl<K: MovedKey> Moved<K> {
    #[inline(never)]
    pub(crate) fn set(&'static self, v: K::T) {
        K::key().set(v)
    }
    #[inline(never)]
    pub(crate) fn replace(&'static self, v: K::T) -> K::T {
        K::key().replace(v)
    }
    /// `set` and `replace` on code that cannot cross a stack switch
    /// between computing the slot and writing it.
    #[inline(always)]
    pub(crate) fn set_here(&'static self, v: K::T) {
        K::key().set(v)
    }
    #[inline(always)]
    pub(crate) fn replace_here(&'static self, v: K::T) -> K::T {
        K::key().replace(v)
    }
}
impl<K: MovedKey<T: Default>> Moved<K> {
    #[inline(never)]
    pub(crate) fn take(&'static self) -> K::T {
        K::key().take()
    }
}
macro_rules! moved_tls {
    (pub(crate) static $name:ident: $t:ty = $init:expr;) => {
        $crate::units::moved_tls!(@ (pub(crate)) (pub(crate)) $name: $t = $init);
    };
    (static $name:ident: $t:ty = $init:expr;) => {
        $crate::units::moved_tls!(@ () (pub(super)) $name: $t = $init);
    };
    (@ ($($vis:tt)*) ($($key:tt)*) $name:ident: $t:ty = $init:expr) => {
        #[allow(non_snake_case)]
        $($vis)* mod $name {
            #[allow(unused_imports)]
            use super::*;
            thread_local! { static INNER: std::cell::Cell<$t> = const { std::cell::Cell::new($init) }; }
            $($key)* struct Key;
            impl $crate::units::MovedKey for Key {
                type T = $t;
                #[inline(always)]
                fn key() -> &'static std::thread::LocalKey<std::cell::Cell<$t>> {
                    &INNER
                }
            }
        }
        $($vis)* static $name: $crate::units::Moved<$name::Key> = $crate::units::Moved(std::marker::PhantomData);
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

struct PoolHooks {
    memory: fn() -> Option<&'static dyn SharedMemory>,
    park: fn(Option<u64>),
    maybe_yield: fn(),
    wake: fn(u32),
    off_slot: fn(&mut (dyn FnMut() + Send)) -> bool,
}

static POOL_HOOKS: OnceLock<PoolHooks> = OnceLock::new();

fn enable_pool() {
    let _ = POOL_HOOKS.set(PoolHooks {
        memory: pool_memory,
        park: park_unit,
        maybe_yield: yield_unit_if_needed,
        wake: wake_unit_by_id,
        off_slot: run_off_slot,
    });
    UNTIED_ON.store(true, Ordering::SeqCst);
}

// A launcher installs these callbacks before any worker starts. A tied
// scheduler does not name the pool's records or queues through its wait path.
fn pool_hooks() -> &'static PoolHooks {
    POOL_HOOKS.get().unwrap()
}
/// `HIRSEL_UNTIED=W`: run the root unit, and every helper unit it makes, on W
/// worker threads. Only executables that can spawn a helper unit read it.
pub(crate) fn workers() -> Option<usize> {
    std::env::var("HIRSEL_UNTIED")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&w: &usize| w >= 1)
}
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Counts from outside the worker pool and from workers that have exited.
pub(crate) static USTATS: [AtomicU64; STATS] = [const { AtomicU64::new(0) }; STATS];
thread_local! {
    static WORKER_STATS: [Cell<u64>; STATS] = [const { Cell::new(0) }; STATS];
}
const S_RESUMES: usize = 0;
const S_PARKS: usize = 1;
const S_YIELDS: usize = 2;
const S_STEALS: usize = 3;
const S_OS_WAKES: usize = 4;
const S_GPOLLS: usize = 5;
const S_GPOLL_BLOCKS: usize = 6;
const S_TIMER_FIRES: usize = 7;
const S_CREATED: usize = 8;
const S_CREATE_NS: usize = 9;
const S_READY_TO_PARK_NS: usize = 10;
const S_GPOLL_EVENTS: usize = 11;
const S_NOTIFIED_REQUEUE: usize = 12;
const S_LIFO: usize = 13;
const STATS: usize = 14;
fn bump(i: usize) {
    add_stat(i, 1);
}
/// `bump` on a worker's own host stack (`find`, `run_unit`). That stack
/// always resumes on its own thread, so the counter is read in place.
#[inline(always)]
fn bump_host(i: usize) {
    WORKER_STATS.with(|stats| stats[i].set(stats[i].get().wrapping_add(1)));
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
    #[cfg(feature = "testing")]
    observed_queue_ns: AtomicU64,
    #[cfg(feature = "testing")]
    observed_queue_cpu: AtomicU32,
    sp: UnsafeCell<*mut u8>,
    /// The running worker's host stack slot.
    host: UnsafeCell<*mut *mut u8>,
    stack: UnsafeCell<Option<UnitStack>>,
    /// Only the running unit reads or changes this heap owner.
    heap: UnsafeCell<std::cell::OnceCell<Arc<crate::task_heap::Heap>>>,
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
    /// The slot whose timer set holds that registration.
    timer_slot: AtomicUsize,
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

pub(crate) struct Pool {
    pub(crate) w: usize,
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
    /// Per worker: whether a wake handed it the search role.
    handed: Vec<AtomicBool>,
    /// Per worker slot: its poller, which only the slot's thread waits in.
    slots: Vec<Slot>,
    /// The slot pollers are io_uring rings (else epoll).
    uring: bool,
    /// A socket registry has registered in the slot pollers.
    net_used: AtomicBool,
    stop: AtomicBool,
    spin_ns: u64,
    connection_spins: u64,
    slice_ns: u64,
    poll_every_ns: u64,
}

/// One worker slot's poller and timers. The slot's thread owns both: it
/// alone waits in the poller and fires the timers, whether parked (blocked
/// in the poller until the earliest timer) or running units (checking the
/// poller and the timers at each unit switch). Other threads post to it a
/// wake, followed by a wake of the poller. A slot's thread never blocks in a
/// host call (`off_slot`), so it is the only thread that ever owns the slot.
///
/// Each slot owns its poller because an io_uring poll's readiness runs as
/// task work on the thread that armed it, and under `COOP_TASKRUN` that is
/// the only thread that sees it without an interrupt: a pool-wide ring
/// polled by whichever worker was free would leave readiness waiting on a
/// busy or parked arming thread. A worker parked in its own poller is woken
/// by its readiness, its timers and the pool's wakes alike.
struct Slot {
    poller: Arc<hirsel_core_net::Poller>,
    /// epoll: the epoll fd and its edge-triggered wake eventfd.
    epfd: i32,
    wakefd: i32,
    /// Live park deadlines on this slot as (deadline, unit id): at most one
    /// per unit.
    timers: Mutex<BTreeSet<(u64, u32)>>,
    /// The set's minimum, stored under its lock.
    next_timer: AtomicU64,
    /// epoll: when the slot's thread last polled without blocking.
    last_poll: AtomicU64,
}
impl Slot {
    fn new(poller: Arc<hirsel_core_net::Poller>) -> Self {
        let epoll = poller.reactor() == hirsel_core_net::Reactor::Epoll;
        Slot {
            epfd: poller.raw_fd(),
            wakefd: if epoll { poller.wake_fd() } else { -1 },
            poller,
            timers: Mutex::new(BTreeSet::new()),
            next_timer: AtomicU64::new(u64::MAX),
            last_poll: AtomicU64::new(0),
        }
    }
}
static POOL: OnceLock<Pool> = OnceLock::new();
/// Units admitted and not yet ended. The initial units count from the
/// start; any later unit is admitted with `admit`. Reaching zero is the
/// decision to stop and is final: `admit` refuses from zero.
static LIVE: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn pool() -> &'static Pool {
    POOL.get().unwrap()
}
pub(crate) fn memory() -> Option<&'static dyn SharedMemory> {
    untied().then(|| (pool_hooks().memory)()).flatten()
}

fn pool_memory() -> Option<&'static dyn SharedMemory> {
    let p = pool();
    (!p.stop.load(Ordering::Acquire)).then_some(&p.memory as &dyn SharedMemory)
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
fn admit() -> bool {
    LIVE.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
        (n > 0).then_some(n + 1)
    })
    .is_ok()
}

/// Make unit `id`, admitted already, and queue it. The record is made here;
/// its stack is taken and its program starts on its first run.
fn create_unit(id: u32) {
    publish_unit(new_unit(id, false));
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
    if !admit() {
        crate::process::stop(b"UnitAfterStop");
    }
    let spare = SPARE_HELPERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop();
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
    let unit = new_unit(id, true);
    unit.job.set(Some(job));
    publish_unit(unit);
    Ok(())
}

// A helper's job is set by its spawner before publication. Keeping the job
// out of the shared constructor also keeps its vtable out of the root path.
fn new_unit(id: u32, helper: bool) -> Box<Unit> {
    let t0 = now();
    let config = BASE.get().unwrap().0;
    Box::new(Unit {
        id,
        #[cfg(feature = "testing")]
        observed_queue_ns: AtomicU64::new(0),
        #[cfg(feature = "testing")]
        observed_queue_cpu: AtomicU32::new(u32::MAX),
        sp: UnsafeCell::new(std::ptr::null_mut()),
        host: UnsafeCell::new(std::ptr::null_mut()),
        stack: UnsafeCell::new(None),
        heap: UnsafeCell::new(std::cell::OnceCell::new()),
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
        timer_slot: AtomicUsize::new(0),
        alone: std::sync::OnceLock::new(),
        alone_depth: Cell::new(0),
        config,
        helper,
        job: Cell::new(None),
    })
}

fn publish_unit(unit: Box<Unit>) {
    let unit: &'static Unit = Box::leak(unit);
    if !TABLE[unit.id as usize]
        .compare_exchange(
            std::ptr::null_mut(),
            (unit as *const Unit).cast_mut(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        crate::process::stop(b"UnitIdTaken");
    }
    bump(S_CREATED);
    add_stat(S_CREATE_NS, now() - unit.created_ns);
    enqueue(pool(), unit);
}

/// Give a unit about to run for the first time its stack and initial frame.
/// Done by the worker that runs it, so a cache miss's round trip to the
/// memory thread never holds the poll turn or the unit's creator.
/// # Safety
/// `u` is dequeued by this worker and has never run.
unsafe fn start_stack(p: &'static Pool, u: &Unit) -> bool {
    // The checker does not model the memory thread: a start maps in place,
    // as a cache hit would.
    #[cfg(test)]
    if check::modelled() {
        // SAFETY: by the contract.
        unsafe { first_frame(u, UnitStack::new()) };
        return true;
    }
    if let Some(stack) = p.memory.cached_unit_stack() {
        // SAFETY: by the contract.
        unsafe { first_frame(u, stack) };
        return true;
    }
    // A cache miss maps on the memory thread. The unit stays queued but on
    // no queue; the job gives it the stack and publishes it again, while
    // this worker goes on to other work.
    let up = UP(u);
    p.memory.post(move || {
        let up = up;
        // SAFETY: nobody else holds the dequeued unit until it is published.
        unsafe { first_frame(&*up.0, UnitStack::new()) };
        publish(p, &p.injector, up);
    });
    false
}

/// # Safety
/// `u` has no stack and nobody else is touching it.
unsafe fn first_frame(u: &Unit, stack: UnitStack) {
    // SAFETY: the frame is written at the top of the unit's own stack, which
    // this thread alone owns until the unit runs.
    unsafe {
        let top = stack.top();
        let words = hirsel_stack::initial_frame(
            unit_entry,
            (u as *const Unit).cast_mut().cast(),
            std::ptr::null_mut(),
        );
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
        // The program and all its local tasks have returned. A page with
        // outstanding slots still owns its Arc, even across record reuse.
        if let Some(heap) = (*u.heap.get()).take() {
            heap.close();
        }
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
    (pool_hooks().park)(deadline);
}

fn park_unit(deadline: Option<u64>) {
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
/// Attach a root context to its running unit's heap.
pub(crate) fn heap_for_context() -> Option<Arc<crate::task_heap::Heap>> {
    let unit = running()?;
    // SAFETY: the caller is inside the unit, whose dispatcher is exclusive.
    let heap = unsafe { &*unit.heap.get() };
    Some(heap.get_or_init(Arc::default).clone())
}

pub(crate) fn unit_id(u: &Unit) -> u32 {
    u.id
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
    if ix >= p.w {
        return false;
    }
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
pub(crate) fn maybe_yield() {
    (pool_hooks().maybe_yield)();
}

fn yield_unit_if_needed() {
    let p = pool();
    let ix = worker_ix();
    let Some(private) = p.lifo.get(ix) else {
        requeue_detached();
        return;
    };
    if p.queued.load(Ordering::Relaxed) == 0 && private.load(Ordering::Relaxed).is_null() {
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

#[cold]
fn requeue_detached() {
    if let Some(unit) = running() {
        leave(unit, After::Yield);
    }
}

/// Publish readiness to the next entry. Only a parked unit needs a queue item.
fn mark_ready(u: &Unit) -> bool {
    u.state
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            PARKED => Some(QUEUED),
            RUNNING => Some(NOTIFIED),
            QUEUED | NOTIFIED => Some(state),
            _ => None,
        })
        == Ok(PARKED)
}

pub(crate) fn wake(u: &Unit) {
    if mark_ready(u) {
        enqueue(pool(), u);
    }
}
pub(crate) fn wake_id(id: u32) {
    (pool_hooks().wake)(id);
}

fn wake_unit_by_id(id: u32) {
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
/// it is taken at the unit's next switch. Nobody else takes the readiness of
/// the slot this unit runs on, so each scheduler turn takes it here first
/// rather than going dry, parking and resuming for readiness already there.
pub(crate) fn take_xready(mut each: impl FnMut(u64, u32)) {
    let (p, ix) = (pool(), worker_ix());
    if ix < p.w {
        check_slot(p, ix);
    }
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

/// The running unit and the slot its registrations go to, for the slot
/// pollers' tags: this thread's slot.
fn here() -> Option<hirsel_core_net::Here> {
    let u = cur();
    if u.is_null() {
        return None;
    }
    // This callback runs before the registry can add a descriptor.
    pool().net_used.store(true, Ordering::Relaxed);
    Some(hirsel_core_net::Here {
        // SAFETY: a live unit record.
        unit: unsafe { (*u).id },
        slot: WORKER_IX.get(),
    })
}

const LIFO_RUNS: u32 = 3;
const INJECTOR_EVERY: u32 = 61;
fn enqueue(p: &Pool, u: &Unit) {
    #[cfg(feature = "testing")]
    observe::queued(u);
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

// ================================================================ off slot
//
// A slot's thread never blocks in a host call. A call that can wait without
// bound (a file or standard-stream operation, a spawn, a wait on the memory
// thread) runs on a host helper thread, or on the thread that owns the work,
// while its unit parks. The slot's thread runs other units meanwhile, and the
// unit resumes on whichever slot takes it after the call completes.

/// Run `call` off the slot: on a host helper thread while the running unit
/// parks, or in place when no pool unit is running on this thread (a tied
/// run, a launcher, a helper thread). The unit's state is frozen while it
/// is parked, so `call` may use everything the unit owns.
pub(crate) fn off_slot<T: Send>(call: impl FnOnce() -> T + Send) -> T {
    let mut call = Some(call);
    let mut answer = None;
    let mut run = || answer = Some((call.take().unwrap())());
    if !(untied() && (pool_hooks().off_slot)(&mut run)) {
        run();
    }
    answer.unwrap()
}

/// True when this thread is a slot thread running a pool unit, which can
/// park on a `Reply`.
pub(crate) fn in_unit() -> bool {
    !cur().is_null() && worker_ix() < pool().w
}

struct HostJob {
    call: *mut (dyn FnMut() + Send),
    reply: *const Reply,
}
// SAFETY: the parked unit pins both until the reply completes.
unsafe impl Send for HostJob {}

struct Helpers {
    jobs: VecDeque<HostJob>,
    idle: usize,
}
static HELPERS: std::sync::Mutex<Helpers> = std::sync::Mutex::new(Helpers {
    jobs: VecDeque::new(),
    idle: 0,
});
static HELPER_WORK: std::sync::Condvar = std::sync::Condvar::new();
/// Idle helpers kept for the next calls; a helper past this many exits.
const KEPT_HELPERS: usize = 16;

fn helpers() -> std::sync::MutexGuard<'static, Helpers> {
    HELPERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Queue `call` for a host helper and park the running unit until it
/// returns. False when this thread runs no pool unit, so the caller runs
/// the call in place.
fn run_off_slot(call: &mut (dyn FnMut() + Send)) -> bool {
    if !in_unit() {
        return false;
    }
    let reply = Reply::new();
    // SAFETY: only the lifetime is erased. The unit stays parked until the
    // helper completes the reply, and the helper never touches `call` after.
    let call: *mut (dyn FnMut() + Send + 'static) =
        unsafe { std::mem::transmute(call as *mut (dyn FnMut() + Send + '_)) };
    let spawn = {
        let mut h = helpers();
        h.jobs.push_back(HostJob {
            call,
            reply: &reply,
        });
        // Every queued call has its own helper, so one call that waits for
        // ever (a FIFO nobody writes) never delays another.
        h.idle < h.jobs.len()
    };
    if spawn {
        if std::thread::Builder::new()
            .name("hirsel-host".into())
            .spawn(run_helper)
            .is_err()
        {
            crate::process::stop(b"HostThreads");
        }
    } else {
        HELPER_WORK.notify_one();
    }
    reply.wait();
    true
}

fn run_helper() {
    let mut h = helpers();
    loop {
        let Some(job) = h.jobs.pop_front() else {
            if h.idle >= KEPT_HELPERS {
                return;
            }
            h.idle += 1;
            h = HELPER_WORK.wait(h).unwrap_or_else(|e| e.into_inner());
            h.idle -= 1;
            continue;
        };
        drop(h);
        // SAFETY: the job's unit is parked on its reply until this
        // completes it; nothing here touches the job after.
        unsafe {
            (*job.call)();
            Reply::complete(job.reply);
        }
        h = helpers();
    }
}

/// Put `up` on queue `q`, count it and tell the pool.
fn publish(p: &Pool, q: &Mutex<VecDeque<UP>>, up: UP) {
    push_counted(p, q, up);
    notify_worker(p);
}

fn push_counted(p: &Pool, q: &Mutex<VecDeque<UP>>, up: UP) {
    q.lock().unwrap().push_back(up);
    p.queued.fetch_add(1, Ordering::SeqCst);
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
        if !covered(p) {
            hand_search(p, idle);
        }
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
    wake_slot(p, w);
    true
}

/// Wake slot `ix`'s thread out of its poller, or make its next wait return
/// at once: a MSG_RING completion on io_uring, an eventfd edge on epoll.
fn wake_slot(p: &Pool, ix: usize) {
    bump(S_OS_WAKES);
    if p.uring {
        p.slots[ix].poller.wake();
    } else {
        let one = 1u64;
        // SAFETY: eight-byte write to a live eventfd.
        unsafe { sync::write(p.slots[ix].wakefd, (&one as *const u64).cast(), 8) };
    }
}

/// Register the parking unit's deadline on slot `ix`, the slot it parked
/// from. `next_timer` is the set's minimum, stored under the set's lock.
/// The slot's own thread reads it before it next waits; a thread that lost
/// the slot wakes its new owner to read it.
fn add_timer(p: &Pool, ix: usize, at: u64, u: &Unit) {
    let slot = &p.slots[ix];
    {
        let mut set = slot.timers.lock().unwrap();
        u.timer_slot.store(ix, Ordering::Relaxed);
        u.timer_at.store(at, Ordering::SeqCst);
        set.insert((at, u.id));
        slot.next_timer
            .store(set.first().map_or(u64::MAX, |e| e.0), Ordering::SeqCst);
    }
    if WORKER_IX.get() != ix {
        wake_slot(p, ix);
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
    let slot = &p.slots[u.timer_slot.load(Ordering::Relaxed)];
    let mut set = slot.timers.lock().unwrap();
    set.remove(&(at, u.id));
    slot.next_timer
        .store(set.first().map_or(u64::MAX, |e| e.0), Ordering::SeqCst);
}
/// Fire slot `ix`'s due timers; only its own thread does.
fn fire_timers(p: &Pool, ix: usize) {
    let slot = &p.slots[ix];
    let next = slot.next_timer.load(Ordering::SeqCst);
    if next == u64::MAX {
        return;
    }
    let t = now();
    if next > t {
        return;
    }
    let mut due = Vec::new();
    {
        let mut set = slot.timers.lock().unwrap();
        while let Some(&(at, id)) = set.first() {
            if at > t {
                break;
            }
            set.pop_first();
            // The registration is live unless its park already ended.
            if let Some(u) = unit_ref(id)
                && u.timer_at
                    .compare_exchange(at, u64::MAX, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                due.push(u);
            }
        }
        slot.next_timer
            .store(set.first().map_or(u64::MAX, |e| e.0), Ordering::SeqCst);
    }
    for u in due {
        bump(S_TIMER_FIRES);
        wake(u);
    }
}

/// One turn of slot `ix`'s poller by its thread, blocking up to
/// `timeout_ns` (None: until an event; Some(0): not at all). Each socket
/// edge goes to its unit, which is queued here in event order; then the
/// slot's due timers fire.
fn poll_slot(p: &Pool, ix: usize, timeout_ns: Option<u64>) {
    let slot = &p.slots[ix];
    let mut evs: [libc::epoll_event; 128] = [libc::epoll_event { events: 0, u64: 0 }; 128];
    bump(S_GPOLLS);
    if timeout_ns != Some(0) {
        bump(S_GPOLL_BLOCKS);
    }
    let n = if p.uring {
        slot.poller.wait_slot(&mut evs, timeout_ns)
    } else {
        let timeout =
            timeout_ns.map_or(-1, |ns| ns.div_ceil(1_000_000).min(i32::MAX as u64) as i32);
        // SAFETY: a live epoll and a writable event array.
        let n = unsafe { sync::epoll_wait(slot.epfd, evs.as_mut_ptr(), evs.len() as i32, timeout) };
        slot.last_poll.store(now(), Ordering::Relaxed);
        n.max(0) as usize
    };
    let mut published = false;
    // A wake carries nothing; epoll's eventfd is edge-triggered and never read.
    for e in evs.iter().take(n).filter(|e| e.u64 != WAKE_TAG) {
        bump(S_GPOLL_EVENTS);
        let tag = e.u64;
        let unit = (tag >> hirsel_core_net::SHARED_UNIT_SHIFT) as u32;
        if let Some(u) = unit_ref(unit) {
            let ident = tag & ((1 << hirsel_core_net::SHARED_UNIT_SHIFT) - 1);
            push_arrival(u, ident, e.events);
            if mark_ready(u) {
                #[cfg(feature = "testing")]
                observe::queued(u);
                // Put arrivals on this slot's queue in event order.
                push_counted(p, &p.locals[ix], UP(u));
                published = true;
            }
        }
    }
    if published {
        // One worker starts the search for the batch. The search path
        // passes the wake on if queued work remains.
        notify_worker(p);
    }
    fire_timers(p, ix);
}

/// The owning thread's check of its slot between units: take whatever its
/// poller holds without blocking (an io_uring ring says so without a
/// syscall; epoll is asked every `poll_every_ns` once sockets exist), then
/// fire the slot's due timers.
fn check_slot(p: &Pool, ix: usize) {
    let due = if p.uring {
        p.slots[ix].poller.due()
    } else {
        p.net_used.load(Ordering::Relaxed)
            && now().saturating_sub(p.slots[ix].last_poll.load(Ordering::Relaxed)) > p.poll_every_ns
    };
    if due {
        poll_slot(p, ix, Some(0));
    } else {
        fire_timers(p, ix);
    }
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
            #[cfg(feature = "testing")]
            observe::found(observe::Queue::Injector);
            return Some(u);
        }
    }
    let l = p.lifo[ix].load(Ordering::Relaxed);
    if !l.is_null() {
        p.lifo[ix].store(ptr::null_mut(), Ordering::Relaxed);
        let runs = p.lifo_runs[ix].load(Ordering::Relaxed);
        if runs < LIFO_RUNS {
            p.lifo_runs[ix].store(runs + 1, Ordering::Relaxed);
            bump_host(S_LIFO);
            #[cfg(feature = "testing")]
            observe::found(observe::Queue::Private);
            return Some(UP(l));
        }
        // Out of private runs. Take the slot only when this worker's
        // queue is empty, as it would come first there.
        if p.locals[ix].lock().unwrap().is_empty() {
            p.lifo_runs[ix].store(0, Ordering::Relaxed);
            #[cfg(feature = "testing")]
            observe::found(observe::Queue::Private);
            return Some(UP(l));
        }
        publish(p, &p.locals[ix], UP(l));
    }
    p.lifo_runs[ix].store(0, Ordering::Relaxed);
    if p.queued.load(Ordering::SeqCst) == 0 {
        fire_timers(p, ix);
        if p.queued.load(Ordering::SeqCst) == 0 {
            return None;
        }
    }
    let taken = |u: UP| {
        p.queued.fetch_sub(1, Ordering::SeqCst);
        Some(u)
    };
    if let Some(u) = p.locals[ix].lock().unwrap().pop_front() {
        #[cfg(feature = "testing")]
        observe::found(observe::Queue::Local);
        return taken(u);
    }
    if let Some(u) = p.injector.lock().unwrap().pop_front() {
        #[cfg(feature = "testing")]
        observe::found(observe::Queue::Injector);
        return taken(u);
    }
    for j in 1..p.w {
        let v = (ix + j) % p.w;
        if let Ok(mut q) = p.locals[v].try_lock() {
            if let Some(u) = q.pop_back() {
                drop(q);
                bump_host(S_STEALS);
                #[cfg(feature = "testing")]
                observe::found(observe::Queue::Stolen);
                return taken(u);
            }
        }
    }
    None
}

/// Socket registrations and armed timers can make units runnable at any
/// point in the turn. They keep the timed slice.
fn has_poll_sources(p: &Pool, ix: usize) -> bool {
    p.net_used.load(Ordering::Relaxed) || p.slots[ix].next_timer.load(Ordering::SeqCst) != u64::MAX
}

/// Run one queued unit until it parks, yields or ends.
unsafe fn run_unit(p: &'static Pool, ix: usize, up: UP, host: &mut *mut u8) {
    // SAFETY: the dequeued record is live; this worker alone dequeued it.
    unsafe {
        let u = &*up.0;
        #[cfg(feature = "testing")]
        let entry_ns = if observe::enabled() { now() } else { 0 };
        if (*u.sp.get()).is_null() && !start_stack(p, u) {
            // The memory thread maps the stack and queues the unit.
            return;
        }
        // Acquire readiness published while this entry was already queued.
        let _prior = u.state.swap(RUNNING, Ordering::SeqCst);
        #[cfg(test)]
        assert_eq!(_prior, QUEUED, "unit {} dequeued twice", u.id);
        // Law 1: never two workers inside one unit.
        if u.inside.swap(true, Ordering::Acquire) {
            eprintln!("untied: law 1 violated: unit {} entered twice", u.id);
            std::process::abort();
        }
        bump_host(S_RESUMES);
        #[cfg(feature = "testing")]
        if entry_ns != 0 {
            observe::enter(u, ix, entry_ns);
        }
        // Any park this entry ends is over: its deadline no longer applies.
        clear_timer(p, u);
        CUR.set(up.0.cast_mut());
        // The worker's host stack does not move threads.
        let prev_fd = crate::fiber::set_stop_fd_here(u.stop_fd.get());
        *u.host.get() = host as *mut *mut u8;
        let timed = has_poll_sources(p, ix) || work_waiting(p, ix);
        u.slice_start.set(timed.then(now));
        hirsel_stack::switch(host as *mut *mut u8, *u.sp.get());
        u.stop_fd.set(crate::fiber::set_stop_fd_here(prev_fd));
        CUR.set(std::ptr::null_mut());
        let after = u.after.replace(After::Run);
        u.inside.store(false, Ordering::Release);
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
                    SPARE_HELPERS
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(up);
                }
                if LIVE.fetch_sub(1, Ordering::SeqCst) == 1 {
                    p.stop.store(true, Ordering::SeqCst);
                    let parked = std::mem::take(&mut *p.idle.lock().unwrap());
                    for w in parked {
                        p.nsleep.fetch_sub(1, Ordering::SeqCst);
                        wake_slot(p, w);
                    }
                }
            }
            After::Yield => {
                #[cfg(feature = "testing")]
                observe::queued(u);
                u.state.swap(QUEUED, Ordering::SeqCst);
                publish(p, &p.injector, up);
            }
            After::Wait(deadline) => {
                if let Some(at) = deadline {
                    add_timer(p, ix, at, u);
                }
                if u.state
                    .compare_exchange(RUNNING, PARKED, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    // Woken while it ran: run it again.
                    bump_host(S_NOTIFIED_REQUEUE);
                    #[cfg(feature = "testing")]
                    observe::queued(u);
                    u.state.swap(QUEUED, Ordering::SeqCst);
                    publish(p, &p.locals[ix], up);
                }
            }
            After::Run => crate::process::stop(b"UnitLeftWithoutReason"),
        }
        // Between units the slot's own thread takes its readiness and fires
        // its timers: nobody else waits in its poller.
        check_slot(p, ix);
    }
}

/// Run slot `ix` on this thread until the pool stops.
fn run_slot(p: &'static Pool, ix: usize) {
    WORKER_IX.set(ix);
    let mut host: *mut u8 = std::ptr::null_mut();
    // A wake handed this worker the search role (already counted).
    let mut handed = false;
    loop {
        if !handed && let Some(u) = find(p, ix) {
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
        // queued count without taking queue locks, and checks its own slot.
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
                    if now() - t0 > p.spin_ns || p.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    check_slot(p, ix);
                }
                std::hint::spin_loop();
            }
        }
        p.searching.fetch_sub(1, Ordering::SeqCst);
        if let Some(u) = got {
            // The last searcher to find work wakes another if work remains.
            if p.queued.load(Ordering::SeqCst) > 0 {
                notify_worker(p);
            }
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if p.stop.load(Ordering::SeqCst) && p.queued.load(Ordering::SeqCst) == 0 {
            break;
        }
        // Park in the slot's own poller until a wake, readiness on its
        // sockets, or its earliest timer. A wake sent after the push below
        // stays pending in the poller, so the wait returns at once.
        {
            let mut idle = p.idle.lock().unwrap();
            p.nsleep.fetch_add(1, Ordering::SeqCst);
            if p.queued.load(Ordering::SeqCst) != 0 || p.stop.load(Ordering::SeqCst) {
                p.nsleep.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            idle.push(ix);
        }
        let slot = &p.slots[ix];
        let nt = slot.next_timer.load(Ordering::SeqCst);
        let timeout = (nt != u64::MAX).then(|| nt.saturating_sub(now()));
        poll_slot(p, ix, timeout);
        // Leave the idle list; a hand that already took this worker off it
        // gave it the search role.
        let mut idle = p.idle.lock().unwrap();
        if let Some(at) = idle.iter().position(|&w| w == ix) {
            idle.swap_remove(at);
            p.nsleep.fetch_sub(1, Ordering::SeqCst);
        }
        drop(idle);
        handed = p.handed[ix].swap(false, Ordering::AcqRel);
    }
}

fn worker_main(p: &'static Pool, ix: usize) {
    #[cfg(feature = "prof")]
    let _worker_profile = crate::prof::Worker::enter();
    crate::fiber::arm();
    run_slot(p, ix);
    merge_stats();
    #[cfg(feature = "testing")]
    observe::flush();
}

fn merge_stats() {
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
    // One poller per worker slot for the descriptors its units register.
    let pollers = hirsel_core_net::slot_pollers(w, here)
        .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
    let _ = POOL.set(Pool {
        w,
        locals: (0..w).map(|_| Mutex::new(VecDeque::new())).collect(),
        lifo: (0..w)
            .map(|_| AtomicPtr::new(std::ptr::null_mut()))
            .collect(),
        lifo_runs: (0..w).map(|_| AtomicU32::new(0)).collect(),
        lifo_on: env_u64("HIRSEL_UNTIED_LIFO", 1) != 0,
        injector: Mutex::new(VecDeque::new()),
        memory: Memory::new(),
        queued: AtomicUsize::new(0),
        searching: AtomicUsize::new(0),
        idle: Mutex::new(Vec::with_capacity(w)),
        nsleep: AtomicUsize::new(0),
        handed: (0..w).map(|_| AtomicBool::new(false)).collect(),
        uring: pollers[0].reactor() == hirsel_core_net::Reactor::IoUring,
        slots: pollers.into_iter().map(Slot::new).collect(),
        net_used: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        connection_spins: env_u64("HIRSEL_UNTIED_CONN_SPINS", 16).min(128),
        spin_ns: env_u64("HIRSEL_UNTIED_SPIN_US", 5) * 1000,
        slice_ns: env_u64("HIRSEL_UNTIED_SLICE_US", 500) * 1000,
        poll_every_ns: env_u64("HIRSEL_UNTIED_POLL_US", 100) * 1000,
    });
    // SAFETY: the launch joins every worker before `run` goes out of scope.
    let run: &'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync) =
        unsafe { std::mem::transmute(run) };
    let _ = RUN.set(RunFn(run));
    let _ = BASE.set(Base(*input));
    LIVE.store(k, Ordering::SeqCst);
    enable_pool();
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
        report::print(p);
    }
    WORST.load(Ordering::Relaxed)
}
