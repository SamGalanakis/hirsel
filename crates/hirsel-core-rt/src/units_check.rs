//! Interleaving checker for the worker pool (test builds only).
//!
//! # For the next person changing the pool
//!
//! - Run: `buck2 test //:hirsel-core-rt-unittest -- --test-arg=units::check`
//!   (or the harness binary with `--exact units::check::tests::<case>`).
//!   Each case searches seeds 0..2000 (`HIRSEL_CHECK_SEEDS`, `_FROM`), one
//!   schedule per child process, and fails with the first seed that breaks
//!   the law, printing threads, kernel and pool state and the last steps.
//! - Replay a seed in this process, report to a file:
//!   `HIRSEL_CHECK_CHILD=<case> HIRSEL_CHECK_SEED=<n> HIRSEL_CHECK_OUT=<file>
//!   <harness> --exact units::check::tests::<case> --nocapture`. A replay
//!   is byte-identical; `HIRSEL_CHECK_LIVE=1` prints every step as it runs.
//!   `HIRSEL_CHECK_ALL=1` runs every seed and lists each failing one.
//! - Add a case: a `Case` in `CASES` (workers, units, unit bodies, outside
//!   producers, the goals that count as served, timing drawn from the seed)
//!   and a `#[test]` calling `check`. A case must be red with the fix it
//!   guards undone, at a seed you can name; draw its timing near the moment
//!   that matters, or wide ranges never line the race up.
//! - The rule: every sleep, park, wake, steal or poll path in the pool, and
//!   any atomic, lock, thread park, clock read or epoll/eventfd call it
//!   decides on, goes through `units::sync` (`units_sync.rs`), never std or
//!   libc directly. A call that bypasses the seam is not a scheduling point,
//!   so the checker silently stops exploring the race around it. An ordinary
//!   build re-exports std and libc there, so this costs nothing.
//!
//! # What it runs
//!
//! The pool's own code runs unchanged: `worker_main`, `find`, `enqueue`,
//! `wake`, `notify_worker`, `add_timer`, `fire_timers`, `check_slot` and
//! `poll_slot`, with units that switch onto their own stacks and call
//! `park`, `wake_id`, `maybe_yield` and `take_xready` as the task scheduler
//! does. An outside completion goes through its real `Outside` and the
//! unit's waiter (`tasks::Readies`), whose state also passes the seam. Every
//! worker is a real OS thread, but only one runs at a time: each call
//! through the seam is a scheduling point where a seeded scheduler (uniform,
//! or PCT where a case asks for it) picks the next thread. A schedule is
//! therefore a function of the seed alone.
//!
//! Kernel and host model:
//! - Time is modelled: every scheduling point advances it by `DELTA_NS`
//!   (20 ns); when every thread waits, it jumps to the earliest timeout.
//! - Each worker slot has its own epoll set and wake eventfd, registered
//!   edge-triggered and never read: a write to the eventfd queues one wake
//!   on that slot's set, which the next `epoll_wait` on it reports once.
//! - A socket edge (edge-triggered) joins its home slot's ready list once
//!   and stays there until one `epoll_wait` on that slot reports it. A
//!   unit's home is the slot it registered from (`wait_arrival`, or
//!   `register`): the worker slot its thread runs.
//! - `epoll_wait` returns at once if anything is ready, returns 0 for a
//!   zero timeout, and otherwise blocks until something is ready or modelled
//!   time reaches its timeout (milliseconds, as the pool passes it).
//! - The statistics, log and untied-switch objects are not scheduling
//!   points (they still block when held).
//! - A host write that never returns (stalled standard output) blocks for
//!   ever, after the pool records entry.
//! - The memory thread is not modelled: a scheduled thread runs its jobs
//!   in place (a first stack's mapping, then the unit's publish).
//! - Outside producers (a client, a sync helper) are checker threads; a
//!   helper waiting for work rechecks every 300 ns of modelled time.
//! - Not modelled: the crossing eventfd and the reactor's own threads; no
//!   case uses a crossing.
//!
//! The law every case checks is liveness: once outside producers stop,
//! every runnable unit runs, every ready descriptor is read and every
//! expired deadline fires within `BOUND` scheduler steps. A run fails when
//! every thread waits with no timeout pending while a goal is unmet (stuck
//! for ever), or when the bound passes.
use super::{
    BASE, Base, POOL, RUN, RunFn, UNTIED_ON, USTATS, create_unit, cur, enable_pool, maybe_yield,
    park as unit_park, pool, take_xready, wake_id, worker_main,
};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
#[path = "units_conn_check.rs"]
mod conn;
#[path = "units_park_waits_check.rs"]
mod park_waits;
#[path = "units_returnable_check.rs"]
mod returnable;
use std::fmt::Write as _;
use std::panic::Location;
use std::sync::atomic::Ordering;

// ------------------------------------------------------------ scheduler

/// Modelled time starts 10 s after the real monotonic clock read when the run
/// begins, so stamps the pool took while being set up (unit creation) are
/// never ahead of it. Trace times are printed relative to it.
static T0V: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn t0() -> u64 {
    T0V.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Wait {
    Run,
    Lock(usize),
    /// Blocked in slot `.0`'s epoll set.
    Epoll(usize, Option<u64>),
    Sleep(u64),
    Host,
    Done,
}

struct Th {
    name: String,
    wait: Wait,
    os: Option<std::thread::Thread>,
    producer: bool,
    at: &'static str,
    line: u32,
}

/// One slot's epoll set as the kernel holds it.
#[derive(Debug)]
struct KSlot {
    handle: hirsel_core_net::ready::Handle,
    waker: hirsel_core_net::ready::Waker,
    /// A wake queued on the set and not yet reported (edge-triggered).
    wake: bool,
    edges: VecDeque<(u64, hirsel_core_net::ready::Ready)>,
}
struct Kernel {
    slots: Vec<KSlot>,
}

struct State {
    threads: Vec<Th>,
    cur: usize,
    step: u64,
    time: u64,
    rng: u64,
    delta: u64,
    /// (thread, line): a note goes on its own thread's latest line.
    trace: VecDeque<(usize, String)>,
    trace_cap: usize,
    names: HashMap<usize, String>,
    /// Objects made during a run, named by the range they occupy.
    ranges: Vec<(usize, usize, String)>,
    quiet: Vec<(usize, usize)>,
    kernel: Kernel,
    quiet_from: Option<u64>,
    bound: u64,
    max_steps: u64,
    goals: fn() -> bool,
    out: String,
    case: &'static str,
    seed: u64,
    /// PCT: per-thread priorities and the steps at which the running
    /// thread's priority drops. Empty: uniform random choice.
    prio: Vec<u64>,
    changes: Vec<u64>,
    pct_until: u64,
    /// Echo every trace line as it happens (`HIRSEL_CHECK_LIVE`).
    live: bool,
}

struct Sched {
    st: std::sync::Mutex<State>,
}
static SCHED: std::sync::OnceLock<Sched> = std::sync::OnceLock::new();

thread_local! {
    static ME: Cell<usize> = const { Cell::new(usize::MAX) };
    /// The thread's last seam operation was quiet (no trace line to annotate).
    static LAST_QUIET: Cell<bool> = const { Cell::new(false) };
}
#[inline(never)]
/// Whether the calling thread is one the checker schedules.
pub(super) fn modelled() -> bool {
    me().is_some()
}
fn me() -> Option<usize> {
    let m = ME.get();
    (m != usize::MAX && SCHED.get().is_some()).then_some(m)
}
fn lock() -> std::sync::MutexGuard<'static, State> {
    SCHED
        .get()
        .unwrap()
        .st
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn rand(&mut self) -> u64 {
        // splitmix64
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn name(&self, addr: usize) -> String {
        // An unnamed object prints without its address, which changes from
        // run to run, so a replayed trace compares byte for byte.
        if let Some(n) = self.names.get(&addr) {
            return n.clone();
        }
        match self.ranges.iter().find(|r| (r.0..r.1).contains(&addr)) {
            Some(r) => r.2.clone(),
            None => "an unnamed object".into(),
        }
    }
    fn log(&mut self, th: usize, text: String) {
        let line = format!(
            "{:>7} {:>9.3}us {:<7} {}",
            self.step,
            (self.time - t0()) as f64 / 1000.0,
            self.threads[th].name,
            text
        );
        if self.live {
            eprintln!("{line}");
        }
        if self.trace.len() == self.trace_cap {
            self.trace.pop_front();
        }
        self.trace.push_back((th, line));
    }
    fn wake_timed(&mut self) {
        let now = self.time;
        for t in &mut self.threads {
            match t.wait {
                Wait::Epoll(_, Some(d)) | Wait::Sleep(d) if d <= now => t.wait = Wait::Run,
                _ => {}
            }
        }
    }
    fn wake_epoll(&mut self, k: usize) {
        for t in &mut self.threads {
            if let Wait::Epoll(j, _) = t.wait
                && j == k
            {
                t.wait = Wait::Run;
            }
        }
    }
    fn complete(&self) -> bool {
        (self.goals)()
    }

    /// Pick the next thread to run, advancing time over idle stretches.
    fn choose(&mut self) -> usize {
        loop {
            let runnable: Vec<usize> = (0..self.threads.len())
                .filter(|&i| self.threads[i].wait == Wait::Run)
                .collect();
            if !runnable.is_empty() {
                if !self.prio.is_empty() && self.step < self.pct_until {
                    return *runnable.iter().max_by_key(|&&i| self.prio[i]).unwrap();
                }
                let k = (self.rand() % runnable.len() as u64) as usize;
                return runnable[k];
            }
            let next = self
                .threads
                .iter()
                .filter_map(|t| match t.wait {
                    Wait::Epoll(_, Some(d)) | Wait::Sleep(d) => Some(d),
                    _ => None,
                })
                .min();
            match next {
                Some(d) => {
                    self.time = self.time.max(d);
                    self.wake_timed();
                }
                None => {
                    if self.complete() {
                        self.finish(0, "pass");
                    }
                    self.finish(
                        1,
                        "FAIL stuck: every thread waits with no timeout pending while a goal is unmet; no later step can serve it",
                    );
                }
            }
        }
    }
    /// Account one scheduling point by thread `me`, which has already set its
    /// wait state, and hand the baton on.
    fn point(&mut self, me: usize) -> usize {
        self.step += 1;
        self.time += self.delta;
        self.wake_timed();
        if self.complete() {
            self.finish(0, "pass");
        }
        if let Some(q) = self.quiet_from
            && self.step - q > self.bound
        {
            self.finish(
                1,
                "FAIL bound: a goal is still unmet this many steps after outside producers stopped",
            );
        }
        if self.step > self.max_steps {
            self.finish(
                2,
                "INCONCLUSIVE: step limit before outside producers stopped",
            );
        }
        if !self.prio.is_empty() && self.changes.contains(&self.step) {
            // PCT change point: the running thread drops below every other.
            let low = self.prio.iter().copied().min().unwrap_or(0);
            self.prio[me] = low.saturating_sub(1);
        }
        self.choose()
    }
    fn dump(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "case {} seed {} step {} time {:.3}us",
            self.case,
            self.seed,
            self.step,
            (self.time - t0()) as f64 / 1000.0
        );
        let _ = writeln!(s, "threads:");
        for t in &self.threads {
            let _ = writeln!(
                s,
                "  {:<7} {:?} last call {}:{}",
                t.name, t.wait, t.at, t.line
            );
        }
        for (k, ks) in self.kernel.slots.iter().enumerate() {
            let _ = writeln!(
                s,
                "kernel slot {k}: wake pending {}, edges queued {:?}",
                ks.wake, ks.edges
            );
        }
        s.push_str(&pool_state());
        let _ = writeln!(s, "trace (last {} steps):", self.trace.len());
        for (_, l) in &self.trace {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
    fn finish(&self, code: i32, what: &str) -> ! {
        let mut text = format!("{what}\n{}", self.dump());
        if code == 0 {
            text.push_str(&conn::obs_text());
        }
        let _ = std::fs::write(&self.out, &text);
        if std::env::var_os("HIRSEL_CHECK_PRINT").is_some() {
            eprintln!("{text}");
        }
        crate::coverage::exiting(code);
        std::process::exit(code)
    }
}

/// Hand the baton to `next` and wait for it to come back to `me`.
fn handoff(mut st: std::sync::MutexGuard<'static, State>, me: usize, next: usize) {
    st.cur = next;
    if next == me {
        return;
    }
    let os = st.threads[next].os.clone().unwrap();
    drop(st);
    os.unpark();
    loop {
        std::thread::park();
        if lock().cur == me {
            return;
        }
    }
}

/// A scheduling point before an operation of the controlled thread `me`.
fn point(
    me: usize,
    wait: Wait,
    what: impl FnOnce(&State) -> String,
    at: &'static Location<'static>,
) {
    let mut st = lock();
    st.threads[me].wait = wait;
    st.threads[me].at = short(at.file());
    st.threads[me].line = at.line();
    let line = |st: &State| format!("{}:{} {}", short(at.file()), at.line(), what(st));
    // A thread that goes on running performs its operation when it next
    // holds the baton, so its line is written then, in the order the
    // operations take effect. A thread that starts to wait is logged now.
    if wait != Wait::Run {
        let text = line(&st);
        st.log(me, text);
        let next = st.point(me);
        handoff(st, me, next);
        return;
    }
    let next = st.point(me);
    handoff(st, me, next);
    let mut st = lock();
    let text = line(&st);
    st.log(me, text);
}
fn short(f: &'static str) -> &'static str {
    f.rsplit('/').next().unwrap_or(f)
}
/// Append a result to the running thread's last trace line.
fn note(text: impl FnOnce() -> String) {
    if me().is_none() || LAST_QUIET.get() {
        return;
    }
    let mut st = lock();
    let m = me().unwrap();
    if let Some((_, l)) = st.trace.iter_mut().rev().find(|(t, _)| *t == m) {
        l.push_str(" -> ");
        l.push_str(&text());
    }
}
fn quiet(addr: usize) -> bool {
    let st = lock();
    st.quiet.iter().any(|&(a, b)| (a..b).contains(&addr))
}
/// A plain step for checker code (unit bodies and producers): an event in
/// the trace and a scheduling point.
fn say(text: String) {
    if let Some(m) = me() {
        point(m, Wait::Run, |_| text, Location::caller());
    }
}

// ----------------------------------------------------------- seam items

#[path = "units_check_seam.rs"]
pub(crate) mod seam;

/// A unit record read without a scheduling point, for the checker's own
/// trace and dumps (which run while the scheduler lock is held).
fn unit_raw(id: u32) -> Option<&'static super::Unit> {
    // SAFETY: unit records are leaked; a published pointer stays valid.
    unsafe { pool().units.slot(id)?.raw().as_ref() }
}
fn unit_name(addr: usize) -> String {
    if addr == 0 {
        return "none".into();
    }
    for id in 0..16u32 {
        if let Some(u) = unit_raw(id)
            && u as *const _ as usize == addr
        {
            return format!("unit {id}");
        }
    }
    "an unnamed object".into()
}

fn pool_state() -> String {
    let p = pool();
    let ids = |q: &VecDeque<super::UP>| {
        q.iter()
            .map(|u| unit_name(u.0 as usize))
            .collect::<Vec<_>>()
    };
    let mut s = String::new();
    let _ = writeln!(
        s,
        "pool: queued {} searching {} nsleep {} live {} stop {}",
        p.queued.raw(),
        p.searching.raw(),
        p.nsleep.raw(),
        super::LIVE.raw(),
        p.stop.raw()
    );
    for ix in 0..p.w {
        let _ = writeln!(
            s,
            "  worker {ix}: private slot {}, local queue {:?}, handed {}, next timer {}, last poll {}, timers {:?}",
            unit_name(p.lifo[ix].raw() as usize),
            p.locals[ix].peek(ids),
            p.handed[ix].raw(),
            rel(p.slots[ix].next_timer.raw()),
            rel(p.slots[ix].last_poll.raw()),
            p.slots[ix]
                .timers
                .peek(|head| super::timers::snapshot(*head, rel))
        );
    }
    let _ = writeln!(
        s,
        "  injector {:?}, idle {:?}",
        p.injector.peek(ids),
        p.idle.peek(|v| v.clone())
    );
    for id in 0..16u32 {
        if let Some(u) = unit_raw(id) {
            let state = ["PARKED", "QUEUED", "RUNNING", "NOTIFIED", "DONE"]
                .get(u.state.raw() as usize)
                .copied()
                .unwrap_or("?");
            let _ = writeln!(
                s,
                "  unit {id}: {state}, arrivals pending {}",
                u.xpending.raw()
            );
        }
    }
    s
}
/// An integer as the trace shows it: a value near modelled time as an
/// instant since the start, so traces do not depend on the real clock.
fn shown(v: u64) -> String {
    let t = t0();
    if v == u64::MAX {
        "MAX".into()
    } else if v > t.saturating_sub(20_000_000_000) && v < t + 1_000_000_000_000 {
        format!("t{}", rel(v))
    } else {
        v.to_string()
    }
}
/// A modelled instant as microseconds since the start, or "none".
fn rel(t: u64) -> String {
    if t == u64::MAX {
        "none".into()
    } else {
        format!("{:.3}us", t.saturating_sub(t0()) as f64 / 1000.0)
    }
}

// ------------------------------------------------- unit bodies and cases

/// Case parameters drawn from the seed before the run starts.
static PARAM: [std::sync::atomic::AtomicU64; 8] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 8];
fn param(i: usize) -> u64 {
    PARAM[i].load(Ordering::Relaxed)
}
/// Goals: set by a unit body when it has been served.
static GOAL: [std::sync::atomic::AtomicBool; 8] =
    [const { std::sync::atomic::AtomicBool::new(false) }; 8];
static NEEDED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
fn goals_met() -> bool {
    let need = NEEDED.load(Ordering::Relaxed);
    (0..8).all(|i| need & (1 << i) == 0 || GOAL[i].load(Ordering::Relaxed))
}
fn served(i: usize, what: &str) {
    GOAL[i].store(true, Ordering::Relaxed);
    say(format!("goal {i} met: {what}"));
}
/// Flags between units and producers; their loads are scheduling points.
static FLAG: [seam::AtomicBool; 4] = [const { seam::AtomicBool::new(false) }; 4];

fn now() -> u64 {
    seam::monotonic_ns()
}
fn this_unit() -> u32 {
    // SAFETY: called by a running unit.
    unsafe { (*cur()).id }
}
/// A computing unit: it reaches a cooperative switch point on every step.
fn compute_until(t: u64) {
    while now() < t {
        maybe_yield();
    }
}
/// The task scheduler's wait for a deadline: park with it, recheck on resume.
fn sleep_until(t: u64) {
    say(format!(
        "unit {} waits for its deadline {}",
        this_unit(),
        rel(t)
    ));
    while now() < t {
        unit_park(Some(t));
    }
}
/// The task scheduler's wait for a wake from elsewhere (a flag set first).
fn wait_flag(f: usize) {
    while !FLAG[f].load(Ordering::SeqCst) {
        unit_park(None);
    }
}
/// Each unit's socket home: the slot its last registration came from.
static HOME: [std::sync::atomic::AtomicUsize; 16] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 16];
/// Register the running unit's socket as the reactor does: on the slot its
/// thread runs (`units::here`).
fn register() {
    let here = super::here().unwrap();
    HOME[here.unit as usize].store(here.slot, Ordering::Relaxed);
    say(format!(
        "unit {} registers its socket on slot {}",
        here.unit, here.slot
    ));
}
/// The task scheduler's wait for readiness on a key it registers first.
fn wait_arrival(ident: u64) {
    register();
    await_arrival(ident);
}
/// The wait for readiness on a key already registered.
fn await_arrival(ident: u64) {
    loop {
        let mut got = false;
        take_xready(|id, _| got |= id == ident);
        if got {
            return;
        }
        unit_park(None);
    }
}
/// A blocking host call that never returns (a write to a stalled pipe).
fn host_block() -> ! {
    let m = me().unwrap();
    point(
        m,
        Wait::Host,
        |_| "blocks in a host write that never returns".into(),
        Location::caller(),
    );
    unreachable!()
}
fn compute_forever() -> ! {
    loop {
        maybe_yield();
        let _ = now();
    }
}
/// A socket edge for unit `unit`'s key `ident` reaches its home slot.
fn edge(unit: u32, ident: u64) {
    let k = HOME[unit as usize].load(Ordering::Relaxed);
    say(format!(
        "a client's bytes make unit {unit} key {ident} readable on slot {k}"
    ));
    let mut st = lock();
    st.kernel.slots[k].edges.push_back((
        (u64::from(unit) << hirsel_core_net::SHARED_UNIT_SHIFT) | ident,
        hirsel_core_net::ready::Ready::READABLE,
    ));
    st.wake_epoll(k);
}
fn sleep_producer(t: u64) {
    let m = me().unwrap();
    point(
        m,
        Wait::Sleep(t),
        |_| format!("sleeps until {}", rel(t)),
        Location::caller(),
    );
}

pub(super) struct Case {
    pub(super) name: &'static str,
    workers: usize,
    units: u32,
    body: fn(u32) -> u8,
    /// Outside producers (a client, a helper thread), as checker threads.
    producers: &'static [(&'static str, fn())],
    goals: u32,
    /// Draw the case's parameters (times) from the seed.
    params: fn(&mut dyn FnMut(u64, u64) -> u64),
    /// The case's default scheduler: `None` for uniform random choice, or
    /// PCT with (depth, steps). `HIRSEL_CHECK_PCT` (a depth, or `off`)
    /// and `HIRSEL_CHECK_PCT_STEPS` override it.
    pct: Option<(u64, u64)>,
}

/// Name the object at `x` in the trace (one made during the run).
fn name_object<T>(x: *const T, s: &str) {
    lock()
        .ranges
        .push((x as usize, x as usize + size_of::<T>(), s.to_string()));
}

/// A fresh draw from the seed's generator, for durations inside a run.
fn draw(lo: u64, hi: u64) -> u64 {
    let mut st = lock();
    lo + st.rand() % (hi - lo + 1)
}
/// An outside thread's wait for a flag, rechecked every 300 ns of modelled
/// time (a condition-variable wait with a short poll).
fn producer_wait_flag(f: usize) {
    while !FLAG[f].load(Ordering::SeqCst) {
        sleep_producer(now() + 300);
    }
}

/// The requeue case's outside work: its ready token and the issuing unit's
/// `Outside`, handed to the helper (not a pool object, so not a scheduling
/// point).
const SYNC_TOKEN: u64 = 0x5ec0;
static SYNC_OUTSIDE: std::sync::atomic::AtomicPtr<crate::tasks::Outside> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// The helper's completion of the outside work its unit handed over: push the
/// ready, then release the unit's outside count.
fn complete_outside() {
    let outside = SYNC_OUTSIDE.swap(std::ptr::null_mut(), Ordering::SeqCst);
    // SAFETY: the unit published this box once, and only the helper takes it.
    unsafe { Box::from_raw(outside) }.complete(SYNC_TOKEN);
}

/// Iterations of the looping unit in the stale-bound case.
const TICKS: u32 = 40;

pub(super) static CASES: &[Case] = &[
    // 1. Stale sleep bound (go/VERIFY.md "Poller sleep race"). One unit
    // alternates a short computation with a short sleep, TICKS times. Each
    // sleep's deadline becomes the poller's `poll_until`; when the poller
    // fires it and goes back to an idle poll with no later timer, the
    // unit's next sleep is published against that stale bound. Every sleep
    // must end.
    Case {
        name: "stale_sleep_bound",
        workers: 2,
        units: 1,
        body: |_| {
            for k in 0..TICKS {
                compute_until(now() + draw(0, param(0)));
                say(format!("unit 0 starts sleep {k}"));
                sleep_until(now() + draw(1_000, param(1)));
            }
            served(0, "unit 0 finished all its sleeps");
            0
        },
        producers: &[],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(1_000, 30_000), Ordering::Relaxed); // longest computation per tick
            PARAM[1].store(r(5_000, 300_000), Ordering::Relaxed); // longest sleep per tick
        },
        pct: None,
    },
    // 2. Busy owner (slot pollers). A socket's readiness reaches only its
    // home slot's poller, and only that slot's thread waits in it. Unit 0
    // registers on its slot, wakes unit 1 (onto the same worker's private
    // slot) and waits for its client's bytes. Unit 1 computes for longer
    // than the poll cadence; the bytes arrive meanwhile. It then wakes unit
    // 2 and ends; unit 2 computes for ever on that worker. The owner's
    // check at the end of unit 1's turn must take the arrival and hand it
    // on, so the other worker (parked in its own poller) runs unit 0.
    Case {
        name: "nobody_in_poller",
        workers: 2,
        units: 3,
        body: |id| {
            match id {
                0 => {
                    FLAG[0].store(true, Ordering::SeqCst);
                    wake_id(1);
                    wait_arrival(5);
                    served(0, "unit 0 read its client's bytes");
                }
                1 => {
                    wait_flag(0);
                    compute_until(now() + param(0));
                    FLAG[1].store(true, Ordering::SeqCst);
                    wake_id(2);
                }
                _ => {
                    wait_flag(1);
                    compute_forever();
                }
            }
            0
        },
        producers: &[("client", || {
            sleep_producer(t0() + param(1));
            edge(0, 5);
        })],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(150_000, 250_000), Ordering::Relaxed); // unit 1 computes
            PARAM[1].store(r(20_000, 140_000), Ordering::Relaxed); // the client sends
        },
        pct: None,
    },
    // 2b. Deadline on a busy slot (slot pollers). Only a slot's own thread
    // fires its timers. Unit 0 wakes unit 1 onto its worker and sleeps;
    // unit 1 computes past unit 0's deadline, wakes unit 2 and ends; unit 2
    // computes for ever on that worker. The deadline must fire at the end
    // of unit 1's turn, the slot's next unit switch, so the other worker
    // runs unit 0.
    Case {
        name: "busy_slot_deadline",
        workers: 2,
        units: 3,
        body: |id| {
            match id {
                0 => {
                    FLAG[0].store(true, Ordering::SeqCst);
                    wake_id(1);
                    sleep_until(now() + param(0));
                    served(0, "unit 0's deadline fired");
                }
                1 => {
                    wait_flag(0);
                    compute_until(now() + param(0) + param(1));
                    FLAG[1].store(true, Ordering::SeqCst);
                    wake_id(2);
                }
                _ => {
                    wait_flag(1);
                    compute_forever();
                }
            }
            0
        },
        producers: &[],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(5_000, 50_000), Ordering::Relaxed); // unit 0's sleep
            PARAM[1].store(r(0, 30_000), Ordering::Relaxed); // unit 1 computes past it
        },
        pct: None,
    },
    // 3. Requeue without notification (lens-sleep/VERIFY.md M4 variant 2).
    // Unit 0 wakes unit 1, hands outside work to a helper thread (not a
    // worker) and waits for it; the helper finishes a few microseconds later
    // and completes it through the real completion path (the unit's
    // `Outside`, its waiter, the pool wake). Unit 1 blocks in a host write
    // that never returns (stalled standard output). Unit 0 must run again.
    Case {
        name: "requeue_without_notification",
        workers: 2,
        units: 2,
        body: |id| {
            match id {
                0 => {
                    compute_until(t0() + param(0));
                    FLAG[0].store(true, Ordering::SeqCst);
                    wake_id(1);
                    // Hand over the unit's outside-work count.
                    let readies = crate::tasks::Readies::for_unit(0);
                    let outside = crate::tasks::Outside::for_readies(readies.clone());
                    name_object(std::sync::Arc::as_ptr(&readies), "unit 0 waiter");
                    SYNC_OUTSIDE.store(Box::into_raw(Box::new(outside)), Ordering::SeqCst);
                    FLAG[2].store(true, Ordering::SeqCst);
                    // Wait as the task scheduler's block does: take the
                    // ready, else park.
                    while !readies.take_for_test(SYNC_TOKEN) {
                        unit_park(None);
                    }
                    served(0, "unit 0 ran after its outside work completed");
                }
                _ => {
                    wait_flag(0);
                    host_block();
                }
            }
            0
        },
        producers: &[("helper", || {
            producer_wait_flag(2);
            sleep_producer(now() + param(1));
            complete_outside();
        })],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(30_000, 100_000), Ordering::Relaxed); // unit 0 computes first
            // The helper's sync time. The race window is the few steps between
            // unit 0's take of its ready and its park; with 0-2 us it was hit
            // 1 in 2000 schedules with the fix undone, with 0-400 ns 7 in 1000.
            PARAM[1].store(r(0, 400), Ordering::Relaxed);
        },
        pct: None,
    },
    // 3b. Quiescence after outside work (sim-idle-hang-1854). Unit 0 hands
    // outside work to a helper, takes its ready, then decides quiescence as the
    // task scheduler's idle does: while outside work is counted it parks,
    // and once none is it may stop with Deadlock or wake its idle waits.
    // The helper pushes the ready before it drops the count, so the unit
    // can take the ready and park while the count is still one. The drop
    // to zero must wake it, or the run waits for ever with nothing to come.
    Case {
        name: "outside_end_wakes_waiter",
        workers: 1,
        units: 1,
        body: |_| {
            let readies = crate::tasks::Readies::for_unit(0);
            let outside = crate::tasks::Outside::for_readies(readies.clone());
            name_object(std::sync::Arc::as_ptr(&readies), "unit 0 waiter");
            SYNC_OUTSIDE.store(Box::into_raw(Box::new(outside)), Ordering::SeqCst);
            FLAG[2].store(true, Ordering::SeqCst);
            while !readies.take_for_test(SYNC_TOKEN) {
                unit_park(None);
            }
            while readies.has_outside() {
                unit_park(None);
            }
            served(0, "unit 0 saw its outside work end");
            0
        },
        producers: &[("helper", || {
            producer_wait_flag(2);
            sleep_producer(now() + param(0));
            complete_outside();
        })],
        goals: 0b1,
        params: |r| {
            // The helper's sync time; the window is between its push and
            // its count drop, whatever the sync took.
            PARAM[0].store(r(0, 400), Ordering::Relaxed);
        },
        pct: None,
    },
    // 4. Injector starved by a private-slot pair (mc-units-sched). One
    // worker. Units 0 and 1 wake each other and park, for ever, so the
    // worker always has a unit in its private slot or (after LIFO_RUNS runs)
    // one it runs at once. Unit 2 waits for a helper thread, whose wake goes
    // to the injector. The worker must take it from there.
    Case {
        name: "injector_starved",
        workers: 1,
        units: 3,
        body: |id| {
            match id {
                0 | 1 => loop {
                    wake_id(1 - id);
                    unit_park(None);
                },
                _ => {
                    wait_flag(1);
                    served(0, "unit 2 ran after the helper woke it");
                }
            }
            0
        },
        producers: &[("helper", || {
            sleep_producer(t0() + param(0));
            FLAG[1].store(true, Ordering::SeqCst);
            wake_id(2);
        })],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(1_000, 20_000), Ordering::Relaxed); // the helper's wake
        },
        pct: None,
    },
    Case {
        name: "connection_spin_yields_to_work",
        workers: 2,
        units: 3,
        body: |id| {
            match id {
                0 => {
                    while !FLAG[0].load(Ordering::SeqCst) {
                        let _ = now();
                    }
                    // These two units occupy the workers; unit 2 is queued.
                    assert!(!super::spin_connection(super::unit_ref(1).unwrap(), || {
                        panic!("checked the port while unit 2 was runnable");
                    }));
                    wait_flag(1);
                    served(0, "unit 0 resumed after unit 2 ran");
                }
                1 => {
                    FLAG[0].store(true, Ordering::SeqCst);
                    while !FLAG[1].load(Ordering::SeqCst) {
                        let _ = now();
                    }
                }
                _ => {
                    FLAG[1].store(true, Ordering::SeqCst);
                    wake_id(0);
                }
            }
            0
        },
        producers: &[],
        goals: 0b1,
        params: |_| {},
        pct: None,
    },
    // 5-8. The connection path and the ledger on pool units
    // (`units_conn_check.rs`, which states the cases and their laws).
    Case {
        name: "conn_wait_serve",
        workers: 2,
        units: 2,
        body: conn::wait_serve,
        producers: &[],
        goals: 0b11,
        params: conn::params_wait_serve,
        pct: None,
    },
    Case {
        name: "conn_seal_in_flight",
        workers: 2,
        units: 2,
        body: conn::seal_in_flight,
        producers: &[],
        goals: 0b11,
        params: conn::params_seal_in_flight,
        pct: None,
    },
    Case {
        name: "conn_helper_forward",
        workers: 3,
        units: 3,
        body: conn::helper_forward,
        producers: &[],
        goals: 0b111,
        params: conn::params_helper_forward,
        pct: None,
    },
    Case {
        name: "conn_release_race",
        workers: 2,
        units: 2,
        body: conn::release_race,
        producers: &[],
        goals: 0b11,
        params: conn::params_release_race,
        pct: None,
    },
    Case {
        name: "conn_slot_reuse",
        workers: 3,
        units: 3,
        body: conn::slot_reuse,
        producers: &[],
        goals: 0b111,
        params: |r| {
            PARAM[0].store(r(0, 400), Ordering::Relaxed);
            PARAM[1].store(r(0, 1), Ordering::Relaxed);
        },
        pct: None,
    },
    // 16. A client that returned its stack at an owned receive boundary is
    // woken by its provider after another task reused and overwrote that
    // stack (mc-units-receive-park). Its provider sequence and carrier
    // lengths equal the stackful client's (`units_returnable_check.rs`).
    Case {
        name: "returnable_stack_reuse",
        workers: 2,
        units: 1,
        body: returnable::body,
        producers: &[("provider", returnable::producer)],
        goals: 0b1,
        params: |_| {},
        pct: None,
    },
    // 18. A receiver that returned its stack at a channel receive is woken
    // by each send after another task reused and overwrote that stack, and
    // cancelled while parked. What it receives equals the stackful
    // receiver's (`units_returnable_check.rs`).
    Case {
        name: "returnable_receive",
        workers: 2,
        units: 1,
        body: returnable::receive_body,
        producers: &[],
        goals: 0b1,
        params: |_| {},
        pct: None,
    },
    // 19. The same receivers, each reached through a transfer from a setup
    // root with a large record, which the receiver reuses for a scribbled
    // block once entered: what they receive equals the stackful receivers'
    // (`units_returnable_check.rs`).
    Case {
        name: "returnable_transfer",
        workers: 2,
        units: 1,
        body: returnable::receive_transfer_body,
        producers: &[],
        goals: 0b1,
        params: |_| {},
        pct: None,
    },
    // 20. A returnable client gives its stack back at a connection receive,
    // a full connection's send, a task join and a helper join, against a real
    // helper unit; what it observes equals the stackful client's
    // (`units_park_waits_check.rs`).
    Case {
        name: "park_waits",
        workers: 3,
        units: 1,
        body: park_waits::body,
        producers: &[],
        goals: 0b1,
        params: |_| {},
        pct: None,
    },
    Case {
        name: "conn_flat_slot_reuse",
        workers: 3,
        units: 3,
        body: conn::flat_slot_reuse,
        producers: &[],
        goals: 0b111,
        params: |r| {
            PARAM[0].store(r(0, 400), Ordering::Relaxed);
            PARAM[1].store(r(0, 1), Ordering::Relaxed);
        },
        pct: None,
    },
];

/// Run one schedule of `case` and exit the process with its verdict.
pub(super) fn run_one(case: &Case, seed: u64, out: &str) -> ! {
    std::panic::set_hook(Box::new({
        let out = out.to_string();
        move |info| {
            let text = format!(
                "PANIC {info}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
            let _ = std::fs::write(&out, &text);
            eprintln!("{text}");
            std::process::exit(3);
        }
    }));
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let pollers = hirsel_core_net::slot_readiness_pollers(case.workers, super::here).unwrap();
    // Setup stamps (unit creation) use the real clock; start the model 10 s
    // ahead of it so no setup stamp is later than modelled time.
    T0V.store(
        crate::os::monotonic_ns() + 10_000_000_000,
        Ordering::Relaxed,
    );
    let w = case.workers;
    let mut st = State {
        threads: Vec::new(),
        cur: usize::MAX,
        step: 0,
        time: t0(),
        rng: seed ^ 0x5eed_0000_0000_0000,
        delta: env("HIRSEL_CHECK_DELTA_NS", 20),
        trace: VecDeque::new(),
        trace_cap: env("HIRSEL_CHECK_TRACE", 600) as usize,
        names: HashMap::new(),
        ranges: Vec::new(),
        live: std::env::var_os("HIRSEL_CHECK_LIVE").is_some(),
        quiet: Vec::new(),
        kernel: Kernel {
            slots: pollers
                .iter()
                .map(|p| KSlot {
                    handle: p.handle(),
                    waker: p.waker().expect("the checker's slots use epoll pollers"),
                    wake: false,
                    edges: VecDeque::new(),
                })
                .collect(),
        },
        quiet_from: None,
        bound: env("HIRSEL_CHECK_BOUND", 100_000),
        max_steps: env("HIRSEL_CHECK_MAX_STEPS", 3_000_000),
        goals: goals_met,
        out: out.to_string(),
        case: case.name,
        seed,
        prio: Vec::new(),
        changes: Vec::new(),
        pct_until: 0,
    };
    // Parameters come from the seed's stream before any scheduling choice.
    (case.params)(&mut |lo, hi| lo + st.rand() % (hi - lo + 1));
    NEEDED.store(case.goals, Ordering::Relaxed);
    super::LIVE.store(case.units as usize, Ordering::SeqCst);
    let plan = super::UntiedPlan::derive(
        case.units as usize,
        w,
        0,
        super::plan::HostEnvelope::UNLIMITED,
        super::plan::ReactorChoice::Readiness,
    )
    .unwrap();
    let _ = POOL.set(super::launch::new_pool(plan, pollers, 5_000));
    pool().net_used.store(true, Ordering::Relaxed);
    static BODY: std::sync::OnceLock<fn(u32) -> u8> = std::sync::OnceLock::new();
    let _ = BODY.set(case.body);
    static DISPATCH: fn(&crate::entry::OsConfig) -> u8 = |_| (BODY.get().unwrap())(this_unit());
    let _ = RUN.set(RunFn(&DISPATCH));
    // SAFETY: an all-zero configuration: no arguments, grants or operations.
    let _ = BASE.set(Base(unsafe { std::mem::zeroed() }));
    enable_pool();
    let p = pool();
    std::mem::forget(p.memory.start(p.plan.memory_stack_bytes).unwrap());
    for id in 0..case.units {
        create_unit(id);
    }
    // Names for the trace.
    fn addr<T>(x: &T) -> usize {
        x as *const T as usize
    }
    let mut n = |addr: usize, s: String| {
        st.names.insert(addr, s);
    };
    n(addr(&p.queued), "queued".into());
    n(addr(&p.searching), "searching".into());
    n(addr(&p.nsleep), "nsleep".into());
    n(addr(&super::LIVE), "live".into());
    n(addr(&p.stop), "stop".into());
    n(addr(&p.injector), "injector".into());
    n(addr(&p.idle), "idle list".into());
    for ix in 0..w {
        n(addr(&p.lifo[ix]), format!("private slot {ix}"));
        n(addr(&p.lifo_runs[ix]), format!("lifo_runs {ix}"));
        n(addr(&p.locals[ix]), format!("local queue {ix}"));
        n(addr(&p.handed[ix]), format!("handed {ix}"));
        n(addr(&p.slots[ix].timers), format!("slot {ix} timers"));
        n(addr(&p.slots[ix].next_timer), format!("slot {ix} timer"));
        n(addr(&p.slots[ix].last_poll), format!("slot {ix} last_poll"));
    }
    for id in 0..case.units {
        let u = unit_raw(id).unwrap();
        n(addr(p.units.slot(id).unwrap()), format!("unit table {id}"));
        n(addr(&u.state), format!("unit {id} state"));
        n(addr(&u.status), format!("unit {id} exit status"));
        n(addr(&u.xpending), format!("unit {id} xpending"));
        n(addr(&u.xready), format!("unit {id} arrivals"));
        n(addr(&u.inside), format!("unit {id} inside"));
    }
    for (i, f) in FLAG.iter().enumerate() {
        n(addr(f), format!("flag {i}"));
    }
    // Statistics counters and the untied switch are not protocol state.
    st.quiet
        .push((addr(&USTATS), addr(&USTATS) + size_of_val(&USTATS)));
    st.quiet.push((addr(&UNTIED_ON), addr(&UNTIED_ON) + 1));
    let base_threads = w + case.producers.len();
    let nthreads = base_threads;
    for i in 0..nthreads {
        let (name, producer) = if i < w {
            (format!("w{i}"), false)
        } else {
            (case.producers[i - w].0.to_string(), true)
        };
        st.threads.push(Th {
            name,
            wait: Wait::Run,
            os: None,
            producer,
            at: "",
            line: 0,
        });
    }
    if case.producers.is_empty() {
        st.quiet_from = Some(0);
    }
    let pct = match std::env::var("HIRSEL_CHECK_PCT").ok().as_deref() {
        Some("off") => None,
        Some(v) => v
            .parse::<u64>()
            .ok()
            .map(|d| (d, case.pct.map_or(20_000, |p| p.1))),
        None => case.pct,
    };
    if let Some((d, steps)) = pct {
        // PCT over the first `pct_until` steps, then uniform (fair) choice.
        st.pct_until = env("HIRSEL_CHECK_PCT_STEPS", steps);
        st.prio = (0..nthreads).map(|i| d + 1 + i as u64).collect();
        for i in (1..nthreads).rev() {
            let j = (st.rand() % (i as u64 + 1)) as usize;
            st.prio.swap(i, j);
        }
        let until = st.pct_until;
        st.changes = (0..d).map(|_| 1 + st.rand() % until).collect();
    }
    let _ = SCHED.set(Sched {
        st: std::sync::Mutex::new(st),
    });
    seam::SCHED_ON.store(true, Ordering::SeqCst);
    let mut handles = Vec::new();
    for i in 0..nthreads {
        let producer = (w..base_threads)
            .contains(&i)
            .then(|| case.producers[i - w].1);
        let h = std::thread::Builder::new()
            .name(format!("check-{i}"))
            .stack_size(1 << 20)
            .spawn(move || {
                ME.set(i);
                while lock().cur != i {
                    std::thread::park();
                }
                if let Some(f) = producer {
                    f();
                } else {
                    worker_main(pool(), i);
                }
                let mut st = lock();
                st.threads[i].wait = Wait::Done;
                let what = if st.threads[i].producer {
                    "outside producer done"
                } else {
                    "worker exits"
                };
                st.log(i, what.into());
                if st
                    .threads
                    .iter()
                    .all(|t| !t.producer || t.wait == Wait::Done)
                    && st.quiet_from.is_none()
                {
                    st.quiet_from = Some(st.step);
                }
                let next = st.choose();
                st.cur = next;
                let os = st.threads[next].os.clone().unwrap();
                drop(st);
                os.unpark();
            })
            .unwrap();
        handles.push(h.thread().clone());
    }
    {
        let mut st = lock();
        for (i, h) in handles.into_iter().enumerate() {
            st.threads[i].os = Some(h);
        }
        let first = st.choose();
        st.cur = first;
        st.threads[first].os.clone().unwrap().unpark();
    }
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
#[path = "units_check_tests.rs"]
mod tests;
