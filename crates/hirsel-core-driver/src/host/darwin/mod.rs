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

pub(crate) use install::{
    INSTALLED_LINKER_LIBRARY, installation_linker, installed_executable, loaded_libraries,
};

/// Darwin has no cgroups: the launcher runs without a delegated subtree.
pub(crate) const CGROUPS: bool = false;

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
    std::env::current_exe()
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
