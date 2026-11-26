//! Small host calls on Darwin: errno, notices, sockets, peer credentials,
//! randomness, a retry timer and the parent-exit arm.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

/// This thread's errno.
#[inline(always)]
pub(crate) fn errno() -> *mut libc::c_int {
    // SAFETY: the location is this thread's own errno.
    unsafe { libc::__error() }
}

/// The value of a terminal's special character that is switched off.
pub(crate) const VDISABLE: libc::cc_t = libc::_POSIX_VDISABLE;

fn owned(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn set_flags(fd: RawFd, descriptor: libc::c_int, status: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl on a live descriptor with integer arguments.
    unsafe {
        if descriptor != 0 && libc::fcntl(fd, libc::F_SETFD, descriptor) < 0 {
            return Err(io::Error::last_os_error());
        }
        if status != 0 {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | status) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// A one-way wakeup a writer posts and one reader takes: a pipe whose write
/// end never blocks, so a signal handler can always post.
pub(crate) struct Notice {
    read: OwnedFd,
    write: OwnedFd,
}

impl Notice {
    /// `nonblocking` makes [`take`] on an empty notice fail with EAGAIN.
    pub(crate) fn new(nonblocking: bool) -> io::Result<Self> {
        let mut ends = [-1; 2];
        // SAFETY: pipe writes exactly two new descriptors.
        if unsafe { libc::pipe(ends.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let notice = Self {
            read: owned(ends[0])?,
            write: owned(ends[1])?,
        };
        let read_status = if nonblocking { libc::O_NONBLOCK } else { 0 };
        set_flags(notice.read.as_raw_fd(), libc::FD_CLOEXEC, read_status)?;
        set_flags(notice.write.as_raw_fd(), libc::FD_CLOEXEC, libc::O_NONBLOCK)?;
        Ok(notice)
    }

    pub(crate) fn reader(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    pub(crate) fn writer(&self) -> RawFd {
        self.write.as_raw_fd()
    }
}

/// Post one wakeup to a notice's writer: one async-signal-safe write. A full
/// pipe is already readable, so a failed post loses nothing.
pub(crate) fn post(writer: RawFd) -> isize {
    // SAFETY: one readable byte for this one write.
    unsafe { libc::write(writer, [1u8].as_ptr().cast(), 1) }
}

/// Take posted wakeups from a notice's reader. Wakeups beyond one read's
/// buffer stay readable and cost the reader one more turn.
pub(crate) fn take(reader: RawFd) -> isize {
    let mut drained = [0u8; 64];
    // SAFETY: the buffer is writable for its length.
    unsafe { libc::read(reader, drained.as_mut_ptr().cast(), drained.len()) }
}

/// A close-on-exec, nonblocking socket that never raises SIGPIPE, or -1
/// with errno.
pub(crate) fn socket(domain: libc::c_int, kind: libc::c_int) -> isize {
    // SAFETY: success returns one fresh descriptor.
    let fd = unsafe { libc::socket(domain, kind, 0) };
    finish_socket(fd)
}

fn finish_socket(fd: libc::c_int) -> isize {
    if fd < 0 {
        return -1;
    }
    // SAFETY: a fresh descriptor this call owns.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    // SAFETY: an int option on a live socket.
    let nosigpipe = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const one).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if nosigpipe < 0 || set_flags(fd, libc::FD_CLOEXEC, libc::O_NONBLOCK).is_err() {
        // Dropping `owned` closes fd; keep the first failure's errno.
        let saved = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        drop(owned);
        // SAFETY: this thread's errno.
        unsafe { *errno() = saved };
        return -1;
    }
    use std::os::fd::IntoRawFd;
    owned.into_raw_fd() as isize
}

/// Accept one close-on-exec, nonblocking connection, or -1 with errno.
///
/// # Safety
/// `address` and `len` are null or a writable address and its length.
pub(crate) unsafe fn accept(
    fd: RawFd,
    address: *mut libc::sockaddr,
    len: *mut libc::socklen_t,
) -> isize {
    // SAFETY: by the contract.
    finish_socket(unsafe { libc::accept(fd, address, len) })
}

/// Darwin has no socket option naming a protocol; a stream socket of an
/// Internet family is TCP.
pub(crate) const PROTOCOL: Option<libc::c_int> = None;

/// The address family of the socket `fd`.
pub(crate) fn socket_domain(fd: RawFd) -> io::Result<libc::c_int> {
    // SAFETY: integer-only address storage, sized by len.
    let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: address and len are live and sized for the call.
    if unsafe { libc::getsockname(fd, (&raw mut address).cast(), &mut len) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(libc::c_int::from(address.ss_family))
}

/// Whether the socket `fd` is listening. Darwin's `getsockopt` refuses
/// `SO_ACCEPTCONN` with ENOPROTOOPT, so the flag is read from the socket's
/// `so_options` in `proc_pidfdinfo`'s `socket_fdinfo` (792 bytes, options
/// at byte 188 in the macOS 26 SDK).
pub(crate) fn listening(fd: RawFd) -> io::Result<bool> {
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKET_FDINFO: usize = 792;
    const SOI_OPTIONS: usize = 188;
    let mut info = [0u8; SOCKET_FDINFO];
    // SAFETY: info is live and writable for its length.
    let got = unsafe {
        libc::proc_pidfdinfo(
            libc::getpid(),
            fd,
            PROC_PIDFDSOCKETINFO,
            info.as_mut_ptr().cast(),
            SOCKET_FDINFO as libc::c_int,
        )
    };
    if got < 0 || got as usize != SOCKET_FDINFO {
        return Err(io::Error::last_os_error());
    }
    let options = i16::from_ne_bytes([info[SOI_OPTIONS], info[SOI_OPTIONS + 1]]);
    Ok(options as libc::c_int & libc::SO_ACCEPTCONN != 0)
}

/// Receive one packet from the datagram socket `fd` into `room` bytes at
/// `data`, cutting a longer one. Returns the packet's whole length, or -1
/// with errno.
///
/// Darwin's `recvmsg` returns the bytes it copied, never the whole length
/// of a cut packet, so the length comes from `SO_NREAD`, which is the size
/// of the first queued packet. The run is the socket's only receiver, so a
/// queued packet stays first until this call takes it. A size of 0 is an
/// empty packet or no packet: a peek of no bytes tells them apart, and the
/// size is read again because a packet can arrive between the two calls.
///
/// # Safety
/// `data` is writable for `room` bytes, or `room` is 0.
pub(crate) unsafe fn receive_packet(fd: RawFd, data: *mut u8, room: usize) -> isize {
    let mut len = first_packet(fd);
    if len == 0 {
        // SAFETY: an empty iovec is never written.
        if unsafe { receive(fd, std::ptr::null_mut(), 0, libc::MSG_PEEK) } < 0 {
            return -1;
        }
        len = first_packet(fd);
    }
    if len < 0 {
        return -1;
    }
    // SAFETY: by the contract.
    let copied = unsafe { receive(fd, data, room, 0) };
    if copied < 0 { -1 } else { len.max(copied) }
}

/// The size of the first packet queued on `fd`, 0 when none is, or -1 with
/// errno.
fn first_packet(fd: RawFd) -> isize {
    let mut size: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: size and len are live, sized for SO_NREAD.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_NREAD,
            (&raw mut size).cast(),
            &mut len,
        )
    };
    if rc < 0 { -1 } else { size as isize }
}

/// One `recvmsg` with no address or control area.
///
/// # Safety
/// `data` is writable for `room` bytes, or `room` is 0.
unsafe fn receive(fd: RawFd, data: *mut u8, room: usize, flags: libc::c_int) -> isize {
    let mut iov = libc::iovec {
        iov_base: data.cast(),
        iov_len: room,
    };
    // SAFETY: integer/pointer-only header.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // SAFETY: by the contract.
    unsafe { libc::recvmsg(fd, &mut msg, flags) }
}

/// The UID of the process at the other end of a connected Unix stream.
pub(crate) fn peer_uid(fd: RawFd) -> Option<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: both outputs are live.
    (unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0).then_some(uid)
}

/// Darwin reports a peer's shutdown as readable end of stream; there is no
/// separate poll bit.
pub(crate) const PEER_CLOSED: libc::c_short = 0;

/// A [`random`] flag. Darwin's pool is seeded before user space runs, so no
/// draw waits and the flag has no effect.
pub(crate) const RANDOM_NOWAIT: u32 = 1;

/// Fill a prefix of `len` bytes at `data` from the kernel's pool, returning
/// its length, or -1 with errno. One `getentropy` call fills at most 256.
///
/// # Safety
/// `data` is writable for `len` bytes.
pub(crate) unsafe fn random(data: *mut u8, len: usize, _flags: u32) -> isize {
    let len = len.min(256);
    // SAFETY: by the contract, for the clamped length.
    if unsafe { libc::getentropy(data.cast(), len) } < 0 {
        return -1;
    }
    len as isize
}

/// A descriptor that turns readable every millisecond until [`tick`]: a
/// kqueue holding one periodic timer.
pub(crate) fn timer() -> io::Result<OwnedFd> {
    // SAFETY: success returns one fresh descriptor.
    let kq = owned(unsafe { libc::kqueue() })?;
    set_flags(kq.as_raw_fd(), libc::FD_CLOEXEC, 0)?;
    let change = libc::kevent {
        ident: 1,
        filter: libc::EVFILT_TIMER,
        flags: libc::EV_ADD,
        fflags: 0,
        data: 1, // milliseconds
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change record and no output.
    let rc = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(kq)
}

/// Consume a [`timer`]'s expirations.
pub(crate) fn tick(fd: RawFd) -> isize {
    // SAFETY: an all-zero kevent is a valid output slot.
    let mut fired: libc::kevent = unsafe { std::mem::zeroed() };
    let now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: one writable output record and a zero timeout.
    unsafe { libc::kevent(fd, std::ptr::null(), 0, &mut fired, 1, &now) as isize }
}

/// Die with the parent `expected`. Darwin has no parent-death signal, so a
/// thread waits on the parent's exit and kills this process. Fails with ESRCH
/// when the parent is already gone.
pub(crate) fn exit_with_parent(expected: libc::pid_t) -> io::Result<()> {
    // SAFETY: success returns one fresh descriptor.
    let kq = owned(unsafe { libc::kqueue() })?;
    let change = libc::kevent {
        ident: expected as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change record and no output.
    if unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // Armed before the check, so a parent that exits after it is seen.
    // SAFETY: no arguments.
    if unsafe { libc::getppid() } != expected {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    std::thread::Builder::new()
        .name("hirsel-parent".into())
        .spawn(move || {
            // SAFETY: an all-zero kevent is a valid output slot.
            let mut fired: libc::kevent = unsafe { std::mem::zeroed() };
            loop {
                // SAFETY: one writable output record; wait without a timeout.
                let n = unsafe {
                    libc::kevent(
                        kq.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        &mut fired,
                        1,
                        std::ptr::null(),
                    )
                };
                if n > 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
            // SAFETY: ends this process, as the Linux parent-death signal does.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        })?;
    Ok(())
}

/// This thread's kernel thread id.
#[cfg(feature = "prof")]
pub(crate) fn thread_id() -> u64 {
    let mut id = 0u64;
    // SAFETY: a null thread names the caller; id is a live output.
    unsafe { libc::pthread_threadid_np(0, &mut id) };
    id
}

#[cfg(feature = "prof")]
/// This thread's stack as `(lowest address, one past the highest)`. Darwin
/// reports the stack's top address and its size.
pub(crate) fn stack_bounds() -> Option<(usize, usize)> {
    // SAFETY: queries on the calling thread.
    let (top, size) = unsafe {
        let this = libc::pthread_self();
        (
            libc::pthread_get_stackaddr_np(this).addr(),
            libc::pthread_get_stacksize_np(this),
        )
    };
    Some((top.checked_sub(size)?, top))
}

#[cfg(feature = "record")]
/// Run this thread only on time other threads leave: Darwin's background
/// band. Best effort; a refusal leaves normal priority.
pub(crate) fn background_thread() {
    // SAFETY: affects only the calling thread.
    unsafe { libc::setpriority(libc::PRIO_DARWIN_THREAD, 0, libc::PRIO_DARWIN_BG) };
}

/// The bytes a new UDP socket's two kernel queues start with, or `None` when
/// either cannot be read. Darwin sizes a UDP socket's receive queue from
/// `net.inet.udp.recvspace` and its send queue from `net.inet.udp.maxdgram`.
pub(crate) fn udp_queue_bytes() -> Option<u64> {
    let size = |name: &std::ffi::CStr| {
        let mut value: libc::c_int = 0;
        let mut len = size_of::<libc::c_int>();
        // SAFETY: the name is NUL-terminated and the kernel writes at most `len` bytes.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&raw mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len != size_of::<libc::c_int>() {
            return None;
        }
        u64::try_from(value).ok()
    };
    size(c"net.inet.udp.recvspace")?.checked_add(size(c"net.inet.udp.maxdgram")?)
}

/// The CPU this thread is running on, for the unit observer's trace lines:
/// `u32::MAX`, the observer's "not recorded", until pthread_cpu_number_np is on
/// the libSystem allowlist.
#[cfg(feature = "testing")]
pub(crate) fn cpu() -> u32 {
    u32::MAX
}

/// The CPUs this process may run on, for tests that pin one child per CPU.
/// Darwin has no affinity masks, so this is every CPU.
#[cfg(test)]
pub(crate) fn allowed_cpus() -> Vec<usize> {
    (0..std::thread::available_parallelism().map_or(1, usize::from)).collect()
}

/// Darwin cannot pin a thread to a CPU; the scheduler places it.
#[cfg(test)]
pub(crate) fn pin_to_cpu(_cpu: usize) {}

/// Minor page faults taken by the process: Darwin's getrusage has no
/// per-thread count, so callers compare deltas on a quiet process.
#[cfg(test)]
pub(crate) fn thread_minor_faults() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes the whole rusage on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    unsafe { usage.assume_init() }.ru_minflt
}

/// `<sys/proc_info.h>`'s flavor that lists a process's thread handles; the
/// libc crate does not export it.
#[cfg(test)]
const PROC_PIDLISTTHREADS: libc::c_int = 6;

/// The names of this process's live threads.
#[cfg(test)]
pub(crate) fn thread_names() -> Vec<String> {
    let pid = std::process::id() as libc::c_int;
    let mut handles = vec![0u64; 4096];
    // SAFETY: the buffer holds `handles.len()` thread handles.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            handles.as_mut_ptr().cast(),
            (handles.len() * size_of::<u64>()) as libc::c_int,
        )
    };
    assert!(bytes > 0, "{}", io::Error::last_os_error());
    handles.truncate(bytes as usize / size_of::<u64>());
    handles
        .into_iter()
        .filter_map(|handle| {
            // SAFETY: zeroed is a valid proc_threadinfo; the call fills it.
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            let size = size_of::<libc::proc_threadinfo>() as libc::c_int;
            // SAFETY: `info` is a writable proc_threadinfo of `size` bytes.
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    handle,
                    (&raw mut info).cast(),
                    size,
                )
            };
            (got == size).then(|| {
                // SAFETY: the kernel NUL-terminates pth_name.
                unsafe { std::ffi::CStr::from_ptr(info.pth_name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

/// Whether file operations take the `hirsel-host` helper thread: kqueue has
/// no file ring, so they always do.
#[cfg(test)]
pub(crate) fn file_helper_expected() -> bool {
    true
}

#[cfg(test)]
/// The directory listing this process's open descriptors.
pub(crate) const FD_DIR: &str = "/dev/fd";

/// The PID an inherited listener's `LISTEN_PID` names: the launcher, this
/// process's parent, since posix_spawn fixes the environment before the child
/// has a PID (launch.md, "macOS").
pub(crate) fn activation_pid() -> u32 {
    // SAFETY: getppid takes no arguments.
    unsafe { libc::getppid() as u32 }
}

/// A shell's spelling of `activation_pid` for the process it execs: its parent's PID.
#[cfg(test)]
pub(crate) const ACTIVATION_PID_SHELL: &str = "$PPID";

/// Whether the open descriptor `fd` is the running executable's file.
pub(crate) fn running_image(fd: RawFd) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: an all-zero stat is a valid output record; fstat writes one.
    let mut image: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut image) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let running = std::fs::metadata(std::env::current_exe()?)?;
    Ok((image.st_dev as u64, image.st_ino) == (running.dev(), running.ino()))
}

/// Return the whole anonymous private writable pages of `[start, start + bytes)`
/// to the host and keep the range mapped: its next reads are zero. Darwin's
/// `MADV_DONTNEED` and `MADV_FREE` keep the pages resident and charged to the
/// process, so the range gets a fresh mapping in place. A refused mapping can
/// leave the range unmapped, so it stops the run.
/// # Safety
/// The caller owns the range, which holds no live data.
#[inline]
pub(crate) unsafe fn discard(start: usize, bytes: usize) -> bool {
    // SAFETY: per the contract; the fixed mapping replaces only the caller's range.
    let fresh = unsafe {
        libc::mmap(
            std::ptr::without_provenance_mut(start),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    if fresh == libc::MAP_FAILED {
        crate::process::stop(b"HostMemory");
    }
    true
}

/// Replace `[start, start + bytes)` with a fresh `PROT_NONE` anonymous mapping,
/// releasing both physical pages and writable commitment.
/// # Safety
/// The caller owns the range, which holds no live data.
#[inline]
pub(crate) unsafe fn decommit(start: usize, bytes: usize) -> bool {
    // SAFETY: per the contract; the fixed mapping replaces only the caller's range.
    unsafe {
        libc::mmap(
            std::ptr::without_provenance_mut(start),
            bytes,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
            -1,
            0,
        ) != libc::MAP_FAILED
    }
}

/// The path that opens the store directory a launcher passed on descriptor 5;
/// Darwin's /dev/fd opens a duplicate of it.
pub(crate) const INHERITED_STORE: &[u8] = b"/dev/fd/5";

/// No page map: Darwin has no query that tells a never-backed page from one
/// the kernel compressed (`mincore` reports both as not resident), so a zero
/// fill writes every page.
pub(crate) fn page_map() -> Option<std::fs::File> {
    None
}

/// This process's resident set in bytes (`proc_pid_rusage`'s `ri_resident_size`).
#[cfg(feature = "prof")]
pub(crate) fn resident_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: RUSAGE_INFO_V4 writes one rusage_info_v4 into the buffer.
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            usage.as_mut_ptr().cast(),
        )
    };
    // SAFETY: the call succeeded, so the whole struct is written.
    (rc == 0).then(|| unsafe { usage.assume_init() }.ri_resident_size)
}

/// Each loaded image's executable `__TEXT` segment as a `/proc/self/maps`
/// line (`start-end r-xp offset major:minor inode path`), so a profile
/// symbolizes sampled PCs the way it does on Linux. An image from the shared
/// cache has no file of its own and reports device and inode zero.
#[cfg(feature = "prof")]
pub(crate) fn mappings() -> Option<Vec<u8>> {
    use std::io::Write as _;
    use std::os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _};
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_SEGMENT_64: u32 = 0x19;
    let mut out = Vec::new();
    // SAFETY: dyld's image list; an index below the count names a loaded image.
    for index in 0..unsafe { libc::_dyld_image_count() } {
        // SAFETY: as above; dyld keeps the header and name while it is loaded.
        let (header, slide, name) = unsafe {
            (
                libc::_dyld_get_image_header(index).cast::<u8>(),
                libc::_dyld_get_image_vmaddr_slide(index),
                libc::_dyld_get_image_name(index),
            )
        };
        if header.is_null() || name.is_null() {
            continue;
        }
        // SAFETY: dyld's NUL-terminated path of a loaded image.
        let path = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes();
        let read32 = |at: usize| {
            // SAFETY: `at` lies within the mapped header and load commands.
            unsafe { header.add(at).cast::<u32>().read_unaligned() }
        };
        let read64 = |at: usize| {
            // SAFETY: as above.
            unsafe { header.add(at).cast::<u64>().read_unaligned() }
        };
        if read32(0) != MH_MAGIC_64 || path.contains(&b'\n') {
            continue;
        }
        let (dev, inode) = std::fs::metadata(std::ffi::OsStr::from_bytes(path))
            .map_or((0, 0), |meta| (meta.dev() as u32, meta.ino()));
        let mut at = 32;
        for _ in 0..read32(16) {
            let (command, size) = (read32(at), read32(at + 4) as usize);
            // SAFETY: segname is 16 bytes inside the load command.
            let segment = unsafe { std::slice::from_raw_parts(header.add(at + 8), 16) };
            if command == LC_SEGMENT_64 && segment.starts_with(b"__TEXT\0") {
                let start = read64(at + 24).wrapping_add_signed(slide as i64);
                let _ = write!(
                    out,
                    "{start:x}-{:x} r-xp {:x} {:x}:{:x} {inode} ",
                    start + read64(at + 32),
                    read64(at + 40),
                    dev >> 24,
                    dev & 0xff_ffff,
                );
                out.extend_from_slice(path);
                out.push(b'\n');
            }
            at += size;
        }
    }
    Some(out)
}
