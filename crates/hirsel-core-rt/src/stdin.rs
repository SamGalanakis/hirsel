//! One helper owns the input stream. A pending read borrows no bytes from a
//! task's stack until the scheduler delivers a fragment to that task.
use crate::{Context, entry, provider::*, tasks::Outside};
use std::{
    collections::VecDeque,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
    sync::{Arc, Condvar, Mutex},
};

pub(crate) type Until = Option<(Provider, u64, u64)>;
const CHUNK: usize = 64 * 1024;

pub(crate) struct Stream {
    // Retain startup errors, including a closed fd 0, for the first read.
    input: Result<Arc<OwnedFd>, u32>,
    helper: Mutex<Option<Arc<Helper>>>,
}

struct Helper {
    changed: OwnedFd,
    work: Condvar,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    closed: bool,
    pending: VecDeque<Pending>,
    fragment: Option<Fragment>,
}

struct Pending {
    token: u64,
    spare: usize,
    outside: Outside,
}

struct Fragment {
    owner: u64,
    at: u64,
    result: Result<Vec<u8>, u32>,
    offset: usize,
}

impl Stream {
    pub(crate) fn new(input: io::Result<OwnedFd>) -> Self {
        Self {
            input: input.map(Arc::new).map_err(errno),
            helper: Mutex::default(),
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

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(helper) = self.helper.get_mut().unwrap().take() {
            helper.state.lock().unwrap().closed = true;
            helper.changed();
        }
    }
}

impl Helper {
    fn start(input: Arc<OwnedFd>) -> io::Result<Arc<Self>> {
        // SAFETY: eventfd either returns an owned fd or signals a host error.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let helper = Arc::new(Self {
            changed: unsafe { OwnedFd::from_raw_fd(fd) },
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
        // SAFETY: the eventfd borrows eight bytes for this one write. A full
        // counter is already readable, so the helper still sees the change.
        let _ = crate::os_net::retry(|| unsafe {
            libc::write(
                self.changed.as_raw_fd(),
                1u64.to_ne_bytes().as_ptr().cast(),
                8,
            )
        });
    }

    fn run(&self, input: &OwnedFd) {
        loop {
            let mut state = self.state.lock().unwrap();
            while !state.closed && (state.pending.is_empty() || state.fragment.is_some()) {
                state = self.work.wait(state).unwrap();
            }
            if state.closed {
                return;
            }
            drop(state);
            let mut fds = [
                libc::pollfd {
                    fd: self.changed.as_raw_fd(),
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
                let mut count = 0u64;
                // SAFETY: the helper is the only eventfd reader and count holds eight bytes.
                let _ = crate::os_net::retry(|| unsafe {
                    libc::read(
                        self.changed.as_raw_fd(),
                        ptr::from_mut(&mut count).cast(),
                        8,
                    )
                });
            }
            let state = self.state.lock().unwrap();
            if state.closed {
                return;
            }
            let Some(first) = state.pending.front() else {
                continue;
            };
            let owner = first.token;
            let spare = first.spare;
            drop(state);
            let result = match poll {
                Err(code) => Err(code as u32),
                Ok(_) if fds[1].revents == 0 => continue,
                Ok(_) => {
                    let mut bytes = vec![0; spare.min(CHUNK)];
                    match crate::os_net::retry(|| unsafe {
                        libc::read(input.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
                    }) {
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
            let mut state = self.state.lock().unwrap();
            if result.as_ref().is_ok_and(|bytes| !bytes.is_empty())
                || state
                    .pending
                    .front()
                    .is_some_and(|first| first.token == owner)
            {
                state.fragment = Some(Fragment {
                    owner,
                    at,
                    result,
                    offset: 0,
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
            part.owner == token && part.result.as_ref().map_or(true, Vec::is_empty)
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

fn other(code: u32) -> Outcome {
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
        if issue {
            read.deadline = crate::os_net::host_deadline(read.until).unwrap_or(0);
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
                outside: Outside::begin(),
            });
        }
        let outcome = state
            .deliver(token, read.deadline, &mut request.buffer)
            .or_else(|| {
                (read.deadline != 0 && read.deadline <= crate::os::monotonic_ns()).then(|| {
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
