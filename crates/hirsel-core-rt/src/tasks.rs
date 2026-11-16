//! Single-worker cooperative tasks. Queue links, wait state and result storage
//! are prepaid at spawn. Only blocking or explicit checkpoints switch stacks.
use crate::{
    Capacity, Context, abi_tasks as abi,
    block::Block,
    causes::{self, Cause, Causes, Cell},
    fiber::Stack,
};
use hirsel_core_layout::charge;
use std::{
    alloc::{Layout, alloc, dealloc},
    cell::Cell as Local,
    ptr::{self, NonNull},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ValueLayout {
    pub bytes: usize,
    pub align: usize,
    pub logical: u64,
    pub drop: unsafe extern "C" fn(*mut Context, *mut u8) -> u8,
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

pub const SPAWN: u8 = 0;
pub const END: u8 = 1;
pub const CANCEL: u8 = 2;
pub const CANCELLED: u8 = 3;
pub const JOIN: u8 = 4;
pub const PUBLISH: u8 = 5;
pub const RECEIVE: u8 = 6;
pub const TRY_SEND: u8 = 7;
pub const TRY_RECEIVE: u8 = 10;
pub const SENDERS: u8 = 8;
pub const CLOSE: u8 = 9;
pub const ARC_NEW: u8 = crate::abi_layout::ARC_EVENT_NEW;
pub const ARC_SHARE: u8 = crate::abi_layout::ARC_EVENT_SHARE;
pub const ARC_RELEASE: u8 = crate::abi_layout::ARC_EVENT_RELEASE;
pub const ARC_UNWRAP: u8 = crate::abi_layout::ARC_EVENT_UNWRAP;
pub const REFUND: u8 = 14;
pub const CHECK_CANCEL: u8 = 16;
pub const CAPACITY: u8 = 17;
/// CONC-13: a `Latest` root replaced, or read. Neither records a value.
pub const LATEST_PUBLISH: u8 = 18;
pub const LATEST_READ: u8 = 19;
/// Only no-effect polls count; effectful operations never consume this allowance.
pub(crate) const POLLS_PER_TURN: u32 = 64;
pub const CHARGE: u8 = 15;
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u8,
    pub task: u64,
    pub object: u64,
    pub value: u64,
}
/// Event callbacks record completed transitions and must not suspend/re-enter.
/// The choice callback selects a runnable queue position. Null uses FIFO.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SchedulerProvider {
    pub state: *mut u8,
    pub event: Option<unsafe extern "C" fn(*mut u8, Event)>,
    pub choose: Option<unsafe extern "C" fn(*mut u8, usize) -> usize>,
}
impl Default for SchedulerProvider {
    fn default() -> Self {
        Self {
            state: ptr::null_mut(),
            event: None,
            choose: None,
        }
    }
}
thread_local! { static CURRENT: Local<*mut Task> = const { Local::new(ptr::null_mut()) }; }
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Blocked,
    /// Parked in a provider operation's W or I. Only that operation's own wakes
    /// (its timer, its token, a cancellation request) move it; `wake` does not.
    Provider,
    Ended,
}
/// The first byte is the emitted Arc gate (TASK_ARC_GATE), which generated
/// code reaches through `Context::events`.
#[repr(C)]
pub struct Task {
    /// Nonzero while an Arc new, share or release hook has work. Without a
    /// sink the only work is delivering this task's refunds (MEM-20), so this
    /// is the task's refund notification flag. With a sink every hook records,
    /// so it stays set and `recorded_refunds` is the notification flag.
    pub(crate) arc_gate: AtomicBool,
    recorded_refunds: AtomicBool,
    refunds: Option<crate::limit::RefundSubscription>,
    polls_left: u32,
    context: *mut Context,
    owned_context: Option<Context>,
    run: *mut Scheduler,
    ready: *mut Task,
    timer_next: *mut Task,
    /// Host CLOCK_MONOTONIC nanoseconds.
    timer_deadline: Option<u64>,
    operation: *mut Operation,
    all: *mut Task,
    state: State,
    id: u64,
    stack: Stack,
    sp: *mut u8,
    body: Body,
    input: Storage,
    result: Storage,
    layout: TaskLayout,
    failed: u8,
    record: Option<Block>,
}
struct Scheduler {
    deferred: *mut Task,
    deferred_tail: *mut Task,
    // The guarded foreign-call stack is needed only when a run issues a host
    // operation. Keeping it lazy avoids committing its startup pages for the
    // ordinary, host-free path.
    host: Option<Stack>,
    idle: *mut u8,
    head: *mut Task,
    tail: *mut Task,
    timers: *mut Task,
    all: *mut Task,
    next: u64,
    provider: SchedulerProvider,
    readies: Arc<Readies>,
    /// Cancellation requests for tasks parked in a provider operation, in
    /// request order, taken at the next scheduler turn.
    cancels: Vec<*mut Task>,
    /// Tasks in State::Blocked. Only `checkpoint(true)` blocks a task and only
    /// `wake` unblocks, readying every one, so a wake with none is a no-op.
    blocked: usize,
}
const HOST_STACK_BYTES: usize = 64 << 10;
impl Task {
    /// The flag foreign refund publication sets for this task (see `arc_gate`).
    #[inline(always)]
    fn refund_flag(&self, recording: bool) -> &AtomicBool {
        if recording {
            &self.recorded_refunds
        } else {
            &self.arc_gate
        }
    }
}
impl Scheduler {
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
            (*task).ready = ptr::null_mut();
            if self.tail.is_null() {
                self.head = task;
            } else {
                (*self.tail).ready = task;
            }
            self.tail = task;
        }
    }
    unsafe fn pop(&mut self) -> *mut Task {
        // Pollers wait until the already-runnable peers have had a turn. This
        // also prevents an adversarial replay chooser alternating two pollers
        // forever while a third ready task never performs an event.
        if self.head.is_null() {
            self.head = std::mem::replace(&mut self.deferred, ptr::null_mut());
            self.tail = std::mem::replace(&mut self.deferred_tail, ptr::null_mut());
        }
        let mut count = 0;
        let mut at = self.head;
        // SAFETY: run queue links belong exclusively to the idle scheduler.
        unsafe {
            while !at.is_null() {
                count += 1;
                at = (*at).ready;
            }
            if count == 0 {
                return ptr::null_mut();
            }
            let choice = self
                .provider
                .choose
                .map_or(0, |choose| choose(self.provider.state, count));
            let mut link = &raw mut self.head;
            let mut before = ptr::null_mut();
            for _ in 0..choice {
                before = *link;
                link = &raw mut (**link).ready;
            }
            let task = *link;
            *link = (*task).ready;
            if self.tail == task {
                self.tail = before;
            }
            (*task).ready = ptr::null_mut();
            task
        }
    }
    #[inline(always)]
    unsafe fn wake(&mut self) {
        if self.blocked != 0 {
            // SAFETY: forwarded caller contract.
            unsafe { self.wake_blocked() }
        }
    }
    /// Ready every blocked task, in creation-list order.
    #[inline(never)]
    unsafe fn wake_blocked(&mut self) {
        self.blocked = 0;
        let mut task = self.all;
        // SAFETY: no task is executing while these single-worker links change.
        unsafe {
            while !task.is_null() {
                if (*task).state == State::Blocked {
                    self.remove_timer(task);
                    self.ready(task);
                }
                task = (*task).all;
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
    unsafe fn take_wakes(&mut self) {
        // SAFETY: no task is running; every parked operation's frame is pinned.
        unsafe {
            if !self.cancels.is_empty() {
                for task in std::mem::take(&mut self.cancels) {
                    if (*task).state == State::Provider {
                        self.wake_operation(task, Wake::Cancel);
                    }
                }
            }
            if self.readies.pending.swap(false, Ordering::Acquire) {
                let mut tokens = std::mem::take(&mut *self.readies.lock());
                // Readies of one token in one turn are one wake, at its first
                // ready; wakes run in the order the readies arrived.
                let mut at = 0;
                while at < tokens.len() {
                    if tokens[..at].contains(&tokens[at]) {
                        tokens.remove(at);
                    } else {
                        at += 1;
                    }
                }
                for token in tokens {
                    let mut task = self.all;
                    while !task.is_null() {
                        let operation = (*task).operation;
                        if !operation.is_null() && (*operation).token == token {
                            self.wake_operation(task, Wake::Ready);
                            break;
                        }
                        task = (*task).all;
                    }
                }
            }
            if self.timers.is_null() {
                return;
            }
            let now = crate::os::monotonic_ns();
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

    fn idle(&mut self) {
        if self.readies.pending.load(Ordering::Acquire) || !self.cancels.is_empty() {
            return;
        }
        // SAFETY: no task is running while the idle scheduler waits.
        let deadline = unsafe { self.timers.as_ref().and_then(|task| task.timer_deadline) };
        // The scheduler thread waits here, never in a provider: readiness from
        // any thread unparks it (hirsel_core_ready).
        match deadline {
            Some(deadline) => std::thread::park_timeout(Duration::from_nanos(
                deadline.saturating_sub(crate::os::monotonic_ns()),
            )),
            None => std::thread::park(),
        }
    }

    unsafe fn runnable(&mut self, task: *mut Task, next: Next) {
        // SAFETY: the task is parked; its operation frame is pinned.
        unsafe {
            self.remove_timer(task);
            (*(*task).operation).phase = Phase::Runnable;
            (*(*task).operation).next = next;
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
            // A request that may not take effect now changes nothing: no wait,
            // no abort, no issue skipped. It stays pending for the task's next
            // cancellation point (CONC-7, FAIL-11, CONC-8).
            if wake == Wake::Cancel && !cancels_operation(task) {
                return false;
            }
            let op = (*task).operation;
            let p = (*op).provider;
            let (request, token) = ((*op).request, (*op).token);
            match (*op).phase {
                // Queued or running: its issue has not been called yet, or its
                // outcome is decided. A ready taken now has nothing to report;
                // readies the provider gives while issue runs are taken after
                // the operation parks in I.
                Phase::Runnable => false,
                Phase::Entry | Phase::Waiting => {
                    let w = match (*op).entered.take() {
                        Some(w) => w,
                        None => {
                            #[cfg(test)]
                            if (*op).phase == Phase::Entry {
                                FIRST_WAITS.set((FIRST_WAITS.get().0, FIRST_WAITS.get().1 + 1));
                            }
                            (p.wait)(p.state, request, token)
                        }
                    };
                    if wake == Wake::Cancel {
                        // W -> C (CONC-7): the request fails the operation and
                        // issue is never called, whatever wait answered.
                        self.runnable(task, Next::Cancelled);
                        return false;
                    }
                    match w.kind {
                        WAIT_READY => {
                            if (*op).phase == Phase::Entry {
                                (*op).phase = Phase::Runnable;
                                (*op).next = Next::Issue;
                                return true;
                            }
                            self.runnable(task, Next::Issue);
                            return false;
                        }
                        WAIT_UNTIL => {
                            (*op).phase = Phase::Waiting;
                            self.remove_timer(task);
                            self.insert_timer(task, w.deadline_ns);
                        }
                        WAIT_PARK => {
                            (*op).phase = Phase::Waiting;
                            self.remove_timer(task);
                        }
                        _ => crate::process::stop(b"Provider"),
                    }
                    if wake == Wake::Parked {
                        self.settle(task);
                    }
                    false
                }
                Phase::Issued => {
                    match wake {
                        Wake::Parked => self.settle(task),
                        Wake::Ready => {
                            let step = (p.resume)(p.state, request, token);
                            self.stepped(task, step);
                        }
                        Wake::Cancel => {
                            let outcome = (p.abort)(p.state, request, token, ABORT_CANCEL);
                            self.runnable(task, Next::Deliver(outcome));
                        }
                        Wake::Timer => {
                            let outcome = (p.abort)(p.state, request, token, ABORT_DEADLINE);
                            self.runnable(task, Next::Deliver(outcome));
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
                STEP_DONE => self.runnable(task, Next::Deliver(step.outcome)),
                STEP_PENDING => {
                    self.remove_timer(task);
                    (*(*task).operation).deadline = step.deadline_ns;
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
            if cancels_operation(task) {
                self.wake_operation(task, Wake::Cancel);
                if (*op).phase == Phase::Runnable {
                    return;
                }
            }
            if (*op).phase == Phase::Issued
                && (*op).deadline != 0
                && (*task).timer_deadline.is_none()
            {
                self.insert_timer(task, (*op).deadline);
            }
        }
    }
}

/// A provider operation in W or I. It lives in the driver's frame on the task's
/// stack, and the scheduler reaches it through `Task::operation` while parked.
pub(crate) struct Operation {
    provider: crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    phase: Phase,
    /// The deadline of the step that left the operation in I; 0 is none.
    deadline: u64,
    next: Next,
    /// Entry only: the first `wait`'s answer, Until or Park, which the task
    /// called itself before parking; None makes the scheduler call it (the
    /// test-only scheduler-switch path).
    entered: Option<crate::provider::Wait>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Parked after its first wait answered Until or Park; the scheduler
    /// applies that answer before any pop.
    Entry,
    /// W after Until or Park.
    Waiting,
    /// I after a Pending issue or resume.
    Issued,
    /// Not parked: running its issue or deliver, or queued to.
    Runnable,
}
#[derive(Clone, Copy)]
enum Next {
    Issue,
    Cancelled,
    Deliver(crate::provider::Outcome),
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
/// Whether a cancellation request reaches this parked operation now: the one
/// decision `take_wakes` (W and I) and `settle` ask, and `check_cancel` (the
/// entry and every other cancellation point) asks for the task.
unsafe fn cancels_operation(task: *mut Task) -> bool {
    // SAFETY: a parked task's context and its operation's request are pinned
    // and not running.
    unsafe {
        crate::provider::cancellation_point((*(*(*task).operation).request).operation)
            && cancel_takes_effect(&*(*task).context)
    }
}
/// Whether a cancellation request takes effect on this task now. A finalizer
/// and a shielded region observe none (CONC-7, tasks.md:129; FAIL-11,
/// failure.md:173; CONC-8, tasks.md:140): under either the request stays
/// pending and changes nothing, and it takes effect at the first cancellation
/// point after both end. A deadline is not cancellation and is not shielded.
#[inline(always)]
fn cancel_takes_effect(context: &Context) -> bool {
    context.cancel_requested.load(Ordering::Acquire)
        && context.finalizing == 0
        && context.cancel_shield == 0
}

/// Readiness tokens from `hirsel_core_ready`, which may run on any thread.
struct Readies {
    tokens: Mutex<Vec<u64>>,
    pending: AtomicBool,
    worker: std::thread::Thread,
}
impl Readies {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(Vec::new()),
            pending: AtomicBool::new(false),
            worker: std::thread::current(),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<u64>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn push(&self, token: u64) {
        self.lock().push(token);
        self.pending.store(true, Ordering::Release);
        self.worker.unpark();
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
        CURRENT.set(ptr::null_mut());
        let wait = C::wait(provider, request, token);
        CURRENT.set(task);
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
fn own_callbacks(identical: bool) -> bool {
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
        phase: Phase::Entry,
        deadline: 0,
        next: Next::Issue,
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
                None => match op.next {
                    Next::Issue => (op.provider.issue)(op.provider.state, request, token),
                    Next::Cancelled => break None,
                    Next::Deliver(outcome) => break Some(outcome),
                },
            };
            match step.kind {
                STEP_DONE => break Some(step.outcome),
                STEP_PENDING => {
                    op.phase = Phase::Issued;
                    op.deadline = step.deadline_ns;
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
        (*task).state = State::Provider;
        hirsel_stack::switch(&raw mut (*task).sp, (*(*task).run).idle);
    }
}
/// An operation with no scheduler (a direct ABI caller, or run teardown): no
/// other task exists, so this thread waits for the wake itself. Callbacks see
/// no task, as under a scheduler. Cancellation is checked only at entry: no
/// task exists to request it later.
unsafe fn alone(
    _context: *mut Context,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
) -> Option<crate::provider::Outcome> {
    let p = provider;
    // This thread's waiter is registered before the first callback, so a ready
    // from inside `wait` or `issue` is not lost. The token is fresh, so the
    // stale tokens cleared here are other operations'.
    let readies = ALONE.with(|r| r.0.clone());
    readies.take(0);
    // Wait until readiness (true) or the deadline (false).
    let block = |deadline: Option<u64>| -> bool {
        loop {
            if readies.pending.load(Ordering::Acquire) && readies.take(token) {
                return true;
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
    // SAFETY: current pins its scheduler; callback cannot suspend or re-enter.
    unsafe {
        let task = NonNull::new_unchecked(task_pointer).as_ref();
        let run = NonNull::new_unchecked(task.run).as_ref();
        // Without a sink the gate is the refund flag, so a quiet event reads
        // one byte; with one the gate stays set and the flag is separate.
        if task.arc_gate.load(Ordering::Acquire)
            && (run.provider.event.is_none() || task.recorded_refunds.load(Ordering::Acquire))
        {
            drain_refunds(task_pointer);
        }
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
        poll_event(task_pointer, kind, value);
    }
}
/// Complete a recorded inline operation without consulting task TLS.
/// # Safety
/// Context's immutable event word is nonzero and pins its current Task/run.
#[inline(always)]
pub(crate) unsafe fn event_in(context: *const Context, kind: u8, object: u64, value: u64) {
    // SAFETY: the emitted gate proves the word is this running task.
    unsafe { event_for((*context).events as *mut Task, kind, object, value) }
}
/// Empty event paths read only the pinned task flag; all mailbox traversal,
/// locking and flag reset belong to this cold, non-suspending path.
#[cold]
#[inline(never)]
unsafe fn drain_refunds(task: *mut Task) {
    // SAFETY: this task owns execution; subscription keeps its atomic flag
    // registered until before its Context can release the payer domain.
    unsafe {
        (*(*task).context).current.drain_notified();
    }
}
/// The one trigger for the spec's no-effect poll family. Record the result
/// before scheduling; never observe cancellation or change that result here.
#[inline(always)]
unsafe fn poll_event(task: *mut Task, kind: u8, value: u64) {
    if ((kind == TRY_SEND || kind == TRY_RECEIVE) && value == 1)
        || ((kind == ARC_UNWRAP || kind == CHECK_CANCEL) && value == 0)
        || kind == CAPACITY
    {
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
        if !(*run).head.is_null() || !(*run).deferred.is_null() {
            (*task).state = State::Ready;
            (*task).ready = ptr::null_mut();
            if (*run).deferred_tail.is_null() {
                (*run).deferred = task;
            } else {
                (*(*run).deferred_tail).ready = task;
            }
            (*run).deferred_tail = task;
            hirsel_stack::switch(&raw mut (*task).sp, (*run).idle);
        }
    }
}
/// Cold refusal only: the successful admission's instruction path is untouched.
/// Snapshot the Capacity before scheduling so the poll never changes its result.
#[cold]
#[inline(never)]
pub(crate) fn capacity_refused(requested: u64, available: u64) -> Capacity {
    event(CAPACITY, requested, available);
    Capacity {
        requested,
        available,
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
/// Whether a task is running on this thread (tests assert where callbacks run).
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
    unsafe { (*(*task).run).id() }
}
/// # Safety
/// Called on the active fiber, with no Rust references held to scheduler/task state.
pub unsafe fn checkpoint(blocked: bool) {
    let task = CURRENT.get();
    // Direct single-task ABI fixtures have no scheduler and never block.
    if task.is_null() {
        return;
    }
    // SAFETY: saving exactly this fiber; idle and task stacks stay mapped.
    unsafe {
        let run = (*task).run;
        if blocked {
            (*task).state = State::Blocked;
            (*run).blocked += 1;
        } else {
            (*run).ready(task);
        }
        hirsel_stack::switch(&raw mut (*task).sp, (*run).idle);
    }
}

/// `event` then `wake`, reading the current task once: a channel change that
/// can unblock peers records its event and then readies every blocked task.
#[inline(always)]
pub(crate) fn event_then_wake(kind: u8, object: u64, value: u64) {
    let task = CURRENT.get();
    if task.is_null() {
        return;
    }
    // SAFETY: the active task exclusively accesses its scheduler between
    // switches; the sink callback cannot suspend or re-enter.
    unsafe {
        event_for(task, kind, object, value);
        (*(*task).run).wake();
    }
}
#[inline]
pub(crate) fn wake() {
    let task = CURRENT.get();
    if task.is_null() {
        return;
    }
    // SAFETY: the active task exclusively accesses its scheduler between switches.
    unsafe {
        (*(*task).run).wake();
    }
}
/// # Safety
/// Context belongs to this task; primary is available on a normal path.
#[inline(always)]
pub unsafe fn check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the operation pins its exclusive, aligned task context. Pending
    // cancellation is the common false condition; shield/grant checks are cold.
    let current = unsafe { NonNull::new_unchecked(context).as_ref() };
    if cancel_takes_effect(current) {
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
        (*context).cancel_requested.store(false, Ordering::Release);
        causes::record(&mut *context, Cause::new(causes::CANCELLED, site));
        event(CANCELLED, 0, site);
    }
}
/// # Safety
/// Active context and available primary on a normal path. This is an explicit
/// checkpoint; production pure code never calls it implicitly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: observe entry before allowing any scheduler action.
    unsafe {
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
    // SAFETY: start owns this fiber. The body cleans its input/temporaries;
    // its result is initialized only on status zero.
    unsafe {
        (*task).failed = ((*task).body)(
            (*task).context,
            (*task).result.as_ptr(),
            (*task).input.as_ptr(),
        );
        if (*task).record.is_some() {
            abi::task_handover((*task).context);
        }
        event(END, (*task).id, (*task).failed as u64);
        (*task).state = State::Ended;
        (*(*task).run).wake();
        hirsel_stack::switch(&raw mut (*task).sp, (*(*task).run).idle);
        std::hint::unreachable_unchecked();
    }
}
unsafe extern "C" fn no_drop(_: *mut Context, _: *mut u8) -> u8 {
    0
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
    crate::fiber::arm();
    let mut run = Scheduler {
        deferred: ptr::null_mut(),
        deferred_tail: ptr::null_mut(),
        host: None,
        idle: ptr::null_mut(),
        head: ptr::null_mut(),
        tail: ptr::null_mut(),
        timers: ptr::null_mut(),
        all: ptr::null_mut(),
        next: 1,
        provider,
        readies: Readies::new(),
        cancels: Vec::new(),
        blocked: 0,
    };
    let registered = Registered(run.readies.clone());
    WAITERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(run.readies.clone());
    let descriptor = ValueLayout {
        bytes: input_bytes,
        align: 16,
        logical: 0,
        drop: no_drop,
    };
    let mut root = Box::new(Task {
        arc_gate: AtomicBool::new(run.provider.event.is_some()),
        recorded_refunds: AtomicBool::new(false),
        refunds: None,
        polls_left: POLLS_PER_TURN,
        context,
        owned_context: None,
        run: &mut run,
        ready: ptr::null_mut(),
        timer_next: ptr::null_mut(),
        timer_deadline: None,
        operation: ptr::null_mut(),
        all: ptr::null_mut(),
        state: State::Ready,
        id: 0,
        stack: Stack::new(),
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
    });
    #[cfg(any(test, feature = "charge-observation"))]
    crate::peak::allocated((&*root as *const Task).cast(), size_of::<Task>());
    // SAFETY: root's stable allocation and both stacks stay live through the loop.
    unsafe {
        ptr::copy_nonoverlapping(argument, root.input.as_ptr(), input_bytes);
        let root_pointer: *mut Task = &mut *root;
        root.refunds = Some((*context).current.subscribe_refunds(NonNull::from(
            root.refund_flag(run.provider.event.is_some()),
        )));
        (*context).events = root_pointer.expose_provenance();
        root.sp = root
            .stack
            .prepare(start, root_pointer.cast(), ptr::null_mut());
        run.all = root_pointer;
        run.ready(root_pointer);
        let previous = CURRENT.get();
        let mut direct = ptr::null_mut::<Task>();
        loop {
            if root.state == State::Ended {
                break;
            }
            let task = if direct.is_null() {
                run.take_wakes();
                let task = run.pop();
                if task.is_null() {
                    run.idle();
                    continue;
                }
                task
            } else {
                std::mem::replace(&mut direct, ptr::null_mut())
            };
            CURRENT.set(task);
            (*task).state = State::Running;
            abi::task_resume((*task).context);
            crate::fiber::guard((*task).stack.bounds());
            hirsel_stack::switch(&mut run.idle, (*task).sp);
            crate::fiber::guard((0, 0));
            CURRENT.set(previous);
            if (*task).state == State::Provider && run.wake_operation(task, Wake::Parked) {
                direct = task;
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

#[repr(C)]
pub struct Spawn {
    pub task: *mut Task,
    pub available: u64,
}
/// # Safety
/// Layout is emitter-produced. Input is initialized X and moves only on success;
/// body consumes it and initializes R only on success. Handle stays with parent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_spawn(
    context: *mut Context,
    bytes: u64,
    input: *mut u8,
    body: Body,
    layout: TaskLayout,
) -> Spawn {
    // SAFETY: spawn is invoked by the current parent in its exclusive context.
    unsafe {
        let mut record =
            match Block::new(&*context, charge::task(layout.result.logical, bytes), 1, 8) {
                Ok(record) => record,
                Err(Capacity { available, .. }) => {
                    return Spawn {
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
            arc_gate: AtomicBool::new((*run).provider.event.is_some()),
            recorded_refunds: AtomicBool::new(false),
            refunds: None,
            polls_left: POLLS_PER_TURN,
            context: ptr::null_mut(),
            owned_context: Some(child_context),
            run,
            ready: ptr::null_mut(),
            timer_next: ptr::null_mut(),
            timer_deadline: None,
            operation: ptr::null_mut(),
            all: (*run).all,
            state: State::Ready,
            id,
            stack: Stack::new(),
            sp: ptr::null_mut(),
            body,
            input: Storage::new(layout.input.bytes, layout.input.align),
            result: Storage::new(layout.result.bytes, layout.result.align),
            layout,
            failed: 0,
            record: Some(record),
        });
        #[cfg(any(test, feature = "charge-observation"))]
        crate::peak::allocated((&*task as *const Task).cast(), size_of::<Task>());
        let task = Box::into_raw(task);
        (*task).context = (*task).owned_context.as_mut().unwrap();
        (*task).refunds = Some((*(*task).context).current.subscribe_refunds(NonNull::from(
            (*task).refund_flag((*run).provider.event.is_some()),
        )));
        (*(*task).context).events = if (*(*task).context).events != 0 {
            task.expose_provenance()
        } else {
            0
        };
        ptr::copy_nonoverlapping(input, (*task).input.as_ptr(), layout.input.bytes);
        (*task).sp = (*task).stack.prepare(start, task.cast(), ptr::null_mut());
        (*run).all = task;
        (*run).ready(task);
        event(SPAWN, (*task).id, bytes);
        Spawn { task, available: 0 }
    }
}
/// # Safety
/// Live unjoined handle owned by this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_cancel(_: *mut Context, task: *mut Task) {
    // SAFETY: parent owns the child's handle and requests without touching its stack.
    unsafe {
        (*(*task).context)
            .cancel_requested
            .store(true, Ordering::Release);
        event(CANCEL, (*task).id, 0);
        if (*task).state == State::Provider {
            (*(*task).run).cancels.push(task);
        }
        wake();
    }
}
unsafe fn retire(task: *mut Task) {
    // SAFETY: task ended, is not queued, and its sole handle is being consumed.
    unsafe {
        let run = (*task).run;
        let mut link = &raw mut (*run).all;
        while *link != task {
            link = &raw mut (**link).all;
        }
        *link = (*task).all;
        let mut owner = Box::from_raw(task);
        // Unregister under the mailbox mutex before either flag or Context dies.
        drop(owner.refunds.take());
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
        loop {
            if check_cancel(context, site) != 0 {
                return Join {
                    kind: 0,
                    causes: ptr::null_mut(),
                    status: 1,
                };
            }
            if (*task).state == State::Ended {
                break;
            }
            checkpoint(true);
        }
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
/// # Safety
/// Transfer the sole live handle. This cleanup shields cancellation, cancels and
/// waits, then cleans any result or drops its complete cause chain, without admission.
pub unsafe fn abandon(context: *mut Context, task: *mut Task) -> u8 {
    // SAFETY: the parent retains task while blocked; cancellation shielding is lexical.
    unsafe {
        (*context).cancel_shield += 1;
        hirsel_core_cancel(context, task);
        loop {
            if (*task).state == State::Ended {
                break;
            }
            checkpoint(true);
        }
        event(JOIN, (*task).id, 2);
        let failed = if (*task).failed == 0 {
            ((*task).layout.result.drop)(context, (*task).result.as_ptr())
        } else {
            0
        };
        let causes: Causes = std::mem::take(&mut (*(*task).context).causes);
        drop(causes);
        retire(task);
        (*context).cancel_shield -= 1;
        failed
    }
}
/// # Safety
/// Same ownership contract as abandon.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_task_drop(context: *mut Context, task: *mut Task) -> u8 {
    // SAFETY: emitter transfers one unjoined handle for structured cleanup.
    unsafe { abandon(context, task) }
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
        let host = (*run)
            .host
            .get_or_insert_with(|| Stack::with_bytes(HOST_STACK_BYTES));
        let target = host.prepare(invoke, (&mut call as *mut Call).cast(), ptr::null_mut());
        crate::fiber::guard(host.bounds());
        hirsel_stack::switch(&mut call.saved, target);
        crate::fiber::guard((*task).stack.bounds());
        call.result.assume_init()
    }
}

#[cfg(any(test, feature = "charge-observation"))]
impl Drop for Task {
    fn drop(&mut self) {
        crate::peak::deallocated((self as *const Self).cast());
    }
}
