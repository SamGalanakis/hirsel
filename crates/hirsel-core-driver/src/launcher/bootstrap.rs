//! The platform host as the launcher's first child (docs/spec/launch.md "Starting the
//! host"). The launcher binds the passive operator listener, starts
//! `platform_main` with that listener as descriptor 3 and the operator UID beside it, and
//! starts it again when it ends: at once the first time, then after 1, 2, 4 ... s up to
//! 5 min, with the delay reset after 5 healthy minutes. A host that ends 64 (usage) or
//! 74 (catalog) is never started again: the launcher ends with its status.
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use super::ready::{Readiness, Watch};
use super::{above_launch_slots, spawn, wait_pid};
use hirsel_core_net::ready::Event;

/// The descriptor the host receives the operator listener on.
pub(super) const OPERATOR_FD: i32 = 3;

/// The environment that names the operator listener to the host's runtime: the launcher's
/// PID (the runtime checks it is the parent), the descriptor and the operator UID.
pub(crate) const OPERATOR_ENV: &str = "HIRSEL_OPERATOR_LISTEN";

const LONGEST: Duration = Duration::from_secs(300);
const HEALTHY: Duration = Duration::from_secs(300);

/// What the launcher does once the host has ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    /// Start it again after this delay.
    After(Duration),
    /// End the launcher with this status.
    Stop(i32),
}

/// The host's restart schedule.
#[derive(Default)]
pub(super) struct Schedule {
    failures: u32,
}

impl Schedule {
    /// The host ended with `code` (None: by a signal) after running for `ran`.
    pub(super) fn ended(&mut self, code: Option<i32>, ran: Duration) -> Next {
        if let Some(code @ (64 | 74)) = code {
            return Next::Stop(code);
        }
        if ran >= HEALTHY {
            self.failures = 0;
        }
        let delay = match self.failures {
            0 => Duration::ZERO,
            n => Duration::from_secs(1u64 << (n - 1).min(16)).min(LONGEST),
        };
        self.failures = self.failures.saturating_add(1);
        Next::After(delay)
    }
}

struct Running {
    process: spawn::Spawned,
    watch: Watch,
    since: Instant,
}

/// The bootstrap child and its operator listener.
pub(super) struct Bootstrap {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    operator: UnixListener,
    uid: u32,
    schedule: Schedule,
    running: Option<Running>,
    due: Option<Instant>,
    stopped: Option<i32>,
}

impl Bootstrap {
    /// The host `program` run with `args` in `cwd` (the state directory), receiving
    /// `operator`, to be started on the launcher's first turn.
    pub(super) fn new(
        program: PathBuf,
        args: Vec<OsString>,
        cwd: PathBuf,
        operator: UnixListener,
    ) -> Self {
        // SAFETY: geteuid has no failure.
        let uid = unsafe { libc::geteuid() };
        Self {
            program,
            args,
            cwd,
            operator,
            uid,
            schedule: Schedule::default(),
            running: None,
            due: Some(Instant::now()),
            stopped: None,
        }
    }

    /// The live host's PID: the one peer the control socket admits.
    pub(super) fn pid(&self) -> Option<libc::pid_t> {
        self.running.as_ref().map(|r| r.process.pid())
    }

    /// The status the launcher ends with, once the host may not be started again.
    pub(super) fn stopped(&self) -> Option<i32> {
        self.stopped
    }

    /// How long the launcher may wait before the next start is due.
    pub(super) fn timeout(&self) -> Option<Duration> {
        self.due
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    fn start(&mut self, poll: &Readiness) -> io::Result<()> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        // SAFETY: getpid has no failure.
        let parent = unsafe { libc::getpid() };
        command.env(OPERATOR_ENV, format!("{parent}:{OPERATOR_FD}:{}", self.uid));
        spawn::clear_activation(&mut command);
        let null = File::options().read(true).open("/dev/null")?;
        let slots = vec![
            (above_launch_slots(null.as_raw_fd())?, 0),
            (above_launch_slots(libc::STDOUT_FILENO)?, 1),
            (above_launch_slots(libc::STDERR_FILENO)?, 2),
            (above_launch_slots(self.operator.as_raw_fd())?, OPERATOR_FD),
        ];
        let process = spawn::start(&command, &self.cwd, &slots, None, false)?;
        let watch = match poll.watch_exit(&process) {
            Ok(watch) => watch,
            Err(error) => {
                let _ = process.signal(libc::SIGKILL);
                let _ = wait_pid(process.pid());
                return Err(error);
            }
        };
        self.running = Some(Running {
            process,
            watch,
            since: Instant::now(),
        });
        Ok(())
    }

    /// One launcher turn: reap an ended host and schedule it, and start one that is due.
    pub(super) fn turn(&mut self, poll: &Readiness, events: &[Event]) -> io::Result<()> {
        if let Some(running) = &self.running
            && running.watch.fired(events)
        {
            let running = self.running.take().expect("checked above");
            drop(running.watch);
            let code = wait_pid(running.process.pid())?.code();
            drop(running.process);
            match self.schedule.ended(code, running.since.elapsed()) {
                Next::After(delay) => self.due = Some(Instant::now() + delay),
                Next::Stop(code) => {
                    self.due = None;
                    self.stopped = Some(code);
                }
            }
        }
        if self.running.is_none()
            && self.stopped.is_none()
            && self.due.is_some_and(|at| at <= Instant::now())
        {
            self.due = None;
            if let Err(error) = self.start(poll) {
                eprintln!("hirsel launcher: cannot start the platform host: {error}");
                let delay = match self.schedule.ended(None, Duration::ZERO) {
                    Next::After(delay) => delay,
                    Next::Stop(_) => LONGEST,
                };
                self.due = Some(Instant::now() + delay);
            }
        }
        Ok(())
    }
}

impl Drop for Bootstrap {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.process.signal(libc::SIGKILL);
            let _ = wait_pid(running.process.pid());
        }
    }
}

pub(super) use crate::host::peer_pid;

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    /// The restart schedule (launch.md "Starting the host"): immediately, then 1, 2, 4 ... s
    /// capped at 300 s, back to immediately after a run of 5 healthy minutes, and never
    /// again after 64 or 74.
    #[test]
    fn host_restart_backoff() {
        let mut s = Schedule::default();
        let quick = Duration::from_millis(10);
        let mut delays = Vec::new();
        for _ in 0..12 {
            match s.ended(Some(75), quick) {
                Next::After(d) => delays.push(d.as_secs()),
                Next::Stop(_) => panic!("75 stopped the host"),
            }
        }
        assert_eq!(delays, [0, 1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        assert_eq!(s.ended(None, quick), Next::After(Duration::from_secs(300)));
        assert_eq!(s.ended(Some(75), 5 * MIN), Next::After(Duration::ZERO));
        assert_eq!(
            s.ended(Some(75), quick),
            Next::After(Duration::from_secs(1))
        );
        assert_eq!(
            s.ended(Some(75), 4 * MIN),
            Next::After(Duration::from_secs(2))
        );
        assert_eq!(s.ended(Some(64), quick), Next::Stop(64));
        assert_eq!(
            Schedule::default().ended(Some(74), 10 * MIN),
            Next::Stop(74)
        );
    }
}
