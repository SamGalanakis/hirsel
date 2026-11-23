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
//! `wake`, `notify_worker`, `add_timer`, `fire_timers`, `poll_due` and
//! `poll_global`, with units that switch onto their own stacks and call
//! `park`, `wake_id`, `maybe_yield` and `take_xready` as the task scheduler
//! does. A sync completion goes through its real cell (`os_sync`) and the
//! unit's waiter (`tasks::Readies`), whose state also passes the seam. Every
//! worker is a real OS thread, but only one runs at a time: each call
//! through the seam is a scheduling point where a seeded scheduler (uniform,
//! or PCT where a case asks for it) picks the next thread. A schedule is
//! therefore a function of the seed alone.
//!
//! Kernel and host model:
//! - Time is modelled: every scheduling point advances it by `DELTA_NS`
//!   (20 ns); when every thread waits, it jumps to the earliest timeout.
//! - The pool's wake eventfd is level-triggered: `epoll_wait` reports it
//!   while its counter is nonzero; a read takes the counter.
//! - A socket edge (edge-triggered) joins the ready list once and stays
//!   there until one `epoll_wait` reports it.
//! - `epoll_wait` returns at once if anything is ready, returns 0 for a
//!   zero timeout, and otherwise blocks until something is ready or modelled
//!   time reaches its timeout (milliseconds, as the pool passes it).
//! - `thread::park` has the standard token semantics and no spurious
//!   returns.
//! - The statistics, log and untied-switch objects are not scheduling
//!   points (they still block when held).
//! - A host write that never returns (stalled standard output) blocks for
//!   ever, after the pool records entry.
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
    BASE, Base, Memory, NONE, POOL, Pool, RUN, RunFn, TABLE, UNTIED_ON, USTATS, create_unit, cur,
    enable_pool, maybe_yield, park as unit_park, pool, take_xready, wake_id, worker_main,
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
    Park,
    ParkUntil(u64),
    Epoll(Option<u64>),
    Sleep(u64),
    Host,
    Done,
}

struct Th {
    name: String,
    wait: Wait,
    token: bool,
    os: Option<std::thread::Thread>,
    producer: bool,
    at: &'static str,
    line: u32,
}

struct Kernel {
    epfd: i32,
    wakefd: i32,
    counter: u64,
    edges: VecDeque<(u64, u32)>,
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
    slot_owners: Vec<Option<usize>>,
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
                Wait::Epoll(Some(d)) | Wait::Sleep(d) | Wait::ParkUntil(d) if d <= now => {
                    t.wait = Wait::Run
                }
                _ => {}
            }
        }
    }
    fn wake_epoll(&mut self) {
        for t in &mut self.threads {
            if let Wait::Epoll(_) = t.wait {
                t.wait = Wait::Run;
            }
        }
    }
    fn complete(&self) -> bool {
        (self.goals)()
            && (!self.case.starts_with("handoff_")
                || self.threads.iter().all(|t| t.wait == Wait::Done))
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
                    Wait::Epoll(Some(d)) | Wait::Sleep(d) | Wait::ParkUntil(d) => Some(d),
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
        let _ = writeln!(
            s,
            "kernel: eventfd counter {}, edges queued {:?}",
            self.kernel.counter, self.kernel.edges
        );
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
pub(super) fn slot_enter(ix: usize) {
    if let Some(id) = me() {
        let mut st = lock();
        if st.case.starts_with("handoff_") {
            assert_eq!(
                st.slot_owners[ix].replace(id),
                None,
                "two threads own slot {ix}"
            );
        }
    }
}

pub(super) fn slot_use(ix: usize) {
    if let Some(id) = me() {
        let st = lock();
        if st.case.starts_with("handoff_") {
            assert_eq!(st.slot_owners[ix], Some(id), "thread {id} lost slot {ix}");
        }
    }
}

pub(super) fn slot_taken(ix: usize) {
    if me().is_some() {
        let mut st = lock();
        assert!(
            st.slot_owners[ix].take().is_some(),
            "slot {ix} has no owner"
        );
    }
}

// Hold the queue lock across the transfer. Sleep on the model clock, so
// the monitor runs while the victim remains blocked.
pub(super) fn during_steal(ix: usize) {
    if me().is_some() && lock().case == "handoff_steal" && ix == 0 && !FLAG[3].raw() {
        FLAG[3].store(true, Ordering::SeqCst);
        while pool().host_calls[ix].0.raw() > super::HOST_ACTIVITY {
            sleep_producer(now() + 100);
        }
    }
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

pub(crate) mod seam {
    use super::{Wait, lock, me, note, point, quiet, shown};
    use std::panic::Location;
    use std::sync::atomic::Ordering;

    fn at_point(addr: usize, op: &'static str, at: &'static Location<'static>) {
        if let Some(m) = me() {
            let q = quiet(addr);
            super::LAST_QUIET.set(q);
            if !q {
                point(m, Wait::Run, |st| format!("{op} {}", st.name(addr)), at);
            }
        }
    }

    macro_rules! int_atomic {
        ($name:ident, $t:ty) => {
            #[repr(transparent)]
            pub(crate) struct $name(std::sync::atomic::$name);
            impl $name {
                pub(crate) const fn new(v: $t) -> Self {
                    Self(std::sync::atomic::$name::new(v))
                }
                fn a(&self) -> usize {
                    self as *const Self as usize
                }
                pub(crate) fn raw(&self) -> $t {
                    self.0.load(Ordering::SeqCst)
                }
                #[track_caller]
                pub(crate) fn load(&self, o: Ordering) -> $t {
                    at_point(self.a(), "load", Location::caller());
                    let v = self.0.load(o);
                    note(|| format!("{}", shown(v as u64)));
                    v
                }
                #[track_caller]
                pub(crate) fn store(&self, v: $t, o: Ordering) {
                    at_point(self.a(), "store", Location::caller());
                    self.0.store(v, o);
                    note(|| format!("{}", shown(v as u64)));
                }
                #[track_caller]
                pub(crate) fn swap(&self, v: $t, o: Ordering) -> $t {
                    at_point(self.a(), "swap", Location::caller());
                    let r = self.0.swap(v, o);
                    note(|| format!("{} => {}", shown(r as u64), shown(v as u64)));
                    r
                }
                #[track_caller]
                pub(crate) fn compare_exchange(
                    &self,
                    c: $t,
                    n: $t,
                    s: Ordering,
                    f: Ordering,
                ) -> Result<$t, $t> {
                    at_point(self.a(), "compare_exchange", Location::caller());
                    let r = self.0.compare_exchange(c, n, s, f);
                    note(|| format!("{c}=>{n}: {r:?}"));
                    r
                }
                #[track_caller]
                pub(crate) fn fetch_update<F: FnMut($t) -> Option<$t>>(
                    &self,
                    s: Ordering,
                    f: Ordering,
                    g: F,
                ) -> Result<$t, $t> {
                    at_point(self.a(), "fetch_update", Location::caller());
                    let r = self.0.fetch_update(s, f, g);
                    let now = self.0.load(Ordering::SeqCst);
                    note(|| format!("{r:?} => {now}"));
                    r
                }
                #[track_caller]
                pub(crate) fn try_update<F: FnMut($t) -> Option<$t>>(
                    &self,
                    s: Ordering,
                    f: Ordering,
                    g: F,
                ) -> Result<$t, $t> {
                    at_point(self.a(), "try_update", Location::caller());
                    let r = self.0.try_update(s, f, g);
                    let now = self.0.load(Ordering::SeqCst);
                    note(|| format!("{r:?} => {now}"));
                    r
                }
                #[track_caller]
                pub(crate) fn fetch_add(&self, v: $t, o: Ordering) -> $t {
                    at_point(self.a(), "fetch_add", Location::caller());
                    let r = self.0.fetch_add(v, o);
                    note(|| format!("{} +{}", shown(r as u64), shown(v as u64)));
                    r
                }
                #[track_caller]
                pub(crate) fn fetch_sub(&self, v: $t, o: Ordering) -> $t {
                    at_point(self.a(), "fetch_sub", Location::caller());
                    let r = self.0.fetch_sub(v, o);
                    note(|| format!("{} -{}", shown(r as u64), shown(v as u64)));
                    r
                }
                #[track_caller]
                pub(crate) fn fetch_min(&self, v: $t, o: Ordering) -> $t {
                    at_point(self.a(), "fetch_min", Location::caller());
                    let r = self.0.fetch_min(v, o);
                    note(|| format!("{} min {}", shown(r as u64), shown(v as u64)));
                    r
                }
                #[track_caller]
                pub(crate) fn fetch_max(&self, v: $t, o: Ordering) -> $t {
                    at_point(self.a(), "fetch_max", Location::caller());
                    let r = self.0.fetch_max(v, o);
                    note(|| format!("{} max {}", shown(r as u64), shown(v as u64)));
                    r
                }
                #[track_caller]
                pub(crate) fn fetch_or(&self, v: $t, o: Ordering) -> $t {
                    at_point(self.a(), "fetch_or", Location::caller());
                    let r = self.0.fetch_or(v, o);
                    note(|| format!("{} or {}", shown(r as u64), shown(v as u64)));
                    r
                }
            }
        };
    }
    int_atomic!(AtomicU8, u8);
    int_atomic!(AtomicU32, u32);
    int_atomic!(AtomicU64, u64);
    int_atomic!(AtomicUsize, usize);

    #[repr(transparent)]
    pub(crate) struct AtomicBool(std::sync::atomic::AtomicBool);
    impl AtomicBool {
        pub(crate) const fn new(v: bool) -> Self {
            Self(std::sync::atomic::AtomicBool::new(v))
        }
        fn a(&self) -> usize {
            self as *const Self as usize
        }
        pub(crate) fn raw(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        #[track_caller]
        pub(crate) fn load(&self, o: Ordering) -> bool {
            at_point(self.a(), "load", Location::caller());
            let v = self.0.load(o);
            note(|| format!("{v}"));
            v
        }
        #[track_caller]
        pub(crate) fn store(&self, v: bool, o: Ordering) {
            at_point(self.a(), "store", Location::caller());
            self.0.store(v, o);
            note(|| format!("{v}"));
        }
        #[track_caller]
        pub(crate) fn swap(&self, v: bool, o: Ordering) -> bool {
            at_point(self.a(), "swap", Location::caller());
            let r = self.0.swap(v, o);
            note(|| format!("{r} => {v}"));
            r
        }
    }

    #[repr(transparent)]
    pub(crate) struct AtomicPtr<T>(std::sync::atomic::AtomicPtr<T>);
    impl<T> AtomicPtr<T> {
        pub(crate) const fn new(v: *mut T) -> Self {
            Self(std::sync::atomic::AtomicPtr::new(v))
        }
        fn a(&self) -> usize {
            self as *const Self as usize
        }
        pub(crate) fn raw(&self) -> *mut T {
            self.0.load(Ordering::SeqCst)
        }
        #[track_caller]
        pub(crate) fn load(&self, o: Ordering) -> *mut T {
            at_point(self.a(), "load", Location::caller());
            let v = self.0.load(o);
            note(|| super::unit_name(v as usize));
            v
        }
        #[track_caller]
        pub(crate) fn store(&self, v: *mut T, o: Ordering) {
            at_point(self.a(), "store", Location::caller());
            self.0.store(v, o);
            note(|| super::unit_name(v.addr()));
        }
        #[track_caller]
        pub(crate) fn swap(&self, v: *mut T, o: Ordering) -> *mut T {
            at_point(self.a(), "swap", Location::caller());
            let r = self.0.swap(v, o);
            note(|| {
                format!(
                    "{} => {}",
                    super::unit_name(r as usize),
                    super::unit_name(v as usize)
                )
            });
            r
        }
        #[track_caller]
        pub(crate) fn compare_exchange(
            &self,
            c: *mut T,
            n: *mut T,
            s: Ordering,
            f: Ordering,
        ) -> Result<*mut T, *mut T> {
            at_point(self.a(), "compare_exchange", Location::caller());
            self.0.compare_exchange(c, n, s, f)
        }
    }

    /// A mutex whose lock is a scheduling point and whose contention blocks
    /// the thread in the checker, so a descheduled holder never spins the
    /// scheduler.
    pub(crate) struct Mutex<T> {
        inner: std::sync::Mutex<T>,
    }
    pub(crate) struct MutexGuard<'a, T> {
        g: std::mem::ManuallyDrop<std::sync::MutexGuard<'a, T>>,
        addr: usize,
    }
    #[derive(Debug)]
    pub(crate) struct WouldBlock;
    impl<T> Mutex<T> {
        pub(crate) const fn new(v: T) -> Self {
            Self {
                inner: std::sync::Mutex::new(v),
            }
        }
        fn a(&self) -> usize {
            self as *const Self as usize
        }
        fn guard(&self) -> Option<MutexGuard<'_, T>> {
            let g = match self.inner.try_lock() {
                Ok(g) => g,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return None,
            };
            Some(MutexGuard {
                g: std::mem::ManuallyDrop::new(g),
                addr: self.a(),
            })
        }
        /// The contents for a state dump, if nobody holds the lock.
        pub(crate) fn peek<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
            self.inner.try_lock().ok().map(|g| f(&g))
        }
        #[track_caller]
        pub(crate) fn lock(&self) -> Result<MutexGuard<'_, T>, WouldBlock> {
            let at = Location::caller();
            let Some(m) = me() else {
                let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                return Ok(MutexGuard {
                    g: std::mem::ManuallyDrop::new(g),
                    addr: self.a(),
                });
            };
            // A quiet lock is no scheduling point, but a held one still waits.
            if !quiet(self.a()) {
                point(m, Wait::Run, |st| format!("lock {}", st.name(self.a())), at);
            }
            loop {
                if let Some(g) = self.guard() {
                    return Ok(g);
                }
                point(
                    m,
                    Wait::Lock(self.a()),
                    |st| format!("lock {} (held: waits)", st.name(self.a())),
                    at,
                );
            }
        }
        #[track_caller]
        pub(crate) fn try_lock(&self) -> Result<MutexGuard<'_, T>, WouldBlock> {
            at_point(self.a(), "try_lock", Location::caller());
            let r = self.guard().ok_or(WouldBlock);
            note(|| {
                if r.is_ok() {
                    "taken".into()
                } else {
                    "held".into()
                }
            });
            r
        }
    }
    impl<T> Drop for MutexGuard<'_, T> {
        fn drop(&mut self) {
            // SAFETY: dropped once, here.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.g) };
            if SCHED_ON.load(Ordering::Relaxed) {
                let mut st = lock();
                for t in &mut st.threads {
                    if t.wait == Wait::Lock(self.addr) {
                        t.wait = Wait::Run;
                    }
                }
            }
        }
    }
    #[track_caller]
    pub(crate) fn read_arrival<T>(node: *const T) {
        at_point(node.addr(), "read arrival", Location::caller());
    }

    pub(super) static SCHED_ON: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    impl<T> std::ops::Deref for MutexGuard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.g
        }
    }
    impl<T> std::ops::DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.g
        }
    }

    pub(crate) mod thread {
        use super::super::{Wait, lock, me, point};
        use std::panic::Location;
        /// A worker's handle: the checker's thread index inside a run, the
        /// standard handle outside one.
        #[derive(Clone)]
        pub(crate) enum Thread {
            Checked(usize),
            Os(std::thread::Thread),
        }
        impl Thread {
            #[track_caller]
            pub(crate) fn unpark(&self) {
                let at = Location::caller();
                let (Some(m), Thread::Checked(t)) = (me(), self) else {
                    if let Thread::Os(t) = self {
                        t.unpark();
                    }
                    return;
                };
                let t = *t;
                point(
                    m,
                    Wait::Run,
                    |st| format!("unpark {}", st.threads[t].name),
                    at,
                );
                let mut st = lock();
                st.threads[t].token = true;
                if matches!(st.threads[t].wait, Wait::Park | Wait::ParkUntil(_)) {
                    st.threads[t].wait = Wait::Run;
                }
            }
        }
        pub(crate) fn current() -> Thread {
            match me() {
                Some(m) => Thread::Checked(m),
                None => Thread::Os(std::thread::current()),
            }
        }
        #[track_caller]
        pub(crate) fn park() {
            let at = Location::caller();
            let Some(m) = me() else {
                return std::thread::park();
            };
            point(m, Wait::Run, |_| "park".into(), at);
            let mut st = lock();
            if std::mem::take(&mut st.threads[m].token) {
                st.log(m, "park returns at once (token)".into());
                return;
            }
            drop(st);
            point(m, Wait::Park, |_| "park: sleeps on its handle".into(), at);
            lock().threads[m].token = false;
        }

        #[track_caller]
        pub(crate) fn park_timeout(duration: std::time::Duration) {
            let at = Location::caller();
            let Some(m) = me() else {
                return std::thread::park_timeout(duration);
            };
            point(m, Wait::Run, |_| "park with timeout".into(), at);
            let mut st = lock();
            if std::mem::take(&mut st.threads[m].token) {
                return;
            }
            let at_ns = st.time + duration.as_nanos() as u64;
            drop(st);
            point(
                m,
                Wait::ParkUntil(at_ns),
                |_| "park until timeout".into(),
                at,
            );
            lock().threads[m].token = false;
        }
    }

    #[track_caller]
    pub(crate) fn monotonic_ns() -> u64 {
        if SCHED.get().is_none() {
            return crate::os::monotonic_ns();
        }
        if let Some(m) = me() {
            point(m, Wait::Run, |_| "clock".into(), Location::caller());
        }
        lock().time
    }
    use super::SCHED;

    /// # Safety
    /// `evs` has room for `max` events.
    #[track_caller]
    pub(crate) unsafe fn epoll_wait(
        epfd: i32,
        evs: *mut libc::epoll_event,
        max: i32,
        timeout: i32,
    ) -> i32 {
        let at = Location::caller();
        let Some(m) = me() else {
            // SAFETY: the caller's contract.
            return unsafe { libc::epoll_wait(epfd, evs, max, timeout) };
        };
        point(
            m,
            Wait::Run,
            |_| format!("epoll_wait timeout {timeout}ms"),
            at,
        );
        let mut deadline = None;
        loop {
            let mut st = lock();
            assert_eq!(
                epfd, st.kernel.epfd,
                "epoll_wait on an unmodelled descriptor"
            );
            let mut n = 0usize;
            let mut got = Vec::new();
            if st.kernel.counter > 0 && n < max as usize {
                // SAFETY: n < max.
                unsafe {
                    *evs.add(n) = libc::epoll_event {
                        events: libc::EPOLLIN as u32,
                        u64: super::super::WAKE_TAG,
                    }
                };
                n += 1;
                got.push("wake eventfd".to_string());
            }
            while n < max as usize
                && let Some((tag, ev)) = st.kernel.edges.pop_front()
            {
                // SAFETY: n < max.
                unsafe {
                    *evs.add(n) = libc::epoll_event {
                        events: ev,
                        u64: tag,
                    }
                };
                n += 1;
                got.push(format!(
                    "edge unit {} key {}",
                    tag >> hirsel_core_net::SHARED_UNIT_SHIFT,
                    tag & 0xffff_ffff
                ));
            }
            if n > 0 {
                st.log(m, format!("epoll_wait returns {n}: {}", got.join(", ")));
                return n as i32;
            }
            if timeout == 0 {
                st.log(m, "epoll_wait returns 0".into());
                return 0;
            }
            if deadline.is_none() && timeout > 0 {
                deadline = Some(st.time + timeout as u64 * 1_000_000);
            }
            if let Some(d) = deadline
                && st.time >= d
            {
                st.log(m, "epoll_wait times out".into());
                return 0;
            }
            drop(st);
            point(
                m,
                Wait::Epoll(deadline),
                |_| {
                    format!(
                        "epoll_wait blocks (deadline {:?})",
                        deadline.map(|d| (d - super::t0()) / 1000)
                    )
                },
                at,
            );
        }
    }
    /// # Safety
    /// `buf` has eight writable bytes.
    #[track_caller]
    pub(crate) unsafe fn read(fd: i32, buf: *mut libc::c_void, n: usize) -> isize {
        let at = Location::caller();
        let Some(m) = me() else {
            // SAFETY: the caller's contract.
            return unsafe { libc::read(fd, buf, n) };
        };
        point(m, Wait::Run, |_| "read wake eventfd".into(), at);
        let mut st = lock();
        assert!(
            fd == st.kernel.wakefd && n == 8,
            "read on an unmodelled descriptor"
        );
        let c = std::mem::take(&mut st.kernel.counter);
        if c == 0 {
            return -1;
        }
        // SAFETY: eight writable bytes.
        unsafe { *buf.cast::<u64>() = c };
        8
    }
    /// # Safety
    /// `buf` has eight readable bytes.
    #[track_caller]
    pub(crate) unsafe fn write(fd: i32, buf: *const libc::c_void, n: usize) -> isize {
        let at = Location::caller();
        let Some(m) = me() else {
            // SAFETY: the caller's contract.
            return unsafe { libc::write(fd, buf, n) };
        };
        point(m, Wait::Run, |_| "write wake eventfd".into(), at);
        let mut st = lock();
        assert!(
            fd == st.kernel.wakefd && n == 8,
            "write on an unmodelled descriptor"
        );
        // SAFETY: eight readable bytes.
        st.kernel.counter += unsafe { *buf.cast::<u64>() };
        st.wake_epoll();
        8
    }
}

/// A unit record read without a scheduling point, for the checker's own
/// trace and dumps (which run while the scheduler lock is held).
fn unit_raw(id: u32) -> Option<&'static super::Unit> {
    // SAFETY: unit records are leaked; a published pointer stays valid.
    unsafe { super::TABLE.get(id as usize)?.raw().as_ref() }
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
        "pool: queued {} searching {} nsleep {} poller_blocked {} poll_until {} next_timer {} live {} stop {}",
        p.queued.raw(),
        p.searching.raw(),
        p.nsleep.raw(),
        p.poller_blocked.raw(),
        rel(p.poll_until.raw()),
        rel(p.next_timer.raw()),
        super::LIVE.raw(),
        p.stop.raw()
    );
    for ix in 0..p.w {
        let _ = writeln!(
            s,
            "  worker {ix}: private slot {}, local queue {:?}, handed {}",
            unit_name(p.lifo[ix].raw() as usize),
            p.locals[ix].peek(ids),
            p.handed[ix].raw()
        );
    }
    let _ = writeln!(
        s,
        "  injector {:?}, idle {:?}",
        p.injector.peek(ids),
        p.idle.peek(|v| v.clone())
    );
    let _ = writeln!(
        s,
        "  timers {:?}",
        p.timers
            .peek(|h| h.iter().map(|r| (rel(r.0), r.1)).collect::<Vec<_>>())
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
/// The task scheduler's wait for readiness on one of its keys.
fn wait_arrival(ident: u64) {
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
    let _blocking = super::blocking_file();
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
/// A socket edge for unit `unit`'s key `ident` reaches the shared epoll set.
fn edge(unit: u32, ident: u64) {
    say(format!(
        "a client's bytes make unit {unit} key {ident} readable"
    ));
    let mut st = lock();
    st.kernel.edges.push_back((
        (u64::from(unit) << hirsel_core_net::SHARED_UNIT_SHIFT) | ident,
        libc::EPOLLIN as u32,
    ));
    st.wake_epoll();
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

/// The requeue case's sync: its ready token and its completion cell (handed
/// to the helper; not a pool object, so not a scheduling point).
const SYNC_TOKEN: u64 = 0x5ec0;
static SYNC_CELL: std::sync::atomic::AtomicPtr<crate::os_sync::SyncCell> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn host_for(delay: u64) {
    let _blocking = super::blocking_file();
    sleep_producer(now() + delay);
}

fn expect_arrivals(expected: &[u64]) {
    let mut ids = Vec::new();
    take_xready(|id, _| ids.push(id));
    assert_eq!(ids, expected);
    let mut stream = conn::OBS[this_unit() as usize].lock().unwrap();
    stream.extend(ids.iter().map(|id| format!("I arrival {id}")));
    stream.push("O consumed once in order".into());
}

fn host_until(flag: usize) {
    let _blocking = super::blocking_file();
    FLAG[2].store(true, Ordering::SeqCst);
    producer_wait_flag(flag);
}

const HOST_THRESHOLD: u64 = 2_000;

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
    // 2. Nobody in the poller (lens-sleep/VERIFY.md M1). Unit 0 computes,
    // wakes unit 1 and ends; unit 1 then computes for ever (a long CPU-bound
    // turn). Unit 2 computes for a while, then waits for a client's bytes,
    // which arrive from outside later. Both workers run for over 100 us
    // without polling, so the end of unit 0's turn makes a due probe of the
    // epoll set while the other worker is about to go idle.
    Case {
        name: "nobody_in_poller",
        workers: 2,
        units: 3,
        body: |id| {
            match id {
                0 => {
                    compute_until(t0() + param(0));
                    FLAG[0].store(true, Ordering::SeqCst);
                    wake_id(1);
                    compute_until(now() + param(1));
                }
                1 => {
                    wait_flag(0);
                    compute_forever();
                }
                _ => {
                    compute_until(t0() + param(3));
                    wait_arrival(5);
                    served(0, "unit 2 read its client's bytes");
                }
            }
            0
        },
        producers: &[("client", || {
            sleep_producer(t0() + param(2));
            edge(2, 5);
        })],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(100_000, 160_000), Ordering::Relaxed); // unit 0 computes before the wake
            PARAM[1].store(r(0, 5_000), Ordering::Relaxed); // and after it
            PARAM[2].store(r(300_000, 600_000), Ordering::Relaxed); // the client sends
            PARAM[3].store(r(100_000, 160_000), Ordering::Relaxed); // unit 2 computes first
        },
        // The race needs the probing worker preempted for microseconds while
        // it holds the poll turn; uniform choice almost never does that (0
        // of 400), PCT at depth 8 over the first 9000 steps does (3 of 3000).
        pct: Some((8, 9_000)),
    },
    // 3. Requeue without notification (lens-sleep/VERIFY.md M4 variant 2).
    // Unit 0 wakes unit 1, submits a file sync to a helper thread (not a
    // worker) and waits for it; the helper finishes a few microseconds later
    // and completes the sync through the real completion path (the sync's
    // cell, the unit's waiter, the pool wake). Unit 1 blocks in a host write
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
                    // Submit, as os_sync::submit does: a completion cell
                    // holding the unit's outside-work count.
                    let readies = crate::tasks::Readies::for_unit(0);
                    let outside = crate::tasks::Outside::for_readies(readies.clone());
                    let cell = crate::os_sync::SyncCell::for_check(SYNC_TOKEN, outside);
                    name_object(std::sync::Arc::as_ptr(&readies), "unit 0 waiter");
                    name_object(cell, "sync cell");
                    SYNC_CELL.store(cell, Ordering::SeqCst);
                    FLAG[2].store(true, Ordering::SeqCst);
                    // Wait as the task scheduler's block does: take the
                    // ready, else park; then take the result from the cell.
                    while !readies.take_for_test(SYNC_TOKEN) {
                        unit_park(None);
                    }
                    // SAFETY: the issuer's handle, taken once after its ready.
                    if unsafe { crate::os_sync::SyncCell::take_for_check(cell) } == Some(0) {
                        served(0, "unit 0 ran after its sync completed");
                    }
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
            // SAFETY: the helper's handle on the cell unit 0 submitted.
            unsafe {
                crate::os_sync::SyncCell::complete_for_check(SYNC_CELL.load(Ordering::SeqCst), 0)
            };
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
    // 3b. Quiescence after a sync (sim-idle-hang-1854). Unit 0 submits a
    // sync, takes its ready and its result, then decides quiescence as the
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
            let cell = crate::os_sync::SyncCell::for_check(SYNC_TOKEN, outside);
            name_object(std::sync::Arc::as_ptr(&readies), "unit 0 waiter");
            name_object(cell, "sync cell");
            SYNC_CELL.store(cell, Ordering::SeqCst);
            FLAG[2].store(true, Ordering::SeqCst);
            while !readies.take_for_test(SYNC_TOKEN) {
                unit_park(None);
            }
            // SAFETY: the issuer's handle, taken once after its ready.
            if unsafe { crate::os_sync::SyncCell::take_for_check(cell) } != Some(0) {
                return 0;
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
            // SAFETY: the helper's handle on the cell unit 0 submitted.
            unsafe {
                crate::os_sync::SyncCell::complete_for_check(SYNC_CELL.load(Ordering::SeqCst), 0)
            };
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
    Case {
        name: "handoff_return",
        workers: 1,
        units: 2,
        body: |id| {
            if id == 1 {
                wait_flag(0);
                return 0;
            }
            host_for(param(0));
            host_for(param(0));
            maybe_yield();
            FLAG[0].store(true, Ordering::SeqCst);
            wake_id(1);
            served(0, "unit returns from its host call and keeps running once");
            0
        },
        producers: &[],
        goals: 0b1,
        params: |r| PARAM[0].store(r(1_000, 5_000), Ordering::Relaxed),
        pct: None,
    },
    Case {
        name: "handoff_steal",
        workers: 2,
        units: 3,
        body: |id| {
            match id {
                0 => {
                    wait_flag(0);
                    FLAG[1].store(true, Ordering::SeqCst);
                    wake_id(1);
                    wake_id(2);
                    {
                        let _blocking = super::blocking_file();
                        FLAG[2].store(true, Ordering::SeqCst);
                        while !GOAL[1].load(Ordering::Relaxed) {
                            sleep_producer(now() + 300);
                        }
                    }
                    maybe_yield();
                }
                1 => wait_flag(1),
                _ => {
                    FLAG[0].store(true, Ordering::SeqCst);
                    wake_id(0);
                    wait_flag(1);
                }
            }
            served(id as usize, "unit completed its turn");
            0
        },
        producers: &[],
        goals: 0b111,
        params: |_| {},
        pct: None,
    },
    Case {
        name: "handoff_private",
        workers: 1,
        units: 2,
        body: |id| {
            if id == 0 {
                wait_flag(0);
                FLAG[1].store(true, Ordering::SeqCst);
                wake_id(1);
                host_until(3);
                maybe_yield();
            } else {
                FLAG[0].store(true, Ordering::SeqCst);
                wake_id(0);
                wait_flag(1);
                FLAG[3].store(true, Ordering::SeqCst);
            }
            served(id as usize, "unit completed its turn");
            0
        },
        producers: &[],
        goals: 0b11,
        params: |_| {},
        pct: None,
    },
    Case {
        name: "handoff_wake",
        workers: 1,
        units: 2,
        body: |id| {
            if id == 1 {
                wait_flag(0);
                return 0;
            }
            host_until(3);
            // The wake was published while this unit was still running.
            unit_park(None);
            expect_arrivals(&[10, 11]);
            FLAG[0].store(true, Ordering::SeqCst);
            wake_id(1);
            served(0, "unit received both arrivals after its host call");
            0
        },
        producers: &[("client", || {
            producer_wait_flag(2);
            sleep_producer(now() + 4 * HOST_THRESHOLD);
            let unit = unit_raw(0).unwrap();
            super::push_arrival(unit, 10, 0);
            super::push_arrival(unit, 11, 0);
            super::wake(unit);
            FLAG[3].store(true, Ordering::SeqCst);
        })],
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
    // 17. The same client and law across the blocking-call hand-off: the
    // unit moves to the other OS thread while the client is parked, and the
    // client resumes there (`units_returnable_check.rs`).
    Case {
        name: "handoff_returnable",
        workers: 1,
        units: 2,
        body: |id| {
            if id == 1 {
                wait_flag(0);
                return 0;
            }
            // A second unit gives the monitor a reason to hand the slot off.
            let status = returnable::handoff_body(id);
            wake_id(1);
            status
        },
        producers: &[("provider", returnable::producer)],
        goals: 0b1,
        params: |r| PARAM[0].store(r(3_000, 6_000), Ordering::Relaxed),
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
    // 21. The same, with the blocking-call hand-off at one of the waits whose
    // wake comes from the helper: the client resumes on another thread.
    Case {
        name: "handoff_park_waits",
        workers: 2,
        units: 2,
        body: |id| {
            if id == 1 {
                wait_flag(0);
                return 0;
            }
            let status = park_waits::handoff_body(id);
            wake_id(1);
            status
        },
        producers: &[],
        goals: 0b1,
        params: |r| {
            PARAM[0].store(r(3_000, 6_000), Ordering::Relaxed);
            PARAM[1].store(r(0, 3), Ordering::Relaxed);
        },
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
    let shared = hirsel_core_net::share_epoll_poller(super::shared_unit).unwrap();
    let (gep, gwake) = (shared.raw_fd(), shared.wake_fd());
    // Setup stamps (unit creation) use the real clock; start the model 10 s
    // ahead of it so no setup stamp is later than modelled time.
    T0V.store(
        crate::os::monotonic_ns() + 10_000_000_000,
        Ordering::Relaxed,
    );
    let w = case.workers;
    let handoff = case.name.starts_with("handoff_");
    let steal = case.name == "handoff_steal";
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
            epfd: gep,
            wakefd: gwake,
            counter: 0,
            edges: VecDeque::new(),
        },
        quiet_from: None,
        bound: env("HIRSEL_CHECK_BOUND", 100_000),
        max_steps: env("HIRSEL_CHECK_MAX_STEPS", 3_000_000),
        goals: goals_met,
        out: out.to_string(),
        case: case.name,
        slot_owners: vec![None; w],
        seed,
        prio: Vec::new(),
        changes: Vec::new(),
        pct_until: 0,
    };
    // Parameters come from the seed's stream before any scheduling choice.
    (case.params)(&mut |lo, hi| lo + st.rand() % (hi - lo + 1));
    NEEDED.store(case.goals, Ordering::Relaxed);
    let spare = handoff.then(|| std::sync::Arc::new(super::Spare::new()));
    super::LIVE.store(case.units as usize, Ordering::SeqCst);
    let _ = POOL.set(Pool {
        w,
        locals: (0..w).map(|_| super::Mutex::new(VecDeque::new())).collect(),
        lifo: (0..w)
            .map(|_| super::AtomicPtr::new(std::ptr::null_mut()))
            .collect(),
        lifo_runs: (0..w).map(|_| super::AtomicU32::new(0)).collect(),
        lifo_on: true,
        injector: super::Mutex::new(VecDeque::new()),
        memory: Memory::new(),
        queued: super::AtomicUsize::new(0),
        searching: super::AtomicUsize::new(0),
        idle: super::Mutex::new(Vec::with_capacity(w)),
        nsleep: super::AtomicUsize::new(0),
        threads: (0..w).map(|_| std::sync::Mutex::new(None)).collect(),
        handoff_ticks: if handoff { 4 } else { 0 },
        handoff_tick_ns: HOST_THRESHOLD / 4,
        host_calls: (0..w)
            .map(|_| super::HostCall(super::AtomicU64::new(0)))
            .collect(),
        spares: super::Mutex::new(spare.iter().cloned().collect()),
        monitor: std::sync::OnceLock::new(),
        handed: (0..w).map(|_| super::AtomicBool::new(false)).collect(),
        poll: super::Mutex::new(()),
        poller_blocked: super::AtomicBool::new(false),
        poll_until: super::AtomicU64::new(u64::MAX),
        poll_held: super::AtomicBool::new(false),
        gep,
        gwake,
        uring: false,
        _shared: shared,
        net_used: super::AtomicBool::new(true),
        timers: super::Mutex::new(std::collections::BTreeSet::new()),
        next_timer: super::AtomicU64::new(u64::MAX),
        stop: super::AtomicBool::new(false),
        last_poll: super::AtomicU64::new(0),
        spin_ns: 5_000,
        connection_spins: 8,
        spin_poll_ns: 20_000,
        slice_ns: 500_000,
        poll_every_ns: 100_000,
    });
    static BODY: std::sync::OnceLock<fn(u32) -> u8> = std::sync::OnceLock::new();
    let _ = BODY.set(case.body);
    static DISPATCH: fn(&crate::entry::OsConfig) -> u8 = |_| (BODY.get().unwrap())(this_unit());
    let _ = RUN.set(RunFn(&DISPATCH));
    // SAFETY: an all-zero configuration: no arguments, grants or operations.
    let _ = BASE.set(Base(unsafe { std::mem::zeroed() }));
    enable_pool();
    let p = pool();
    std::mem::forget(p.memory.start());
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
    n(addr(&p.poller_blocked), "poller_blocked".into());
    n(addr(&p.poll_until), "poll_until".into());
    n(addr(&p.next_timer), "next_timer".into());
    n(addr(&p.last_poll), "last_poll".into());
    n(addr(&super::LIVE), "live".into());
    n(addr(&super::HOST_TICK.0), "host clock tick".into());
    n(
        addr(&super::HOST_MONITOR_PARKED),
        "host monitor parked".into(),
    );
    n(addr(&p.stop), "stop".into());
    n(addr(&p.injector), "injector".into());
    n(addr(&p.idle), "idle list".into());
    n(addr(&p.poll), "poll turn".into());
    n(addr(&p.timers), "timer heap".into());
    for ix in 0..w {
        n(addr(&p.lifo[ix]), format!("private slot {ix}"));
        n(addr(&p.lifo_runs[ix]), format!("lifo_runs {ix}"));
        n(addr(&p.locals[ix]), format!("local queue {ix}"));
        n(addr(&p.handed[ix]), format!("handed {ix}"));
        n(addr(&p.host_calls[ix].0), format!("slot {ix} host call"));
    }
    for id in 0..case.units {
        let u = unit_raw(id).unwrap();
        n(addr(&TABLE[id as usize]), format!("unit table {id}"));
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
    let nthreads = base_threads + if handoff { 2 } else { 0 };
    for i in 0..nthreads {
        let (name, producer) = if i < w {
            (format!("w{i}"), false)
        } else if i < base_threads {
            (case.producers[i - w].0.to_string(), true)
        } else if i == base_threads {
            ("spare".into(), false)
        } else {
            ("monitor".into(), false)
        };
        st.threads.push(Th {
            name,
            wait: Wait::Run,
            token: false,
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
        let spare = spare.clone();
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
                } else if i < w {
                    if steal && i == 1 {
                        producer_wait_flag(2);
                    }
                    worker_main(pool(), Some(i), None);
                } else if i == base_threads {
                    worker_main(pool(), None, spare);
                } else {
                    super::watch_blocking(pool(), |_| false);
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
mod tests {
    use super::{CASES, run_one};
    use std::process::Command;

    /// Run `case` over seeds in child processes (one schedule each, on the
    /// CPUs this process may use, two at a time). Returns the first failing
    /// seed and its report, and the number of schedules run.
    fn search(case: &str) -> (Option<(u64, String)>, u64) {
        let env = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let first = env("HIRSEL_CHECK_FROM", 0);
        let count = env("HIRSEL_CHECK_SEEDS", 2000);
        let jobs = env("HIRSEL_CHECK_JOBS", 2) as usize;
        // HIRSEL_CHECK_ALL: run every seed and list each failing one (for
        // measuring how often an unaided search hits a case).
        let all = std::env::var_os("HIRSEL_CHECK_ALL").is_some();
        let mut fails = Vec::new();
        let mut inconclusive = 0u64;
        let cpus = {
            // SAFETY: zeroed is the empty set; the call fills it.
            let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) };
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
                .collect::<Vec<_>>()
        };
        let dir = std::env::temp_dir().join(format!("hirsel-units-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().unwrap();
        let test = format!("units::check::tests::{case}");
        let mut seed = first;
        let mut ran = 0;
        let mut running: Vec<(u64, std::process::Child, std::path::PathBuf)> = Vec::new();
        let mut found = None;
        let mut law = super::conn::Law::default();
        while (seed < first + count && (all || found.is_none())) || !running.is_empty() {
            while running.len() < jobs && seed < first + count && (all || found.is_none()) {
                let out = dir.join(format!("{case}-{seed}.txt"));
                let cpu = cpus[running.len() % cpus.len()];
                let child = Command::new(&exe)
                    .env("HIRSEL_CHECK_CPU", cpu.to_string())
                    .args(["--exact", &test, "--nocapture", "--test-threads=1", "-q"])
                    .env("HIRSEL_CHECK_CHILD", case)
                    .env("HIRSEL_CHECK_SEED", seed.to_string())
                    .env("HIRSEL_CHECK_OUT", &out)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                running.push((seed, child, out));
                seed += 1;
            }
            let (s, mut child, out) = running.remove(0);
            let status = child.wait().unwrap();
            ran += 1;
            let text = std::fs::read_to_string(&out).unwrap_or_default();
            let _ = std::fs::remove_file(&out);
            match status.code() {
                Some(0) => {
                    if let Some(v) = law.add(s, &text) {
                        fails.push(s);
                        if found.as_ref().is_none_or(|(f, _): &(u64, String)| s < *f) {
                            found = Some((s, format!("FAIL determinism: {v}\n")));
                        }
                    }
                }
                Some(2) => {
                    inconclusive += 1;
                    eprintln!("{case} seed {s}: inconclusive");
                }
                code => {
                    fails.push(s);
                    if all {
                        eprintln!(
                            "{case} seed {s}: fails (exit {code:?}): {}",
                            text.lines().next().unwrap_or("")
                        );
                    }
                    if found.as_ref().is_none_or(|(f, _): &(u64, String)| s < *f) {
                        found = Some((s, text));
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        if all {
            fails.sort();
            eprintln!(
                "{case}: {} of {ran} schedules fail, {inconclusive} inconclusive; failing seeds {fails:?}",
                fails.len()
            );
        }
        (found, ran)
    }

    fn check(case: &str) {
        if std::env::var("HIRSEL_CHECK_CHILD").as_deref() == Ok(case) {
            if let Some(cpu) = std::env::var("HIRSEL_CHECK_CPU")
                .ok()
                .and_then(|c| c.parse::<usize>().ok())
            {
                // SAFETY: a one-CPU set for this process's main thread; the
                // checker's threads inherit it.
                unsafe {
                    let mut set: libc::cpu_set_t = std::mem::zeroed();
                    libc::CPU_SET(cpu, &mut set);
                    libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set);
                }
            }
            let seed = std::env::var("HIRSEL_CHECK_SEED").unwrap().parse().unwrap();
            let out = std::env::var("HIRSEL_CHECK_OUT").unwrap();
            run_one(CASES.iter().find(|c| c.name == case).unwrap(), seed, &out);
        }
        let t = std::time::Instant::now();
        let (found, ran) = search(case);
        let secs = t.elapsed().as_secs_f64();
        if let Some((seed, report)) = found {
            panic!(
                "{case}: law violated at seed {seed} (schedule {ran} of the search, {secs:.1}s)\n{report}"
            );
        }
        eprintln!("{case}: {ran} schedules hold the law ({secs:.1}s)");
    }

    #[test]
    fn stale_sleep_bound() {
        check("stale_sleep_bound");
    }
    #[test]
    fn nobody_in_poller() {
        check("nobody_in_poller");
    }
    #[test]
    fn requeue_without_notification() {
        check("requeue_without_notification");
    }
    #[test]
    fn outside_end_wakes_waiter() {
        check("outside_end_wakes_waiter");
    }
    #[test]
    fn injector_starved() {
        check("injector_starved");
    }
    #[test]
    fn connection_spin_yields_to_work() {
        check("connection_spin_yields_to_work");
    }
    #[test]
    fn handoff_return() {
        check("handoff_return");
    }
    #[test]
    fn handoff_steal() {
        check("handoff_steal");
    }
    #[test]
    fn handoff_private() {
        check("handoff_private");
    }
    #[test]
    fn handoff_wake() {
        check("handoff_wake");
    }
    #[test]
    fn conn_wait_serve() {
        check("conn_wait_serve");
    }
    #[test]
    fn conn_seal_in_flight() {
        check("conn_seal_in_flight");
    }
    #[test]
    fn conn_helper_forward() {
        check("conn_helper_forward");
    }
    #[test]
    fn conn_release_race() {
        check("conn_release_race");
    }
    #[test]
    fn conn_slot_reuse() {
        check("conn_slot_reuse");
    }
    #[test]
    fn returnable_stack_reuse() {
        check("returnable_stack_reuse");
    }
    #[test]
    fn handoff_returnable() {
        check("handoff_returnable");
    }
    #[test]
    fn returnable_receive() {
        check("returnable_receive");
    }
    #[test]
    fn returnable_transfer() {
        check("returnable_transfer");
    }
    #[test]
    fn park_waits() {
        check("park_waits");
    }
    #[test]
    fn handoff_park_waits() {
        check("handoff_park_waits");
    }

    #[test]
    fn conn_flat_slot_reuse() {
        check("conn_flat_slot_reuse");
    }
}
