use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::time::Duration;

pub(super) const READ: u32 = hirsel_core_net::sys::EPOLLIN as u32;
pub(super) const WRITE: u32 = hirsel_core_net::sys::EPOLLOUT as u32;
pub(super) const CHANGE: u32 = hirsel_core_net::sys::EPOLLPRI as u32;

pub(super) struct Epoll(Rc<OwnedFd>);

impl Epoll {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: a successful invocation creates a fresh descriptor.
        let fd =
            unsafe { hirsel_core_net::sys::epoll_create1(hirsel_core_net::sys::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor is fresh and uniquely owned.
        Ok(Self(Rc::new(unsafe { OwnedFd::from_raw_fd(fd) })))
    }

    pub(super) fn watch(&self, fd: RawFd) -> Watch {
        Watch {
            epoll: Rc::clone(&self.0),
            fd,
            interest: 0,
        }
    }

    pub(super) fn wait(
        &self,
        timeout: Option<Duration>,
        events: &mut [hirsel_core_net::sys::epoll_event],
    ) -> io::Result<usize> {
        let millis = timeout.map_or(-1, |t| {
            t.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32
        });
        // SAFETY: epoll writes at most events.len() events to the output slice.
        let count = unsafe {
            hirsel_core_net::sys::epoll_wait(
                self.0.as_raw_fd(),
                events.as_mut_ptr(),
                events.len() as i32,
                millis,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
            return Ok(0);
        }
        Ok(count as usize)
    }
}

/// Keep the registration with its descriptor. Drop the watch before the file,
/// since a serving process can still hold a duplicate of the listener.
pub(super) struct Watch {
    epoll: Rc<OwnedFd>,
    fd: RawFd,
    interest: u32,
}

impl Watch {
    pub(super) fn set(&mut self, interest: u32) -> io::Result<()> {
        if self.interest == interest {
            return Ok(());
        }
        let op = match (self.interest, interest) {
            (_, 0) => hirsel_core_net::sys::EPOLL_CTL_DEL,
            (0, _) => hirsel_core_net::sys::EPOLL_CTL_ADD,
            _ => hirsel_core_net::sys::EPOLL_CTL_MOD,
        };
        let mut event = hirsel_core_net::sys::epoll_event {
            events: interest,
            u64: self.fd as u64,
        };
        // SAFETY: this registration's descriptor is still live, and epoll
        // copies the initialized event on ADD/MOD.
        if unsafe {
            hirsel_core_net::sys::epoll_ctl(self.epoll.as_raw_fd(), op, self.fd, &mut event)
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        self.interest = interest;
        Ok(())
    }

    pub(super) fn fired(&self, events: &[hirsel_core_net::sys::epoll_event]) -> bool {
        self.interest != 0 && events.iter().any(|event| event.u64 == self.fd as u64)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.set(0);
    }
}
