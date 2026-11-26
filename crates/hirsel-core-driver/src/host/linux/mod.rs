//! The driver's Linux host mechanisms.
use std::{
    ffi::{CStr, OsString},
    fs::File,
    io,
    num::NonZeroUsize,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStringExt, net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

mod install;
#[cfg(test)]
mod report_laws;
pub(crate) mod spawn;

pub(crate) use install::{installed_executable, loaded_libraries};

pub(super) fn runtime_root(uid: libc::uid_t) -> Option<PathBuf> {
    Some(Path::new("/run/user").join(uid.to_string()))
}

#[cfg(target_arch = "x86_64")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::X86_64Linux;
#[cfg(target_arch = "aarch64")]
pub(crate) const HOST_TARGET: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Linux;

#[cfg(test)]
pub(crate) fn test_link(_runtime: &Path) -> crate::native::Link {
    crate::native::Link::Driver(
        PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
    )
}

#[cfg(test)]
pub(crate) fn test_compiler(_runtime: &Path) -> PathBuf {
    std::env::current_exe().unwrap()
}

/// The kit's Linux linker has both the ELF and Mach-O ports.
pub(crate) fn installation_linker(kit: &Path, _library: &Path) -> io::Result<PathBuf> {
    Ok(kit.join("ld.lld"))
}

pub(crate) const INSTALLED_LINKER_LIBRARY: Option<&str> = None;

/// A delegated cgroup v2 subtree can hold the launcher's children (launch.md).
pub(crate) const CGROUPS: bool = true;

/// Linux cannot run or verify an arm64 Darwin image.
#[cfg(test)]
pub(crate) const RUNS_DARWIN_IMAGES: bool = false;

/// Keep the checked image across an atomic replacement of its path.
pub(crate) fn release_command(file: &File, executable: &Path) -> Command {
    let mut command = Command::new(format!("/proc/self/fd/{}", file.as_raw_fd()));
    command.arg0(executable);
    command
}

/// Create `to` as a copy of `from`: a reflink (`FICLONE`) where the file
/// system shares extents, else a byte copy into the same new inode.
pub(crate) fn clone_file(from: &mut File, to: &Path) -> io::Result<()> {
    let mut to = File::create_new(to)?;
    // SAFETY: FICLONE borrows both descriptors for the call.
    if unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) } != 0 {
        io::copy(from, &mut to)?;
    }
    Ok(())
}

/// Reflink `from` into the empty `to` (`FICLONE`). False where the file system
/// shares no extents between them; `to` is then unchanged.
pub(crate) fn reflink(to: &File, from: &File) -> bool {
    // SAFETY: FICLONE reads two descriptors, both open for the call. A failed
    // clone leaves the destination unchanged.
    unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) == 0 }
}

/// Give the calling thread a descriptor table of its own, released at thread
/// exit, so no other thread's fork copies a descriptor it opens.
pub(crate) fn private_descriptors() -> io::Result<()> {
    // Docker's default seccomp profile blocks unshare. Use the close_range
    // syscall directly for the glibc 2.28 ABI floor.
    // SAFETY: close_range takes no pointers. The empty range keeps open
    // descriptors while the kernel gives this thread a private table.
    if unsafe {
        libc::syscall(
            libc::SYS_close_range,
            u32::MAX,
            u32::MAX,
            libc::CLOSE_RANGE_UNSHARE,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOSYS) {
            return Err(error);
        }
        // Kernels without close_range still support unshare.
        // SAFETY: CLONE_FILES takes no pointers and gives this thread a
        // private copy of the descriptor table.
        if unsafe { libc::unshare(libc::CLONE_FILES) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Have `command`'s child receive `signal` when this process dies, and refuse
/// to start it if this process is already gone.
pub(crate) fn die_with_parent(command: &mut Command, signal: i32) {
    let parent = std::process::id();
    // SAFETY: these syscalls only set this child's parent-death signal and
    // inspect its parent, and run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, signal) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() as u32 != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn peer_credentials(stream: &UnixStream) -> io::Result<libc::ucred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are live, sized for SO_PEERCRED.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        Ok(cred)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The result of setting a socket option. Linux sets one on a socket whose peer has
/// closed.
pub(crate) fn socket_option(set: io::Result<()>) -> io::Result<()> {
    set
}

/// The PID of the process at the other end of `stream`.
pub(crate) fn peer_pid(stream: &UnixStream) -> Option<libc::pid_t> {
    peer_credentials(stream).ok().map(|cred| cred.pid)
}

/// The effective UID of the process at the other end of `stream`.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    peer_credentials(stream).map(|cred| cred.uid)
}

/// A pipe whose ends are both close-on-exec from creation.
pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills two fresh descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are fresh and owned here.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// `recvmsg` on `fd`, every descriptor it receives close-on-exec from receipt.
/// # Safety
/// `header` describes live buffers, as `recvmsg` requires.
pub(crate) unsafe fn receive(fd: RawFd, header: &mut libc::msghdr) -> isize {
    // SAFETY: the caller's contract.
    unsafe { libc::recvmsg(fd, header, libc::MSG_CMSG_CLOEXEC) }
}

/// The CPUs this process may run on.
pub(crate) fn available_cpus() -> io::Result<NonZeroUsize> {
    let mut mask = vec![0usize; 16];
    loop {
        // SAFETY: The kernel can write up to the allocated byte length to mask.
        let status = unsafe {
            libc::sched_getaffinity(
                0,
                std::mem::size_of_val(mask.as_slice()),
                mask.as_mut_ptr().cast(),
            )
        };
        if status == 0 {
            let count = mask.iter().map(|word| word.count_ones() as usize).sum();
            return NonZeroUsize::new(count)
                .ok_or_else(|| io::Error::other("empty CPU affinity mask"));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
        // A fixed cpu_set_t can be too small for the kernel's CPU mask.
        mask.resize(mask.len() * 2, 0);
    }
}

/// Make this process adopt its orphaned descendants, so the launcher can reap
/// what a cgroup kill leaves.
pub(crate) fn own_orphans() -> io::Result<()> {
    // SAFETY: this prctl only changes who adopts this process's descendants.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The path through which this process reaches the open file `fd`.
pub(crate) fn fd_path(fd: &impl AsRawFd) -> io::Result<PathBuf> {
    Ok(PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd())))
}

/// Mark every descriptor above stderr close-on-exec. A kernel without
/// close_range (before 5.11) leaves them as they are.
pub fn close_inherited_descriptors() {
    // SAFETY: close_range only changes descriptor flags; it touches no memory.
    unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
}

/// The path through which another process (a linker this process runs) reads
/// the open file `fd`.
fn shared_path(fd: &impl AsRawFd) -> io::Result<PathBuf> {
    Ok(PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        fd.as_raw_fd()
    )))
}

/// The running executable. /proc/self/exe stays bound to the loaded inode
/// across replacement of its path.
pub(crate) fn current_image() -> io::Result<PathBuf> {
    Ok(PathBuf::from("/proc/self/exe"))
}

/// An anonymous file that, once sealed, no one can change: a sealed memfd,
/// read by other processes through `shared_path`.
pub(crate) struct SealedFile(File);

impl SealedFile {
    pub(crate) fn new(name: &CStr) -> io::Result<Self> {
        // SAFETY: the name is NUL-terminated and these flags take no other arguments.
        let fd = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a fresh owned descriptor.
        Ok(Self(unsafe { File::from_raw_fd(fd) }))
    }

    pub(crate) fn file(&self) -> &File {
        &self.0
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.0
    }

    /// Forbid every further write, resize and seal.
    pub(crate) fn seal(&mut self) -> io::Result<()> {
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        // SAFETY: the file is open and F_ADD_SEALS takes a single integer argument.
        if unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The path through which another process reads the file.
    pub(crate) fn path(&self) -> io::Result<PathBuf> {
        shared_path(&self.0)
    }
}

fn owned(fd: RawFd) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Open `name` in `parent` without following it: a directory, a file or the
/// link itself, as a path-only descriptor.
pub(crate) fn open_child(parent: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: the descriptor and the NUL-terminated component live through openat.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

/// The parent of the open directory `directory`.
pub(crate) fn open_parent(directory: &File) -> io::Result<File> {
    // SAFETY: directory is live and ".." is a static C string.
    owned(unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"..".as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })
}

/// The target of the link `link`, opened as `name` in `parent`.
pub(crate) fn link_target(_parent: &File, _name: &CStr, link: &File) -> io::Result<PathBuf> {
    let mut bytes = vec![0u8; 256];
    loop {
        // SAFETY: an empty name reads the opened link itself. The output buffer
        // is writable for the supplied length and the descriptor stays open.
        let n = unsafe {
            libc::readlinkat(
                link.as_raw_fd(),
                c"".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = n as usize;
        if n < bytes.len() {
            bytes.truncate(n);
            return Ok(OsString::from_vec(bytes).into());
        }
        bytes.resize(bytes.len() * 2, 0);
    }
}

/// Open the regular file `opened` (`name` in `parent`) for reading. The caller
/// checks that the result is the same file.
pub(crate) fn reopen(_parent: &File, _name: &CStr, opened: &File) -> io::Result<File> {
    File::open(format!("/proc/self/fd/{}", opened.as_raw_fd())).map_err(|error| {
        match error.kind() {
            // No /proc: the path-only descriptor cannot be reopened here.
            io::ErrorKind::NotFound => io::ErrorKind::Unsupported.into(),
            _ => error,
        }
    })
}

/// What a C driver links after the runtime archive for a program the launcher
/// laws build by hand: the C library's split-out parts.
#[cfg(test)]
pub(crate) const RUNTIME_LIBRARIES: &[&str] =
    &["-lgcc_s", "-lpthread", "-ldl", "-lm", "-lrt", "-lutil"];

/// A shell fragment that appends `<n> <kind>:[<id>]` to `$out` for each open
/// descriptor `$n` (launcher laws).
#[cfg(test)]
pub(crate) const FD_REPORT: &str =
    "[ -e /proc/$$/fd/$n ] && echo \"$n $(readlink /proc/$$/fd/$n)\" >> \"$out\"";

/// Process `pid` as the launcher laws watch it: `None` once it no longer exists,
/// `Some(true)` while it is a zombie its parent has not reaped, `Some(false)` while it runs.
#[cfg(test)]
pub(crate) fn zombie(pid: libc::pid_t) -> Option<bool> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        stat.rsplit_once(") ")
            .is_some_and(|(_, state)| state.starts_with('Z')),
    )
}

/// `<kind>:[<id>]` of this process's descriptor `fd`, as FD_REPORT writes it.
#[cfg(test)]
pub(crate) fn fd_report(fd: RawFd) -> String {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

/// This process's resident set in bytes (`/proc/self/statm`), 0 when it cannot be read.
pub(crate) fn resident_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        // SAFETY: sysconf reads a constant.
        .map_or(0, |pages| {
            pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64
        })
}

/// This process's umask, as the kernel reports it in `/proc/self/status`; reading
/// it changes nothing, so no other thread ever creates a file under another mask.
pub(crate) fn umask() -> io::Result<u32> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Umask:"))
        .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
        .ok_or_else(|| io::Error::other("/proc/self/status names no umask"))
}

/// Which count [`boot_clock`] reads: this boot (`/proc/sys/kernel/random/boot_id`)
/// and this time namespace (`/proc/self/ns/time`), whose offsets move
/// CLOCK_BOOTTIME. `None` without a well-formed pair.
pub(crate) fn boot_clock_identity() -> Option<String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok();
    let namespace = std::fs::read_link("/proc/self/ns/time")
        .ok()
        .and_then(|link| link.into_os_string().into_string().ok());
    crate::tool_inputs::clock_identity(boot.as_deref(), namespace.as_deref())
}

/// CLOCK_BOOTTIME in nanoseconds through `/proc/uptime` (10 ms resolution), as
/// the interval the true time lies in: never stepped, counts suspend.
pub(crate) fn boot_clock() -> Option<(u64, u64)> {
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let (seconds, hundredths) = uptime.split_whitespace().next()?.split_once('.')?;
    let at =
        seconds.parse::<u64>().ok()? * 1_000_000_000 + hundredths.parse::<u64>().ok()? * 10_000_000;
    Some((at, at + 10_000_000))
}

/// The text of process `pid`'s cgroup membership (`/proc/<pid>/cgroup`); this
/// process's own when `pid` is `None`.
pub(crate) fn cgroup_membership(pid: Option<libc::pid_t>) -> io::Result<String> {
    match pid {
        Some(pid) => std::fs::read_to_string(format!("/proc/{pid}/cgroup")),
        None => std::fs::read_to_string("/proc/self/cgroup"),
    }
}

/// The text of this process's mount table (`/proc/self/mountinfo`), where the cgroup v2
/// hierarchy is found.
pub(crate) fn mountinfo() -> io::Result<String> {
    std::fs::read_to_string("/proc/self/mountinfo")
}

/// The live children of process `pid` (its main thread's, which is every child
/// a single-threaded launcher or a test's child has).
pub(crate) fn child_pids(pid: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?;
    text.split_whitespace()
        .map(|child| {
            child
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a child pid"))
        })
        .collect()
}

/// A shell fragment that exits 24 unless the shell running it leads its own
/// session (launcher laws).
#[cfg(test)]
pub(crate) const SESSION_LEADER: &str = "read -r pid name state parent pgrp sid rest < /proc/$$/stat; test \"$sid\" = \"$$\" || exit 24";

/// The paths this process holds open, one entry per descriptor that names one.
#[cfg(test)]
pub(crate) fn open_paths() -> Vec<PathBuf> {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|fd| std::fs::read_link(fd.unwrap().path()).ok())
        .collect()
}

/// Process `pid`'s open descriptors, each as `<n> <what it names>`, sorted.
#[cfg(test)]
pub(crate) fn child_descriptors(pid: u32) -> Vec<String> {
    let mut open: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
        .map(|fd| {
            let target = std::fs::read_link(fd.path()).unwrap_or_default();
            format!("{} {}", fd.file_name().to_string_lossy(), target.display())
        })
        .collect();
    open.sort();
    open
}

/// The executable address range of process `pid`'s mapping of `image`.
#[cfg(test)]
pub(crate) fn child_code_range(pid: u32, image: &Path) -> std::ops::Range<u64> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let hex = |at: &str| u64::from_str_radix(at, 16).unwrap();
    let range = maps
        .lines()
        .filter(|line| line.ends_with(image.to_str().unwrap()))
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next()?;
            fields.next()?.contains('x').then_some(range)
        })
        .unwrap();
    let (start, end) = range.split_once('-').unwrap();
    hex(start)..hex(end)
}

/// How many times `needle` appears in process `pid`'s readable memory. The
/// scan must cover more than 1 MiB, or it proves nothing.
#[cfg(test)]
pub(crate) fn child_memory_hits(pid: u32, needle: &[u8]) -> usize {
    use std::os::unix::fs::FileExt;
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    let mem = File::open(format!("/proc/{pid}/mem"))
        .expect("the test must be able to read its child's memory (ptrace)");
    let (mut hits, mut scanned) = (0, 0);
    for map in maps.lines() {
        let mut fields = map.split_whitespace();
        let (range, perms) = (fields.next().unwrap(), fields.next().unwrap());
        if !perms.starts_with('r') {
            continue;
        }
        let (start, end) = range.split_once('-').unwrap();
        let start = u64::from_str_radix(start, 16).unwrap();
        let end = u64::from_str_radix(end, 16).unwrap();
        let mut region = vec![0; (end - start) as usize];
        // [vvar] and similar special mappings refuse reads.
        if mem.read_exact_at(&mut region, start).is_err() {
            continue;
        }
        scanned += region.len();
        hits += region
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count();
    }
    assert!(scanned > 1 << 20, "scanned only {scanned} bytes");
    hits
}
