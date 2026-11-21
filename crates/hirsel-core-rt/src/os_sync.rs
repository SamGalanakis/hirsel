//! SYNC and SYNC_DIR off the dispatcher: helper threads run each `fdatasync` or
//! `fsync` while the issuing task parks in I, so the other tasks keep running
//! (a journal's next group forms during its sync).
//!
//! Each issued sync allocates an owned `SyncCell` whose pointer is carried by
//! the pinned `Request` on the issuer side and by the queued `Job` on the
//! helper side. There is no process-wide result map. A cell has two ownership
//! bits (`CELL_DONE` set by the helper after storing the syscall result, and
//! `CELL_TAKEN` set by the issuer when consuming the result or detaching on
//! unit end): whichever side observes the other bit already set frees the cell.
//! If the issuer detached before completion, the helper frees the cell and
//! drops `Outside` without waking the ended unit.
//!
//! An issued sync is not cancellable (the scheduler does not abort it; a cancel
//! takes effect at the task's next cancellation point). A failure is never
//! retried. A failed duplicate or a helper that cannot start answers its errno
//! before any syscall; a later sync tries the start again. A fork without exec
//! after the first sync is unsupported: the child stops at its next runtime
//! entry or sync, before taking any lock.
use crate::provider::{IoError, Outcome, Request, STEP_DONE, STEP_PENDING, Step};
use crate::tasks::Outside;
// The cell's state goes through the pool's seam, so the pool checker
// schedules the helper's completion against the issuing unit's take.
use crate::units::sync::AtomicU8;
use std::{
    cell::UnsafeCell,
    collections::VecDeque,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    ptr,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

const STACK: usize = 64 * 1024;

const CELL_DONE: u8 = 1;
const CELL_TAKEN: u8 = 2;

pub(crate) struct SyncCell {
    state: AtomicU8,
    errno: UnsafeCell<i32>,
    token: u64,
    outside: UnsafeCell<Option<Outside>>,
}

// SAFETY: `errno` is written once by the helper before setting `CELL_DONE`
// (Release) and read only by the issuer after observing `CELL_DONE` (Acquire).
// `outside` is initialized at creation and taken once by the helper before
// setting `CELL_DONE`. Freeing is serialized by the `CELL_DONE | CELL_TAKEN`
// transition.
unsafe impl Send for SyncCell {}
unsafe impl Sync for SyncCell {}

impl SyncCell {
    fn new(token: u64, outside: Option<Outside>) -> *mut Self {
        Box::into_raw(Box::new(Self {
            state: AtomicU8::new(0),
            errno: UnsafeCell::new(0),
            token,
            outside: UnsafeCell::new(outside),
        }))
    }

    /// Complete from the helper thread with `errno` (0 on success, >0 on failure).
    /// # Safety
    /// `cell` is a live `SyncCell` whose helper handle has not yet completed.
    unsafe fn complete(cell: *mut Self, errno: i32) {
        // SAFETY: sole helper owner; copy fields before publishing `CELL_DONE`.
        let (token, outside) = unsafe {
            let token = (*cell).token;
            let outside = (*(*cell).outside.get()).take();
            *(*cell).errno.get() = errno;
            (token, outside)
        };
        let prev = unsafe { (*cell).state.fetch_or(CELL_DONE, Ordering::AcqRel) };
        if prev & CELL_TAKEN != 0 {
            // Issuer already detached (unit ended): free now and do not deliver a wake.
            // SAFETY: both handles finished; we are the sole remaining owner.
            unsafe { drop(Box::from_raw(cell)) };
            drop(outside);
            return;
        }
        // Never touch `cell` after setting `CELL_DONE` without `CELL_TAKEN`.
        if let Some(outside) = outside {
            outside.complete(token);
        }
    }

    /// Try to consume the result on the issuer side. Returns `Some(errno)` once
    /// `CELL_DONE` is observed, freeing the cell; returns `None` while in flight.
    /// # Safety
    /// `cell` is a live `SyncCell` whose issuer handle has not yet been taken.
    unsafe fn try_take(cell: *mut Self) -> Option<i32> {
        // SAFETY: issuer holds its handle until `CELL_TAKEN` is set.
        if unsafe { (*cell).state.load(Ordering::Acquire) } & CELL_DONE == 0 {
            return None;
        }
        let errno = unsafe { *(*cell).errno.get() };
        let prev = unsafe { (*cell).state.fetch_or(CELL_TAKEN, Ordering::AcqRel) };
        debug_assert!(prev & CELL_DONE != 0);
        // SAFETY: both `CELL_DONE` and `CELL_TAKEN` are set; we are the last owner.
        unsafe { drop(Box::from_raw(cell)) };
        Some(errno)
    }

    /// For the pool checker: a cell as `submit` makes one, the helper's
    /// completion, and the issuer's take after its ready.
    #[cfg(test)]
    pub(crate) fn for_check(token: u64, outside: Outside) -> *mut Self {
        Self::new(token, Some(outside))
    }
    /// # Safety
    /// As `complete`.
    #[cfg(test)]
    pub(crate) unsafe fn complete_for_check(cell: *mut Self, errno: i32) {
        unsafe { Self::complete(cell, errno) }
    }
    /// # Safety
    /// As `try_take`.
    #[cfg(test)]
    pub(crate) unsafe fn take_for_check(cell: *mut Self) -> Option<i32> {
        unsafe { Self::try_take(cell) }
    }

    /// Detach the issuer handle when a unit or provider ends while a sync is in
    /// flight. If the helper already completed, frees the cell immediately;
    /// otherwise the helper frees it upon completion and delivers nothing.
    /// # Safety
    /// `cell` is a live `SyncCell` whose issuer handle has not yet been taken.
    pub(crate) unsafe fn detach(cell: *mut Self) {
        let prev = unsafe { (*cell).state.fetch_or(CELL_TAKEN, Ordering::AcqRel) };
        if prev & CELL_DONE != 0 {
            // SAFETY: helper already completed; we are the sole remaining owner.
            unsafe { drop(Box::from_raw(cell)) };
        }
    }
}

struct Job {
    directory: bool,
    fd: OwnedFd,
    cell: *mut SyncCell,
}
// SAFETY: `SyncCell` is Send+Sync and `Job` holds its sole helper handle.
unsafe impl Send for Job {}

static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
static WORK: Condvar = Condvar::new();
/// Serialises starting the helper; a failed start is not remembered.
static START: Mutex<()> = Mutex::new(());
static STARTED: AtomicBool = AtomicBool::new(false);
/// The process that first tried to start the helper, or 0. It is recorded
/// before START is taken, so a child forked during a start (when START or
/// the queue may be held by a thread it does not have) also sees another id.
static HELPER_PID: AtomicU32 = AtomicU32::new(0);

/// Test-only: while set, the helper holds each sync before its syscall.
#[cfg(test)]
static HOLD: Mutex<bool> = Mutex::new(false);
#[cfg(test)]
static HELD: Condvar = Condvar::new();
#[cfg(test)]
pub(crate) fn hold(on: bool) {
    *lock(&HOLD) = on;
    HELD.notify_all();
}

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Stops a forked child of a process whose helper started: the child has no
/// helper, and a lock may have been held across the fork. Takes no lock.
#[inline]
pub(crate) fn check_fork() {
    let pid = HELPER_PID.load(Ordering::Acquire);
    if pid != 0 && pid != std::process::id() {
        crate::process::stop(b"SyncAfterFork");
    }
}

fn start() -> Result<(), i32> {
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    HELPER_PID.store(std::process::id(), Ordering::Release);
    let _start = lock(&START);
    if STARTED.load(Ordering::Acquire) {
        return Ok(());
    }
    // mc-units-proto: HIRSEL_SYNC_HELPERS runs that many helpers on one queue.
    let helpers = std::env::var("HIRSEL_SYNC_HELPERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n >= 1)
        .unwrap_or(1);
    for _ in 0..helpers {
        std::thread::Builder::new()
            .name("hirsel-sync".into())
            .stack_size(STACK)
            .spawn(run)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EAGAIN))?;
    }
    STARTED.store(true, Ordering::Release);
    Ok(())
}

fn run() {
    loop {
        let job = {
            let mut queue = lock(&QUEUE);
            loop {
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = WORK.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        #[cfg(test)]
        {
            let mut held = lock(&HOLD);
            while *held {
                held = HELD.wait(held).unwrap_or_else(|e| e.into_inner());
            }
        }
        let Job {
            directory,
            fd,
            cell,
        } = job;
        // SAFETY: the job owns its duplicate descriptor. One call, no retry.
        let rc = unsafe {
            if directory {
                libc::fsync(fd.as_raw_fd())
            } else {
                libc::fdatasync(fd.as_raw_fd())
            }
        };
        // errno is this thread's and is read before anything else runs here.
        let errno = if rc < 0 {
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO)
        } else {
            0
        };
        drop(fd);
        // SAFETY: `cell` was allocated by `submit` and this Job is its helper handle.
        unsafe { SyncCell::complete(cell, errno) };
    }
}

/// Queue a sync of `fd` for `token` and answer Pending (recording the owned
/// `SyncCell` in `request.path` and returning its pointer when not tracked by
/// a pooled unit); or Done with the errno of a duplicate or helper start that
/// failed before any syscall.
/// # Safety
/// `request` is a pinned `SYNC` or `SYNC_DIR` request whose `path` is unused
/// by the caller and stays live until `resume` returns `STEP_DONE`.
pub(crate) unsafe fn submit(
    fd: RawFd,
    directory: bool,
    token: u64,
    request: *mut Request,
) -> (Step, Option<*mut SyncCell>) {
    check_fork();
    let started = if STARTED.load(Ordering::Acquire) {
        Ok(())
    } else if let Some(memory) = crate::units::memory() {
        // Starting the helpers can map their OS thread stacks.
        memory.map(start)
    } else {
        start()
    };
    if let Err(errno) = started {
        return (done(errno), None);
    }
    let _blocking = crate::units::blocking();
    // SAFETY: fd is the program's live descriptor for the request's handle.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return (
            done(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EMFILE),
            ),
            None,
        );
    }
    // SAFETY: fcntl returned a new descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(dup) };
    let outside = Outside::begin();
    let cell = SyncCell::new(token, Some(outside));
    // SAFETY: `request` is pinned and `path` is unused for SYNC/SYNC_DIR.
    unsafe { (*request).path = cell.cast::<u8>().cast_const() };
    let untracked = (!crate::units::track_sync(cell)).then_some(cell);
    lock(&QUEUE).push_back(Job {
        directory,
        fd,
        cell,
    });
    WORK.notify_one();
    (
        Step {
            kind: STEP_PENDING,
            ..Step::default()
        },
        untracked,
    )
}

fn done(errno: i32) -> Step {
    Step {
        kind: STEP_DONE,
        outcome: outcome(errno),
        deadline_ns: 0,
    }
}

fn outcome(errno: i32) -> Outcome {
    if errno == 0 {
        Outcome::default()
    } else {
        Outcome {
            error: IoError::from_errno(errno),
            ..Outcome::default()
        }
    }
}

/// Done with the sync's result once the helper has it; Pending before.
/// Returns the consumed cell pointer when `STEP_DONE` is returned so the
/// caller can remove it from its provider tracking list.
/// # Safety
/// `request` is the pinned `SYNC` or `SYNC_DIR` request passed to `submit`.
pub(crate) unsafe fn resume(request: *mut Request) -> (Step, Option<*mut SyncCell>) {
    let cell = unsafe { (*request).path as *mut SyncCell };
    if cell.is_null() {
        return (
            Step {
                kind: STEP_PENDING,
                ..Step::default()
            },
            None,
        );
    }
    // SAFETY: `cell` is owned by this request until `try_take` returns `Some`.
    match unsafe { SyncCell::try_take(cell) } {
        Some(errno) => {
            unsafe { (*request).path = ptr::null() };
            crate::units::untrack_sync(cell);
            (done(errno), Some(cell))
        }
        None => (
            Step {
                kind: STEP_PENDING,
                ..Step::default()
            },
            None,
        ),
    }
}

/// An issued sync is not cancellable and has no deadline, so the scheduler
/// never aborts one.
pub(crate) fn abort() -> Outcome {
    crate::process::stop(b"SyncAbort")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    struct HoldGuard;
    impl Drop for HoldGuard {
        fn drop(&mut self) {
            hold(false);
        }
    }

    fn temp_file() -> OwnedFd {
        let path = std::env::temp_dir().join(format!(
            "hirsel-sync-cell-{}-{}.tmp",
            std::process::id(),
            crate::os::monotonic_ns()
        ));
        let file = File::options()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("create temp file");
        let _ = std::fs::remove_file(&path);
        file.into()
    }

    #[test]
    fn sync_cell_completion_before_resume_delivers_once() {
        let fd = temp_file();
        let mut req: Request = unsafe { std::mem::zeroed() };
        let (step, _) = unsafe { submit(fd.as_raw_fd(), false, 1001, &mut req) };
        assert_eq!(step.kind, STEP_PENDING);

        // Wait until the helper finishes before calling `resume`, then verify
        // delivery happens exactly once and clears `request.path`.
        let cell = req.path as *mut SyncCell;
        while unsafe { (*cell).state.load(Ordering::Acquire) } & CELL_DONE == 0 {
            std::thread::yield_now();
        }
        let (first, consumed) = unsafe { resume(&mut req) };
        assert_eq!(first.kind, STEP_DONE);
        assert_eq!(first.outcome.error, IoError::OK);
        assert_eq!(consumed, Some(cell));
        assert!(req.path.is_null());

        let (second, consumed2) = unsafe { resume(&mut req) };
        assert_eq!(second.kind, STEP_PENDING);
        assert_eq!(consumed2, None);
    }

    #[test]
    fn sync_cell_directory_fsync_succeeds() {
        let dir = File::open(std::env::temp_dir()).expect("open temp dir");
        let mut req: Request = unsafe { std::mem::zeroed() };
        let (step, _) = unsafe { submit(dir.as_raw_fd(), true, 1002, &mut req) };
        assert_eq!(step.kind, STEP_PENDING);
        let done_step = loop {
            let (s, _) = unsafe { resume(&mut req) };
            if s.kind == STEP_DONE {
                break s;
            }
            std::thread::yield_now();
        };
        assert_eq!(done_step.outcome.error, IoError::OK);
    }

    #[test]
    fn sync_cell_unit_end_while_in_flight_detaches_without_delivery() {
        let fd = temp_file();
        let readies = crate::tasks::Readies::for_unit(crate::units::NONE);

        // Case 1: unit ends (detaches) BEFORE helper completes.
        // Hold helper so completion cannot race ahead of detach.
        let _guard = HoldGuard;
        hold(true);
        let outside = Outside::for_readies(readies.clone());
        let cell = SyncCell::new(2001, Some(outside));
        let dup = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(dup >= 0);
        let dup_fd = unsafe { OwnedFd::from_raw_fd(dup) };
        start().expect("start helper");
        lock(&QUEUE).push_back(Job {
            directory: false,
            fd: dup_fd,
            cell,
        });
        WORK.notify_one();

        // Unit ends while sync is still held in flight.
        unsafe { SyncCell::detach(cell) };
        hold(false);

        // Wait until the helper finishes and drops `Outside`.
        while readies.has_outside() {
            std::thread::yield_now();
        }
        // Nothing was delivered to the ended unit's waiter.
        assert!(!readies.take_for_test(2001));

        // Case 2: helper completes BEFORE unit ends (detaches).
        let outside2 = Outside::for_readies(readies.clone());
        let cell2 = SyncCell::new(2002, Some(outside2));
        let dup2 = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(dup2 >= 0);
        let dup_fd2 = unsafe { OwnedFd::from_raw_fd(dup2) };
        lock(&QUEUE).push_back(Job {
            directory: false,
            fd: dup_fd2,
            cell: cell2,
        });
        WORK.notify_one();
        while readies.has_outside() {
            std::thread::yield_now();
        }
        // Detaching after completion frees the cell cleanly without double-free.
        unsafe { SyncCell::detach(cell2) };
    }
}
