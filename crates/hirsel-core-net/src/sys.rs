//! The Linux system interfaces the runtime and driver are written against,
//! with the same names and signatures on every host.
//!
//! Linux re-exports libc. macOS emulates the rest: epoll on kqueue (an
//! edge-triggered interest is `EV_CLEAR`, a one-shot one `EV_DISPATCH`), an
//! eventfd as a nonblocking pipe whose write end this module keeps, and the
//! flag-taking descriptor calls as the plain call followed by `fcntl`. Read
//! and write an eventfd with [`eventfd_read`] and [`eventfd_write`].

/// The result of setting a socket's read or write timeout. macOS refuses
/// both with EINVAL once the peer has closed; such a socket no longer
/// blocks, so the read or write that follows reports the close itself.
pub fn timeout_set(result: std::io::Result<()>) -> std::io::Result<()> {
    match result {
        Err(error) if cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EINVAL) => {
            Ok(())
        }
        result => result,
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "linux")]
mod linux {
    pub use libc::{
        CLOCK_MONOTONIC, CPU_EQUAL, SO_PROTOCOL, getsockopt, gettid, sched_setaffinity,
    };
    pub use libc::{CPU_ISSET, CPU_SET, CPU_SETSIZE, cpu_set_t, sched_getaffinity, sched_getcpu};
    pub use libc::{
        EFD_CLOEXEC, EFD_NONBLOCK, EPOLL_CLOEXEC, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD,
        EPOLLERR, EPOLLET, EPOLLHUP, EPOLLIN, EPOLLONESHOT, EPOLLOUT, EPOLLPRI, EPOLLRDHUP,
        MSG_NOSIGNAL, O_PATH, POLLRDHUP, SOCK_CLOEXEC, SOCK_NONBLOCK, accept4, dup3, epoll_create1,
        epoll_ctl, epoll_event, epoll_wait, eventfd, fdatasync, getrandom, pipe2, socket,
        socketpair,
    };
    pub use libc::{GRND_NONBLOCK, mmsghdr, recvmmsg, sendmmsg};
    pub use libc::{RESOLVE_BENEATH, RESOLVE_NO_MAGICLINKS, RESOLVE_NO_SYMLINKS};

    /// `openat2(2)` with `how.resolve = resolve`. Returns the new descriptor, or -1 with errno.
    ///
    /// # Safety
    /// `path` is a NUL-terminated string.
    pub unsafe fn openat2(
        dirfd: c_int,
        path: *const libc::c_char,
        flags: c_int,
        mode: libc::mode_t,
        resolve: u64,
    ) -> c_int {
        // SAFETY: open_how is an integer-only Linux UAPI structure.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = flags as u64;
        how.mode = u64::from(mode);
        how.resolve = resolve;
        // SAFETY: Linux openat2 atomically enforces confinement, including symlinks.
        unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dirfd,
                path,
                &how,
                size_of::<libc::open_how>(),
            ) as c_int
        }
    }
    use libc::{c_int, c_void, size_t, ssize_t};
    use std::sync::atomic::AtomicU32;

    /// Sleeps while `word` holds `expected`, until woken or `timeout` passes.
    pub fn futex_wait(word: &AtomicU32, expected: u32, timeout: Option<&libc::timespec>) {
        // SAFETY: FUTEX_WAIT reads the pinned word and returns at once if it changed.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
                expected,
                timeout.map_or(std::ptr::null(), |t| t as *const libc::timespec),
            );
        }
    }

    /// Wakes up to `count` waiters on `word`.
    pub fn futex_wake(word: &AtomicU32, count: i32) {
        // SAFETY: waking a pinned word has no other effect.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                word.as_ptr(),
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                count,
            );
        }
    }

    /// # Safety
    /// As `read(2)` on an eventfd.
    #[inline]
    pub unsafe fn eventfd_read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
        unsafe { libc::read(fd, buf, count) }
    }

    /// # Safety
    /// As `write(2)` on an eventfd.
    #[inline]
    pub unsafe fn eventfd_write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
        unsafe { libc::write(fd, buf, count) }
    }

    /// The calling thread's `errno`.
    pub fn errno_location() -> *mut c_int {
        // SAFETY: always returns this thread's errno slot.
        unsafe { libc::__errno_location() }
    }
}

#[cfg(target_os = "macos")]
#[allow(non_snake_case, non_camel_case_types)]
mod macos {
    use libc::{c_int, c_uint, c_void, size_t, sockaddr, socklen_t, ssize_t};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub const EPOLLIN: c_int = 0x001;
    pub const EPOLLPRI: c_int = 0x002;
    pub const EPOLLOUT: c_int = 0x004;
    pub const EPOLLERR: c_int = 0x008;
    pub const EPOLLHUP: c_int = 0x010;
    pub const EPOLLRDHUP: c_int = 0x2000;
    pub const EPOLLONESHOT: c_int = 0x4000_0000;
    pub const EPOLLET: c_int = 0x8000_0000u32 as c_int;
    pub const EPOLL_CLOEXEC: c_int = 0x8_0000;
    pub const EPOLL_CTL_ADD: c_int = 1;
    pub const EPOLL_CTL_DEL: c_int = 2;
    pub const EPOLL_CTL_MOD: c_int = 3;
    pub const EFD_CLOEXEC: c_int = 0x8_0000;
    pub const EFD_NONBLOCK: c_int = 0x800;
    /// Bits above every socket type, stripped by [`socket`], [`socketpair`]
    /// and [`accept4`] before the call and applied with `fcntl` after it.
    pub const SOCK_NONBLOCK: c_int = 0x1000_0000;
    pub const SOCK_CLOEXEC: c_int = 0x2000_0000;
    /// Sockets made here carry `SO_NOSIGPIPE` instead.
    pub const MSG_NOSIGNAL: c_int = 0;
    pub const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    pub const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    pub const RESOLVE_BENEATH: u64 = 0x08;

    /// `openat2(2)` by open flags: `RESOLVE_BENEATH` is `O_RESOLVE_BENEATH`
    /// and `RESOLVE_NO_SYMLINKS` is `O_NOFOLLOW_ANY`. There are no magic links.
    ///
    /// # Safety
    /// `path` is a NUL-terminated string.
    pub unsafe fn openat2(
        dirfd: c_int,
        path: *const libc::c_char,
        flags: c_int,
        mode: libc::mode_t,
        resolve: u64,
    ) -> c_int {
        const O_RESOLVE_BENEATH: c_int = 0x1000;
        const O_NOFOLLOW_ANY: c_int = 0x2000_0000;
        let mut flags = flags;
        if resolve & RESOLVE_BENEATH != 0 {
            flags |= O_RESOLVE_BENEATH;
        }
        if resolve & RESOLVE_NO_SYMLINKS != 0 {
            flags |= O_NOFOLLOW_ANY;
        }
        unsafe { libc::openat(dirfd, path, flags, libc::c_uint::from(mode)) }
    }

    unsafe extern "C" {
        /// Gives this thread its own working directory (`fd`), or returns it
        /// to the process's (`-1`).
        pub fn pthread_fchdir_np(fd: c_int) -> c_int;
    }

    /// The monotonic clock, in nanoseconds: macOS's CLOCK_MONOTONIC counts
    /// whole microseconds, and CLOCK_MONOTONIC_RAW (which also runs through
    /// sleep) nanoseconds.
    pub const CLOCK_MONOTONIC: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;

    /// macOS's getsockopt has no SO_PROTOCOL; [`getsockopt`] answers it.
    pub const SO_PROTOCOL: c_int = 0x1022;

    /// `getsockopt(2)`, answering SO_ACCEPTCONN and SO_PROTOCOL, which macOS
    /// refuses with ENOPROTOOPT, from the kernel's record of the socket
    /// (`proc_pidfdinfo`'s `socket_fdinfo`).
    ///
    /// # Safety
    /// As `getsockopt(2)`; an answered option writes one int.
    pub unsafe fn getsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *mut c_void,
        len: *mut socklen_t,
    ) -> c_int {
        if level != libc::SOL_SOCKET || !matches!(name, libc::SO_ACCEPTCONN | SO_PROTOCOL) {
            // SAFETY: the caller's option buffer.
            return unsafe { libc::getsockopt(fd, level, name, value, len) };
        }
        const PROC_PIDFDSOCKETINFO: c_int = 3;
        // sizeof(struct socket_fdinfo) and its psi.soi_protocol and
        // psi.soi_options offsets.
        const SIZE: usize = 792;
        let mut info = [0u8; SIZE];
        // SAFETY: the buffer holds SIZE bytes for the kernel to fill.
        let filled = unsafe {
            libc::proc_pidfdinfo(
                libc::getpid(),
                fd,
                PROC_PIDFDSOCKETINFO,
                info.as_mut_ptr().cast(),
                SIZE as c_int,
            )
        };
        if filled != SIZE as c_int {
            if filled > 0 {
                set_errno(libc::ENOTSOCK);
            }
            return -1;
        }
        let answer = if name == SO_PROTOCOL {
            c_int::from_le_bytes(info[180..184].try_into().unwrap())
        } else {
            c_int::from(i16::from_le_bytes(info[188..190].try_into().unwrap()) & 0x2 != 0)
        };
        // SAFETY: an int option's buffer, as the caller sized it.
        unsafe {
            value.cast::<c_int>().write_unaligned(answer);
            *len = size_of::<c_int>() as socklen_t;
        }
        0
    }

    /// kqueue reports a peer's shutdown as end of file on read.
    pub const POLLRDHUP: libc::c_short = 0;
    /// No path-only descriptors: a plain read-only open.
    pub const O_PATH: c_int = 0;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct epoll_event {
        pub events: u32,
        pub u64: u64,
    }

    pub fn errno_location() -> *mut c_int {
        // SAFETY: always returns this thread's errno slot.
        unsafe { libc::__error() }
    }

    /// No CPU affinity: the scheduler places threads, so every affinity
    /// call fails and callers keep their defaults.
    pub const CPU_SETSIZE: c_int = 1024;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct cpu_set_t {
        bits: [u64; CPU_SETSIZE as usize / 64],
    }

    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn CPU_ISSET(cpu: usize, set: &cpu_set_t) -> bool {
        set.bits
            .get(cpu / 64)
            .is_some_and(|word| word & (1 << (cpu % 64)) != 0)
    }

    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn CPU_SET(cpu: usize, set: &mut cpu_set_t) {
        if let Some(word) = set.bits.get_mut(cpu / 64) {
            *word |= 1 << (cpu % 64);
        }
    }

    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn sched_getaffinity(_pid: c_int, _size: size_t, _set: *mut cpu_set_t) -> c_int {
        set_errno(libc::ENOSYS);
        -1
    }

    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn sched_setaffinity(_pid: c_int, _size: size_t, _set: *const cpu_set_t) -> c_int {
        set_errno(libc::ENOSYS);
        -1
    }

    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn CPU_EQUAL(a: &cpu_set_t, b: &cpu_set_t) -> bool {
        a.bits == b.bits
    }

    /// This thread's kernel thread ID, as a Mach port name.
    ///
    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn gettid() -> libc::pid_t {
        // SAFETY: pthread_self is always this live thread.
        unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) as libc::pid_t }
    }

    /// The CPU this thread last ran on.
    ///
    /// # Safety
    /// None; unsafe to match Linux.
    pub unsafe fn sched_getcpu() -> c_int {
        unsafe extern "C" {
            fn pthread_cpu_number_np(cpu: *mut size_t) -> c_int;
        }
        let mut cpu = 0;
        // SAFETY: writes one size_t we own.
        if unsafe { pthread_cpu_number_np(&mut cpu) } != 0 {
            return -1;
        }
        cpu as c_int
    }

    unsafe extern "C" {
        fn os_sync_wait_on_address(
            addr: *mut c_void,
            value: u64,
            size: size_t,
            flags: u32,
        ) -> c_int;
        fn os_sync_wait_on_address_with_timeout(
            addr: *mut c_void,
            value: u64,
            size: size_t,
            flags: u32,
            clock: u32,
            timeout_ns: u64,
        ) -> c_int;
        fn os_sync_wake_by_address_any(addr: *mut c_void, size: size_t, flags: u32) -> c_int;
        fn os_sync_wake_by_address_all(addr: *mut c_void, size: size_t, flags: u32) -> c_int;
    }

    /// Sleeps while `word` holds `expected`, until woken or `timeout` passes.
    pub fn futex_wait(
        word: &std::sync::atomic::AtomicU32,
        expected: u32,
        timeout: Option<&libc::timespec>,
    ) {
        const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;
        let addr = word.as_ptr().cast();
        // SAFETY: the word is pinned by the caller's borrow for the call.
        unsafe {
            match timeout {
                None => os_sync_wait_on_address(addr, u64::from(expected), 4, 0),
                Some(t) => {
                    let ns = (t.tv_sec.max(0) as u64)
                        .saturating_mul(1_000_000_000)
                        .saturating_add(t.tv_nsec.max(0) as u64);
                    if ns == 0 {
                        return;
                    }
                    os_sync_wait_on_address_with_timeout(
                        addr,
                        u64::from(expected),
                        4,
                        0,
                        OS_CLOCK_MACH_ABSOLUTE_TIME,
                        ns,
                    )
                }
            };
        }
    }

    /// Wakes up to `count` waiters on `word`.
    pub fn futex_wake(word: &std::sync::atomic::AtomicU32, count: i32) {
        let addr = word.as_ptr().cast();
        // SAFETY: waking a pinned word has no other effect.
        unsafe {
            if count == 1 {
                os_sync_wake_by_address_any(addr, 4, 0);
            } else {
                os_sync_wake_by_address_all(addr, 4, 0);
            }
        }
    }

    /// # Safety
    /// As Linux `getrandom(2)`: `buf` has `len` writable bytes.
    /// Accepted and ignored: macOS's entropy source never blocks.
    pub const GRND_NONBLOCK: c_uint = 1;

    pub unsafe fn getrandom(buf: *mut c_void, len: size_t, _flags: c_uint) -> ssize_t {
        let mut done = 0;
        while done < len {
            let step = (len - done).min(256);
            // SAFETY: in bounds of the caller's buffer.
            if unsafe { libc::getentropy(buf.cast::<u8>().add(done).cast(), step) } != 0 {
                return -1;
            }
            done += step;
        }
        len as ssize_t
    }

    /// # Safety
    /// As `fdatasync(2)`.
    pub unsafe fn fdatasync(fd: c_int) -> c_int {
        unsafe { libc::fsync(fd) }
    }

    fn set_errno(errno: c_int) {
        // SAFETY: this thread's errno slot.
        unsafe { *errno_location() = errno };
    }

    /// Applies `FD_CLOEXEC` and `O_NONBLOCK` as requested. Returns -1 with
    /// errno set on failure.
    fn apply(fd: c_int, cloexec: bool, nonblock: bool) -> c_int {
        // SAFETY: fcntl on a descriptor the caller owns.
        unsafe {
            if cloexec && libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                return -1;
            }
            if nonblock {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                    return -1;
                }
            }
        }
        0
    }

    fn close_keeping_errno(fd: c_int) {
        let errno = unsafe { *errno_location() };
        // SAFETY: the caller owns fd and is abandoning it.
        unsafe { libc::close(fd) };
        set_errno(errno);
    }

    fn nosigpipe(fd: c_int) {
        let one: c_int = 1;
        // SAFETY: setsockopt on a socket the caller owns.
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&raw const one).cast(),
                size_of::<c_int>() as socklen_t,
            )
        };
    }

    /// # Safety
    /// As Linux `socket(2)`.
    pub unsafe fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
        let fd = unsafe { libc::socket(domain, ty & !(SOCK_CLOEXEC | SOCK_NONBLOCK), protocol) };
        if fd < 0 {
            return fd;
        }
        if apply(fd, ty & SOCK_CLOEXEC != 0, ty & SOCK_NONBLOCK != 0) < 0 {
            close_keeping_errno(fd);
            return -1;
        }
        nosigpipe(fd);
        fd
    }

    /// # Safety
    /// As Linux `socketpair(2)`.
    pub unsafe fn socketpair(domain: c_int, ty: c_int, protocol: c_int, sv: *mut c_int) -> c_int {
        let rc =
            unsafe { libc::socketpair(domain, ty & !(SOCK_CLOEXEC | SOCK_NONBLOCK), protocol, sv) };
        if rc < 0 {
            return rc;
        }
        for i in 0..2 {
            // SAFETY: socketpair filled both slots.
            let fd = unsafe { *sv.add(i) };
            if apply(fd, ty & SOCK_CLOEXEC != 0, ty & SOCK_NONBLOCK != 0) < 0 {
                close_keeping_errno(unsafe { *sv });
                close_keeping_errno(unsafe { *sv.add(1) });
                return -1;
            }
            nosigpipe(fd);
        }
        0
    }

    /// # Safety
    /// As Linux `accept4(2)`.
    pub unsafe fn accept4(
        fd: c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flags: c_int,
    ) -> c_int {
        let accepted = unsafe { libc::accept(fd, addr, len) };
        if accepted < 0 {
            return accepted;
        }
        if apply(
            accepted,
            flags & SOCK_CLOEXEC != 0,
            flags & SOCK_NONBLOCK != 0,
        ) < 0
        {
            close_keeping_errno(accepted);
            return -1;
        }
        nosigpipe(accepted);
        accepted
    }

    /// # Safety
    /// As Linux `pipe2(2)`.
    pub unsafe fn pipe2(fds: *mut c_int, flags: c_int) -> c_int {
        let rc = unsafe { libc::pipe(fds) };
        if rc < 0 {
            return rc;
        }
        for i in 0..2 {
            // SAFETY: pipe filled both slots.
            let fd = unsafe { *fds.add(i) };
            if apply(
                fd,
                flags & libc::O_CLOEXEC != 0,
                flags & libc::O_NONBLOCK != 0,
            ) < 0
            {
                close_keeping_errno(unsafe { *fds });
                close_keeping_errno(unsafe { *fds.add(1) });
                return -1;
            }
        }
        0
    }

    /// # Safety
    /// As Linux `dup3(2)`.
    pub unsafe fn dup3(old: c_int, new: c_int, flags: c_int) -> c_int {
        if old == new {
            set_errno(libc::EINVAL);
            return -1;
        }
        let fd = unsafe { libc::dup2(old, new) };
        if fd >= 0 && flags & libc::O_CLOEXEC != 0 && apply(fd, true, false) < 0 {
            return -1;
        }
        fd
    }

    /// # Safety
    /// As `epoll_create1(2)`.
    pub unsafe fn epoll_create1(flags: c_int) -> c_int {
        let kq = unsafe { libc::kqueue() };
        if kq >= 0 && flags & EPOLL_CLOEXEC != 0 && apply(kq, true, false) < 0 {
            close_keeping_errno(kq);
            return -1;
        }
        kq
    }

    fn change(fd: c_int, filter: i16, flags: u16, token: u64) -> libc::kevent {
        libc::kevent {
            ident: fd as usize,
            filter,
            flags,
            fflags: 0,
            data: 0,
            udata: token as *mut c_void,
        }
    }

    /// # Safety
    /// As `epoll_ctl(2)`: `event` is readable for ADD and MOD.
    pub unsafe fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut epoll_event) -> c_int {
        let mut changes = Vec::with_capacity(2);
        match op {
            EPOLL_CTL_DEL => {
                // A filter that was never added answers ENOENT, which is fine.
                for filter in [libc::EVFILT_READ, libc::EVFILT_WRITE] {
                    changes.push(change(fd, filter, libc::EV_DELETE | libc::EV_RECEIPT, 0));
                }
            }
            EPOLL_CTL_ADD | EPOLL_CTL_MOD => {
                // SAFETY: the caller passes a readable event.
                let event = unsafe { *event };
                let bits = event.events as c_int;
                let mut mode = libc::EV_ADD | libc::EV_ENABLE | libc::EV_RECEIPT;
                if bits & EPOLLET != 0 {
                    mode |= libc::EV_CLEAR;
                }
                if bits & EPOLLONESHOT != 0 {
                    mode |= libc::EV_DISPATCH;
                }
                for (filter, wanted) in [
                    (
                        libc::EVFILT_READ,
                        bits & (EPOLLIN | EPOLLRDHUP | EPOLLPRI) != 0,
                    ),
                    (libc::EVFILT_WRITE, bits & EPOLLOUT != 0),
                ] {
                    if wanted {
                        changes.push(change(fd, filter, mode, event.u64));
                    } else if op == EPOLL_CTL_MOD {
                        changes.push(change(fd, filter, libc::EV_DELETE | libc::EV_RECEIPT, 0));
                    }
                }
            }
            _ => {
                set_errno(libc::EINVAL);
                return -1;
            }
        }
        if changes.is_empty() {
            return 0;
        }
        let mut receipts = changes.clone();
        // SAFETY: both arrays hold `changes.len()` events.
        let n = unsafe {
            libc::kevent(
                epfd,
                changes.as_ptr(),
                changes.len() as c_int,
                receipts.as_mut_ptr(),
                receipts.len() as c_int,
                std::ptr::null(),
            )
        };
        if n < 0 {
            return -1;
        }
        let mut deleted = 0;
        for receipt in &receipts[..n as usize] {
            let errno = receipt.data as c_int;
            if receipt.flags & libc::EV_ERROR == 0 || errno == 0 {
                continue;
            }
            if errno == libc::ENOENT && op != EPOLL_CTL_ADD {
                deleted += 1;
                continue;
            }
            set_errno(errno);
            return -1;
        }
        if op == EPOLL_CTL_DEL && deleted == changes.len() {
            set_errno(libc::ENOENT);
            return -1;
        }
        0
    }

    /// # Safety
    /// As `epoll_wait(2)`: `events` has room for `max` events.
    pub unsafe fn epoll_wait(
        epfd: c_int,
        events: *mut epoll_event,
        max: c_int,
        timeout_ms: c_int,
    ) -> c_int {
        if max <= 0 {
            set_errno(libc::EINVAL);
            return -1;
        }
        let timeout = libc::timespec {
            tv_sec: (timeout_ms.max(0) / 1000) as libc::time_t,
            tv_nsec: (timeout_ms.max(0) % 1000) as libc::c_long * 1_000_000,
        };
        let mut raw: Vec<libc::kevent> = Vec::with_capacity(max as usize);
        // SAFETY: raw has room for max events.
        let n = unsafe {
            libc::kevent(
                epfd,
                std::ptr::null(),
                0,
                raw.as_mut_ptr(),
                max,
                if timeout_ms < 0 {
                    std::ptr::null()
                } else {
                    &timeout
                },
            )
        };
        if n < 0 {
            return n;
        }
        // SAFETY: kevent initialized n events.
        unsafe { raw.set_len(n as usize) };
        // One entry per token, as epoll reports one per descriptor.
        let mut out = 0usize;
        for kev in &raw {
            if kev.flags & libc::EV_ERROR != 0 {
                continue;
            }
            let mut bits = 0u32;
            let eof = kev.flags & libc::EV_EOF != 0;
            if kev.filter == libc::EVFILT_READ {
                bits |= EPOLLIN as u32;
                if eof {
                    bits |= EPOLLRDHUP as u32;
                }
            } else if kev.filter == libc::EVFILT_WRITE {
                bits |= EPOLLOUT as u32;
                if eof {
                    bits |= EPOLLHUP as u32;
                }
            }
            if eof && kev.fflags != 0 {
                bits |= EPOLLERR as u32;
            }
            let token = kev.udata as u64;
            // SAFETY: out < n <= max entries of the caller's buffer.
            let filled = unsafe { std::slice::from_raw_parts_mut(events, out) };
            if let Some(same) = filled.iter_mut().find(|e| e.u64 == token) {
                same.events |= bits;
            } else {
                unsafe {
                    events.add(out).write(epoll_event {
                        events: bits,
                        u64: token,
                    })
                };
                out += 1;
            }
        }
        out as c_int
    }

    const F_SETNOSIGPIPE: c_int = 73;

    /// Read ends this table can index; eventfd refuses higher descriptors.
    const EVENTFDS: usize = 1 << 16;
    const BLOCKING: u32 = 1 << 31;
    /// Per read end: 0, or the write end plus one, with [`BLOCKING`] set when
    /// reads block. Lock-free, so [`eventfd_write`] is async-signal-safe.
    static WRITE_ENDS: [AtomicU32; EVENTFDS] = [const { AtomicU32::new(0) }; EVENTFDS];

    /// # Safety
    /// As `eventfd(2)`.
    pub unsafe fn eventfd(init: c_uint, flags: c_int) -> c_int {
        let mut fds = [0; 2];
        if unsafe { pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK) } < 0 {
            return -1;
        }
        let [read, write] = fds;
        if read as usize >= EVENTFDS {
            close_keeping_errno(read);
            close_keeping_errno(write);
            set_errno(libc::EMFILE);
            return -1;
        }
        if apply(write, true, false) < 0
            || (flags & EFD_CLOEXEC != 0 && apply(read, true, false) < 0)
            || unsafe { libc::fcntl(write, F_SETNOSIGPIPE, 1) } < 0
        {
            close_keeping_errno(read);
            close_keeping_errno(write);
            return -1;
        }
        let blocking = if flags & EFD_NONBLOCK == 0 {
            BLOCKING
        } else {
            0
        };
        // A read end closed earlier left its write end here; close it now.
        let stale = WRITE_ENDS[read as usize].swap((write as u32 + 1) | blocking, Ordering::AcqRel);
        if stale & !BLOCKING != 0 {
            // SAFETY: this module alone owns the write ends in the table.
            unsafe { libc::close(((stale & !BLOCKING) - 1) as c_int) };
        }
        if init != 0 {
            let value = u64::from(init);
            unsafe { eventfd_write(read, (&raw const value).cast(), 8) };
        }
        read
    }

    fn write_end(fd: c_int) -> Option<(c_int, bool)> {
        let entry = WRITE_ENDS
            .get(usize::try_from(fd).ok()?)?
            .load(Ordering::Acquire);
        let write = entry & !BLOCKING;
        (write != 0).then(|| ((write - 1) as c_int, entry & BLOCKING != 0))
    }

    /// Adds the eight-byte counter at `buf` to the eventfd `fd`.
    ///
    /// # Safety
    /// As `write(2)` on an eventfd.
    pub unsafe fn eventfd_write(fd: c_int, buf: *const c_void, count: size_t) -> ssize_t {
        let Some((write, _)) = write_end(fd) else {
            return unsafe { libc::write(fd, buf, count) };
        };
        if count < 8 {
            set_errno(libc::EINVAL);
            return -1;
        }
        let n = unsafe { libc::write(write, buf, 8) };
        // A full pipe already holds a wakeup for the reader.
        if n < 0 && unsafe { *errno_location() } == libc::EAGAIN {
            return 8;
        }
        n
    }

    /// Reads and resets the eventfd `fd`'s counter into the eight bytes at `buf`.
    ///
    /// # Safety
    /// As `read(2)` on an eventfd.
    pub unsafe fn eventfd_read(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
        let Some((_, blocking)) = write_end(fd) else {
            return unsafe { libc::read(fd, buf, count) };
        };
        if count < 8 {
            set_errno(libc::EINVAL);
            return -1;
        }
        loop {
            let mut sum = 0u64;
            let mut any = false;
            let mut chunk = [0u64; 64];
            loop {
                let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), size_of_val(&chunk)) };
                if n <= 0 {
                    break;
                }
                any = true;
                for value in &chunk[..n as usize / 8] {
                    sum = sum.wrapping_add(*value);
                }
            }
            if any {
                unsafe { buf.cast::<u64>().write_unaligned(sum) };
                return 8;
            }
            if unsafe { *errno_location() } != libc::EAGAIN || !blocking {
                return -1;
            }
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, -1) } < 0 {
                return -1;
            }
        }
    }

    /// Linux's `mmsghdr`: one message of a batch and the bytes it moved.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct mmsghdr {
        pub msg_hdr: libc::msghdr,
        pub msg_len: c_uint,
    }

    /// `sendmmsg` as one `sendmsg` per message: the messages sent before the
    /// first that fails, or that failure when it is the first.
    pub unsafe fn sendmmsg(
        fd: c_int,
        messages: *mut mmsghdr,
        count: c_uint,
        flags: c_int,
    ) -> c_int {
        for i in 0..count as usize {
            // SAFETY: the caller passes `count` initialized messages.
            let message = unsafe { &mut *messages.add(i) };
            let sent = unsafe { libc::sendmsg(fd, &message.msg_hdr, flags) };
            if sent < 0 {
                return if i == 0 { -1 } else { i as c_int };
            }
            message.msg_len = sent as c_uint;
        }
        count as c_int
    }

    /// `recvmmsg` as one `recvmsg` per message, under the same rule as
    /// [`sendmmsg`]. A timeout is not supported: callers pass none. Under
    /// `MSG_TRUNC` a message's length is the datagram's, as on Linux, read
    /// beforehand with `SO_NREAD`.
    pub unsafe fn recvmmsg(
        fd: c_int,
        messages: *mut mmsghdr,
        count: c_uint,
        flags: c_int,
        _timeout: *mut libc::timespec,
    ) -> c_int {
        for i in 0..count as usize {
            // SAFETY: the caller passes `count` initialized messages.
            let message = unsafe { &mut *messages.add(i) };
            let mut pending: c_int = 0;
            let mut size = size_of::<c_int>() as socklen_t;
            // SAFETY: SO_NREAD writes one int, the next datagram's length.
            let known = flags & libc::MSG_TRUNC != 0
                && unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_NREAD,
                        (&raw mut pending).cast(),
                        &mut size,
                    )
                } == 0;
            let received =
                unsafe { libc::recvmsg(fd, &mut message.msg_hdr, flags & !libc::MSG_TRUNC) };
            if received < 0 {
                return if i == 0 { -1 } else { i as c_int };
            }
            message.msg_len = if known && pending as isize > received {
                pending as c_uint
            } else {
                received as c_uint
            };
        }
        count as c_int
    }
}
