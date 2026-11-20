//! Commit ownership. The residual domain has one token, passed FIFO within
//! an L3 cohort for a bounded run before a waiting remote cohort is served
//! (lock cohorting: Dice, Marathe and Shavit, C-BO-MCS shape, with MCS at
//! both levels). The tape records the actual commit order, so the grant
//! order is not part of the recorded schedule.
use std::{
    cell::Cell,
    ptr,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering},
    },
};

/// Commits granted inside one cohort while another cohort waits.
const COHORT_PASSES: u32 = 64;
/// Delegated requests a releasing holder serves before handing off.
const SERVE_BATCH: usize = 8;

/// A commit another worker may perform for a waiting requester (typed
/// delegation, H-Synch shape). `serve` runs under the token on the holder's
/// worker; it either completes the request and returns true, or returns
/// false before any mutation, and the requester is granted the token.
#[repr(C)]
pub(crate) struct Request {
    pub(crate) serve: unsafe fn(*mut Request) -> bool,
}
const COHORTS: usize = 8;

#[derive(Default)]
pub(crate) struct Sequencer {
    global: Mcs,
    cohorts: [Cohort; COHORTS],
    lobby: Lobby,
    home: OnceLock<Option<Home>>,
}

/// Requesters beyond this many queue in the lobby, parked, instead of
/// spinning in the token queue (capping active spinners: Dice's Malthusian
/// locks, CNA). A token queue longer than this only adds spinning threads
/// that preempt the holder; it cannot raise the serial commit rate.
const ACTIVE: u32 = 3;
/// Reads of the admitted count a full lobby makes before it parks.
const LOBBY_SPINS: u32 = 512;

#[repr(align(64))]
#[derive(Default)]
struct Lobby {
    /// Requesters admitted to the token queue, the holder included.
    active: AtomicU32,
    /// Bumped on each departure; parked requesters wait on it.
    turn: AtomicU32,
    parked: AtomicU32,
}

impl Lobby {
    fn enter(&self) {
        let mut spins = 0;
        loop {
            let active = self.active.load(Ordering::Relaxed);
            if active >= ACTIVE && spins < LOBBY_SPINS {
                spins += 1;
                std::hint::spin_loop();
                continue;
            }
            if active < ACTIVE {
                if self
                    .active
                    .compare_exchange_weak(active, active + 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            let turn = self.turn.load(Ordering::Acquire);
            self.parked.fetch_add(1, Ordering::SeqCst);
            if self.active.load(Ordering::SeqCst) >= ACTIVE {
                #[cfg(feature = "testing")]
                PARKS[1].fetch_add(1, Ordering::Relaxed);
                futex_wait(&self.turn, turn);
            }
            self.parked.fetch_sub(1, Ordering::Relaxed);
            spins = 0;
        }
    }

    fn leave(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        if self.parked.load(Ordering::SeqCst) != 0 {
            self.turn.fetch_add(1, Ordering::Release);
            futex_wake(&self.turn);
        }
    }
}

#[repr(align(64))]
#[derive(Default)]
struct Cohort {
    local: Mcs,
    /// The cohort's place in the global queue, used only by its local holder.
    node: Node,
    /// Local grants since the token entered the cohort; holder-only.
    passes: AtomicU32,
}

#[derive(Default)]
struct Mcs {
    tail: AtomicPtr<Node>,
}

#[repr(align(64))]
#[derive(Default)]
struct Node {
    next: AtomicPtr<Node>,
    /// GRANTED, SPINNING or PARKED; the waiter parks on this word.
    waiting: AtomicU32,
    /// The predecessor passed the global token with the local one.
    inherited: AtomicBool,
    /// The cohort of the worker that granted this node the token.
    from: AtomicU32,
    /// A delegable request, published before the node is enqueued.
    request: AtomicPtr<Request>,
    /// The holder completed the request instead of granting the token.
    served: AtomicBool,
    #[cfg(feature = "testing")]
    released_at: std::sync::atomic::AtomicU64,
}

impl Mcs {
    /// # Safety
    /// Node stays pinned and belongs exclusively to this requester until it
    /// releases.
    unsafe fn acquire(&self, node: *mut Node) -> bool {
        // SAFETY: each requester owns its node. The tail keeps the predecessor
        // pinned until the successor publishes its link.
        unsafe {
            (*node).next.store(ptr::null_mut(), Ordering::Relaxed);
            (*node).waiting.store(SPINNING, Ordering::Relaxed);
            (*node).inherited.store(false, Ordering::Relaxed);
            (*node).served.store(false, Ordering::Relaxed);
            let predecessor = self.tail.swap(node, Ordering::AcqRel);
            if !predecessor.is_null() {
                (*predecessor).next.store(node, Ordering::Release);
                let mut spins = 0u32;
                while (*node).waiting.load(Ordering::Acquire) != GRANTED {
                    if spins < WAIT_SPINS {
                        spins += 1;
                        std::hint::spin_loop();
                    } else if (*node)
                        .waiting
                        .compare_exchange(SPINNING, PARKED, Ordering::Acquire, Ordering::Acquire)
                        .is_ok()
                        || (*node).waiting.load(Ordering::Acquire) == PARKED
                    {
                        #[cfg(feature = "testing")]
                        PARKS[0].fetch_add(1, Ordering::Relaxed);
                        futex_wait(&(*node).waiting, PARKED);
                        spins = 0;
                    }
                }
            }
            !predecessor.is_null()
        }
    }

    /// # Safety
    /// Node is the current holder's node.
    unsafe fn has_waiter(&self, node: *mut Node) -> bool {
        // SAFETY: the holder pins its node.
        unsafe {
            !(*node).next.load(Ordering::Acquire).is_null()
                || self.tail.load(Ordering::Acquire) != node
        }
    }

    /// # Safety
    /// Node is the current holder's node and was acquired exactly once.
    unsafe fn release(&self, node: *mut Node, inherited: bool, from: usize, _measured: bool) {
        // SAFETY: the holder pins the node. Once linked, a successor owns
        // its own node until it has taken and then released the token.
        unsafe {
            let mut next = (*node).next.load(Ordering::Acquire);
            if next.is_null() {
                if self
                    .tail
                    .compare_exchange(node, ptr::null_mut(), Ordering::Release, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
                let mut spins = 0u32;
                loop {
                    next = (*node).next.load(Ordering::Acquire);
                    if !next.is_null() {
                        break;
                    }
                    backoff(&mut spins);
                }
            }
            #[cfg(feature = "testing")]
            if _measured {
                (*next)
                    .released_at
                    .store(crate::os::monotonic_ns(), Ordering::Relaxed);
            }
            (*next).inherited.store(inherited, Ordering::Relaxed);
            (*next).from.store(from as u32, Ordering::Relaxed);
            grant(next);
        }
    }
}

enum Acquired {
    Free,
    /// Granted after waiting; the node carries the hand-off stamp.
    Queued(*const Node),
    Served,
}

impl Sequencer {
    /// Take the token for `node` in `cohort`. Returns the node whose
    /// hand-off stamp describes a queued grant, if the requester waited.
    /// # Safety
    /// As for `Mcs::acquire`; the same cohort is passed to `release`.
    unsafe fn acquire(&self, node: *mut Node, cohort: usize) -> Acquired {
        let c = &self.cohorts[cohort];
        // SAFETY: the worker owns its node; the local holder alone owns the
        // cohort's global node.
        unsafe {
            let queued = c.local.acquire(node);
            if (*node).served.load(Ordering::Relaxed) {
                return Acquired::Served;
            }
            if (*node).inherited.load(Ordering::Relaxed) {
                return Acquired::Queued(node);
            }
            c.passes.store(0, Ordering::Relaxed);
            if self.global.acquire(&raw const c.node as *mut Node) {
                Acquired::Queued(&raw const c.node)
            } else if queued {
                Acquired::Queued(node)
            } else {
                Acquired::Free
            }
        }
    }

    /// Complete the delegated requests linked behind the holder in its cohort,
    /// in queue order, stopping at the first that needs the token itself.
    /// # Safety
    /// Node and cohort are the current holder's, and it holds the token.
    unsafe fn serve(&self, node: *mut Node, cohort: usize) -> usize {
        let local = &self.cohorts[cohort].local;
        let mut served = 0;
        // SAFETY: a linked waiter's node stays pinned until it is signalled;
        // its request was published before its link.
        unsafe {
            while served < SERVE_BATCH {
                let next = (*node).next.load(Ordering::Acquire);
                if next.is_null() {
                    break;
                }
                let request = (*next).request.load(Ordering::Acquire);
                if request.is_null() || !((*request).serve)(request) {
                    break;
                }
                let mut after = (*next).next.load(Ordering::Acquire);
                if after.is_null() {
                    (*node).next.store(ptr::null_mut(), Ordering::Relaxed);
                    if local
                        .tail
                        .compare_exchange(next, node, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        let mut spins = 0u32;
                        loop {
                            after = (*next).next.load(Ordering::Acquire);
                            if !after.is_null() {
                                break;
                            }
                            backoff(&mut spins);
                        }
                        (*node).next.store(after, Ordering::Release);
                    }
                } else {
                    (*node).next.store(after, Ordering::Release);
                }
                (*next).served.store(true, Ordering::Relaxed);
                grant(next);
                served += 1;
            }
        }
        served
    }

    /// # Safety
    /// Node and cohort are the current holder's.
    unsafe fn has_waiter(&self, node: *mut Node, cohort: usize) -> bool {
        let c = &self.cohorts[cohort];
        // SAFETY: the holder pins both of its nodes.
        unsafe {
            c.local.has_waiter(node) || self.global.has_waiter(&raw const c.node as *mut Node)
        }
    }

    /// # Safety
    /// Node and cohort are the current holder's.
    unsafe fn release(&self, node: *mut Node, cohort: usize, measured: bool) {
        let c = &self.cohorts[cohort];
        let global = &raw const c.node as *mut Node;
        // SAFETY: the holder owns both tokens and both of its nodes.
        unsafe {
            if c.local.has_waiter(node) {
                let remote = self.global.has_waiter(global);
                let passes = c.passes.load(Ordering::Relaxed);
                if !remote || passes < COHORT_PASSES {
                    c.passes
                        .store(passes + u32::from(remote), Ordering::Relaxed);
                    c.local.release(node, true, cohort, measured);
                    return;
                }
            }
            self.global.release(global, false, cohort, measured);
            c.local.release(node, false, cohort, measured);
        }
    }
}

/// Each CPU's cohort: its L3 id from sysfs, read once per process.
fn cohort_map() -> &'static [u8] {
    static L3: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    L3.get_or_init(|| {
        (0..4096)
            .map_while(|cpu| {
                std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index3/id"))
                    .ok()
                    .map(|id| (id.trim().parse::<usize>().unwrap_or(0) % COHORTS) as u8)
            })
            .collect()
    })
}

/// The calling CPU's cohort.
fn current_cohort() -> usize {
    // SAFETY: sched_getcpu has no preconditions.
    let cpu = unsafe { libc::sched_getcpu() };
    usize::try_from(cpu)
        .ok()
        .and_then(|cpu| cohort_map().get(cpu))
        .map_or(0, |&id| usize::from(id))
}

/// Keepable commits one holder may run without releasing.
const KEPT_RUN: u32 = 64;

/// This worker's cohort as of its last re-read, or the current CPU's.
pub(crate) fn cohort() -> usize {
    // SAFETY: install pins the current worker on this thread.
    match unsafe { OWNER.get().as_ref() } {
        Some(owner) if owner.recheck.get() != 0 => owner.cohort.get(),
        _ => current_cohort(),
    }
}

/// Queued grants per hand-off affinity decision.
const AFFINITY_WINDOW: u32 = 64;
/// A worker off the home L3 moves once this fraction of its window crossed.
const AFFINITY_SHARE: u32 = 8;

/// The L3 with the most allowed CPUs (lowest on a tie), when the worker
/// pool's inherited mask spans multiple L3s.
struct Home {
    cohort: usize,
    cpus: libc::cpu_set_t,
    allowed: libc::cpu_set_t,
    capacity: usize,
    moved: AtomicUsize,
}

impl Home {
    fn detect() -> Option<Self> {
        // SAFETY: an all-zero cpu_set_t is the empty set.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: the set is writable and sized; pid 0 is the calling thread,
        // which still runs with the pool's inherited mask.
        if unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut allowed) } != 0 {
            return None;
        }
        Self::from_allowed(allowed, cohort_map())
    }

    fn from_allowed(allowed: libc::cpu_set_t, map: &[u8]) -> Option<Self> {
        let mut counts = [0usize; COHORTS];
        for (cpu, &cohort) in map.iter().take(libc::CPU_SETSIZE as usize).enumerate() {
            // SAFETY: CPU_ISSET reads the initialized set within its size.
            if unsafe { libc::CPU_ISSET(cpu, &allowed) } {
                counts[usize::from(cohort)] += 1;
            }
        }
        if counts.iter().filter(|&&n| n > 0).count() < 2 {
            return None;
        }
        let home = (0..COHORTS).max_by_key(|&c| (counts[c], std::cmp::Reverse(c)))?;
        // SAFETY: an all-zero cpu_set_t is the empty set.
        let mut cpus: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for (cpu, &cohort) in map.iter().take(libc::CPU_SETSIZE as usize).enumerate() {
            // SAFETY: CPU_ISSET reads the initialized set within its size.
            if usize::from(cohort) == home && unsafe { libc::CPU_ISSET(cpu, &allowed) } {
                // SAFETY: CPU_SET writes within the set's size.
                unsafe { libc::CPU_SET(cpu, &mut cpus) };
            }
        }
        Some(Self {
            cohort: home,
            cpus,
            allowed,
            capacity: counts[home],
            moved: AtomicUsize::new(0),
        })
    }

    /// A successful move uses a slot for the lifetime of the pool.
    fn try_move(&self, apply: impl FnOnce(&libc::cpu_set_t) -> bool) -> bool {
        if self
            .moved
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |moved| {
                (moved < self.capacity).then_some(moved + 1)
            })
            .is_err()
        {
            return false;
        }
        if apply(&self.cpus) {
            return true;
        }
        self.moved.fetch_sub(1, Ordering::Relaxed);
        false
    }
}

fn set_affinity(cpus: &libc::cpu_set_t) -> bool {
    // SAFETY: the set is a valid cpu_set_t; 0 names the calling thread.
    unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), cpus) == 0 }
}

/// Outermost acquisitions between re-reads of the worker's CPU. A worker
/// rarely migrates, and a stale cohort only costs locality.
const COHORT_RECHECK: u32 = 64;

const GRANTED: u32 = 0;
const SPINNING: u32 = 1;
const PARKED: u32 = 2;
/// Spins before a queued waiter parks. A spinning waiter that the host
/// preempts, or that shares a core with the holder, stalls every waiter
/// behind it once it is granted; past about one queue's worth of commits a
/// parked waiter costs one futex wake instead (spin-then-park, as in
/// parking_lot and the lock-cohorting papers' blocking variants).
const WAIT_SPINS: u32 = 1 << 13;

/// Hand the token to a queued node, waking it if it parked.
/// # Safety
/// Node is a linked waiter that has not been granted.
unsafe fn grant(node: *mut Node) {
    // SAFETY: the waiter pins its node until it observes GRANTED.
    unsafe {
        if (*node).waiting.swap(GRANTED, Ordering::Release) == PARKED {
            futex_wake(&(*node).waiting);
        }
    }
}

/// Instrumented sample: token-queue parks, lobby parks, idle worker parks,
/// unparks of a pinned fiber's worker and unparks of a sleeper for stealable work.
#[cfg(feature = "testing")]
pub(crate) static PARKS: [std::sync::atomic::AtomicU64; 5] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 5];

fn futex_wait(word: &AtomicU32, expected: u32) {
    // SAFETY: FUTEX_WAIT reads the pinned word and returns at once if it changed.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            ptr::null::<libc::timespec>(),
        );
    }
}

fn futex_wake(word: &AtomicU32) {
    // SAFETY: waking a pinned word has no other effect.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1,
        );
    }
}

/// Spin briefly, then yield the CPU: a waiter that shares its CPU with the
/// holder or a preempted enqueuer would otherwise spin out its whole time
/// slice before the token can move (parking_lot's SpinWait shape).
#[inline(always)]
fn backoff(spins: &mut u32) {
    if *spins < SPIN_LIMIT {
        *spins += 1;
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}
const SPIN_LIMIT: u32 = 1 << 16;
const RECEIVE_RUN_LIMIT: u8 = 8;

/// A worker pins this record on its OS stack. Nested runtime operations use
/// the current commit without adding themselves to the MCS queue.
pub(crate) struct Owner {
    sequencer: *const Sequencer,
    node: Node,
    depth: Cell<usize>,
    held: Cell<bool>,
    cohort: Cell<usize>,
    recheck: Cell<u32>,
    /// Queued grants in the current affinity window, and those from another L3.
    grants: Cell<u32>,
    crossed: Cell<u32>,
    narrowed: Cell<bool>,
    /// The outermost commit may be kept for the next one.
    keepable: Cell<bool>,
    /// Commits kept without a release since the last hand-off.
    kept: Cell<u32>,
    receive_run: Cell<u8>,
    published: fn(),
    /// The run's live-unit count; one means this worker runs the only unit.
    units: *const std::sync::atomic::AtomicUsize,
    #[cfg(feature = "testing")]
    sample: Option<Sample>,
}
impl Owner {
    pub(crate) fn new(
        sequencer: &Sequencer,
        published: fn(),
        units: &std::sync::atomic::AtomicUsize,
        _measured: bool,
    ) -> Self {
        Self {
            sequencer,
            node: Node::default(),
            depth: Cell::new(0),
            held: Cell::new(false),
            cohort: Cell::new(0),
            recheck: Cell::new(0),
            grants: Cell::new(0),
            crossed: Cell::new(0),
            narrowed: Cell::new(false),
            keepable: Cell::new(false),
            kept: Cell::new(0),
            receive_run: Cell::new(0),
            published,
            units,
            #[cfg(feature = "testing")]
            sample: _measured.then(Sample::default),
        }
    }

    /// False only when the holder served this node's request instead.
    fn acquire(&self) -> bool {
        let cohort = match self.recheck.get() {
            0 => {
                self.recheck.set(COHORT_RECHECK);
                let cohort = current_cohort();
                self.cohort.set(cohort);
                cohort
            }
            n => {
                self.recheck.set(n - 1);
                self.cohort.get()
            }
        };
        // SAFETY: the sequencer outlives its owners.
        let lobby = unsafe { &(*self.sequencer).lobby };
        lobby.enter();
        // SAFETY: the pinned worker enqueues only its own idle node.
        let _queued =
            match unsafe { (*self.sequencer).acquire(&raw const self.node as *mut Node, cohort) } {
                Acquired::Served => {
                    lobby.leave();
                    return false;
                }
                Acquired::Free => None,
                Acquired::Queued(stamp) => Some(stamp),
            };
        if let Some(stamp) = _queued {
            // SAFETY: the stamped node is this worker's or its cohort's, both pinned.
            self.count_grant(unsafe { (*stamp).from.load(Ordering::Relaxed) } != cohort as u32);
        }
        #[cfg(feature = "testing")]
        if let Some(sample) = &self.sample {
            let now = crate::os::monotonic_ns();
            sample.started.set(now);
            if let Some(stamp) = _queued {
                // SAFETY: the stamped node is this worker's or its cohort's, both pinned.
                let stamp = unsafe { &*stamp };
                let released = stamp.released_at.load(Ordering::Relaxed);
                let elapsed = now.saturating_sub(released);
                sample.handoff.set(sample.handoff.get() + elapsed);
                sample.transfers.set(sample.transfers.get() + 1);
                if stamp.from.load(Ordering::Relaxed) != cohort as u32 {
                    sample.cross.set(sample.cross.get() + 1);
                    sample.cross_ns.set(sample.cross_ns.get() + elapsed);
                }
            }
        }
        true
    }

    /// An off-home worker can follow frequent remote grants onto the home
    /// L3, up to one narrowed worker per allowed CPU there.
    fn count_grant(&self, cross: bool) {
        if self.narrowed.get() {
            return;
        }
        let seen = self.grants.get() + 1;
        let crossed = self.crossed.get() + u32::from(cross);
        if seen < AFFINITY_WINDOW {
            self.grants.set(seen);
            self.crossed.set(crossed);
            return;
        }
        self.grants.set(0);
        self.crossed.set(0);
        // SAFETY: the sequencer outlives its owners.
        let home = unsafe { &(*self.sequencer).home };
        if crossed >= AFFINITY_WINDOW / AFFINITY_SHARE
            && let Some(home) = home.get_or_init(Home::detect)
            && self.cohort.get() != home.cohort
            && home.try_move(set_affinity)
        {
            self.narrowed.set(true);
            self.recheck.set(0);
        }
    }

    /// Serve linked delegated requests, then release. Nested entries made by
    /// a served request reuse the held token.
    fn finish(&self) {
        let _receives = self.receive_run.replace(0);
        #[cfg(feature = "testing")]
        if _receives != 0
            && let Some(sample) = &self.sample
        {
            let mut histogram = sample.receives.get();
            histogram[usize::from(_receives - 1)] += 1;
            sample.receives.set(histogram);
        }
        // A keepable commit by the run's only live unit keeps the token for a
        // bounded run while nobody waits: a requester can only be an idle
        // worker or a timer turn, which wait the same at N=1. The next
        // ordinary commit, switch or waiter ends the run.
        // SAFETY: the pool outlives every worker's owner.
        if self.keepable.replace(false)
            && self.kept.get() < KEPT_RUN
            && unsafe { (*self.units).load(Ordering::Acquire) } == 1
        {
            // SAFETY: this worker holds the token with its node and cohort.
            let idle = unsafe {
                !(*self.sequencer).has_waiter(&raw const self.node as *mut Node, self.cohort.get())
            };
            if idle {
                self.kept.set(self.kept.get() + 1);
                return;
            }
        }
        self.kept.set(0);
        self.depth.set(1);
        // SAFETY: this worker holds the token with its node and cohort.
        let _served = unsafe {
            (*self.sequencer).serve(&raw const self.node as *mut Node, self.cohort.get())
        };
        #[cfg(feature = "testing")]
        if let Some(sample) = &self.sample {
            sample.serves.set(sample.serves.get() + _served as u64);
        }
        self.depth.set(0);
        self.held.set(false);
        self.release();
        (self.published)();
    }

    fn release(&self) {
        #[cfg(feature = "testing")]
        let measured = self.sample.as_ref().is_some_and(|sample| {
            let elapsed = crate::os::monotonic_ns().saturating_sub(sample.started.get());
            sample.service.set(sample.service.get() + elapsed);
            sample.commits.set(sample.commits.get() + 1);
            true
        });
        #[cfg(not(feature = "testing"))]
        let measured = false;
        // SAFETY: only the token holder releases its node.
        unsafe {
            (*self.sequencer).release(
                &raw const self.node as *mut Node,
                self.cohort.get(),
                measured,
            );
            (*self.sequencer).lobby.leave();
        }
    }

    #[cfg(feature = "testing")]
    pub(crate) fn report(&self, index: usize) {
        if let Some(sample) = &self.sample {
            eprintln!(
                "sequencer worker={index} commits={} service_ns={} handoffs={} handoff_ns={} cross_l3={} cross_l3_ns={} served={} delegated={} parks={:?} receive_runs={:?}",
                sample.commits.get(),
                sample.service.get(),
                sample.transfers.get(),
                sample.handoff.get(),
                sample.cross.get(),
                sample.cross_ns.get(),
                sample.serves.get(),
                sample.delegated.get(),
                PARKS.each_ref().map(|n| n.load(Ordering::Relaxed)),
                sample.receives.get(),
            );
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if self.narrowed.get() {
            // SAFETY: the sequencer outlives this owner on the worker's stack.
            let home = unsafe { &(*self.sequencer).home };
            if let Some(Some(home)) = home.get() {
                set_affinity(&home.allowed);
            }
        }
    }
}

#[cfg(feature = "testing")]
#[derive(Default)]
struct Sample {
    started: Cell<u64>,
    commits: Cell<u64>,
    service: Cell<u64>,
    transfers: Cell<u64>,
    handoff: Cell<u64>,
    cross: Cell<u64>,
    cross_ns: Cell<u64>,
    serves: Cell<u64>,
    delegated: Cell<u64>,
    receives: Cell<[u64; RECEIVE_RUN_LIMIT as usize]>,
}

thread_local! {
    static OWNER: Cell<*mut Owner> = const { Cell::new(ptr::null_mut()) };
}

/// # Safety
/// Owner and its sequencer are pinned throughout run. No guard leaves run.
pub(crate) unsafe fn install<R>(owner: *mut Owner, run: impl FnOnce() -> R) -> R {
    struct Restore(*mut Owner);
    impl Drop for Restore {
        fn drop(&mut self) {
            OWNER.set(self.0);
        }
    }
    let _restore = Restore(OWNER.replace(owner));
    run()
}

#[must_use]
pub(crate) struct Guard(*mut Owner);

#[inline(always)]
pub(crate) fn enter() -> Guard {
    let owner = OWNER.get();
    // SAFETY: install pins the current worker and its sequencer on this thread.
    if let Some(owner) = unsafe { owner.as_ref() } {
        let depth = owner.depth.get();
        if depth == 0 {
            owner.keepable.set(false);
        }
        if !owner.held.replace(true) {
            // SAFETY: either enter reuses a token handed from the fiber or
            // its worker's outermost entry enqueues the idle node.
            owner.acquire();
        }
        owner.depth.set(depth + 1);
    }
    Guard(owner)
}

impl Guard {
    /// Keep this acquisition for bounded, private work in the same fiber.
    /// The emitter ends it before leaving that work or entering a slow path.
    #[inline(always)]
    pub(crate) fn receive_succeeded(self, more: bool) {
        // SAFETY: the worker is pinned by this guard.
        if let Some(owner) = unsafe { self.0.as_ref() }
            && owner.depth.get() == 1
        {
            let count = owner.receive_run.get() + 1;
            owner.receive_run.set(count);
            if more && count < RECEIVE_RUN_LIMIT {
                owner.depth.set(0);
                std::mem::forget(self);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn receives_in_grant() -> u8 {
    // SAFETY: the test reads its active worker's count.
    unsafe { OWNER.get().as_ref() }.map_or(0, |owner| owner.receive_run.get())
}

pub(crate) fn end_receive_run() {
    // SAFETY: the installed owner is pinned. A nonzero count at this
    // boundary belongs to the active fiber, outside a runtime commit.
    if let Some(owner) = unsafe { OWNER.get().as_ref() }
        && owner.receive_run.get() != 0
    {
        owner.finish();
    }
}

impl Drop for Guard {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: this guard is thread-bound and its scope pins the owner.
        if let Some(owner) = unsafe { self.0.as_ref() } {
            let depth = owner.depth.get() - 1;
            owner.depth.set(depth);
            if depth == 0 {
                owner.finish();
            }
        }
    }
}

/// Enter a commit its owner may keep for the next one (see `Owner::finish`).
#[inline(always)]
pub(crate) fn enter_keepable() -> Guard {
    let guard = enter();
    // SAFETY: install pins the current worker on this thread.
    if let Some(owner) = unsafe { guard.0.as_ref() }
        && owner.depth.get() == 1
    {
        owner.keepable.set(true);
    }
    guard
}

/// Transfer commit custody across the physical switch to the same worker's
/// dispatcher. No other worker may observe a parked or ended task until its
/// stack has stopped. The dispatcher adopts and releases the outer token.
pub(crate) fn to_dispatcher<R>(switch: impl FnOnce() -> R) -> R {
    let at = OWNER.get();
    // SAFETY: the installed worker is live across the synchronous switch.
    let Some(owner) = (unsafe { at.as_ref() }) else {
        return switch();
    };
    let depth = owner.depth.replace(0);
    let result = switch();
    // A dispatcher that resumes this stack hands over its held token.
    if depth != 0 && !owner.held.replace(true) {
        // SAFETY: the suspending stack resumed on the same worker.
        owner.acquire();
    }
    owner.depth.set(depth);
    result
}

/// Switch from the dispatcher into a fiber, handing it the dispatcher's
/// outermost commit. A suspended fiber resumes inside its own commit and keeps
/// the token; a fresh fiber releases it in `release_handed`.
pub(crate) fn to_fiber<R>(commit: Guard, switch: impl FnOnce() -> R) -> R {
    let at = commit.0;
    std::mem::forget(commit);
    // SAFETY: the installed worker is live across the synchronous switch.
    if let Some(owner) = unsafe { at.as_ref() } {
        debug_assert_eq!(owner.depth.get(), 1);
        owner.depth.set(0);
    }
    switch()
}

/// A fresh fiber's first instruction: release a token its dispatcher handed over.
pub(crate) fn release_handed() {
    let at = OWNER.get();
    // SAFETY: install pins the current worker and its sequencer on this thread.
    if let Some(owner) = unsafe { at.as_ref() }
        && owner.depth.get() == 0
        && owner.held.get()
    {
        owner.finish();
    }
}

/// The outcome of offering a commit for delegation.
pub(crate) enum Entry {
    /// The requester holds the token, as after `enter`.
    Held(Guard),
    /// Another worker completed the request under the token.
    Served,
}

/// Enter the commit, or let the current holder complete `request` for this
/// worker while it waits in the same cohort.
/// # Safety
/// Request and everything it points to stay pinned until this returns.
pub(crate) unsafe fn enter_or_delegate(request: *mut Request) -> Entry {
    let at = OWNER.get();
    // SAFETY: install pins the current worker and its sequencer on this thread.
    let Some(owner) = (unsafe { at.as_ref() }) else {
        return Entry::Held(Guard(at));
    };
    if owner.held.get() {
        return Entry::Held(enter());
    }
    owner.node.request.store(request, Ordering::Relaxed);
    owner.held.set(true);
    let served = !owner.acquire();
    owner.node.request.store(ptr::null_mut(), Ordering::Relaxed);
    if served {
        owner.held.set(false);
        #[cfg(feature = "testing")]
        if let Some(sample) = &owner.sample {
            sample.delegated.set(sample.delegated.get() + 1);
        }
        return Entry::Served;
    }
    owner.depth.set(1);
    Entry::Held(Guard(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrowed_workers_fit_the_home_cohort() {
        // Two allowed CPUs per L3. The lower L3 wins the tie, so at most
        // two workers may lose CPUs 2 and 3 from their original mask.
        // SAFETY: these four CPU indices fit a zeroed cpu_set_t.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for cpu in 0..4 {
            // SAFETY: the set is initialized and the index fits.
            unsafe { libc::CPU_SET(cpu, &mut allowed) };
        }
        let home = Home::from_allowed(allowed, &[0, 0, 1, 1]).unwrap();
        let mut masks = [allowed; 4];
        for mask in &mut masks {
            home.try_move(|cpus| {
                *mask = *cpus;
                true
            });
        }
        for mask in masks.iter().take(2) {
            // SAFETY: all indices fit and the set is initialized.
            assert!(unsafe {
                libc::CPU_ISSET(0, mask)
                    && libc::CPU_ISSET(1, mask)
                    && !libc::CPU_ISSET(2, mask)
                    && !libc::CPU_ISSET(3, mask)
            });
        }
        for mask in masks.iter().skip(2) {
            // SAFETY: the initialized set includes both CPUs on the other L3.
            assert!(unsafe { libc::CPU_ISSET(2, mask) && libc::CPU_ISSET(3, mask) });
        }
    }
}
