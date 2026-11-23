use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;

use super::{above_launch_slots, pidfd, send, wait_pid};

pub(super) const IMAGE_FD: RawFd = 6;

fn cstring(word: &OsStr) -> io::Result<CString> {
    CString::new(word.as_encoded_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

pub(super) fn start(
    command: &Command,
    workdir: &Path,
    slots: &[(OwnedFd, RawFd)],
    cgroup: Option<&File>,
    serving: bool,
) -> io::Result<(libc::pid_t, OwnedFd)> {
    let image = std::path::absolute(command.get_program())?;
    let program = cstring(image.as_os_str())?;
    let argv: Vec<_> = std::iter::once(Ok(program.clone()))
        .chain(command.get_args().map(cstring))
        .collect::<io::Result<_>>()?;
    let arguments = pointers(&argv);
    let mut environment: BTreeMap<_, _> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            environment.insert(key.to_owned(), value.to_owned());
        } else {
            environment.remove(key);
        }
    }
    let env: Vec<_> = environment
        .into_iter()
        .map(|(mut key, value)| {
            key.push("=");
            key.push(value);
            cstring(&key)
        })
        .collect::<io::Result<_>>()?;
    let mut envp = pointers(&env);
    if serving {
        envp.pop();
        envp.extend([std::ptr::null(), std::ptr::null()]);
    }
    let cwd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
        .open(workdir)?;
    let raw: Vec<_> = slots
        .iter()
        .map(|(fd, slot)| (fd.as_raw_fd(), *slot))
        .collect();
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
                &program,
                &arguments,
                &mut envp,
                &raw,
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
        Ok(0) => Ok((child, fd)),
        Ok(n) => {
            let result = errors.read_exact(&mut bytes[n..]);
            let _ = wait_pid(child);
            result?;
            Err(io::Error::from_raw_os_error(i32::from_ne_bytes(bytes)))
        }
        Err(error) => {
            let _ = send(fd.as_raw_fd(), libc::SIGKILL);
            let _ = wait_pid(child);
            Err(error)
        }
    }
}

fn pointers(words: &[CString]) -> Vec<*const libc::c_char> {
    words
        .iter()
        .map(|word| word.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect()
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
