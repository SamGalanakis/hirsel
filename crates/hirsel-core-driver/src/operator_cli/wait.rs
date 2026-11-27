//! Deadline and interruption belong to the local observer of the host operation.
use super::finite::Failure;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupted(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

pub(crate) fn duration(text: &str) -> Result<Duration, String> {
    let (digits, scale) = [("ms", 1u64), ("s", 1000), ("m", 60_000), ("h", 3_600_000)]
        .into_iter()
        .find_map(|(suffix, scale)| text.strip_suffix(suffix).map(|v| (v, scale)))
        .ok_or("--wait requires a duration in ms, s, m or h")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid --wait duration".into());
    }
    let ms = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .ok_or("wait duration too large")?;
    Ok(Duration::from_millis(ms))
}

#[derive(Clone, Copy)]
pub(crate) struct Observer(Option<Instant>);

impl Observer {
    pub(crate) fn check(self) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};
        if INTERRUPTED.load(Ordering::Relaxed) {
            return Err(Error::new(ErrorKind::Interrupted, "observer detached"));
        }
        if self.0.is_some_and(|d| Instant::now() >= d) {
            return Err(Error::new(ErrorKind::TimedOut, "wait deadline reached"));
        }
        Ok(())
    }
}

pub(super) struct Wait {
    pub observer: Observer,
    previous: libc::sigaction,
}

impl Wait {
    pub fn start(limit: Option<Duration>) -> Result<Self, Failure> {
        let end = limit
            .map(|d| {
                Instant::now()
                    .checked_add(d)
                    .ok_or_else(|| Failure::input("wait", "deadline out of range"))
            })
            .transpose()?;
        // SAFETY: sigaction writes the previous action to an initialized slot. The
        // process has one CLI invocation, and its handler only sets an atomic flag.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let mut previous = std::mem::zeroed();
            action.sa_sigaction = interrupted as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            INTERRUPTED.store(false, Ordering::Relaxed);
            if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                return Err(Failure::io(std::io::Error::last_os_error()));
            }
            Ok(Self {
                observer: Observer(end),
                previous,
            })
        }
    }

    pub fn remaining(&self) -> Result<Duration, Failure> {
        if INTERRUPTED.load(Ordering::Relaxed) {
            return Err(Failure::blocked("Interrupted", "observer detached"));
        }
        match self.observer.0 {
            Some(end) => end
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| Failure::blocked("Deadline", "wait deadline reached")),
            None => Ok(Duration::MAX),
        }
    }

    pub fn poll(&self) -> Result<(), Failure> {
        std::thread::sleep(self.remaining()?.min(Duration::from_millis(100)));
        self.remaining().map(|_| ())
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        // SAFETY: this action came from the successful installation in start.
        unsafe { libc::sigaction(libc::SIGINT, &self.previous, std::ptr::null_mut()) };
    }
}
