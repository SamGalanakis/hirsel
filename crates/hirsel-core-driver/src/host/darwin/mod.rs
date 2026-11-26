//! The driver's Darwin host mechanisms. Darwin has no memfd, no `/proc`, no
//! cgroups and no parent-death signal; nothing here forks.
use std::{
    ffi::{CStr, CString, OsString},
    fs::File,
    io,
    num::NonZeroUsize,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    process::Command,
};

mod install;
pub(crate) mod spawn;

#[cfg(test)]
pub(crate) use install::{test_compiler, test_link};

pub(crate) use install::{
    INSTALLED_LINKER_LIBRARY, installation_linker, installed_executable, loaded_libraries,
};

pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;

/// Darwin has no cgroups: the launcher runs without a delegated subtree.
pub(crate) const CGROUPS: bool = false;

/// A Mac runs and verifies the arm64 Darwin images a test links.
#[cfg(test)]
pub(crate) const RUNS_DARWIN_IMAGES: bool = true;

pub(crate) fn release_command(_file: &File, executable: &Path) -> Command {
    Command::new(executable)
}

/// Create `to` as a copy of `from`: an APFS clone (`fclonefileat`), else a
/// byte copy into a new file, as Linux does without reflinks.
pub(crate) fn clone_file(from: &mut File, to: &Path) -> io::Result<()> {
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    let directory = File::open(parent)?;
    let name = CString::new(name.as_bytes())?;
    // SAFETY: both descriptors and the name stay live for the call; the clone
    // creates `name` and refuses one that exists.
    let rc =
        unsafe { libc::fclonefileat(from.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EEXIST) {
        return Err(error);
    }
    let mut to = File::create_new(to)?;
    io::copy(from, &mut to)?;
    Ok(())
}

/// Darwin clones a file only into a new name (`clone_file`), never into an
/// open one, so `fill` copies.
pub(crate) fn reflink(_to: &File, _from: &File) -> bool {
    false
}

/// Nothing to do: nothing in the driver forks on Darwin. Children start with
/// `posix_spawn` (the launcher with `POSIX_SPAWN_CLOEXEC_DEFAULT`, `Command`
/// with its own spawn), and every descriptor the driver opens is close-on-exec.
pub(crate) fn private_descriptors() -> io::Result<()> {
    Ok(())
}

/// Darwin has no parent-death signal; a Hirsel child watches its parent itself
/// (`HIRSEL_HOST_PID`). Setting no pre-exec hook keeps `Command` on `posix_spawn`.
pub(crate) fn die_with_parent(_command: &mut Command, _signal: i32) {}

const SOL_LOCAL: libc::c_int = 0;
const LOCAL_PEERPID: libc::c_int = 0x002;

/// The PID of the process at the other end of `stream`.
pub(crate) fn peer_pid(stream: &UnixStream) -> Option<libc::pid_t> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: pid and len are live, sized for LOCAL_PEERPID.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            SOL_LOCAL,
            LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(pid)
}

/// The effective UID of the process at the other end of `stream`.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes two IDs for a connected socket.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

fn close_on_exec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a live descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A pipe whose ends are both close-on-exec. Darwin has no `pipe2`; no fork
/// can copy the ends before the flag is set, since nothing here forks.
pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe fills two fresh descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are fresh and owned here.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    close_on_exec(ends.0.as_raw_fd())?;
    close_on_exec(ends.1.as_raw_fd())?;
    Ok(ends)
}

/// `recvmsg` on `fd`, then every descriptor it received made close-on-exec.
/// Darwin has no `MSG_CMSG_CLOEXEC`.
/// # Safety
/// `header` describes live buffers, as `recvmsg` requires.
pub(crate) unsafe fn receive(fd: RawFd, header: &mut libc::msghdr) -> isize {
    // SAFETY: the caller's contract; the control walk stays inside the
    // buffer recvmsg filled.
    unsafe {
        let got = libc::recvmsg(fd, header, 0);
        if got > 0 {
            let mut cmsg = libc::CMSG_FIRSTHDR(header);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                    let n = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    for i in 0..n {
                        let _ = close_on_exec(data.add(i).read_unaligned());
                    }
                }
                cmsg = libc::CMSG_NXTHDR(header, cmsg);
            }
        }
        got
    }
}

/// The CPUs this process may run on. Darwin has no affinity mask.
pub(crate) fn available_cpus() -> io::Result<NonZeroUsize> {
    std::thread::available_parallelism()
}

/// Never called: without cgroups the launcher adopts no orphans.
pub(crate) fn own_orphans() -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The path of the open file `fd` (`F_GETPATH`): where it is now, resolved
/// again by whoever opens it.
pub(crate) fn fd_path(fd: &impl AsRawFd) -> io::Result<PathBuf> {
    let mut buffer = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes, NUL included.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&buffer).map_err(|_| io::ErrorKind::InvalidData)?;
    Ok(PathBuf::from(OsString::from_vec(path.to_bytes().to_vec())))
}

/// Mark every descriptor above stderr close-on-exec. Darwin has no
/// `close_range`; `/dev/fd` lists this process's open descriptors.
pub fn close_inherited_descriptors() {
    let Ok(listing) = std::fs::read_dir("/dev/fd") else {
        return;
    };
    let open: Vec<RawFd> = listing
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|&fd| fd > 2)
        .collect();
    for fd in open {
        // SAFETY: F_SETFD only changes a descriptor flag; a descriptor the
        // listing itself held is already closed and fails harmlessly.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

/// The running executable's path.
pub(crate) fn current_image() -> io::Result<PathBuf> {
    crate::installation::compiler_executable()
}

/// A file that, once sealed, no one can change: Darwin has no memfd, so it
/// lives in a private 0700 directory, is made read-only when sealed and is
/// kept only through a read-only descriptor. The directory goes with it.
pub(crate) struct SealedFile {
    file: File,
    directory: PathBuf,
    path: PathBuf,
}

impl SealedFile {
    pub(crate) fn new(name: &CStr) -> io::Result<Self> {
        let mut template = std::env::temp_dir()
            .join("hirsel-sealed-XXXXXX")
            .into_os_string()
            .into_vec();
        template.push(0);
        // SAFETY: the template is NUL-terminated and writable.
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        let directory = PathBuf::from(OsString::from_vec(template));
        let path = directory.join(OsString::from_vec(name.to_bytes().to_vec()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path);
        match file {
            Ok(file) => Ok(Self {
                file,
                directory,
                path,
            }),
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                Err(error)
            }
        }
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Make the file read-only and drop the only writable descriptor.
    pub(crate) fn seal(&mut self) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        self.file
            .set_permissions(std::fs::Permissions::from_mode(0o400))?;
        let position = std::io::Seek::stream_position(&mut self.file)?;
        let mut sealed = File::open(&self.path)?;
        std::io::Seek::seek(&mut sealed, std::io::SeekFrom::Start(position))?;
        self.file = sealed;
        Ok(())
    }

    /// The path through which another process reads the file.
    pub(crate) fn path(&self) -> io::Result<PathBuf> {
        Ok(self.path.clone())
    }
}

impl Drop for SealedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
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
/// link itself, for events only, so opening has no effect on a device or FIFO.
pub(crate) fn open_child(parent: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: the descriptor and the NUL-terminated component live through openat.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_EVTONLY | libc::O_SYMLINK | libc::O_NONBLOCK | libc::O_CLOEXEC,
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
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })
}

/// The target of the link `name` in `parent`. Darwin reads a link only by name.
pub(crate) fn link_target(parent: &File, name: &CStr, _link: &File) -> io::Result<PathBuf> {
    let mut bytes = vec![0u8; 256];
    loop {
        // SAFETY: the output buffer is writable for the supplied length and
        // the descriptor and name stay live.
        let n = unsafe {
            libc::readlinkat(
                parent.as_raw_fd(),
                name.as_ptr(),
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

/// Open the regular file `name` in `parent` for reading, without following a
/// link. The caller checks that it is the file it walked to.
pub(crate) fn reopen(parent: &File, name: &CStr, _opened: &File) -> io::Result<File> {
    // SAFETY: the descriptor and the NUL-terminated component live through openat.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    })
}

/// What a C driver links after the runtime archive for a program the launcher
/// laws build by hand: the frameworks besides libSystem that the runtime imports.
#[cfg(test)]
pub(crate) const RUNTIME_LIBRARIES: &[&str] =
    &["-framework", "Security", "-framework", "CoreFoundation"];

/// A shell fragment that appends `<n> <kind>:[<inode>]` to `$out` for each
/// open descriptor `$n` (launcher laws); a pipe is a FIFO here.
#[cfg(test)]
pub(crate) const FD_REPORT: &str = "[ -e /dev/fd/$n ] && stat -L -f '%HT %i' /dev/fd/$n | { read k i; k=$(echo \"$k\" | tr A-Z a-z); [ \"$k\" = fifo ] && k=pipe; echo \"$n $k:[$i]\" >> \"$out\"; }";

/// Process `pid` as the launcher laws watch it: `None` once it no longer exists,
/// `Some(true)` while it is a zombie its parent has not reaped, `Some(false)` while it runs.
#[cfg(test)]
pub(crate) fn zombie(pid: libc::pid_t) -> Option<bool> {
    // kill(pid, 0) finds a zombie too; proc_pidinfo gives its BSD status (SZOMB 5),
    // and refuses a zombie on some releases, which reads as a zombie here.
    const SZOMB: u32 = 5;
    let exists = unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !exists {
        return None;
    }
    // SAFETY: an all-zero proc_bsdinfo is a valid output record; proc_pidinfo fills it.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    Some(read != size || info.pbi_status == SZOMB)
}

/// `<kind>:[<inode>]` of this process's descriptor `fd`, as FD_REPORT writes it.
#[cfg(test)]
pub(crate) fn fd_report(fd: RawFd) -> String {
    // SAFETY: an all-zero stat is a valid output record; fstat writes one.
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(fd, &mut status) }, 0);
    let kind = match status.st_mode & libc::S_IFMT {
        libc::S_IFSOCK => "socket",
        libc::S_IFIFO => "pipe",
        _ => "file",
    };
    format!("{kind}:[{}]", status.st_ino)
}

/// This process's physical footprint in bytes (`proc_pid_rusage`'s
/// `ri_phys_footprint`), 0 when it cannot be read. The footprint, not the
/// resident size, is the measure a memory cap needs: on macOS 26.6 the pages
/// mimalloc returns (`MADV_FREE_REUSABLE`) leave the footprint at once but stay
/// resident until the kernel wants them, so a resident-size cap never fits.
pub(crate) fn resident_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: RUSAGE_INFO_V4 writes one rusage_info_v4 into the buffer.
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            usage.as_mut_ptr().cast(),
        )
    };
    if rc != 0 {
        return 0;
    }
    // SAFETY: the call succeeded, so the whole struct is written.
    unsafe { usage.assume_init() }.ri_phys_footprint
}

/// This process's umask. Darwin has no call that only reads it, so this sets
/// the mask and restores it at once; a file another thread creates in between
/// is created under 0o077.
pub(crate) fn umask() -> io::Result<u32> {
    // SAFETY: umask only swaps the process's mask; the old one is restored.
    let mask = unsafe {
        let mask = libc::umask(0o077);
        libc::umask(mask);
        mask
    };
    Ok(u32::from(mask))
}

/// Which count [`boot_clock`] reads: this boot, named by
/// `kern.bootsessionuuid`, a UUID the kernel draws at each boot. Darwin has no
/// time namespace. `None` unless the kernel gives a 36-character UUID.
pub(crate) fn boot_clock_identity() -> Option<String> {
    let mut uuid = [0u8; 64];
    let mut len = uuid.len();
    // SAFETY: the name is NUL-terminated and the kernel writes at most `len` bytes.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            uuid.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let uuid = CStr::from_bytes_until_nul(&uuid[..len])
        .ok()?
        .to_str()
        .ok()?;
    let well_formed = uuid.len() == 36
        && uuid.char_indices().all(|(at, c)| {
            if matches!(at, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    well_formed.then(|| format!("darwin-boot:{uuid}"))
}

/// Darwin's CLOCK_MONOTONIC in nanoseconds (microsecond resolution), as the
/// interval the true time lies in: it counts from boot, keeps counting while
/// the machine sleeps and is never stepped (`clock_gettime(3)`).
pub(crate) fn boot_clock() -> Option<(u64, u64)> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return None;
    }
    let at = u64::try_from(now.tv_sec).ok()? * 1_000_000_000 + u64::try_from(now.tv_nsec).ok()?;
    Some((at, at + 1_000))
}

/// Darwin has no cgroups, so no process is a member of one.
pub(crate) fn cgroup_membership(_pid: Option<libc::pid_t>) -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Darwin has no cgroups",
    ))
}

/// The live children of process `pid` (`proc_listchildpids`).
pub(crate) fn child_pids(pid: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let mut children = vec![0 as libc::c_int; 4096];
    let bytes = (children.len() * size_of::<libc::c_int>()) as libc::c_int;
    // SAFETY: the buffer is live and writable for `bytes`.
    let got = unsafe { libc::proc_listchildpids(pid, children.as_mut_ptr().cast(), bytes) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    children.truncate(got as usize);
    children.retain(|child| *child > 0);
    Ok(children)
}

/// A shell fragment that exits 24 unless the shell running it leads its own
/// session (launcher laws): `ps` marks a session leader with `s`.
#[cfg(test)]
pub(crate) const SESSION_LEADER: &str = "case $(ps -o stat= -p $$) in *s*) ;; *) exit 24 ;; esac";

/// The paths this process holds open, one entry per descriptor that names one.
#[cfg(test)]
pub(crate) fn open_paths() -> Vec<PathBuf> {
    let fds: Vec<RawFd> = std::fs::read_dir("/dev/fd")
        .unwrap()
        .filter_map(|fd| fd.unwrap().file_name().to_str()?.parse().ok())
        .collect();
    fds.into_iter()
        // SAFETY: F_GETPATH only reads the descriptor; one closed since the
        // listing (the listing's own) fails with EBADF.
        .filter_map(|fd| fd_path(&unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }).ok())
        .collect()
}

/// Process `pid`'s open descriptors, each as `<n> <kind>`, sorted; a socket
/// reads `socket:`, as Linux names it.
#[cfg(test)]
pub(crate) fn child_descriptors(pid: u32) -> Vec<String> {
    let size = size_of::<libc::proc_fdinfo>() as libc::c_int;
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        4096
    ];
    // SAFETY: the buffer is live and writable for its whole length.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            size * fds.len() as libc::c_int,
        )
    };
    assert!(got >= 0, "{}", io::Error::last_os_error());
    fds.truncate((got / size) as usize);
    let mut open: Vec<String> = fds
        .iter()
        .map(|fd| match fd.proc_fdtype as libc::c_int {
            libc::PROX_FDTYPE_SOCKET => format!("{} socket:", fd.proc_fd),
            kind => format!("{} kind:{kind}", fd.proc_fd),
        })
        .collect();
    open.sort();
    open
}

/// `hirsel prof attach` refuses on macOS (it needs `task_for_pid`), so a
/// Darwin test that needs a child's code range has no capture to compare.
#[cfg(test)]
pub(crate) fn child_code_range(_pid: u32, _image: &Path) -> std::ops::Range<u64> {
    panic!("hirsel prof attach refuses on macOS; no attach capture to compare")
}

/// Reading another process's memory on macOS needs `task_for_pid`, which a
/// test process is not entitled to.
#[cfg(test)]
pub(crate) fn child_memory_hits(_pid: u32, _needle: &[u8]) -> usize {
    panic!("reading a child's memory on macOS needs task_for_pid, which tests are not entitled to")
}
