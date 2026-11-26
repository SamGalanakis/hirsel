//! Virtual clocks keep intrusive arms in each waiting task's timer slot.
//! Expiry publishes the arm's unique token to the ordinary ready inbox.
use super::{Readies, Task, Timer};
use std::ptr;

pub(crate) const TOKEN_TAG: u64 = 1 << 63;

pub(crate) struct Arm {
    next: *mut Arm,
    at: u64,
    sequence: u64,
    token: u64,
    readies: *const Readies,
}

const _: () = {
    assert!(size_of::<Arm>() == size_of::<Timer>());
    assert!(align_of::<Arm>() == align_of::<Timer>());
};

pub(crate) type Link = *mut Arm;

#[derive(Default)]
pub(crate) struct Arms(Link);

// SAFETY: the clock mutex owns the links. A parked task pins its own arm and
// its scheduler until the task takes the same lock to retire it.
unsafe impl Send for Arms {}

impl Arms {
    pub(crate) fn first(&self) -> Option<(u64, u64)> {
        // SAFETY: the clock lock pins the list.
        unsafe { self.0.as_ref().map(|arm| (arm.at, arm.sequence)) }
    }

    /// # Safety
    /// The running task owns its timer slot, which is outside every list.
    pub(super) unsafe fn initialize(task: *mut Task, at: u64) -> Link {
        // SAFETY: the task pins its timer slot and scheduler.
        unsafe {
            let arm = (*task).timer.get().cast::<Arm>();
            let token = super::operation_token();
            arm.write(Arm {
                next: ptr::null_mut(),
                at,
                sequence: 0,
                token: token | TOKEN_TAG,
                readies: std::sync::Arc::as_ptr(&(*(*task).run).readies),
            });
            arm
        }
    }

    /// # Safety
    /// The clock lock is held and the given arm has just been initialized.
    pub(crate) unsafe fn insert(&mut self, arm: Link, sequence: u64) {
        // SAFETY: the clock lock controls these links and the new arm is private.
        unsafe {
            (*arm).sequence = sequence;
            let mut link = &raw mut self.0;
            while !(*link).is_null() && (**link).at <= (*arm).at {
                link = &raw mut (**link).next;
            }
            (*arm).next = *link;
            *link = arm;
        }
    }

    /// # Safety
    /// The clock lock pins all these arms, including the nonempty head.
    pub(crate) unsafe fn fire_first(&mut self) {
        // SAFETY: readiness is posted before the arm can be retired.
        unsafe {
            let arm = self.0;
            self.0 = (*arm).next;
            (&*(*arm).readies).push((*arm).token);
        }
    }

    /// # Safety
    /// The clock lock is held and the running task still owns this arm.
    pub(crate) unsafe fn retire(&mut self, arm: Link) {
        // SAFETY: the clock lock guards all chain edits. Once the arm is out
        // of the chain the clock can no longer publish this token.
        unsafe {
            let mut link = &raw mut self.0;
            while !(*link).is_null() {
                if *link == arm {
                    *link = (*arm).next;
                    return;
                }
                link = &raw mut (**link).next;
            }
            (&*(*arm).readies)
                .lock()
                .retain(|&token| token != (*arm).token);
        }
    }
}

/// # Safety
/// The scheduler owns the task, which is waiting on an armed virtual clock.
pub(super) unsafe fn token(task: *mut Task) -> u64 {
    // SAFETY: the token is immutable throughout registration.
    unsafe { (*(*task).timer.get().cast::<Arm>()).token }
}

/// # Safety
/// The task has removed this arm from the clock and the ready inbox.
pub(super) unsafe fn reset(task: *mut Task) {
    // SAFETY: only the task can reach the timer storage again.
    unsafe { (*task).timer.get().write(Timer::default()) }
}
