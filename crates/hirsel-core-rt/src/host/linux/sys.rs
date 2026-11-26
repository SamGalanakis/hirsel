//! Small host calls on Linux: errno, notices, sockets, peer credentials,
//! randomness, a retry timer and the parent-death arm.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

/// This thread's errno.
#[inline(always)]
pub(crate) fn errno() -> *mut libc::c_int {
    // SAFETY: the location is this thread's own errno.
    unsafe { libc::__errno_location() }
}

/// A one-way wakeup a writer posts and one reader takes: an eventfd, whose
/// single descriptor is both ends.
pub(crate) struct Notice(OwnedFd);

impl Notice {
    /// `nonblocking` makes [`take`] on an empty notice fail with EAGAIN.
    pub(crate) fn new(nonblocking: bool) -> io::Result<Self> {
        let flags = libc::EFD_CLOEXEC | if nonblocking { libc::EFD_NONBLOCK } else { 0 };
        // SAFETY: eventfd either returns an owned fd or signals a host error.
        let fd = unsafe { libc::eventfd(0, flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub(crate) fn reader(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    pub(crate) fn writer(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// Post one wakeup to a notice's writer: one async-signal-safe write. A full
/// counter is already readable, so a failed post loses nothing.
pub(crate) fn post(writer: RawFd) -> isize {
    // SAFETY: eight readable bytes for this one write.
    unsafe { libc::write(writer, 1u64.to_ne_bytes().as_ptr().cast(), 8) }
}

/// Take every posted wakeup from a notice's reader.
pub(crate) fn take(reader: RawFd) -> isize {
    let mut count = 0u64;
    // SAFETY: count holds the eventfd's eight bytes.
    unsafe { libc::read(reader, (&raw mut count).cast(), 8) }
}

/// A close-on-exec, nonblocking socket, or -1 with errno.
pub(crate) fn socket(domain: libc::c_int, kind: libc::c_int) -> isize {
    // SAFETY: success returns one fresh descriptor.
    unsafe { libc::socket(domain, kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) as isize }
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
    unsafe { libc::accept4(fd, address, len, libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK) as isize }
}

/// The socket option naming a socket's protocol, where the host has one.
pub(crate) const PROTOCOL: Option<libc::c_int> = Some(libc::SO_PROTOCOL);

/// Whether the socket `fd` is listening.
pub(crate) fn listening(fd: RawFd) -> io::Result<bool> {
    let mut value: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: value and len are live and sized for the option.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            (&raw mut value).cast(),
            &mut len,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value == 1)
    }
}

/// The address family of the socket `fd`.
pub(crate) fn socket_domain(fd: RawFd) -> io::Result<libc::c_int> {
    let mut value: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: value and len are live and sized for the option.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            (&raw mut value).cast(),
            &mut len,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

/// Receive one packet from the datagram socket `fd` into `room` bytes at
/// `data`, cutting a longer one. Returns the packet's whole length, or -1
/// with errno.
///
/// # Safety
/// `data` is writable for `room` bytes, or `room` is 0.
pub(crate) unsafe fn receive_packet(fd: RawFd, data: *mut u8, room: usize) -> isize {
    let mut iov = libc::iovec {
        iov_base: data.cast(),
        iov_len: room,
    };
    // SAFETY: integer/pointer-only header. There is no address or control area.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // SAFETY: by the contract. With MSG_TRUNC the result is the whole length.
    unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_TRUNC) }
}

/// The UID of the process at the other end of a connected Unix stream.
pub(crate) fn peer_uid(fd: RawFd) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are live, sized for SO_PEERCRED.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

/// The poll bit for a peer that shut down its sending half.
pub(crate) const PEER_CLOSED: libc::c_short = libc::POLLRDHUP;

/// A [`random`] flag: fail with EAGAIN rather than wait for the pool.
pub(crate) const RANDOM_NOWAIT: u32 = libc::GRND_NONBLOCK;

/// Fill a prefix of `len` bytes at `data` from the kernel's pool, returning
/// its length, or -1 with errno.
///
/// # Safety
/// `data` is writable for `len` bytes.
pub(crate) unsafe fn random(data: *mut u8, len: usize, flags: u32) -> isize {
    // SAFETY: by the contract.
    unsafe { libc::getrandom(data.cast(), len, flags) as isize }
}

/// A descriptor that turns readable every millisecond until [`tick`].
pub(crate) fn timer() -> io::Result<OwnedFd> {
    // SAFETY: success returns one fresh descriptor.
    let raw = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor uniquely owned here.
    let timer = unsafe { OwnedFd::from_raw_fd(raw) };
    let interval = libc::timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000,
    };
    let spec = libc::itimerspec {
        it_interval: interval,
        it_value: interval,
    };
    // SAFETY: a live timerfd and a valid spec.
    if unsafe { libc::timerfd_settime(raw, 0, &spec, std::ptr::null_mut()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(timer)
}

/// Consume a [`timer`]'s expirations.
pub(crate) fn tick(fd: RawFd) -> isize {
    let mut count = 0u64;
    // SAFETY: a live timerfd and its exact eight-byte result slot.
    unsafe { libc::read(fd, (&raw mut count).cast(), size_of::<u64>()) }
}

/// Die with the parent `expected`: the kernel kills this process when its
/// parent exits. Fails with ESRCH when the parent is already gone.
pub(crate) fn exit_with_parent(expected: libc::pid_t) -> io::Result<()> {
    // SAFETY: these calls take no memory arguments and affect only this process.
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
            return Err(io::Error::last_os_error());
        }
        if libc::getppid() != expected {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    Ok(())
}

/// This thread's kernel thread id.
#[cfg(feature = "prof")]
pub(crate) fn thread_id() -> u64 {
    // SAFETY: gettid has no operands.
    unsafe { libc::syscall(libc::SYS_gettid) as u64 }
}

#[cfg(feature = "prof")]
/// This thread's stack as `(lowest address, one past the highest)`.
pub(crate) fn stack_bounds() -> Option<(usize, usize)> {
    // SAFETY: pthread attributes are initialized by pthread_getattr_np and
    // destroyed once.
    unsafe {
        let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        if libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()) != 0 {
            return None;
        }
        let mut attributes = attributes.assume_init();
        let mut address = std::ptr::null_mut();
        let mut size = 0;
        let result = libc::pthread_attr_getstack(&attributes, &mut address, &mut size);
        libc::pthread_attr_destroy(&mut attributes);
        (result == 0).then(|| (address.addr(), address.addr() + size))
    }
}

#[cfg(feature = "record")]
/// Run this thread only on time other threads leave: the idle scheduling
/// class. Best effort; a refusal leaves normal priority.
pub(crate) fn background_thread() {
    let idle = libc::sched_param { sched_priority: 0 };
    // SAFETY: plain syscall on the calling thread with a valid parameter.
    unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &idle) };
}

/// The CPU this thread is running on, for the unit observer's trace lines.
#[cfg(feature = "testing")]
pub(crate) fn cpu() -> u32 {
    // SAFETY: sched_getcpu takes no pointers and has no program-visible effect.
    unsafe { libc::sched_getcpu() as u32 }
}

/// The CPUs this process may run on, for tests that pin one child per CPU.
#[cfg(test)]
pub(crate) fn allowed_cpus() -> Vec<usize> {
    // SAFETY: zeroed is the empty set; the call fills it.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) };
    (0..libc::CPU_SETSIZE as usize)
        .filter(|&c| unsafe { libc::CPU_ISSET(c, &set) })
        .collect()
}

/// Pins this process's main thread, and the threads it starts later, to `cpu`.
#[cfg(test)]
pub(crate) fn pin_to_cpu(cpu: usize) {
    // SAFETY: a one-CPU set for this process's main thread.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set);
    }
}

/// Minor page faults taken by the calling thread.
#[cfg(test)]
pub(crate) fn thread_minor_faults() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes the whole rusage on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_THREAD, usage.as_mut_ptr()) },
        0
    );
    unsafe { usage.assume_init() }.ru_minflt
}

/// The names of this process's live threads.
#[cfg(test)]
pub(crate) fn thread_names() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|task| std::fs::read_to_string(task.unwrap().path().join("comm")).ok())
        .map(|comm| comm.trim_end().to_owned())
        .collect()
}

/// Whether file operations take the `hirsel-host` helper thread: only epoll
/// has no ring to submit them to.
#[cfg(test)]
pub(crate) fn file_helper_expected() -> bool {
    hirsel_core_net::reactor().unwrap() == hirsel_core_net::Reactor::Epoll
}

#[cfg(test)]
/// The directory listing this process's open descriptors.
pub(crate) const FD_DIR: &str = "/proc/self/fd";
