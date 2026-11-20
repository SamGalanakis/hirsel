//! SYNC and SYNC_DIR off the dispatcher: one helper thread runs each
//! `fdatasync` or `fsync` while the issuing task parks in I, so the other
//! tasks keep running (a journal's next group forms during its sync).
//!
//! One ordered path: every sync is queued in issue order and run by the one
//! helper, exactly once, on a duplicate of the program's descriptor (same
//! open file description, same writeback-error cursor). Its result, Ok or
//! the syscall's errno, is the only answer: an issued sync is not
//! cancellable (the scheduler does not abort it; a cancel takes effect at
//! the task's next cancellation point). A failure is never retried.
//!
//! The queue holds one entry per parked task. A failed duplicate or a helper
//! that cannot start answers its errno before any syscall; a later sync
//! tries the start again. A fork without exec after the first sync is
//! unsupported: the child stops at its next runtime entry or sync, before
//! taking any lock.
use crate::provider::{IoError, Outcome, STEP_DONE, STEP_PENDING, Step};
use crate::tasks::Outside;
use std::{
    collections::{BTreeMap, VecDeque},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

const STACK: usize = 64 * 1024;

struct Job {
    token: u64,
    /// mc-units-untied: the issuing unit, whose waiter alone gets the ready.
    unit: u32,
    directory: bool,
    fd: OwnedFd,
    /// Holds the issuer's waiter out of quiescence until the ready is pushed.
    outside: Outside,
}

static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
static WORK: Condvar = Condvar::new();
/// Serialises starting the helper; a failed start is not remembered.
static START: Mutex<()> = Mutex::new(());
static STARTED: AtomicBool = AtomicBool::new(false);
/// The process that first tried to start the helper, or 0. It is recorded
/// before START is taken, so a child forked during a start (when START or
/// the queue may be held by a thread it does not have) also sees another id.
static HELPER_PID: AtomicU32 = AtomicU32::new(0);
/// Each queued, running or finished sync not yet delivered: RUNNING, or 0 or
/// the syscall's errno. The slot is made at submit, on the issuing thread, so
/// the helper only writes it and never allocates.
static RESULTS: Mutex<BTreeMap<u64, i32>> = Mutex::new(BTreeMap::new());
const RUNNING: i32 = -1;

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
            .spawn(|| {
                crate::units::restore_mask();
                run()
            })
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
            token,
            unit,
            directory,
            fd,
            outside,
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
        if let Some(slot) = lock(&RESULTS).get_mut(&token) {
            *slot = errno;
        }
        crate::tasks::ready_unit(token, unit);
        drop(outside);
    }
}

/// Queue a sync of `fd` for `token` and answer Pending; or Done with the
/// errno of a duplicate or helper start that failed before any syscall.
pub(crate) fn submit(fd: RawFd, directory: bool, token: u64) -> Step {
    check_fork();
    if let Err(errno) = start() {
        return done(errno);
    }
    // SAFETY: fd is the program's live descriptor for the request's handle.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return done(
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EMFILE),
        );
    }
    // SAFETY: fcntl returned a new descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(dup) };
    let outside = Outside::begin();
    lock(&RESULTS).insert(token, RUNNING);
    lock(&QUEUE).push_back(Job {
        token,
        unit: crate::units::current(),
        directory,
        fd,
        outside,
    });
    WORK.notify_one();
    Step {
        kind: STEP_PENDING,
        ..Step::default()
    }
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
pub(crate) fn resume(token: u64) -> Step {
    let mut results = lock(&RESULTS);
    match results.get(&token) {
        Some(&errno) if errno != RUNNING => {
            results.remove(&token);
            done(errno)
        }
        _ => Step {
            kind: STEP_PENDING,
            ..Step::default()
        },
    }
}

/// An issued sync is not cancellable and has no deadline, so the scheduler
/// never aborts one.
pub(crate) fn abort() -> Outcome {
    crate::process::stop(b"SyncAbort")
}
