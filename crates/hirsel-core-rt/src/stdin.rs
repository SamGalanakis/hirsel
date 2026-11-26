//! One helper owns the input stream. A pending read borrows no bytes from a
//! task's stack until the scheduler delivers a fragment to that task.
use crate::{
    Context, entry,
    host::sys::{self, Notice},
    provider::*,
    tasks::Outside,
};
use std::{
    collections::VecDeque,
    io,
    os::fd::{AsRawFd, OwnedFd},
    ptr,
    sync::{Arc, Condvar, Mutex},
};
use zeroize::Zeroizing;

pub(crate) type Until = Option<(Provider, u64, u64)>;
const CHUNK: usize = 64 * 1024;

pub(crate) struct Stream {
    // Retain startup errors, including a closed fd 0, for the first read.
    input: Result<Arc<OwnedFd>, u32>,
    helper: Mutex<Option<Arc<Helper>>>,
    // Taken at the run's first secret read, given back at the run's end.
    handlers: Mutex<Option<crate::term_restore::Handlers>>,
}

struct Helper {
    changed: Notice,
    work: Condvar,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    closed: bool,
    // The helper is reading for the head of `pending`, outside the lock.
    reading: bool,
    pending: VecDeque<Pending>,
    fragment: Option<Fragment>,
}

struct Pending {
    token: u64,
    spare: usize,
    /// A secret read: the keys its line editor takes.
    secret: Option<crate::stdin_secret::Keys>,
    outside: Outside,
}

struct Fragment {
    owner: u64,
    at: u64,
    // Every read is wiped when dropped; a secret line must be.
    result: Result<Zeroizing<Vec<u8>>, u32>,
    offset: usize,
    secret: bool,
}

/// `stdin.read_secret`'s failures, as the helper decides them.
pub(crate) const NOT_TERMINAL: u8 = 1;
pub(crate) const BUSY: u8 = 2;

impl Stream {
    pub(crate) fn new(input: io::Result<OwnedFd>) -> Self {
        Self {
            input: input.map(Arc::new).map_err(errno),
            helper: Mutex::default(),
            handlers: Mutex::default(),
        }
    }

    fn helper(&self) -> Result<Arc<Helper>, u32> {
        let mut helper = self.helper.lock().unwrap();
        if let Some(helper) = &*helper {
            return Ok(Arc::clone(helper));
        }
        let input = self.input.as_ref().map_err(|&code| code)?;
        let started = Helper::start(Arc::clone(input)).map_err(errno)?;
        *helper = Some(Arc::clone(&started));
        Ok(started)
    }

    #[cfg(test)]
    pub(crate) fn has_fragment(&self) -> bool {
        self.helper()
            .unwrap()
            .state
            .lock()
            .unwrap()
            .fragment
            .is_some()
    }
}

impl Stream {
    /// Issues a secret read for `token`: checks the descriptor is a terminal
    /// and no other read is pending, installs the run's handlers, arms the
    /// restore, changes the modes and drops any held read-ahead.
    pub(crate) fn begin_secret(&self, token: u64) -> Result<crate::term_restore::Armed, Outcome> {
        let refuse = |tag| Outcome {
            error: IoError { tag, code: 0 },
            ..Outcome::default()
        };
        let Ok(input) = &self.input else {
            return Err(refuse(NOT_TERMINAL));
        };
        let fd = input.as_raw_fd();
        // SAFETY: a zeroed termios is valid, and the stream owns the descriptor.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(refuse(NOT_TERMINAL));
        }
        let helper = self.helper().map_err(other)?;
        let mut state = helper.state.lock().unwrap();
        if state.reading || !state.pending.is_empty() {
            return Err(refuse(BUSY));
        }
        self.handlers.lock().unwrap().get_or_insert_with(|| {
            crate::term_restore::Handlers::install(crate::stop_signals::relayed())
        });
        let armed = crate::term_restore::arm(fd, &saved).map_err(|error| match error {
            crate::term_restore::ArmError::Busy => refuse(BUSY),
            crate::term_restore::ArmError::Other(code) => other(code),
        })?;
        state.fragment = None;
        state.pending.push_back(Pending {
            token,
            spare: crate::stdin_secret::LINE_MAX + 2,
            secret: Some(crate::stdin_secret::Keys::of(&saved)),
            outside: Outside::begin(),
        });
        drop(state);
        helper.changed();
        Ok(armed)
    }

    /// The line the helper edited for the secret read `token`, once it has
    /// ended, and not when it ended after `deadline`. Delivering echoes the
    /// Enter that ended it, then restores the modes in `armed` before the read
    /// leaves the queue, so the flush has discarded input typed after the line
    /// before a read behind it can start.
    pub(crate) fn take_secret(
        &self,
        token: u64,
        deadline: u64,
        armed: &mut Option<crate::term_restore::Armed>,
    ) -> Option<Result<Zeroizing<Vec<u8>>, u32>> {
        let helper = self.helper().ok()?;
        let mut state = helper.state.lock().unwrap();
        if state.pending.front()?.token != token {
            return None;
        }
        let part = state.fragment.as_ref()?;
        if part.owner != token || !part.secret {
            // Read-ahead from before the modes changed is never the line.
            state.fragment = None;
            drop(state);
            helper.changed();
            return None;
        }
        if deadline != 0 && part.at > deadline {
            return None;
        }
        let part = state.fragment.take()?;
        if let (Ok(input), Ok(line)) = (&self.input, &part.result)
            && line.last() == Some(&b'\n')
        {
            crate::term_restore::newline(input.as_raw_fd());
        }
        armed.take();
        state.remove(token);
        drop(state);
        helper.changed();
        Some(part.result)
    }

    /// Ends the secret read `token` without a line; any bytes read for it go.
    /// As for a delivery, the modes are restored before the read leaves the
    /// queue.
    pub(crate) fn cancel_secret(&self, token: u64, armed: &mut Option<crate::term_restore::Armed>) {
        if let Ok(helper) = self.helper() {
            let mut state = helper.state.lock().unwrap();
            armed.take();
            state.remove(token);
            drop(state);
            helper.changed();
        }
        armed.take();
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // The run ends with a secret read still pending: restore its modes.
        if let Ok(input) = &self.input {
            crate::term_restore::release_fd(input.as_raw_fd());
        }
        if let Some(helper) = self.helper.get_mut().unwrap().take() {
            helper.state.lock().unwrap().closed = true;
            helper.changed();
        }
    }
}

impl Helper {
    fn start(input: Arc<OwnedFd>) -> io::Result<Arc<Self>> {
        let helper = Arc::new(Self {
            changed: Notice::new(true)?,
            work: Condvar::new(),
            state: Mutex::default(),
        });
        let worker = Arc::clone(&helper);
        std::thread::Builder::new()
            .name("hirsel-stdin".into())
            .stack_size(CHUNK)
            .spawn(move || worker.run(&input))?;
        Ok(helper)
    }

    fn changed(&self) {
        self.work.notify_one();
        // A full notice is already readable, so the helper still sees the change.
        let _ = crate::os_net::retry(|| sys::post(self.changed.writer()));
    }

    fn run(&self, input: &OwnedFd) {
        // The secret line being edited, and the read it is for.
        let mut editing: Option<(u64, crate::stdin_secret::Editor)> = None;
        loop {
            let mut state = self.state.lock().unwrap();
            // A line whose read has ended without it is wiped before waiting.
            if editing.as_ref().is_some_and(|(owner, _)| {
                state
                    .pending
                    .front()
                    .is_none_or(|first| first.token != *owner)
            }) {
                editing = None;
            }
            while !state.closed && (state.pending.is_empty() || state.fragment.is_some()) {
                state = self.work.wait(state).unwrap();
            }
            if state.closed {
                return;
            }
            drop(state);
            let mut fds = [
                libc::pollfd {
                    fd: self.changed.reader(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: input.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: both owned fds and both writable pollfd rows live through poll.
            let poll =
                crate::os_net::retry(|| unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) as isize });
            if fds[0].revents != 0 {
                // The helper is the notice's only reader.
                let _ = crate::os_net::retry(|| sys::take(self.changed.reader()));
            }
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return;
            }
            let Some(first) = state.pending.front() else {
                continue;
            };
            let (owner, spare, secret) = (first.token, first.spare, first.secret);
            let result = match poll {
                Err(code) => Err(code as u32),
                Ok(_) if fds[1].revents == 0 => continue,
                Ok(_) => {
                    // A secret read cannot begin while this read is outside the lock.
                    state.reading = true;
                    drop(state);
                    let mut bytes = Zeroizing::new(vec![0; spare.min(CHUNK)]);
                    let read = crate::os_net::retry(|| unsafe {
                        libc::read(input.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
                    });
                    state = self.state.lock().unwrap();
                    state.reading = false;
                    match read {
                        Ok(n) => {
                            bytes.truncate(n as usize);
                            Ok(bytes)
                        }
                        // The inherited description may already be nonblocking.
                        Err(libc::EAGAIN) => continue,
                        Err(code) => Err(code as u32),
                    }
                }
            };
            let at = crate::os::monotonic_ns();
            let owned = state
                .pending
                .front()
                .is_some_and(|first| first.token == owner);
            // A secret read's bytes go to its line editor, and only the ended
            // line is delivered. Bytes for a read that has ended are wiped,
            // never kept as read-ahead.
            let result = match (secret, result) {
                (Some(_), _) if !owned => continue,
                (Some(keys), Ok(typed)) => {
                    if editing.as_ref().is_none_or(|(editor, _)| *editor != owner) {
                        editing = Some((owner, crate::stdin_secret::Editor::new(keys)));
                    }
                    let (_, editor) = editing.as_mut().unwrap();
                    if !editor.feed(&typed) {
                        continue;
                    }
                    Ok(editing.take().unwrap().1.into_line())
                }
                (_, result) => result,
            };
            let secret = secret.is_some();
            if (!secret && result.as_ref().is_ok_and(|bytes| !bytes.is_empty())) || owned {
                state.fragment = Some(Fragment {
                    owner,
                    at,
                    result,
                    offset: 0,
                    secret,
                });
                state.signal_front();
            }
        }
    }
}

impl State {
    fn signal_front(&self) {
        if self.fragment.is_some()
            && let Some(first) = self.pending.front()
        {
            first.outside.ready(first.token);
        }
    }

    fn remove(&mut self, token: u64) {
        let removed_head = self
            .pending
            .front()
            .is_some_and(|entry| entry.token == token);
        if removed_head {
            self.pending.pop_front();
        } else {
            self.pending.retain(|entry| entry.token != token);
        }
        if self.fragment.as_ref().is_some_and(|part| {
            part.owner == token
                && (part.secret || part.result.as_ref().map_or(true, |bytes| bytes.is_empty()))
        }) {
            self.fragment = None;
        }
        if removed_head {
            self.signal_front();
        }
    }

    fn deliver(&mut self, token: u64, deadline: u64, buffer: &mut Buffer) -> Option<Outcome> {
        if self.pending.front()?.token != token {
            return None;
        }
        let part = self.fragment.as_mut()?;
        if deadline != 0 && part.at > deadline {
            return None;
        }
        let value = match &part.result {
            Ok(bytes) => {
                let source = &bytes[part.offset..];
                let n = source.len().min(buffer.capacity - buffer.len);
                if n != 0 {
                    // SAFETY: n bytes fit the pinned buffer's spare extent.
                    unsafe {
                        ptr::copy_nonoverlapping(source.as_ptr(), buffer.data.add(buffer.len), n)
                    };
                    part.offset += n;
                    buffer.len += n;
                }
                if part.offset == bytes.len() {
                    self.fragment = None;
                }
                Outcome {
                    value: n as u64,
                    ..Outcome::default()
                }
            }
            Err(code) => {
                let value = other(*code);
                self.fragment = None;
                value
            }
        };
        self.remove(token);
        Some(value)
    }
}

fn errno(error: io::Error) -> u32 {
    error.raw_os_error().unwrap_or(libc::EIO) as u32
}

pub(crate) fn other(code: u32) -> Outcome {
    Outcome {
        error: IoError { tag: 8, code },
        ..Outcome::default()
    }
}

pub(crate) struct ReadRequest {
    pub(crate) until: Until,
    pub(crate) deadline: u64,
    operations: &'static Operations,
}

impl ReadRequest {
    pub(crate) fn new(until: Until) -> Self {
        Self {
            until,
            deadline: 0,
            operations: &OPERATIONS,
        }
    }
}

pub(crate) struct Operations {
    pub(crate) step: unsafe fn(*mut Request, u64, bool) -> Step,
    pub(crate) abort: unsafe fn(*mut Request, u64, u8) -> Outcome,
}

static OPERATIONS: Operations = Operations { step, abort };

pub(crate) unsafe fn operations(request: *const Request) -> &'static Operations {
    // SAFETY: the Stdin adapter pins the private ReadRequest through settlement.
    unsafe { (*(*request).path.cast::<ReadRequest>()).operations }
}

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

unsafe fn step(request: *mut Request, token: u64, issue: bool) -> Step {
    // SAFETY: the adapter pins ReadRequest and the carrier, and the live OS
    // grant supplies a Stream for this run. The worker never borrows them.
    unsafe {
        let request = &mut *request;
        if request.buffer.len >= request.buffer.capacity {
            return done(Outcome::default());
        }
        let read = &mut *request.path.cast_mut().cast::<ReadRequest>();
        let due = crate::os_net::deadline(read.until, token);
        if issue {
            read.deadline = due.host().unwrap_or(0);
        }
        let stream = &*(request.handle as *const Stream);
        let helper = match stream.helper() {
            Ok(helper) => helper,
            Err(code) => return done(other(code)),
        };
        let mut state = helper.state.lock().unwrap();
        if issue {
            state.pending.push_back(Pending {
                token,
                spare: request.buffer.capacity - request.buffer.len,
                secret: None,
                outside: Outside::begin(),
            });
        }
        let outcome = state
            .deliver(token, read.deadline, &mut request.buffer)
            .or_else(|| {
                (due == crate::os_net::Due::Passed).then(|| {
                    state.remove(token);
                    crate::os_net::signal(6)
                })
            });
        if issue || outcome.is_some() {
            helper.changed();
        }
        outcome.map_or(
            Step {
                kind: STEP_PENDING,
                deadline_ns: read.deadline,
                ..Step::default()
            },
            done,
        )
    }
}

unsafe fn abort(request: *mut Request, token: u64, reason: u8) -> Outcome {
    // SAFETY: a pending OS read pins its stream and both operands until abort.
    unsafe {
        let request = &mut *request;
        let helper = (&*(request.handle as *const Stream)).helper().unwrap();
        let mut state = helper.state.lock().unwrap();
        let outcome = if reason == ABORT_DEADLINE {
            let deadline = (*request.path.cast::<ReadRequest>()).deadline;
            state.deliver(token, deadline, &mut request.buffer)
        } else {
            None
        }
        .unwrap_or_else(|| {
            state.remove(token);
            crate::os_net::signal(if reason == ABORT_CANCEL { 7 } else { 6 })
        });
        helper.changed();
        outcome
    }
}

/// # Safety
/// The grants belong to the live run and operand is an exclusive, pinned Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_stdin_read(
    context: *mut Context,
    grant: u64,
    operand: *mut Buffer,
    until_kind: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> Outcome {
    unsafe {
        let mut read = ReadRequest::new(crate::net::deadline(context, until_kind, clock, at));
        let mut request = Request {
            operation: STDIN_READ,
            mode: 0,
            handle: entry::hirsel_core_capability_handle(context, grant),
            path: ptr::from_mut(&mut read).cast(),
            path_len: 0,
            buffer: *operand,
            site,
        };
        let provider = entry::hirsel_core_capability_provider(context, grant);
        let answer = crate::provider::drive(context, provider, &mut request, None);
        (*operand).len = request.buffer.len;
        answer
    }
}
