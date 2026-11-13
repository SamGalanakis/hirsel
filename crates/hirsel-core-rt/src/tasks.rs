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
pub const SENDERS: u8 = 8;
pub const CLOSE: u8 = 9;
pub const ARC_NEW: u8 = crate::abi_layout::ARC_EVENT_NEW;
pub const ARC_SHARE: u8 = crate::abi_layout::ARC_EVENT_SHARE;
pub const ARC_RELEASE: u8 = crate::abi_layout::ARC_EVENT_RELEASE;
pub const ARC_UNWRAP: u8 = crate::abi_layout::ARC_EVENT_UNWRAP;
pub const REFUND: u8 = 14;
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
    Ended,
}
pub struct Task {
    context: *mut Context,
    owned_context: Option<Context>,
    run: *mut Scheduler,
    ready: *mut Task,
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
    host: Stack,
    idle: *mut u8,
    head: *mut Task,
    tail: *mut Task,
    all: *mut Task,
    next: u64,
    provider: SchedulerProvider,
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
    unsafe fn wake(&mut self) {
        let mut task = self.all;
        // SAFETY: no task is executing while these single-worker links change.
        unsafe {
            while !task.is_null() {
                if (*task).state == State::Blocked {
                    self.ready(task);
                }
                task = (*task).all;
            }
        }
    }
}

/// Record only semantic identifiers, never native addresses. Arc IDs are assigned
/// by their event stream; zero denotes the operation's implicit current object.
pub(crate) fn event(kind: u8, object: u64, value: u64) {
    let task = CURRENT.get();
    if task.is_null() {
        return;
    }
    // SAFETY: current pins its scheduler; callback cannot suspend or re-enter.
    unsafe {
        let run = &*(*task).run;
        if let Some(event) = run.provider.event {
            event(
                run.provider.state,
                Event {
                    kind,
                    task: (*task).id,
                    object,
                    value,
                },
            );
        }
    }
}
/// Complete a recorded inline operation without consulting task TLS.
/// # Safety
/// Context's immutable event word is nonzero and pins its current Task/run.
pub(crate) unsafe fn event_in(context: *const Context, kind: u8, object: u64, value: u64) {
    // SAFETY: the emitted gate proves the run has this immutable event sink.
    unsafe {
        let task = (*context).events as *const Task;
        let run = &*(*task).run;
        let sink = run.provider.event.unwrap_unchecked();
        sink(
            run.provider.state,
            Event {
                kind,
                task: (*task).id,
                object,
                value,
            },
        );
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
        } else {
            (*run).ready(task);
        }
        hirsel_stack::switch(&raw mut (*task).sp, (*run).idle);
    }
}
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
pub unsafe fn check_cancel(context: *mut Context, site: u64) -> u8 {
    // SAFETY: the operation entry exclusively owns this task context.
    unsafe {
        if (*context).finalizing == 0
            && (*context).cancel_shield == 0
            && (*context).cancel_requested
        {
            (*context).cancel_requested = false;
            causes::record(
                &mut *context,
                Cause {
                    kind: causes::CANCELLED,
                    site,
                    message: ptr::null_mut(),
                    drop_message: None,
                },
            );
            event(CANCELLED, 0, site);
            1
        } else {
            0
        }
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
        if (*context).finalizing == 0 && (*context).cancel_shield == 0 {
            checkpoint(false);
        }
        check_cancel(context, site)
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
        host: Stack::with_bytes(8 << 20),
        idle: ptr::null_mut(),
        head: ptr::null_mut(),
        tail: ptr::null_mut(),
        all: ptr::null_mut(),
        next: 1,
        provider,
    };
    let descriptor = ValueLayout {
        bytes: input_bytes,
        align: 16,
        logical: 0,
        drop: no_drop,
    };
    let mut root = Box::new(Task {
        context,
        owned_context: None,
        run: &mut run,
        ready: ptr::null_mut(),
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
    // SAFETY: root's stable allocation and both stacks stay live through the loop.
    unsafe {
        ptr::copy_nonoverlapping(argument, root.input.as_ptr(), input_bytes);
        let root_pointer: *mut Task = &mut *root;
        (*context).events = if run.provider.event.is_some() {
            root_pointer.addr()
        } else {
            0
        };
        root.sp = root
            .stack
            .prepare(start, root_pointer.cast(), ptr::null_mut());
        run.all = root_pointer;
        run.ready(root_pointer);
        let previous = CURRENT.get();
        loop {
            if root.state == State::Ended {
                break;
            }
            let task = run.pop();
            if task.is_null() {
                std::thread::park();
                continue;
            }
            CURRENT.set(task);
            (*task).state = State::Running;
            abi::task_resume((*task).context);
            crate::fiber::guard((*task).stack.bounds());
            hirsel_stack::switch(&mut run.idle, (*task).sp);
            crate::fiber::guard((0, 0));
            CURRENT.set(previous);
        }
        if root.failed == 0 {
            ptr::copy_nonoverlapping(root.result.as_ptr(), destination, result_bytes);
        }
        (*context).events = 0;
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
            context: ptr::null_mut(),
            owned_context: Some(child_context),
            run,
            ready: ptr::null_mut(),
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
        let task = Box::into_raw(task);
        (*task).context = (*task).owned_context.as_mut().unwrap();
        (*(*task).context).events = if (*(*task).context).events != 0 {
            task.addr()
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
        (*(*task).context).cancel_requested = true;
        event(CANCEL, (*task).id, 0);
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
            .is_some_and(|cause| cause.kind == causes::CANCELLED);
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
        let target = (*run)
            .host
            .prepare(invoke, (&mut call as *mut Call).cast(), ptr::null_mut());
        crate::fiber::guard((*run).host.bounds());
        hirsel_stack::switch(&mut call.saved, target);
        crate::fiber::guard((*task).stack.bounds());
        call.result.assume_init()
    }
}
