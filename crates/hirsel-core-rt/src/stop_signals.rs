//! The live Process provider. A self-pipe relay takes Unix signal delivery into
//! the scheduler's ordinary provider readiness path.
use crate::{
    entry::ProcessOperations,
    provider::{CANCELLED, Outcome, Provider, Request, STEP_PENDING, Step, Wait, deliver},
};

use std::{
    collections::BTreeSet,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
    thread::{self, JoinHandle},
};

#[unsafe(no_mangle)]
pub static hirsel_core_os_process_ops: ProcessOperations = ProcessOperations {
    install: StopSignals::install,
};

const SIGNALS: [i32; 2] = [libc::SIGTERM, libc::SIGINT];
const DISARMED: i32 = -1;
const NOTIFYING: i32 = -2;
const RECEIVED: i32 = -3;
// A nonnegative value identifies the live relay's eventfd. During NOTIFYING
// the handler owns its write, so shutdown waits before closing the descriptor.
static SIGNAL_FD: AtomicI32 = AtomicI32::new(DISARMED);
static ACTIVE: Mutex<Option<Relay>> = Mutex::new(None);

#[derive(Default)]
struct State {
    stopped: bool,
    tokens: BTreeSet<u64>,
}

pub(crate) struct StopSignals {
    provider: Provider,
    release: fn(),
}
impl StopSignals {
    fn install() -> io::Result<Self> {
        // The relay's start and its lock wait off the slot. The relay keeps
        // `state` while this user holds it.
        let state = crate::units::off_slot(|| {
            let mut active = ACTIVE.lock().unwrap();
            if active.is_none() {
                *active = Some(Relay::install()?);
            }
            let relay = active.as_mut().unwrap();
            relay.users += 1;
            io::Result::Ok(relay.state.clone())
        })?;
        Ok(Self {
            provider: Provider {
                state: Arc::as_ptr(&state).cast_mut().cast(),
                wait,
                issue: await_stop,
                resume: await_stop,
                abort,
                deliver,
            },
            release,
        })
    }

    pub(crate) fn provider(&self) -> *const Provider {
        &self.provider
    }
}
impl Drop for StopSignals {
    fn drop(&mut self) {
        (self.release)();
    }
}

fn release() {
    // The last user joins the relay thread, off the slot.
    crate::units::off_slot(|| {
        let mut active = ACTIVE.lock().unwrap();
        let relay = active.as_mut().unwrap();
        relay.users -= 1;
        if relay.users == 0 {
            drop(active.take());
        }
    });
}

struct Relay {
    fd: OwnedFd,
    thread: Option<JoinHandle<()>>,
    state: Arc<Mutex<State>>,
    previous: Vec<(i32, libc::sigaction)>,
    users: usize,
}
impl Relay {
    fn install() -> io::Result<Self> {
        // SAFETY: eventfd returns a new descriptor. A blocking reader wakes on
        // the first signal or shutdown. These are the only two possible writes.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let thread = thread::Builder::new().spawn(move || relay(fd, &shared))?;
        let mut relay = Self {
            fd: owned,
            thread: Some(thread),
            state,
            previous: Vec::with_capacity(SIGNALS.len()),
            users: 0,
        };
        SIGNAL_FD.store(fd, Ordering::Release);
        for signal in SIGNALS {
            // SAFETY: the actions have valid masks and a signal-safe handler.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous: libc::sigaction = std::mem::zeroed();
                libc::sigemptyset(&mut action.sa_mask);
                action.sa_sigaction = handle as *const () as usize;
                action.sa_flags = libc::SA_RESTART;
                if libc::sigaction(signal, &action, &mut previous) < 0 {
                    return Err(io::Error::last_os_error());
                }
                relay.previous.push((signal, previous));
            }
        }
        Ok(relay)
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.drain(..) {
            // SAFETY: restore the action saved for this signal at installation.
            unsafe { libc::sigaction(signal, &previous, std::ptr::null_mut()) };
        }
        loop {
            let current = SIGNAL_FD.load(Ordering::Acquire);
            if current == NOTIFYING {
                std::hint::spin_loop();
            } else if SIGNAL_FD
                .compare_exchange(current, DISARMED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        notify(self.fd.as_raw_fd());
        self.thread.take().unwrap().join().unwrap();
    }
}

extern "C" fn handle(signal: i32) {
    loop {
        let fd = SIGNAL_FD.load(Ordering::Acquire);
        match fd {
            DISARMED => {
                // A handler already dispatched when shutdown restored the
                // embedding process's action. Hand delivery back to that action.
                unsafe { libc::raise(signal) };
                return;
            }
            NOTIFYING | RECEIVED => {
                // SAFETY: only async-signal-safe POSIX calls. Unblock this
                // signal on this thread and take its default crash action.
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                    let mut mask = std::mem::zeroed();
                    libc::sigemptyset(&mut mask);
                    libc::sigaddset(&mut mask, signal);
                    libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut());
                    libc::raise(signal);
                    libc::_exit(128 + signal);
                }
            }
            _ => {
                if SIGNAL_FD
                    .compare_exchange(fd, NOTIFYING, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    notify(fd);
                    SIGNAL_FD.store(RECEIVED, Ordering::Release);
                    return;
                }
            }
        }
    }
}

fn notify(fd: i32) {
    // SAFETY: write is async-signal-safe. Restore the interrupted thread's
    // errno even if the write was interrupted by another signal.
    unsafe {
        let errno = libc::__errno_location();
        let saved = *errno;
        let one = 1u64;
        while libc::write(fd, (&one as *const u64).cast(), size_of::<u64>()) < 0
            && *errno == libc::EINTR
        {}
        *errno = saved;
    }
}

fn relay(fd: i32, state: &Mutex<State>) {
    let mut value = 0u64;
    loop {
        // SAFETY: installation owns the eventfd until this thread joins.
        if unsafe { libc::read(fd, (&mut value as *mut u64).cast(), size_of::<u64>()) } > 0 {
            break;
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            crate::process::stop(b"Signal");
        }
    }
    let tokens = {
        let mut state = state.lock().unwrap();
        state.stopped = true;
        std::mem::take(&mut state.tokens)
    };
    for token in tokens {
        crate::tasks::hirsel_core_ready(token);
    }
}

unsafe extern "C" fn wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait::default()
}

unsafe extern "C" fn await_stop(state: *mut u8, _: *mut Request, token: u64) -> Step {
    // SAFETY: the OS launch pins the relay through every Process operation.
    let mut state = unsafe { &*state.cast::<Mutex<State>>() }.lock().unwrap();
    if state.stopped {
        Step::default()
    } else {
        state.tokens.insert(token);
        Step {
            kind: STEP_PENDING,
            ..Step::default()
        }
    }
}

unsafe extern "C" fn abort(state: *mut u8, _: *mut Request, token: u64, _: u8) -> Outcome {
    // SAFETY: the OS launch pins the relay through every Process operation.
    unsafe { &*state.cast::<Mutex<State>>() }
        .lock()
        .unwrap()
        .tokens
        .remove(&token);
    CANCELLED
}
