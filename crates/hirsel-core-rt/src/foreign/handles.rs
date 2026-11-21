//! FFI-6: the pointer index is retired at issue and the ordinal at delivery.
//! Reserved slots account for every make in flight, so delivery allocates nothing.
use super::Thunk;
use crate::{
    Context,
    provider::{self, Outcome, Provider, Request, Step, Wait},
    tasks::{self, Outside},
};
use std::{
    collections::{HashMap, HashSet},
    ptr,
    sync::Mutex,
};

/// A nominal kind owns its release thunk. Each linked image has one
/// descriptor per (interface, kind).
#[repr(C)]
pub struct Kind {
    pub release: Thunk,
    pub waits: bool,
}

/// A thunk takes a pair of words per argument. Scalars use the first,
/// loans use both, and handles start with the ordinal in the first word and
/// are passed to C using the pointer resolved into the second.
pub type Argument = [u64; 2];

pub const RELEASED: u8 = 2;
pub const IN_USE: u8 = 3;
pub const WRONG_KIND: u8 = 4;
pub const ALREADY_HELD: u8 = 5;
pub(crate) const RELEASE_WAIT_OP: u8 = u8::MAX - 1;

#[derive(Default)]
pub(crate) struct Handles(Mutex<Table>);

#[derive(Default)]
struct Table {
    entries: HashMap<u64, Handle>,
    pointers: HashSet<u64>,
    last_ordinal: u64,
    newest: u64,
    reserved: usize,
}

struct Handle {
    pointer: u64,
    kind: &'static Kind,
    in_flight: bool,
    older: u64,
    newer: u64,
    release_waiters: Option<WaitLink>,
}

#[derive(Clone, Copy)]
pub(crate) struct Operands {
    pub table: *const Handles,
    pub arguments: *mut Argument,
    pub kinds: *const *const Kind,
    pub count: usize,
    pub result: Option<&'static Kind>,
    pub release: bool,
}

impl Operands {
    /// # Safety
    /// The call owns its packed argument words across all three phases. The
    /// kinds table is immutable and all nonnull kinds name live descriptors.
    unsafe fn for_handles(self, mut apply: impl FnMut(*mut Argument, &'static Kind)) {
        for i in 0..self.count {
            // SAFETY: the descriptor and words have the same, live extent.
            unsafe {
                if let Some(kind) = (*self.kinds.add(i)).as_ref() {
                    apply(self.arguments.add(i), kind);
                }
            }
        }
    }

    /// # Safety
    /// As for_handles. Caller owns the frame until delivery.
    pub(crate) unsafe fn prepare(self, context: *mut Context, site: u64) -> u8 {
        // SAFETY: the pinned run owns the handle table.
        let table = unsafe { &*self.table };
        {
            let mut state = table.0.lock().unwrap();
            // SAFETY: descriptor and args are the caller's pinned operands.
            match unsafe { state.preflight(self) } {
                Preflight::Ready => return 0,
                Preflight::Refused(status) => return status,
                Preflight::Wait => {}
            }
        }
        // SAFETY: Wait is possible only for a single release operand.
        unsafe { wait_for_use(context, self, site) }
    }

    /// # Safety
    /// Exactly one rollback may run, before issue, after successful preflight.
    pub(crate) unsafe fn rollback(self) {
        // SAFETY: preflight owns these in-flight handles and any reservation.
        unsafe {
            let mut state = (*self.table).0.lock().unwrap();
            self.for_handles(|word, _| {
                state.entries.get_mut(&(*word)[0]).unwrap().make_live();
            });
            if self.result.is_some() {
                state.reserved -= 1;
            }
        }
    }

    /// # Safety
    /// Exactly once after successful preflight, when C starts.
    pub(crate) unsafe fn issue(self) {
        if self.release {
            // SAFETY: release preflight marked the single handle in flight.
            unsafe {
                let mut state = (*self.table).0.lock().unwrap();
                state.pointers.remove(&(*self.arguments)[1]);
            }
        }
    }

    /// # Safety
    /// Exactly once after the thunk has returned, for a prepared issued call.
    pub(crate) unsafe fn deliver(self, bits: u64) -> Outcome {
        // SAFETY: this call owns each in-flight operand and its result reservation.
        unsafe {
            let mut state = (*self.table).0.lock().unwrap();
            if self.release {
                state.remove((*self.arguments)[0]).wake_releases();
            } else {
                self.for_handles(|word, _| {
                    state.entries.get_mut(&(*word)[0]).unwrap().make_live();
                });
            }
            let Some(kind) = self.result else {
                return Outcome {
                    value: bits,
                    ..Outcome::default()
                };
            };
            state.reserved -= 1;
            if bits == 0 {
                return Outcome::default();
            }
            if !state.pointers.insert(bits) {
                return Outcome {
                    status: ALREADY_HELD,
                    ..Outcome::default()
                };
            }
            Outcome {
                value: state.insert(bits, kind),
                ..Outcome::default()
            }
        }
    }
}

enum Preflight {
    Ready,
    Refused(u8),
    Wait,
}

impl Handle {
    fn make_live(&mut self) {
        self.in_flight = false;
        self.wake_releases();
    }

    fn wake_releases(&mut self) {
        let mut next = self.release_waiters.take();
        while let Some(link) = next {
            // SAFETY: each waiter is pinned until it runs its next check, and
            // only the table lock reads and clears this link and wake token.
            unsafe {
                next = (*link.0).next.take();
                (*link.0).armed = false;
                (*link.0).outside.as_ref().unwrap().ready((*link.0).token);
            }
        }
    }
}

impl Table {
    /// # Safety
    /// The queue and words are pinned and this call is entering preflight.
    unsafe fn preflight(&mut self, call: Operands) -> Preflight {
        let mut outcome = Preflight::Ready;
        // SAFETY: preflight examines the words only. No state changes unless
        // the left-to-right check succeeds for every argument.
        unsafe {
            call.for_handles(|word, kind| {
                if !matches!(outcome, Preflight::Ready) {
                    return;
                }
                let ordinal = (*word)[0];
                let Some(handle) = self.entries.get(&ordinal) else {
                    outcome = Preflight::Refused(RELEASED);
                    return;
                };
                let duplicate = (0..word.offset_from(call.arguments) as usize).any(|i| {
                    !(*call.kinds.add(i)).is_null() && (*call.arguments.add(i))[0] == ordinal
                });
                if duplicate || (handle.in_flight && !call.release) {
                    outcome = Preflight::Refused(IN_USE);
                } else if !ptr::eq(handle.kind, kind) {
                    outcome = Preflight::Refused(WRONG_KIND);
                } else if handle.in_flight {
                    outcome = Preflight::Wait;
                }
            });
            if !matches!(outcome, Preflight::Ready) {
                return outcome;
            }
            if call.result.is_some() {
                self.reserve();
            }
            call.for_handles(|word, _| {
                let handle = self.entries.get_mut(&(*word)[0]).unwrap();
                handle.in_flight = true;
                (*word)[1] = handle.pointer;
            });
        }
        Preflight::Ready
    }

    fn reserve(&mut self) {
        let needed = self
            .reserved
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        if self.last_ordinal.checked_add(needed as u64).is_none() {
            crate::process::stop(b"HostHandles");
        }
        if self.entries.try_reserve(needed).is_err() || self.pointers.try_reserve(needed).is_err() {
            crate::process::stop(b"HostMemory");
        }
        self.reserved = needed;
    }

    fn insert(&mut self, pointer: u64, kind: &'static Kind) -> u64 {
        self.last_ordinal += 1;
        let ordinal = self.last_ordinal;
        if let Some(newest) = self.entries.get_mut(&self.newest) {
            newest.newer = ordinal;
        }
        self.entries.insert(
            ordinal,
            Handle {
                pointer,
                kind,
                in_flight: false,
                older: self.newest,
                newer: 0,
                release_waiters: None,
            },
        );
        self.newest = ordinal;
        ordinal
    }

    fn remove(&mut self, ordinal: u64) -> Handle {
        let handle = self.entries.remove(&ordinal).unwrap();
        if let Some(older) = self.entries.get_mut(&handle.older) {
            older.newer = handle.newer;
        }
        if let Some(newer) = self.entries.get_mut(&handle.newer) {
            newer.older = handle.older;
        } else {
            self.newest = handle.older;
        }
        handle
    }
}

impl Handles {
    pub(crate) fn kind(&self, ordinal: u64) -> Option<&'static Kind> {
        self.0
            .lock()
            .unwrap()
            .entries
            .get(&ordinal)
            .map(|handle| handle.kind)
    }

    /// Take the most recently made remaining obligation after the last task.
    pub(crate) fn pop(&self) -> Option<(&'static Kind, Argument)> {
        let mut state = self.0.lock().unwrap();
        let ordinal = state.newest;
        let handle = state.entries.get(&ordinal)?;
        let pointer = handle.pointer;
        let kind = handle.kind;
        state.pointers.remove(&pointer);
        state.remove(ordinal);
        Some((kind, [ordinal, pointer]))
    }
}

struct ReleaseWait {
    operands: Operands,
    outcome: Outcome,
    outside: Option<Outside>,
    token: u64,
    armed: bool,
    next: Option<WaitLink>,
}

#[derive(Clone, Copy)]
struct WaitLink(*mut ReleaseWait);
// SAFETY: the scheduler pins a registered waiter. The table mutex owns every
// link, and wakes relinquish it before the waiting task can return.
unsafe impl Send for WaitLink {}

/// # Safety
/// A release was blocked on an in-flight use. Its single handle argument and
/// the current task are pinned until delivery.
unsafe fn wait_for_use(context: *mut Context, operands: Operands, site: u64) -> u8 {
    unsafe extern "C" fn wait(raw: *mut u8, _: *mut Request, token: u64) -> Wait {
        let link = WaitLink(raw.cast());
        // SAFETY: the caller pins this frame. The table lock owns all changes
        // to its wait link and recognizes a delivered use before reserving it.
        unsafe {
            let mut state = (*(*link.0).operands.table).0.lock().unwrap();
            let frame = &mut *link.0;
            match state.preflight(frame.operands) {
                Preflight::Ready => Wait::default(),
                Preflight::Refused(status) => {
                    frame.outcome.status = status;
                    Wait::default()
                }
                Preflight::Wait => {
                    if !frame.armed {
                        let ordinal = (*frame.operands.arguments)[0];
                        let handle = state.entries.get_mut(&ordinal).unwrap();
                        frame.outside.get_or_insert_with(Outside::begin);
                        frame.token = token;
                        frame.next = handle.release_waiters.replace(link);
                        frame.armed = true;
                    }
                    Wait {
                        kind: provider::WAIT_PARK,
                        ..Wait::default()
                    }
                }
            }
        }
    }

    unsafe extern "C" fn issue(raw: *mut u8, _: *mut Request, _: u64) -> Step {
        // SAFETY: wait detached the frame after marking a result; issue is
        // synchronous and no consumer can still reach it.
        unsafe {
            Step {
                outcome: (*raw.cast::<ReleaseWait>()).outcome,
                ..Step::default()
            }
        }
    }

    unsafe extern "C" fn resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
        unreachable!("release eligibility yields a synchronous issue");
    }
    unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
        unreachable!("a release is not cancellable");
    }

    let mut frame = ReleaseWait {
        operands,
        outcome: Outcome::default(),
        outside: None,
        token: 0,
        armed: false,
        next: None,
    };
    let provider = Provider {
        state: (&raw mut frame).cast(),
        wait,
        issue,
        resume,
        abort,
        deliver: provider::DELIVER,
    };
    let _commit = tasks::before_provider_entry();
    let mut request = super::helpers::request(RELEASE_WAIT_OP, site);
    // SAFETY: the frame and task stay pinned while waiting for this release.
    unsafe { tasks::provider_operation(context, &provider, &mut request, None).status }
}
