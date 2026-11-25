pub(super) use hirsel_core_net::ready::Event as ReadyEvent;
use hirsel_core_net::ready::{Event, Ready, Selector, Trigger};
use std::io;
use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::Duration;

pub(super) const READ: Ready = Ready::READABLE;
pub(super) const WRITE: Ready = Ready::WRITABLE;
pub(super) const CHANGE: Ready = Ready::CHANGED;
pub(super) const NONE: Ready = Ready::EMPTY;

/// The launcher's readiness set; each watch is keyed by its descriptor.
pub(super) struct Readiness(Rc<Selector>);

impl Readiness {
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self(Rc::new(Selector::new()?)))
    }

    pub(super) fn watch(&self, fd: RawFd) -> Watch {
        Watch {
            selector: Rc::clone(&self.0),
            fd,
            interest: Ready::EMPTY,
        }
    }

    /// An interrupted wait reports no events.
    pub(super) fn wait(
        &self,
        timeout: Option<Duration>,
        events: &mut [Event],
    ) -> io::Result<usize> {
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
    fd: RawFd,
    interest: Ready,
}

impl Watch {
    pub(super) fn set(&mut self, interest: Ready) -> io::Result<()> {
        if self.interest == interest {
            return Ok(());
        }
        let token = self.fd as u64;
        if interest.is_empty() {
            self.selector.remove(self.fd)?;
        } else if self.interest.is_empty() {
            self.selector
                .add(self.fd, token, interest, Trigger::Level)?;
        } else {
            self.selector.modify(self.fd, token, interest)?;
        }
        self.interest = interest;
        Ok(())
    }

    pub(super) fn fired(&self, events: &[Event]) -> bool {
        !self.interest.is_empty() && events.iter().any(|event| event.token() == self.fd as u64)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.set(Ready::EMPTY);
    }
}
