//! Linux try-first socket attempts. Go's SO_ERROR + getpeername sequence rejects
//! spurious writable connect events; combined interests live in core-net.
mod addr;
pub(crate) use addr::is_operation;
mod tls_server;
use addr::{address, decoded, query_address};
pub(crate) use tls_server::{ACCEPT_OPERATIONS, HELLO_OPERATIONS};

use crate::{
    entry::SocketActivation,
    host::inherited::{adopt_listener, receive_listener, receive_operator},
    host::trust::{platform_tls, system_roots},
    net::{Address, NetworkRequest, RulesStorage},
    os_tls::{Handshake, Stream},
    provider::*,
    tls::{HandshakeState, TlsRequest},
    units::{HostCall, off_slot},
};
use std::{
    io,
    mem::{size_of, zeroed},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr,
};

/// Installed by the native Net adapters on their pinned request. The common
/// OS provider can forward a socket operation without linking its backend.
pub(crate) struct Operations {
    pub step: unsafe fn(*mut Request, u64, bool) -> Step,
    pub abort: unsafe fn(*mut Request, u64, u8) -> Outcome,
    pub adopt_listener: unsafe fn(*mut Request, OwnedFd) -> Step,
}

pub(crate) static OPERATIONS: Operations = Operations {
    step,
    abort,
    adopt_listener,
};

pub(crate) static TLS_OPERATIONS: Operations = Operations {
    step: tls_step,
    abort: tls_abort,
    adopt_listener,
};

#[unsafe(no_mangle)]
pub static hirsel_core_os_socket_activation: SocketActivation = SocketActivation {
    receive: receive_listener,
    rules: RulesStorage::from_views,
    unix_probe: hirsel_core_net::unix::probe,
    unix_roots: crate::os_unix::configure_roots,
    operator: receive_operator,
};

fn errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
pub(crate) fn retry(mut call: impl FnMut() -> isize) -> Result<isize, i32> {
    loop {
        let rc = call();
        if rc >= 0 {
            return Ok(rc);
        }
        let e = errno();
        if e != libc::EINTR {
            return Err(e);
        }
    }
}
pub(crate) fn error(code: i32) -> Outcome {
    let tag = match code {
        libc::EACCES => 1,
        libc::ECONNREFUSED => 2,
        libc::ENETUNREACH | libc::EHOSTUNREACH | libc::ETIMEDOUT => 3,
        _ => 8,
    };
    Outcome {
        error: IoError {
            tag,
            code: if tag == 8 { code as u32 } else { 0 },
        },
        ..Outcome::default()
    }
}
pub(crate) fn signal(tag: u8) -> Outcome {
    Outcome {
        error: IoError { tag, code: 0 },
        ..Outcome::default()
    }
}

pub(crate) unsafe fn register_socket(n: &mut NetworkRequest, owned: OwnedFd) -> Result<u64, i32> {
    // SAFETY: the request pins the registry through issue.
    unsafe { adopt_socket(n.sockets, owned) }.map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
}

pub(crate) unsafe fn adopt_socket(
    sockets: *mut hirsel_core_net::Sockets,
    owned: OwnedFd,
) -> io::Result<u64> {
    // SAFETY: the run's one worker calls this, and the run pins the socket registry.
    unsafe {
        let poller = (*sockets).poller()?;
        crate::tasks::attach_net(sockets, poller);
        Ok((*sockets).insert(owned).unwrap_or_else(|| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostHandles"))
        }))
    }
}

fn options(fd: i32, family: u8, listener: bool, accepted: bool) -> Result<(), i32> {
    let one = 1i32;
    for (level, name) in [
        (libc::IPPROTO_TCP, libc::TCP_NODELAY),
        (
            if family == 1 {
                libc::IPPROTO_IPV6
            } else {
                libc::SOL_SOCKET
            },
            if family == 1 {
                libc::IPV6_V6ONLY
            } else {
                libc::SO_REUSEADDR
            },
        ),
    ] {
        if (level == libc::SOL_SOCKET && !listener) || (accepted && level != libc::IPPROTO_TCP) {
            continue;
        }
        // SAFETY: borrowed descriptor and initialized scalar socket option.
        retry(|| unsafe {
            libc::setsockopt(
                fd,
                level,
                name,
                (&one as *const i32).cast(),
                size_of::<i32>() as u32,
            ) as isize
        })?;
    }
    if family == 1 && listener {
        retry(|| unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                (&one as *const i32).cast(),
                4,
            ) as isize
        })?;
    }
    Ok(())
}
unsafe fn socket(n: &mut NetworkRequest, listener: bool) -> Result<u64, i32> {
    unsafe {
        let domain = if n.address.family == 0 {
            libc::AF_INET
        } else {
            libc::AF_INET6
        };
        let fd = retry(|| crate::host::sys::socket(domain, libc::SOCK_STREAM))? as i32;
        let owned = OwnedFd::from_raw_fd(fd);
        options(fd, n.address.family, listener, false)?;
        register_socket(n, owned)
    }
}
#[cfg(test)]
thread_local! { pub(crate) static ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// EMB-3: suppress `SIGPIPE` on child stdin pipe writes without altering the
/// host's process-wide `SIGPIPE` disposition.
///
/// On Darwin, the pipe write end already has `F_SETNOSIGPIPE`. On Linux, this
/// blocks `SIGPIPE` on the calling worker thread around `writev`, drains a
/// thread-pending `SIGPIPE` with zero-timeout `sigtimedwait` when `writev`
/// fails with `EPIPE`, and restores the previous thread mask.
pub(crate) unsafe fn writev_pipe_nosigpipe(
    fd: i32,
    prefix: &Buffer,
    tail: &Buffer,
) -> Result<isize, i32> {
    let parts = [
        libc::iovec {
            iov_base: prefix.data.cast(),
            iov_len: prefix.len,
        },
        libc::iovec {
            iov_base: tail.data.cast(),
            iov_len: tail.len,
        },
    ];
    // SAFETY: `parts` points to pinned request buffers; host seam suppresses SIGPIPE.
    unsafe { crate::host::sys::writev_pipe_nosigpipe(fd, &parts) }
}

/// An attempt with two nonempty parts is one sendmsg. An empty part still
/// permits the single-buffer syscall.
unsafe fn send_pair(fd: i32, prefix: &Buffer, tail: &Buffer) -> isize {
    // SAFETY: both immutable prefixes are pinned by the request. sendmsg
    // reads the iovecs and their bytes synchronously and retains no pointer.
    unsafe {
        if prefix.len == 0 || tail.len == 0 {
            let bytes = if prefix.len == 0 { tail } else { prefix };
            return libc::send(fd, bytes.data.cast(), bytes.len, libc::MSG_NOSIGNAL);
        }
        let mut parts = [
            libc::iovec {
                iov_base: prefix.data.cast(),
                iov_len: prefix.len,
            },
            libc::iovec {
                iov_base: tail.data.cast(),
                iov_len: tail.len,
            },
        ];
        let mut message: libc::msghdr = zeroed();
        message.msg_iov = parts.as_mut_ptr();
        message.msg_iovlen = parts.len() as _;
        libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL)
    }
}

/// A not-ready attempt is None. All other attempts commit one result.
unsafe fn attempt(request: &mut Request, n: &mut NetworkRequest, first: bool) -> Option<Outcome> {
    #[cfg(test)]
    ATTEMPTS.set(ATTEMPTS.get() + 1);
    unsafe {
        let write = matches!(request.operation, CONNECT | SEND_PAIR | SEND_SECRET | SCRAM);
        let sockets = &mut *n.sockets;
        let key = n.connecting.unwrap_or(request.handle);
        if !first && request.operation != LISTEN {
            sockets.disarm(key, write);
        }
        if first && matches!(request.operation, SEND_PAIR | SEND_SECRET | SCRAM) {
            sockets.resume_send(key);
        }
        let result = match request.operation {
            LOCAL_ADDR | PEER_ADDR => {
                query_address(sockets.fd(key), request.operation, &mut n.address)
            }
            LISTEN => {
                let key = match socket(n, true) {
                    Ok(k) => k,
                    Err(e) => return Some(error(e)),
                };
                (*n.sockets).mark_own_listener(key);
                let fd = (*n.sockets).fd(key);
                let (a, len) = address(n.address);
                let result = retry(|| {
                    libc::bind(fd, (&a as *const libc::sockaddr_storage).cast(), len) as isize
                })
                .and_then(|_| retry(|| libc::listen(fd, i32::from(n.backlog)) as isize))
                .and_then(|_| {
                    let mut a = zeroed();
                    let mut len = size_of::<libc::sockaddr_storage>() as u32;
                    retry(|| {
                        libc::getsockname(
                            fd,
                            (&mut a as *mut libc::sockaddr_storage).cast(),
                            &mut len,
                        ) as isize
                    })?;
                    n.address = decoded(&mut a);
                    Ok(Outcome {
                        value: key,
                        ..Outcome::default()
                    })
                });
                if result.is_err() {
                    (*n.sockets).release(key);
                }
                match result {
                    Err(libc::EADDRINUSE) => Ok(signal(4)),
                    result => result,
                }
            }
            CONNECT => {
                if first && n.public {
                    // NET-18 part 2: a public-only connect to an address the
                    // host holds, or whose holdings cannot be read, is refused
                    // before any socket call.
                    let ip = (u128::from(n.address.hi) << 64) | u128::from(n.address.lo);
                    if hirsel_core_net::own_address::holds(n.address.family, ip) != Ok(false) {
                        return Some(Outcome {
                            error: IoError {
                                tag: 1,
                                code: hirsel_core_net::OWN_ADDRESS,
                            },
                            ..Outcome::default()
                        });
                    }
                }
                if first {
                    let key = match socket(n, false) {
                        Ok(k) => k,
                        Err(e) => return Some(error(e)),
                    };
                    n.connecting = Some(key);
                    let (a, len) = address(n.address);
                    match retry(|| {
                        libc::connect(
                            (*n.sockets).fd(key),
                            (&a as *const libc::sockaddr_storage).cast(),
                            len,
                        ) as isize
                    }) {
                        Ok(_) => Ok(Outcome {
                            value: key,
                            ..Outcome::default()
                        }),
                        Err(libc::EINPROGRESS) => return None,
                        Err(e) => Err(e),
                    }
                } else {
                    let fd = sockets.fd(key);
                    let mut code = 0i32;
                    let mut len = size_of::<i32>() as u32;
                    match retry(|| {
                        libc::getsockopt(
                            fd,
                            libc::SOL_SOCKET,
                            libc::SO_ERROR,
                            (&mut code as *mut i32).cast(),
                            &mut len,
                        ) as isize
                    }) {
                        Err(e) => Err(e),
                        Ok(_) if matches!(code, libc::EINPROGRESS | libc::EALREADY) => return None,
                        Ok(_) if code != 0 => Err(code),
                        Ok(_) => {
                            let mut peer: libc::sockaddr_storage = zeroed();
                            let mut len = size_of::<libc::sockaddr_storage>() as u32;
                            match retry(|| {
                                libc::getpeername(
                                    fd,
                                    (&mut peer as *mut libc::sockaddr_storage).cast(),
                                    &mut len,
                                ) as isize
                            }) {
                                Ok(_) => Ok(Outcome {
                                    value: key,
                                    ..Outcome::default()
                                }),
                                Err(libc::ENOTCONN) => return None,
                                Err(e) => Err(e),
                            }
                        }
                    }
                }
            }
            ACCEPT => {
                let mut peer = zeroed();
                let mut len = size_of::<libc::sockaddr_storage>() as u32;
                let accepted = loop {
                    let result = retry(|| {
                        crate::host::sys::accept(
                            sockets.fd(key),
                            (&mut peer as *mut libc::sockaddr_storage).cast(),
                            &mut len,
                        )
                    });
                    // An aborted peer does not prove the accept queue is drained.
                    if result != Err(libc::ECONNABORTED) {
                        break result;
                    }
                };
                match accepted {
                    Ok(fd) => {
                        let owned = OwnedFd::from_raw_fd(fd as i32);
                        n.address = decoded(&mut peer);
                        match options(fd as i32, n.address.family, false, true) {
                            Ok(()) => Ok(Outcome {
                                value: sockets.insert(owned).unwrap_or_else(|| {
                                    crate::run_stop::raise(crate::run_stop::Stop::from_static(
                                        b"HostHandles",
                                    ))
                                }),
                                ..Outcome::default()
                            }),
                            Err(e) => Err(e),
                        }
                    }
                    Err(libc::EAGAIN) => return None,
                    Err(e) => Err(e),
                }
            }
            SEND_SECRET | SCRAM => {
                let Some(stream) = n
                    .tls
                    .as_mut()
                    .and_then(|sessions| sessions.get_mut(&(key & !3)))
                else {
                    return Some(crate::net::DENIED);
                };
                let send = request.path.cast_mut().cast::<crate::secret::SendRequest>();
                let committed = &mut (*send).committed;
                let fd = sockets.fd(key);
                let zc = sockets.send_target(key).ok().flatten();
                let mut skip = *committed;
                // SEND_SECRET's message is whole in `secret`; its buffer and
                // tail are the wire and marks it was built from.
                let none = crate::net::empty_buffer();
                let parts = if request.operation == SCRAM {
                    [&request.buffer, &(*send).secret, &*n.send_tail]
                } else {
                    [&none, &(*send).secret, &none]
                };
                for part in parts {
                    let part = crate::tls::bytes(part);
                    if skip >= part.len() {
                        skip -= part.len();
                        continue;
                    }
                    let part = &part[skip..];
                    let len = part.len();
                    let answer = stream.send(fd, zc.clone(), part, &[])?;
                    if answer.error != IoError::OK {
                        return Some(answer);
                    }
                    *committed += answer.value as usize;
                    if answer.value < len as u64 {
                        return None;
                    }
                    skip = 0;
                }
                // Success requires flushing the final committed record too.
                return stream.send(fd, zc, &[], &[]);
            }
            RECV_READY => {
                if let Some(stream) = n
                    .tls
                    .as_mut()
                    .and_then(|sessions| sessions.get_mut(&(key & !3)))
                {
                    return stream.ready(sockets.fd(key));
                }
                // An ended or reset half answers its next receive at once.
                if sockets.ended(key) || sockets.reset(key) {
                    return Some(Outcome::default());
                }
                // Bytes, end of stream and a pending error all make the fd
                // readable; none is consumed, so the receive still reports it.
                let mut poll = libc::pollfd {
                    fd: sockets.fd(key),
                    events: libc::POLLIN | crate::host::sys::PEER_CLOSED,
                    revents: 0,
                };
                match retry(|| libc::poll(&mut poll, 1, 0) as isize) {
                    Ok(0) => {
                        sockets.mark_read_drained(key);
                        return None;
                    }
                    Ok(_) => Ok(Outcome::default()),
                    Err(e) => Err(e),
                }
            }
            RECV_BOUNDED | SEND_PAIR => {
                let b = &mut request.buffer;
                let receiving = request.operation == RECV_BOUNDED;
                let tail = (!receiving).then(|| &*n.send_tail);
                if let Some(stream) = n
                    .tls
                    .as_mut()
                    .and_then(|sessions| sessions.get_mut(&(key & !3)))
                {
                    let fd = sockets.fd(key);
                    if let Some(tail) = tail {
                        let zc = sockets.send_target(key).ok().flatten();
                        if !n.tls_send_pending {
                            let length = (b.len + tail.len) as u64;
                            let answer = stream.send(
                                fd,
                                zc.clone(),
                                crate::tls::bytes(b),
                                crate::tls::bytes(tail),
                            )?;
                            if length == 0 || answer.error != IoError::OK || answer.value < length {
                                return Some(answer);
                            }
                            // After a full write the caller may wait for a reply. Keep
                            // the write waiter until the final slot reaches the socket.
                            n.tls_send_pending = true;
                        }
                        return stream.send(fd, zc, &[], &[]);
                    }
                    let spare = if b.capacity <= b.len {
                        &mut [][..]
                    } else {
                        std::slice::from_raw_parts_mut(b.data.add(b.len), b.capacity - b.len)
                    };
                    let result = stream.receive(fd, spare);
                    if let Some(outcome) = result {
                        b.len += outcome.value as usize;
                    }
                    return result;
                }
                if (receiving && b.len >= b.capacity)
                    || tail.is_some_and(|tail| b.len == 0 && tail.len == 0)
                {
                    return Some(Outcome::default());
                }
                if !receiving && sockets.reset(key) {
                    return Some(signal(5));
                }
                if receiving && sockets.ended(key) && !sockets.reset(key) {
                    return Some(Outcome::default());
                }
                let fd = sockets.fd(key);
                let pipe = sockets.pipe(key);
                let rc = if let Some(tail) = tail
                    && pipe
                {
                    writev_pipe_nosigpipe(fd, b, tail)
                } else {
                    retry(|| {
                        if let Some(tail) = tail {
                            send_pair(fd, b, tail)
                        } else if pipe {
                            libc::read(fd, b.data.add(b.len).cast(), b.capacity - b.len)
                        } else {
                            libc::recv(fd, b.data.add(b.len).cast(), b.capacity - b.len, 0)
                        }
                    })
                };
                match rc {
                    Ok(0) if receiving && sockets.reset(key) => return Some(signal(5)),
                    Ok(count) => {
                        if receiving {
                            b.len += count as usize;
                            if count == 0 {
                                sockets.mark_ended(key);
                            } else if b.len < b.capacity {
                                // Linux stream short reads drain the readable prefix.
                                sockets.mark_read_drained(key);
                            }
                        }
                        Ok(Outcome {
                            value: count as u64,
                            ..Outcome::default()
                        })
                    }
                    Err(libc::EAGAIN) if !receiving || !sockets.reset(key) => {
                        if receiving {
                            sockets.mark_read_drained(key);
                        }
                        return None;
                    }
                    Err(e)
                        if matches!(e, libc::ECONNRESET | libc::EPIPE | libc::ETIMEDOUT)
                            || (e == libc::ENOTCONN && sockets.reset(key))
                            || (e == libc::EAGAIN && sockets.reset(key)) =>
                    {
                        sockets.mark_reset(key);
                        return Some(signal(5));
                    }
                    Err(e) => Err(e),
                }
            }
            _ => unreachable!(),
        };
        Some(match result {
            Ok(outcome) => outcome,
            Err(e) => error(e),
        })
    }
}
unsafe fn finish(request: &Request, n: &mut NetworkRequest, mut outcome: Outcome) -> Step {
    unsafe {
        if request.operation == CONNECT && outcome.error.tag != 0 {
            if let Some(key) = n.connecting.take() {
                (*n.sockets).release(key);
            }
        }
    }
    if n.tls_send_pending {
        // A deadline or cancellation cannot take back already committed bytes.
        outcome = Outcome {
            value: request.buffer.len as u64 + unsafe { (*n.send_tail).len as u64 },
            ..Outcome::default()
        };
    }
    if matches!(request.operation, SEND_SECRET | SCRAM) && outcome.error != IoError::OK {
        // SAFETY: this operation pins its distinct progress field through delivery.
        outcome.written =
            unsafe { (*request.path.cast::<crate::secret::SendRequest>()).committed as u64 };
    }
    Step {
        outcome,
        ..Step::default()
    }
}
/// Where a network deadline stands (CONC-7): the deadline lives on its own
/// Clock's timers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Due {
    /// No deadline.
    Never,
    /// Reached: the operation times out.
    Passed,
    /// Not reached; the scheduler's timer fires at this host deadline.
    Host(u64),
    /// Not reached; the clock itself readies the operation's token when it is
    /// (a virtual clock's `advance_to`). No host timer stands in for it.
    Clock,
}
impl Due {
    pub(crate) fn host(self) -> Option<u64> {
        match self {
            Due::Host(at) => Some(at),
            _ => None,
        }
    }
}

fn deadline_source(provider: Provider, clock: u64) -> Option<(Provider, u64)> {
    // Deadline scheduling is outside the program's taped clock reads.
    #[cfg(feature = "record")]
    if crate::tape::owns(&provider) {
        return crate::tape::source(&provider, clock);
    }
    Some((provider, clock))
}

/// Retire a completed operation's timer on a provided clock.
/// # Safety
/// The clock provider and handle in `until` are live.
pub(crate) unsafe fn retire_deadline(until: Option<(Provider, u64, u64)>, token: u64) {
    if let Some((provider, clock, _)) = until
        && let Some((provider, _)) = deadline_source(provider, clock)
    {
        // SAFETY: the caller pins the clock through retirement.
        unsafe { crate::provide::retire_deadline(&provider, token) };
    }
}

/// The deadline `until` for the operation `token`. On the OS clock it is the
/// program deadline plus the provider's epoch, as `sleep_wait` converts it,
/// against a fresh host read. Any other clock answers its own SLEEP_UNTIL wait
/// for this token: ready when reached, Until in the scheduler's domain, or
/// Park, after which the clock readies the token when the deadline is reached.
/// Host time never translates another clock's reading.
pub(crate) unsafe fn deadline(
    until: Option<(crate::provider::Provider, u64, u64)>,
    token: u64,
) -> Due {
    unsafe {
        let Some((p, clock, at)) = until else {
            return Due::Never;
        };
        let Some((p, clock)) = deadline_source(p, clock) else {
            return Due::Never;
        };
        let host = |at: u64| {
            if at <= crate::os::monotonic_ns() {
                Due::Passed
            } else {
                Due::Host(at)
            }
        };
        if crate::os::owns(&p) {
            return host(crate::os::epoch(p.state).saturating_add(at));
        }
        let mut bytes = at.to_ne_bytes();
        let mut r = Request {
            operation: crate::provider::SLEEP_UNTIL,
            mode: 0,
            handle: clock,
            path: ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 8,
                capacity: 8,
            },
            site: 0,
        };
        let w = (p.wait)(p.state, &mut r, token);
        match w.kind {
            crate::provider::WAIT_READY => Due::Passed,
            crate::provider::WAIT_UNTIL => host(w.deadline_ns),
            _ => Due::Clock,
        }
    }
}
pub(crate) unsafe fn step(request: *mut Request, token: u64, first: bool) -> Step {
    unsafe {
        let request = &mut *request;
        let n = &mut *request.path.cast_mut().cast::<NetworkRequest>();
        if first && request.operation == SEND_SECRET && deadline(n.until, token) == Due::Passed {
            return finish(request, n, signal(6));
        }
        let drained = first
            && (request.operation == RECV_READY
                || (request.operation == RECV_BOUNDED
                    && request.buffer.len < request.buffer.capacity))
            && (*n.sockets).read_drained(request.handle)
            && n.tls
                .as_ref()
                .is_none_or(|sessions| !sessions.contains_key(&(request.handle & !3)));
        let deadline = if drained {
            let due = deadline(n.until, token);
            // Even a drained reader gets one try at an already-expired deadline.
            if due == Due::Passed {
                let outcome = attempt(request, n, first).unwrap_or_else(|| signal(6));
                return finish(request, n, outcome);
            }
            due.host()
        } else {
            if let Some(outcome) = attempt(request, n, first) {
                return finish(request, n, outcome);
            }
            let due = deadline(n.until, token);
            if due == Due::Passed {
                return finish(request, n, signal(6));
            }
            due.host()
        };
        let key = n.connecting.unwrap_or(request.handle);
        let write = matches!(request.operation, CONNECT | SEND_PAIR | SEND_SECRET | SCRAM);
        if let Err(e) = (*n.sockets).arm(key, write, token) {
            return finish(request, n, error(e.raw_os_error().unwrap_or(libc::EIO)));
        }
        // ADD sees existing readiness. Later edges remain queued until the
        // registry owner can dispatch the waiter published by arm.
        crate::tasks::attach_net(n.sockets, (*n.sockets).poller().unwrap());
        Step {
            kind: STEP_PENDING,
            deadline_ns: deadline.unwrap_or(0),
            ..Step::default()
        }
    }
}
pub(crate) unsafe fn abort(request: *mut Request, token: u64, reason: u8) -> Outcome {
    unsafe {
        if reason == ABORT_DEADLINE {
            let step = step(request, token, false);
            if step.kind == STEP_DONE {
                return step.outcome;
            }
        }
        let r = &*request;
        let n = &mut *r.path.cast_mut().cast::<NetworkRequest>();
        let key = n.connecting.unwrap_or(r.handle);
        (*n.sockets).disarm(
            key,
            matches!(r.operation, CONNECT | SEND_PAIR | SEND_SECRET | SCRAM),
        );
        if matches!(r.operation, SEND_PAIR | SEND_SECRET | SCRAM) {
            // A peer that stopped reading must not hold the abandoned bytes,
            // or the socket, past its close.
            if let Some(stream) = n
                .tls
                .as_mut()
                .and_then(|sessions| sessions.get_mut(&(key & !3)))
            {
                stream.cancel_send();
            }
            (*n.sockets).abandon_send(key);
        }
        finish(r, n, signal(if reason == ABORT_CANCEL { 7 } else { 6 })).outcome
    }
}

unsafe fn tls_step(request: *mut Request, token: u64, first: bool) -> Step {
    unsafe {
        let request = &*request;
        let tls = &mut *request.path.cast_mut().cast::<TlsRequest>();
        let key = request.handle;
        let n = &mut tls.network;
        let sessions = &mut *n.tls;
        let sockets = &mut *n.sockets;
        let s = &mut *tls.state.cast::<HandshakeState>();
        if first {
            let mut byte = 0u8;
            let peek = libc::recv(
                sockets.fd(key),
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            );
            let refusal = match peek {
                0 => Some(crate::net::tls_failure(5, 0)),
                1 => Some(crate::net::tls_failure(4, 0)),
                _ => {
                    let e = errno();
                    (e != libc::EAGAIN && e != libc::EWOULDBLOCK).then(|| error(e))
                }
            };
            if let Some(outcome) = refusal {
                return tls_finish(tls, key, outcome);
            }
            match client_stream(s) {
                Ok(stream) => s.stream = Some(stream),
                Err(outcome) => return tls_finish(tls, key, outcome),
            }
        } else if s.stream.as_ref().unwrap().waiting_for_trust() {
            if deadline(n.until, token) == Due::Passed {
                return tls_finish(tls, key, signal(6));
            }
        } else {
            sockets.disarm(key, s.pending_write);
        }
        let stream = s.stream.as_mut().unwrap();
        match stream.handshake(sockets.fd(key), token) {
            Ok(Handshake::Done) => {
                let protocol = &mut *tls.protocol_out;
                let out = std::slice::from_raw_parts_mut(protocol.data, protocol.capacity);
                let (len, binding, stream) =
                    match s.stream.take().unwrap().finish(out, &mut *tls.peer_out) {
                        Ok(result) => result,
                        Err(outcome) => return tls_finish(tls, key, outcome),
                    };
                protocol.len = len;
                let binding_out = &mut *tls.binding_out;
                let binding = binding.bytes();
                ptr::copy_nonoverlapping(binding.as_ptr(), binding_out.data, binding.len());
                binding_out.len = binding.len();
                sessions.insert(key, stream);
                sockets.split(key);
                Step {
                    outcome: Outcome {
                        value: key,
                        ..Outcome::default()
                    },
                    ..Step::default()
                }
            }
            Ok(Handshake::Socket { write }) => {
                let due = deadline(n.until, token);
                if due == Due::Passed {
                    return tls_finish(tls, key, signal(6));
                }
                let deadline = due.host();
                s.pending_write = write;
                if let Err(e) = sockets.arm(key, write, token) {
                    return tls_finish(tls, key, error(e.raw_os_error().unwrap_or(libc::EIO)));
                }
                crate::tasks::attach_net(n.sockets, sockets.poller().unwrap());
                Step {
                    kind: STEP_PENDING,
                    deadline_ns: deadline.unwrap_or(0),
                    ..Step::default()
                }
            }
            Ok(Handshake::Trust) => {
                let due = deadline(n.until, token);
                if due == Due::Passed {
                    return tls_finish(tls, key, signal(6));
                }
                Step {
                    kind: STEP_PENDING,
                    deadline_ns: due.host().unwrap_or(0),
                    ..Step::default()
                }
            }
            Err(outcome) => tls_finish(tls, key, outcome),
        }
    }
}

unsafe fn client_stream(state: &mut HandshakeState) -> Result<Stream, Outcome> {
    let name = state.name.take().unwrap();
    let alpn = std::mem::take(&mut state.alpn);
    if state.roots.is_none() && platform_tls() {
        return off_slot(HostCall::Bounded, || Stream::platform(name, alpn));
    }
    // SAFETY: the issued handshake pins its run's TLS cache through delivery.
    let cache = unsafe { (*state.cache).get() };
    let roots = match state.roots.take() {
        Some(roots) => roots,
        None => {
            if cache.system.get().is_none() {
                // Loaded off the slot, outside the cell: no unit waits in
                // get_or_init for another unit's load.
                let _ = cache.system.set(off_slot(HostCall::Bounded, system_roots));
            }
            cache.system.get().unwrap().clone().map_err(error)?
        }
    };
    // Building a client configuration can take milliseconds of CPU.
    let cacheable = state.cacheable;
    off_slot(HostCall::Bounded, || {
        cache
            .client_config(roots, alpn, cacheable)
            .and_then(|config| Stream::new(name, config))
    })
}

unsafe fn tls_finish(tls: &mut TlsRequest, key: u64, outcome: Outcome) -> Step {
    unsafe {
        let n = &mut tls.network;
        let state = &mut *tls.state.cast::<HandshakeState>();
        drop(state.stream.take());
        (*n.sockets).release(key);
        state.consumed = true;
    }
    Step {
        outcome,
        ..Step::default()
    }
}

unsafe fn tls_abort(request: *mut Request, _: u64, reason: u8) -> Outcome {
    unsafe {
        let r = &*request;
        let tls = &mut *r.path.cast_mut().cast::<TlsRequest>();
        tls_finish(
            tls,
            r.handle,
            signal(if reason == ABORT_CANCEL { 7 } else { 6 }),
        )
        .outcome
    }
}
