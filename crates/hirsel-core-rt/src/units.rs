//! The unit pool (CONC-15). For a program that can reach `task.spawn_unit`,
//! `HIRSEL_UNTIED=W` runs its root and helper units on W worker threads;
//! without it a helper runs on its own OS thread (units_lang.rs). A unit is one
//! order of events: its entry chain, scheduler, tasks, reactor and budget live
//! on its own stack and heap records. Units meet only through connections
//! (units_lang.rs over the ledger, units_ledger.rs). A socket can move as an
//! owned item over a connection.
//!
//! A unit recording: docs/recording.md, "Unit recordings". Each unit is its
//! own stream in the tape: its entry, its host frames, a TAKE frame for every
//! arrival it takes (at `take`, `take_single_item` and `release_take`), and
//! its unit operations. Sends and spawns are checked at issue, and whether one
//! is accepted is an input, so it is supplied from the stream. Each unit
//! replays from its own stream alone. Today (U2) each helper of a recorded run
//! has its own stream: its entry, host frames and END (tape/helper.rs). Takes,
//! unit operations and helper replay are not built.
#[path = "unit_memory.rs"]
mod memory;
#[cfg(feature = "testing")]
#[path = "units_observe.rs"]
pub(crate) mod observe;
use memory::{AdmittedStacks, StackPair};
pub(crate) use memory::{Memory, SharedMemory};
#[cfg(test)]
#[path = "units_check.rs"]
mod check;
#[path = "units_directory.rs"]
mod directory;
#[cfg(test)]
use directory::create_unit;
use directory::{UnitDirectory, create_initial_unit};
pub(crate) use directory::{spawn_job, spawn_language_job};
#[path = "units_env.rs"]
mod env;
pub(crate) use env::workers;
#[path = "units_hooks.rs"]
mod hooks;
#[path = "units_host.rs"]
mod host_calls;
use host_calls::Helpers;
pub(crate) use host_calls::{HostCall, off_slot};
#[path = "units_launch.rs"]
mod launch;
#[path = "units_ledger.rs"]
pub(crate) mod ledger;
pub(crate) use launch::launch;
#[path = "units_plan.rs"]
mod plan;
use hooks::{UNTIED_ON, enable_pool, pool_hooks};
pub(crate) use hooks::{return_root_stack, take_root_stack, untied};
use plan::UntiedPlan;
#[path = "units_reply.rs"]
mod reply;
pub(crate) use reply::{Reply, file_request, uses_file_ring};
#[path = "units_report.rs"]
mod report;
#[path = "units_timers.rs"]
mod timers;
use timers::{add_timer, clear_timer, fire_timers};
#[path = "units_sync.rs"]
pub(crate) mod sync;
use hirsel_core_net::ready::Ready;
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
use std::cell::UnsafeCell;
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
const WAKE_TAG: u64 = u64::MAX;
const CONNECTION_SPINS: u64 = 16;
const SLICE_NS: u64 = 500_000;
const POLL_EVERY_NS: u64 = 100_000;

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
    pub(super) stack: UnsafeCell<AdmittedStacks>,
    /// Only the running unit reads or changes this heap owner.
    heap: UnsafeCell<std::cell::OnceCell<Arc<crate::task_heap::Heap>>>,
    state: AtomicU8,
    inside: AtomicBool,
    after: Cell<After>,
    /// Readiness for this unit's keys, as (reactor identity, conditions):
    /// crossing events and the shared poller's socket edges. The unit's
    /// scheduler takes them at its next turn, in this order.
    xready: Mutex<Vec<(u64, Ready)>>,
    /// Set with the lock held whenever `xready` gains an entry, so an empty
    /// take costs one load.
    xpending: AtomicBool,
    /// The running unit's batch buffer, swapped with `xready` at a take.
    xspare: UnsafeCell<Vec<(u64, Ready)>>,
    status: AtomicU8,
    stop_fd: Cell<i32>,
    slice_start: Cell<Option<u64>>,
    created_ns: u64,
    first_park: Cell<bool>,
    /// The deadline of this unit's one live timer registration, or
    /// `u64::MAX`. Set when the unit parks with a deadline; cleared, and the
    /// set entry removed, when the park ends.
    timer_at: AtomicU64,
    /// The slot whose timer list holds that registration.
    timer_slot: AtomicU32,
    /// The next unit in the slot's list, under that slot's timer lock.
    timer_next: AtomicU32,
    /// The unit's scheduler-less waiter (replaces the thread's ALONE).
    pub(crate) alone: std::sync::OnceLock<Arc<crate::tasks::Readies>>,
    pub(crate) alone_depth: Cell<usize>,
    config: crate::entry::OsConfig,
    /// A helper unit's body (CONC-15), run in place of main.
    job: Cell<Option<Job>>,
}
/// A helper unit's body.
pub(crate) type Job = Box<dyn FnOnce() + Send>;
// SAFETY: the Cells are touched only by the one worker inside the unit;
// entry and exit are ordered by the state word and the queue locks.
unsafe impl Sync for Unit {}
unsafe impl Send for Unit {}

fn unit_ref(id: u32) -> Option<&'static Unit> {
    POOL.get()?.units.get(id)
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
pub(super) struct UP(*const Unit);
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
    plan: UntiedPlan,
    units: UnitDirectory,
    helpers: Helpers,
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
    /// The poller's readiness set, waited on directly when it is not an
    /// io_uring ring, and that set's wake.
    handle: hirsel_core_net::ready::Handle,
    waker: Option<hirsel_core_net::ready::Waker>,
    /// Live park deadlines on this slot as (deadline, unit id): at most one
    /// per unit.
    timers: Mutex<u32>,
    /// The set's minimum, stored under its lock.
    next_timer: AtomicU64,
    /// epoll: when the slot's thread last polled without blocking.
    last_poll: AtomicU64,
}
impl Slot {
    fn new(poller: Arc<hirsel_core_net::Poller>) -> Self {
        Slot {
            handle: poller.handle(),
            waker: poller.waker(),
            poller,
            timers: Mutex::new(NONE),
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

/// Admit a unit's guarded unit and root-task stack pair before dispatch.
/// # Safety
/// `u` is dequeued by worker `ix` and has never run.
unsafe fn start_stack(p: &'static Pool, ix: usize, u: &Unit) -> bool {
    // SAFETY: `u` was exclusively dequeued by this worker.
    if let Some(unit_stack) = unsafe { (*u.stack.get()).unit.as_ref() } {
        // SAFETY: `u` owns its admitted stack; preparing on the worker thread
        // keeps the unit-stack page fault off the memory thread.
        unsafe {
            *u.sp.get() =
                unit_stack.prepare_frame(unit_entry, (u as *const Unit).cast_mut().cast());
        }
        return true;
    }
    #[cfg(test)]
    if check::modelled() {
        let pair = StackPair::new(p.plan.unit_stack_bytes, p.plan.task_stack_reserve);
        // SAFETY: by the contract.
        unsafe { p.memory.admit_pair_to_unit(u, pair, true) };
        return true;
    }
    // SAFETY: by the contract.
    unsafe { p.memory.admit_or_refill(p, ix, u) }
}

pub(super) unsafe extern "C" fn unit_entry(a: *mut u8, _b: *mut u8) -> ! {
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
    if untied() { cur() } else { ptr::null_mut() }
}

/// Spin only while a peer can make progress on another worker. Check work
/// on every iteration, including the private slot that queued excludes.
pub(crate) fn spin_connection(peer: &Unit, mut ready: impl FnMut() -> bool) -> bool {
    let p = pool();
    if p.w == 1
        || !matches!(peer.state.load(Ordering::Relaxed), RUNNING | NOTIFIED)
        || ptr::eq(peer, cur())
    {
        return false;
    }
    let ix = worker_ix();
    if ix >= p.w {
        return false;
    }
    for _ in 0..CONNECTION_SPINS {
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
        .is_some_and(|start| now().saturating_sub(start) < SLICE_NS)
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
    if ix < p.w && !cur().is_null() {
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

/// True when this thread is a slot thread running a pool unit, which can
/// park on a `Reply`.
pub(crate) fn in_unit() -> bool {
    !cur().is_null() && worker_ix() < pool().w
}

/// Put `up` on queue `q`, count it and tell the pool.
fn publish(p: &Pool, q: &Mutex<VecDeque<UP>>, up: UP) {
    push_counted(p, q, up);
    notify_worker(p, 0);
}

pub(super) fn publish_batch(p: &Pool, batch: Vec<UP>) {
    let n = batch.len();
    p.injector.lock().unwrap().extend(batch);
    p.queued.fetch_add(n, Ordering::SeqCst);
    for _ in 0..n.min(p.w) {
        notify_worker(p, 0);
    }
}

fn push_counted(p: &Pool, q: &Mutex<VecDeque<UP>>, up: UP) {
    q.lock().unwrap().push_back(up);
    p.queued.fetch_add(1, Ordering::SeqCst);
}

/// Wake a sleeping worker unless the workers already searching cover the
/// queued units. A handed worker counts as searching before the OS runs it,
/// so a burst wakes several workers at once instead of one wake at a time.
fn notify_worker(p: &Pool, reserved: usize) {
    let covered = |p: &Pool| {
        let s = p.searching.load(Ordering::SeqCst) + reserved;
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
/// at once: a MSG_RING completion on io_uring, the set's own wake otherwise.
fn wake_slot(p: &Pool, ix: usize) {
    bump(S_OS_WAKES);
    match p.slots[ix].waker {
        Some(waker) => sync::wake(waker),
        None => p.slots[ix].poller.wake(),
    }
}

#[path = "units_poll.rs"]
mod poll;
pub(crate) use poll::take_xready;
use poll::{check_slot, park_slot};

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
        if (*u.sp.get()).is_null() && !start_stack(p, ix, u) {
            // The memory thread maps the stack pair and queues the unit.
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
                // The terminal switch returned to the host stack. DONE
                // rejects further wakes, and nothing can resume this stack.
                p.memory.release_unit_stacks(ix, (*u.stack.get()).take());
                *u.sp.get() = std::ptr::null_mut();
                if p.units.retire(up) {
                    host_calls::stop_pool(p);
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
            After::Run if crate::run_stop::is_ended() => return,
            After::Run => {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitLeftWithoutReason"))
            }
        }
        // Between units the slot's own thread takes its readiness and fires
        // its timers: nobody else waits in its poller.
        check_slot(p, ix);
    }
}

fn pool_done(p: &Pool) -> bool {
    crate::run_stop::is_ended()
        || (p.stop.load(Ordering::SeqCst) && p.queued.load(Ordering::SeqCst) == 0)
}

/// Run slot `ix` on this thread until the pool stops.
fn run_slot(p: &'static Pool, ix: usize) {
    WORKER_IX.set(ix);
    let mut host: *mut u8 = std::ptr::null_mut();
    // A wake handed this worker the search role (already counted).
    let mut handed = false;
    loop {
        if !crate::run_stop::is_ended()
            && !handed
            && let Some(u) = find(p, ix)
        {
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if pool_done(p) {
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
                    if now() - t0 > p.spin_ns
                        || p.stop.load(Ordering::Relaxed)
                        || crate::run_stop::is_ended()
                    {
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
                notify_worker(p, 0);
            }
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if pool_done(p) {
            break;
        }
        handed = park_slot(p, ix);
    }
}

pub(crate) use host_calls::wake_all_for_stop;

fn worker_main(p: &'static Pool, ix: usize) {
    #[cfg(feature = "prof")]
    let _worker_profile = crate::prof::Worker::enter();
    crate::fiber::arm();
    let _alt = crate::fiber::Alternate::install().unwrap_or_else(|s| crate::run_stop::raise(s));
    run_slot(p, ix);
    WORKER_STATS.with(|stats| {
        for (total, local) in USTATS.iter().zip(stats) {
            total.fetch_add(local.get(), Ordering::Relaxed);
        }
    });
    #[cfg(feature = "testing")]
    observe::flush();
}
