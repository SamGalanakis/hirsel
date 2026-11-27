//! Native tasks and their pinned-fiber dispatchers. Queue links, wait state and result storage
//! are prepaid at spawn. Only blocking or explicit checkpoints switch stacks.
#[cfg(feature = "prof")]
use crate::prof::{self, Kind as ProfKind, Reason, WakeSource};
use crate::{
    Capacity, Context, abi_tasks as abi,
    block::Block,
    causes::{self, Cause, Causes, Cell},
    fiber::{Stack, Stock},
};
use hirsel_core_layout::charge;
#[cfg(test)]
use std::cell::Cell as Local;
use std::{
    alloc::Layout,
    ptr::{self, NonNull},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

mod frame;
mod quiet;
mod stop;
mod storage;
mod timers;
pub use stop::run_root;
pub(crate) use stop::{
    host_operation, on_host_stack, run_root_contained, run_root_with_unit, wake_all_waiters,
};
use timers::{Timer, TimerHeap};
mod transfer;
use frame::FrameRecord;
pub(crate) use frame::held_wait;
#[cfg(test)]
pub(crate) use frame::{FRAME_HEADER, hirsel_core_frame_record};
pub(crate) use quiet::{Member, RunView, run_tag};
use storage::MAX_SPARE_TASK_BYTES;
pub(crate) use storage::Storage;
pub use transfer::hirsel_core_transfer;
use transfer::{Moved, ZeroBlock};

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

pub use hirsel_core_layout::event::{
    ARC_NEW, ARC_RELEASE, ARC_SHARE, ARC_UNWRAP, CANCEL, CANCELLED, CAPACITY, CHECK_CANCEL, CLOSE,
    END, JOIN, LATEST_PUBLISH, LATEST_READ, PUBLISH, RECEIVE, REFUND, SENDERS, SPAWN, TRY_RECEIVE,
    TRY_SEND,
};
/// Only no-effect polls count; effectful operations never consume this allowance.
pub(crate) const POLLS_PER_TURN: u32 = 64;
/// A live run asks the kernel for readiness at one event in this many; the
/// dispatcher still polls at every turn before timers, and idle still waits
/// in the poll. Tokio's current-thread `event_interval` default.
const LIVE_PROBE_INTERVAL: u32 = 61;
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
crate::units::moved_tls! { pub(crate) static CALLBACK_TASK: *mut Task = ptr::null_mut(); }
crate::units::moved_tls! { pub(crate) static CURRENT: *mut Task = ptr::null_mut(); }

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
pub(super) enum State {
    Ready,
    Running,
    Waiting,
    ChannelDeadline,
    CellChannelDeadline,
    /// Parked in a provider operation's W or I. Only that operation's own wakes
    /// (its timer, its token, a cancellation request) move it; `wake` does not.
    Provider,
    Ended,
}
impl State {
    fn channel_deadline(self) -> bool {
        matches!(self, Self::ChannelDeadline | Self::CellChannelDeadline)
    }

    fn cell_channel_deadline(self) -> bool {
        self == Self::CellChannelDeadline
    }

    fn local_wait(self) -> bool {
        self == Self::Waiting || self.channel_deadline()
    }
}

pub(crate) mod channel_clock;

#[cfg(feature = "record")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelTape {
    None,
    Parked,
    Expired,
}

/// The first byte sends Arc count operations through the runtime commit path
/// on taped replay. Another task counts inline. Its emitted code skips the init/share/release hooks
/// while the second byte is clear and its payer's domain has no posted credit.
#[repr(C)]
pub struct Task {
    pub(crate) arc_runtime: bool,
    /// Frozen at TASK_ARC_HOOKS: the inline init/share/release hooks must run
    /// whatever the payer's credit (`Scheduler::arc_hooks`). Every writer of
    /// an input recomputes it; the constructors start it set.
    pub(crate) arc_hooks: bool,
    #[cfg(feature = "record")]
    replay: bool,
    #[cfg(feature = "record")]
    latest_pair: bool,
    /// Replay paused this task's turn at a tape boundary; it resumes with no `TURN`.
    #[cfg(feature = "record")]
    owns_turn: bool,
    polls_left: u32,
    context: *mut Context,
    owned_context: Option<Context>,
    run: *mut Scheduler,
    ready: *mut Task,
    waiters: Waiters,
    waiting_on: *mut Waiters,
    wait_previous: *mut Task,
    wait_next: *mut Task,
    timer: std::cell::UnsafeCell<Timer>,
    operation: *mut Operation,
    /// The scheduler's creation list, newest first, linked both ways so a
    /// retire unlinks in O(1) however many tasks are live.
    all: *mut Task,
    all_previous: *mut Task,
    /// The scheduler's warm list: tasks a serial run dispatched since network
    /// idle last released the stack pages below their parked frames.
    warm_next: *mut Task,
    warm_previous: *mut Task,
    warm: bool,
    #[cfg(feature = "record")]
    channel_tape: ChannelTape,
    state: State,
    id: u64,
    #[cfg(feature = "record")]
    group: Option<crate::tape::schedule::Group>,
    stack: Option<Stack>,
    sp: *mut u8,
    body: Body,
    input: *mut u8,
    result: *mut u8,
    storage_layout: Layout,
    layout: TaskLayout,
    failed: u8,
    record: Option<Block>,
    /// The returnable form's frame record: the owned operation state and the
    /// represented frames, at one address from the first call until the end.
    frame: Option<FrameRecord>,
    /// Set by a returnable wait that parked: the body returned through its
    /// represented frames, and the stack holds nothing live.
    stackless: bool,
    /// What the body reads in place of `input` once a transfer has moved
    /// the task's job (`hirsel_core_transfer`).
    moved: Moved,
    /// A transfer made by the running body: its new input, body and resume
    /// glue, taken by `start` once the body has returned through its frames.
    transfer: Option<(ZeroBlock, Body, Option<Body>)>,
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

#[path = "tasks_ready.rs"]
pub(crate) mod ready;
#[cfg(test)]
pub(crate) use ready::check_ready_scratch;
use ready::{ready_scratch_bytes, release_ready_scratch};

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
            .unwrap_or_else(|| stop::stop_static(b"HostHandles"));
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
            .unwrap_or_else(|| stop::stop_static(b"HostHandles"));
        captured.extend(tokens.into_iter().filter_map(|token| {
            if token & channel_clock::TOKEN_TAG != 0 {
                return Some(ReadyArm { token, identity: 0 });
            }
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
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.first.is_null() && self.last.is_null()
    }

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
    pub(crate) unsafe fn wake_all(
        queue: *mut Self,
        #[cfg(feature = "prof")] source: (WakeSource, *mut Task, u64),
    ) -> usize {
        let mut count = 0;
        // SAFETY: each removal leaves the successor at the head.
        unsafe {
            while !(*queue).first.is_null() {
                let task = (*queue).first;
                #[cfg(feature = "prof")]
                prof_wake(task, source.0, source.1, source.2);
                Self::remove(task);
                (*(*task).run).ready(task);
                count += 1;
            }
        }
        count
    }
}

#[cfg(feature = "record")]
use crate::tape::schedule::Group;

#[cfg(feature = "record")]
struct LiveTape {
    group: Group,
    opened: u64,
    members: usize,
}

#[cfg(feature = "record")]
#[derive(Default)]
struct Tapes {
    live: Vec<LiveTape>,
    next_open: u64,
    // Opening a tape links its scheduler input path, including task ordering.
    take_inputs: Option<unsafe fn(&mut Scheduler, bool)>,
}

#[cfg(feature = "record")]
impl Tapes {
    fn open(&mut self, group: Group) {
        let opened = self.next_open;
        self.next_open = opened
            .checked_add(1)
            .unwrap_or_else(|| stop::stop_static(b"TapeSlots"));
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

    fn seal_deadlock(&mut self) -> Option<(String, crate::tape::Fault)> {
        self.live
            .drain(..)
            .filter_map(|entry| {
                entry
                    .group
                    .seal_deadlock()
                    .err()
                    .map(|deadlock| (entry.opened, deadlock))
            })
            .min_by_key(|(opened, _)| *opened)
            .map(|(_, deadlock)| deadlock)
    }
}

/// # Safety
/// The idle serial dispatcher has exclusive custody of the root and its heap.
unsafe fn trim_run_heap(root: *mut Task) {
    // SAFETY: the run pins the root context, and this dispatcher has custody.
    unsafe {
        if let Some(heap) = (*(*root).context).heap.get()
            && heap.pending()
        {
            heap.trim();
        }
    }
}

unsafe fn switch_to_idle(task: *mut Task) {
    // SAFETY: the running task is pinned here; only its run's dispatcher owns the idle stack.
    unsafe {
        // MEM-30: a fiber off the CPU holds no step credit; it draws again at
        // its next charged step.
        crate::steps::suspend(&mut *(*task).context);
        hirsel_stack::switch(&raw mut (*task).sp, (*(*task).run).idle)
    }
}

/// Return what ended tasks left resident: retained task-heap pages and this
/// run's surplus stacks.
fn release_idle(run: *mut Scheduler) {
    crate::task_heap::purge();
    // SAFETY: the run pins its stock and only this OS thread uses it.
    unsafe { (*run).stacks.release() };
}

struct Scheduler {
    #[cfg(feature = "prof")]
    prof_id: u64,
    share_heap: bool,
    deferred: *mut Task,
    deferred_tail: *mut Task,
    stacks: Stock,
    spare_task: Option<Storage>,
    idle: *mut u8,
    head: *mut Task,
    tail: *mut Task,
    timers: TimerHeap,
    all: *mut Task,
    next: u64,
    /// Tasks dispatched since the last network idle, whose parked stacks may
    /// hold pages below their live frames.
    warm: *mut Task,
    provider: SchedulerProvider,
    #[cfg(any(test, feature = "testing"))]
    script: Option<Arc<crate::schedule_script::ScheduleScript>>,
    #[cfg(all(feature = "record", any(test, feature = "testing")))]
    script_groups: Vec<crate::tape::schedule::Group>,
    readies: Arc<Readies>,
    ready_tokens: Vec<u64>,
    ready_arms: Vec<ReadyArm>,
    /// Cancellation requests for tasks parked in a provider operation, in
    /// request order, taken at the next scheduler turn.
    cancels: Vec<*mut Task>,
    #[cfg(feature = "record")]
    outside_cancels: Vec<*mut Task>,
    #[cfg(feature = "record")]
    tapes: Tapes,
    net: *mut hirsel_core_net::Sockets,
    /// Events left before a live run's next in-task kernel probe.
    probes_left: u32,
    parked: WaitArms<*mut Task>,
    /// The unit beside this run (units_lang): null until the first unit
    /// operation; a helper's comes with the run and its thread owns it.
    lang: *mut crate::units_lang::LangUnit,
    lang_owned: bool,
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
    // SAFETY: the fixture owns this live handle throughout the check.
    unsafe { (*task).state == State::Provider }
}

#[cfg(feature = "testing")]
#[repr(C)]
pub struct ReadyWaitBacking {
    readers: usize,
    stacks: usize,
    frames: usize,
}

/// Inspect the mounted stacks and frame records of readers parked in
/// recv_ready. The native fixture calls this from its serial scheduler.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub extern "C" fn hirsel_core_test_ready_wait_backing() -> ReadyWaitBacking {
    let current = CURRENT.get();
    // SAFETY: CURRENT pins this scheduler. The serial dispatcher cannot
    // change any task or operation while the caller is running.
    unsafe {
        assert!(!current.is_null());
        let run = &*(*current).run;
        let mut backing = ReadyWaitBacking {
            readers: 0,
            stacks: 0,
            frames: 0,
        };
        let mut task = run.all;
        while let Some(reader) = task.as_ref() {
            if reader.state == State::Provider
                && (*(*reader.operation).request).operation == crate::provider::RECV_READY
            {
                backing.readers += 1;
                backing.stacks += usize::from(reader.stack.is_some());
                backing.frames += usize::from(reader.frame.is_some());
            }
            task = reader.all;
        }
        backing
    }
}

// Time the actual callback, preserving which provider the operation calls.
macro_rules! provider_callback {
    ($token:expr, $phase:ident, $call:expr) => {{
        #[cfg(feature = "prof")]
        let _callback = prof::Callback::begin($token, prof::Phase::$phase);
        $call
    }};
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
    /// closes on them, or something else observes them. Readiness is not
    /// taken at an Arc count: a count is not a poll kind.
    fn arc_hooks(
        &self,
        #[cfg_attr(not(feature = "record"), allow(unused_variables))] task: &Task,
    ) -> bool {
        #[cfg(feature = "record")]
        if task.group.is_some() || task.latest_pair {
            return true;
        }
        self.arc_observed()
    }

    fn id(&mut self) -> u64 {
        let next = self.next;
        self.next = self
            .next
            .checked_add(1)
            .unwrap_or_else(|| stop::stop_static(b"TaskIds"));
        next
    }
    unsafe fn ready(&mut self, task: *mut Task) {
        // SAFETY: the scheduler owns this unqueued task.
        unsafe {
            self.ready_with_reason(
                task,
                #[cfg(feature = "prof")]
                Reason::None,
            )
        };
    }

    #[inline(always)]
    unsafe fn ready_with_reason(
        &mut self,
        task: *mut Task,
        #[cfg(feature = "prof")] reason: Reason,
    ) {
        // SAFETY: scheduler owns task; only unqueued tasks can enter the run queue.
        unsafe {
            #[cfg(feature = "prof")]
            prof_state(task, State::Ready, reason, 0);
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
            #[cfg(feature = "record")]
            for entry in &self.tapes.live {
                let group = entry.group;
                use crate::tape::schedule::Turn;
                let turn = group.turn();
                let wanted = match turn {
                    Turn::Wait(id) | Turn::Event(Some(id)) | Turn::Dispatch(id) => Some(id),
                    Turn::Retired(id) if !group.fifo() => Some(id),
                    Turn::Grant | Turn::Finish => Some(0),
                    _ => None,
                };
                if let Some(id) = wanted {
                    let target = self.group_task(group, id);
                    if !target.is_null()
                        && (*target).state == State::Ready
                        && self.takes_turn(group, target, &turn)
                    {
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
            #[cfg(feature = "testing")]
            if crate::units::observe::sample_depth() {
                let mut depth = 0;
                for mut next in [self.head, self.deferred] {
                    while !next.is_null() {
                        depth += 1;
                        next = (*next).ready;
                    }
                }
                crate::units::observe::depth(depth);
            }
            #[cfg(any(test, feature = "testing"))]
            if let Some(script) = self.script.clone() {
                let selected = self.scripted_task(&script);
                #[cfg(feature = "record")]
                let selected = self.ungated(selected);
                return self.unlink_ready(selected);
            }
            let selected = self.head;
            #[cfg(feature = "record")]
            let selected = self.ungated(selected);
            self.unlink_ready(selected)
        }
    }

    #[cfg(any(test, feature = "testing"))]
    unsafe fn scripted_task(
        &mut self,
        script: &crate::schedule_script::ScheduleScript,
    ) -> *mut Task {
        // SAFETY: pop holds the dispatcher and head is ready.
        unsafe {
            #[cfg(feature = "record")]
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
                #[cfg(feature = "record")]
                let untaped = (*task).group.is_none();
                #[cfg(not(feature = "record"))]
                let untaped = true;
                if untaped {
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
            if (*task).state.local_wait()
                && cancel_takes_effect(&*(*task).context, cancel_flag(task))
            {
                #[cfg(feature = "prof")]
                prof_wake(task, WakeSource::Cancel, ptr::null_mut(), 0);
                Waiters::remove(task);
                self.ready(task);
            }
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
        #[cfg(feature = "record")]
        let take_inputs = self.tapes.take_inputs;
        #[cfg(feature = "record")]
        if let Some(take_inputs) = take_inputs {
            // SAFETY: idle owns every task and operation frame.
            unsafe { take_inputs(self, false) };
        }
        self.poll_net(net_timeout);
        if !self.lang.is_null() {
            self.readies.port_pending.store(false, Ordering::Relaxed);
            // SAFETY: the run pins its unit; no task runs.
            unsafe {
                (*self.lang).pump_ready();
                let sockets = (*self.lang).socket_registry;
                if self.net != sockets
                    && let Some(poller) = sockets.as_ref().and_then(|s| s.active_poller())
                {
                    self.set_net(sockets, poller);
                }
            }
        }
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
        // A recorded WAKE or DONE was written by the readiness below, after
        // this turn's cancels, so replay takes it here: a task cancelled in
        // the same turn is still queued ahead of the one the input readies.
        #[cfg(feature = "record")]
        if let Some(take_inputs) = take_inputs {
            // SAFETY: idle owns every task and operation frame.
            unsafe { take_inputs(self, true) };
        }
        if self.readies.pending.swap(false, Ordering::Acquire) {
            std::mem::swap(&mut self.ready_tokens, &mut *self.readies.lock());
            self.parked.capture(self.ready_tokens.drain(..), captured);
            release_ready_scratch(&mut self.ready_tokens, ready_scratch_bytes() / 2);
        }
    }

    unsafe fn apply_ready(&mut self, ready: ReadyArm) {
        if ready.token & channel_clock::TOKEN_TAG != 0 {
            // SAFETY: this turn owns the task list. A token only wakes its
            // original arm, which is live exactly in a cell deadline wait.
            unsafe {
                let mut task = self.all;
                while !task.is_null() {
                    if (*task).state.cell_channel_deadline()
                        && channel_clock::token(task) == ready.token
                    {
                        self.wake_channel_deadline(task);
                        break;
                    }
                    task = (*task).all;
                }
            }
        } else if let Some(task) = self.parked.take_captured(ready) {
            // SAFETY: the captured identity still names this parked frame.
            unsafe { self.wake_operation(task, Wake::Ready) };
        }
    }

    unsafe fn wake_channel_deadline(&mut self, task: *mut Task) {
        // SAFETY: the scheduler owns the pinned task and channel queue.
        unsafe {
            #[cfg(feature = "record")]
            if (*task).channel_tape == ChannelTape::Parked {
                (*task).channel_tape = ChannelTape::Expired;
            }
            #[cfg(feature = "prof")]
            prof_wake(task, WakeSource::Timer, ptr::null_mut(), 0);
            Waiters::remove(task);
            self.ready(task);
        }
    }

    unsafe fn take_timers_at(&mut self, now: u64) {
        // SAFETY: this turn owns the timer heap and pinned operation frames.
        unsafe {
            // Detach every expired timer first: a wait that answers Until(now)
            // again re-arms for the next turn, so the other tasks still run.
            // Detached timers lend their unused parent link to this wake batch.
            let mut expired = ptr::null_mut::<Task>();
            let mut tail = &raw mut expired;
            while self
                .timers
                .deadline()
                .is_some_and(|deadline| deadline <= now)
            {
                let task = self.timers.root;
                self.timers.remove(task);
                *tail = task;
                tail = &raw mut (*(*task).timer.get()).parent;
            }
            while !expired.is_null() {
                let task = expired;
                expired = (*(*task).timer.get()).parent;
                (*(*task).timer.get()).parent = ptr::null_mut();
                if (*task).state.channel_deadline() {
                    self.wake_channel_deadline(task);
                } else if (*task).state == State::Provider {
                    self.wake_operation(task, Wake::Timer);
                }
            }
        }
    }

    unsafe fn take_timers(&mut self) {
        if !self.timers.root.is_null() {
            #[cfg(test)]
            let sample = TIMER_SAMPLE.get();
            #[cfg(not(test))]
            let sample: Option<u64> = None;
            // SAFETY: the scheduler owns the timer heap during this turn.
            unsafe { self.take_timers_at(sample.unwrap_or_else(crate::os::monotonic_ns)) };
        }
    }

    unsafe fn take_wakes(&mut self, net_timeout: i32) {
        // SAFETY: the cooperative scheduler owns this whole dispatch turn.
        unsafe {
            let mut captured = std::mem::take(&mut self.ready_arms);
            self.capture_wakes(&mut captured, net_timeout);
            let readied = !captured.is_empty();
            for ready in captured.drain(..) {
                #[cfg(any(test, feature = "testing"))]
                if let Some(script) = &self.script {
                    script.pause(crate::schedule_script::PausePoint::ReadinessTaken {
                        token: ready.token,
                    });
                }
                self.apply_ready(ready);
            }
            release_ready_scratch(&mut captured, ready_scratch_bytes());
            self.ready_arms = captured;
            self.take_timers();
            #[cfg(feature = "record")]
            if !self.tapes.live.is_empty() {
                self.take_tape_deadlines(readied);
            }
            #[cfg(not(feature = "record"))]
            let _ = readied;
        }
    }

    /// A wait on something outside the run: a sync's ready still to come or a host provider's.
    fn has_outside_wait(&self) -> bool {
        #[cfg(feature = "record")]
        {
            self.readies.outside()
                // SAFETY: each entry points to its pinned provider operation.
                || self.parked.live.values().any(|arm| unsafe {
                    let task = arm.task;
                    let provider = &(*(*task).operation).provider;
                    !crate::provide::owns(provider)
                        && !(*task)
                            .group
                            .is_some_and(|group| group.drives_replay(*provider))
                })
        }
        #[cfg(not(feature = "record"))]
        {
            self.readies.outside()
                // SAFETY: each entry points to its pinned provider operation.
                || self.parked.live.values().any(|arm| unsafe {
                    !crate::provide::owns(&(*(*arm.task).operation).provider)
                })
        }
    }

    fn stop_quiescent(&mut self) {
        #[cfg(feature = "record")]
        if let Some((unit, fault)) = self.tapes.seal_deadlock() {
            let mut line = crate::tape::format_verdict(fault, 0, &unit);
            line.push('\n');
            // SAFETY: stderr is fd 2 and `line` is valid for its length.
            unsafe {
                let _ = libc::write(2, line.as_ptr().cast(), line.len());
                libc::_exit(1);
            }
        }
        stop::record_static(b"Deadlock");
    }

    fn set_net(
        &mut self,
        sockets: *mut hirsel_core_net::Sockets,
        poller: Arc<hirsel_core_net::Poller>,
    ) {
        self.net = sockets;
        *self.readies.net.lock().unwrap_or_else(|e| e.into_inner()) = Some(poller);
    }

    fn poll_net(&mut self, timeout: i32) {
        if !self.net.is_null() && crate::units::untied() {
            let readies = &self.readies;
            let net = self.net;
            // SAFETY: the run pins its registry; no task runs during polling.
            crate::units::take_xready(|id, flags| unsafe {
                (*net).dispatch(id, flags, |token| readies.enqueue(token))
            });
            // The unit pool's poller holds every unit's sockets.
            return;
        }
        if !self.net.is_null() {
            let readies = &self.readies;
            // SAFETY: the run pins its shared registry; no task runs during
            // polling, and the only dispatcher is already awake.
            if unsafe { (*self.net).poll(timeout, |token| readies.enqueue(token)) }.is_err() {
                stop::record_static(b"HostPoll");
            }
        }
    }

    fn net_idle_wait(&self, timeout: i32) -> (i32, Option<u64>) {
        crate::task_heap::idle_wait(timeout, self.stacks.has_warm_kept() || !self.warm.is_null())
    }

    fn release_net_idle(&mut self) {
        release_idle(self);
        self.stacks.cool_kept();
        self.cool_warm();
    }

    /// Record a serial dispatch: until the next network idle, the task's
    /// stack may grow below wherever it parks.
    ///
    /// # Safety
    /// The serial dispatcher has exclusive custody of `task`, which is live.
    #[inline]
    unsafe fn mark_warm(&mut self, task: *mut Task) {
        // SAFETY: per the contract; the warm list holds only live tasks.
        unsafe {
            if (*task).warm {
                return;
            }
            (*task).warm = true;
            (*task).warm_previous = ptr::null_mut();
            (*task).warm_next = self.warm;
            if !self.warm.is_null() {
                (*self.warm).warm_previous = task;
            }
            self.warm = task;
        }
    }

    /// # Safety
    /// `task` is live and owned by this scheduler's dispatcher.
    unsafe fn unmark_warm(&mut self, task: *mut Task) {
        // SAFETY: per the contract; warm neighbours are live tasks.
        unsafe {
            if !(*task).warm {
                return;
            }
            let (previous, next) = ((*task).warm_previous, (*task).warm_next);
            if previous.is_null() {
                self.warm = next;
            } else {
                (*previous).warm_next = next;
            }
            if !next.is_null() {
                (*next).warm_previous = previous;
            }
            (*task).warm = false;
        }
    }

    /// Release the pages below each parked frame of every task that ran since
    /// the last network idle, however deep it went while running.
    fn cool_warm(&mut self) {
        let mut task = std::mem::replace(&mut self.warm, ptr::null_mut());
        // SAFETY: the serial dispatcher has exclusive custody of these tasks.
        // A waiting stack's saved SP begins the switch frame, below the
        // caller's live frames and any caller red zone.
        unsafe {
            while !task.is_null() {
                if ((*task).state.local_wait() || (*task).state == State::Provider)
                    && let Some(stack) = &(*task).stack
                {
                    stack.discard_below((*task).sp.addr());
                }
                (*task).warm = false;
                task = (*task).warm_next;
            }
        }
    }

    fn idle(&mut self) {
        if crate::run_stop::is_ended() || self.readies.has_pending() || !self.cancels.is_empty() {
            return;
        }
        let deadline = self.timers.deadline();
        if deadline.is_none() && !self.has_outside_wait() {
            match self.quiet() {
                quiet::Decision::Run => return self.active(),
                quiet::Decision::Deadlock => {
                    self.stop_quiescent();
                    return;
                }
                quiet::Decision::Park => {}
            }
        }
        if crate::units::untied() {
            crate::units::park(deadline);
            self.active();
            if !self.net.is_null() {
                self.poll_net(0);
            }
            return;
        }
        // The scheduler thread waits here, never in a provider: readiness from
        // any thread unparks it (hirsel_core_ready).
        let release = match deadline {
            Some(deadline) => {
                if !self.net.is_null() {
                    let (timeout, release) = self.net_idle_wait(poll_timeout(Some(deadline)));
                    self.poll_net(timeout);
                    release.is_some_and(|due| crate::os::monotonic_ns() >= due)
                } else {
                    release_idle(self);
                    std::thread::park_timeout(Duration::from_nanos(
                        deadline.saturating_sub(crate::os::monotonic_ns()),
                    ));
                    false
                }
            }
            None => {
                if !self.net.is_null() {
                    let (timeout, release) = self.net_idle_wait(-1);
                    self.poll_net(timeout);
                    release.is_some_and(|due| crate::os::monotonic_ns() >= due)
                } else {
                    release_idle(self);
                    std::thread::park();
                    false
                }
            }
        };
        self.active();
        if release && !self.readies.has_pending() {
            self.release_net_idle();
        }
    }

    unsafe fn runnable(
        &mut self,
        task: *mut Task,
        state: OperationState,
        #[cfg(feature = "prof")] wake: Wake,
    ) {
        // SAFETY: the task is parked; its operation frame is pinned.
        unsafe {
            #[cfg(feature = "prof")]
            if prof::enabled() {
                let op = &mut *(*task).operation;
                prof::pending(op.token, std::mem::take(&mut op.prof_pending));
                let request = &*op.request;
                use crate::provider as host;
                let source = match wake {
                    Wake::Cancel => WakeSource::Cancel,
                    Wake::Timer => WakeSource::Timer,
                    Wake::Ready
                        if crate::os::owns(&op.provider)
                            && (host::unix_operation(request.operation)
                                || crate::os_net::is_operation(request.operation)) =>
                    {
                        WakeSource::Net
                    }
                    _ => WakeSource::Unknown,
                };
                prof_wake(task, source, ptr::null_mut(), op.token);
            }
            self.parked.remove((*(*task).operation).token);
            self.timers.remove(task);
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
            #[cfg(feature = "prof")]
            if wake != Wake::Parked {
                prof::pending((*op).token, std::mem::take(&mut (*op).prof_pending));
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
                            provider_callback!(token, Wait, (p.wait)(p.state, request, token))
                        }
                    };
                    // A cancel in W still runs wait at the wake (REPLAY-SPEC §3).
                    // Cancellation wins even if the callback returns Ready.
                    if wake == Wake::Cancel {
                        // A cell forgets the wait's registration (FAIL-15).
                        if (*request).operation == crate::foreign::helpers::WAIT_OP
                            || crate::provide::owns(&p)
                        {
                            provider_callback!(
                                token,
                                Abort,
                                (p.abort)(p.state, request, token, ABORT_CANCEL)
                            );
                        }
                        #[cfg(feature = "record")]
                        if let Some(group) = (*task).group {
                            group.cancel_wait(request);
                        }
                        self.runnable(
                            task,
                            OperationState::ReadyToDeliver(None),
                            #[cfg(feature = "prof")]
                            wake,
                        );
                        return false;
                    }
                    match w.kind {
                        WAIT_READY if starting => {
                            (*op).state = OperationState::ReadyToIssue;
                            return true;
                        }
                        WAIT_READY => {
                            #[cfg(feature = "record")]
                            if let Some(group) = (*task).group {
                                group.wake(request);
                            }
                            self.runnable(
                                task,
                                OperationState::ReadyToIssue,
                                #[cfg(feature = "prof")]
                                wake,
                            );
                            return false;
                        }
                        WAIT_UNTIL => {
                            self.timers.remove(task);
                            self.timers.insert(task, w.deadline_ns);
                        }
                        WAIT_PARK => self.timers.remove(task),
                        _ => {
                            stop::record_static(b"Provider");
                            return false;
                        }
                    }
                    #[cfg(feature = "prof")]
                    if (*op).prof_pending == 0 {
                        (*op).prof_pending = prof::timing_start();
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
                            let step = provider_callback!(
                                token,
                                Resume,
                                (p.resume)(p.state, request, token)
                            );
                            self.stepped(task, step);
                        }
                        Wake::Cancel | Wake::Timer => {
                            let reason = if wake == Wake::Cancel {
                                ABORT_CANCEL
                            } else {
                                ABORT_DEADLINE
                            };
                            #[cfg(feature = "record")]
                            if wake == Wake::Timer
                                && crate::provide::owns(&p)
                                && let Some(group) = (*task).group
                            {
                                group.wake_deadline();
                            }
                            let outcome = provider_callback!(
                                token,
                                Abort,
                                (p.abort)(p.state, request, token, reason)
                            );
                            self.runnable(
                                task,
                                OperationState::ReadyToDeliver(Some(outcome)),
                                #[cfg(feature = "prof")]
                                wake,
                            );
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
                STEP_DONE => self.runnable(
                    task,
                    OperationState::ReadyToDeliver(Some(step.outcome)),
                    #[cfg(feature = "prof")]
                    Wake::Ready,
                ),
                STEP_PENDING => {
                    #[cfg(feature = "prof")]
                    {
                        (*(*task).operation).prof_pending = prof::timing_start();
                    }
                    self.parked.insert((*(*task).operation).token, task);
                    self.timers.remove(task);
                    (*(*task).operation).state = OperationState::Issued {
                        deadline: step.deadline_ns,
                    };
                    if step.deadline_ns != 0 {
                        self.timers.insert(task, step.deadline_ns);
                    }
                }
                _ => stop::record_static(b"Provider"),
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
                && (*(*task).timer.get()).arm == 0
            {
                self.timers.insert(task, deadline);
            }
        }
    }
}

/// One provider operation, stored on its task's pinned stack. Each transition
/// owns the right to issue, resume, abort or deliver through this state.
pub(crate) struct Operation {
    #[cfg(feature = "prof")]
    prof_pending: u64,
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
    ABORT_CANCEL, ABORT_DEADLINE, STEP_DONE, STEP_PENDING, SYNC, SYNC_DIR, Step, WAIT_PARK,
    WAIT_READY, WAIT_UNTIL,
};
/// # Safety
/// A live task pins this flag.
unsafe fn cancel_flag<'a>(task: *const Task) -> &'a AtomicBool {
    // SAFETY: the caller pins the task and requests never borrow its Context.
    unsafe { &(*(*task).context).cancel_requested }
}

/// Whether the operation `token`, parked in the running task's run, has a
/// cancellation request it observes that the scheduler has not applied yet. A
/// served port's reply loses to it (CONC-7; provide NOTES 16), so the outcome
/// does not depend on whether the server yielded after `task.cancel`.
pub(crate) fn call_cancelled(token: u64) -> bool {
    let task = CURRENT.get();
    if task.is_null() {
        return false;
    }
    // SAFETY: the running task pins its run; a parked task pins its operation.
    unsafe {
        let Some(arm) = (*(*task).run).parked.live.get(&ReadyToken(token)) else {
            return false;
        };
        let caller = arm.task;
        let op = (*caller).operation;
        !op.is_null() && (*op).observes_cancel && cancel_flag(caller).load(Ordering::Acquire)
    }
}

/// The running task's context, for a provider's `deliver`.
pub(crate) fn current_context() -> *mut Context {
    let task = CURRENT.get();
    if task.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the running task pins its context.
    unsafe { (*task).context }
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
pub(crate) struct Readies {
    tokens: Mutex<Vec<u64>>,
    /// Through the pool's seam: the pool checker schedules around it.
    pending: crate::units::sync::AtomicBool,
    port_pending: crate::units::sync::AtomicBool,
    worker: std::thread::Thread,
    net: Mutex<Option<Arc<hirsel_core_net::Poller>>>,
    /// Host work this waiter depends on whose ready is still to come (a
    /// queued or running sync). It drops only after that ready is pushed, so
    /// a waiter that reads zero and then no pending ready is quiescent. The
    /// drop to zero notifies the waiter, which may have taken the ready and
    /// parked while the count was still one. Through the pool's seam, so the
    /// pool checker schedules that drop against the waiter's park.
    outside: crate::units::sync::AtomicUsize,
    /// mc-units-untied: the unit this waiter belongs to, or units::NONE.
    unit: u32,
    /// A scheduler's run (tasks/quiet.rs), and whether it idles quiet.
    run: Option<Arc<RunView>>,
    quiet: quiet::Quiet,
}
impl Readies {
    pub(crate) fn for_unit(unit: u32) -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(Vec::new()),
            pending: crate::units::sync::AtomicBool::new(false),
            port_pending: crate::units::sync::AtomicBool::new(false),
            worker: std::thread::current(),
            net: Mutex::new(None),
            outside: crate::units::sync::AtomicUsize::new(0),
            unit,
            run: None,
            quiet: quiet::Quiet::default(),
        })
    }
    fn outside(&self) -> bool {
        self.outside.load(Ordering::SeqCst) != 0
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<u64>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn enqueue(&self, token: u64) {
        self.lock().push(token);
        self.pending.store(true, Ordering::Release);
    }
    pub(crate) fn unit_id(&self) -> u32 {
        self.unit
    }
    /// The port head carries its own arrivals. There is no operation token
    /// to enqueue or capture when the port readies the scheduler.
    pub(crate) fn ready_port(&self) {
        self.port_pending.store(true, Ordering::Release);
        self.notify();
    }
    fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) || self.port_pending.load(Ordering::Acquire)
    }
    fn push(&self, token: u64) {
        self.enqueue(token);
        self.notify();
    }
    fn notify(&self) {
        self.wake_member();
        if self.unit != crate::units::NONE && crate::units::untied() {
            crate::units::wake_id(self.unit);
            return;
        }
        self.worker.unpark();
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
    #[cfg(test)]
    pub(crate) fn has_outside(&self) -> bool {
        self.outside()
    }
    #[cfg(test)]
    pub(crate) fn take_for_test(&self, token: u64) -> bool {
        self.take(token)
    }
}
/// Every live waiter: each run's scheduler, and a scheduler-less operation
/// while it waits. A token is unique in the process, so a ready reaches every
/// waiter and only the one holding that operation acts on it.
static WAITERS: Mutex<Vec<Arc<Readies>>> = Mutex::new(Vec::new());
struct Registered(Arc<Readies>);
impl Drop for Registered {
    fn drop(&mut self) {
        WAITERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|r| !Arc::ptr_eq(r, &self.0));
    }
}
static TOKENS: AtomicU64 = AtomicU64::new(1);

pub(crate) fn operation_token() -> u64 {
    let token = TOKENS.fetch_add(1, Ordering::Relaxed);
    if token >= channel_clock::TOKEN_TAG {
        stop::stop_static(b"HostHandles");
    }
    token
}
thread_local! {
    /// This thread's scheduler-less waiter, in WAITERS only while an
    /// operation alone waits (`alone`; depth counts nested callers).
    static ALONE: (Arc<Readies>, std::cell::Cell<usize>) = (Readies::for_unit(crate::units::NONE), std::cell::Cell::new(0));
}
/// The scheduler-less waiter's registration for one `alone` operation.
struct AloneWait(Arc<Readies>);
impl AloneWait {
    fn enter() -> Self {
        if let Some(u) = crate::units::running() {
            let readies = alone_waiter();
            if u.alone_depth.get() == 0 {
                WAITERS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(readies.clone());
            }
            u.alone_depth.set(u.alone_depth.get() + 1);
            return Self(readies);
        }
        let readies = ALONE.with(|(readies, depth)| {
            if depth.get() == 0 {
                WAITERS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(readies.clone());
            }
            depth.set(depth.get() + 1);
            readies.clone()
        });
        Self(readies)
    }
}
impl Drop for AloneWait {
    fn drop(&mut self) {
        if let Some(u) = crate::units::running() {
            u.alone_depth.set(u.alone_depth.get() - 1);
            if u.alone_depth.get() == 0 {
                WAITERS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|r| !Arc::ptr_eq(r, &self.0));
            }
            return;
        }
        ALONE.with(|(readies, depth)| {
            depth.set(depth.get() - 1);
            if depth.get() == 0 {
                WAITERS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|r| !Arc::ptr_eq(r, readies));
            }
        });
    }
}

/// mc-units-untied: the scheduler-less waiter is the running unit's, else
/// this thread's.
fn alone_waiter() -> Arc<Readies> {
    if let Some(u) = crate::units::running() {
        return u
            .alone
            .get_or_init(|| Readies::for_unit(crate::units::unit_id(u)))
            .clone();
    }
    ALONE.with(|(r, _)| r.clone())
}

/// Host work whose ready its issuer's waiter still awaits: the running task's
/// run, or this thread's scheduler-less waiter. While one is held that waiter
/// does not declare quiescence; drop it only after the ready is pushed. Each
/// run counts only its own, so one run's sync never holds another's verdict.
pub(crate) struct Outside(Arc<Readies>);
impl Outside {
    pub(crate) fn begin() -> Self {
        let mut task = CURRENT.get();
        if task.is_null() {
            task = CALLBACK_TASK.get();
        }
        let readies = if task.is_null() {
            alone_waiter()
        } else {
            // SAFETY: the issuing task pins its run for this call.
            unsafe { (*(*task).run).readies.clone() }
        };
        let outside = readies.outside.fetch_add(1, Ordering::SeqCst) + 1;
        // Room for each outstanding ready, so the helper's push to this
        // waiter does not allocate on its thread.
        readies.lock().reserve(outside);
        Self(readies)
    }

    /// Publish to the issuing waiter before releasing its outside work.
    pub(crate) fn complete(self, token: u64) {
        self.ready(token);
    }

    pub(crate) fn ready(&self, token: u64) {
        self.0.push(token);
    }

    #[cfg(test)]
    pub(crate) fn for_readies(readies: Arc<Readies>) -> Self {
        let outside = readies.outside.fetch_add(1, Ordering::SeqCst) + 1;
        readies.lock().reserve(outside);
        Self(readies)
    }
}
impl Drop for Outside {
    fn drop(&mut self) {
        // The waiter may have taken this work's ready and parked while the
        // count was still one: with no other wake to come it would wait for
        // ever instead of deciding quiescence (FAIL-15).
        if self.0.outside.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.notify();
        }
    }
}

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
/// mc-units-untied: at a park, this worker's thread-locals hold the idle
/// values every unit sees at its park point, so none is carried across. One
/// call reads each in place: nothing in it crosses a stack switch, so it
/// needs no `Moved` accessor.
#[inline(never)]
pub(crate) fn check_idle_tls() {
    if !CURRENT.get_here().is_null() || !CALLBACK_TASK.get_here().is_null() {
        stop::stop_static(b"UntiedParkWithTask");
    }
    crate::limit::check_unbound();
}

/// The end of operation `token`'s profile span.
#[cfg(feature = "prof")]
fn prof_io_end(token: u64, outcome: &crate::provider::Outcome) {
    prof::append(
        ProfKind::IoEnd,
        &[
            token,
            outcome.written,
            u64::from(outcome.error.tag),
            u64::from(outcome.error.code),
            u64::from(outcome.status),
        ],
        &[],
    );
}
/// `$call`'s outcome, ending its profile span when it was delivered; a
/// returnable park ends it at the resume that delivers.
macro_rules! provider_outcome_opt {
    ($token:expr, $call:expr) => {{
        let outcome: Option<crate::provider::Outcome> = $call;
        #[cfg(feature = "prof")]
        if let Some(outcome) = &outcome {
            prof_io_end($token, outcome);
        }
        outcome
    }};
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
    // SAFETY: forwarded; the stackful form always ends with an outcome.
    unsafe {
        provider_operation_in::<false>(context, provider, request, issued)
            .unwrap_or_else(|| unreachable!("a stackful operation never returns parked"))
    }
}
/// `provider_operation` in the returnable form: None means the operation
/// parked with its state in the task's frame record, and the caller returns
/// through its represented frames (`hirsel_core_frame_record`).
/// # Safety
/// As `provider_operation`; the request lives in the current task's frame
/// record, which exists.
#[inline(always)]
pub(crate) unsafe fn provider_operation_returnable(
    context: *mut Context,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
) -> Option<crate::provider::Outcome> {
    // SAFETY: forwarded.
    unsafe { provider_operation_in::<true>(context, provider, request, None) }
}
#[inline(always)]
unsafe fn provider_operation_in<const RETURNABLE: bool>(
    context: *mut Context,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> Option<crate::provider::Outcome> {
    let token = operation_token();
    if token == u64::MAX {
        stop::stop_static(b"HostHandles");
    }
    let task = CURRENT.get();
    // SAFETY: provider contract; request pinned by the caller through delivery.
    unsafe {
        #[cfg(feature = "prof")]
        {
            let [unit, id] = prof_key(task);
            prof::append(
                ProfKind::IoBegin,
                &[
                    token,
                    unit,
                    id,
                    u64::from((*request).operation),
                    (*request).site,
                ],
                &[],
            );
        }
        if task.is_null() {
            return provider_outcome_opt!(
                token,
                Some(slow(
                    context,
                    task,
                    provider,
                    request,
                    token,
                    Start::Alone,
                    issued,
                ))
            );
        }
        #[cfg(test)]
        if SCHEDULER_ENTRY.get() {
            let start = Start::Entry(None);
            return provider_outcome_opt!(
                token,
                off_fast_path::<RETURNABLE>(context, task, provider, request, token, start, issued)
            );
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
            provider_outcome_opt!(
                token,
                operation::<OsCalls, RETURNABLE>(context, task, provider, request, token, issued)
            )
        } else {
            provider_outcome_opt!(
                token,
                operation::<PointerCalls, RETURNABLE>(
                    context, task, provider, request, token, issued
                )
            )
        }
    }
}
/// Leave the fast path in the form the caller runs.
#[inline(always)]
unsafe fn off_fast_path<const RETURNABLE: bool>(
    context: *mut Context,
    task: *mut Task,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    start: Start,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> Option<crate::provider::Outcome> {
    // SAFETY: forwarded.
    unsafe {
        if RETURNABLE {
            returnable(context, task, provider, request, token, start)
        } else {
            Some(slow(context, task, provider, request, token, start, issued))
        }
    }
}
/// The fast path of `provider_operation` with a scheduler: one body for every
/// provider, generic only over how it calls `wait` and `deliver`.
/// # Safety
/// As `provider_operation`; `task` is the current task.
#[inline(always)]
unsafe fn operation<C: Calls, const RETURNABLE: bool>(
    context: *mut Context,
    task: *mut Task,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> Option<crate::provider::Outcome> {
    // SAFETY: provider contract; request pinned by the caller through delivery.
    unsafe {
        CALLBACK_TASK.set(task);
        CURRENT.set(ptr::null_mut());
        let wait = provider_callback!(token, Wait, C::wait(provider, request, token));
        CURRENT.set(task);
        CALLBACK_TASK.set(ptr::null_mut());
        if wait.kind != WAIT_READY {
            let start = Start::Entry(Some(wait));
            return off_fast_path::<RETURNABLE>(
                context, task, provider, request, token, start, issued,
            );
        }
        let step = provider_callback!(
            token,
            Issue,
            crate::provider::issue(provider, request, token)
        );
        if step.kind != STEP_DONE {
            let start = Start::Issued(step);
            return off_fast_path::<RETURNABLE>(
                context, task, provider, request, token, start, issued,
            );
        }
        if let Some(issued) = issued {
            *issued = Some(step.outcome);
        }
        Some(provider_callback!(
            token,
            Deliver,
            C::deliver(provider, request, step.outcome)
        ))
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
        delivered(context, &provider, request, token, outcome, issued)
    }
}
/// The end of an operation that left the fast path, in either form.
/// # Safety
/// As `slow`; the operation has ended.
unsafe fn delivered(
    context: *mut Context,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    #[cfg_attr(not(feature = "prof"), allow(unused_variables))] token: u64,
    outcome: Option<crate::provider::Outcome>,
    issued: Option<&mut Option<crate::provider::Outcome>>,
) -> crate::provider::Outcome {
    // SAFETY: forwarded.
    unsafe {
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
        provider_callback!(
            token,
            Deliver,
            (provider.deliver)(provider.state, request, outcome)
        )
    }
}
/// The returnable form of `slow` with a scheduler: the operation's state is
/// built in the task's frame record, and a park returns None instead of
/// switching, so the caller gives its stack back through its represented frames.
#[cold]
#[inline(never)]
unsafe fn returnable(
    context: *mut Context,
    task: *mut Task,
    provider: &crate::provider::Provider,
    request: *mut crate::provider::Request,
    token: u64,
    start: Start,
) -> Option<crate::provider::Outcome> {
    // SAFETY: the record outlives every park; the scheduler clears no field.
    unsafe {
        let op = (*task)
            .frame
            .as_ref()
            .unwrap_or_else(|| stop::stop_static(b"FrameRecord"))
            .operation();
        op.write(Operation::new(task, provider, request, token));
        match (*op).begin(start) {
            Next::Park => {
                park_returnable(task, op);
                None
            }
            Next::Done(produced) => Some(delivered(
                context,
                &(*op).provider,
                request,
                token,
                produced,
                None,
            )),
        }
    }
}
/// Resume a returnable operation the scheduler made runnable: the same step
/// the stackful form takes after its switch back. None means it parked again.
/// # Safety
/// The current task's operation is the one in its frame record.
pub(crate) unsafe fn resume_returnable(context: *mut Context) -> Option<crate::provider::Outcome> {
    // The stackful form holds this commit from its resume through delivery.
    // SAFETY: by the contract.
    unsafe {
        let task = CURRENT.get();
        let op = (*task).operation;
        debug_assert!(op == (*task).frame.as_ref().unwrap().operation().cast());
        match (*op).advance(None) {
            Next::Park => {
                park_returnable(task, op);
                None
            }
            Next::Done(produced) => {
                (*task).operation = ptr::null_mut();
                let request = (*op).request;
                let outcome = delivered(
                    context,
                    &(*op).provider,
                    request,
                    (*op).token,
                    produced,
                    None,
                );
                #[cfg(feature = "prof")]
                prof_io_end((*op).token, &outcome);
                Some(outcome)
            }
        }
    }
}
/// Park as `park_operation` does, in the same order, but leave the switch to
/// `start`, after the represented frames have returned.
unsafe fn park_returnable(task: *mut Task, op: *mut Operation) {
    // SAFETY: the active task owns its operation and record.
    unsafe {
        (*task).operation = op;
        mark_parked(task);
        give_back(task);
    }
}
/// The stack release every returnable park ends with: the task returns its
/// stack through its represented frames (`start` switches to the dispatcher,
/// which unmounts it).
unsafe fn give_back(task: *mut Task) {
    // SAFETY: the active task owns its record.
    unsafe { (*task).stackless = true };
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
    // SAFETY: the current fiber owns the context and the request.
    let mut op = unsafe { Operation::new(task, provider, request, token) };
    // SAFETY: the frame outlives every park; the scheduler clears no field.
    unsafe {
        let mut next = op.begin(start);
        loop {
            match next {
                Next::Park => {
                    (*task).operation = &mut op;
                    park_operation(task);
                    next = op.advance(None);
                }
                Next::Done(produced) => {
                    (*task).operation = ptr::null_mut();
                    return produced;
                }
            }
        }
    }
}
/// Where one step of a parked operation leaves it.
enum Next {
    /// It waits for the scheduler's next verdict.
    Park,
    /// It ended: a missing outcome is the unissued operation's cancellation.
    Done(Option<crate::provider::Outcome>),
}
impl Operation {
    /// # Safety
    /// The current fiber owns the task's context and the request.
    unsafe fn new(
        task: *mut Task,
        provider: &crate::provider::Provider,
        request: *mut crate::provider::Request,
        token: u64,
    ) -> Self {
        Self {
            #[cfg(feature = "prof")]
            prof_pending: 0,
            provider: *provider,
            request,
            token,
            state: OperationState::Waiting,
            // SAFETY: by the contract.
            observes_cancel: unsafe {
                crate::provider::cancellation_point((*request).operation)
                    && (*(*task).context).cancel_shield == 0
                    && (*(*task).context).finalizing == 0
            },
            entered: None,
        }
    }
    /// The operation's first step off the fast path.
    /// # Safety
    /// The request is pinned; the operation is not yet parked.
    unsafe fn begin(&mut self, start: Start) -> Next {
        match start {
            Start::Entry(entered) => {
                self.entered = entered;
                Next::Park
            }
            // SAFETY: forwarded.
            Start::Issued(step) => unsafe { self.advance(Some(step)) },
            Start::Alone => unreachable!("an operation alone never parks"),
        }
    }
    /// One step of a parked operation, from `step` or, with none, from the
    /// scheduler's verdict: both forms (stackful and returnable) take it.
    /// # Safety
    /// The request is pinned; the scheduler has made the task runnable.
    unsafe fn advance(&mut self, step: Option<Step>) -> Next {
        // SAFETY: by the contract.
        unsafe {
            let step = match step {
                Some(step) => step,
                None => match self.state {
                    OperationState::ReadyToIssue => {
                        provider_callback!(
                            self.token,
                            Issue,
                            crate::provider::issue(&self.provider, self.request, self.token)
                        )
                    }
                    OperationState::ReadyToDeliver(outcome) => return Next::Done(outcome),
                    _ => unreachable!("resumed provider operation has no next action"),
                },
            };
            match step.kind {
                STEP_DONE => Next::Done(Some(step.outcome)),
                STEP_PENDING => {
                    // An issued sync is not cancellable (a provider that
                    // parks one, or its replay from a tape). A submitted
                    // foreign call decides the race with cancellation when
                    // its helper takes custody of the pinned frame.
                    if matches!(
                        (*self.request).operation,
                        SYNC | SYNC_DIR | crate::foreign::helpers::WAIT_OP
                    ) {
                        self.observes_cancel = false;
                    }
                    self.state = OperationState::Issued {
                        deadline: step.deadline_ns,
                    };
                    Next::Park
                }
                _ => stop::stop_static(b"Provider"),
            }
        }
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
        mark_parked(task);
        switch_to_idle(task);
    }
}
/// The scheduler-visible half of a park, shared by both forms.
unsafe fn mark_parked(task: *mut Task) {
    // SAFETY: as park_operation.
    unsafe {
        let op = (*task).operation;
        if matches!((*op).state, OperationState::Issued { .. }) {
            (*(*task).run).parked.insert((*op).token, task);
        }
        #[cfg(feature = "prof")]
        {
            (*op).prof_pending = if (*op).entered.is_some()
                || matches!((*op).state, OperationState::Issued { .. })
            {
                prof::timing_start()
            } else {
                0
            };
            let reason = if matches!(
                (*(*op).request).operation,
                crate::provider::SLEEP_UNTIL | crate::provider::SLEEP_UNTIL_WALL
            ) {
                Reason::Timer
            } else {
                Reason::ProviderIo
            };
            prof_state(task, State::Provider, reason, (*op).token);
        }
        (*task).state = State::Provider;
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
    let registration = AloneWait::enter();
    let readies = registration.0.clone();
    struct AloneNet(Arc<Readies>);
    impl Drop for AloneNet {
        fn drop(&mut self) {
            self.0.net.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }
    let _network = AloneNet(readies.clone());
    readies.take(0);
    // Wait until readiness (true) or the deadline (false).
    let block = |deadline: Option<u64>| -> bool {
        loop {
            if crate::units::running().is_some() {
                // mc-units-untied: the unit parks; its worker runs other units.
                if !unsafe { (*context).host_files }.is_null() {
                    // SAFETY: the unit's own registry, pinned through this wait.
                    unsafe {
                        let sockets = &mut (*(*context).host_files).sockets;
                        sockets
                            .poll(0, |token| readies.push(token))
                            .unwrap_or_else(|_| stop::stop_static(b"HostPoll"));
                        crate::units::take_xready(|id, flags| {
                            sockets.dispatch(id, flags, |token| readies.push(token))
                        });
                    }
                }
                if readies.pending.load(Ordering::Acquire) && readies.take(token) {
                    return true;
                }
                if deadline.is_some_and(|at| at <= crate::os::monotonic_ns()) {
                    return false;
                }
                crate::units::park(deadline);
                continue;
            }
            if !unsafe { (*context).host_files }.is_null() {
                // SAFETY: direct ABI caller pins its registry through this wait.
                unsafe {
                    (*(*context).host_files)
                        .sockets
                        .poll(0, |token| readies.push(token))
                }
                .unwrap_or_else(|_| stop::stop_static(b"HostPoll"));
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
                .unwrap_or_else(|_| stop::stop_static(b"HostPoll"));
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
    let wait = |deadline| {
        #[cfg(feature = "prof")]
        let start = prof::timing_start();
        let ready = block(deadline);
        #[cfg(feature = "prof")]
        prof::pending(token, start);
        ready
    };
    // SAFETY: provider contract; request pinned by the caller.
    unsafe {
        loop {
            let w = provider_callback!(token, Wait, (p.wait)(p.state, request, token));
            match w.kind {
                WAIT_READY => break,
                WAIT_UNTIL => {
                    wait(Some(w.deadline_ns));
                }
                WAIT_PARK => {
                    // Only the program readies a cell's wait, and no other task runs.
                    if crate::provide::owns(p)
                        && !crate::provide::quiesce(crate::provide::Scope::Token(token))
                    {
                        stop::stop_static(b"Deadlock");
                    }
                    wait(None);
                }
                _ => stop::stop_static(b"Provider"),
            }
        }
        let mut step = provider_callback!(token, Issue, crate::provider::issue(p, request, token));
        loop {
            match step.kind {
                STEP_DONE => return Some(step.outcome),
                STEP_PENDING => {
                    // A call on a served port waits on its server, and no
                    // other task runs to serve it (FAIL-15).
                    if crate::provide::owns(p) {
                        stop::stop_static(b"Deadlock");
                    }
                    let deadline = (step.deadline_ns != 0).then_some(step.deadline_ns);
                    if wait(deadline) {
                        step =
                            provider_callback!(token, Resume, (p.resume)(p.state, request, token));
                    } else {
                        return Some(provider_callback!(
                            token,
                            Abort,
                            (p.abort)(p.state, request, token, ABORT_DEADLINE)
                        ));
                    }
                }
                _ => stop::stop_static(b"Provider"),
            }
        }
    }
}

/// Replay dispatches before the first read or mutation of shared state.
#[inline(always)]
pub(crate) fn before_commit() {
    #[cfg(feature = "record")]
    replay_boundary(false);
}

/// `before_commit` with the task already resolved by the operation entry.
#[inline(always)]
pub(crate) fn before_commit_for(_task: *mut Task) {
    #[cfg(feature = "record")]
    replay_boundary_for(_task, false);
}

/// Only a provider entry can reach a taped WAIT without first committing.
/// Inline: a task that is not replaying pays one field read for the boundary.
#[inline(always)]
pub(crate) fn before_provider_entry() {
    #[cfg(feature = "record")]
    {
        let task = CURRENT.get();
        // SAFETY: CURRENT pins this task.
        if !task.is_null() && unsafe { (*task).replay } {
            replay_boundary(true);
        }
    }
}

#[cfg(feature = "record")]
#[path = "tasks_tape.rs"]
mod tape_tasks;
#[cfg(feature = "record")]
pub(crate) use tape_tasks::{attach_tape, before_tape_frame, detach_tape, group_index};
#[cfg(feature = "record")]
use tape_tasks::{live_index, replay_boundary, replay_boundary_for};

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
        // The event commit's mailbox drain: a take-point position (kind 0).
        #[cfg(feature = "record")]
        crate::tape::units::reach((*task_pointer).group);
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
        #[cfg(feature = "record")]
        {
            let opens_pair = matches!(kind, LATEST_READ | LATEST_PUBLISH);
            let was_pair = task.latest_pair;
            if let Some(group) = task.group {
                if was_pair
                    && task.replay
                    && !matches!(
                        group.turn(),
                        crate::tape::schedule::Turn::Fifo
                            | crate::tape::schedule::Turn::Event(Some(_))
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
/// sink, test schedule, open LATEST pair or pending credit. Such an event needs no token; any other goes through `event_in`.
/// # Safety
/// `context` is the running task's, with a nonzero event word.
pub(crate) unsafe fn arc_unobserved(context: *const Context) -> bool {
    // SAFETY: the event word pins the running task and its scheduler. The
    // task alone writes its tape and pair state while it runs.
    unsafe {
        let task = &*((*context).events as *const Task);
        !task.arc_runtime && !task.arc_hooks && !(*context).current.pending()
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
        let run = (*task).run;
        // A serial task on a FIFO tape is never preempted for readiness: the
        // tape records where an input arrives, not whether the running task
        // was still runnable then, so readiness is taken only where the task
        // leaves the processor by its own logic (park, provider wait, poll
        // turn, slice end, end), which replay reaches by itself.
        #[cfg(feature = "record")]
        let preemptible = || !(*task).group.is_some_and(|group| group.fifo());
        #[cfg(not(feature = "record"))]
        let preemptible = || true;
        if !(*run).net.is_null() && preemptible() {
            (*run).probes_left -= 1;
            if (*run).probes_left == 0 {
                (*run).probes_left = LIVE_PROBE_INTERVAL;
                (*run).poll_net(0);
            }
            // Readiness another thread already queued is taken at every event.
            if (*run).readies.as_ref().pending.load(Ordering::Acquire) {
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
        if !(*run).head.is_null() || !(*run).deferred.is_null() {
            (*task).state = State::Ready;
            #[cfg(feature = "prof")]
            prof_state(task, State::Ready, Reason::None, 0);
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
    before_commit();
    event(CAPACITY, requested, available);
    Capacity {
        requested,
        available,
    }
}
#[cfg(feature = "prof")]
unsafe fn prof_key(task: *mut Task) -> [u64; 2] {
    if task.is_null() {
        return [0, 0];
    }
    // SAFETY: caller pins the task and its scheduler. Both keys are immutable.
    unsafe { [(*(*task).run).prof_id, (*task).id] }
}

#[cfg(feature = "prof")]
pub(crate) fn prof_current_task() -> [u64; 2] {
    let task = CURRENT.get();
    // SAFETY: either binding pins the task through the active callback or dispatch.
    unsafe {
        prof_key(if task.is_null() {
            CALLBACK_TASK.get()
        } else {
            task
        })
    }
}

#[cfg(feature = "prof")]
unsafe fn prof_created(task: *mut Task, parent: *mut Task) {
    // SAFETY: caller pins both task keys.
    let ([unit, id], [parent_unit, parent_id]) = unsafe { (prof_key(task), prof_key(parent)) };
    prof::append(ProfKind::Task, &[unit, id, parent_unit, parent_id], &[]);
}

#[cfg(feature = "prof")]
unsafe fn prof_state(task: *mut Task, state: State, reason: Reason, object: u64) {
    // SAFETY: the transition pins the task.
    prof::task_state(unsafe { prof_key(task) }, state, reason, object);
}

#[cfg(feature = "prof")]
unsafe fn prof_dispatch(task: *mut Task, entering: bool) {
    if task.is_null() {
        return;
    }
    let index = 0u64;
    // SAFETY: the dispatch pins the task.
    let [unit, id] = unsafe { prof_key(task) };
    let kind = if entering {
        ProfKind::DispatchBegin
    } else {
        ProfKind::DispatchEnd
    };
    prof::append(kind, &[unit, id, index], &[]);
}

#[cfg(feature = "prof")]
unsafe fn prof_wake(task: *mut Task, kind: WakeSource, source: *mut Task, object: u64) {
    // SAFETY: the wake pins the destination and optional source.
    let ([unit, id], [src_unit, src_id]) = unsafe { (prof_key(task), prof_key(source)) };
    prof::append(
        ProfKind::Wake,
        &[unit, id, kind as u64, src_unit, src_id, object],
        &[],
    );
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
#[cfg(feature = "record")]
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
#[cfg(feature = "record")]
pub(crate) fn provider_group() -> Option<crate::tape::schedule::Group> {
    let task = if CURRENT.get().is_null() {
        CALLBACK_TASK.get()
    } else {
        CURRENT.get()
    };
    // SAFETY: the callback's task is pinned for the synchronous callback.
    unsafe { task.as_ref().and_then(|t| t.group) }
}

/// Whether the running task has exclusive custody of this heap.
pub(crate) fn owns_current_heap(heap: &Arc<crate::task_heap::Heap>) -> bool {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins its task and context, or is null outside a task.
    unsafe {
        task.as_ref()
            .and_then(|task| task.context.as_ref())
            .and_then(|context| context.heap.get())
            .is_some_and(|owner| Arc::ptr_eq(owner, heap))
    }
}

#[cfg(feature = "record")]
pub(crate) fn current_group() -> Option<crate::tape::schedule::Group> {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins its task, or is null outside a task.
    unsafe { task.as_ref().and_then(|t| t.group) }
}
pub(crate) fn attach_net(
    sockets: *mut hirsel_core_net::Sockets,
    poller: Arc<hirsel_core_net::Poller>,
) {
    let mut task = CURRENT.get();
    if task.is_null() {
        task = CALLBACK_TASK.get();
    }
    if task.is_null() {
        *alone_waiter().net.lock().unwrap_or_else(|e| e.into_inner()) = Some(poller);
    } else {
        // SAFETY: the callback or task pins the scheduler and its registry.
        unsafe { (*(*task).run).set_net(sockets, poller) };
    }
}

fn poll_timeout(deadline: Option<u64>) -> i32 {
    deadline.map_or(-1, |at| {
        at.saturating_sub(crate::os::monotonic_ns())
            .div_ceil(1_000_000)
            .min(i32::MAX as u64) as i32
    })
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
    let run = unsafe { &mut *task.run };
    let script = run.script.clone()?;
    #[cfg(feature = "record")]
    {
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
    #[cfg(not(feature = "record"))]
    Some((script, UNTAPED))
}
/// Whether a task is running on this thread (tests assert where callbacks run).
#[cfg(test)]
pub(crate) fn request_flow_fixture_cancel() {
    // SAFETY: the test hook is on the owner's active stack.
    unsafe { cancel_flag(CURRENT.get()).store(true, Ordering::Release) };
}

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
pub unsafe fn checkpoint() {
    // SAFETY: this passes the same active fiber and pinned state.
    unsafe {
        checkpoint_inner(
            #[cfg(feature = "prof")]
            Reason::None,
        )
    }
}

/// # Safety
/// As for checkpoint, after installing a new step slice.
pub(crate) unsafe fn step_checkpoint() {
    // SAFETY: this passes the same active fiber and pinned state.
    unsafe {
        checkpoint_inner(
            #[cfg(feature = "prof")]
            Reason::StepSlice,
        )
    }
}

#[inline(always)]
unsafe fn checkpoint_inner(#[cfg(feature = "prof")] reason: Reason) {
    let task = CURRENT.get();
    // SAFETY: saving exactly this fiber; idle and task stacks stay mapped.
    unsafe {
        if !task.is_null() {
            let run = (*task).run;
            (*run).ready_with_reason(
                task,
                #[cfg(feature = "prof")]
                reason,
            );
            switch_to_idle(task);
        }
    }
}

#[cfg(test)]
thread_local! { static TIMER_SAMPLE: Local<Option<u64>> = const { Local::new(None) }; }

/// Use a specified instant on the next scheduler turn of a single-worker test.
#[cfg(test)]
pub(crate) unsafe fn checkpoint_at(now: u64) {
    TIMER_SAMPLE.set(Some(now));
    // SAFETY: the test calls this at an active fiber's checkpoint.
    unsafe { checkpoint() };
    TIMER_SAMPLE.set(None);
}

/// The task's thread-locals before its unit parks and after it resumes, in
/// one call each: both run on the unit's stack between switches.
#[inline(never)]
fn leave_task_tls() {
    #[cfg(feature = "prof")]
    {
        let task = CURRENT.get();
        // SAFETY: CURRENT pins the outgoing task.
        unsafe { prof_dispatch(task, false) };
    }
    CURRENT.set_here(ptr::null_mut());
    crate::fiber::guard_here((0, 0, 0));
}
/// # Safety
/// `task` is the resuming task, pinned by its run.
#[inline(never)]
unsafe fn enter_task_tls(task: *mut Task) {
    #[cfg(feature = "prof")]
    // SAFETY: the switch pins the resuming task.
    unsafe {
        prof_dispatch(task, true)
    };
    CURRENT.set_here(task);
    // SAFETY: by the contract.
    crate::fiber::guard_here(unsafe { (*task).stack.as_ref().unwrap().bounds() });
}

#[path = "tasks_beside.rs"]
mod beside;
pub(crate) use beside::run_beside;

pub(crate) fn current_task() -> *mut Task {
    CURRENT.get()
}

/// The running task's unit (units_lang), made at its run's first unit
/// operation.
/// # Safety
/// A task is running and holds the commit.
pub(crate) unsafe fn lang_unit() -> *mut crate::units_lang::LangUnit {
    // SAFETY: forwarded from the active task or callback.
    unsafe { lang_unit_for(CURRENT.get()) }
}

/// # Safety
/// `task` is the active task, or null in a callback.
#[inline(always)]
pub(crate) unsafe fn lang_unit_for(task: *mut Task) -> *mut crate::units_lang::LangUnit {
    // SAFETY: the running task pins its run.
    unsafe {
        if !task.is_null() && !(*(*task).run).lang.is_null() {
            return (*(*task).run).lang;
        }
        lang_unit_slow(task)
    }
}
/// # Safety
/// As `lang_unit_for`.
#[cold]
#[inline(never)]
unsafe fn lang_unit_slow(mut task: *mut Task) -> *mut crate::units_lang::LangUnit {
    if task.is_null() {
        task = CALLBACK_TASK.get();
    }
    if task.is_null() {
        stop::stop_static(b"UnitOutsideTask");
    }
    // SAFETY: the running task pins its run.
    unsafe {
        let run = (*task).run;
        if (*run).lang.is_null() {
            (*run).lang = crate::units_lang::root_unit(&(*run).readies);
            (*run).lang_owned = true;
        }
        (*run).lang
    }
}

/// units_lang: when the running task is the only thing its run could run
/// next (no ready task, timer, socket, sync, cancellation or tape), park its
/// unit right here instead of switching to the scheduler, whose next turn
/// would only park the unit too. Returns false, doing nothing, otherwise.
/// The caller pumps the unit's port and rechecks its condition either way;
/// a pump that readies another task sends the next wait the long way.
/// # Safety
/// The caller holds no commit and pins its condition across the park.
pub(crate) unsafe fn park_unit_alone(task: *mut Task, spin: impl FnOnce() -> bool) -> bool {
    if task.is_null() || !crate::units::untied() || !CALLBACK_TASK.get().is_null() {
        return false;
    }
    // SAFETY: the running task pins its run; only this thread uses it.
    unsafe {
        let run = &mut *(*task).run;
        #[cfg(feature = "record")]
        if !run.tapes.live.is_empty()
            || run.tapes.take_inputs.is_some()
            || !run.outside_cancels.is_empty()
        {
            return false;
        }
        if !run.net.is_null()
            || !run.head.is_null()
            || !run.deferred.is_null()
            || !run.cancels.is_empty()
            || !run.timers.root.is_null()
            || !run.parked.live.is_empty()
            || run.readies.outside()
            || run.readies.has_pending()
        {
            return false;
        }
        if spin() {
            return true;
        }
        leave_task_tls();
        crate::limit::Funding::unbind();
        crate::units::park(None);
        // The caller takes its arrivals after resuming. Clear their flag
        // independently of the scheduler's ready tokens.
        let run = &*(*task).run;
        run.readies.port_pending.store(false, Ordering::Relaxed);
        enter_task_tls(task);
        abi::task_resume((*task).context);
    }
    true
}

/// # Safety
/// The active task holds the condition's owner alive across suspension. No
/// references to scheduler/task state survive the switch.
pub(crate) unsafe fn park(queue: *mut Waiters, #[cfg(feature = "prof")] wait: (Reason, u64)) {
    // SAFETY: the active task and queue meet the contract.
    unsafe {
        park_for(
            CURRENT.get(),
            queue,
            #[cfg(feature = "prof")]
            wait,
        )
    }
}

/// The deadline source is already resolved in the clock's own domain.
#[derive(Clone, Copy)]
pub(crate) enum ChannelDeadline {
    Host(u64),
    Cell(crate::provide::ChannelClock, u64),
    /// While replaying, the tape supplies an external deadline's wake.
    #[cfg(feature = "record")]
    Recorded,
}

/// Wait on a channel and an optional deadline. Returns false if the clock
/// reached a local cell deadline before registration.
/// # Safety
/// The running task pins the channel, clock and its wait links throughout.
pub(crate) unsafe fn park_channel(
    queue: *mut Waiters,
    id: u64,
    deadline: ChannelDeadline,
    recorded: bool,
) -> bool {
    // SAFETY: the active task owns its wait state and the channel belongs to
    // this unit. The cell's lock owns its timer links while registered.
    unsafe {
        let task = CURRENT.get();
        let run = (*task).run;
        if let ChannelDeadline::Cell(clock, at) = deadline {
            let arm = channel_clock::Arms::initialize(task, at);
            if !clock.arm(arm, at, recorded) {
                channel_clock::reset(task);
                return false;
            }
        }
        if let ChannelDeadline::Host(at) = deadline {
            (*run).timers.insert(task, at);
        }
        record_event(task, RECEIVE, id, hirsel_core_layout::event::CHANNEL_PARKED);
        #[cfg(feature = "record")]
        if recorded {
            (*task).channel_tape = ChannelTape::Parked;
        }
        #[cfg(feature = "prof")]
        prof_state(task, State::Waiting, Reason::ChannelRecv, id);
        (*task).state = match deadline {
            ChannelDeadline::Cell(..) => State::CellChannelDeadline,
            _ => State::ChannelDeadline,
        };
        Waiters::push(queue, task);
        switch_to_idle(task);
        #[cfg(feature = "record")]
        {
            (*task).channel_tape = ChannelTape::None;
        }
        match deadline {
            ChannelDeadline::Host(_) => (*run).timers.remove(task),
            ChannelDeadline::Cell(clock, _) => {
                clock.retire((*task).timer.get().cast());
                channel_clock::reset(task);
            }
            #[cfg(feature = "record")]
            ChannelDeadline::Recorded => {}
        }
        true
    }
}

#[cfg(test)]
pub(crate) fn channel_deadline_registered() -> bool {
    let task = CURRENT.get();
    // SAFETY: the running task owns its detached timer slot.
    unsafe { (*(*task).timer.get()).arm != 0 }
}

/// The returnable form of `park_for`: register in `queue` the same way, then
/// leave the switch to `start` after the represented frames have returned.
/// The resumed wait reenters its row and continues where this park left off.
/// # Safety
/// As for `park_for`, with a live task that runs a returnable root.
pub(crate) unsafe fn park_returning(
    task: *mut Task,
    queue: *mut Waiters,
    #[cfg(feature = "prof")] wait: (Reason, u64),
) {
    // SAFETY: as park_for.
    unsafe {
        #[cfg(feature = "prof")]
        prof_state(task, State::Waiting, wait.0, wait.1);
        (*task).state = State::Waiting;
        Waiters::push(queue, task);
        give_back(task);
    }
}

/// # Safety
/// As for `park`, with the active task passed by the caller.
pub(crate) unsafe fn park_for(
    task: *mut Task,
    queue: *mut Waiters,
    #[cfg(feature = "prof")] wait: (Reason, u64),
) {
    // SAFETY: the condition was just checked and the queue is pinned.
    unsafe {
        if !task.is_null() {
            #[cfg(feature = "prof")]
            prof_state(task, State::Waiting, wait.0, wait.1);
            (*task).state = State::Waiting;
            Waiters::push(queue, task);
            switch_to_idle(task);
        }
    }
}

/// Whether the running task's waits may park with its stack given back: it
/// runs a returnable root (`FrameRecord`) and is not replaying a tape, whose
/// boundaries a park that returns has not been audited against.
pub(crate) fn returnable_waits() -> bool {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the task while this fiber is executing.
    unsafe {
        let Some(task) = task.as_ref() else {
            return false;
        };
        #[cfg(feature = "record")]
        if task.replay {
            return false;
        }
        task.frame.is_some()
    }
}

/// Record a waking event for the running task and wake `queue`.
/// # Safety
/// The caller holds the condition's owner alive through the event.
pub(crate) unsafe fn event_wake_waiters(kind: u8, object: u64, value: u64, queue: *mut Waiters) {
    let task = CURRENT.get();
    // SAFETY: recording and waking happen before any switch.
    unsafe {
        if !task.is_null() {
            record_event(task, kind, object, value);
            Waiters::wake_all(
                queue,
                #[cfg(feature = "prof")]
                (WakeSource::Task, task, object),
            );
            poll_event(task, kind, value);
        }
    }
}

/// # Safety
/// The context and task belong to the running operation. Task may be null.
#[inline(always)]
pub(crate) unsafe fn cancel_pending_for(context: *mut Context, task: *mut Task) -> bool {
    // SAFETY: the operation pins its exclusive, aligned task context.
    let current = unsafe { NonNull::new_unchecked(context).as_ref() };
    let requested = if task.is_null() {
        &current.cancel_requested
    } else {
        // SAFETY: the active fiber pins the task.
        unsafe { cancel_flag(task) }
    };
    cancel_takes_effect(current, requested)
}

/// # Safety
/// The caller is the current task and pins its cancellation flag through
/// delivery. Other threads may only load the flag atomically.
pub(crate) unsafe fn foreign_cancel_flag(context: *const Context) -> *const AtomicBool {
    // SAFETY: the current task pins this context and its request flag.
    unsafe {
        if (*context).cancel_shield != 0 || (*context).finalizing != 0 {
            ptr::null()
        } else {
            cancel_flag(CURRENT.get())
        }
    }
}

/// # Safety
/// Context belongs to this task; primary is available on a normal path.
#[inline(always)]
pub unsafe fn check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the operation pins the current task and its context.
    unsafe { check_cancel_for(context, site, CURRENT.get()) }
}

/// # Safety
/// As for `check_cancel`, with the active task passed by the caller.
#[inline(always)]
pub(crate) unsafe fn check_cancel_for(context: *mut Context, site: u64, task: *mut Task) -> u8 {
    // SAFETY: the operation pins the running task and its context.
    if unsafe { cancel_pending_for(context, task) } {
        // SAFETY: this task exclusively owns the observed request and primary.
        unsafe { observe_cancel(context, site, task) };
        1
    } else {
        0
    }
}
#[cold]
#[inline(never)]
unsafe fn observe_cancel(context: *mut Context, site: u64, task: *mut Task) {
    // SAFETY: the operation entry exclusively owns this task context.
    unsafe {
        before_commit();
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

/// Give a task that returned its stack a fresh one to resume on: its body
/// starts again and its represented frames replay to the parked operation.
/// # Safety
/// The task is runnable, off-CPU and has no stack.
unsafe fn mount(task: *mut Task, stack: Stack) {
    // SAFETY: by the contract; the stack is idle.
    unsafe {
        (*task).sp = stack.prepare(start, task.cast(), ptr::null_mut());
        (*task).stack = Some(stack);
    }
}
/// Take back the stack of a task that parked in the returnable form.
/// # Safety
/// The task has just switched out of `start` for the last time on this stack.
unsafe fn unmount(task: *mut Task) -> Stack {
    // SAFETY: by the contract, nothing on the stack is live.
    unsafe {
        (*task).stackless = false;
        let stack = (*task).stack.take().unwrap();
        #[cfg(any(test, debug_assertions, feature = "testing"))]
        stack.poison_above((*task).sp);
        (*task).sp = ptr::null_mut();
        stack
    }
}
/// One block of the task's unit heap at zero logical charge, outside the
/// observed retained backing, like the stack the returnable form stands in for.
pub(crate) fn zero_block(context: &Context, bytes: usize) -> NonNull<u8> {
    // A zero charge admits nothing, so an injected refusal of real
    // charges never reaches it; the block pins its payer until freed.
    let payer = context.payer();
    payer.pin_zero();
    // Like the stack it stands in for, the block is not retained backing;
    // its free, of a block the observer never took, is no event.
    #[cfg(any(test, feature = "charge-observation"))]
    let stack_like = crate::peak::StackLike::hold();
    let base = crate::block::allocate_admitted(context, payer, bytes, 8, crate::block::PLAIN);
    #[cfg(any(test, feature = "charge-observation"))]
    drop(stack_like);
    base
}
/// A returnable row's result: the delivered `value` with the caller's child
/// word 0, or `parked` with the word 1 when the wait gave the stack back.
/// # Safety
/// `word` is the caller's child word.
#[inline(always)]
pub(crate) unsafe fn returned<T>(word: *mut u64, value: Option<T>, parked: T) -> T {
    // SAFETY: by the contract.
    unsafe { word.write(u64::from(value.is_none())) };
    value.unwrap_or(parked)
}

unsafe extern "C" fn start(first: *mut u8, _: *mut u8) -> ! {
    let task = first.cast::<Task>();
    // SAFETY: start owns this fiber. The body cleans its input/temporaries;
    // its result is initialized only on status zero.
    unsafe {
        // FAIL-17: the limit is committed before the body first runs on this stack.
        crate::stack::commit(
            &mut *(*task).context,
            (*task).stack.as_ref().unwrap().bounds(),
        );
        loop {
            let input = match &mut (*task).moved {
                Moved::Spawned => (*task).input,
                Moved::Block { block, .. } => block.base.as_ptr(),
                Moved::Consumed { code } => (&raw mut *code).cast(),
            };
            (*task).failed = ((*task).body)((*task).context, (*task).result, input);
            if (*task).stackless {
                // The body returned through its represented frames from a park
                // (`park_returnable`); the dispatcher takes this stack back.
                // A root whose job is dead after a park gives its moved input
                // back now, keeping only the code word its resume glue reads.
                if let Moved::Block {
                    resume: Some(resume),
                    ..
                } = (*task).moved
                {
                    let Moved::Block { block, .. } =
                        std::mem::replace(&mut (*task).moved, Moved::Spawned)
                    else {
                        std::hint::unreachable_unchecked()
                    };
                    let code = block.base.as_ptr().cast::<u64>().read();
                    block.release(&*(*task).context);
                    (*task).moved = Moved::Consumed { code };
                    (*task).body = resume;
                }
                switch_to_idle(task);
                std::hint::unreachable_unchecked();
            }
            let Some((moved, body, resume)) = (*task).transfer.take() else {
                break;
            };
            // The body returned through its frames from a transfer site
            // (`hirsel_core_transfer`): nothing reads its record or its input
            // now, and the new body makes its own record at its first call.
            let context = &*(*task).context;
            if let Some(record) = (*task).frame.take() {
                record.release(context);
            }
            // The spawn input, its job moved out, is kept to the task's end,
            // so its observed backing is freed when it always was.
            let fresh = Moved::Block {
                block: moved,
                resume,
            };
            if let Moved::Block { block, .. } = std::mem::replace(&mut (*task).moved, fresh) {
                block.release(context);
            }
            (*task).body = body;
        }
        if let Some(record) = (*task).frame.take() {
            record.release(&*(*task).context);
        }
        if let Moved::Block { block, .. } = std::mem::replace(&mut (*task).moved, Moved::Spawned) {
            block.release(&*(*task).context);
        }
        crate::foreign::contained::end_task((*task).context);
        crate::steps::end(&mut *(*task).context);
        if (*task).record.is_some() {
            abi::task_handover((*task).context);
        }
        before_commit();
        record_event(task, END, (*task).id, (*task).failed as u64);
        #[cfg(feature = "prof")]
        prof_state(task, State::Ended, Reason::None, 0);
        (*task).state = State::Ended;
        #[cfg(feature = "record")]
        if let Some(group) = (*task).group
            && let Some(members) = (*(*task).run).tapes.membership(group)
        {
            *members -= 1;
        }
        Waiters::wake_all(
            &raw mut (*task).waiters,
            #[cfg(feature = "prof")]
            (WakeSource::Task, task, (*task).id),
        );
        switch_to_idle(task);
        std::hint::unreachable_unchecked();
    }
}

/// The tape group a helper's root task records in, when its spawner records.
#[cfg(feature = "record")]
pub(crate) type UnitGroup = Option<Group>;
#[cfg(not(feature = "record"))]
pub(crate) type UnitGroup = ();

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn run_root_with_unit_contained(
    context: *mut Context,
    body: Body,
    argument: *mut u8,
    destination: *mut u8,
    input_bytes: usize,
    result_bytes: usize,
    provider: SchedulerProvider,
    unit: Option<&mut crate::units_lang::LangUnit>,
    group: UnitGroup,
) -> Result<u8, crate::run_stop::EndReason> {
    // SAFETY: forwarded caller contract.
    let ended = unsafe {
        run_root_scheduler(
            context,
            body,
            argument,
            destination,
            input_bytes,
            result_bytes,
            provider,
            unit,
            group,
        )
    };
    // FAIL-17: the root task's stack is gone; code the run's teardown runs on
    // this context, on another stack, is refused nothing.
    // SAFETY: the caller pins the context through the run.
    unsafe {
        (*context).stack_limit = 0;
        (*context).stack_floor = 0;
    }
    ended
}

/// The root scheduler loop of [`run_root_with_unit_contained`].
/// # Safety
/// As for [`run_root`].
#[allow(clippy::too_many_arguments)]
unsafe fn run_root_scheduler(
    context: *mut Context,
    body: Body,
    argument: *mut u8,
    destination: *mut u8,
    input_bytes: usize,
    result_bytes: usize,
    provider: SchedulerProvider,
    unit: Option<&mut crate::units_lang::LangUnit>,
    group: UnitGroup,
) -> Result<u8, crate::run_stop::EndReason> {
    use crate::run_stop::EndReason;
    #[cfg(feature = "prof")]
    let _worker_profile = prof::Worker::enter();
    crate::fiber::arm_embedded().map_err(|e| EndReason::RunStop(e.into()))?;
    let _alt = crate::fiber::Alternate::install_if_needed().map_err(EndReason::RunStop)?;
    let view = unit.as_ref().and_then(|u| u.run_view());
    let root_member = view.is_none().then(|| Member::of(RunView::new()));
    let mut run = Scheduler {
        #[cfg(feature = "prof")]
        prof_id: prof::run_id(),
        share_heap: true,
        deferred: ptr::null_mut(),
        deferred_tail: ptr::null_mut(),
        stacks: Stock::default(),
        spare_task: None,
        idle: ptr::null_mut(),
        head: ptr::null_mut(),
        tail: ptr::null_mut(),
        timers: TimerHeap::default(),
        all: ptr::null_mut(),
        next: 1,
        warm: ptr::null_mut(),
        provider,
        #[cfg(any(test, feature = "testing"))]
        script: crate::schedule_script::installed(),
        #[cfg(all(feature = "record", any(test, feature = "testing")))]
        script_groups: Vec::new(),
        readies: Readies::in_run(
            view.or_else(|| root_member.as_ref().map(Member::view))
                .unwrap(),
        ),
        ready_tokens: Vec::new(),
        ready_arms: Vec::new(),
        cancels: Vec::new(),
        #[cfg(feature = "record")]
        outside_cancels: Vec::new(),
        #[cfg(feature = "record")]
        tapes: Tapes::default(),
        net: ptr::null_mut(),
        probes_left: LIVE_PROBE_INTERVAL,
        parked: WaitArms::default(),
        lang: ptr::null_mut(),
        lang_owned: false,
    };
    if let Some(unit) = unit {
        unit.attach(&run.readies);
        run.lang = unit;
    }
    // A helper's entry can carry sockets already registered for this unit.
    // SAFETY: the caller pins the context and its registry through the run.
    if let Some(files) = unsafe { (*context).host_files.as_mut() }
        && let Some(poller) = files.sockets.active_poller()
    {
        run.net = &raw mut files.sockets;
        *run.readies.net.lock().unwrap() = Some(poller);
    }
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
    let input_storage = Storage::try_new(input_bytes, 16).map_err(EndReason::RunStop)?;
    let result_storage = Storage::try_new(result_bytes, 16).map_err(EndReason::RunStop)?;
    let root_stack = match crate::units::take_root_stack() {
        Some(stack) => stack,
        None => run.stacks.try_take().map_err(EndReason::RunStop)?,
    };
    let mut root = Box::new(Task {
        arc_runtime: false,
        arc_hooks: true,
        #[cfg(feature = "record")]
        replay: false,
        #[cfg(feature = "record")]
        latest_pair: false,
        #[cfg(feature = "record")]
        owns_turn: false,
        polls_left: POLLS_PER_TURN,
        context,
        owned_context: None,
        run: &mut run,
        ready: ptr::null_mut(),
        waiters: Waiters::default(),
        waiting_on: ptr::null_mut(),
        wait_previous: ptr::null_mut(),
        wait_next: ptr::null_mut(),
        timer: Default::default(),
        operation: ptr::null_mut(),
        all: ptr::null_mut(),
        all_previous: ptr::null_mut(),
        warm_next: ptr::null_mut(),
        warm_previous: ptr::null_mut(),
        warm: false,
        #[cfg(feature = "record")]
        channel_tape: ChannelTape::None,
        state: State::Ready,
        id: 0,
        #[cfg(feature = "record")]
        group: None,
        stack: Some(root_stack),
        sp: ptr::null_mut(),
        body,
        input: input_storage.as_ptr(),
        result: result_storage.as_ptr(),
        storage_layout: Layout::new::<Task>(),
        layout: TaskLayout {
            input: descriptor,
            result: ValueLayout {
                bytes: result_bytes,
                ..descriptor
            },
        },
        failed: 0,
        record: None,
        frame: None,
        stackless: false,
        moved: Moved::Spawned,
        transfer: None,
        holder: std::mem::MaybeUninit::uninit(),
        cursor: 0,
    });
    // A recorded helper's root task runs in its own tape group.
    #[cfg(feature = "record")]
    group.inspect(|&g| tape_tasks::join(&mut run.tapes, &mut root, g));
    #[cfg(not(feature = "record"))]
    let () = group;
    crate::backing_observation::observe!(
        allocate,
        (&*root as *const Task).cast(),
        size_of::<Task>(),
        TaskStorage,
    );
    // SAFETY: root's stable allocation and both stacks stay live through the loop.
    unsafe {
        ptr::copy_nonoverlapping(argument, root.input, input_bytes);
        let root_pointer: *mut Task = &mut *root;
        if !run.lang.is_null() {
            (*run.lang).root = root_pointer;
        }
        (*context).events = root_pointer.expose_provenance();
        root.sp = root
            .stack
            .as_ref()
            .unwrap()
            .prepare(start, root_pointer.cast(), ptr::null_mut());
        #[cfg(feature = "prof")]
        prof_created(root_pointer, ptr::null_mut());
        run.all = root_pointer;
        root.arc_hooks = run.arc_hooks(&root);
        run.ready(root_pointer);
        RunPtr(&mut run).dispatch_serial(root_pointer);
        if crate::run_stop::is_ended() {
            let end = stop::teardown_ended(&mut run, root_pointer);
            (*context).events = 0;
            if run.lang_owned {
                crate::units_lang::LangUnit::end_root(run.lang);
            }
            drop(registered);
            crate::backing_observation::observe!(free, root_pointer.cast(), size_of::<Task>());
            drop(root);
            drop(input_storage);
            drop(result_storage);
            return Err(end);
        }
        if root.failed == 0 {
            ptr::copy_nonoverlapping(root.result, destination, result_bytes);
        }
        (*context).events = 0;
        if run.lang_owned {
            crate::units_lang::LangUnit::end_root(run.lang);
        }
        drop(registered);
        let failed = root.failed;
        crate::backing_observation::observe!(free, root_pointer.cast(), size_of::<Task>());
        drop(root);
        drop(input_storage);
        drop(result_storage);
        Ok(failed)
    }
}

#[derive(Clone, Copy)]
struct RunPtr(*mut Scheduler);

impl RunPtr {
    /// The dispatcher exclusively owns the run.
    unsafe fn dispatch_serial(self, root: *mut Task) {
        // SAFETY: only this thread reads or writes task and scheduler state.
        unsafe {
            let run = self.0;
            let idle = &raw mut (*run).idle;
            let previous = CURRENT.get();
            let mut direct = ptr::null_mut::<Task>();
            loop {
                if (*root).state == State::Ended || crate::run_stop::is_ended() {
                    break;
                }
                #[cfg(feature = "record")]
                let popped = direct.is_null();
                let task = if direct.is_null() {
                    // An idle native run can collect readiness in its wait.
                    // Ready tokens are still applied before any expired timer.
                    let run = &mut *run;
                    #[cfg(feature = "record")]
                    let no_live_tapes = run.tapes.live.is_empty();
                    #[cfg(not(feature = "record"))]
                    let no_live_tapes = true;
                    let wait_for_net = !run.net.is_null()
                        && run.head.is_null()
                        && run.deferred.is_null()
                        && no_live_tapes
                        && run.cancels.is_empty()
                        && !run.readies.has_pending()
                        && (run.has_outside_wait() || run.lang_waits());
                    let untied = crate::units::untied();
                    if wait_for_net {
                        trim_run_heap(root);
                    }
                    let (timeout, release) = if wait_for_net && untied {
                        (poll_timeout(run.timers.deadline()), None)
                    } else if wait_for_net {
                        run.net_idle_wait(poll_timeout(run.timers.deadline()))
                    } else {
                        (0, None)
                    };
                    // mc-units-proto: HIRSEL_XSPIN busy-polls instead of sleeping.
                    let timeout = if crate::units::spin() { 0 } else { timeout };
                    if untied && timeout != 0 {
                        // mc-units-untied: the unit parks; any worker resumes it.
                        crate::units::park(run.timers.deadline());
                        run.take_wakes(0);
                    } else {
                        if untied {
                            crate::units::maybe_yield();
                        }
                        run.take_wakes(timeout);
                    }
                    let task = run.pop();
                    if task.is_null() {
                        if release.is_some_and(|due| crate::os::monotonic_ns() >= due) {
                            run.release_net_idle();
                        }
                        if !wait_for_net {
                            if run.share_heap {
                                trim_run_heap(root);
                            }
                            run.idle();
                        }
                        continue;
                    }
                    task
                } else {
                    std::mem::replace(&mut direct, ptr::null_mut())
                };
                if (*task).stack.is_none() {
                    match (*self.0).stacks.try_take() {
                        Ok(stack) => mount(task, stack),
                        Err(stop) => {
                            crate::run_stop::record(stop);
                            break;
                        }
                    }
                }
                #[cfg(feature = "record")]
                if (*task).channel_tape == ChannelTape::Expired
                    && let Some(group) = (*task).group
                {
                    group.wake_deadline();
                }
                #[cfg(feature = "record")]
                if popped {
                    (*run).record_turn(task);
                }
                CURRENT.set(task);
                (*task).state = State::Running;
                (*run).mark_warm(task);
                #[cfg(feature = "prof")]
                prof_dispatch(task, true);
                abi::task_resume((*task).context);
                let prev_idle = crate::run_stop::set_idle_ptr(idle);
                crate::fiber::guard((*task).stack.as_ref().unwrap().bounds());
                hirsel_stack::switch(idle, (*task).sp);
                #[cfg(feature = "prof")]
                prof_dispatch(task, false);
                crate::fiber::guard((0, 0, 0));
                crate::run_stop::set_idle_ptr(prev_idle);
                crate::limit::Funding::unbind();
                CURRENT.set(previous);
                if crate::run_stop::is_ended() {
                    break;
                }
                match (*task).state {
                    State::Ended => (*self.0)
                        .stacks
                        .recycle_ended(task == root, (*task).stack.take().unwrap()),
                    State::Provider => {
                        if (*task).stackless {
                            (*self.0).stacks.recycle(unmount(task));
                        }
                        if (*run).wake_operation(task, Wake::Parked) {
                            direct = task;
                        }
                    }
                    _ if (*task).stackless => {
                        (*self.0).stacks.recycle(unmount(task));
                    }
                    _ => {}
                }
            }
        }
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
    stack: &mut Option<Stack>,
    context: *mut Context,
    bytes: u64,
    input: *mut u8,
    body: Body,
    layout: TaskLayout,
    #[cfg(feature = "record")] group: Option<crate::tape::schedule::Group>,
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
        let child_context = abi::task_child(context, &mut record, bytes, id, (*run).share_heap);
        let (storage, input_slot, result_slot) = Storage::for_task(&mut (*run).spare_task, layout);
        let task = storage.as_ptr().cast::<Task>();
        task.write(Task {
            arc_runtime: false,
            arc_hooks: true,
            #[cfg(feature = "record")]
            replay: false,
            #[cfg(feature = "record")]
            latest_pair: false,
            #[cfg(feature = "record")]
            owns_turn: false,
            polls_left: POLLS_PER_TURN,
            context: ptr::null_mut(),
            owned_context: Some(child_context),
            run,
            ready: ptr::null_mut(),
            waiters: Waiters::default(),
            waiting_on: ptr::null_mut(),
            wait_previous: ptr::null_mut(),
            wait_next: ptr::null_mut(),
            timer: Default::default(),
            operation: ptr::null_mut(),
            all: (*run).all,
            all_previous: ptr::null_mut(),
            warm_next: ptr::null_mut(),
            warm_previous: ptr::null_mut(),
            warm: false,
            #[cfg(feature = "record")]
            channel_tape: ChannelTape::None,
            state: State::Ready,
            id,
            #[cfg(feature = "record")]
            group: group.map(|group| group.child()),
            stack: stack.take(),
            sp: ptr::null_mut(),
            body,
            input: input_slot,
            result: result_slot,
            storage_layout: storage.layout,
            layout,
            failed: 0,
            record: Some(record),
            frame: None,
            stackless: false,
            moved: Moved::Spawned,
            transfer: None,
            holder: std::mem::MaybeUninit::uninit(),
            cursor: 0,
        });
        // The scheduler's published task now owns the allocation until retire.
        std::mem::forget(storage);
        #[cfg(feature = "record")]
        if let Some(group) = (*task).group {
            (*task).replay = group.replaying();
            (*task).arc_runtime = (*task).replay;
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
        ptr::copy_nonoverlapping(input, input_slot, layout.input.bytes);
        (*task).sp = (*task)
            .stack
            .as_ref()
            .unwrap()
            .prepare(start, task.cast(), ptr::null_mut());
        if !(*task).all.is_null() {
            (*(*task).all).all_previous = task;
        }
        (*run).all = task;
        #[cfg(feature = "prof")]
        prof_created(task, CURRENT.get());
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
        // A refused spawn returns its fresh stack to the run's stock.
        let stock = &raw mut (*(*CURRENT.get()).run).stacks;
        let mut stack = Some((*stock).take());
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
    _tasks: u64,
    bytes: u64,
    input: *mut u8,
    body: Body,
    layout: TaskLayout,
    _site: u64,
    stack: &mut Option<Stack>,
) -> Spawn {
    // SAFETY: the caller passes its generated operands through unchanged.
    unsafe {
        #[cfg(feature = "record")]
        let spawned = spawn_in(stack, context, bytes, input, body, layout, current_group());
        #[cfg(not(feature = "record"))]
        let spawned = spawn_in(stack, context, bytes, input, body, layout);
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
    if crate::units_lang::is_helper(task) {
        // SAFETY: the owner's handle pins the helper.
        return unsafe { crate::units_lang::cancel(task) };
    }
    // SAFETY: parent owns the child's handle and requests without touching its stack.
    unsafe {
        before_commit();
        #[cfg(feature = "record")]
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
    // SAFETY: task ended, is not queued, and its sole handle is being consumed.
    unsafe {
        let run = (*task).run;
        #[cfg(feature = "record")]
        (*run).outside_cancels.retain(|&t| t != task);
        (*run).unmark_warm(task);
        let (previous, next) = ((*task).all_previous, (*task).all);
        if previous.is_null() {
            debug_assert!((*run).all == task);
            (*run).all = next;
        } else {
            (*previous).all = next;
        }
        if !next.is_null() {
            (*next).all_previous = previous;
        }
        let backing = Storage {
            pointer: NonNull::new_unchecked(task.cast()),
            layout: (*task).storage_layout,
        };
        // Ended context returns unused primary before record/result refunds.
        drop((*task).owned_context.take());
        drop((*task).record.take());
        ptr::drop_in_place(task);
        if backing.layout.size() <= MAX_SPARE_TASK_BYTES {
            (*run).spare_task = Some(backing);
        }
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
    // SAFETY: by the contract; the stackful form never parks returning.
    unsafe { join::<false>(context, task, result, site, false) }.unwrap_or_else(|| unreachable!())
}
/// The returnable form of `join` (`RtOp::Returnable`): `word` is the
/// caller's child word, 1 while the join waits with the stack given back.
/// # Safety
/// As `hirsel_core_join`, in a task that runs a returnable root; a resume
/// passes the same handle and destination.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_join_rec(
    context: *mut Context,
    word: *mut u64,
    task: *mut Task,
    result: *mut u8,
    site: u64,
) -> Join {
    let parked = Join {
        kind: 0,
        causes: ptr::null_mut(),
        status: 0,
    };
    // SAFETY: by the contract.
    unsafe {
        let joined = join::<true>(context, task, result, site, *word != 0);
        returned(word, joined, parked)
    }
}
/// `REC` parks with the stack given back (None). A task join's resume runs
/// its loop again from the top, as the stackful form's next iteration does.
#[inline(always)]
unsafe fn join<const REC: bool>(
    context: *mut Context,
    task: *mut Task,
    result: *mut u8,
    site: u64,
    resumed: bool,
) -> Option<Join> {
    if crate::units_lang::is_helper(task) {
        // SAFETY: as below, for a helper's handle.
        return unsafe { crate::units_lang::join::<REC>(context, task, result, site, resumed) };
    }
    // SAFETY: parent owns task throughout waits, no references survive switches.
    unsafe {
        loop {
            before_commit();
            if check_cancel(context, site) != 0 {
                return Some(Join {
                    kind: 0,
                    causes: ptr::null_mut(),
                    status: 1,
                });
            }
            if (*task).state == State::Ended {
                break;
            }
            if REC {
                park_returning(
                    CURRENT.get(),
                    &raw mut (*task).waiters,
                    #[cfg(feature = "prof")]
                    (Reason::Join, (*task).id),
                );
                return None;
            }
            park(
                &raw mut (*task).waiters,
                #[cfg(feature = "prof")]
                (Reason::Join, (*task).id),
            );
        }
        let failed = (*task).failed != 0;
        let causes = std::mem::take(&mut (*(*task).context).causes);
        let cancelled = causes
            .iter()
            .next()
            .is_some_and(|cause| cause.kind() == causes::CANCELLED);
        if !failed {
            ptr::copy_nonoverlapping((*task).result, result, (*task).layout.result.bytes);
        }
        event(JOIN, (*task).id, u64::from(failed));
        retire(task);
        Some(Join {
            kind: if cancelled { 2 } else { u8::from(failed) },
            causes: causes.into_raw(),
            status: 0,
        })
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
            park(
                &raw mut (*task).waiters,
                #[cfg(feature = "prof")]
                (Reason::Join, (*task).id),
            );
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
            && !clean(context, (*task).result, &mut (*task).cursor, list)
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
    if crate::units_lang::is_helper(task) {
        // SAFETY: the caller transfers the helper's handle.
        return unsafe { crate::units_lang::abandon(context, task) };
    }
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
    if crate::units_lang::is_helper(task) {
        // SAFETY: as abandon; a helper's result owns no cleanup to stage.
        unsafe { crate::units_lang::abandon(context, task) };
        return true;
    }
    // SAFETY: as abandon.
    unsafe {
        let holder = abandon_begin(context, task);
        (*list).stage_holder(holder, abandon_step, point);
        true
    }
}

#[cfg(test)]
mod ready_routing_laws;
