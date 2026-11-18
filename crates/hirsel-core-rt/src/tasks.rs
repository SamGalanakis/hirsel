//! Single-worker cooperative tasks. Queue links, wait state and result storage
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
    pub(crate) replay: bool,
    latest_pair: bool,
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

#[derive(Default)]
struct Parked {
    live: std::collections::HashMap<
        ReadyToken,
        *mut Task,
        std::hash::BuildHasherDefault<std::hash::DefaultHasher>,
    >,
}
impl Parked {
    fn insert(&mut self, token: u64, task: *mut Task) {
        self.live.insert(ReadyToken(token), task);
    }
    fn take(&mut self, token: u64) -> Option<*mut Task> {
        self.live.remove(&ReadyToken(token))
    }
    #[cfg(test)]
    fn peek(&self, token: u64) -> Option<*mut Task> {
        self.live.get(&ReadyToken(token)).copied()
    }
}

struct Scheduler {
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
    /// Cancellation requests for tasks parked in a provider operation, in
    /// request order, taken at the next scheduler turn.
    cancels: Vec<*mut Task>,
    outside_cancels: Vec<*mut Task>,
    /// Tasks in State::Blocked. Only `checkpoint(true)` blocks a task and only
    /// `wake` unblocks, readying every one, so a wake with none is a no-op.
    blocked: usize,
    taped: bool,
    net: *mut hirsel_core_net::Sockets,
    model: Option<hirsel_core_net::model::Model>,
    parked: Parked,
}
const _: () = assert!(std::mem::offset_of!(Task, replay) == crate::abi_layout::TASK_ARC_REPLAY);

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
#[cfg(test)]
pub(crate) unsafe fn task_stack(task: *const Task) -> Option<usize> {
    // SAFETY: the test owns the live task handle and the task is off-CPU.
    unsafe { (*task).stack.as_ref().map(|stack| stack.bounds().0) }
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
        // SAFETY: only the idle scheduler manipulates the ready queues.
        unsafe {
            if self.taped {
                let mut task = self.all;
                while !task.is_null() {
                    if let Some(group) = (*task).group
                        && !group.failed()
                    {
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
                            if matches!(group.turn(), Turn::Wait(_) | Turn::Grant | Turn::Finish) {
                                group.fail();
                            }
                        }
                    }
                    task = (*task).all;
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
        // SAFETY: idle owns every task and operation frame.
        if self.taped {
            // SAFETY: idle owns tasks; retirement removes queued requests.
            unsafe {
                for task in std::mem::take(&mut self.outside_cancels) {
                    if let Some(group) = (*task).group
                        && group.cancel(None)
                    {
                        (*(*task).context)
                            .cancel_requested
                            .store(true, Ordering::Release);
                        if (*task).state == State::Provider {
                            self.cancels.push(task);
                        }
                        self.wake();
                    }
                }
                self.tape_inputs();
            }
        }
        self.poll_net(0);
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
                let tokens = std::mem::take(&mut *self.readies.lock());
                // Deactivate the whole turn before callbacks: a resume can repark
                // the same operation, but a duplicate from this turn cannot wake it again.
                let mut ready = Vec::new();
                for token in tokens {
                    if let Some(task) = self.parked.take(token) {
                        ready.push(task);
                    }
                }
                for task in ready {
                    self.wake_operation(task, Wake::Ready);
                }
            }
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

    unsafe fn tape_inputs(&mut self) {
        // SAFETY: called only from idle; tasks and their pinned operations live.
        unsafe {
            let mut groups = Vec::new();
            let mut task = self.all;
            while !task.is_null() {
                if let Some(group) = (*task).group
                    && !groups
                        .iter()
                        .any(|g: &crate::tape::schedule::Group| g.same(group))
                {
                    groups.push(group);
                }
                task = (*task).all;
            }
            for group in groups {
                use crate::tape::schedule::Turn;
                loop {
                    let turn = group.turn();
                    let id = match turn {
                        Turn::Wake(id) | Turn::Cancel(id) | Turn::Done(id) => id,
                        Turn::Choose(index) => {
                            let live = self.live_group_tasks(group);
                            if let Some(&(id, at)) =
                                usize::try_from(index).ok().and_then(|i| live.get(i))
                                && (*at).state == State::Ready
                            {
                                group.choose(id);
                            } else {
                                group.fail();
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
                            (*(*target).context)
                                .cancel_requested
                                .store(true, Ordering::Release);
                            if (*target).state == State::Provider {
                                self.wake_operation(target, Wake::Cancel);
                            }
                            self.wake();
                        }
                        Turn::Wake(_)
                            if (*target).state == State::Provider
                                && (*(*target).operation).phase == Phase::Waiting =>
                        {
                            group.consume_input();
                            group.cancel_wait((*(*target).operation).request);
                            self.runnable(target, Next::Issue);
                        }
                        Turn::Done(_)
                            if (*target).state == State::Provider
                                && (*(*target).operation).phase == Phase::Issued =>
                        {
                            self.wake_operation(target, Wake::Ready);
                        }
                        _ => {
                            group.fail();
                            break;
                        }
                    }
                }
                if group.failed() {
                    let mut target = self.all;
                    while !target.is_null() {
                        if (*target).group.is_some_and(|g| g.same(group))
                            && (*target).state == State::Provider
                        {
                            self.runnable(target, Next::Deliver(crate::tape::REFUSED));
                        }
                        target = (*target).all;
                    }
                }
            }
        }
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
            // SAFETY: the run pins its shared registry; no task runs during polling.
            unsafe { (*self.net).poll(timeout, |token| self.readies.push(token)) }
                .unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
        }
    }

    fn idle(&mut self) {
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
            {
                crate::process::stop(b"Deadlock");
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
                // A closed replay cannot await host readiness. An input that
                // cannot make progress at this coordinate is a schedule fault.
                if self.taped {
                    let mut task = self.all;
                    // SAFETY: idle owns the task list; only taped runs need replay-input diagnosis.
                    unsafe {
                        while !task.is_null() {
                            if let Some(group) = (*task).group
                                && group.replaying()
                                && (*task).state == State::Provider
                            {
                                group.fail();
                                self.tape_inputs();
                                return;
                            }
                            task = (*task).all;
                        }
                    }
                }
                let provider = !self.parked.live.is_empty();
                if self.head.is_null() && self.deferred.is_null() && !provider {
                    crate::process::stop(b"Deadlock");
                }
                if !self.net.is_null() {
                    self.poll_net(-1);
                } else {
                    std::thread::park();
                }
            }
        }
    }

    unsafe fn runnable(&mut self, task: *mut Task, next: Next) {
        // SAFETY: the task is parked; its operation frame is pinned.
        unsafe {
            self.parked.take((*(*task).operation).token);
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
            #[cfg(test)]
            if wake == Wake::Ready {
                ROUTE_WAKES.set(ROUTE_WAKES.get() + 1);
            }
            // A request that may not take effect now changes nothing: no wait,
            // no abort, no issue skipped. It stays pending for the task's next
            // cancellation point (CONC-7, FAIL-11, CONC-8).
            if wake == Wake::Cancel && !cancels_operation(task) {
                return false;
            }
            let _callback = CallbackTask::new(task);
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
                        if let Some(group) = (*task).group {
                            group.cancel_wait(request);
                        }
                        self.runnable(task, Next::Cancelled);
                        return false;
                    }
                    match w.kind {
                        WAIT_READY => {
                            if (*op).phase == Phase::Entry {
                                self.parked.take(token);
                                (*op).phase = Phase::Runnable;
                                (*op).next = Next::Issue;
                                return true;
                            }
                            if let Some(group) = (*task).group {
                                group.wake(request);
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
                    self.parked.insert(token, task);
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
                    self.parked.insert((*(*task).operation).token, task);
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
    net: Mutex<Option<Arc<hirsel_core_net::Poller>>>,
    model: Mutex<Option<hirsel_core_net::model::Model>>,
}
impl Readies {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(Vec::new()),
            pending: AtomicBool::new(false),
            worker: std::thread::current(),
            net: Mutex::new(None),
            model: Mutex::new(None),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<u64>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn push(&self, token: u64) {
        self.lock().push(token);
        self.pending.store(true, Ordering::Release);
        self.worker.unpark();
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
        (*(*task).run)
            .parked
            .insert((*(*task).operation).token, task);
        (*task).state = State::Provider;
        hirsel_stack::switch(&raw mut (*task).sp, (*(*task).run).idle);
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
pub(crate) fn before_commit() {
    replay_boundary(false);
}

/// Only a provider entry can reach a taped WAIT without first committing.
pub(crate) fn before_provider_entry() {
    replay_boundary(true);
}

pub(crate) fn before_tape_frame(group: crate::tape::schedule::Group, ending: bool) {
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
                Turn::Finish => return,
                Turn::Grant => {
                    group.fail();
                    return;
                }
                _ => checkpoint(false),
            }
        }
    }
}

fn replay_boundary(provider_entry: bool) {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins this task. No scheduler reference is held across a switch.
    unsafe {
        if task.is_null() || !(*task).replay {
            return;
        }
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
                Turn::Fifo => return,
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
                        .is_none_or(|wanted| wanted.state != State::Ready) =>
                {
                    group.fail();
                    return;
                }
                Turn::Event(None) => {
                    group.fail();
                    return;
                }
                _ => checkpoint(false),
            }
        }
    }
}

unsafe fn live_index(task: *mut Task, group: crate::tape::schedule::Group) -> u64 {
    let mut index = 0;
    // SAFETY: the active committer owns its scheduler's creation list.
    unsafe {
        let mut at = (*(*task).run).all;
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
        let task = NonNull::new_unchecked(task_pointer).as_ref();
        let run = NonNull::new_unchecked(task.run).as_ref();
        // Without a sink the gate is the refund flag, so a quiet event reads
        // one byte; with one the gate stays set and the flag is separate.
        if task.arc_gate.load(Ordering::Acquire)
            && ((run.provider.event.is_none() && task.group.is_none())
                || task.recorded_refunds.load(Ordering::Acquire))
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
        (*task_pointer).latest_pair = opens_pair;
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
    // SAFETY: this synchronous poll only queues tokens, never invokes task callbacks.
    unsafe {
        if !(*(*task).run).net.is_null() {
            (*(*task).run).poll_net(0);
            if (&(*(*task).run).readies).pending.load(Ordering::Acquire) {
                checkpoint(false);
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
    before_commit();
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
pub(crate) fn recorded_chooser() -> u8 {
    #[cfg(any(test, feature = "testing"))]
    {
        let task = CURRENT.get();
        // SAFETY: CURRENT pins the scheduler while the tape opens.
        if unsafe { !task.is_null() && (*(*task).run).script.is_some() } {
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
pub(crate) fn attach_model(model: &hirsel_core_net::model::Model) {
    let task = if CURRENT.get().is_null() {
        CALLBACK_TASK.get()
    } else {
        CURRENT.get()
    };
    if task.is_null() {
        ALONE.with(|r| *r.0.model.lock().unwrap_or_else(|e| e.into_inner()) = Some(model.clone()));
    } else {
        // SAFETY: the provider callback's task pins its run on this worker.
        unsafe {
            (*(*task).run).model = Some(model.clone());
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
            (*(*task).run).net = sockets;
            (*(*task).run).readies.clone()
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

pub(crate) fn attach_tape(group: crate::tape::schedule::Group) {
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: CURRENT pins this task while tape.open executes.
        unsafe {
            assign_tape(task, group);
        }
    }
}
unsafe fn assign_tape(task: *mut Task, group: crate::tape::schedule::Group) {
    // SAFETY: caller exclusively owns this task before publication or execution.
    unsafe {
        (*(*task).run).taped = true;
        (*task).refunds.take();
        (*task).group = Some(group);
        (*task).replay = group.replaying();
        (*task).refunds = Some(
            (*(*task).context)
                .current
                .subscribe_refunds(NonNull::from(&(*task).recorded_refunds)),
        );
        (*task).arc_gate.store(true, Ordering::Release);
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
        before_commit();
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
    // SAFETY: observe entry in its commit turn.
    unsafe {
        before_commit();
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
        before_commit();
        event(END, (*task).id, (*task).failed as u64);
        (*task).state = State::Ended;
        (*(*task).run).wake();
        hirsel_stack::switch(&raw mut (*task).sp, (*(*task).run).idle);
        std::hint::unreachable_unchecked();
    }
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
        cancels: Vec::new(),
        outside_cancels: Vec::new(),
        blocked: 0,
        taped: false,
        net: ptr::null_mut(),
        model: None,
        parked: Parked::default(),
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
        clean: None,
    };
    let mut root = Box::new(Task {
        arc_gate: AtomicBool::new(run.provider.event.is_some()),
        replay: false,
        latest_pair: false,
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
        root.refunds = Some((*context).current.subscribe_refunds(NonNull::from(
            root.refund_flag(run.provider.event.is_some()),
        )));
        (*context).events = root_pointer.expose_provenance();
        root.sp = root
            .stack
            .as_ref()
            .unwrap()
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
            crate::fiber::guard((*task).stack.as_ref().unwrap().bounds());
            hirsel_stack::switch(&mut run.idle, (*task).sp);
            crate::fiber::guard((0, 0));
            CURRENT.set(previous);
            match (*task).state {
                // Only the scheduler can move an ended stack to stock. The body
                // exits after its last provider call has delivered or aborted.
                State::Ended => run.stacks.recycle((*task).stack.take().unwrap()),
                State::Provider if run.wake_operation(task, Wake::Parked) => direct = task,
                _ => {}
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
struct Spawned {
    task: *mut Task,
    available: u64,
}
/// # Safety
/// Layout is emitter-produced. Input is initialized X and moves only on success;
/// body consumes it and initializes R only on success. Handle stays with parent.
unsafe fn spawn_in(
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
        before_commit();
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
            arc_gate: AtomicBool::new((*run).provider.event.is_some()),
            replay: false,
            latest_pair: false,
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
            group: group.map(|group| group.child()),
            stack: Some((*run).stacks.take()),
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
        if let Some(group) = (*task).group {
            (*task).replay = group.replaying();
            (*task).arc_gate.store(true, Ordering::Release);
        }
        (*task).context = (*task).owned_context.as_mut().unwrap();
        (*task).refunds = Some((*(*task).context).current.subscribe_refunds(NonNull::from(
            (*task).refund_flag((*run).provider.event.is_some() || (*task).group.is_some()),
        )));
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
        let spawned = spawn_in(context, bytes, input, body, layout, group);
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
        before_commit();
        if let Some(group) = (*task).group
            && !current_group().is_some_and(|source| source.same(group))
        {
            if (*task).state != State::Ended {
                (*(*task).run).outside_cancels.push(task);
            }
            event(CANCEL, (*task).id, 0);
            return;
        }
        (*(*task).context)
            .cancel_requested
            .store(true, Ordering::Release);
        if (*task).state == State::Provider {
            (*(*task).run).cancels.push(task);
        }
        event(CANCEL, (*task).id, 0);
        wake();
    }
}
unsafe fn retire(task: *mut Task) {
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
            before_commit();
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
/// Cancel the task and wait for it to end, under shielding; its result (if it
/// succeeded) is then this cleanup's alone.
unsafe fn abandon_begin(context: *mut Context, task: *mut Task) -> *mut crate::counted::Holder {
    // SAFETY: the parent retains task while blocked; cancellation shielding is lexical.
    unsafe {
        (*context).cancel_shield += 1;
        hirsel_core_cancel(context, task);
        loop {
            before_commit();
            if (*task).state == State::Ended {
                break;
            }
            checkpoint(true);
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
        let host = (*run).stacks.take();
        let target = host.prepare(invoke, (&mut call as *mut Call).cast(), ptr::null_mut());
        crate::fiber::guard(host.bounds());
        hirsel_stack::switch(&mut call.saved, target);
        crate::fiber::guard((*task).stack.as_ref().unwrap().bounds());
        (*run).stacks.recycle(host);
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
                operation: RECV,
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
                cancels: Vec::new(),
                outside_cancels: Vec::new(),
                blocked: 0,
                taped: false,
                net: ptr::null_mut(),
                model: None,
                parked: Parked::default(),
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
                    phase: Phase::Issued,
                    deadline: 0,
                    next: Next::Issue,
                    entered: None,
                })
                .collect();
            let mut tasks: Vec<_> = (0..n)
                .map(|i| {
                    Box::new(Task {
                        arc_gate: AtomicBool::new(false),
                        replay: false,
                        latest_pair: false,
                        recorded_refunds: AtomicBool::new(false),
                        refunds: None,
                        polls_left: POLLS_PER_TURN,
                        context: &mut context,
                        owned_context: None,
                        run: &mut run,
                        ready: ptr::null_mut(),
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
            run.net = &mut sockets;
            assert!(run.parked.peek(1).is_some());
            let selected = if n == 3 { 1 } else { n - 1 };
            peers[selected].write_all(b"x").unwrap();
            ROUTE_WORK.set(0);
            ROUTE_WAKES.set(0);
            // SAFETY: task/operation frames and callback storage are pinned; no fiber runs.
            unsafe {
                run.take_wakes();
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
            if let Next::Deliver(outcome) = operations[selected].next {
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
                    run.take_wakes();
                }
                assert_eq!(ROUTE_WAKES.get(), 3, "dedup_dropped");
                assert_eq!(counters[0], 3);
                assert!(run.parked.live.is_empty());
                for i in [0, 2] {
                    if let Next::Deliver(outcome) = operations[i].next {
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
                operations[0].phase = Phase::Issued;
                tasks[0].state = State::Provider;
                run.parked.insert(4, &mut *tasks[0]);
                sockets.arm(replacement, false, 4).unwrap();
                sockets
                    .dispatch(stale, libc::EPOLLIN as u32, |token| run.readies.push(token))
                    .unwrap();
                unsafe {
                    run.take_wakes();
                }
                assert_eq!(ROUTE_WAKES.get(), 3, "route_ignores_generation");
                assert_eq!(run.parked.live.len(), 1);
                peer.write_all(b"new").unwrap();
                unsafe {
                    run.take_wakes();
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
                    run.take_wakes();
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
