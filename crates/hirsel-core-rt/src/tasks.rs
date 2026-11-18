//! Native tasks and their pinned-fiber dispatchers. Queue links, wait state and result storage
//! are prepaid at spawn. Only blocking or explicit checkpoints switch stacks.
use crate::{
    Capacity, Context, abi_tasks as abi,
    block::Block,
    causes::{self, Cause, Causes, Cell},
    fiber::{Stack, Stock},
};
use hirsel_core_layout::charge;
use std::{
    alloc::{Layout, alloc, dealloc},
    cell::Cell as Local,
    collections::{BTreeMap, VecDeque},
    ptr::{self, NonNull},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ValueLayout {
    pub bytes: usize,
    pub align: usize,
    pub logical: u64,
    /// The compiler's resumable cleanup of one value in place, run on the
    /// active drain (counted::Recipe). None for a task input, which the child
    /// body always cleans itself.
    pub clean: Option<crate::counted::Recipe>,
}
/// A generated body cleans every local owner, including task handles, before
/// returning. The runtime trusts that cleanup; it never scans for omitted drops.
pub type Body = unsafe extern "C" fn(*mut Context, *mut u8, *mut u8) -> u8;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TaskLayout {
    pub input: ValueLayout,
    pub result: ValueLayout,
}

/// Host bookkeeping storage, whose logical charge was already admitted whole.
pub(crate) struct Storage {
    pointer: NonNull<u8>,
    layout: Layout,
}
impl Storage {
    pub(crate) fn new(bytes: usize, align: usize) -> Self {
        let layout = Layout::from_size_align(bytes, align)
            .unwrap_or_else(|_| crate::process::stop(b"HostMemory"));
        // SAFETY: nonzero compiler-produced layout, uniquely owned until Drop.
        let pointer = unsafe {
            if bytes == 0 {
                NonNull::new_unchecked(align as *mut u8)
            } else {
                NonNull::new(alloc(layout)).unwrap_or_else(|| crate::process::stop(b"HostMemory"))
            }
        };
        #[cfg(any(test, feature = "charge-observation"))]
        if bytes != 0 {
            crate::peak::allocated(pointer.as_ptr(), bytes);
        }
        Self { pointer, layout }
    }
    pub(crate) fn as_ptr(&self) -> *mut u8 {
        self.pointer.as_ptr()
    }
}
impl Drop for Storage {
    fn drop(&mut self) {
        // SAFETY: sole owner of this host storage; payload is already moved/cleaned.
        unsafe {
            if self.layout.size() != 0 {
                #[cfg(any(test, feature = "charge-observation"))]
                crate::peak::deallocated(self.pointer.as_ptr());
                dealloc(self.pointer.as_ptr(), self.layout);
            }
        }
    }
}

pub use hirsel_core_layout::event::{
    ARC_NEW, ARC_RELEASE, ARC_SHARE, ARC_UNWRAP, CANCEL, CANCELLED, CAPACITY, CHECK_CANCEL, CLOSE,
    END, JOIN, LATEST_PUBLISH, LATEST_READ, PUBLISH, RECEIVE, REFUND, SENDERS, SPAWN, TRY_RECEIVE,
    TRY_SEND,
};
/// Only no-effect polls count; effectful operations never consume this allowance.
pub(crate) const POLLS_PER_TURN: u32 = 64;
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u8,
    pub task: u64,
    pub object: u64,
    pub value: u64,
}
/// Event callbacks record completed transitions and must not suspend/re-enter.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SchedulerProvider {
    pub state: *mut u8,
    pub event: Option<unsafe extern "C" fn(*mut u8, Event)>,
    /// Reserved ABI word. Schedule scripts are installed by the Rust test harness.
    pub reserved: usize,
}
impl Default for SchedulerProvider {
    fn default() -> Self {
        Self {
            state: ptr::null_mut(),
            event: None,
            reserved: 0,
        }
    }
}
thread_local! { static CALLBACK_TASK: Local<*mut Task> = const { Local::new(ptr::null_mut()) }; }
thread_local! { static CURRENT: Local<*mut Task> = const { Local::new(ptr::null_mut()) }; }

pub(crate) fn private_flow_step<R>(run: impl FnOnce() -> R) -> R {
    struct Restore(*mut Task);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.set(self.0);
        }
    }
    let _restore = Restore(CURRENT.replace(ptr::null_mut()));
    crate::limit::preserve_binding(run)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Waiting,
    /// A private flow is still computing the answer. Cancellation only sets the flag.
    Flow,
    /// Parked in a provider operation's W or I. Only that operation's own wakes
    /// (its timer, its token, a cancellation request) move it; `wake` does not.
    Provider,
    Ended,
}
/// The first byte sends Arc count operations through the runtime commit path:
/// taped replay, and a pool task whose events something records. Another
/// task counts inline. Its emitted code skips the init/share/release hooks
/// while the second byte is clear and its payer's domain has no posted credit.
#[repr(C)]
pub struct Task {
    pub(crate) arc_runtime: bool,
    /// Frozen at TASK_ARC_HOOKS: the inline init/share/release hooks must run
    /// whatever the payer's credit (`Scheduler::arc_hooks`). Every writer of
    /// an input recomputes it; the constructors start it set.
    pub(crate) arc_hooks: bool,
    cancel: AtomicBool,
    replay: bool,
    latest_pair: bool,
    polls_left: u32,
    context: *mut Context,
    owned_context: Option<Context>,
    run: *mut Scheduler,
    ready: *mut Task,
    domain: u64,
    worker: Option<usize>,
    #[cfg(feature = "testing")]
    enqueued_at: u64,
    waiters: Waiters,
    waiting_on: *mut Waiters,
    wait_previous: *mut Task,
    wait_next: *mut Task,
    timer_next: *mut Task,
    /// Host CLOCK_MONOTONIC nanoseconds.
    timer_deadline: Option<u64>,
    operation: *mut Operation,
    all: *mut Task,
    state: State,
    id: u64,
    group: Option<crate::tape::schedule::Group>,
    stack: Option<Stack>,
    sp: *mut u8,
    body: Body,
    input: Storage,
    result: Storage,
    layout: TaskLayout,
    failed: u8,
    record: Option<Block>,
    /// The abandoning parent's cleanup frame, once the task has ended.
    holder: std::mem::MaybeUninit<crate::counted::Holder>,
    /// How far the result's cleanup has got.
    cursor: u64,
}
#[derive(Clone, Copy, Eq)]
struct ReadyToken(u64);
impl PartialEq for ReadyToken {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(test)]
        ROUTE_WORK.set(ROUTE_WORK.get() + 1);
        self.0 == other.0
    }
}
impl std::hash::Hash for ReadyToken {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        #[cfg(test)]
        ROUTE_WORK.set(ROUTE_WORK.get() + 1);
        self.0.hash(state);
    }
}
#[cfg(test)]
thread_local! {
    static ROUTE_WORK: Local<usize> = const { Local::new(0) };
    static ROUTE_WAKES: Local<usize> = const { Local::new(0) };
}

#[derive(Clone, Copy)]
pub(crate) struct ReadyArm {
    token: u64,
    identity: u64,
}

// A network poll can return 128 events, each with a read and a write waiter.
// Larger batches still drain in full, but release their backing after the turn.
const RETAINED_READY_CAPACITY: usize = 256;

struct Arm<T> {
    task: T,
    identity: u64,
    last_turn: u64,
}

#[derive(Default)]
pub(crate) struct WaitArms<T> {
    // Only runtime-issued tokens become keys. Providers can submit lookups.
    live: std::collections::HashMap<ReadyToken, Arm<T>, foldhash::fast::RandomState>,
    next_identity: u64,
    turn: u64,
}

impl<T> WaitArms<T> {
    pub(crate) fn insert(&mut self, token: u64, task: T) {
        let identity = self.next_identity;
        self.next_identity = identity
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        self.live.insert(
            ReadyToken(token),
            Arm {
                task,
                identity,
                last_turn: 0,
            },
        );
    }

    pub(crate) fn capture(
        &mut self,
        tokens: impl IntoIterator<Item = u64>,
        captured: &mut Vec<ReadyArm>,
    ) {
        self.turn = self
            .turn
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        captured.extend(tokens.into_iter().filter_map(|token| {
            let arm = self.live.get_mut(&ReadyToken(token))?;
            if arm.last_turn == self.turn {
                return None;
            }
            arm.last_turn = self.turn;
            Some(ReadyArm {
                token,
                identity: arm.identity,
            })
        }));
    }

    pub(crate) fn take_captured(&mut self, ready: ReadyArm) -> Option<T> {
        use std::collections::hash_map::Entry;
        match self.live.entry(ReadyToken(ready.token)) {
            Entry::Occupied(arm) if arm.get().identity == ready.identity => Some(arm.remove().task),
            _ => None,
        }
    }

    fn remove(&mut self, token: u64) {
        self.live.remove(&ReadyToken(token));
    }

    #[cfg(test)]
    fn peek(&self, token: u64) -> Option<&T> {
        self.live.get(&ReadyToken(token)).map(|arm| &arm.task)
    }
}

/// FIFO waiters on one task or channel condition. The owner stays pinned
/// by every waiter, and registration lasts until a wake or cancellation.
#[derive(Default)]
pub(crate) struct Waiters {
    first: *mut Task,
    last: *mut Task,
}

impl Waiters {
    unsafe fn push(queue: *mut Self, task: *mut Task) {
        // SAFETY: the active task and the queue are exclusively accessible.
        unsafe {
            (*task).waiting_on = queue;
            (*task).wait_previous = (*queue).last;
            if (*queue).last.is_null() {
                (*queue).first = task;
            } else {
                (*(*queue).last).wait_next = task;
            }
            (*queue).last = task;
        }
    }

    unsafe fn remove(task: *mut Task) {
        // SAFETY: only a registered task can be removed. Its wait pins the queue.
        unsafe {
            let queue = (*task).waiting_on;
            let previous = (*task).wait_previous;
            let next = (*task).wait_next;
            if previous.is_null() {
                (*queue).first = next;
            } else {
                (*previous).wait_next = next;
            }
            if next.is_null() {
                (*queue).last = previous;
            } else {
                (*next).wait_previous = previous;
            }
            (*task).waiting_on = ptr::null_mut();
            (*task).wait_previous = ptr::null_mut();
            (*task).wait_next = ptr::null_mut();
        }
    }

    /// Ready the registered waiters in park order.
    ///
    /// # Safety
    /// The active commit owns the pinned queue and the scheduling state.
    pub(crate) unsafe fn wake_all(queue: *mut Self) -> usize {
        let mut count = 0;
        // SAFETY: each removal leaves the successor at the head.
        unsafe {
            while !(*queue).first.is_null() {
                let task = (*queue).first;
                Self::remove(task);
                (*(*task).run).ready(task);
                count += 1;
            }
        }
        count
    }
}

use crate::tape::schedule::Group;

struct LiveTape {
    group: Group,
    opened: u64,
    members: usize,
}

#[derive(Default)]
struct Tapes {
    live: Vec<LiveTape>,
    next_open: u64,
    // Opening a tape links its scheduler input path, including task ordering.
    take_inputs: Option<unsafe fn(&mut Scheduler)>,
}

impl Tapes {
    fn open(&mut self, group: Group) {
        let opened = self.next_open;
        self.next_open = opened
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"TapeSlots"));
        self.live.push(LiveTape {
            group,
            opened,
            members: 0,
        });
        self.take_inputs = Some(Scheduler::take_tape_inputs);
    }

    fn membership(&mut self, group: Group) -> Option<&mut usize> {
        self.live
            .iter_mut()
            .find(|entry| entry.group.same(group))
            .map(|entry| &mut entry.members)
    }

    fn close(&mut self, group: Group) {
        if let Some(index) = self.live.iter().position(|entry| entry.group.same(group)) {
            self.live.swap_remove(index);
            if self.live.is_empty() {
                self.take_inputs = None;
            }
        }
    }

    fn seal_deadlock(&mut self) -> Option<([u8; 16], crate::tape::Fault)> {
        self.live
            .drain(..)
            .filter_map(|entry| {
                entry
                    .group
                    .seal_deadlock()
                    .err()
                    .map(|fault| (entry.opened, fault))
            })
            .min_by_key(|(opened, _)| *opened)
            .map(|(_, fault)| fault)
    }
}

const RESIDUAL_DOMAIN: u64 = 0;
const LOCAL_QUEUE: usize = 256;
/// How long an idle worker spins for a wake before it parks its thread.
const IDLE_SPIN_NS: u64 = 50_000;
const LIFO_TURNS: u8 = 3;

#[derive(Clone, Copy)]
struct TaskPtr(*mut Task);
// SAFETY: a published pointer is a sole queue unit; the dispatcher may
// access Task metadata only in its commit, and only the pinned worker runs it.
unsafe impl Send for TaskPtr {}

struct Delivery {
    task: TaskPtr,
    worker: Option<usize>,
    deferred: bool,
}

#[derive(Default)]
struct Domain {
    sequencer: crate::sequencer::Sequencer,
    ready: AtomicUsize,
    handoffs: AtomicUsize,
}

struct WorkerQueue {
    pinned: VecDeque<TaskPtr>,
    flow_wakes: VecDeque<TaskPtr>,
    deferred: VecDeque<TaskPtr>,
    lifo: Option<TaskPtr>,
    /// A thief passed over the current LIFO task once.
    lifo_seen: bool,
    /// A thief in another L3 cohort passed this queue over once.
    remote_seen: bool,
    lifo_turns: u8,
    local: VecDeque<TaskPtr>,
}
impl WorkerQueue {
    fn new() -> Self {
        Self {
            pinned: VecDeque::new(),
            flow_wakes: VecDeque::new(),
            deferred: VecDeque::new(),
            lifo: None,
            lifo_seen: false,
            remote_seen: false,
            lifo_turns: 0,
            local: VecDeque::with_capacity(LOCAL_QUEUE),
        }
    }

    fn pop(&mut self) -> Option<TaskPtr> {
        if let Some(task) = self.pinned.pop_front() {
            return Some(task);
        }
        if self.lifo_turns < LIFO_TURNS
            && let Some(task) = self.lifo.take()
        {
            self.lifo_turns += 1;
            return Some(task);
        }
        self.lifo_turns = 0;
        self.local
            .pop_front()
            .or_else(|| self.lifo.take())
            .or_else(|| self.deferred.pop_front())
    }
}

struct Pool {
    registry: Mutex<()>,
    domains: BTreeMap<u64, Arc<Domain>>,
    queues: Vec<Mutex<WorkerQueue>>,
    injector: Mutex<VecDeque<TaskPtr>>,
    flow: Mutex<VecDeque<FlowJob>>,
    threads: Vec<OnceLock<std::thread::Thread>>,
    sleepers: Mutex<Vec<usize>>,
    sleeping: AtomicUsize,
    /// Each worker's last observed L3 cohort.
    cohorts: Vec<AtomicUsize>,
    /// Set by every unpark of the worker, so its idle spin sees the wake.
    nudged: Vec<AtomicBool>,

    spare: crate::fiber::Spare,
    readies: Arc<Readies>,
    funding: crate::limit::CacheDomain,
    units: AtomicUsize,
    serial_replay: bool,
    stop_flow_workers: AtomicBool,
    measure: bool,
    #[cfg(feature = "testing")]
    sample: PoolSample,
    #[cfg(test)]
    idle_checks: AtomicUsize,
    #[cfg(any(test, feature = "testing"))]
    script: Option<Arc<crate::schedule_script::ScheduleScript>>,
}
impl Pool {
    fn new(
        workers: usize,
        readies: Arc<Readies>,
        funding: crate::limit::CacheDomain,
        serial_replay: bool,
    ) -> Self {
        Self {
            registry: Mutex::default(),
            domains: BTreeMap::from([(RESIDUAL_DOMAIN, Arc::new(Domain::default()))]),
            queues: (0..workers)
                .map(|_| Mutex::new(WorkerQueue::new()))
                .collect(),
            injector: Mutex::default(),
            flow: Mutex::default(),
            threads: (0..workers).map(|_| OnceLock::new()).collect(),
            sleepers: Mutex::default(),
            sleeping: AtomicUsize::new(0),
            cohorts: (0..workers).map(|_| AtomicUsize::new(0)).collect(),
            nudged: (0..workers).map(|_| AtomicBool::new(false)).collect(),

            spare: crate::fiber::Spare::default(),
            readies,
            funding,
            units: AtomicUsize::new(usize::from(!serial_replay)),
            serial_replay,
            stop_flow_workers: AtomicBool::new(false),
            measure: measurement_enabled(),
            #[cfg(feature = "testing")]
            sample: PoolSample::default(),
            #[cfg(test)]
            idle_checks: AtomicUsize::new(0),
            #[cfg(any(test, feature = "testing"))]
            script: crate::schedule_script::installed(),
        }
    }

    #[cfg(feature = "testing")]
    fn report_domain_lookup(&self) {
        use std::hint::black_box;
        const TRIALS: usize = 100_000;
        let direct = &self.domains[&RESIDUAL_DOMAIN];
        let start = crate::os::monotonic_ns();
        for _ in 0..TRIALS {
            black_box(black_box(direct));
        }
        let direct_ns = crate::os::monotonic_ns() - start;
        let start = crate::os::monotonic_ns();
        for _ in 0..TRIALS {
            black_box(&black_box(&self.domains)[&black_box(RESIDUAL_DOMAIN)]);
        }
        let keyed_ns = crate::os::monotonic_ns() - start;
        eprintln!("domain_lookup trials={TRIALS} direct_ns={direct_ns} keyed_ns={keyed_ns}");
    }

    fn wake_workers(&self) {
        for index in 0..self.threads.len() {
            self.unpark(index);
        }
        self.readies.wake_poller();
    }

    fn unpark(&self, index: usize) {
        self.nudged[index].store(true, Ordering::Release);
        if let Some(thread) = self.threads[index].get() {
            thread.unpark();
        }
    }

    /// Wake one parked worker for newly stealable work, as Tokio's
    /// `notify_parked` does. The fence pairs with `sleep`'s.
    fn notify_one(&self, source: usize) {
        std::sync::atomic::fence(Ordering::SeqCst);
        if self.sleeping.load(Ordering::SeqCst) == 0 {
            return;
        }
        // Prefer a sleeper in the publisher's cohort, so the work and the
        // commits it makes stay in one L3.
        let cohort = self.cohorts[source].load(Ordering::Relaxed);
        let sleeper = {
            let mut sleepers = self.sleepers.lock().unwrap();
            let at = sleepers
                .iter()
                .rposition(|&i| self.cohorts[i].load(Ordering::Relaxed) == cohort)
                .or_else(|| sleepers.len().checked_sub(1));
            at.map(|at| sleepers.swap_remove(at))
        };
        if let Some(index) = sleeper {
            #[cfg(feature = "testing")]
            crate::sequencer::PARKS[4].fetch_add(1, Ordering::Relaxed);
            self.sleeping.fetch_sub(1, Ordering::SeqCst);
            self.unpark(index);
        }
    }

    /// Register before the last look at stealable work, so a publisher that
    /// misses the work's consumer finds it here instead.
    fn sleep(&self, index: usize) {
        self.cohorts[index].store(crate::sequencer::cohort(), Ordering::Relaxed);
        let mut sleepers = self.sleepers.lock().unwrap();
        if !sleepers.contains(&index) {
            sleepers.push(index);
            self.sleeping.fetch_add(1, Ordering::SeqCst);
        }
        drop(sleepers);
        std::sync::atomic::fence(Ordering::SeqCst);
    }

    fn wake(&self, index: usize) {
        let mut sleepers = self.sleepers.lock().unwrap();
        if let Some(at) = sleepers.iter().position(|&i| i == index) {
            sleepers.swap_remove(at);
            self.sleeping.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn has_work(&self, index: usize) -> bool {
        let own = self.queues[index].lock().unwrap();
        if !own.pinned.is_empty()
            || !own.deferred.is_empty()
            || !own.flow_wakes.is_empty()
            || own.lifo.is_some()
            || !own.local.is_empty()
        {
            return true;
        }
        drop(own);
        !self.flow.lock().unwrap().is_empty()
            || !self.injector.lock().unwrap().is_empty()
            || self.queues.iter().any(|queue| {
                let queue = queue.lock().unwrap();
                !queue.local.is_empty() || queue.lifo.is_some()
            })
    }

    fn publish(&self, source: usize, deliveries: &mut Vec<Delivery>) {
        if deliveries.is_empty() {
            return;
        }
        for delivery in deliveries.drain(..) {
            if let Some(worker) = delivery.worker {
                let mut queue = self.queues[worker].lock().unwrap();
                if delivery.deferred {
                    queue.deferred.push_back(delivery.task);
                } else {
                    queue.pinned.push_back(delivery.task);
                }
                drop(queue);
                #[cfg(feature = "testing")]
                crate::sequencer::PARKS[3].fetch_add(1, Ordering::Relaxed);
                self.unpark(worker);
            } else {
                let mut queue = self.queues[source].lock().unwrap();
                queue.lifo_seen = false;
                if let Some(previous) = queue.lifo.replace(delivery.task) {
                    if queue.local.len() >= LOCAL_QUEUE {
                        let mut injector = self.injector.lock().unwrap();
                        injector.extend(queue.local.drain(..LOCAL_QUEUE / 2));
                    }
                    queue.local.push_back(previous);
                }
                drop(queue);
                self.notify_one(source);
            }
        }
        self.readies.wake_poller();
    }

    fn pop_flow_wake(&self, index: usize) -> Option<TaskPtr> {
        self.queues[index].lock().unwrap().flow_wakes.pop_front()
    }

    fn run_flow_job(&self) -> bool {
        let job = self.flow.lock().unwrap().pop_front();
        let Some(job) = job else { return false };
        // SAFETY: the dispatcher owns this worker's TLS slot, with no active commit.
        unsafe {
            crate::sequencer::install(ptr::null_mut(), || job.flow.run(job.stage));
        }
        let was_last = self.units.fetch_sub(1, Ordering::AcqRel) == 1;
        if was_last && self.serial_replay {
            self.wake_flow_owner(0);
        }
        true
    }

    fn wake_flow_owner(&self, index: usize) {
        self.unpark(index);
        self.readies.wake_poller();
    }

    /// `batch` is the thief's reusable steal storage; it is empty between calls.
    fn pop(&self, index: usize, batch: &mut Vec<TaskPtr>) -> Option<TaskPtr> {
        if let Some(task) = self.queues[index].lock().unwrap().pop() {
            return Some(task);
        }
        if let Some(task) = self.injector.lock().unwrap().pop_front() {
            return Some(task);
        }
        let cohort = crate::sequencer::cohort();
        self.cohorts[index].store(cohort, Ordering::Relaxed);
        for offset in 1..self.queues.len() {
            let other = (index + offset) % self.queues.len();
            let Ok(mut victim) = self.queues[other].try_lock() else {
                continue;
            };
            // Like hierarchical work stealing, a thief in another L3 takes
            // work only after passing it over once, so a same-L3 thief wins.
            if self.cohorts[other].load(Ordering::Relaxed) != cohort {
                if victim.local.is_empty() && victim.lifo.is_none() {
                    victim.remote_seen = false;
                    continue;
                }
                if !std::mem::replace(&mut victim.remote_seen, true) {
                    continue;
                }
                victim.remote_seen = false;
            }
            if let Some(first) = victim.local.pop_front() {
                let count = victim.local.len() / 2;
                batch.extend(victim.local.drain(..count));
                #[cfg(feature = "testing")]
                if self.measure {
                    self.sample.stolen.fetch_add(1 + count, Ordering::Relaxed);
                }
                drop(victim);
                self.queues[index]
                    .lock()
                    .unwrap()
                    .local
                    .extend(batch.drain(..));
                return Some(first);
            }
            // A LIFO task usually runs next on its own worker, which keeps
            // a spawn-and-join child local. It becomes stealable once a thief
            // has passed it over, so a busy owner cannot strand it.
            if victim.lifo.is_some() {
                if victim.lifo_seen {
                    #[cfg(feature = "testing")]
                    if self.measure {
                        self.sample.stolen.fetch_add(1, Ordering::Relaxed);
                    }
                    victim.lifo_seen = false;
                    return victim.lifo.take();
                }
                victim.lifo_seen = true;
            }
        }
        None
    }
}

pub(crate) trait FlowWork: Send + Sync {
    fn run(self: Arc<Self>, stage: usize);
}

struct FlowJob {
    flow: Arc<dyn FlowWork>,
    stage: usize,
}

/// Count a stage before publishing it. An in-flight predecessor or the owner
/// still owns its unit until every successor has taken one.
pub(crate) fn enqueue_flow(flow: Arc<dyn FlowWork>, stage: usize) {
    // SAFETY: the owner or a stage is running on a scoped pool worker.
    let (pool, source) = unsafe { (&*(*WORKER.get()).pool, (*WORKER.get()).index) };
    pool.units.fetch_add(1, Ordering::AcqRel);
    pool.flow.lock().unwrap().push_back(FlowJob { flow, stage });
    pool.notify_one(source);
    pool.readies.wake_poller();
}

pub(crate) struct FlowWaiter {
    task: TaskPtr,
    worker: usize,
}
impl FlowWaiter {
    pub(crate) fn current() -> Self {
        // SAFETY: the calling owner runs on its pinned pool worker.
        Self {
            task: TaskPtr(CURRENT.get()),
            worker: unsafe { (*WORKER.get()).index },
        }
    }

    pub(crate) fn wake(self) {
        // SAFETY: the owner pins its task and the run until it is resumed.
        // Only the pinned worker consumes the mailbox, after its own switch.
        let pool = unsafe { &*(*WORKER.get()).pool };
        pool.units.fetch_add(1, Ordering::AcqRel);
        pool.queues[self.worker]
            .lock()
            .unwrap()
            .flow_wakes
            .push_back(self.task);
        pool.wake_flow_owner(self.worker);
    }
}

/// The caller registered a waiter while holding the flow lock and released
/// that lock. Its mailbox can be woken before or after this switch.
pub(crate) fn park_flow() {
    let _commit = crate::sequencer::enter();
    let task = CURRENT.get();
    // SAFETY: the active owner's stack is pinned to this worker.
    unsafe {
        (*task).state = State::Flow;
        switch_to_idle(task);
    }
}

/// A transform cannot suspend. It borrows a guarded stack for one call and
/// returns it before the stage can publish its result or release its unit.
pub(crate) fn call_flow<F: FnOnce() -> R, R>(body: F) -> R {
    struct Call<F, R> {
        body: Option<F>,
        result: Option<R>,
        saved: *mut u8,
    }
    unsafe extern "C" fn start<F: FnOnce() -> R, R>(first: *mut u8, _: *mut u8) -> ! {
        // SAFETY: the dispatcher pins the call frame until this switch returns.
        unsafe {
            let call = first.cast::<Call<F, R>>();
            (*call).result = Some((*call).body.take().unwrap()());
            hirsel_stack::switch(&mut ptr::null_mut(), (*call).saved);
            std::hint::unreachable_unchecked()
        }
    }
    // SAFETY: this is the worker's dispatcher, and only it uses its stock.
    unsafe {
        let worker = WORKER.get();
        let stack = (*worker).stock.take();
        let mut call = Call {
            body: Some(body),
            result: None,
            saved: ptr::null_mut(),
        };
        let target = stack.prepare(start::<F, R>, (&raw mut call).cast(), ptr::null_mut());
        crate::fiber::guard(stack.bounds());
        hirsel_stack::switch(&raw mut call.saved, target);
        crate::fiber::guard((0, 0));
        (*worker).stock.recycle(stack);
        call.result.unwrap()
    }
}

#[cfg(feature = "testing")]
#[derive(Default)]
struct PoolSample {
    stolen: AtomicUsize,
    items: AtomicUsize,
    channel_wakes: AtomicUsize,
}

#[cfg(feature = "testing")]
#[derive(Default)]
struct DispatchSample {
    tasks: usize,
    nanoseconds: u64,
}

#[cfg(feature = "testing")]
impl DispatchSample {
    /// # Safety
    /// The dispatch turn owns the newly dequeued task.
    unsafe fn take(&mut self, task: *mut Task) {
        let enqueued = unsafe { std::mem::take(&mut (*task).enqueued_at) };
        if enqueued != 0 {
            self.tasks += 1;
            self.nanoseconds += crate::os::monotonic_ns().saturating_sub(enqueued);
        }
    }

    fn report(&self, index: usize) {
        eprintln!(
            "dispatch worker={index} first_runs={} nanoseconds={}",
            self.tasks, self.nanoseconds
        );
    }
}

fn measurement_enabled() -> bool {
    #[cfg(feature = "testing")]
    return std::env::var_os("HIRSEL_S8_STATS").is_some_and(|value| value == "1");
    #[cfg(not(feature = "testing"))]
    return false;
}

struct Worker {
    index: usize,
    pool: *const Pool,
    idle: *mut u8,
    stock: Stock,
    #[cfg(feature = "testing")]
    dispatch_sample: DispatchSample,
    outbox: Vec<Delivery>,
    /// Steal storage, reused so a steal does not allocate.
    stolen: Vec<TaskPtr>,
    #[cfg(any(test, feature = "testing"))]
    script: Option<Arc<crate::schedule_script::ScheduleScript>>,
}

thread_local! {
    static WORKER: Local<*mut Worker> = const { Local::new(ptr::null_mut()) };
}

fn publish_deliveries() {
    // SAFETY: a worker pins its pool and only it mutates the outbox. The
    // sequencer has just been released. Receiver units are already counted.
    unsafe {
        let worker = &mut *WORKER.get();
        #[cfg(any(test, feature = "testing"))]
        if !worker.outbox.is_empty()
            && let Some(script) = &worker.script
        {
            script.pause(crate::schedule_script::PausePoint::Handoff {
                sender_domain: RESIDUAL_DOMAIN,
                receiver_domain: RESIDUAL_DOMAIN,
            });
        }
        (*worker.pool).publish(worker.index, &mut worker.outbox);
    }
}

#[cfg(test)]
pub(crate) fn wait_for_peer_idle_check() {
    // SAFETY: called by the T1 pause on the sender's dispatcher.
    let pool = unsafe { &*(*WORKER.get()).pool };
    let before = pool.idle_checks.load(Ordering::Acquire);
    pool.wake_workers();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while pool.idle_checks.load(Ordering::Acquire) == before {
        assert!(
            std::time::Instant::now() < deadline,
            "peer never rechecked quiescence"
        );
        std::thread::yield_now();
    }
}

/// Use the worker's idle stack after publishing a park under its commit.
/// The dispatcher adopts the token before it inspects or recycles the stack.
unsafe fn switch_to_idle(task: *mut Task) {
    let worker = WORKER.get();
    // SAFETY: the running task is pinned here, and only this worker owns the
    // idle stack. N=1 keeps the existing direct switch.
    unsafe {
        if worker.is_null() {
            hirsel_stack::switch(&raw mut (*task).sp, (*(*task).run).idle);
        } else {
            let _commit = crate::sequencer::enter();
            crate::sequencer::to_dispatcher(|| {
                hirsel_stack::switch(&raw mut (*task).sp, (*worker).idle);
            });
        }
    }
}

fn worker_stock(run: *mut Scheduler) -> *mut Stock {
    let worker = WORKER.get();
    // SAFETY: both alternatives are pinned and only this OS thread uses
    // the chosen stock. Raw address formation creates no shared borrow.
    unsafe {
        if worker.is_null() {
            &raw mut (*run).stacks
        } else {
            &raw mut (*worker).stock
        }
    }
}

struct Scheduler {
    pool: Option<NonNull<Pool>>,
    deferred: *mut Task,
    deferred_tail: *mut Task,
    stacks: Stock,
    idle: *mut u8,
    head: *mut Task,
    tail: *mut Task,
    timers: *mut Task,
    all: *mut Task,
    next: u64,
    provider: SchedulerProvider,
    #[cfg(any(test, feature = "testing"))]
    script: Option<Arc<crate::schedule_script::ScheduleScript>>,
    #[cfg(any(test, feature = "testing"))]
    script_groups: Vec<crate::tape::schedule::Group>,
    readies: Arc<Readies>,
    ready_tokens: Vec<u64>,
    ready_arms: Vec<ReadyArm>,
    #[cfg(feature = "testing")]
    measure: bool,
    /// Cancellation requests for tasks parked in a provider operation, in
    /// request order, taken at the next scheduler turn.
    cancels: Vec<*mut Task>,
    outside_cancels: Vec<*mut Task>,
    tapes: Tapes,
    net: *mut hirsel_core_net::Sockets,
    model: Option<Arc<dyn ModelClock>>,
    parked: WaitArms<*mut Task>,
}
const _: () =
    assert!(std::mem::offset_of!(Task, arc_runtime) == crate::abi_layout::TASK_ARC_REPLAY);
const _: () = assert!(std::mem::offset_of!(Task, arc_hooks) == crate::abi_layout::TASK_ARC_HOOKS);

#[cfg(test)]
pub(crate) unsafe fn task_stack(task: *const Task) -> Option<usize> {
    // SAFETY: the test owns the live task handle and the task is off-CPU.
    unsafe { (*task).stack.as_ref().map(|stack| stack.bounds().0) }
}

#[cfg(test)]
pub(crate) unsafe fn task_waits_for_provider(task: *const Task) -> bool {
    let _commit = crate::sequencer::enter();
    // SAFETY: the fixture owns this live handle throughout the check.
    unsafe { (*task).state == State::Provider }
}

#[cfg(test)]
pub(crate) unsafe fn task_waits_for_join(task: *const Task) -> bool {
    let _commit = crate::sequencer::enter();
    // SAFETY: the child runs while its supervisor is live.
    unsafe { (*task).state == State::Waiting }
}

impl Scheduler {
    /// Whether something beyond a task's own tape observes Arc count events:
    /// the provider's event sink or a test schedule.
    fn arc_observed(&self) -> bool {
        #[cfg(any(test, feature = "testing"))]
        if self.script.is_some() {
            return true;
        }
        self.provider.event.is_some()
    }

    /// Whether `task`'s inline Arc count events must take their hook even
    /// with no posted credit: its tape records them, an open LATEST pair
    /// closes on them, something else observes them, or a serial run with
    /// sockets polls the network at every event.
    fn arc_hooks(&self, task: &Task) -> bool {
        task.group.is_some()
            || task.latest_pair
            || self.arc_observed()
            || (self.pool.is_none() && !self.net.is_null())
    }

    fn id(&mut self) -> u64 {
        let next = self.next;
        self.next = self
            .next
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"TaskIds"));
        next
    }
    unsafe fn ready(&mut self, task: *mut Task) {
        // SAFETY: scheduler owns task; only unqueued tasks can enter the run queue.
        unsafe {
            (*task).state = State::Ready;
            if self.pool.is_some() {
                self.publish(task, false);
                return;
            }
            (*task).ready = ptr::null_mut();
            if self.tail.is_null() {
                self.head = task;
            } else {
                (*self.tail).ready = task;
            }
            self.tail = task;
        }
    }
    unsafe fn publish(&self, task: *mut Task, deferred: bool) {
        // SAFETY: the active worker holds the task's domain commit, its pool
        // and TLS outbox are pinned, and the task has no other ready unit.
        unsafe {
            let pool = self.pool.unwrap().as_ref();
            let worker = &mut *WORKER.get();
            let domain = &pool.domains[&(*task).domain];
            domain.ready.fetch_add(1, Ordering::Relaxed);
            domain.handoffs.fetch_add(1, Ordering::Relaxed);
            pool.units.fetch_add(1, Ordering::Relaxed);
            worker.outbox.push(Delivery {
                task: TaskPtr(task),
                worker: (*task).worker,
                deferred,
            });
        }
    }

    unsafe fn pop(&mut self) -> *mut Task {
        // SAFETY: only the idle scheduler manipulates the ready queues.
        unsafe {
            for entry in &self.tapes.live {
                let group = entry.group;
                use crate::tape::schedule::Turn;
                let wanted = match group.turn() {
                    Turn::Wait(id) | Turn::Event(Some(id)) => Some(id),
                    Turn::Grant | Turn::Finish => Some(0),
                    _ => None,
                };
                if let Some(id) = wanted {
                    let target = self.group_task(group, id);
                    if !target.is_null() && (*target).state == State::Ready {
                        return self.unlink_ready(target);
                    }
                }
            }
            // The FIFO identity retains the old ready order, including the
            // deferred queue for pollers.
            if self.head.is_null() {
                self.head = std::mem::replace(&mut self.deferred, ptr::null_mut());
                self.tail = std::mem::replace(&mut self.deferred_tail, ptr::null_mut());
            }
            if self.head.is_null() {
                return ptr::null_mut();
            }
            #[cfg(any(test, feature = "testing"))]
            if let Some(script) = self.script.clone() {
                let selected = self.scripted_task(&script);
                return self.unlink_ready(selected);
            }
            self.unlink_ready(self.head)
        }
    }

    #[cfg(any(test, feature = "testing"))]
    unsafe fn scripted_task(
        &mut self,
        script: &crate::schedule_script::ScheduleScript,
    ) -> *mut Task {
        // SAFETY: pop holds the dispatcher and head is ready.
        unsafe {
            if let Some(group) = (*self.head).group {
                if group.replaying() || group.fifo() {
                    return self.head;
                }
                let domain = self
                    .script_groups
                    .iter()
                    .position(|g| g.same(group))
                    .unwrap_or_else(|| {
                        self.script_groups.push(group);
                        self.script_groups.len() - 1
                    });
                let live = self.live_group_tasks(group);
                let runnable: Vec<_> = live
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &(_, task))| {
                        ((*task).state == State::Ready).then_some(index)
                    })
                    .collect();
                return live[script.choose_task(domain as u64, live.len(), &runnable)].1;
            }

            let mut runnable = Vec::new();
            let mut task = self.head;
            while !task.is_null() {
                if (*task).group.is_none() {
                    runnable.push(task);
                }
                task = (*task).ready;
            }
            let indices: Vec<_> = (0..runnable.len()).collect();
            let selected =
                script.choose_task(crate::schedule_script::UNTAPED, runnable.len(), &indices);
            runnable[selected]
        }
    }

    unsafe fn live_group_tasks(
        &self,
        group: crate::tape::schedule::Group,
    ) -> Vec<(u64, *mut Task)> {
        let mut live = Vec::new();
        let mut task = self.all;
        // SAFETY: the dispatcher owns the creation list.
        unsafe {
            while !task.is_null() {
                if (*task).state != State::Ended
                    && let Some(member) = (*task).group
                    && member.same(group)
                {
                    live.push((member.task, task));
                }
                task = (*task).all;
            }
        }
        live.sort_unstable_by_key(|&(id, _)| id);
        live
    }

    unsafe fn group_task(&self, group: crate::tape::schedule::Group, id: u64) -> *mut Task {
        let mut task = self.all;
        // SAFETY: scheduler owns the creation list; retired tasks are unlinked.
        unsafe {
            while !task.is_null() {
                if (*task).group.is_some_and(|g| g.same(group) && g.task == id) {
                    return task;
                }
                task = (*task).all;
            }
        }
        ptr::null_mut()
    }

    unsafe fn unlink_ready(&mut self, task: *mut Task) -> *mut Task {
        // SAFETY: Ready is queued in one of these two lists; idle owns both.
        unsafe {
            for (head, tail) in [
                (&mut self.head, &mut self.tail),
                (&mut self.deferred, &mut self.deferred_tail),
            ] {
                let mut link = head as *mut *mut Task;
                let mut before = ptr::null_mut();
                while !(*link).is_null() {
                    if *link == task {
                        *link = (*task).ready;
                        if *tail == task {
                            *tail = before;
                        }
                        (*task).ready = ptr::null_mut();
                        return task;
                    }
                    before = *link;
                    link = &raw mut (**link).ready;
                }
            }
        }
        unreachable!("ready task has lost its queue");
    }

    /// A pending request moves only its registered target, and only if the
    /// cancellation point is allowed to observe it.
    unsafe fn wake_cancelled(&mut self, task: *mut Task) {
        // SAFETY: the target's wait and context are pinned until dequeue.
        unsafe {
            if (*task).state == State::Waiting
                && cancel_takes_effect(&*(*task).context, cancel_flag(task))
            {
                Waiters::remove(task);
                self.ready(task);
            }
        }
    }

    unsafe fn insert_timer(&mut self, task: *mut Task, deadline: u64) {
        // SAFETY: the task is exclusively parked and is not in the timer list.
        unsafe {
            (*task).timer_deadline = Some(deadline);
            let mut link = &raw mut self.timers;
            while !(*link).is_null() && (*(*link)).timer_deadline.is_some_and(|at| at <= deadline) {
                link = &raw mut (**link).timer_next;
            }
            (*task).timer_next = *link;
            *link = task;
        }
    }

    unsafe fn remove_timer(&mut self, task: *mut Task) {
        // SAFETY: only the idle scheduler mutates timer links.
        unsafe {
            if (*task).timer_deadline.is_none() {
                return;
            }
            let mut link = &raw mut self.timers;
            while !(*link).is_null() && *link != task {
                link = &raw mut (**link).timer_next;
            }
            if *link == task {
                *link = (*task).timer_next;
            }
            (*task).timer_next = ptr::null_mut();
            (*task).timer_deadline = None;
        }
    }

    /// One scheduler turn's wakes, before any pop: cancellation requests, then
    /// readiness, then expired timers.
    ///
    /// A cancellation request takes effect at its CANCEL event, which the
    /// requesting task emitted before this turn. A ready takes effect at the
    /// turn that takes it: that is the wake's moment (REPLAY-SPEC section 3,
    /// step 3), wherever and whenever `hirsel_core_ready` was called. So every
    /// request recorded before the turn precedes every ready taken at it, and a
    /// cancel and a ready pending together (with or without events between
    /// them) resolve to the cancellation: W fails Cancelled with no issue, I
    /// aborts with Cancel. The order is a function of the recorded events and
    /// the turn, so replay reproduces it. A ready taken at an earlier turn has
    /// already woken the operation, and a later request applies at the task's
    /// next cancellation point (CONC-7). Readiness before timers is the network
    /// rule that readiness wins a tie with a deadline (net-proposal-999 D3).
    unsafe fn capture_wakes(&mut self, captured: &mut Vec<ReadyArm>, net_timeout: i32) {
        if let Some(take_inputs) = self.tapes.take_inputs {
            // SAFETY: idle owns every task and operation frame.
            unsafe { take_inputs(self) };
        }
        self.poll_net(net_timeout);
        // SAFETY: no task is running; every parked operation's frame is pinned.
        unsafe {
            if !self.cancels.is_empty() {
                for task in std::mem::take(&mut self.cancels) {
                    if (*task).state == State::Provider {
                        self.wake_operation(task, Wake::Cancel);
                    }
                }
            }
        }
        if self.readies.pending.swap(false, Ordering::Acquire) {
            std::mem::swap(&mut self.ready_tokens, &mut *self.readies.lock());
            self.parked.capture(self.ready_tokens.drain(..), captured);
            if self.ready_tokens.capacity() > RETAINED_READY_CAPACITY {
                self.ready_tokens = Vec::new();
            }
        }
    }

    unsafe fn apply_ready(&mut self, ready: ReadyArm) {
        if let Some(task) = self.parked.take_captured(ready) {
            // SAFETY: the captured identity still names this parked frame.
            unsafe { self.wake_operation(task, Wake::Ready) };
        }
    }

    unsafe fn take_timers(&mut self) {
        // SAFETY: this turn owns the timer list and pinned operation frames.
        unsafe {
            if self.timers.is_null() {
                return;
            }
            let now = self.now();
            // Detach every expired timer first: a wait that answers Until(now)
            // again re-arms for the next turn, so the other tasks still run.
            let mut expired = ptr::null_mut::<Task>();
            let mut tail = &raw mut expired;
            while !self.timers.is_null()
                && (*self.timers)
                    .timer_deadline
                    .is_some_and(|deadline| deadline <= now)
            {
                let task = self.timers;
                self.timers = (*task).timer_next;
                (*task).timer_next = ptr::null_mut();
                (*task).timer_deadline = None;
                *tail = task;
                tail = &raw mut (*task).timer_next;
            }
            while !expired.is_null() {
                let task = expired;
                expired = (*task).timer_next;
                (*task).timer_next = ptr::null_mut();
                self.wake_operation(task, Wake::Timer);
            }
        }
    }

    unsafe fn take_wakes(&mut self, net_timeout: i32) {
        // SAFETY: the cooperative scheduler owns this whole dispatch turn.
        unsafe {
            let mut captured = std::mem::take(&mut self.ready_arms);
            self.capture_wakes(&mut captured, net_timeout);
            for ready in captured.drain(..) {
                #[cfg(any(test, feature = "testing"))]
                if let Some(script) = &self.script {
                    script.pause(crate::schedule_script::PausePoint::ReadinessTaken {
                        token: ready.token,
                    });
                }
                self.apply_ready(ready);
            }
            if captured.capacity() <= RETAINED_READY_CAPACITY {
                self.ready_arms = captured;
            }
            self.take_timers();
        }
    }

    unsafe fn take_tape_inputs(&mut self) {
        // SAFETY: idle owns tasks; retirement removes queued requests.
        unsafe {
            for task in std::mem::take(&mut self.outside_cancels) {
                if let Some(group) = (*task).group
                    && group.cancel(None)
                {
                    cancel_flag(task).store(true, Ordering::Release);
                    if (*task).state == State::Provider {
                        self.cancels.push(task);
                    }
                    self.wake_cancelled(task);
                }
            }
            for index in 0..self.tapes.live.len() {
                let group = self.tapes.live[index].group;
                use crate::tape::schedule::Turn;
                loop {
                    let turn = group.turn();
                    let id = match turn {
                        Turn::Wake(id) | Turn::Cancel(id) | Turn::Done(id) => id,
                        Turn::Choose(index) => {
                            let live = self.live_group_tasks(group);
                            match usize::try_from(index).ok().and_then(|i| live.get(i)) {
                                Some(&(id, at)) if (*at).state == State::Ready => group.choose(id),
                                Some(&(_, at)) if (*at).state == State::Flow => {}
                                _ => group.fail(),
                            }
                            break;
                        }
                        _ => break,
                    };
                    let target = self.group_task(group, id);
                    if target.is_null() {
                        group.fail();
                        break;
                    }
                    match turn {
                        Turn::Cancel(_) if (*target).state != State::Ended => {
                            group.consume_input();
                            cancel_flag(target).store(true, Ordering::Release);
                            if (*target).state == State::Provider {
                                self.wake_operation(target, Wake::Cancel);
                            }
                            self.wake_cancelled(target);
                        }
                        Turn::Wake(_)
                            if (*target).state == State::Provider
                                && matches!(
                                    (*(*target).operation).state,
                                    OperationState::Waiting
                                ) =>
                        {
                            group.consume_input();
                            group.cancel_wait((*(*target).operation).request);
                            self.runnable(target, OperationState::ReadyToIssue);
                        }
                        Turn::Done(_)
                            if (*target).state == State::Provider
                                && matches!(
                                    (*(*target).operation).state,
                                    OperationState::Issued { .. }
                                ) =>
                        {
                            self.wake_operation(target, Wake::Ready);
                        }
                        _ => {
                            group.fail();
                            break;
                        }
                    }
                }
            }
        }
    }

    fn has_outside_wait(&self) -> bool {
        // SAFETY: each entry points to its pinned provider operation.
        self.parked.live.values().any(|arm| unsafe {
            let task = arm.task;
            !(*task)
                .group
                .is_some_and(|group| group.drives_replay((*(*task).operation).provider))
        })
    }

    fn stop_quiescent(&mut self) -> ! {
        if let Some((id, fault)) = self.tapes.seal_deadlock() {
            let tape = u128::from_be_bytes(id);
            let verdict = match fault {
                crate::tape::Fault::Diverged(divergence) => {
                    format!(
                        "Replay tape {tape:032x}: Diverged({:?}) at {}",
                        divergence.kind, divergence.at
                    )
                }
                crate::tape::Fault::Truncated(at) => {
                    format!("Replay tape {tape:032x}: Truncated at {at}")
                }
                crate::tape::Fault::Damaged(block) => {
                    format!("Replay tape {tape:032x}: Damaged block {block}")
                }
                crate::tape::Fault::Corrupt => format!("Replay tape {tape:032x}: Corrupt"),
                fault => format!("Tape {tape:032x}: {fault}"),
            };
            crate::process::stop(verdict.as_bytes());
        }
        crate::process::stop(b"Deadlock");
    }

    fn now(&self) -> u64 {
        self.model
            .as_ref()
            .map_or_else(crate::os::monotonic_ns, |m| m.now())
    }
    fn poll_net(&mut self, timeout: i32) {
        if let Some(model) = &self.model {
            // Readiness is taken before timers by take_wakes, including ties.
            let deadline = unsafe { self.timers.as_ref().and_then(|t| t.timer_deadline) };
            for token in model.poll(deadline, timeout != 0) {
                self.readies.push(token);
            }
            return;
        }
        if !self.net.is_null() {
            let readies = &self.readies;
            let notify = self.pool.is_some();
            // SAFETY: the run pins its shared registry; no task runs during polling.
            unsafe {
                (*self.net).poll(timeout, |token| {
                    if notify {
                        readies.push(token);
                    } else {
                        // The only dispatcher is already awake.
                        readies.enqueue(token);
                    }
                })
            }
            .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
        }
    }

    fn idle(&mut self, flow_pending: bool) {
        if self.readies.pending.load(Ordering::Acquire) || !self.cancels.is_empty() {
            return;
        }
        // SAFETY: no task is running while the idle scheduler waits.
        let deadline = unsafe { self.timers.as_ref().and_then(|task| task.timer_deadline) };
        if self.model.is_some() {
            let now = self.now();
            self.poll_net(-1);
            if !self.readies.pending.load(Ordering::Acquire)
                && self.now() == now
                && deadline.is_none_or(|at| at > now)
                && !flow_pending
            {
                self.stop_quiescent();
            }
            return;
        }
        // The scheduler thread waits here, never in a provider: readiness from
        // any thread unparks it (hirsel_core_ready).
        match deadline {
            Some(deadline) => {
                if !self.net.is_null() {
                    self.poll_net(poll_timeout(Some(deadline)));
                } else {
                    std::thread::park_timeout(Duration::from_nanos(
                        deadline.saturating_sub(crate::os::monotonic_ns()),
                    ));
                }
            }
            None => {
                if !self.has_outside_wait() && !flow_pending {
                    self.stop_quiescent();
                }
                if !self.net.is_null() {
                    self.poll_net(-1);
                } else {
                    std::thread::park();
                }
            }
        }
    }

    unsafe fn runnable(&mut self, task: *mut Task, state: OperationState) {
        // SAFETY: the task is parked; its operation frame is pinned.
        unsafe {
            self.parked.remove((*(*task).operation).token);
            self.remove_timer(task);
            (*(*task).operation).state = state;
            self.ready(task);
        }
    }

    /// Run the parked operation's callback for one wake, at that moment and
    /// outside any task. Returns true only when an operation that has just
    /// entered W is ready at once: the task continues to issue without a
    /// scheduling point, exactly as if it had not parked.
    unsafe fn wake_operation(&mut self, task: *mut Task, wake: Wake) -> bool {
        // SAFETY: called only by the idle scheduler with no current task; the
        // operation frame lives on the parked task's stack until it delivers.
        unsafe {
            debug_assert!(CURRENT.get().is_null());
            #[cfg(test)]
            if wake == Wake::Ready {
                ROUTE_WAKES.set(ROUTE_WAKES.get() + 1);
            }
            // A request that may not take effect now changes nothing: no wait,
            // no abort, no issue skipped. It stays pending for the task's next
            // cancellation point (CONC-7, FAIL-11, CONC-8).
            let op = (*task).operation;
            if wake == Wake::Cancel && !(*op).observes_cancel {
                return false;
            }
            let _callback = CallbackTask::new(task);
            let p = (*op).provider;
            let (request, token) = ((*op).request, (*op).token);
            match (*op).state {
                OperationState::ReadyToIssue | OperationState::ReadyToDeliver(_) => false,
                OperationState::Waiting => {
                    let starting = wake == Wake::Parked;
                    let w = match (*op).entered.take() {
                        Some(w) => w,
                        None => {
                            #[cfg(test)]
                            if starting {
                                FIRST_WAITS.set((FIRST_WAITS.get().0, FIRST_WAITS.get().1 + 1));
                            }
                            (p.wait)(p.state, request, token)
                        }
                    };
                    // A cancel in W still runs wait at the wake (REPLAY-SPEC §3).
                    // Cancellation wins even if the callback returns Ready.
                    if wake == Wake::Cancel {
                        if let Some(group) = (*task).group {
                            group.cancel_wait(request);
                        }
                        self.runnable(task, OperationState::ReadyToDeliver(None));
                        return false;
                    }
                    match w.kind {
                        WAIT_READY if starting => {
                            (*op).state = OperationState::ReadyToIssue;
                            return true;
                        }
                        WAIT_READY => {
                            if let Some(group) = (*task).group {
                                group.wake(request);
                            }
                            self.runnable(task, OperationState::ReadyToIssue);
                            return false;
                        }
                        WAIT_UNTIL => {
                            self.remove_timer(task);
                            self.insert_timer(task, w.deadline_ns);
                        }
                        WAIT_PARK => self.remove_timer(task),
                        _ => crate::process::stop(b"Provider"),
                    }
                    self.parked.insert(token, task);
                    if starting {
                        self.settle(task);
                    }
                    false
                }
                OperationState::Issued { .. } => {
                    match wake {
                        Wake::Parked => self.settle(task),
                        Wake::Ready => {
                            let step = (p.resume)(p.state, request, token);
                            self.stepped(task, step);
                        }
                        Wake::Cancel | Wake::Timer => {
                            let reason = if wake == Wake::Cancel {
                                ABORT_CANCEL
                            } else {
                                ABORT_DEADLINE
                            };
                            let outcome = (p.abort)(p.state, request, token, reason);
                            self.runnable(task, OperationState::ReadyToDeliver(Some(outcome)));
                        }
                    }
                    false
                }
            }
        }
    }

    /// An issue or resume answered: Done delivers, Pending stays in I with the
    /// step's own deadline (0 is none).
    unsafe fn stepped(&mut self, task: *mut Task, step: Step) {
        // SAFETY: as wake_operation.
        unsafe {
            match step.kind {
                STEP_DONE => {
                    self.runnable(task, OperationState::ReadyToDeliver(Some(step.outcome)))
                }
                STEP_PENDING => {
                    self.parked.insert((*(*task).operation).token, task);
                    self.remove_timer(task);
                    (*(*task).operation).state = OperationState::Issued {
                        deadline: step.deadline_ns,
                    };
                    if step.deadline_ns != 0 {
                        self.insert_timer(task, step.deadline_ns);
                    }
                }
                _ => crate::process::stop(b"Provider"),
            }
        }
    }

    /// A cancellation request made while the operation was not parked applies
    /// at its park; readies are taken at the next turn, before it.
    unsafe fn settle(&mut self, task: *mut Task) {
        // SAFETY: as wake_operation.
        unsafe {
            let op = (*task).operation;
            if (*op).observes_cancel && cancel_flag(task).load(Ordering::Acquire) {
                self.wake_operation(task, Wake::Cancel);
            }
            if let OperationState::Issued { deadline } = (*op).state
                && deadline != 0
                && (*task).timer_deadline.is_none()
            {
                self.insert_timer(task, deadline);
            }
        }
    }
}

/// One provider operation, stored on its task's pinned stack. Each transition
/// owns the right to issue, resume, abort or deliver through this state.
pub(crate) struct Operation {
    provider: crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    state: OperationState,
    /// A parked fiber cannot change its shield or operation kind.
    observes_cancel: bool,
    /// The first W answer when the fiber called wait before it parked.
    entered: Option<crate::provider::Wait>,
}
#[derive(Clone, Copy)]
enum OperationState {
    Waiting,
    ReadyToIssue,
    Issued {
        deadline: u64,
    },
    /// A missing outcome is the unissued operation's cancellation.
    ReadyToDeliver(Option<crate::provider::Outcome>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wake {
    /// The task has just parked its operation.
    Parked,
    Ready,
    Cancel,
    Timer,
}
use crate::provider::{
    ABORT_CANCEL, ABORT_DEADLINE, STEP_DONE, STEP_PENDING, Step, WAIT_PARK, WAIT_READY, WAIT_UNTIL,
};
/// # Safety
/// A live task pins this flag. For a parallel task it lives outside the
/// exclusively mutable Context, so cancellation need not borrow any of it.
unsafe fn cancel_flag<'a>(task: *const Task) -> &'a AtomicBool {
    // SAFETY: the caller pins the task and requests never borrow its Context.
    unsafe {
        if (*(*task).run).pool.is_some() {
            &(*task).cancel
        } else {
            &(*(*task).context).cancel_requested
        }
    }
}

/// Whether a cancellation request takes effect on this task now. A finalizer
/// and a shielded region observe none (CONC-7, tasks.md:129; FAIL-11,
/// failure.md:173; CONC-8, tasks.md:140): under either the request stays
/// pending and changes nothing, and it takes effect at the first cancellation
/// point after both end. A deadline is not cancellation and is not shielded.
#[inline(always)]
fn cancel_takes_effect(context: &Context, requested: &AtomicBool) -> bool {
    requested.load(Ordering::Acquire) && context.finalizing == 0 && context.cancel_shield == 0
}

/// Readiness tokens from `hirsel_core_ready`, which may run on any thread.
struct Readies {
    tokens: Mutex<Vec<u64>>,
    pending: AtomicBool,
    worker: std::thread::Thread,
    peers: Mutex<Vec<std::thread::Thread>>,
    net: Mutex<Option<Arc<hirsel_core_net::Poller>>>,
    model: Mutex<Option<Arc<dyn ModelClock>>>,
}
impl Readies {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(Vec::new()),
            pending: AtomicBool::new(false),
            worker: std::thread::current(),
            peers: Mutex::default(),
            net: Mutex::new(None),
            model: Mutex::new(None),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<u64>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn enqueue(&self, token: u64) {
        self.lock().push(token);
        self.pending.store(true, Ordering::Release);
    }
    fn push(&self, token: u64) {
        self.enqueue(token);
        self.worker.unpark();
        for peer in self.peers.lock().unwrap().iter() {
            peer.unpark();
        }
        self.wake_poller();
    }
    fn wake_poller(&self) {
        if let Some(net) = self.net.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            net.wake();
        }
    }
    /// Take `token` if it was readied; other tokens are not this waiter's.
    fn take(&self, token: u64) -> bool {
        self.pending.store(false, Ordering::Release);
        let mut tokens = self.lock();
        let found = tokens.contains(&token);
        tokens.clear();
        found
    }
}
/// Every live waiter: each run's scheduler, and a scheduler-less operation
/// while it waits. A token is unique in the process, so a ready reaches every
/// waiter and only the one holding that operation acts on it.
static WAITERS: Mutex<Vec<Arc<Readies>>> = Mutex::new(Vec::new());
struct Registered(Arc<Readies>);
impl Registered {
    fn new() -> Self {
        let readies = Readies::new();
        WAITERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(readies.clone());
        Self(readies)
    }
}
impl Drop for Registered {
    fn drop(&mut self) {
        WAITERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|r| !Arc::ptr_eq(r, &self.0));
    }
}
static TOKENS: AtomicU64 = AtomicU64::new(1);
thread_local! { static ALONE: Registered = Registered::new(); }

/// A provider readies the Park or Pending operation `token`. It may be called on
/// any thread, from inside a callback or a task: it only records the token, and
/// the operation's scheduler takes it at its next turn, after that turn's
/// cancellation requests and before any task is popped. Taken in W it calls `wait`; in I it calls `resume`. Readies of one
/// token in one turn are one wake. A token that is unknown, whose operation has
/// delivered, or whose operation is queued or running (between parks) is
/// dropped; a ready given while `issue` runs is taken after the operation parks
/// in I, so `resume` follows it. `resume` may answer Pending again.
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_ready(token: u64) {
    for waiter in WAITERS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        waiter.push(token);
    }
}

/// W and I of one operation (REPLAY-SPEC section 3, driver steps 2 to 6). Runs
/// in the task; `wait`, `resume` and `abort` run with no task current, and
/// `deliver` once. Returns what `deliver` returned and records in `issued`
/// what the provider produced; or returns Cancelled, recording nothing, when
/// the operation was cancelled in W.
///
/// The first `wait` is made here, on the task's stack, with no task current,
/// and not by switching to the scheduler and back. The two are the same call:
/// a wait at entry happens at the task's own time, not at a wake. Under the
/// switch the scheduler calls `wait` straight after the task parks, before it
/// takes any wake or pops any task, and on Ready it switches straight back to
/// the same task. So between entry and the call no other task runs, no event
/// is emitted, and no ready, cancellation request or timer is taken. The
/// callback sees no task (`CURRENT` is null in both), the same thread, the
/// same request and token, and the same event position, so the OS provider, a
/// wrapper and a tape that records waits all observe and record the same
/// thing. Resuming the task re-binds the payer it already had bound, a no-op.
/// What differs is only the stack the callback runs on: the task's own stack,
/// as for `issue`, which already runs there. An Until or Park answer parks the
/// task with the answer, and the scheduler applies it exactly as it would have
/// applied its own call's answer (`Operation::entered`).
/// `provider_s0_tests::s0_direct_entry_wait_matches_the_scheduler_switch`
/// compares the two paths.
/// # Safety
/// The provider is installed; request is pinned through delivery.
#[inline(always)]
pub(crate) unsafe fn provider_operation(
    context: *mut Context,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> crate::provider::Outcome {
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    if token == u64::MAX {
        crate::process::stop(b"HostHandles");
    }
    let task = CURRENT.get();
    // SAFETY: provider contract; request pinned by the caller through delivery.
    unsafe {
        if task.is_null() {
            return slow(
                context,
                task,
                provider,
                request,
                token,
                Start::Alone,
                issued,
            );
        }
        #[cfg(test)]
        if SCHEDULER_ENTRY.get() {
            let start = Start::Entry(None);
            return slow(context, task, provider, request, token, start, issued);
        }
        #[cfg(test)]
        FIRST_WAITS.set((FIRST_WAITS.get().0 + 1, FIRST_WAITS.get().1));
        // Identity may select a call form, never behaviour: this branch selects
        // nothing but which `Calls` the one body `operation` is instantiated
        // with, so a provider that forwards to the OS provider is
        // indistinguishable from it (REPLAY-SPEC section 3, as amended by the
        // orchestrator's 2026-11-16 ruling on review 1025 F2).
        if own_callbacks(
            ptr::fn_addr_eq(provider.wait, crate::os::WAIT)
                && ptr::fn_addr_eq(provider.deliver, crate::provider::DELIVER),
        ) {
            operation::<OsCalls>(context, task, provider, request, token, issued)
        } else {
            operation::<PointerCalls>(context, task, provider, request, token, issued)
        }
    }
}
/// The fast path of `provider_operation` with a scheduler: one body for every
/// provider, generic only over how it calls `wait` and `deliver`.
/// # Safety
/// As `provider_operation`; `task` is the current task.
#[inline(always)]
unsafe fn operation<C: Calls>(
    context: *mut Context,
    task: *mut Task,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> crate::provider::Outcome {
    // SAFETY: provider contract; request pinned by the caller through delivery.
    unsafe {
        CALLBACK_TASK.set(task);
        CURRENT.set(ptr::null_mut());
        let wait = C::wait(provider, request, token);
        CURRENT.set(task);
        CALLBACK_TASK.set(ptr::null_mut());
        if wait.kind != WAIT_READY {
            let start = Start::Entry(Some(wait));
            return slow(context, task, provider, request, token, start, issued);
        }
        let step = (provider.issue)(provider.state, request, token);
        if step.kind != STEP_DONE {
            let start = Start::Issued(step);
            return slow(context, task, provider, request, token, start, issued);
        }
        if let Some(issued) = issued {
            *issued = Some(step.outcome);
        }
        C::deliver(provider, request, step.outcome)
    }
}
/// How `operation` calls a provider's `wait` and `deliver`. A form may contain
/// nothing but the call: the same function, with the same arguments.
trait Calls {
    /// # Safety
    /// The provider's `wait` contract.
    unsafe fn wait(
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        token: u64,
    ) -> crate::provider::Wait;
    /// # Safety
    /// The provider's `deliver` contract.
    unsafe fn deliver(
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        outcome: crate::provider::Outcome,
    ) -> crate::provider::Outcome;
}
/// Through the provider's function pointers.
struct PointerCalls;
impl Calls for PointerCalls {
    #[inline(always)]
    unsafe fn wait(
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        token: u64,
    ) -> crate::provider::Wait {
        // SAFETY: forwarded.
        unsafe { (provider.wait)(provider.state, request, token) }
    }
    #[inline(always)]
    unsafe fn deliver(
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        outcome: crate::provider::Outcome,
    ) -> crate::provider::Outcome {
        // SAFETY: forwarded.
        unsafe { (provider.deliver)(provider.state, request, outcome) }
    }
}
/// Directly, for a provider whose `wait` and `deliver` are the OS provider's
/// own, so they inline.
struct OsCalls;
impl Calls for OsCalls {
    #[inline(always)]
    unsafe fn wait(
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        token: u64,
    ) -> crate::provider::Wait {
        // SAFETY: forwarded; `provider.wait` is this function.
        unsafe { crate::os::wait(provider.state, request, token) }
    }
    #[inline(always)]
    unsafe fn deliver(
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        outcome: crate::provider::Outcome,
    ) -> crate::provider::Outcome {
        // SAFETY: forwarded; `provider.deliver` is this function.
        unsafe { crate::provider::deliver(provider.state, request, outcome) }
    }
}
/// Whether `provider_operation` calls the OS provider's own callbacks directly
/// (`OsCalls`). Test builds can force the pointer calls (`OS_DIRECT`) and count
/// the direct operations (`OS_DIRECT_CALLS`);
/// `provider_s0_tests::s0_os_callbacks_by_identity_match_the_pointer_calls`
/// compares both forms with a forwarding wrapper.
#[inline(always)]
pub(crate) fn own_callbacks(identical: bool) -> bool {
    #[cfg(test)]
    {
        let direct = identical && OS_DIRECT.get();
        if direct {
            OS_DIRECT_CALLS.set(OS_DIRECT_CALLS.get() + 1);
        }
        direct
    }
    #[cfg(not(test))]
    identical
}
/// An operation that does not finish at once: it parks, or has no scheduler.
/// The provider is copied before the first park.
#[cold]
#[inline(never)]
unsafe fn slow(
    context: *mut Context,
    task: *mut Task,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    start: Start,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> crate::provider::Outcome {
    let provider = *provider;
    // SAFETY: forwarded.
    unsafe {
        let outcome = match start {
            Start::Alone => alone(context, &provider, request, token),
            start => parked(task, &provider, request, token, start),
        };
        let Some(outcome) = outcome else {
            // The scheduler failed the operation in W because a request is
            // pending and unshielded; observing it records the cause.
            let observed = check_cancel(context, (*request).site);
            debug_assert_eq!(observed, 1);
            return crate::provider::CANCELLED;
        };
        if let Some(issued) = issued {
            *issued = Some(outcome);
        }
        (provider.deliver)(provider.state, request, outcome)
    }
}
/// Where an operation that does not finish at once leaves the fast path.
enum Start {
    /// No scheduler: the thread waits itself (`alone`).
    Alone,
    /// Its first `wait` answered Until or Park (Some), or, test-only, the
    /// scheduler is to make that call (None).
    Entry(Option<crate::provider::Wait>),
    /// `issue` answered this step, which is not Done.
    Issued(crate::provider::Step),
}
/// The rest of an operation that parks: in W after its first wait, or in I
/// after its issue. The provider is copied before the first park.
#[cold]
#[inline(never)]
unsafe fn parked(
    task: *mut Task,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    start: Start,
) -> Option<crate::provider::Outcome> {
    let mut op = Operation {
        provider: *provider,
        request,
        token,
        state: OperationState::Waiting,
        // SAFETY: the current fiber owns the context and the request.
        observes_cancel: unsafe {
            crate::provider::cancellation_point((*request).operation)
                && (*(*task).context).cancel_shield == 0
                && (*(*task).context).finalizing == 0
        },
        entered: None,
    };
    // SAFETY: the frame outlives every park; the scheduler clears no field.
    unsafe {
        let mut step = match start {
            Start::Entry(entered) => {
                op.entered = entered;
                (*task).operation = &mut op;
                park_operation(task);
                None
            }
            Start::Issued(step) => Some(step),
            Start::Alone => unreachable!("an operation alone never parks"),
        };
        let produced = loop {
            let step = match step.take() {
                Some(step) => step,
                None => match op.state {
                    OperationState::ReadyToIssue => {
                        (op.provider.issue)(op.provider.state, request, token)
                    }
                    OperationState::ReadyToDeliver(outcome) => break outcome,
                    _ => unreachable!("resumed provider operation has no next action"),
                },
            };
            match step.kind {
                STEP_DONE => break Some(step.outcome),
                STEP_PENDING => {
                    op.state = OperationState::Issued {
                        deadline: step.deadline_ns,
                    };
                    (*task).operation = &mut op;
                    park_operation(task);
                }
                _ => crate::process::stop(b"Provider"),
            }
        };
        (*task).operation = ptr::null_mut();
        produced
    }
}
/// Test-only: make the scheduler call the first `wait` (switch out and back),
/// to compare against the direct call.
#[cfg(test)]
thread_local! { pub(crate) static SCHEDULER_ENTRY: Local<bool> = const { Local::new(false) }; }
/// Test-only: first waits made (directly by the task, by the scheduler after
/// the switch), so the comparison sees which path each of its halves took.
#[cfg(test)]
thread_local! { pub(crate) static FIRST_WAITS: Local<(usize, usize)> = const { Local::new((0, 0)) }; }
/// Test-only: false calls the OS provider's own callbacks through their
/// pointers too; the direct calls taken are counted, so a comparison sees
/// which form each of its runs used.
#[cfg(test)]
thread_local! {
    pub(crate) static OS_DIRECT: Local<bool> = const { Local::new(true) };
    pub(crate) static OS_DIRECT_CALLS: Local<usize> = const { Local::new(0) };
}
unsafe fn park_operation(task: *mut Task) {
    // SAFETY: the active task switches to its scheduler, which reads the
    // operation before any other task runs.
    unsafe {
        let op = (*task).operation;
        if matches!((*op).state, OperationState::Issued { .. }) {
            (*(*task).run).parked.insert((*op).token, task);
        }
        (*task).state = State::Provider;
        switch_to_idle(task);
    }
}
/// An operation with no scheduler (a direct ABI caller, or run teardown): no
/// other task exists, so this thread waits for the wake itself. Callbacks see
/// no task, as under a scheduler. Cancellation is checked only at entry: no
/// task exists to request it later.
unsafe fn alone(
    context: *mut Context,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
) -> Option<crate::provider::Outcome> {
    let p = provider;
    // This thread's waiter is registered before the first callback, so a ready
    // from inside `wait` or `issue` is not lost. The token is fresh, so the
    // stale tokens cleared here are other operations'.
    let readies = ALONE.with(|r| r.0.clone());
    struct AloneNet(Arc<Readies>);
    impl Drop for AloneNet {
        fn drop(&mut self) {
            self.0.net.lock().unwrap_or_else(|e| e.into_inner()).take();
            self.0
                .model
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
        }
    }
    let _network = AloneNet(readies.clone());
    readies.take(0);
    // Wait until readiness (true) or the deadline (false).
    let block = |deadline: Option<u64>| -> bool {
        loop {
            let model = readies
                .model
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(model) = model {
                let before = model.now();
                for token in model.poll(deadline, true) {
                    readies.push(token);
                }
                if readies.pending.load(Ordering::Acquire) && readies.take(token) {
                    return true;
                }
                if deadline.is_some_and(|at| at <= model.now()) {
                    return false;
                }
                if model.now() != before {
                    continue;
                }
                crate::process::stop(b"Deadlock");
            }
            if !unsafe { (*context).host_files }.is_null() {
                // SAFETY: direct ABI caller pins its registry through this wait.
                unsafe {
                    (*(*context).host_files)
                        .sockets
                        .poll(0, |token| readies.push(token))
                }
                .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
            }
            if readies.pending.load(Ordering::Acquire) && readies.take(token) {
                return true;
            }
            let network = readies
                .net
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if network {
                if deadline.is_some_and(|at| at <= crate::os::monotonic_ns()) {
                    return false;
                }
                unsafe {
                    (*(*context).host_files)
                        .sockets
                        .poll(poll_timeout(deadline), |token| readies.push(token))
                }
                .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
                continue;
            }
            match deadline {
                Some(at) => {
                    let now = crate::os::monotonic_ns();
                    if now >= at {
                        return false;
                    }
                    std::thread::park_timeout(Duration::from_nanos(at - now));
                }
                None => std::thread::park(),
            }
        }
    };
    // SAFETY: provider contract; request pinned by the caller.
    unsafe {
        loop {
            let w = (p.wait)(p.state, request, token);
            match w.kind {
                WAIT_READY => break,
                WAIT_UNTIL => {
                    block(Some(w.deadline_ns));
                }
                WAIT_PARK => {
                    block(None);
                }
                _ => crate::process::stop(b"Provider"),
            }
        }
        let mut step = (p.issue)(p.state, request, token);
        loop {
            match step.kind {
                STEP_DONE => return Some(step.outcome),
                STEP_PENDING => {
                    let deadline = (step.deadline_ns != 0).then_some(step.deadline_ns);
                    if block(deadline) {
                        step = (p.resume)(p.state, request, token);
                    } else {
                        return Some((p.abort)(p.state, request, token, ABORT_DEADLINE));
                    }
                }
                _ => crate::process::stop(b"Provider"),
            }
        }
    }
}

/// Replay dispatches before the first read or mutation of shared state.
pub(crate) fn before_commit() -> crate::sequencer::Guard {
    replay_boundary(false);
    crate::sequencer::enter()
}

/// Only a pool fiber can retain a receive acquisition. Replay has to
/// dispatch each scalar receive at its own tape gap.
pub(crate) fn can_fuse_receives() -> bool {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the task while this fiber is executing.
    unsafe { !task.is_null() && !WORKER.get().is_null() && !(*task).replay }
}

/// An Arc count commit, which the sole live unit may keep for its next one.
pub(crate) fn before_arc_commit() -> crate::sequencer::Guard {
    replay_boundary(false);
    crate::sequencer::enter_keepable()
}

/// Only a provider entry can reach a taped WAIT without first committing.
/// Inline: a task that is not replaying pays one field read for the boundary.
#[inline(always)]
pub(crate) fn before_provider_entry() -> crate::sequencer::Guard {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins this task.
    if !task.is_null() && unsafe { (*task).replay } {
        replay_boundary(true);
    }
    crate::sequencer::enter()
}

pub(crate) fn before_tape_frame(group: Group, ending: bool) -> crate::sequencer::Guard {
    tape_boundary(group, ending);
    crate::sequencer::enter()
}

fn tape_boundary(group: Group, ending: bool) {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the live opener; checkpoint borrows no task fields.
    unsafe {
        if task.is_null() || !(*task).replay || !(*task).group.is_some_and(|g| g.same(group)) {
            return;
        }
        use crate::tape::schedule::Turn;
        loop {
            match group.turn() {
                Turn::Fifo | Turn::Event(None | Some(0)) => return,
                Turn::Grant if !ending => return,
                Turn::Finish | Turn::Deadlock => return,
                Turn::Grant => {
                    group.fail();
                    return;
                }
                _ => checkpoint(),
            }
        }
    }
}

#[inline(never)]
fn replay_boundary(provider_entry: bool) {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins this task. No scheduler reference is held across a switch.
    unsafe {
        if task.is_null() || !(*task).replay {
            return;
        }
        let _commit = crate::sequencer::enter();
        let group = (*task).group.unwrap();
        use crate::tape::schedule::Turn;
        if (*task).latest_pair {
            if !matches!(group.turn(), Turn::Fifo | Turn::Event(Some(_))) {
                group.fail();
            }
            return;
        }
        loop {
            match group.turn() {
                Turn::Fifo | Turn::Deadlock => return,
                Turn::Event(Some(id)) if id == group.task => return,
                Turn::Wait(id) if id == group.task && provider_entry => return,
                Turn::Wait(id) if id == group.task => {
                    group.fail();
                    return;
                }
                Turn::Grant | Turn::Finish if group.task == 0 => {
                    if provider_entry {
                        return;
                    }
                    group.fail();
                    return;
                }
                Turn::Event(Some(id))
                    if (*(*task).run)
                        .group_task(group, id)
                        .as_ref()
                        .is_none_or(|wanted| {
                            !matches!(wanted.state, State::Ready | State::Flow)
                        }) =>
                {
                    group.fail();
                    return;
                }
                Turn::Event(None) => {
                    group.fail();
                    return;
                }
                _ => checkpoint(),
            }
        }
    }
}

unsafe fn live_index(task: *mut Task, group: crate::tape::schedule::Group) -> u64 {
    if group.task == 0 {
        return 0;
    }
    let mut index = 0;
    // SAFETY: the active committer owns its scheduler's creation list.
    unsafe {
        // The list is newest first. Each tape starts at its opener and assigns
        // child ordinals in creation order, so only older tasks can rank lower.
        let mut at = (*task).all;
        while !at.is_null() {
            if (*at).state != State::Ended
                && (*at)
                    .group
                    .is_some_and(|g| g.same(group) && g.task < group.task)
            {
                index += 1;
            }
            at = (*at).all;
        }
    }
    index
}

pub(crate) fn group_index() -> u64 {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the task while a provider issues synchronously.
    unsafe {
        task.as_ref()
            .and_then(|t| t.group)
            .map_or(0, |g| live_index(task, g))
    }
}

/// `event` for a caller already inside its commit.
#[inline(always)]
pub(crate) fn event_committed(kind: u8, object: u64, value: u64) {
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: the running task pins its scheduler; the caller holds the token.
        unsafe {
            record_event(task, kind, object, value);
            poll_event(task, kind, value);
        }
    }
}

/// Record only semantic identifiers, never native addresses. Arc IDs are assigned
/// by their event stream; zero denotes the operation's implicit current object.
#[inline(always)]
pub(crate) fn event(kind: u8, object: u64, value: u64) {
    let task_pointer = CURRENT.get();
    if task_pointer.is_null() {
        return;
    }
    // SAFETY: current pins its scheduler; callback cannot suspend or re-enter.
    unsafe { event_for(task_pointer, kind, object, value) }
}
/// # Safety
/// Task is the running task, so it pins its scheduler and sink.
#[inline(always)]
unsafe fn event_for(task_pointer: *mut Task, kind: u8, object: u64, value: u64) {
    let _commit = crate::sequencer::enter();
    // SAFETY: the running task pins the scheduler through both operations.
    unsafe {
        record_event(task_pointer, kind, object, value);
        if !matches!(kind, LATEST_READ | LATEST_PUBLISH) {
            poll_event(task_pointer, kind, value);
        }
    }
}

#[inline(always)]
unsafe fn record_event(task_pointer: *mut Task, kind: u8, object: u64, value: u64) {
    // SAFETY: current pins its scheduler; callback cannot suspend or re-enter.
    unsafe {
        (*(*task_pointer).context).current.drain();
        record_folded(task_pointer, kind, object, value);
    }
}

/// Record an event whose owner has no pending credit to fold.
#[inline(always)]
unsafe fn record_folded(task_pointer: *mut Task, kind: u8, object: u64, value: u64) {
    // SAFETY: the token holder pins the task's scheduler; callback cannot suspend.
    unsafe {
        let task = NonNull::new_unchecked(task_pointer).as_ref();
        let run = NonNull::new_unchecked(task.run).as_ref();
        if let Some(event) = run.provider.event {
            event(
                run.provider.state,
                Event {
                    kind,
                    task: task.id,
                    object,
                    value,
                },
            );
        }
        let opens_pair = matches!(kind, LATEST_READ | LATEST_PUBLISH);
        let was_pair = task.latest_pair;
        if let Some(group) = task.group {
            if was_pair
                && task.replay
                && !matches!(
                    group.turn(),
                    crate::tape::schedule::Turn::Fifo | crate::tape::schedule::Turn::Event(Some(_))
                )
            {
                group.fail();
            }
            group.event(|| live_index(task_pointer, group), kind == END);
        }
        if (*task_pointer).latest_pair != opens_pair {
            (*task_pointer).latest_pair = opens_pair;
            (*task_pointer).arc_hooks = (*(*task_pointer).run).arc_hooks(&*task_pointer);
        }
    }
}
/// Publish the accounting change before a scheduler poll can switch tasks.
pub(crate) fn commit_refund(owner: u64, bytes: u64, publish: impl FnOnce()) {
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: the running task pins the scheduler. Recording cannot switch.
        unsafe { record_event(task, REFUND, owner, bytes) };
    }
    publish();
}

pub(crate) fn poll_after_refund() {
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: the running task pins its scheduler; the refund is published.
        unsafe { poll_event(task, REFUND, 0) };
    }
}

/// Complete a recorded inline operation without consulting task TLS.
/// # Safety
/// Context's immutable event word is nonzero and pins its current Task/run.
#[inline(always)]
/// Whether an inline Arc count event has nothing to fold or record: no tape,
/// sink, test schedule, open LATEST pair or pending credit, and no network
/// poll owed (a pool polls only at poll kinds, which Arc counts are not).
/// Such an event needs no token; any other goes through `event_in`.
/// # Safety
/// `context` is the running task's, with a nonzero event word.
pub(crate) unsafe fn arc_unobserved(context: *const Context) -> bool {
    // SAFETY: the event word pins the running task and its scheduler. The
    // task alone writes its tape and pair state while it runs.
    unsafe {
        let task = &*((*context).events as *const Task);
        !task.arc_runtime
            && !task.arc_hooks
            && !(*(*context).current.pending_word()).load(Ordering::Acquire)
    }
}
pub(crate) unsafe fn event_in(context: *const Context, kind: u8, object: u64, value: u64) {
    // SAFETY: the emitted gate proves the word is this running task.
    unsafe { event_for((*context).events as *mut Task, kind, object, value) }
}
/// The one trigger for the spec's no-effect poll family. Record the result
/// before scheduling; never observe cancellation or change that result here.
#[inline(always)]
unsafe fn poll_event(task: *mut Task, kind: u8, value: u64) {
    // SAFETY: this synchronous poll only queues tokens, never invokes task callbacks.
    unsafe {
        if !(*(*task).run).net.is_null()
            && ((*(*task).run).pool.is_none() || hirsel_core_layout::event::is_poll(kind, value))
        {
            (*(*task).run).poll_net(0);
            if (*(*task).run)
                .readies
                .as_ref()
                .pending
                .load(Ordering::Acquire)
            {
                checkpoint();
            }
        }
    }
    if hirsel_core_layout::event::is_poll(kind, value) {
        // SAFETY: the active task exclusively owns its counter and event turn.
        unsafe {
            let state = NonNull::new_unchecked(task).as_mut();
            // The scheduler initializes/reset this private counter to 64 and
            // reaches this operation only while it is nonzero.
            state.polls_left = state.polls_left.wrapping_sub(1);
            if state.polls_left == 0 {
                poll_turn(task);
            }
        }
    }
}
#[cold]
#[inline(never)]
unsafe fn poll_turn(task: *mut Task) {
    // SAFETY: the active task owns the queue until switching to idle. Reset the
    // allowance even without a peer; effectful events never reach this path.
    unsafe {
        (*task).polls_left = POLLS_PER_TURN;
        let run = (*task).run;
        if let Some(pool) = (*run).pool {
            // SAFETY: the active commit pins the worker and its run.
            if pool.as_ref().units.load(Ordering::Relaxed) > 1
                || (*run).readies.as_ref().pending.load(Ordering::Acquire)
            {
                (*task).state = State::Ready;
                (*run).publish(task, true);
                switch_to_idle(task);
            }
            return;
        }
        if !(*run).head.is_null() || !(*run).deferred.is_null() {
            (*task).state = State::Ready;
            (*task).ready = ptr::null_mut();
            if (*run).deferred_tail.is_null() {
                (*run).deferred = task;
            } else {
                (*(*run).deferred_tail).ready = task;
            }
            (*run).deferred_tail = task;
            switch_to_idle(task);
        }
    }
}
/// Cold refusal only: the successful admission's instruction path is untouched.
/// Snapshot the Capacity before scheduling so the poll never changes its result.
#[cold]
#[inline(never)]
pub(crate) fn capacity_refused(requested: u64, available: u64) -> Capacity {
    let _commit = before_commit();
    event(CAPACITY, requested, available);
    Capacity {
        requested,
        available,
    }
}
struct CallbackTask(*mut Task);
impl CallbackTask {
    fn new(task: *mut Task) -> Self {
        Self(CALLBACK_TASK.replace(task))
    }
}
impl Drop for CallbackTask {
    fn drop(&mut self) {
        CALLBACK_TASK.set(self.0);
    }
}
pub(crate) fn parallel() -> bool {
    // SAFETY: the thread-local worker pins its pool.
    unsafe {
        WORKER
            .get()
            .as_ref()
            .is_some_and(|worker| !(*worker.pool).serial_replay)
    }
}

pub(crate) fn flow_parallel() -> bool {
    !WORKER.get().is_null()
}

pub(crate) fn recorded_chooser() -> u8 {
    #[cfg(any(test, feature = "testing"))]
    {
        let task = CURRENT.get();
        // SAFETY: CURRENT pins the scheduler while the tape opens.
        if unsafe {
            !task.is_null() && ((*(*task).run).script.is_some() || (*(*task).run).pool.is_some())
        } {
            return hirsel_tape_format::CHOOSER_COMMIT;
        }
    }
    hirsel_tape_format::CHOOSER_FIFO
}
pub(crate) fn has_provider_task() -> bool {
    !CURRENT.get().is_null() || !CALLBACK_TASK.get().is_null()
}
pub(crate) fn provider_group() -> Option<crate::tape::schedule::Group> {
    let task = if CURRENT.get().is_null() {
        CALLBACK_TASK.get()
    } else {
        CURRENT.get()
    };
    // SAFETY: the callback's task is pinned for the synchronous callback.
    unsafe { task.as_ref().and_then(|t| t.group) }
}

pub(crate) fn current_group() -> Option<crate::tape::schedule::Group> {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins its task, or is null outside a task.
    unsafe { task.as_ref().and_then(|t| t.group) }
}
/// A model's clock and readiness drive the scheduler only after its provider
/// attaches. Keeping these callbacks with the provider lets native-only runs
/// link the scheduler without the deterministic network simulation.
pub(crate) trait ModelClock: Send + Sync {
    fn now(&self) -> u64;
    fn poll(&self, deadline: Option<u64>, idle: bool) -> Vec<u64>;
}

pub(crate) fn attach_model(model: Arc<dyn ModelClock>) {
    let task = if CURRENT.get().is_null() {
        CALLBACK_TASK.get()
    } else {
        CURRENT.get()
    };
    if task.is_null() {
        ALONE.with(|r| *r.0.model.lock().unwrap_or_else(|e| e.into_inner()) = Some(model));
    } else {
        // SAFETY: the provider callback's task pins its run on this worker.
        unsafe {
            (*(*task).run).model = Some(model);
        }
    }
}
pub(crate) fn attach_net(
    sockets: *mut hirsel_core_net::Sockets,
    poller: Arc<hirsel_core_net::Poller>,
) {
    let mut task = CURRENT.get();
    if task.is_null() {
        task = CALLBACK_TASK.get();
    }
    let readies = if task.is_null() {
        ALONE.with(|r| r.0.clone())
    } else {
        // SAFETY: the callback/task pins the run and its shared registry.
        unsafe {
            let run = &mut *(*task).run;
            run.net = sockets;
            // Sockets change only a serial run's hooks, and its tasks all run
            // on this thread; pool tasks keep their byte and are not touched.
            let mut at = if run.pool.is_none() {
                run.all
            } else {
                ptr::null_mut()
            };
            while !at.is_null() {
                (*at).arc_hooks = run.arc_hooks(&*at);
                at = (*at).all;
            }
            run.readies.clone()
        }
    };
    *readies.net.lock().unwrap_or_else(|e| e.into_inner()) = Some(poller);
}
fn poll_timeout(deadline: Option<u64>) -> i32 {
    deadline.map_or(-1, |at| {
        at.saturating_sub(crate::os::monotonic_ns())
            .div_ceil(1_000_000)
            .min(i32::MAX as u64) as i32
    })
}

pub(crate) fn attach_tape(group: Group) {
    let _commit = crate::sequencer::enter();
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: CURRENT pins the active task and run through the membership move.
        unsafe {
            let tapes = &mut (*(*task).run).tapes;
            if let Some(old) = (*task).group
                && let Some(members) = tapes.membership(old)
            {
                *members -= 1;
            }
            tapes.open(group);
            *tapes.membership(group).unwrap() += 1;
            (*task).group = Some(group);
            (*task).replay = group.replaying();
            (*task).arc_runtime |= (*task).replay || (*(*task).run).pool.is_some();
            (*task).arc_hooks = true;
        }
    }
}

pub(crate) fn detach_tape(group: Group) {
    let _commit = crate::sequencer::enter();
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: the active task owns the inventory through the tape close.
        unsafe { (*(*task).run).tapes.close(group) };
    }
}

pub(crate) fn current_id() -> u64 {
    let task = CURRENT.get();
    if task.is_null() {
        return 0;
    }
    // SAFETY: TLS pins the live currently running task.
    unsafe { (*task).id }
}

#[cfg(any(test, feature = "testing"))]
pub(crate) fn flow_script() -> Option<(Arc<crate::schedule_script::ScheduleScript>, u64)> {
    use crate::schedule_script::{UNTAPED, installed};
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the running owner. The test control is read at an
    // owner's operation boundary, outside the dispatcher.
    let Some(task) = (unsafe { task.as_ref() }) else {
        return installed().map(|script| (script, UNTAPED));
    };
    let _commit = crate::sequencer::enter();
    let run = unsafe { &mut *task.run };
    let script = run.script.clone()?;
    let Some(group) = task.group else {
        return Some((script, UNTAPED));
    };
    let domain = run
        .script_groups
        .iter()
        .position(|g| g.same(group))
        .unwrap_or_else(|| {
            run.script_groups.push(group);
            run.script_groups.len() - 1
        });
    Some((script, domain as u64))
}
/// Whether a task is running on this thread (tests assert where callbacks run).
#[cfg(test)]
pub(crate) fn request_flow_fixture_cancel() {
    // SAFETY: the test hook is on the owner's active stack.
    unsafe { cancel_flag(CURRENT.get()).store(true, Ordering::Release) };
}

#[cfg(test)]
pub(crate) fn in_task() -> bool {
    !CURRENT.get().is_null()
}
pub(crate) fn identity() -> u64 {
    // SAFETY: channel/task creation occurs in an active run.
    let task = CURRENT.get();
    if task.is_null() {
        return 0;
    }
    let _commit = crate::sequencer::enter();
    unsafe { (*(*task).run).id() }
}
/// # Safety
/// Called on the active fiber, with no Rust references held to scheduler/task state.
pub unsafe fn checkpoint() {
    let _commit = crate::sequencer::enter();
    let task = CURRENT.get();
    // SAFETY: saving exactly this fiber; idle and task stacks stay mapped.
    unsafe {
        if !task.is_null() {
            let run = (*task).run;
            (*run).ready(task);
            switch_to_idle(task);
        }
    }
}

/// The returned guard keeps the resumed commit through the caller's retry,
/// so a woken waiter rechecks its condition without a second acquisition.
/// # Safety
/// The active task owns the commit and holds the condition's owner alive
/// across suspension. No references to scheduler/task state survive the switch.
pub(crate) unsafe fn park(queue: *mut Waiters) -> crate::sequencer::Guard {
    let commit = crate::sequencer::enter();
    let task = CURRENT.get();
    // SAFETY: the condition was checked in this commit and the queue is pinned.
    unsafe {
        if !task.is_null() {
            (*task).state = State::Waiting;
            Waiters::push(queue, task);
            switch_to_idle(task);
        }
    }
    commit
}

/// # Safety
/// The active commit holds the condition's owner alive through the event.
pub(crate) unsafe fn event_wake_waiters(kind: u8, object: u64, value: u64, queue: *mut Waiters) {
    let task = CURRENT.get();
    // SAFETY: as for event_wake_waiters_for, with the running task.
    unsafe { event_wake_waiters_for(task, kind, object, value, queue, true) }
}

/// Record a waking event for `task`, the running task or a delegated
/// requester. A served event has already been shown to need no fold.
/// # Safety
/// The active commit holds the condition's owner alive through the event.
pub(crate) unsafe fn event_wake_waiters_for(
    task: *mut Task,
    kind: u8,
    object: u64,
    value: u64,
    queue: *mut Waiters,
    fold: bool,
) {
    let _commit = crate::sequencer::enter();
    // SAFETY: recording and waking use the same commit, before any switch.
    unsafe {
        if !task.is_null() {
            if fold {
                record_event(task, kind, object, value);
            } else {
                record_folded(task, kind, object, value);
            }
            let _wakes = Waiters::wake_all(queue);
            #[cfg(feature = "testing")]
            if matches!(kind, PUBLISH | RECEIVE)
                && let Some(pool) = (*(*task).run).pool
                && pool.as_ref().measure
            {
                let sample = &pool.as_ref().sample;
                sample.channel_wakes.fetch_add(_wakes, Ordering::Relaxed);
                if kind == RECEIVE {
                    sample.items.fetch_add(1, Ordering::Relaxed);
                }
            }
            poll_event(task, kind, value);
        }
    }
}

/// A channel commit another worker may complete for this task.
#[repr(C)]
pub(crate) struct Delegated {
    request: crate::sequencer::Request,
    pub(crate) task: *mut Task,
    context: *mut Context,
    pending: *const AtomicBool,
    kind: u8,
    pub(crate) channel: *mut u8,
    pub(crate) value: *mut u8,
}

/// Offer a channel commit for delegation. None means a holder completed it;
/// otherwise the caller holds the token and runs the operation itself.
/// # Safety
/// The running task's context and the operands stay pinned for the call.
#[inline(always)]
pub(crate) unsafe fn delegate(
    context: *mut Context,
    kind: u8,
    channel: *mut u8,
    value: *mut u8,
    serve: unsafe fn(*mut Delegated) -> bool,
) -> Option<crate::sequencer::Guard> {
    let task = CURRENT.get();
    // SAFETY: the running task pins its context and scheduler.
    unsafe {
        if task.is_null() || WORKER.get().is_null() {
            return Some(crate::sequencer::enter());
        }
        if (*task).replay {
            return Some(before_commit());
        }
        let mut request = Delegated {
            request: crate::sequencer::Request {
                serve: std::mem::transmute::<
                    unsafe fn(*mut Delegated) -> bool,
                    unsafe fn(*mut crate::sequencer::Request) -> bool,
                >(serve),
            },
            task,
            context,
            pending: (*context).current.pending_word(),
            kind,
            channel,
            value,
        };
        match crate::sequencer::enter_or_delegate(&raw mut request.request) {
            crate::sequencer::Entry::Held(guard) => Some(guard),
            crate::sequencer::Entry::Served => None,
        }
    }
}

/// Whether a holder may complete `request` without its task: no pending
/// cancellation, no credit to fold, no poll turn or driver poll at this event.
/// # Safety
/// The caller holds the token; the requester is blocked on its request.
pub(crate) unsafe fn delegable(request: &Delegated) -> bool {
    // SAFETY: the blocked requester pins its task, context and scheduler.
    unsafe {
        let task = request.task;
        let run = (*task).run;
        let poll = hirsel_core_layout::event::is_poll(request.kind, 0);
        !(*task).replay
            && !cancel_takes_effect(&*request.context, cancel_flag(task))
            && !(*request.pending).load(Ordering::Acquire)
            && !(poll && (*task).polls_left <= 1)
            && ((*run).net.is_null() || ((*run).pool.is_some() && !poll))
    }
}

/// # Safety
/// Context belongs to this task; primary is available on a normal path.
#[inline(always)]
pub(crate) unsafe fn cancel_pending(context: *mut Context) -> bool {
    // SAFETY: the operation pins its exclusive, aligned task context. Pending
    // cancellation is the common false condition; shield/grant checks are cold.
    let current = unsafe { NonNull::new_unchecked(context).as_ref() };
    let task = CURRENT.get();
    let requested = if task.is_null() {
        &current.cancel_requested
    } else {
        // SAFETY: the active fiber pins the task.
        unsafe { cancel_flag(task) }
    };
    cancel_takes_effect(current, requested)
}

/// # Safety
/// Context belongs to this task; primary is available on a normal path.
#[inline(always)]
pub unsafe fn check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the operation pins the running task and its context.
    if unsafe { cancel_pending(context) } {
        // SAFETY: this task exclusively owns the observed request and primary.
        unsafe { observe_cancel(context, site) };
        1
    } else {
        0
    }
}
#[cold]
#[inline(never)]
unsafe fn observe_cancel(context: *mut Context, site: u64) {
    // SAFETY: the operation entry exclusively owns this task context.
    unsafe {
        let _commit = before_commit();
        let task = CURRENT.get();
        if task.is_null() {
            (*context).cancel_requested.store(false, Ordering::Release);
        } else {
            cancel_flag(task).store(false, Ordering::Release);
        }
        causes::record(&mut *context, Cause::new(causes::CANCELLED, site));
        event(CANCELLED, 0, site);
    }
}
/// # Safety
/// Active context and available primary on a normal path. This is an explicit
/// checkpoint; production pure code never calls it implicitly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: observe entry in its commit turn.
    unsafe {
        let _commit = before_commit();
        if check_cancel(context, site) != 0 {
            return 1;
        }
        if (*context).events != 0 {
            event_in(context, CHECK_CANCEL, 0, 0);
        }
        0
    }
}

unsafe extern "C" fn start(first: *mut u8, _: *mut u8) -> ! {
    let task = first.cast::<Task>();
    crate::sequencer::release_handed();
    // SAFETY: start owns this fiber. The body cleans its input/temporaries;
    // its result is initialized only on status zero.
    unsafe {
        (*task).failed = ((*task).body)(
            (*task).context,
            (*task).result.as_ptr(),
            (*task).input.as_ptr(),
        );
        // The hand-over and END share one hold of the token.
        let _handover = (*task).record.is_some().then(|| {
            let commit = crate::sequencer::enter();
            abi::task_handover((*task).context);
            commit
        });
        let _commit = before_commit();
        record_event(task, END, (*task).id, (*task).failed as u64);
        (*task).state = State::Ended;
        if let Some(group) = (*task).group
            && let Some(members) = (*(*task).run).tapes.membership(group)
        {
            *members -= 1;
        }
        Waiters::wake_all(&raw mut (*task).waiters);
        switch_to_idle(task);
        std::hint::unreachable_unchecked();
    }
}

#[cfg(any(test, feature = "testing"))]
thread_local! {
    static WORKER_COUNT: Local<usize> = const { Local::new(0) };
    static REPLAY_STAGES: Local<bool> = const { Local::new(false) };
}

fn worker_count() -> usize {
    #[cfg(any(test, feature = "testing"))]
    if WORKER_COUNT.get() != 0 {
        return WORKER_COUNT.get();
    }
    #[cfg(all(feature = "testing", not(test)))]
    if let Some(width) = std::env::var_os("HIRSEL_S8_WORKERS") {
        return width
            .to_str()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| crate::process::stop(b"TestWorkerCount"));
    }
    1
}

/// Run a native test fixture with a fixed number of workers.
#[cfg(any(test, feature = "testing"))]
pub fn with_workers<R>(workers: usize, f: impl FnOnce() -> R) -> R {
    assert_ne!(workers, 0);
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            WORKER_COUNT.set(self.0);
        }
    }
    let _restore = Restore(WORKER_COUNT.replace(workers));
    f()
}

/// Replay a single domain on its reference dispatcher while the other
/// workers run only untaped flow stages.
#[cfg(any(test, feature = "testing"))]
pub fn with_replay_stages<R>(workers: usize, f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REPLAY_STAGES.set(self.0);
        }
    }
    let _restore = Restore(REPLAY_STAGES.replace(true));
    with_workers(workers, f)
}

fn replay_stages() -> bool {
    #[cfg(any(test, feature = "testing"))]
    return REPLAY_STAGES.get();
    #[cfg(not(any(test, feature = "testing")))]
    false
}

/// Execute a root on the same guarded stack discipline as its children.
/// # Safety
/// Context/body/argument/destination remain pinned and task-owned until completion.
pub unsafe fn run_root(
    context: *mut Context,
    body: Body,
    argument: *mut u8,
    destination: *mut u8,
    input_bytes: usize,
    result_bytes: usize,
    provider: SchedulerProvider,
) -> u8 {
    let workers = worker_count();
    crate::fiber::arm();
    let mut run = Scheduler {
        pool: None,
        deferred: ptr::null_mut(),
        deferred_tail: ptr::null_mut(),
        stacks: Stock::default(),
        idle: ptr::null_mut(),
        head: ptr::null_mut(),
        tail: ptr::null_mut(),
        timers: ptr::null_mut(),
        all: ptr::null_mut(),
        next: 1,
        provider,
        #[cfg(any(test, feature = "testing"))]
        script: crate::schedule_script::installed(),
        #[cfg(any(test, feature = "testing"))]
        script_groups: Vec::new(),
        readies: Readies::new(),
        ready_tokens: Vec::new(),
        ready_arms: Vec::new(),
        #[cfg(feature = "testing")]
        measure: measurement_enabled(),
        cancels: Vec::new(),
        outside_cancels: Vec::new(),
        tapes: Tapes::default(),
        net: ptr::null_mut(),
        model: None,
        parked: WaitArms::default(),
    };
    let pool = (workers > 1).then(|| {
        // SAFETY: the caller pins the root context through worker teardown.
        let funding = unsafe { (*context).current.cache_domain() };
        Box::new(Pool::new(
            workers,
            run.readies.clone(),
            funding,
            replay_stages(),
        ))
    });
    run.pool = pool
        .as_deref()
        .filter(|pool| !pool.serial_replay)
        .map(NonNull::from);
    let registered = Registered(run.readies.clone());
    WAITERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(run.readies.clone());
    let descriptor = ValueLayout {
        bytes: input_bytes,
        align: 16,
        logical: 0,
        clean: None,
    };
    let mut root = Box::new(Task {
        arc_runtime: false,
        arc_hooks: true,
        cancel: AtomicBool::new(false),
        replay: false,
        latest_pair: false,
        polls_left: POLLS_PER_TURN,
        context,
        owned_context: None,
        run: &mut run,
        ready: ptr::null_mut(),
        domain: RESIDUAL_DOMAIN,
        worker: None,
        #[cfg(feature = "testing")]
        enqueued_at: 0,
        waiters: Waiters::default(),
        waiting_on: ptr::null_mut(),
        wait_previous: ptr::null_mut(),
        wait_next: ptr::null_mut(),
        timer_next: ptr::null_mut(),
        timer_deadline: None,
        operation: ptr::null_mut(),
        all: ptr::null_mut(),
        state: State::Ready,
        id: 0,
        group: None,
        stack: Some(run.stacks.take()),
        sp: ptr::null_mut(),
        body,
        input: Storage::new(input_bytes, 16),
        result: Storage::new(result_bytes, 16),
        layout: TaskLayout {
            input: descriptor,
            result: ValueLayout {
                bytes: result_bytes,
                ..descriptor
            },
        },
        failed: 0,
        record: None,
        holder: std::mem::MaybeUninit::uninit(),
        cursor: 0,
    });
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::allocated((&*root as *const Task).cast(), size_of::<Task>());
    // SAFETY: root's stable allocation and both stacks stay live through the loop.
    unsafe {
        ptr::copy_nonoverlapping(argument, root.input.as_ptr(), input_bytes);
        let root_pointer: *mut Task = &mut *root;
        (*context).events = root_pointer.expose_provenance();
        root.sp = root
            .stack
            .as_ref()
            .unwrap()
            .prepare(start, root_pointer.cast(), ptr::null_mut());
        run.all = root_pointer;
        root.arc_hooks = run.arc_hooks(&root);
        if let Some(pool) = pool.as_deref().filter(|pool| !pool.serial_replay) {
            root.arc_runtime = run.arc_observed();
            root.worker = Some(0);
            root.state = State::Ready;
            let domain = &pool.domains[&root.domain];
            domain.ready.store(1, Ordering::Relaxed);
            domain.handoffs.store(1, Ordering::Relaxed);
            pool.queues[0]
                .lock()
                .unwrap()
                .pinned
                .push_back(TaskPtr(root_pointer));
            run_workers(RunPtr(&mut run), TaskPtr(root_pointer), pool);
            #[cfg(feature = "testing")]
            if pool.measure {
                pool.report_domain_lookup();
                eprintln!(
                    "pool workers={} stolen={} channel_items={} channel_wakes={}",
                    workers,
                    pool.sample.stolen.load(Ordering::Relaxed),
                    pool.sample.items.load(Ordering::Relaxed),
                    pool.sample.channel_wakes.load(Ordering::Relaxed),
                );
            }
        } else {
            run.ready(root_pointer);
            let run = RunPtr(&mut run);
            if let Some(pool) = pool.as_deref() {
                run_workers(run, TaskPtr(root_pointer), pool);
            } else {
                run.dispatch_serial(TaskPtr(root_pointer), ptr::null_mut());
            }
        }
        if root.failed == 0 {
            ptr::copy_nonoverlapping(root.result.as_ptr(), destination, result_bytes);
        }
        (*context).events = 0;
        drop(registered);
        root.failed
    }
}

#[derive(Clone, Copy)]
struct RunPtr(*mut Scheduler);
// SAFETY: the run is scoped and pinned. The residual sequencer serializes
// changes to this Scheduler; only a fiber's owner can execute its stack.
unsafe impl Send for RunPtr {}

impl RunPtr {
    fn worker(self, root: TaskPtr, pool: &Pool, index: usize, stop_fd: i32) {
        crate::fiber::arm();
        let previous_fd = crate::fiber::set_stop_fd(stop_fd);
        pool.threads[index].set(std::thread::current()).unwrap();
        if index != 0 {
            pool.readies
                .peers
                .lock()
                .unwrap()
                .push(std::thread::current());
        }
        let mut worker = Worker {
            index,
            pool,
            idle: ptr::null_mut(),
            stock: Stock::default(),
            #[cfg(feature = "testing")]
            dispatch_sample: DispatchSample::default(),
            outbox: Vec::new(),
            stolen: Vec::with_capacity(LOCAL_QUEUE / 2),
            #[cfg(any(test, feature = "testing"))]
            script: pool.script.clone(),
        };
        let previous = WORKER.replace(&mut worker);
        if pool.serial_replay {
            // SAFETY: the run and worker stay pinned until the scoped workers join.
            unsafe {
                if index == 0 {
                    self.dispatch_serial(root, &raw mut worker);
                    pool.stop_flow_workers.store(true, Ordering::Release);
                    pool.wake_workers();
                } else {
                    while !pool.stop_flow_workers.load(Ordering::Acquire) {
                        if !pool.run_flow_job() {
                            pool.sleep(index);
                            if !pool.stop_flow_workers.load(Ordering::Acquire)
                                && pool.flow.lock().unwrap().is_empty()
                            {
                                std::thread::park();
                            }
                            pool.wake(index);
                        }
                    }
                }
            }
        } else {
            let domain = &pool.domains[&RESIDUAL_DOMAIN];
            let mut owner = crate::sequencer::Owner::new(
                &domain.sequencer,
                publish_deliveries,
                &pool.units,
                pool.measure,
            );
            // SAFETY: scoped workers pin all operands. The root remains alive
            // until after every worker has returned its vacant-slot lease.
            unsafe {
                crate::sequencer::install(&mut owner, || {
                    pool.funding.lease_vacant();
                    self.dispatch(root, &raw mut worker);
                    let stats = crate::limit::Funding::return_vacant();
                    if pool.measure {
                        eprintln!(
                            "slots worker={} transfers={} leased={} returned={} peak={} returned_at_exit={}",
                            index,
                            stats.transfers,
                            stats.leased,
                            stats.returned,
                            stats.peak,
                            stats.at_exit,
                        );
                    }
                });
            }
            #[cfg(feature = "testing")]
            if pool.measure {
                worker.dispatch_sample.report(index);
                owner.report(index);
            }
        }
        WORKER.set(previous);
        crate::fiber::set_stop_fd(previous_fd);
    }

    /// The reference dispatcher exclusively owns the run. Flow workers may
    /// publish a wake but never read task state or take a domain turn.
    unsafe fn dispatch_serial(self, root: TaskPtr, worker: *mut Worker) {
        // SAFETY: only this thread reads or writes task and scheduler state.
        unsafe {
            let run = self.0;
            let pool = worker.as_ref().map(|worker| &*worker.pool);
            let idle = if worker.is_null() {
                &raw mut (*run).idle
            } else {
                &raw mut (*worker).idle
            };
            #[cfg(feature = "testing")]
            let mut dispatch_sample = DispatchSample::default();
            let previous = CURRENT.get();
            let mut direct = ptr::null_mut::<Task>();
            loop {
                if (*root.0).state == State::Ended {
                    break;
                }
                let task = if direct.is_null() {
                    if let Some(pool) = pool {
                        while let Some(woken) = pool.pop_flow_wake(0) {
                            (*run).ready(woken.0);
                            pool.units.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                    // An idle native run can collect readiness in its wait.
                    // Ready tokens are still applied before any expired timer.
                    let run = &mut *run;
                    let wait_for_net = pool.is_none()
                        && !run.net.is_null()
                        && run.model.is_none()
                        && run.head.is_null()
                        && run.deferred.is_null()
                        && run.tapes.live.is_empty()
                        && run.cancels.is_empty()
                        && !run.readies.pending.load(Ordering::Acquire)
                        && run.has_outside_wait();
                    let timeout = if wait_for_net {
                        poll_timeout(run.timers.as_ref().and_then(|task| task.timer_deadline))
                    } else {
                        0
                    };
                    run.take_wakes(timeout);
                    let task = run.pop();
                    if task.is_null() {
                        if !wait_for_net {
                            let flow_pending =
                                pool.is_some_and(|pool| pool.units.load(Ordering::Acquire) != 0);
                            run.idle(flow_pending);
                        }
                        continue;
                    }
                    task
                } else {
                    std::mem::replace(&mut direct, ptr::null_mut())
                };
                #[cfg(feature = "testing")]
                dispatch_sample.take(task);
                CURRENT.set(task);
                (*task).state = State::Running;
                abi::task_resume((*task).context);
                crate::fiber::guard((*task).stack.as_ref().unwrap().bounds());
                hirsel_stack::switch(idle, (*task).sp);
                crate::fiber::guard((0, 0));
                crate::limit::Funding::unbind();
                CURRENT.set(previous);
                match (*task).state {
                    State::Ended => (*worker_stock(self.0)).recycle((*task).stack.take().unwrap()),
                    State::Provider if (*run).wake_operation(task, Wake::Parked) => direct = task,
                    _ => {}
                }
            }
            #[cfg(feature = "testing")]
            if (*run).measure {
                dispatch_sample.report(0);
            }
        }
    }

    /// # Safety
    /// The calling OS thread owns this Worker, and scoped startup pins run
    /// and root until all dispatchers return.
    unsafe fn dispatch(self, root: TaskPtr, worker: *mut Worker) {
        // SAFETY: shared run mutations take the domain commit. Task stack
        // access stays on its physical worker after the first dispatch.
        unsafe {
            let run = self.0;
            let pool = &*(*worker).pool;
            let mut captured = Vec::new();
            let mut direct = ptr::null_mut::<Task>();
            loop {
                let mut flow_wake = None;
                if direct.is_null() {
                    flow_wake = pool.pop_flow_wake((*worker).index);
                    if flow_wake.is_none() && pool.run_flow_job() {
                        continue;
                    }
                }
                #[allow(unused_mut)]
                let mut commit = crate::sequencer::enter();
                if (*root.0).state == State::Ended {
                    return;
                }
                let task = if let Some(woken) = flow_wake {
                    woken.0
                } else if direct.is_null() {
                    (*run).capture_wakes(&mut captured, 0);
                    for ready in captured.drain(..) {
                        #[cfg(any(test, feature = "testing"))]
                        if let Some(script) = &pool.script {
                            // Keep the captured input in flight across the test pause.
                            pool.units.fetch_add(1, Ordering::Relaxed);
                            drop(commit);
                            script.pause(crate::schedule_script::PausePoint::ReadinessTaken {
                                token: ready.token,
                            });
                            commit = crate::sequencer::enter();
                            pool.units.fetch_sub(1, Ordering::Relaxed);
                        }
                        (*run).apply_ready(ready);
                    }
                    if captured.capacity() > RETAINED_READY_CAPACITY {
                        captured = Vec::new();
                    }
                    (*run).take_timers();
                    if (*root.0).state == State::Ended {
                        return;
                    }
                    let Some(task) = pool.pop((*worker).index, &mut (*worker).stolen) else {
                        drop(commit);
                        self.park_worker(pool, root, (*worker).index);
                        continue;
                    };
                    let domain = &pool.domains[&(*task.0).domain];
                    domain.ready.fetch_sub(1, Ordering::Relaxed);
                    domain.handoffs.fetch_sub(1, Ordering::Relaxed);
                    task.0
                } else {
                    std::mem::replace(&mut direct, ptr::null_mut())
                };
                #[cfg(feature = "testing")]
                (*worker).dispatch_sample.take(task);
                (*task).worker.get_or_insert((*worker).index);
                (*task).state = State::Running;
                CURRENT.set(task);
                abi::task_resume((*task).context);
                crate::fiber::guard((*task).stack.as_ref().unwrap().bounds());
                crate::sequencer::to_fiber(commit, || {
                    hirsel_stack::switch(&raw mut (*worker).idle, (*task).sp);
                });
                crate::fiber::guard((0, 0));
                let commit = crate::sequencer::enter();
                crate::limit::Funding::unbind();
                CURRENT.set(ptr::null_mut());
                let mut ended = None;
                match (*task).state {
                    State::Ended => {
                        ended = (*task).stack.take();
                        if task == root.0 {
                            pool.wake_workers();
                        }
                    }
                    State::Provider if (*run).wake_operation(task, Wake::Parked) => direct = task,
                    _ => {}
                }
                // The outgoing active unit is live across release. This
                // publishes every receiver's already-counted hand-off first.
                drop(commit);
                // The worker-private stock unmaps an excess stack outside the commit.
                if let Some(stack) = ended {
                    (*worker).stock.recycle_shared(stack, &pool.spare);
                }
                // Retiring the unit after publication needs no commit: a peer's
                // quiescence check that reads zero also sees every counted delivery.
                if direct.is_null() {
                    pool.units.fetch_sub(1, Ordering::Release);
                }
            }
        }
    }

    /// # Safety
    /// The caller is a registered worker and owns none of the run's locks.
    unsafe fn park_worker(self, pool: &Pool, root: TaskPtr, index: usize) {
        pool.nudged[index].store(false, Ordering::Relaxed);
        pool.sleep(index);
        if pool.has_work(index) {
            pool.wake(index);
            return;
        }
        // Spin briefly for a wake before the quiescence commit and the OS
        // park: a worker whose pinned fiber is woken a few microseconds
        // later would otherwise pay two context switches per wake (Go's
        // spinning Ms). The spin reads only this worker's flag and the
        // readiness flag, which outside threads set without a nudge.
        let deadline = crate::os::monotonic_ns().saturating_add(IDLE_SPIN_NS);
        loop {
            if pool.nudged[index].load(Ordering::Acquire)
                || pool.readies.pending.load(Ordering::Acquire)
            {
                pool.wake(index);
                return;
            }
            if crate::os::monotonic_ns() >= deadline {
                break;
            }
            for _ in 0..64 {
                std::hint::spin_loop();
            }
        }
        // SAFETY: recheck under registry exclusion then the residual lock.
        let (deadline, poller) = unsafe {
            let _registry = pool.registry.lock().unwrap();
            let _commit = crate::sequencer::enter();
            let run = &mut *self.0;
            if (*root.0).state == State::Ended {
                pool.wake(index);
                return;
            }
            let deadline = run.timers.as_ref().and_then(|task| task.timer_deadline);
            if run.readies.pending.load(Ordering::Acquire) {
                pool.wake(index);
                return;
            }
            if pool.units.load(Ordering::Acquire) == 0
                && pool.domains.values().all(|domain| {
                    domain.ready.load(Ordering::Relaxed) == 0
                        && domain.handoffs.load(Ordering::Relaxed) == 0
                })
                && deadline.is_none()
                && !run.has_outside_wait()
            {
                run.stop_quiescent();
            }
            #[cfg(test)]
            pool.idle_checks.fetch_add(1, Ordering::Release);
            let poller = run.readies.net.lock().unwrap().clone();
            (deadline, poller)
        };
        if let Some(poller) = poller
            && let Some(events) = poller
                .poll(poll_timeout(deadline))
                .unwrap_or_else(|_| crate::process::stop(b"HostPoll"))
        {
            let _commit = crate::sequencer::enter();
            // SAFETY: the commit owns the socket registry and the poll batch
            // belongs to this run's driver. Re-armed tags are filtered there.
            unsafe {
                let run = &mut *self.0;
                if let Some(net) = run.net.as_mut() {
                    net.dispatch_polled(events, |token| run.readies.push(token));
                }
            }
        } else if let Some(deadline) = deadline {
            std::thread::park_timeout(Duration::from_nanos(
                deadline.saturating_sub(crate::os::monotonic_ns()),
            ));
        } else {
            #[cfg(feature = "testing")]
            crate::sequencer::PARKS[2].fetch_add(1, Ordering::Relaxed);
            std::thread::park();
        }
        pool.wake(index);
    }
}

fn run_workers(run: RunPtr, root: TaskPtr, pool: &Pool) {
    let stop_fd = crate::fiber::stop_fd();
    std::thread::scope(|scope| {
        for index in 1..pool.queues.len() {
            scope.spawn(move || run.worker(root, pool, index, stop_fd));
        }
        run.worker(root, pool, 0, stop_fd);
    });
}

#[repr(C)]
struct Spawned {
    task: *mut Task,
    available: u64,
}
/// # Safety
/// Layout is emitter-produced. Input is initialized X and moves only on success;
/// body consumes it and initializes R only on success. Handle stays with parent.
unsafe fn spawn_in(
    stack: &mut Option<Stack>,
    context: *mut Context,
    bytes: u64,
    input: *mut u8,
    body: Body,
    layout: TaskLayout,
    group: Option<crate::tape::schedule::Group>,
) -> Spawned {
    // SAFETY: spawn is invoked by the current parent in its exclusive context.
    unsafe {
        crate::process::deny_describe_capability(context);
        let _commit = before_commit();
        let mut record =
            match Block::new(&*context, charge::task(layout.result.logical, bytes), 1, 8) {
                Ok(record) => record,
                Err(Capacity { available, .. }) => {
                    return Spawned {
                        task: ptr::null_mut(),
                        available,
                    };
                }
            };
        let parent = CURRENT.get();
        let run = (*parent).run;
        let id = (*run).id();
        let child_context = abi::task_child(context, &mut record, bytes, id);
        let task = Box::new(Task {
            arc_runtime: false,
            arc_hooks: true,
            cancel: AtomicBool::new(false),
            replay: false,
            latest_pair: false,
            polls_left: POLLS_PER_TURN,
            context: ptr::null_mut(),
            owned_context: Some(child_context),
            run,
            ready: ptr::null_mut(),
            domain: RESIDUAL_DOMAIN,
            worker: None,
            #[cfg(feature = "testing")]
            enqueued_at: 0,
            waiters: Waiters::default(),
            waiting_on: ptr::null_mut(),
            wait_previous: ptr::null_mut(),
            wait_next: ptr::null_mut(),
            timer_next: ptr::null_mut(),
            timer_deadline: None,
            operation: ptr::null_mut(),
            all: (*run).all,
            state: State::Ready,
            id,
            group: group.map(|group| group.child()),
            stack: stack.take(),
            sp: ptr::null_mut(),
            body,
            input: Storage::new(layout.input.bytes, layout.input.align),
            result: Storage::new(layout.result.bytes, layout.result.align),
            layout,
            failed: 0,
            record: Some(record),
            holder: std::mem::MaybeUninit::uninit(),
            cursor: 0,
        });
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::allocated((&*task as *const Task).cast(), size_of::<Task>());
        let task = Box::into_raw(task);
        (*task).arc_runtime =
            (*run).pool.is_some() && ((*task).group.is_some() || (*run).arc_observed());
        if let Some(group) = (*task).group {
            (*task).replay = group.replaying();
            (*task).arc_runtime |= (*task).replay;
            if let Some(members) = (*run).tapes.membership(group) {
                *members += 1;
            }
        }
        (*task).arc_hooks = (*run).arc_hooks(&*task);
        (*task).context = (*task).owned_context.as_mut().unwrap();
        (*(*task).context).events = if (*(*task).context).events != 0 {
            task.expose_provenance()
        } else {
            0
        };
        ptr::copy_nonoverlapping(input, (*task).input.as_ptr(), layout.input.bytes);
        (*task).sp = (*task)
            .stack
            .as_ref()
            .unwrap()
            .prepare(start, task.cast(), ptr::null_mut());
        (*run).all = task;
        #[cfg(feature = "testing")]
        if (*run).measure {
            (*task).enqueued_at = crate::os::monotonic_ns();
        }
        (*run).ready(task);
        event(SPAWN, (*task).id, bytes);
        Spawned { task, available: 0 }
    }
}
#[repr(C)]
pub struct Spawn {
    pub task: *mut Task,
    pub available: u64,
    pub status: u8,
}
/// Spawn through the supplied Tasks capability's provider. The reservation
/// admitted by spawn is the child's fresh accounting boundary (CONC-3).
/// # Safety
/// Same operands as `hirsel_core_spawn`, plus a live Tasks grant of this run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_spawn(
    context: *mut Context,
    tasks: u64,
    bytes: u64,
    input: *mut u8,
    body: Body,
    layout: TaskLayout,
    site: u64,
) -> Spawn {
    // SAFETY: generated operands belong to the active parent and run.
    unsafe {
        // The stock is worker-private, so a fresh stack is mapped outside the
        // commit, and a refused spawn returns it after release.
        let stock = worker_stock((*CURRENT.get()).run);
        let worker = WORKER.get();
        let mut stack = Some(if worker.is_null() {
            (*stock).take()
        } else {
            (*stock).take_shared(&(*(*worker).pool).spare)
        });
        let spawn = spawn_committed(context, tasks, bytes, input, body, layout, site, &mut stack);
        if let Some(stack) = stack {
            (*stock).recycle(stack);
        }
        spawn
    }
}

/// # Safety
/// As for `hirsel_core_spawn`; stack is taken only by a successful spawn.
#[allow(clippy::too_many_arguments)]
unsafe fn spawn_committed(
    context: *mut Context,
    tasks: u64,
    bytes: u64,
    input: *mut u8,
    body: Body,
    layout: TaskLayout,
    site: u64,
    stack: &mut Option<Stack>,
) -> Spawn {
    // SAFETY: the caller passes its generated operands through unchanged.
    unsafe {
        let _commit = crate::sequencer::enter();
        let group = if tasks & crate::host_file::DYNAMIC_GRANT != 0 {
            (*(*context).host_files).registry.grant_group(tasks)
        } else if tasks == 0 && (*context).run.is_null() {
            current_group()
        } else {
            None
        };
        if let Some(group) = group
            && let Err(f) = group.check()
        {
            crate::tape::refuse(f);
            causes::record(&mut *context, Cause::new(causes::TAPE, site));
            return Spawn {
                task: ptr::null_mut(),
                available: 0,
                status: 1,
            };
        }
        let spawned = spawn_in(stack, context, bytes, input, body, layout, group);
        Spawn {
            task: spawned.task,
            available: spawned.available,
            status: 0,
        }
    }
}

/// # Safety
/// Live unjoined handle owned by this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_cancel(_: *mut Context, task: *mut Task) {
    // SAFETY: parent owns the child's handle and requests without touching its stack.
    unsafe {
        let _commit = before_commit();
        if let Some(group) = (*task).group
            && !current_group().is_some_and(|source| source.same(group))
        {
            if (*task).state != State::Ended {
                (*(*task).run).outside_cancels.push(task);
            }
            event(CANCEL, (*task).id, 0);
            return;
        }
        cancel_flag(task).store(true, Ordering::Release);
        if (*task).state == State::Provider {
            (*(*task).run).cancels.push(task);
        }
        let source = CURRENT.get();
        if !source.is_null() {
            record_event(source, CANCEL, (*task).id, 0);
        }
        (*(*task).run).wake_cancelled(task);
        if !source.is_null() {
            poll_event(source, CANCEL, 0);
        }
    }
}
unsafe fn retire(task: *mut Task) {
    let _commit = crate::sequencer::enter();
    // SAFETY: task ended, is not queued, and its sole handle is being consumed.
    unsafe {
        let run = (*task).run;
        (*run).outside_cancels.retain(|&t| t != task);
        let mut link = &raw mut (*run).all;
        while *link != task {
            link = &raw mut (**link).all;
        }
        *link = (*task).all;
        let mut owner = Box::from_raw(task);
        // Ended context returns unused primary before record/result refunds.
        drop(owner.owned_context.take());
        drop(owner.record.take());
        drop(owner);
    }
}
#[repr(C)]
pub struct Join {
    pub kind: u8,
    pub causes: *mut Cell,
    pub status: u8,
}
/// # Safety
/// Live handle and writable R destination; caller consumes the handle only when
/// status is zero. On cancellation it remains owned for failure cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_join(
    context: *mut Context,
    task: *mut Task,
    result: *mut u8,
    site: u64,
) -> Join {
    // SAFETY: parent owns task throughout waits, no references survive switches.
    unsafe {
        let mut _resumed = None;
        let _commit = loop {
            let commit = before_commit();
            if check_cancel(context, site) != 0 {
                return Join {
                    kind: 0,
                    causes: ptr::null_mut(),
                    status: 1,
                };
            }
            if (*task).state == State::Ended {
                break commit;
            }
            _resumed = Some(park(&raw mut (*task).waiters));
        };
        let failed = (*task).failed != 0;
        let causes = std::mem::take(&mut (*(*task).context).causes);
        let cancelled = causes
            .iter()
            .next()
            .is_some_and(|cause| cause.kind() == causes::CANCELLED);
        if !failed {
            ptr::copy_nonoverlapping((*task).result.as_ptr(), result, (*task).layout.result.bytes);
        }
        event(JOIN, (*task).id, u64::from(failed));
        retire(task);
        Join {
            kind: if cancelled { 2 } else { u8::from(failed) },
            causes: causes.into_raw(),
            status: 0,
        }
    }
}
/// Cancel the task and wait for it to end, under shielding; its result (if it
/// succeeded) is then this cleanup's alone.
unsafe fn abandon_begin(context: *mut Context, task: *mut Task) -> *mut crate::counted::Holder {
    // SAFETY: the parent retains task while blocked; cancellation shielding is lexical.
    unsafe {
        (*context).cancel_shield += 1;
        hirsel_core_cancel(context, task);
        let mut _resumed = None;
        loop {
            let _commit = before_commit();
            if (*task).state == State::Ended {
                break;
            }
            _resumed = Some(park(&raw mut (*task).waiters));
        }
        event(JOIN, (*task).id, 2);
        (*task).cursor = 0;
        (*task).holder.as_mut_ptr()
    }
}
/// Clean the result, if any, then drop the child's cause chain and retire it.
unsafe fn abandon_step(
    context: *mut Context,
    holder: *mut crate::counted::Holder,
    list: *mut crate::counted::Worklist,
) -> bool {
    // SAFETY: the frame is the ended task's, whose sole handle is being consumed.
    unsafe {
        let task = holder
            .cast::<u8>()
            .sub(std::mem::offset_of!(Task, holder))
            .cast::<Task>();
        if (*task).failed == 0
            && let Some(clean) = (*task).layout.result.clean
            && !clean(context, (*task).result.as_ptr(), &mut (*task).cursor, list)
        {
            return false;
        }
        let causes: Causes = std::mem::take(&mut (*(*task).context).causes);
        drop(causes);
        retire(task);
        (*context).cancel_shield -= 1;
        true
    }
}
/// # Safety
/// Transfer the sole live handle. This cleanup shields cancellation, cancels and
/// waits, then cleans any result or drops its complete cause chain, without admission.
pub unsafe fn abandon(context: *mut Context, task: *mut Task) -> u8 {
    // SAFETY: as abandon_begin; the drain runs the cleanup to its end.
    unsafe {
        let holder = abandon_begin(context, task);
        u8::from(crate::counted::drain_holder(
            context,
            holder,
            abandon_step,
            0,
        ))
    }
}
/// # Safety
/// Same ownership contract as abandon.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_task_drop(context: *mut Context, task: *mut Task) -> u8 {
    // SAFETY: emitter transfers one unjoined handle for structured cleanup.
    unsafe { abandon(context, task) }
}
/// Abandon the task inside a running drain: wait for it to end, then hand its
/// cleanup over at `point`. Always stages, so always returns true.
/// # Safety
/// As abandon; `list` is the running drain's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_task_release(
    context: *mut Context,
    task: *mut Task,
    list: *mut crate::counted::Worklist,
    point: u64,
) -> bool {
    // SAFETY: as abandon.
    unsafe {
        let holder = abandon_begin(context, task);
        (*list).stage_holder(holder, abandon_step, point);
        true
    }
}

/// Run non-suspending OS code off the language fiber. Scripted provider issue and
/// delivery callbacks stay on their task stacks and may suspend independently.
/// # Safety
/// Operation does not suspend/re-enter Hirsel; state/request stay pinned.
pub(crate) unsafe fn host_operation(
    state: *mut u8,
    request: *mut crate::provider::Request,
    operation: unsafe extern "C" fn(
        *mut u8,
        *mut crate::provider::Request,
    ) -> crate::provider::Outcome,
) -> crate::provider::Outcome {
    struct Call {
        state: *mut u8,
        request: *mut crate::provider::Request,
        operation: unsafe extern "C" fn(
            *mut u8,
            *mut crate::provider::Request,
        ) -> crate::provider::Outcome,
        saved: *mut u8,
        result: std::mem::MaybeUninit<crate::provider::Outcome>,
    }
    unsafe extern "C" fn invoke(first: *mut u8, _: *mut u8) -> ! {
        // SAFETY: caller pins this call frame; OS operation cannot suspend.
        unsafe {
            let call = first.cast::<Call>();
            (*call)
                .result
                .write(((*call).operation)((*call).state, (*call).request));
            let mut discard = ptr::null_mut();
            hirsel_stack::switch(&mut discard, (*call).saved);
            std::hint::unreachable_unchecked();
        }
    }
    let task = CURRENT.get();
    // SAFETY: task owns the shared host stack until this non-suspending call completes.
    unsafe {
        if task.is_null() {
            return operation(state, request);
        }
        let run = (*task).run;
        let mut call = Call {
            state,
            request,
            operation,
            saved: ptr::null_mut(),
            result: std::mem::MaybeUninit::uninit(),
        };
        let host = (*worker_stock(run)).take();
        let target = host.prepare(invoke, (&mut call as *mut Call).cast(), ptr::null_mut());
        crate::fiber::guard(host.bounds());
        hirsel_stack::switch(&mut call.saved, target);
        crate::fiber::guard((*task).stack.as_ref().unwrap().bounds());
        (*worker_stock(run)).recycle(host);
        call.result.assume_init()
    }
}

#[cfg(any(test, feature = "charge-observation"))]
impl Drop for Task {
    fn drop(&mut self) {
        crate::peak::deallocated((self as *const Self).cast());
    }
}

#[cfg(test)]
mod ready_routing_laws {
    use super::*;
    use crate::provider::*;
    use std::{io::Write, os::unix::net::UnixStream};
    unsafe extern "C" fn no_body(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
        0
    }
    unsafe extern "C" fn wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
        Wait::default()
    }
    unsafe extern "C" fn resume(state: *mut u8, _: *mut Request, _: u64) -> Step {
        // SAFETY: the fixture pins its two counters until every callback ends.
        unsafe {
            (*state.cast::<[usize; 2]>())[0] += 1;
        }
        Step::default()
    }
    unsafe extern "C" fn deliver(state: *mut u8, _: *mut Request, outcome: Outcome) -> Outcome {
        // SAFETY: as resume.
        unsafe {
            (*state.cast::<[usize; 2]>())[1] += 1;
        }
        outcome
    }
    unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
        Outcome::default()
    }
    #[test]
    fn ready_routes_one_and_work_is_independent_of_parked_count() {
        for n in [3, 10_000] {
            let mut counters = [0usize; 2];
            let provider = Provider {
                state: counters.as_mut_ptr().cast(),
                wait,
                issue: resume,
                resume,
                abort,
                deliver,
            };
            let mut request = Request {
                operation: RECV_BOUNDED,
                mode: 0,
                handle: 0,
                path: ptr::null(),
                path_len: 0,
                buffer: Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                },
                site: 0,
            };
            let mut run = Scheduler {
                pool: None,
                deferred: ptr::null_mut(),
                deferred_tail: ptr::null_mut(),
                stacks: Stock::default(),
                idle: ptr::null_mut(),
                head: ptr::null_mut(),
                tail: ptr::null_mut(),
                timers: ptr::null_mut(),
                all: ptr::null_mut(),
                next: 1,
                provider: SchedulerProvider::default(),
                script: None,
                script_groups: Vec::new(),
                readies: Readies::new(),
                ready_tokens: Vec::new(),
                ready_arms: Vec::new(),
                #[cfg(feature = "testing")]
                measure: false,
                cancels: Vec::new(),
                outside_cancels: Vec::new(),
                tapes: Tapes::default(),
                net: ptr::null_mut(),
                model: None,
                parked: WaitArms::default(),
            };
            let mut context = Context::new(1 << 20);
            let layout = ValueLayout {
                bytes: 0,
                align: 1,
                logical: 0,
                clean: None,
            };
            let mut operations: Vec<_> = (0..n)
                .map(|i| Operation {
                    provider,
                    request: &mut request,
                    token: i as u64 + 1,
                    state: OperationState::Issued { deadline: 0 },
                    observes_cancel: false,
                    entered: None,
                })
                .collect();
            let mut tasks: Vec<_> = (0..n)
                .map(|i| {
                    Box::new(Task {
                        arc_runtime: false,
                        arc_hooks: true,
                        cancel: AtomicBool::new(false),
                        replay: false,
                        latest_pair: false,
                        polls_left: POLLS_PER_TURN,
                        context: &mut context,
                        owned_context: None,
                        run: &mut run,
                        ready: ptr::null_mut(),
                        domain: RESIDUAL_DOMAIN,
                        worker: None,
                        #[cfg(feature = "testing")]
                        enqueued_at: 0,
                        waiters: Waiters::default(),
                        waiting_on: ptr::null_mut(),
                        wait_previous: ptr::null_mut(),
                        wait_next: ptr::null_mut(),
                        timer_next: ptr::null_mut(),
                        timer_deadline: None,
                        operation: &mut operations[i],
                        all: ptr::null_mut(),
                        state: State::Provider,
                        id: i as u64,
                        group: None,
                        stack: Some(Stack::new()),
                        sp: ptr::null_mut(),
                        body: no_body,
                        input: Storage::new(0, 1),
                        result: Storage::new(0, 1),
                        layout: TaskLayout {
                            input: layout,
                            result: layout,
                        },
                        failed: 0,
                        record: None,
                        holder: std::mem::MaybeUninit::uninit(),
                        cursor: 0,
                    })
                })
                .collect();
            for i in 0..n - 1 {
                tasks[i].all = &mut *tasks[i + 1];
            }
            run.all = &mut *tasks[0];
            let mut sockets = hirsel_core_net::Sockets::default();
            let mut keys = Vec::new();
            let mut tags = Vec::new();
            let mut peers = Vec::new();
            for (i, task) in tasks.iter_mut().enumerate() {
                run.parked.insert(i as u64 + 1, &mut **task);
                let (socket, peer) = UnixStream::pair().unwrap();
                socket.set_nonblocking(true).unwrap();
                let key = sockets.insert(socket.into()).unwrap();
                tags.push(sockets.arm(key, false, i as u64 + 1).unwrap());
                keys.push(key);
                peers.push(peer);
            }
            // Registration queues writable edges even though every arm waits
            // for a read. Drain these before making the selected socket ready.
            while sockets
                .poll(0, |_| panic!("readable before a peer wrote"))
                .unwrap()
                != 0
            {}
            run.net = &mut sockets;
            assert!(run.parked.peek(1).is_some());
            let selected = if n == 3 { 1 } else { n - 1 };
            peers[selected].write_all(b"x").unwrap();
            ROUTE_WORK.set(0);
            ROUTE_WAKES.set(0);
            // SAFETY: task/operation frames and callback storage are pinned; no fiber runs.
            unsafe {
                run.take_wakes(0);
            }
            let work = ROUTE_WORK.get();
            println!(
                "ready_routes_one parked={n} route_key_operations={work} wakes={} resumes={}",
                ROUTE_WAKES.get(),
                counters[0]
            );
            assert_eq!(counters[0], 1);
            assert_eq!(ROUTE_WAKES.get(), 1);
            assert_eq!(run.head, &mut *tasks[selected] as *mut Task);
            for (i, task) in tasks.iter().enumerate() {
                assert!(
                    task.state
                        == if i == selected {
                            State::Ready
                        } else {
                            State::Provider
                        }
                );
            }
            if let OperationState::ReadyToDeliver(Some(outcome)) = operations[selected].state {
                // SAFETY: selected operation's task is now runnable for this one delivery.
                unsafe {
                    (provider.deliver)(provider.state, &mut request, outcome);
                }
            } else {
                panic!("the ready operation did not deliver");
            }
            assert_eq!(counters, [1, 1]);
            assert_eq!(run.parked.live.len(), n - 1, "entry_kept_after_wake");
            assert!(
                work <= 8,
                "ready routing scanned parked tasks: {work} for {n}"
            );
            if n == 3 {
                // First-arrival dedup and absent tokens are resolved before any resume.
                for token in [2, 1, 1, 3, u64::MAX] {
                    run.readies.push(token);
                }
                unsafe {
                    run.take_wakes(0);
                }
                assert_eq!(ROUTE_WAKES.get(), 3, "dedup_dropped");
                assert_eq!(counters[0], 3);
                assert!(run.parked.live.is_empty());
                for i in [0, 2] {
                    if let OperationState::ReadyToDeliver(Some(outcome)) = operations[i].state {
                        unsafe {
                            (provider.deliver)(provider.state, &mut request, outcome);
                        }
                    }
                }
                assert_eq!(counters, [3, 3]);
                unsafe { while !run.pop().is_null() {} }
                let stale = tags[0];
                sockets.release(keys[0]);
                let (socket, mut peer) = UnixStream::pair().unwrap();
                socket.set_nonblocking(true).unwrap();
                let replacement = sockets.insert(socket.into()).unwrap();
                assert_eq!((keys[0] >> 2) as u32, (replacement >> 2) as u32);
                assert!(replacement >> 34 > keys[0] >> 34);
                operations[0].token = 4;
                operations[0].state = OperationState::Issued { deadline: 0 };
                tasks[0].state = State::Provider;
                run.parked.insert(4, &mut *tasks[0]);
                sockets.arm(replacement, false, 4).unwrap();
                sockets.dispatch(stale, libc::EPOLLIN as u32, |token| run.readies.push(token));
                unsafe {
                    run.take_wakes(0);
                }
                assert_eq!(ROUTE_WAKES.get(), 3, "route_ignores_generation");
                assert_eq!(run.parked.live.len(), 1);
                peer.write_all(b"new").unwrap();
                unsafe {
                    run.take_wakes(0);
                }
                assert_eq!(ROUTE_WAKES.get(), 4);
                assert_eq!(counters[0], 4);
                assert!(run.parked.live.is_empty());
            } else {
                ROUTE_WORK.set(0);
                for token in 1..=n as u64 {
                    run.readies.push(token);
                    run.readies.push(token);
                }
                unsafe {
                    run.take_wakes(0);
                }
                println!(
                    "ready_dedup tokens={} route_key_operations={}",
                    2 * n,
                    ROUTE_WORK.get()
                );
                assert_eq!(
                    ROUTE_WAKES.get(),
                    n,
                    "linear dedup wakes each parked operation once"
                );
                assert_eq!(counters[0], n);
                assert!(run.parked.live.is_empty());
                assert!(
                    ROUTE_WORK.get() <= 16 * n,
                    "wide-turn dedup must do linear key work"
                );
            }
        }
    }
}
