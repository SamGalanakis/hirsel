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

/// What a started child's executed image needs kept until the child is reaped.
#[cfg(target_os = "macos")]
pub(super) type Image = Option<PrivateImage>;
#[cfg(target_os = "linux")]
pub(super) type Image = ();

fn cstring(word: &OsStr) -> io::Result<CString> {
    CString::new(word.as_encoded_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// macOS has no clone3, pidfds, cgroups, close_range or execveat. The child
/// is forked, closes the descriptors the parent listed just before the fork
/// (later ones are close-on-exec), and executes a pinned image by the path
/// of a private copy (see [`PrivateImage`]); the parent pins it with a kqueue
/// NOTE_EXIT watch in place of a pidfd.
pub(super) fn start(
    command: &Command,
    workdir: &Path,
    slots: &[(OwnedFd, RawFd)],
    cgroup: Option<&File>,
    serving: bool,
) -> io::Result<(libc::pid_t, OwnedFd, Image)> {
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
    // A launched run ends with the launcher, as Linux PR_SET_PDEATHSIG ends it.
    #[cfg(target_os = "macos")]
    environment.insert(
        hirsel_core_ids::launch::PARENT_DEATH_ENV.into(),
        std::process::id().to_string().into(),
    );
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
        .custom_flags(hirsel_core_net::sys::O_PATH | libc::O_DIRECTORY)
        .open(workdir)?;
    #[cfg_attr(target_os = "linux", allow(unused_mut))]
    let mut raw: Vec<_> = slots
        .iter()
        .map(|(fd, slot)| (fd.as_raw_fd(), *slot))
        .collect();
    let image = raw.iter().any(|&(_, target)| target == IMAGE_FD);
    #[cfg(target_os = "macos")]
    let private = match raw.iter_mut().find(|(_, target)| *target == IMAGE_FD) {
        Some((source, _)) => {
            let copy = PrivateImage::copy(*source)?;
            *source = copy.file.as_raw_fd();
            Some(copy)
        }
        None => None,
    };
    #[cfg(target_os = "macos")]
    let image_path = private.as_ref().map(|copy| &copy.path);
    let parent = std::process::id() as libc::pid_t;
    let (mut errors, report) = error_pipe()?;
    #[cfg(target_os = "macos")]
    let open = {
        if cgroup.is_some() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        open_descriptors()?
    };
    let mut child_fd = -1;
    let child = fork_child(cgroup, &mut child_fd)?;
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
                #[cfg(target_os = "macos")]
                image_path,
                #[cfg(target_os = "macos")]
                &open,
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
    #[cfg(target_os = "linux")]
    let private = ();
    match result {
        Ok(0) => Ok((child, fd, private)),
        Ok(n) => {
            let result = errors.read_exact(&mut bytes[n..]);
            let _ = wait_pid(child);
            result?;
            Err(io::Error::from_raw_os_error(i32::from_ne_bytes(bytes)))
        }
        Err(error) => {
            let _ = send(child, fd.as_raw_fd(), libc::SIGKILL);
            let _ = wait_pid(child);
            Err(error)
        }
    }
}

/// Fork the child: into `cgroup` and with a pidfd in `child_fd` where clone3 can.
#[cfg(target_os = "linux")]
fn fork_child(cgroup: Option<&File>, child_fd: &mut RawFd) -> io::Result<libc::pid_t> {
    const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;
    // Linux clone_args version 2 ends with the cgroup directory descriptor.
    let mut args = [0u64; 11];
    args[0] = libc::CLONE_PIDFD as u64;
    args[1] = std::ptr::from_mut(child_fd) as u64;
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
    Ok(child)
}

#[cfg(target_os = "macos")]
fn fork_child(_cgroup: Option<&File>, _child_fd: &mut RawFd) -> io::Result<libc::pid_t> {
    // SAFETY: the child calls only async-signal-safe functions on data
    // prepared before the fork, then executes or exits.
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(child)
}

/// macOS executes only by path, and the verified image's path could be
/// replaced after its check. So the image is copied from its verified
/// descriptor into a fresh private directory (0700) that no other launch can
/// name; the child gets the copy as descriptor 6 and executes it, so the run's
/// identity check compares the running image with that same file. dyld reads
/// the image by path as the child starts, so the caller keeps the copy until
/// it reaps the child.
#[cfg(target_os = "macos")]
pub(super) struct PrivateImage {
    dir: std::path::PathBuf,
    path: CString,
    file: OwnedFd,
}

#[cfg(target_os = "macos")]
impl PrivateImage {
    fn copy(source: RawFd) -> io::Result<Self> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{FileExt, PermissionsExt};
        let dir = tempfile::Builder::new()
            .prefix("hirsel-launch-")
            .tempdir()?
            .keep();
        let path = dir.join("image");
        let copy = (|| {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o500)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            // SAFETY: a borrowed duplicate; reads at offsets leave the source's own.
            let input = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(source) });
            let mut block = vec![0; 1 << 20];
            let mut at = 0;
            loop {
                let n = input.read_at(&mut block, at)?;
                if n == 0 {
                    break;
                }
                out.write_all(&block[..n])?;
                at += n as u64;
            }
            out.sync_all()?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?;
            Ok::<_, io::Error>(above_launch_slots(file.as_raw_fd())?)
        })();
        let path_c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        match copy {
            Ok(file) => Ok(Self {
                dir,
                path: path_c,
                file,
            }),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(error)
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for PrivateImage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// This process's open descriptors, for the child to close without close_range.
#[cfg(target_os = "macos")]
fn open_descriptors() -> io::Result<Vec<RawFd>> {
    let entry = size_of::<libc::proc_fdinfo>();
    let mut room = 256;
    loop {
        // SAFETY: proc_fdinfo is two integers; zeroed entries are valid.
        let mut fds: Vec<libc::proc_fdinfo> = vec![unsafe { std::mem::zeroed() }; room];
        // SAFETY: the buffer holds `room` entries for the kernel to fill.
        let filled = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                (room * entry) as i32,
            )
        };
        if filled <= 0 {
            return Err(io::Error::last_os_error());
        }
        let count = filled as usize / entry;
        if count < room {
            return Ok(fds[..count].iter().map(|info| info.proc_fd).collect());
        }
        room *= 4;
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
    if unsafe { hirsel_core_net::sys::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the pipe ends are fresh descriptors.
    let (read, write) = unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((read, above_launch_slots(write.as_raw_fd())?))
}

/// Close `first..=last`, or every listed descriptor in that range on macOS.
///
/// # Safety
/// Runs in the forked child: only syscalls.
unsafe fn close_from(
    first: RawFd,
    last: RawFd,
    #[cfg(target_os = "macos")] open: &[RawFd],
) -> bool {
    // SAFETY: closing this fork's own descriptors.
    unsafe {
        #[cfg(target_os = "linux")]
        return libc::syscall(libc::SYS_close_range, first as u32, last as u32, 0u32) == 0;
        #[cfg(target_os = "macos")]
        {
            for &fd in open {
                if (first..=last).contains(&fd) {
                    libc::close(fd);
                }
            }
            true
        }
    }
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
    #[cfg(target_os = "macos")] image_path: Option<&CString>,
    #[cfg(target_os = "macos")] open: &[RawFd],
) -> ! {
    // SAFETY: this runs after clone in the child. The pointers and fd sources
    // were prepared by the parent. Every operation here is async-signal-safe.
    unsafe {
        #[cfg(target_os = "linux")]
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
            failed(report, *hirsel_core_net::sys::errno_location());
        }
        if libc::setsid() < 0
            || libc::fchdir(cwd) != 0
            || libc::signal(libc::SIGPIPE, libc::SIG_DFL) == libc::SIG_ERR
        {
            failed(report, *hirsel_core_net::sys::errno_location());
        }
        if libc::getppid() != parent {
            failed(report, libc::ESRCH);
        }
        for &(source, target) in fds {
            if libc::dup2(source, target) < 0 {
                failed(report, *hirsel_core_net::sys::errno_location());
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
        if report != report_slot
            && hirsel_core_net::sys::dup3(report, report_slot, libc::O_CLOEXEC) < 0
        {
            failed(report, *hirsel_core_net::sys::errno_location());
        }
        if image
            && !serving
            && !close_from(
                3,
                5,
                #[cfg(target_os = "macos")]
                open,
            )
        {
            failed(report_slot, *hirsel_core_net::sys::errno_location());
        }
        if !close_from(
            report_slot + 1,
            RawFd::MAX,
            #[cfg(target_os = "macos")]
            open,
        ) {
            failed(report_slot, *hirsel_core_net::sys::errno_location());
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
        #[cfg(target_os = "linux")]
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
        // The pinned image runs by the path it was opened at; descriptor 6
        // still holds it for the run's identity check.
        #[cfg(target_os = "macos")]
        libc::execve(
            image_path.map_or(program.as_ptr(), |path| path.as_ptr()),
            argv.as_ptr(),
            env.as_ptr(),
        );
        failed(report_slot, *hirsel_core_net::sys::errno_location());
    }
}

unsafe fn failed(report: RawFd, errno: i32) -> ! {
    let bytes = errno.to_ne_bytes();
    // SAFETY: the report pipe is live and the four-byte write is atomic.
    unsafe {
        while libc::write(report, bytes.as_ptr().cast(), bytes.len()) < 0
            && *hirsel_core_net::sys::errno_location() == libc::EINTR
        {}
        libc::_exit(127);
    }
}
