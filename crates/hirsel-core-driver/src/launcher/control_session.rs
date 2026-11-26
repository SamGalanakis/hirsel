//! Operator/control session framing, peer admission, request decoding, and event flush.
//!
//! All control-socket framing and wire decoding live here so the launcher's session
//! boundary stays in one place.
use super::bootstrap;
use super::ready::{NONE, READ, Readiness, ReadyEvent, WRITE, Watch};
#[cfg(test)]
use super::session::Child;
use super::session::{self, Event, Request, Wire};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
#[cfg(test)]
use std::os::fd::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};

/// A nonblocking framed stream: the control session, or a serving child's endpoint.
pub(super) struct Conn {
    pub(super) watch: Watch,
    pub(super) stream: UnixStream,
    pub(super) input: Vec<u8>,
    pub(super) output: Vec<u8>,
    pub(super) closed: bool,
    /// Reading is held back while an admission's input waits for its worker.
    pub(super) paused: bool,
}

impl Conn {
    pub(super) fn new(poll: &Readiness, stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            watch: poll.watch(stream.as_raw_fd()),
            stream,
            input: Vec::new(),
            output: Vec::new(),
            closed: false,
            paused: false,
        })
    }

    pub(super) fn watch(&mut self) -> io::Result<()> {
        let read = if self.paused { NONE } else { READ };
        let interest = if self.closed {
            NONE
        } else if self.output.is_empty() {
            read
        } else {
            read | WRITE
        };
        self.watch.set(interest)
    }

    /// Read what is ready. A peer that closed or failed ends the stream.
    pub(super) fn fill(&mut self) {
        let mut buffer = [0; 1 << 16];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => return self.closed = true,
                Ok(n) => self.input.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return self.closed = true,
            }
        }
    }

    pub(super) fn frame(&mut self) -> Option<Vec<u8>> {
        let size = u32::from_le_bytes(self.input.get(..4)?.try_into().unwrap()) as usize;
        if size > session::MAX_FRAME {
            self.closed = true;
            return None;
        }
        let body = self.input.get(4..4 + size)?.to_vec();
        self.input.drain(..4 + size);
        Some(body)
    }

    pub(super) fn queue(&mut self, value: &impl Wire) {
        self.closed |= session::write_frame(&mut self.output, value).is_err();
    }

    /// Write what the peer takes now; a stalled peer never blocks the loop.
    pub(super) fn flush(&mut self) {
        while !self.output.is_empty() && !self.closed {
            match self.stream.write(&self.output) {
                Ok(n) => drop(self.output.drain(..n)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => self.closed = true,
            }
        }
    }
}

/// Which local process may hold the control session.
#[derive(Clone, Copy)]
pub(super) enum PeerPolicy {
    /// Any local peer able to connect to the control socket.
    Any,
    /// Only the bootstrap host process with this PID (or none while no host is live).
    Only(Option<libc::pid_t>),
}

/// The launcher's single active operator/host control session and outbound event queue.
#[derive(Default)]
pub(super) struct ControlSession {
    control_watch: Option<Watch>,
    control: Option<UnixListener>,
    session: Option<Conn>,
    out: Vec<Event>,
}

impl ControlSession {
    /// Serve the control session on `listener`; the socket's permission is the host's.
    pub(super) fn listen(&mut self, poll: &Readiness, listener: UnixListener) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        let mut watch = poll.watch(listener.as_raw_fd());
        watch.set(READ)?;
        self.control_watch = Some(watch);
        self.control = Some(listener);
        Ok(())
    }

    /// Update the active session socket's readiness interest for the upcoming poll wait.
    pub(super) fn prepare_watch(&mut self, paused: bool) -> io::Result<()> {
        if let Some(conn) = &mut self.session {
            conn.paused = paused;
            conn.watch()?;
        }
        Ok(())
    }

    /// Accept any ready control connections under `policy`. Returns `true` when a new
    /// connection replaced the session (clearing unsent outbound events).
    pub(super) fn accept_if_ready(
        &mut self,
        poll: &Readiness,
        events: &[ReadyEvent],
        policy: PeerPolicy,
    ) -> bool {
        if !self
            .control_watch
            .as_ref()
            .is_some_and(|watch| watch.fired(events))
        {
            return false;
        }
        let mut accepted = None;
        if let Some(control) = &self.control {
            while let Ok((stream, _)) = control.accept() {
                // With a bootstrap host, a connection from any other process is closed
                // before it can replace the session (launch.md "The operator socket").
                let allowed = match policy {
                    PeerPolicy::Any => true,
                    PeerPolicy::Only(expected) => {
                        expected.is_some() && bootstrap::peer_pid(&stream) == expected
                    }
                };
                if !allowed {
                    continue;
                }
                accepted = Conn::new(poll, stream).ok().or(accepted);
            }
        }
        if let Some(conn) = accepted {
            self.session = Some(conn);
            self.out.clear();
            true
        } else {
            false
        }
    }

    /// Read available bytes from the control session unless admission backpressure pauses it.
    pub(super) fn fill(&mut self, paused: bool) {
        if let Some(conn) = &mut self.session
            && !paused
        {
            conn.fill();
        }
    }

    /// Decode the next complete request frame from the control session. Frames that
    /// arrived before the peer closed are still served; a malformed frame marks the
    /// session closed and `flush` drops it.
    pub(super) fn next_request(&mut self) -> Option<Request> {
        let conn = self.session.as_mut()?;
        loop {
            let frame = conn.frame()?;
            match Request::decode(&frame) {
                Ok(request) => return Some(request),
                Err(_) => conn.closed = true,
            }
        }
    }

    pub(super) fn emit(&mut self, event: Event) {
        self.out.push(event);
    }

    pub(super) fn extend(&mut self, events: impl IntoIterator<Item = Event>) {
        self.out.extend(events);
    }

    pub(super) fn clear_out(&mut self) {
        self.out.clear();
    }

    /// Queue and flush outbound events to the active session. Returns `true` if the
    /// session closed and was dropped.
    pub(super) fn flush(&mut self) -> bool {
        let Some(conn) = &mut self.session else {
            return false;
        };
        for event in self.out.drain(..) {
            conn.queue(&event);
        }
        conn.flush();
        if conn.closed {
            self.session = None;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(super) fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.out)
    }

    #[cfg(test)]
    pub(super) fn take_start_reply(&mut self, child: &Child) -> Option<Event> {
        let pos = self.out.iter().position(
            |ev| matches!(ev, Event::Launched(c, _) | Event::Refused(c, _) if c == child),
        )?;
        Some(self.out.remove(pos))
    }

    #[cfg(test)]
    pub(super) fn has_session(&self) -> bool {
        self.session.is_some()
    }

    #[cfg(test)]
    pub(super) fn control_fd(&self) -> Option<RawFd> {
        self.control.as_ref().map(AsRawFd::as_raw_fd)
    }
}
