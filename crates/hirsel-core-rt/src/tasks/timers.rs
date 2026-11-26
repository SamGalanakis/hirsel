use super::Task;
use std::{ptr, ptr::NonNull};

#[derive(Default)]
pub(super) struct Timer {
    pub(super) parent: *mut Task,
    children: [*mut Task; 2],
    /// Host CLOCK_MONOTONIC nanoseconds.
    deadline: u64,
    /// Zero for a detached timer.
    pub(super) arm: u64,
}

/// Complete binary min-heap over the tasks' prepaid links. The arm sequence
/// preserves FIFO expiry at equal deadlines, including after a re-arm.
#[derive(Default)]
pub(super) struct TimerHeap {
    pub(super) root: *mut Task,
    pub(super) len: usize,
    arm_sequence: u64,
}

impl TimerHeap {
    pub(super) fn deadline(&self) -> Option<u64> {
        // SAFETY: only pinned operations belong to this heap.
        NonNull::new(self.root).map(|task| unsafe { (*(*task.as_ptr()).timer.get()).deadline })
    }

    unsafe fn precedes(a: *mut Task, b: *mut Task) -> bool {
        // SAFETY: both pointers name live tasks owned by the scheduler turn.
        unsafe {
            ((*(*a).timer.get()).deadline, (*(*a).timer.get()).arm)
                < ((*(*b).timer.get()).deadline, (*(*b).timer.get()).arm)
        }
    }

    /// Follow a one-based heap index's bits below the leading 1.
    unsafe fn at(&self, index: usize) -> *mut Task {
        debug_assert!((1..=self.len).contains(&index));
        // SAFETY: the complete tree has a node at every index through len.
        unsafe {
            let mut bit = (1 << index.ilog2()) >> 1;
            let mut task = self.root;
            while bit != 0 {
                task = (*(*task).timer.get()).children[usize::from(index & bit != 0)];
                bit >>= 1;
            }
            task
        }
    }

    unsafe fn replace(&mut self, old: *mut Task, new: *mut Task) {
        // SAFETY: old is linked in the heap. A non-null new task is being
        // moved to its position by the exclusive scheduler turn.
        unsafe {
            let parent = (*(*old).timer.get()).parent;
            if parent.is_null() {
                self.root = new;
            } else {
                let side = usize::from((*(*parent).timer.get()).children[1] == old);
                (*(*parent).timer.get()).children[side] = new;
            }
            if !new.is_null() {
                (*(*new).timer.get()).parent = parent;
            }
        }
    }

    unsafe fn set_children(task: *mut Task, children: [*mut Task; 2]) {
        // SAFETY: all non-null pointers are live tasks in this turn's heap.
        unsafe {
            (*(*task).timer.get()).children = children;
            for child in children {
                if !child.is_null() {
                    (*(*child).timer.get()).parent = task;
                }
            }
        }
    }

    /// Exchange two adjacent positions without changing the tree's shape.
    unsafe fn exchange_with_parent(&mut self, task: *mut Task, parent: *mut Task) {
        // SAFETY: both tasks are live, and parent is task's current parent.
        unsafe {
            let mut children = (*(*parent).timer.get()).children;
            let side = usize::from(children[1] == task);
            children[side] = parent;
            self.replace(parent, task);
            Self::set_children(parent, (*(*task).timer.get()).children);
            Self::set_children(task, children);
        }
    }

    unsafe fn sift_up(&mut self, task: *mut Task) {
        // SAFETY: task is a live heap node and only its order may be wrong.
        unsafe {
            let mut parent = (*(*task).timer.get()).parent;
            while !parent.is_null() && Self::precedes(task, parent) {
                self.exchange_with_parent(task, parent);
                parent = (*(*task).timer.get()).parent;
            }
        }
    }

    unsafe fn sift_down(&mut self, task: *mut Task) {
        // SAFETY: task and its descendants are live nodes in this turn's heap.
        unsafe {
            loop {
                let [left, right] = (*(*task).timer.get()).children;
                if left.is_null() {
                    return;
                }
                let child = if !right.is_null() && Self::precedes(right, left) {
                    right
                } else {
                    left
                };
                if !Self::precedes(child, task) {
                    return;
                }
                self.exchange_with_parent(child, task);
            }
        }
    }

    pub(super) unsafe fn insert(&mut self, task: *mut Task, deadline: u64) {
        // SAFETY: this turn owns the parked task, which is not armed.
        unsafe {
            debug_assert_eq!((*(*task).timer.get()).arm, 0);
            self.arm_sequence = self.arm_sequence.checked_add(1).unwrap_or_else(|| {
                crate::run_stop::raise(crate::run_stop::Stop::from_static(b"TimerArms"))
            });
            let index = self.len + 1;
            let parent = if self.len == 0 {
                self.root = task;
                ptr::null_mut()
            } else {
                let parent = self.at(index / 2);
                (*(*parent).timer.get()).children[index % 2] = task;
                parent
            };
            self.len = index;
            (*(*task).timer.get()) = Timer {
                parent,
                children: [ptr::null_mut(); 2],
                deadline,
                arm: self.arm_sequence,
            };
            self.sift_up(task);
        }
    }

    pub(super) unsafe fn remove(&mut self, task: *mut Task) {
        // SAFETY: only this turn mutates the live task's timer. Its links
        // locate it directly when armed.
        unsafe {
            if (*(*task).timer.get()).arm == 0 {
                return;
            }
            let last = self.at(self.len);
            self.replace(last, ptr::null_mut());
            self.len -= 1;
            if last != task {
                self.replace(task, last);
                Self::set_children(last, (*(*task).timer.get()).children);
                let parent = (*(*last).timer.get()).parent;
                if !parent.is_null() && Self::precedes(last, parent) {
                    self.sift_up(last);
                } else {
                    self.sift_down(last);
                }
            }
            (*(*task).timer.get()) = Timer::default();
        }
    }
}
