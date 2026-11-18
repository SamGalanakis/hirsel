//! FIFO commit ownership. The residual domain has one MCS tail.
use std::{
    cell::Cell,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

#[derive(Default)]
pub(crate) struct Sequencer {
    tail: AtomicPtr<Node>,
}

#[repr(align(64))]
#[derive(Default)]
struct Node {
    next: AtomicPtr<Node>,
    waiting: AtomicBool,
    #[cfg(feature = "testing")]
    released_at: std::sync::atomic::AtomicU64,
    #[cfg(feature = "testing")]
    released_l3: std::sync::atomic::AtomicU32,
}

impl Sequencer {
    /// # Safety
    /// Node stays pinned and belongs exclusively to this requester until it
    /// releases. A worker uses one node for its outermost commit.
    unsafe fn acquire(&self, node: *mut Node) -> bool {
        // SAFETY: each worker owns its node. The tail keeps the predecessor
        // pinned until the successor publishes its link.
        unsafe {
            (*node).next.store(ptr::null_mut(), Ordering::Relaxed);
            (*node).waiting.store(true, Ordering::Relaxed);
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
    /// Node is the current guard's node and was acquired exactly once.
    unsafe fn release(&self, node: *mut Node, _measured: bool) {
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
            (*next).waiting.store(false, Ordering::Release);
        }
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
const SPIN_LIMIT: u32 = 1 << 10;

/// A worker pins this record on its OS stack. Nested runtime operations use
/// the current commit without adding themselves to the MCS queue.
pub(crate) struct Owner {
    sequencer: *const Sequencer,
    node: Node,
    depth: Cell<usize>,
    held: Cell<bool>,
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
            published,
            #[cfg(feature = "testing")]
            sample: _measured.then(Sample::default),
        }
    }

    fn acquire(&self) {
        // SAFETY: the pinned worker enqueues only its own idle node.
        let _queued = unsafe { (*self.sequencer).acquire(&raw const self.node as *mut Node) };
        #[cfg(feature = "testing")]
        if let Some(sample) = &self.sample {
            let now = crate::os::monotonic_ns();
            sample.started.set(now);
            if _queued {
                let released = self.node.released_at.load(Ordering::Relaxed);
                let elapsed = now.saturating_sub(released);
                sample.handoff.set(sample.handoff.get() + elapsed);
                sample.transfers.set(sample.transfers.get() + 1);
                if self.node.released_l3.load(Ordering::Relaxed) != current_l3() {
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
        unsafe { (*self.sequencer).release(&raw const self.node as *mut Node, measured) };
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

/// The measuring host's L3 id for the calling CPU, from sysfs.
#[cfg(feature = "testing")]
fn current_l3() -> u32 {
    static L3: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    let ids = L3.get_or_init(|| {
        (0..1024)
            .map_while(|cpu| {
                std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index3/id"))
                    .ok()
                    .map(|id| id.trim().parse().unwrap_or(0))
            })
            .collect()
    });
    // SAFETY: sched_getcpu has no preconditions.
    let cpu = unsafe { libc::sched_getcpu() };
    usize::try_from(cpu)
        .ok()
        .and_then(|cpu| ids.get(cpu).copied())
        .unwrap_or(u32::MAX)
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
