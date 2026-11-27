//! The embedder's side of the helper: spawn requests, and a reader thread
//! that routes the helper's replies and child exits to their waiters.
use super::spawn::{self, Started};
use super::wire::{self, Fields, Frame, Inbox};
use crate::host::exec::{Signal, SpawnError, SpawnInput, Termination};
use crate::run_stop::{Execution, Stop};
use crate::tasks::Outside;
use std::collections::HashMap;
use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

type Pipes = [Option<OwnedFd>; 3];

enum Reply {
    Spawned(Pipes),
    Failed(io::Error),
}

#[derive(Default)]
struct Entry {
    ended: Option<Result<Termination, i32>>,
    waiter: Option<(Outside, u64)>,
}

#[derive(Default)]
struct State {
    next: u64,
    procs: HashMap<u64, Entry>,
    reply: Option<Reply>,
    lost: bool,
}

struct Shared {
    socket: OwnedFd,
    send: Mutex<()>,
    state: Mutex<State>,
    replied: Condvar,
}

impl Shared {
    fn send(&self, frame: Frame, fds: &[i32]) -> io::Result<()> {
        let _turn = self.send.lock().unwrap();
        frame.send(self.socket.as_raw_fd(), fds)
    }
}

/// A running helper. Drop closes the socket, which the helper answers by
/// releasing every child it holds and ending; Drop then reaps it.
pub(crate) struct Helper {
    shared: Arc<Shared>,
    spawn_slot: Mutex<()>,
    closing: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    started: Option<Started>,
}

impl Helper {
    /// Start the helper at `image` and wait up to `timeout` for its startup
    /// verdict: its Supervisor's startup checks run in the helper and their
    /// refusal is this one. A helper that is missing, denied, silent or of
    /// another build is an `ExecHelper(..)` refusal. The helper's unexpected
    /// end later records the `ExecSupervisor` run stop on `generation`.
    pub(crate) fn start(
        image: &CStr,
        timeout: Duration,
        generation: Arc<Execution>,
    ) -> Result<Arc<Self>, String> {
        let path = image.to_string_lossy();
        let fail = |error: io::Error| format!("ExecHelper({path}: {error})");
        let (ours, theirs) = wire::pair().map_err(fail)?;
        let started = spawn::start(image, &theirs).map_err(|error| match error.raw_os_error() {
            Some(libc::ENOENT | libc::ENOTDIR) => format!("ExecHelper(missing: {path})"),
            Some(libc::EACCES | libc::EPERM) => format!("ExecHelper(denied: {path})"),
            _ => fail(error),
        })?;
        drop(theirs);
        let mut inbox = Inbox::new(ours.as_raw_fd());
        let verdict = receive_timeout(&ours, Some(timeout))
            .map_err(fail)
            .and_then(|()| verdict(&mut inbox))
            .and_then(|()| receive_timeout(&ours, None).map_err(fail));
        if let Err(error) = verdict {
            started.kill();
            drop(ours);
            started.reap();
            return Err(error);
        }
        let shared = Arc::new(Shared {
            socket: ours,
            send: Mutex::new(()),
            state: Mutex::new(State::default()),
            replied: Condvar::new(),
        });
        let closing = Arc::new(AtomicBool::new(false));
        let reader = {
            let shared = shared.clone();
            let closing = closing.clone();
            thread::Builder::new()
                .name("hirsel-exec-helper".into())
                .spawn(move || read(&shared, inbox, &closing, &generation))
                .map_err(fail)?
        };
        Ok(Arc::new(Self {
            shared,
            spawn_slot: Mutex::new(()),
            closing,
            reader: Some(reader),
            started: Some(started),
        }))
    }

    pub(super) fn spawn(&self, input: SpawnInput) -> Result<(Pipes, Proc), SpawnError> {
        crate::units::off_slot(crate::units::HostCall::Bounded, || self.spawn_now(input))
    }

    fn spawn_now(&self, input: SpawnInput) -> Result<(Pipes, Proc), SpawnError> {
        let _slot = self.spawn_slot.lock().unwrap();
        let id = {
            let mut state = self.shared.state.lock().unwrap();
            if state.lost {
                return Err(io::Error::from_raw_os_error(libc::EIO).into());
            }
            state.next += 1;
            state.next
        };
        let piped = input
            .piped
            .iter()
            .enumerate()
            .fold(0u8, |mask, (at, &on)| mask | u8::from(on) << at);
        let mut frame = Frame::new(wire::SPAWN).u64(id).u8(piped);
        frame = match input.pin {
            Some(pin) => frame.u8(1).bytes(&pin.encode()),
            None => frame.u8(0),
        };
        frame = frame
            .bytes(input.image.to_bytes())
            .u64(input.argv.len() as u64);
        for arg in &input.argv {
            frame = frame.bytes(arg.as_bytes());
        }
        frame = frame.u64(input.envp.len() as u64);
        for var in &input.envp {
            frame = frame.bytes(var.as_bytes());
        }
        self.shared
            .state
            .lock()
            .unwrap()
            .procs
            .insert(id, Entry::default());
        let sent = self.shared.send(frame, &[input.cwd.as_raw_fd()]);
        drop(input);
        let mut state = self.shared.state.lock().unwrap();
        let reply = match sent {
            Ok(()) => {
                state = self
                    .shared
                    .replied
                    .wait_while(state, |s| s.reply.is_none() && !s.lost)
                    .unwrap();
                state
                    .reply
                    .take()
                    .unwrap_or_else(|| Reply::Failed(io::Error::from_raw_os_error(libc::EIO)))
            }
            Err(error) => Reply::Failed(error),
        };
        match reply {
            Reply::Spawned(pipes) => Ok((
                pipes,
                Proc {
                    shared: self.shared.clone(),
                    id,
                    termination: None,
                },
            )),
            Reply::Failed(error) => {
                state.procs.remove(&id);
                Err(error.into())
            }
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        // SAFETY: a live socket; shutdown ends both directions for the peer.
        unsafe { libc::shutdown(self.shared.socket.as_raw_fd(), libc::SHUT_RDWR) };
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(started) = self.started.take() {
            started.reap();
        }
    }
}

/// The startup verdict: `READY` with this build's identity, or the refusal.
fn verdict(inbox: &mut Inbox) -> Result<(), String> {
    let want = wire::identity();
    match inbox.next() {
        Ok(Some((wire::READY, fields))) => match Fields(&fields).bytes() {
            Ok(got) if got == want.as_bytes() => Ok(()),
            got => Err(format!(
                "ExecHelper(version: got {}, want {want})",
                String::from_utf8_lossy(got.unwrap_or_default())
            )),
        },
        Ok(Some((wire::REFUSED, fields))) => Err(Fields(&fields)
            .bytes()
            .map(|text| String::from_utf8_lossy(text).into_owned())
            .unwrap_or_else(|_| "ExecHelper(refused)".into())),
        Ok(_) => Err("ExecHelper(ended during startup)".into()),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            Err("ExecHelper(no answer)".into())
        }
        Err(error) => Err(format!("ExecHelper({error})")),
    }
}

/// Bound (or, with `None`, unbound) each receive on `socket`.
fn receive_timeout(socket: &OwnedFd, timeout: Option<Duration>) -> io::Result<()> {
    let timeout = timeout.unwrap_or_default();
    let value = libc::timeval {
        tv_sec: timeout.as_secs().min(i32::MAX as u64) as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    // SAFETY: SO_RCVTIMEO reads one timeval.
    let set = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw const value).cast(),
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    if set < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read(shared: &Shared, mut inbox: Inbox, closing: &AtomicBool, generation: &Execution) {
    while let Ok(Some(frame)) = inbox.next() {
        if route(shared, &mut inbox, frame).is_err() {
            break;
        }
    }
    let waiters: Vec<_> = {
        let mut state = shared.state.lock().unwrap();
        state.lost = true;
        state
            .procs
            .values_mut()
            .filter_map(|entry| entry.waiter.take())
            .collect()
    };
    shared.replied.notify_all();
    for (outside, token) in waiters {
        outside.complete(token);
    }
    if !closing.load(Ordering::SeqCst) {
        // This reader runs no part of the generation, so it wakes its workers.
        generation.record_stop(Stop::from_static(b"ExecSupervisor"));
        crate::tasks::wake_all_waiters();
        crate::units::wake_all_for_stop();
    }
}

fn route(shared: &Shared, inbox: &mut Inbox, (tag, bytes): (u8, Vec<u8>)) -> io::Result<()> {
    let mut fields = Fields(&bytes);
    match tag {
        wire::SPAWNED => {
            let _id = fields.u64()?;
            let mask = fields.u8()?;
            let mut pipes: Pipes = [None, None, None];
            for (at, pipe) in pipes.iter_mut().enumerate() {
                if mask & (1 << at) != 0 {
                    *pipe = Some(inbox.fd()?);
                }
            }
            shared.state.lock().unwrap().reply = Some(Reply::Spawned(pipes));
            shared.replied.notify_all();
        }
        wire::SPAWN_FAILED => {
            let _id = fields.u64()?;
            let errno = fields.u64()? as i32;
            shared.state.lock().unwrap().reply =
                Some(Reply::Failed(io::Error::from_raw_os_error(errno)));
            shared.replied.notify_all();
        }
        wire::EXITED | wire::WAIT_FAILED => {
            let id = fields.u64()?;
            let ended = if tag == wire::EXITED {
                let kind = fields.u8()?;
                let code = fields.u8()?;
                Ok(if kind == 0 {
                    Termination::Exited(code)
                } else {
                    Termination::Signalled(code)
                })
            } else {
                Err(fields.u64()? as i32)
            };
            let waiter = {
                let mut state = shared.state.lock().unwrap();
                state.procs.get_mut(&id).and_then(|entry| {
                    entry.ended = Some(ended);
                    entry.waiter.take()
                })
            };
            if let Some((outside, token)) = waiter {
                outside.complete(token);
            }
        }
        _ => return Err(io::Error::from_raw_os_error(libc::EIO)),
    }
    Ok(())
}

/// A child the helper holds for this embedder.
pub(crate) struct Proc {
    shared: Arc<Shared>,
    id: u64,
    termination: Option<Termination>,
}

impl Proc {
    pub(super) fn wait_or_park(&mut self, token: Option<u64>) -> io::Result<Option<Termination>> {
        if self.termination.is_some() {
            return Ok(self.termination);
        }
        let mut state = self.shared.state.lock().unwrap();
        let lost = state.lost;
        let entry = state
            .procs
            .get_mut(&self.id)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ECHILD))?;
        match entry.ended {
            Some(Ok(status)) => {
                entry.waiter = None;
                self.termination = Some(status);
            }
            Some(Err(errno)) => return Err(io::Error::from_raw_os_error(errno)),
            None if lost => return Err(io::Error::from_raw_os_error(libc::EIO)),
            None => {
                if let Some(token) = token {
                    entry.waiter = Some((Outside::begin(), token));
                }
            }
        }
        Ok(self.termination)
    }

    pub(super) fn withdraw_wait(&self) {
        if let Some(entry) = self.shared.state.lock().unwrap().procs.get_mut(&self.id) {
            entry.waiter = None;
        }
    }

    /// The helper signals the child unless it has been reaped. A lost helper
    /// has already killed it.
    pub(super) fn signal(&mut self, signal: Signal) -> io::Result<()> {
        if self.termination.is_some() {
            return Ok(());
        }
        let frame = Frame::new(wire::SIGNAL)
            .u64(self.id)
            .u8(super::signal_code(signal));
        match self.shared.send(frame, &[]) {
            Err(error) if self.shared.state.lock().unwrap().lost => {
                drop(error);
                Ok(())
            }
            sent => sent,
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let entry = self.shared.state.lock().unwrap().procs.remove(&self.id);
        let reaped = entry.is_some_and(|entry| entry.ended.is_some());
        if !reaped {
            // A helper that is gone has already killed the child.
            let _ = self
                .shared
                .send(Frame::new(wire::RELEASE).u64(self.id), &[]);
        }
    }
}
