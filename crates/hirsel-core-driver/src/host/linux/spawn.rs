//! Starting a launcher child on Linux: `clone3` with `CLONE_PIDFD` (and
//! `CLONE_INTO_CGROUP` under a delegated subtree), then in the child the
//! parent-death signal, a new session, the working directory, the descriptor
//! slots and `execveat` of the verified image (launch.md, "Children").
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::launcher::spawn::{Prepared, pointers};
use crate::launcher::{above_launch_slots, wait_pid};

pub(crate) const IMAGE_FD: RawFd = 6;

/// A started child, pinned by its pidfd until it is reaped.
pub(crate) struct Spawned {
    pid: libc::pid_t,
    pidfd: OwnedFd,
}

impl Spawned {
    pub(crate) fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Send `signal` to the child. A child that has exited is not an error.
    pub(crate) fn signal(&self, signal: i32) -> io::Result<()> {
        send(&self.pidfd, signal)
    }

    /// Whether the child has exited, without waiting or reaping it.
    pub(crate) fn exited(&self) -> bool {
        let mut record = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows one initialized record and does not wait.
        unsafe { libc::poll(&raw mut record, 1, 0) > 0 && record.revents & libc::POLLIN != 0 }
    }

    /// The child's exit source for a readiness set: its pidfd, which the set
    /// removes when the watch ends, under the pidfd's number.
    pub(crate) fn exit_source(&self) -> (hirsel_core_net::ready::Child<'_>, u64, Option<RawFd>) {
        let fd = self.pidfd.as_raw_fd();
        (self.pidfd.as_fd(), fd as u64, Some(fd))
    }
}

fn send(pidfd: &OwnedFd, signal: i32) -> io::Result<()> {
    // SAFETY: a pidfd signal reaches only the process it pins.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if rc != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open pins a kernel process identity, with no signal.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pidfd_open returns a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Result of polling a pending child's close-on-exec error pipe.
pub(crate) enum ExecPoll {
    Pending,
    Ready(Spawned),
    Failed(io::Error),
}

/// Outcome of beginning a child spawn.
pub(crate) enum StartOutcome {
    #[allow(dead_code)]
    Started(Spawned),
    Pending(PendingSpawn),
}

/// A cloned child whose exec acknowledgement has not yet arrived.
pub(crate) struct PendingSpawn {
    pid: libc::pid_t,
    pidfd: Option<OwnedFd>,
    errors: File,
    buf: [u8; size_of::<i32>()],
    len: usize,
}

impl PendingSpawn {
    pub(crate) fn error_fd(&self) -> RawFd {
        self.errors.as_raw_fd()
    }

    pub(crate) fn exit_source(&self) -> (hirsel_core_net::ready::Child<'_>, u64, Option<RawFd>) {
        let pidfd = self.pidfd.as_ref().expect("pending pidfd");
        let fd = pidfd.as_raw_fd();
        (pidfd.as_fd(), fd as u64, Some(fd))
    }

    pub(crate) fn signal(&self, signal: i32) -> io::Result<()> {
        match &self.pidfd {
            Some(pidfd) => send(pidfd, signal),
            None => Ok(()),
        }
    }

    /// Nonblocking poll of the exec error pipe.
    ///
    /// Closes the error path identically to synchronous `start`:
    /// - EOF with 0 bytes => `Ready(Spawned)` (exec closed the `O_CLOEXEC` write end).
    /// - 4 bytes => `wait_pid` + `Failed(from_raw_os_error)`.
    /// - partial bytes then EOF => `wait_pid` + `Failed(UnexpectedEof)`.
    /// - I/O error => `SIGKILL` + `wait_pid` + `Failed(error)`.
    pub(crate) fn poll_exec(&mut self) -> ExecPoll {
        loop {
            match self.errors.read(&mut self.buf[self.len..]) {
                Ok(0) => {
                    if self.len == 0 {
                        let pidfd = self.pidfd.take().expect("pending pidfd");
                        return ExecPoll::Ready(Spawned {
                            pid: self.pid,
                            pidfd,
                        });
                    }
                    self.pidfd.take();
                    let _ = wait_pid(self.pid);
                    return ExecPoll::Failed(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                Ok(n) => {
                    self.len += n;
                    if self.len == self.buf.len() {
                        self.pidfd.take();
                        let _ = wait_pid(self.pid);
                        return ExecPoll::Failed(io::Error::from_raw_os_error(i32::from_ne_bytes(
                            self.buf,
                        )));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return ExecPoll::Pending;
                }
                Err(error) => {
                    if let Some(pidfd) = self.pidfd.take() {
                        let _ = send(&pidfd, libc::SIGKILL);
                    }
                    let _ = wait_pid(self.pid);
                    return ExecPoll::Failed(error);
                }
            }
        }
    }

    /// Kill and reap an unfinished pending child (e.g. on launcher teardown).
    pub(crate) fn kill_and_reap(mut self) {
        if let Some(pidfd) = self.pidfd.take() {
            let _ = send(&pidfd, libc::SIGKILL);
            let _ = wait_pid(self.pid);
        }
    }

    /// Block until the exec pipe completes (used by synchronous `start`).
    pub(crate) fn wait(mut self) -> io::Result<Spawned> {
        loop {
            match self.poll_exec() {
                ExecPoll::Ready(spawned) => return Ok(spawned),
                ExecPoll::Failed(error) => return Err(error),
                ExecPoll::Pending => {
                    let mut record = libc::pollfd {
                        fd: self.errors.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: poll borrows one initialized record.
                    if unsafe { libc::poll(&raw mut record, 1, -1) } < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        self.kill_and_reap();
                        return Err(error);
                    }
                }
            }
        }
    }
}

impl Drop for PendingSpawn {
    fn drop(&mut self) {
        if let Some(pidfd) = self.pidfd.take() {
            let _ = send(&pidfd, libc::SIGKILL);
            let _ = wait_pid(self.pid);
        }
    }
}

#[cfg(test)]
fn test_exec_delay_ms(program: &CString) -> u64 {
    let Ok(text) = program.to_str() else {
        return 0;
    };
    let name = Path::new(text)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    name.rsplit_once("-delay-")
        .and_then(|(_, rest)| rest.strip_suffix("ms"))
        .and_then(|digits| digits.parse::<u64>().ok())
        .unwrap_or(0)
}

pub(crate) fn begin(
    prepared: &Prepared,
    workdir: &Path,
    raw: &[(RawFd, RawFd)],
    cgroup: Option<&File>,
    serving: bool,
) -> io::Result<StartOutcome> {
    let program = &prepared.program;
    #[cfg(test)]
    let delay_ms = test_exec_delay_ms(program);
    let arguments = pointers(&prepared.argv);
    let mut envp = pointers(&prepared.env);
    // Two slots: LISTEN_PID, written in the child once it has its PID, and the end.
    if serving {
        envp.pop();
        envp.extend([std::ptr::null(), std::ptr::null()]);
    }
    let cwd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(workdir)?;
    let image = raw.iter().any(|&(_, target)| target == IMAGE_FD);
    let parent = std::process::id() as libc::pid_t;
    let (errors, report) = error_pipe()?;
    let mut child_fd = -1;
    const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;
    // Linux clone_args version 2 ends with the cgroup directory descriptor.
    let mut args = [0u64; 11];
    args[0] = libc::CLONE_PIDFD as u64;
    args[1] = (&raw mut child_fd) as u64;
    args[4] = libc::SIGCHLD as u64;
    if let Some(cgroup) = cgroup {
        args[0] |= CLONE_INTO_CGROUP;
        args[10] = cgroup.as_raw_fd() as u64;
    }
    // SAFETY: these flags give the child private memory and descriptors.
    // The child calls only syscalls on already prepared data before exec.
    let mut child = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            args.as_ptr(),
            std::mem::size_of_val(&args),
        ) as libc::pid_t
    };
    if child < 0 {
        let error = io::Error::last_os_error();
        if cgroup.is_some() || !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) {
            return Err(error);
        }
        // SAFETY: fork has the same child-side constraints as clone3 above.
        child = unsafe { libc::fork() };
        if child < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if child == 0 {
        // SAFETY: only this fork's descriptors, memory and signal state change.
        unsafe {
            exec(
                program,
                &arguments,
                &mut envp,
                raw,
                cwd.as_raw_fd(),
                report.as_raw_fd(),
                parent,
                serving,
                image,
                #[cfg(test)]
                delay_ms,
            )
        }
    }
    drop(report);
    let fd = if child_fd < 0 {
        pidfd(child as u32).inspect_err(|_| {
            // SAFETY: this unreaped PID belongs to our fork.
            unsafe { libc::kill(child, libc::SIGKILL) };
            let _ = wait_pid(child);
        })?
    } else {
        // SAFETY: CLONE_PIDFD installed a new descriptor in the parent.
        unsafe { OwnedFd::from_raw_fd(child_fd) }
    };
    Ok(StartOutcome::Pending(PendingSpawn {
        pid: child,
        pidfd: Some(fd),
        errors,
        buf: [0; size_of::<i32>()],
        len: 0,
    }))
}

pub(crate) fn start(
    prepared: &Prepared,
    workdir: &Path,
    raw: &[(RawFd, RawFd)],
    cgroup: Option<&File>,
    serving: bool,
) -> io::Result<Spawned> {
    match begin(prepared, workdir, raw, cgroup, serving)? {
        StartOutcome::Started(spawned) => Ok(spawned),
        StartOutcome::Pending(pending) => pending.wait(),
    }
}

fn error_pipe() -> io::Result<(File, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 initializes two descriptors and neither survives exec.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the pipe ends are fresh descriptors.
    let (read, write) = unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // The parent polls the read end nonblocking; the child's write end stays blocking.
    // SAFETY: fcntl on an owned descriptor.
    if unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((read, above_launch_slots(write.as_raw_fd())?))
}

#[allow(clippy::too_many_arguments)]
unsafe fn exec(
    program: &CString,
    argv: &[*const libc::c_char],
    env: &mut [*const libc::c_char],
    fds: &[(RawFd, RawFd)],
    cwd: RawFd,
    report: RawFd,
    parent: libc::pid_t,
    serving: bool,
    image: bool,
    #[cfg(test)] delay_ms: u64,
) -> ! {
    // SAFETY: this runs after clone in the child. The pointers and fd sources
    // were prepared by the parent. Every operation here is async-signal-safe.
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || libc::setsid() < 0
            || libc::fchdir(cwd) != 0
            || libc::signal(libc::SIGPIPE, libc::SIG_DFL) == libc::SIG_ERR
        {
            failed(report, *libc::__errno_location());
        }
        if libc::getppid() != parent {
            failed(report, libc::ESRCH);
        }
        for &(source, target) in fds {
            if libc::dup2(source, target) < 0 {
                failed(report, *libc::__errno_location());
            }
        }
        // The report pipe goes above every launch slot: 6 for a serving child, 4 for the
        // platform host (whose operator listener is 3), 3 otherwise.
        let mut first = if serving { 6 } else { 3 };
        for &(_, target) in fds {
            if target >= first && target < IMAGE_FD {
                first = target + 1;
            }
        }
        let report_slot = if image { IMAGE_FD + 1 } else { first };
        if report != report_slot && libc::dup3(report, report_slot, libc::O_CLOEXEC) < 0 {
            failed(report, *libc::__errno_location());
        }
        if image && !serving && libc::syscall(libc::SYS_close_range, 3u32, 5u32, 0u32) < 0 {
            failed(report_slot, *libc::__errno_location());
        }
        if libc::syscall(
            libc::SYS_close_range,
            (report_slot + 1) as u32,
            u32::MAX,
            0u32,
        ) < 0
        {
            failed(report_slot, *libc::__errno_location());
        }
        let mut activation = *b"LISTEN_PID=0000000000\0";
        if serving {
            let mut pid = libc::getpid() as u32;
            for digit in activation[11..21].iter_mut().rev() {
                *digit = b'0' + (pid % 10) as u8;
                pid /= 10;
            }
            let slot = env.len() - 2;
            env[slot] = activation.as_ptr().cast();
        }
        #[cfg(test)]
        if delay_ms > 0 {
            let ts = libc::timespec {
                tv_sec: (delay_ms / 1000) as libc::time_t,
                tv_nsec: ((delay_ms % 1000) * 1_000_000) as libc::c_long,
            };
            libc::nanosleep(&ts, std::ptr::null_mut());
        }
        if image {
            libc::syscall(
                libc::SYS_execveat,
                IMAGE_FD,
                c"".as_ptr(),
                argv.as_ptr(),
                env.as_ptr(),
                libc::AT_EMPTY_PATH,
            );
        } else {
            libc::execve(program.as_ptr(), argv.as_ptr(), env.as_ptr());
        }
        failed(report_slot, *libc::__errno_location());
    }
}

unsafe fn failed(report: RawFd, errno: i32) -> ! {
    let bytes = errno.to_ne_bytes();
    // SAFETY: the report pipe is live and the four-byte write is atomic.
    unsafe {
        while libc::write(report, bytes.as_ptr().cast(), bytes.len()) < 0
            && *libc::__errno_location() == libc::EINTR
        {}
        libc::_exit(127);
    }
}
