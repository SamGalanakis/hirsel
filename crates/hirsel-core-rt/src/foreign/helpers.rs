//! One run owns its helpers. Scheduler callbacks reserve a lane in FIFO order;
//! the helper itself checks cancellation at issue and delivers before freeing it.
use super::{Invocation, handles::RELEASE_WAIT_OP};
use crate::{
    Context,
    provider::{self, Outcome, Provider, Request, Step, Wait},
    tasks::{self, Outside},
};
use std::{
    io, ptr,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

const WIDTH: usize = 4;
const STACK: usize = 8 << 20;
/// The helper protocol uses the scheduler's parking adapter privately.
pub(crate) const WAIT_OP: u8 = u8::MAX;

pub(crate) struct Helpers {
    shared: Arc<Shared>,
    workers: [Option<JoinHandle<()>>; WIDTH],
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    work: Condvar,
}

#[derive(Default)]
struct State {
    slots: [Slot; WIDTH],
    first: Option<Link>,
    last: Option<Link>,
    closing: bool,
}

#[derive(Clone, Copy, Default)]
enum Slot {
    #[default]
    Free,
    Reserved,
    Submitted(Link),
    Running,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Link(*mut Call);
// SAFETY: the scheduler keeps the task's call frame pinned through delivery
// and the run mutex serializes access to the queue links and result. The one
// helper that takes the slot alone owns access to the C arguments and loans
// until delivery. Neither the scheduler nor the task reads them in that time.
unsafe impl Send for Link {}

struct Call {
    shared: *const Shared,
    invocation: Invocation,
    cancel: *const AtomicBool,
    #[cfg(feature = "prof")]
    profile: ([u64; 2], u64),
    next: Option<Link>,
    token: u64,
    outside: Option<Outside>,
    slot: Option<usize>,
    outcome: Option<Outcome>,
}

impl Helpers {
    pub(crate) fn start() -> io::Result<Self> {
        let mut helpers = Self {
            shared: Arc::new(Shared::default()),
            workers: [const { None }; WIDTH],
        };
        for worker in &mut helpers.workers {
            let shared = helpers.shared.clone();
            *worker = Some(
                std::thread::Builder::new()
                    .name("hirsel-foreign".into())
                    .stack_size(STACK)
                    .spawn(move || shared.run())?,
            );
        }
        Ok(helpers)
    }

    /// # Safety
    /// The calling task and its arguments are live until delivery. The thunk
    /// follows the foreign native contract and no alias writes the loans.
    pub(crate) unsafe fn call(
        &self,
        context: *mut Context,
        invocation: Invocation,
        cancellable: bool,
        site: u64,
    ) -> Outcome {
        // SAFETY: cancel flag belongs to the current task. It stays pinned
        // across the park and is read only atomically from the helper.
        tasks::before_provider_entry();
        let cancel = if cancellable {
            unsafe { tasks::foreign_cancel_flag(context) }
        } else {
            ptr::null()
        };
        let mut call = Call {
            shared: &*self.shared,
            invocation,
            cancel,
            #[cfg(feature = "prof")]
            profile: (tasks::prof_current_task(), site),
            next: None,
            token: 0,
            outside: None,
            slot: None,
            outcome: None,
        };
        let provider = Provider {
            state: (&raw mut call).cast(),
            wait,
            issue,
            resume,
            abort,
            deliver: provider::DELIVER,
        };
        let mut request = request(
            if cancellable {
                WAIT_OP
            } else {
                RELEASE_WAIT_OP
            },
            site,
        );
        // SAFETY: the callbacks unlink the frame before returning delivery.
        // If the helper observed a cancellation during issue, the caller
        // records it here. The scheduler has already recorded a W cancellation.
        let mut issued = None;
        let outcome = unsafe {
            tasks::provider_operation(context, &provider, &mut request, Some(&mut issued))
        };
        if outcome.status == provider::CANCELLED.status && issued.is_some() {
            // SAFETY: the task owns its cause cell again after delivery.
            unsafe { tasks::check_cancel(context, site) };
        }
        outcome
    }
}

impl Drop for Helpers {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().closing = true;
        self.shared.work.notify_all();
        for worker in self.workers.iter_mut().filter_map(Option::take) {
            worker.join().unwrap();
        }
    }
}

impl State {
    fn free_slot(&self) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| matches!(slot, Slot::Free))
    }

    /// # Safety
    /// The run lock pins every linked frame through removal.
    unsafe fn push(&mut self, link: Link) {
        if let Some(last) = self.last {
            // SAFETY: a linked frame remains live and only this lock writes it.
            unsafe { (*last.0).next = Some(link) };
        } else {
            self.first = Some(link);
        }
        self.last = Some(link);
    }

    /// # Safety
    /// The run lock pins every linked frame through removal.
    unsafe fn unlink(&mut self, target: Link) {
        let mut previous: Option<Link> = None;
        let mut link = self.first;
        while let Some(node) = link {
            // SAFETY: the run lock owns the linked frame's next field.
            link = unsafe { (*node.0).next };
            if node == target {
                if let Some(previous) = previous {
                    // SAFETY: both frame links are live under the lock.
                    unsafe { (*previous.0).next = link };
                } else {
                    self.first = link;
                }
                if self.last == Some(target) {
                    self.last = previous;
                }
                // SAFETY: the removal owns the target's next field.
                unsafe { (*target.0).next = None };
                return;
            }
            previous = Some(node);
        }
    }

    /// # Safety
    /// The run lock pins every queued frame.
    unsafe fn wake_first(&self) {
        if let Some(link) = self.first
            && self.free_slot().is_some()
        {
            // SAFETY: a queued frame has a wake token and cannot retire under the lock.
            unsafe { (*link.0).outside.as_ref().unwrap().ready((*link.0).token) };
        }
    }
}

impl Shared {
    fn run(&self) {
        #[cfg(feature = "prof")]
        let _worker_profile = crate::prof::Worker::enter();
        loop {
            let (index, link, invocation, cancelled) = {
                let mut state = self.state.lock().unwrap();
                loop {
                    if state.closing {
                        return;
                    }
                    if let Some((index, Slot::Submitted(link))) = state
                        .slots
                        .iter()
                        .copied()
                        .enumerate()
                        .find(|(_, slot)| matches!(slot, Slot::Submitted(_)))
                    {
                        state.slots[index] = Slot::Running;
                        // SAFETY: this worker owns the submitted slot until
                        // delivery. The task pins the optional atomic flag.
                        let call = unsafe { &*link.0 };
                        let cancelled = unsafe { call.cancel.as_ref() }
                            .is_some_and(|flag| flag.load(Ordering::Acquire));
                        break (index, link, call.invocation, cancelled);
                    }
                    state = self.work.wait(state).unwrap();
                }
            };
            // SAFETY: this worker owns the transition out of the submitted
            // slot, and the task pins its C operands through completion.
            let outcome = unsafe {
                if cancelled {
                    invocation.rollback();
                    provider::CANCELLED
                } else {
                    #[cfg(feature = "prof")]
                    let _interval = {
                        // The running slot keeps the caller's profile data pinned.
                        let (task, site) = (*link.0).profile;
                        crate::prof::HostInterval::begin(task, site)
                    };
                    invocation.issue()
                }
            };
            let mut state = self.state.lock().unwrap();
            state.slots[index] = Slot::Free;
            // SAFETY: the active slot pins the task's frame through delivery.
            // Its result lock makes the C writes visible to the resumed task.
            unsafe {
                (*link.0).outcome = Some(outcome);
                let outside = (*link.0).outside.take().unwrap();
                outside.ready((*link.0).token);
                state.wake_first();
            }
        }
    }
}

unsafe extern "C" fn wait(raw: *mut u8, _: *mut Request, token: u64) -> Wait {
    let link = Link(raw.cast());
    // SAFETY: provider callback runs while the caller pins its frame. The
    // mutex owns its queue links and no helper can access it before issue.
    unsafe {
        let shared = &*(*link.0).shared;
        let mut state = shared.state.lock().unwrap();
        if (*link.0).outside.is_none() {
            (*link.0).token = token;
            (*link.0).outside = Some(Outside::begin());
            state.push(link);
        }
        if state.first == Some(link)
            && let Some(index) = state.free_slot()
        {
            state.unlink(link);
            state.slots[index] = Slot::Reserved;
            (*link.0).slot = Some(index);
            state.wake_first();
            return Wait::default();
        }
        Wait {
            kind: provider::WAIT_PARK,
            ..Wait::default()
        }
    }
}

unsafe extern "C" fn issue(raw: *mut u8, _: *mut Request, _: u64) -> Step {
    let link = Link(raw.cast());
    // SAFETY: the task reserved this slot in wait and pins the frame.
    unsafe {
        let shared = &*(*link.0).shared;
        let mut state = shared.state.lock().unwrap();
        state.slots[(*link.0).slot.unwrap()] = Slot::Submitted(link);
        shared.work.notify_one();
    }
    Step {
        kind: provider::STEP_PENDING,
        ..Step::default()
    }
}

unsafe extern "C" fn resume(raw: *mut u8, _: *mut Request, _: u64) -> Step {
    let call = raw.cast::<Call>();
    // SAFETY: the parked task still owns the frame; reads share the result lock.
    unsafe {
        let _state = (*(*call).shared).state.lock().unwrap();
        match (*call).outcome {
            Some(outcome) => Step {
                outcome,
                ..Step::default()
            },
            None => Step {
                kind: provider::STEP_PENDING,
                ..Step::default()
            },
        }
    }
}

unsafe extern "C" fn abort(raw: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    let link = Link(raw.cast());
    // SAFETY: only W cancellation can enter this callback. No helper owns
    // the frame. A wait that just returned Ready can have reserved a slot.
    unsafe {
        let mut state = (*(*link.0).shared).state.lock().unwrap();
        if let Some(slot) = (*link.0).slot.take() {
            state.slots[slot] = Slot::Free;
        } else {
            state.unlink(link);
        }
        (*link.0).invocation.rollback();
        (*link.0).outside.take();
        state.wake_first();
    }
    provider::CANCELLED
}

pub(super) fn request(operation: u8, site: u64) -> Request {
    Request {
        operation,
        mode: 0,
        handle: 0,
        path: ptr::null(),
        path_len: 0,
        buffer: provider::Buffer {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
        },
        site,
    }
}
