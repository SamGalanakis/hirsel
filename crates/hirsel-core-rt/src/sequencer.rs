//! Commit ownership. The residual domain has one token, passed FIFO within
//! an L3 cohort for a bounded run before a waiting remote cohort is served
//! (lock cohorting: Dice, Marathe and Shavit, C-BO-MCS shape, with MCS at
//! both levels). The tape records the actual commit order, so the grant
//! order is not part of the recorded schedule.
use std::{
    cell::Cell,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
};

/// Commits granted inside one cohort while another cohort waits.
const COHORT_PASSES: u32 = 64;
const COHORTS: usize = 8;

#[derive(Default)]
pub(crate) struct Sequencer {
    global: Mcs,
    cohorts: [Cohort; COHORTS],
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
    waiting: AtomicBool,
    /// The predecessor passed the global token with the local one.
    inherited: AtomicBool,
    #[cfg(feature = "testing")]
    released_at: std::sync::atomic::AtomicU64,
    #[cfg(feature = "testing")]
    released_l3: std::sync::atomic::AtomicU32,
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
            (*node).waiting.store(true, Ordering::Relaxed);
            (*node).inherited.store(false, Ordering::Relaxed);
            let predecessor = self.tail.swap(node, Ordering::AcqRel);
            if !predecessor.is_null() {
                (*predecessor).next.store(node, Ordering::Release);
                let mut spins = 0u32;
                while (*node).waiting.load(Ordering::Acquire) {
                    backoff(&mut spins);
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
    unsafe fn release(&self, node: *mut Node, inherited: bool, _measured: bool) {
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
                (*next).released_l3.store(current_l3(), Ordering::Relaxed);
                (*next)
                    .released_at
                    .store(crate::os::monotonic_ns(), Ordering::Relaxed);
            }
            (*next).inherited.store(inherited, Ordering::Relaxed);
            (*next).waiting.store(false, Ordering::Release);
        }
    }
}

impl Sequencer {
    /// Take the token for `node` in `cohort`. Returns the node whose
    /// hand-off stamp describes a queued grant, if the requester waited.
    /// # Safety
    /// As for `Mcs::acquire`; the same cohort is passed to `release`.
    unsafe fn acquire(&self, node: *mut Node, cohort: usize) -> Option<*const Node> {
        let c = &self.cohorts[cohort];
        // SAFETY: the worker owns its node; the local holder alone owns the
        // cohort's global node.
        unsafe {
            let queued = c.local.acquire(node);
            if (*node).inherited.load(Ordering::Relaxed) {
                return Some(node);
            }
            c.passes.store(0, Ordering::Relaxed);
            let remote = self.global.acquire(&raw const c.node as *mut Node);
            if remote {
                Some(&raw const c.node)
            } else {
                queued.then_some(node.cast_const())
            }
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
                    c.local.release(node, true, measured);
                    return;
                }
            }
            self.global.release(global, false, measured);
            c.local.release(node, false, measured);
        }
    }
}

/// The calling CPU's cohort: its L3 id from sysfs, read once per process.
fn current_cohort() -> usize {
    static L3: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let ids = L3.get_or_init(|| {
        (0..4096)
            .map_while(|cpu| {
                std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index3/id"))
                    .ok()
                    .map(|id| (id.trim().parse::<usize>().unwrap_or(0) % COHORTS) as u8)
            })
            .collect()
    });
    // SAFETY: sched_getcpu has no preconditions.
    let cpu = unsafe { libc::sched_getcpu() };
    usize::try_from(cpu)
        .ok()
        .and_then(|cpu| ids.get(cpu))
        .map_or(0, |&id| usize::from(id))
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
const SPIN_LIMIT: u32 = 1 << 10;

/// A worker pins this record on its OS stack. Nested runtime operations use
/// the current commit without adding themselves to the MCS queue.
pub(crate) struct Owner {
    sequencer: *const Sequencer,
    node: Node,
    depth: Cell<usize>,
    held: Cell<bool>,
    cohort: Cell<usize>,
    published: fn(),
    #[cfg(feature = "testing")]
    sample: Option<Sample>,
}
impl Owner {
    pub(crate) fn new(sequencer: &Sequencer, published: fn(), _measured: bool) -> Self {
        Self {
            sequencer,
            node: Node::default(),
            depth: Cell::new(0),
            held: Cell::new(false),
            cohort: Cell::new(0),
            published,
            #[cfg(feature = "testing")]
            sample: _measured.then(Sample::default),
        }
    }

    fn acquire(&self) {
        let cohort = current_cohort();
        self.cohort.set(cohort);
        // SAFETY: the pinned worker enqueues only its own idle node.
        let _queued =
            unsafe { (*self.sequencer).acquire(&raw const self.node as *mut Node, cohort) };
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
                if stamp.released_l3.load(Ordering::Relaxed) != current_l3() {
                    sample.cross.set(sample.cross.get() + 1);
                    sample.cross_ns.set(sample.cross_ns.get() + elapsed);
                }
            }
        }
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
        }
    }

    #[cfg(feature = "testing")]
    pub(crate) fn report(&self, index: usize) {
        if let Some(sample) = &self.sample {
            eprintln!(
                "sequencer worker={index} commits={} service_ns={} handoffs={} handoff_ns={} cross_l3={} cross_l3_ns={}",
                sample.commits.get(),
                sample.service.get(),
                sample.transfers.get(),
                sample.handoff.get(),
                sample.cross.get(),
                sample.cross_ns.get(),
            );
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
}

#[cfg(feature = "testing")]
fn current_l3() -> u32 {
    current_cohort() as u32
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
        if !owner.held.replace(true) {
            // SAFETY: either enter reuses a token handed from the fiber or
            // its worker's outermost entry enqueues the idle node.
            owner.acquire();
        }
        owner.depth.set(depth + 1);
    }
    Guard(owner)
}

impl Drop for Guard {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: this guard is thread-bound and its scope pins the owner.
        if let Some(owner) = unsafe { self.0.as_ref() } {
            let depth = owner.depth.get() - 1;
            owner.depth.set(depth);
            if depth == 0 {
                // SAFETY: the worker exits its outermost commit.
                owner.held.set(false);
                owner.release();
                (owner.published)();
            }
        }
    }
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
        && owner.held.replace(false)
    {
        owner.release();
        (owner.published)();
    }
}
