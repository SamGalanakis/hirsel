pub(super) use hirsel_core_net::ready::Event as ReadyEvent;
use hirsel_core_net::ready::{Event, Ready, Selector, Trigger};
use std::cell::Cell;
use std::io;
use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::Duration;

pub(super) const READ: Ready = Ready::READABLE;
pub(super) const WRITE: Ready = Ready::WRITABLE;
pub(super) const CHANGE: Ready = Ready::CHANGED;
pub(super) const NONE: Ready = Ready::EMPTY;

/// The launcher's readiness set; each watch is keyed by its descriptor, or a
/// child's exit by the token its host gives it.
pub(super) struct Readiness(Rc<Selector>, Cell<bool>);

impl Readiness {
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self(Rc::new(Selector::new()?), Cell::new(false)))
    }

    pub(super) fn watch(&self, fd: RawFd) -> Watch {
        Watch {
            selector: Rc::clone(&self.0),
            fd: Some(fd),
            token: fd as u64,
            interest: Ready::EMPTY,
            exited: false,
        }
    }

    /// Watch `child`'s exit, reported once. A child that exited before the
    /// watch could be registered (Darwin's `EVFILT_PROC` refuses it with
    /// ESRCH) fires on every turn, and the next wait does not block.
    pub(super) fn watch_exit(&self, child: &super::spawn::Spawned) -> io::Result<Watch> {
        let (source, token, fd) = child.exit_source();
        self.watch_exit_source(source, token, fd)
    }

    pub(super) fn watch_exit_source(
        &self,
        source: hirsel_core_net::ready::Child<'_>,
        token: u64,
        fd: Option<RawFd>,
    ) -> io::Result<Watch> {
        let mut watch = Watch {
            selector: Rc::clone(&self.0),
            fd,
            token,
            interest: Ready::EMPTY,
            exited: false,
        };
        match self.0.add_exit(source, token) {
            Ok(()) => watch.interest = READ,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                watch.exited = true;
                self.1.set(true);
            }
            Err(error) => return Err(error),
        }
        Ok(watch)
    }

    /// An interrupted wait reports no events.
    pub(super) fn wait(
        &self,
        timeout: Option<Duration>,
        events: &mut [Event],
    ) -> io::Result<usize> {
        let timeout = if self.1.take() {
            Some(Duration::ZERO)
        } else {
            timeout
        };
        match self.0.wait(events, timeout) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(0),
            result => result,
        }
    }
}

/// Keep the registration with its descriptor. Drop the watch before the file,
/// since a serving process can still hold a duplicate of the listener.
pub(super) struct Watch {
    selector: Rc<Selector>,
    /// The watched descriptor; none for an exit the host reports by PID.
    fd: Option<RawFd>,
    token: u64,
    interest: Ready,
    /// The child exited before its watch was registered.
    exited: bool,
}

impl Watch {
    pub(super) fn set(&mut self, interest: Ready) -> io::Result<()> {
        if self.interest == interest {
            return Ok(());
        }
        // A PID exit watch is one-shot and needs no removal.
        let Some(fd) = self.fd else {
            self.interest = interest;
            return Ok(());
        };
        let token = self.token;
        if interest.is_empty() {
            self.selector.remove(fd)?;
        } else if self.interest.is_empty() {
            self.selector.add(fd, token, interest, Trigger::Level)?;
        } else {
            self.selector.modify(fd, token, interest)?;
        }
        self.interest = interest;
        Ok(())
    }

    pub(super) fn token(&self) -> u64 {
        self.token
    }

    pub(super) fn is_exited(&self) -> bool {
        self.exited
    }

    pub(super) fn fired(&self, events: &[Event]) -> bool {
        self.exited
            || !self.interest.is_empty() && events.iter().any(|event| event.token() == self.token)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.set(Ready::EMPTY);
    }
}
