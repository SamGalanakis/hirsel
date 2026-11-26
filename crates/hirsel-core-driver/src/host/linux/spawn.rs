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

pub(crate) fn start(
    prepared: &Prepared,
    workdir: &Path,
    raw: &[(RawFd, RawFd)],
    cgroup: Option<&File>,
    serving: bool,
) -> io::Result<Spawned> {
    let program = &prepared.program;
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
    let (mut errors, report) = error_pipe()?;
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
    let mut bytes = [0; size_of::<i32>()];
    let result = loop {
        match errors.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result,
        }
    };
    match result {
        Ok(0) => Ok(Spawned {
            pid: child,
            pidfd: fd,
        }),
        Ok(n) => {
            let result = errors.read_exact(&mut bytes[n..]);
            let _ = wait_pid(child);
            result?;
            Err(io::Error::from_raw_os_error(i32::from_ne_bytes(bytes)))
        }
        Err(error) => {
            let _ = send(&fd, libc::SIGKILL);
            let _ = wait_pid(child);
            Err(error)
        }
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
