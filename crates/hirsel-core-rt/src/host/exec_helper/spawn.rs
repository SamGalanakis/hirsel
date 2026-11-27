//! Starting, killing and reaping the helper: the shipped `hirsel-exec-helper`
//! executable whose path the embedder passes at `start` (EMB-3).
use std::ffi::CStr;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// The helper's end of the socket.
pub(super) const SOCKET_FD: RawFd = 3;

/// A started helper: its pid, and on Linux a pidfd that pins it, so signalling
/// or reaping it can never reach a host child that reused its pid.
pub(super) struct Started {
    #[cfg(target_vendor = "apple")]
    pid: libc::pid_t,
    #[cfg(target_os = "linux")]
    pidfd: Option<OwnedFd>,
}

fn spawned(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

struct Actions(libc::posix_spawn_file_actions_t);

impl Drop for Actions {
    fn drop(&mut self) {
        // SAFETY: initialized in `start` and destroyed once.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

struct Attributes(libc::posix_spawnattr_t);

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized in `start` and destroyed once.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

/// Start `image` with `socket` as its descriptor 3, `/dev/null` on 0..=2, an
/// argument vector of the image path alone, an empty environment, every
/// signal at its default, an empty mask, and its own process group. Darwin
/// closes every other descriptor at spawn; on Linux the helper closes them
/// first thing in its `main`.
pub(super) fn start(image: &CStr, socket: &OwnedFd) -> io::Result<Started> {
    // dup2 onto the same number keeps close-on-exec; move it clear of 0..=3.
    // SAFETY: a live descriptor; the result is owned below.
    let high = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if high < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a fresh descriptor.
    let high = unsafe { OwnedFd::from_raw_fd(high) };
    let null = c"/dev/null";
    let argv = [image.as_ptr().cast_mut(), std::ptr::null_mut()];
    let envp = [std::ptr::null_mut()];
    // SAFETY: zeroed opaque objects are initialized before use.
    let mut actions = Actions(unsafe { mem::zeroed() });
    let mut attributes = Attributes(unsafe { mem::zeroed() });
    let mut pid = 0;
    // SAFETY: each call receives initialized objects and live, NUL-terminated
    // strings; posix_spawn copies argv and envp before it returns.
    unsafe {
        spawned(libc::posix_spawn_file_actions_init(&mut actions.0))?;
        spawned(libc::posix_spawnattr_init(&mut attributes.0))?;
        spawned(libc::posix_spawn_file_actions_addopen(
            &mut actions.0,
            0,
            null.as_ptr(),
            libc::O_RDONLY,
            0,
        ))?;
        for fd in [1, 2] {
            spawned(libc::posix_spawn_file_actions_addopen(
                &mut actions.0,
                fd,
                null.as_ptr(),
                libc::O_WRONLY,
                0,
            ))?;
        }
        spawned(libc::posix_spawn_file_actions_adddup2(
            &mut actions.0,
            high.as_raw_fd(),
            SOCKET_FD,
        ))?;
        let mut all: libc::sigset_t = mem::zeroed();
        let mut none: libc::sigset_t = mem::zeroed();
        libc::sigfillset(&mut all);
        libc::sigdelset(&mut all, libc::SIGKILL);
        libc::sigdelset(&mut all, libc::SIGSTOP);
        libc::sigemptyset(&mut none);
        let flags = libc::POSIX_SPAWN_SETSIGDEF
            | libc::POSIX_SPAWN_SETSIGMASK
            | libc::POSIX_SPAWN_SETPGROUP;
        #[cfg(target_vendor = "apple")]
        let flags = flags | libc::POSIX_SPAWN_CLOEXEC_DEFAULT;
        spawned(libc::posix_spawnattr_setflags(
            &mut attributes.0,
            flags as libc::c_short,
        ))?;
        spawned(libc::posix_spawnattr_setsigdefault(&mut attributes.0, &all))?;
        spawned(libc::posix_spawnattr_setsigmask(&mut attributes.0, &none))?;
        spawned(libc::posix_spawnattr_setpgroup(&mut attributes.0, 0))?;
        spawned(libc::posix_spawn(
            &mut pid,
            image.as_ptr(),
            &actions.0,
            &attributes.0,
            argv.as_ptr(),
            envp.as_ptr(),
        ))?;
    }
    drop(high);
    #[cfg(target_os = "linux")]
    {
        // SAFETY: pidfd_open takes a pid and flags; the result is owned below.
        // The helper is unreaped here, so its pid names it.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) } as RawFd;
        // A host that reaps every child itself may have taken it already.
        let pidfd = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) });
        Ok(Started { pidfd })
    }
    #[cfg(target_vendor = "apple")]
    Ok(Started { pid })
}

impl Started {
    /// Kill a helper that never answered. It is unreaped until `reap`.
    pub(super) fn kill(&self) {
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = &self.pidfd {
            // SAFETY: the pidfd pins this child; a reaped one gives ESRCH.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0u32,
                )
            };
        }
        #[cfg(target_vendor = "apple")]
        // SAFETY: the helper stays unreaped until `reap`, so its pid names it
        // unless the host reaps every child itself.
        unsafe {
            libc::kill(self.pid, libc::SIGKILL)
        };
    }

    /// Wait for the helper to finish and reap it. A host that reaps every
    /// child (or ignores SIGCHLD) may have reaped it already: that is ECHILD,
    /// and the helper is gone either way.
    pub(super) fn reap(self) {
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = self.pidfd {
            loop {
                // SAFETY: waitid writes siginfo; the pidfd pins this child.
                let mut info: libc::siginfo_t = unsafe { mem::zeroed() };
                let rc = unsafe {
                    libc::waitid(
                        libc::P_PIDFD,
                        pidfd.as_raw_fd() as libc::id_t,
                        &mut info,
                        libc::WEXITED,
                    )
                };
                if rc == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    return;
                }
            }
        }
        #[cfg(target_vendor = "apple")]
        loop {
            let mut status = 0;
            // SAFETY: waitpid writes status. The helper stays unreaped until
            // here unless the host reaps every child itself.
            let rc = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if rc >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return;
            }
        }
    }
}
