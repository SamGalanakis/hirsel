//! The terminal modes `stdin.read_secret` changes, and their restore on every
//! way out of the read: its own return, the run's end, a run stop and a
//! terminating signal (core/std/api/stdin.md, Secret input).
//!
//! The saved modes live in a lock-free slot so a signal handler can restore
//! them with async-signal-safe calls only. Arming and disarming are serialized
//! by a mutex no handler takes; a handler reads the slot as a seqlock.
use std::{
    mem::size_of,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering, fence},
    },
};

const WORDS: usize = size_of::<libc::termios>().div_ceil(8);
/// 0 while disarmed, else `generation << 32 | (fd + 1)`.
static STATE: AtomicU64 = AtomicU64::new(0);
/// The armed descriptor's modes before the read, then the modes the read
/// sets, written only while disarmed.
static SAVED: [AtomicU64; 2 * WORDS] = [const { AtomicU64::new(0) }; 2 * WORDS];
/// Serializes arming and disarming and numbers the arms.
static SLOT: Mutex<u32> = Mutex::new(0);

pub(crate) enum ArmError {
    /// Another read holds the slot.
    Busy,
    /// The mode change failed with this errno.
    Other(u32),
}

/// One armed read. Dropping it restores the saved modes, with `TCSAFLUSH` so
/// input left over from the line goes with them, then disarms.
pub(crate) struct Armed {
    word: u64,
    fd: i32,
    saved: libc::termios,
}

impl Drop for Armed {
    fn drop(&mut self) {
        let _slot = SLOT.lock().unwrap();
        if STATE.load(Ordering::Relaxed) == self.word {
            set(self.fd, libc::TCSAFLUSH, &self.saved);
            STATE.store(0, Ordering::Release);
        }
    }
}

/// Publishes `saved` for `fd`, then sets the quiet modes with `TCSAFLUSH`. Arming does not block `SIGTTOU`, so a background process stops
/// here instead of changing the foreground's terminal.
pub(crate) fn arm(fd: i32, saved: &libc::termios) -> Result<Armed, ArmError> {
    let word = {
        let mut generation = SLOT.lock().unwrap();
        if STATE.load(Ordering::Relaxed) != 0 {
            return Err(ArmError::Busy);
        }
        *generation = generation.wrapping_add(1);
        let mut words = [0u64; 2 * WORDS];
        // SAFETY: each half of `words` spans at least size_of::<termios>() bytes.
        unsafe {
            for (n, modes) in [saved, &quiet(saved)].into_iter().enumerate() {
                ptr::copy_nonoverlapping(
                    ptr::from_ref(modes).cast::<u8>(),
                    words[n * WORDS..].as_mut_ptr().cast::<u8>(),
                    size_of::<libc::termios>(),
                );
            }
        }
        // Readers that see any of these words also see the disarmed state.
        fence(Ordering::Release);
        for (to, word) in SAVED.iter().zip(words) {
            to.store(word, Ordering::Relaxed);
        }
        let word = u64::from(*generation) << 32 | u64::from(fd as u32 + 1);
        STATE.store(word, Ordering::Release);
        word
    };
    let armed = Armed {
        word,
        fd,
        saved: *saved,
    };
    let quiet = quiet(saved);
    // SAFETY: a live descriptor and an initialized termios.
    let code = loop {
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } == 0 {
            break 0;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => {}
            code => break code.unwrap_or(libc::EIO) as u32,
        }
    };
    if code != 0 {
        // Nothing changed, so disarm without a restore.
        let _slot = SLOT.lock().unwrap();
        STATE.store(0, Ordering::Release);
        std::mem::forget(armed);
        return Err(ArmError::Other(code));
    }
    Ok(armed)
}

/// The modes a read sets over `saved`: echo off, and non-canonical input a
/// byte at a time, so the runtime edits the line itself and no host line limit
/// or host line editing applies. `ECHONL` is off because Linux ignores it
/// without `ICANON` and Darwin does not, and `IEXTEN` because Darwin still
/// takes literal-next and discard without `ICANON` and Linux does not; the
/// read echoes the Enter itself ([`newline`]).
fn quiet(saved: &libc::termios) -> libc::termios {
    let mut quiet = *saved;
    quiet.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::IEXTEN);
    quiet.c_cc[libc::VMIN] = 1;
    quiet.c_cc[libc::VTIME] = 0;
    quiet
}

/// Echoes the Enter that ended a secret line, as `ECHONL` would: one newline,
/// which the terminal's output processing may write as `\r\n`. `SIGTTOU` is
/// blocked as for a restore. A descriptor that refuses the write, such as one
/// opened read-only, writes nothing.
pub(crate) fn newline(fd: i32) {
    ttou_blocked(|| {
        // SAFETY: one byte from a static, to a live descriptor.
        while unsafe { libc::write(fd, b"\n".as_ptr().cast(), 1) } < 0
            && unsafe { *crate::host::sys::errno() } == libc::EINTR
        {}
    });
}

/// At a stream's end: restore and disarm a read still armed on `fd`.
pub(crate) fn release_fd(fd: i32) {
    let _slot = SLOT.lock().unwrap();
    let state = STATE.load(Ordering::Relaxed);
    if state != 0 && (state & 0xffff_ffff) as i32 - 1 == fd {
        if let Some((fd, saved, _)) = saved() {
            set(fd, libc::TCSAFLUSH, &saved);
        }
        STATE.store(0, Ordering::Release);
    }
}

/// Restores an armed read's modes now, with `TCSANOW`, and leaves the slot
/// armed. Async-signal-safe and allocation-free: the terminating arm of each
/// signal handler and a run stop call it just before the process ends.
///
/// It restores only while the terminal holds the modes the read set. A read
/// stopped by `SIGTTOU` before its change, and killed from there, would
/// otherwise put stale modes over the ones the foreground has set since.
pub(crate) fn restore_now() {
    if let Some((fd, saved, quiet)) = saved() {
        let errno = crate::host::sys::errno();
        // SAFETY: errno belongs to the calling thread; tcgetattr fills `now`.
        unsafe {
            let kept = *errno;
            let mut now: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut now) == 0 && same(&now, &quiet) {
                set(fd, libc::TCSANOW, &saved);
            }
            *errno = kept;
        }
    }
}

/// Equal modes, ignoring `FLUSHO` and `PENDIN`: the kernel flips those as
/// output and input state (Ctrl-O sets `FLUSHO`), so they never mean the
/// foreground changed the modes.
fn same(a: &libc::termios, b: &libc::termios) -> bool {
    let state = libc::FLUSHO | libc::PENDIN;
    a.c_iflag == b.c_iflag
        && a.c_oflag == b.c_oflag
        && a.c_cflag == b.c_cflag
        && (a.c_lflag ^ b.c_lflag) & !state == 0
        && a.c_cc == b.c_cc
}

/// The armed descriptor, its saved modes and the modes the read sets, read as
/// a seqlock.
fn saved() -> Option<(i32, libc::termios, libc::termios)> {
    // Each retry follows a disarm or a new arm, both rare.
    loop {
        let before = STATE.load(Ordering::Acquire);
        if before == 0 {
            return None;
        }
        let mut words = [0u64; 2 * WORDS];
        for (to, word) in words.iter_mut().zip(&SAVED) {
            *to = word.load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        if STATE.load(Ordering::Relaxed) == before {
            // SAFETY: each half holds a termios copied in whole by `arm`; any
            // bit pattern is a valid termios.
            let (saved, quiet) = unsafe {
                (
                    ptr::read_unaligned(words.as_ptr().cast::<libc::termios>()),
                    ptr::read_unaligned(words[WORDS..].as_ptr().cast::<libc::termios>()),
                )
            };
            return Some(((before & 0xffff_ffff) as i32 - 1, saved, quiet));
        }
    }
}

/// `tcsetattr` with `SIGTTOU` blocked, so a process moved to the background
/// during the read restores instead of stopping. Async-signal-safe.
fn set(fd: i32, action: i32, termios: &libc::termios) {
    ttou_blocked(|| {
        // SAFETY: a live descriptor and an initialized termios.
        while unsafe { libc::tcsetattr(fd, action, termios) } != 0
            && unsafe { *crate::host::sys::errno() } == libc::EINTR
        {}
    });
}

/// Runs `f` with `SIGTTOU` blocked on this thread. Async-signal-safe.
fn ttou_blocked(f: impl FnOnce()) {
    // SAFETY: valid signal sets on this thread.
    unsafe {
        let mut block = std::mem::zeroed();
        let mut kept = std::mem::zeroed();
        libc::sigemptyset(&mut block);
        libc::sigaddset(&mut block, libc::SIGTTOU);
        libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut kept);
        f();
        libc::pthread_sigmask(libc::SIG_SETMASK, &kept, ptr::null_mut());
    }
}

const ALWAYS: [i32; 2] = [libc::SIGHUP, libc::SIGQUIT];
const UNRELAYED: [i32; 2] = [libc::SIGINT, libc::SIGTERM];

struct Installed {
    users: usize,
    previous: Vec<(i32, libc::sigaction)>,
}

static HANDLERS: Mutex<Installed> = Mutex::new(Installed {
    users: 0,
    previous: Vec::new(),
});

/// A run's hold on the restore-then-default handlers. The first hold installs
/// them over each default action, and the last one puts the previous actions
/// back where the handler is still installed. A signal whose action is not the
/// default keeps it, and so does one whose action another owner, such as an
/// overlapping run's stop relay, has set since.
pub(crate) struct Handlers;

impl Handlers {
    /// `relayed`: a Process stop relay owns `SIGINT` and `SIGTERM`, and its
    /// terminating arm restores.
    pub(crate) fn install(relayed: bool) -> Self {
        let mut installed = HANDLERS.lock().unwrap();
        if installed.users == 0 {
            let signals = if relayed { &[][..] } else { &UNRELAYED[..] };
            for &signal in ALWAYS.iter().chain(signals) {
                // SAFETY: the action has a valid mask and a signal-safe handler.
                unsafe {
                    let mut current: libc::sigaction = std::mem::zeroed();
                    libc::sigaction(signal, ptr::null(), &mut current);
                    if current.sa_sigaction != libc::SIG_DFL {
                        continue;
                    }
                    let mut action: libc::sigaction = std::mem::zeroed();
                    libc::sigemptyset(&mut action.sa_mask);
                    action.sa_sigaction = restore_then_default as *const () as usize;
                    let mut previous: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(signal, &action, &mut previous) == 0 {
                        installed.previous.push((signal, previous));
                    }
                }
            }
        }
        installed.users += 1;
        Self
    }
}

impl Drop for Handlers {
    fn drop(&mut self) {
        let mut installed = HANDLERS.lock().unwrap();
        installed.users -= 1;
        if installed.users == 0 {
            for (signal, previous) in installed.previous.drain(..) {
                // SAFETY: read the current action, then restore the one saved
                // at installation while this handler is still the action.
                unsafe {
                    let mut current: libc::sigaction = std::mem::zeroed();
                    libc::sigaction(signal, ptr::null(), &mut current);
                    if current.sa_sigaction == restore_then_default as *const () as usize {
                        libc::sigaction(signal, &previous, ptr::null_mut());
                    }
                }
            }
        }
    }
}

extern "C" fn restore_then_default(signal: i32) {
    restore_now();
    // SAFETY: only async-signal-safe POSIX calls. Take the default action,
    // with this signal unblocked on this thread.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        let mut mask = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, signal);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, ptr::null_mut());
        libc::raise(signal);
        libc::_exit(128 + signal);
    }
}
