//! Linux try-first socket attempts. Go's SO_ERROR + getpeername sequence rejects
//! spurious writable connect events; combined interests live in core-net.
use crate::{
    net::{Address, NetworkRequest},
    provider::*,
};
use std::{
    io,
    mem::{size_of, zeroed},
    os::fd::{FromRawFd, OwnedFd},
    ptr,
};

/// Installed by the native Net adapters on their pinned request. The common
/// OS provider can forward a socket operation without linking its backend.
pub(crate) struct Operations {
    pub step: unsafe fn(*mut Request, u64, bool) -> Step,
    pub abort: unsafe fn(*mut Request, u64, u8) -> Outcome,
}

pub(crate) static OPERATIONS: Operations = Operations { step, abort };

fn errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
fn retry(mut call: impl FnMut() -> isize) -> Result<isize, i32> {
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
fn error(code: i32) -> Outcome {
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
fn signal(tag: u8) -> Outcome {
    Outcome {
        error: IoError { tag, code: 0 },
        ..Outcome::default()
    }
}
unsafe fn address(a: Address) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: zero is a valid initial representation of these C address structs.
    unsafe {
        let mut storage: libc::sockaddr_storage = zeroed();
        let len = if a.family == 0 {
            let v4 = storage_as_mut::<libc::sockaddr_in>(&mut storage);
            v4.sin_family = libc::AF_INET as u16;
            v4.sin_port = a.port.to_be();
            v4.sin_addr.s_addr = (a.lo as u32).to_be();
            size_of::<libc::sockaddr_in>()
        } else {
            let v6 = storage_as_mut::<libc::sockaddr_in6>(&mut storage);
            v6.sin6_family = libc::AF_INET6 as u16;
            v6.sin6_port = a.port.to_be();
            v6.sin6_addr.s6_addr = ((u128::from(a.hi) << 64) | u128::from(a.lo)).to_be_bytes();
            size_of::<libc::sockaddr_in6>()
        };
        (storage, len as libc::socklen_t)
    }
}
unsafe fn storage_as_mut<T>(storage: &mut libc::sockaddr_storage) -> &mut T {
    // SAFETY: callers select the family-specific C struct that fits storage.
    unsafe { &mut *(storage as *mut libc::sockaddr_storage).cast::<T>() }
}
unsafe fn decoded(storage: &mut libc::sockaddr_storage) -> Address {
    unsafe {
        if storage.ss_family as i32 == libc::AF_INET {
            let a = storage_as_mut::<libc::sockaddr_in>(storage);
            Address {
                family: 0,
                hi: 0,
                lo: u64::from(u32::from_be(a.sin_addr.s_addr)),
                port: u16::from_be(a.sin_port),
            }
        } else {
            let a = storage_as_mut::<libc::sockaddr_in6>(storage);
            let ip = u128::from_be_bytes(a.sin6_addr.s6_addr);
            Address {
                family: 1,
                hi: (ip >> 64) as u64,
                lo: ip as u64,
                port: u16::from_be(a.sin6_port),
            }
        }
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
        let fd = retry(|| {
            libc::socket(
                if n.address.family == 0 {
                    libc::AF_INET
                } else {
                    libc::AF_INET6
                },
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            ) as isize
        })? as i32;
        let owned = OwnedFd::from_raw_fd(fd);
        options(fd, n.address.family, listener, false)?;
        (*n.sockets)
            .poller()
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        crate::tasks::attach_net(n.sockets, (*n.sockets).poller().unwrap());
        Ok((*n.sockets)
            .insert(owned)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles")))
    }
}
#[cfg(test)]
thread_local! { pub(crate) static ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

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
        message.msg_iovlen = parts.len();
        libc::sendmsg(fd, &message, libc::MSG_NOSIGNAL)
    }
}

/// A not-ready attempt is None. All other attempts commit one result.
unsafe fn attempt(request: &mut Request, n: &mut NetworkRequest, first: bool) -> Option<Outcome> {
    #[cfg(test)]
    ATTEMPTS.set(ATTEMPTS.get() + 1);
    unsafe {
        let write = matches!(request.operation, CONNECT | SEND_PAIR);
        let sockets = &mut *n.sockets;
        let key = n.connecting.unwrap_or(request.handle);
        if !first && request.operation != LISTEN {
            sockets.disarm(key, write);
        }
        let result = match request.operation {
            LISTEN => {
                let key = match socket(n, true) {
                    Ok(k) => k,
                    Err(e) => return Some(error(e)),
                };
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
                        libc::accept4(
                            sockets.fd(key),
                            (&mut peer as *mut libc::sockaddr_storage).cast(),
                            &mut len,
                            libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                        ) as isize
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
                                value: sockets
                                    .insert(owned)
                                    .unwrap_or_else(|| crate::process::stop(b"HostHandles")),
                                ..Outcome::default()
                            }),
                            Err(e) => Err(e),
                        }
                    }
                    Err(libc::EAGAIN) => return None,
                    Err(e) => Err(e),
                }
            }
            RECV_BOUNDED | SEND_PAIR => {
                let b = &mut request.buffer;
                let receiving = request.operation == RECV_BOUNDED;
                let tail = (!receiving).then(|| &*n.send_tail);
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
                let rc = retry(|| {
                    if let Some(tail) = tail {
                        send_pair(sockets.fd(key), b, tail)
                    } else {
                        libc::recv(
                            sockets.fd(key),
                            b.data.add(b.len).cast(),
                            b.capacity - b.len,
                            0,
                        )
                    }
                });
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
unsafe fn finish(request: &Request, n: &mut NetworkRequest, outcome: Outcome) -> Step {
    unsafe {
        if request.operation == CONNECT && outcome.error.tag != 0 {
            if let Some(key) = n.connecting.take() {
                (*n.sockets).release(key);
            }
        }
    }
    Step {
        outcome,
        ..Step::default()
    }
}
/// A network deadline in the scheduler's host CLOCK_MONOTONIC domain. On the OS
/// clock it is the program deadline plus the provider's epoch, as `sleep_wait`
/// converts it, with no clock read; the caller's fresh read decides expiry. A
/// substituted clock translates by its own reading, saturating at now.
unsafe fn pending_deadline(n: &NetworkRequest) -> Option<u64> {
    unsafe {
        n.until.and_then(|(p, clock, at)| {
            // This translation is host scheduling, outside the program's clock reads.
            let (p, clock) = if crate::tape::owns(&p) {
                crate::tape::source(&p, clock)?
            } else {
                (p, clock)
            };
            if crate::os::owns(&p) {
                return Some(crate::os::epoch(p.state).saturating_add(at));
            }
            let mut r = Request {
                operation: MONO_NS,
                mode: 0,
                handle: clock,
                path: ptr::null(),
                path_len: 0,
                buffer: Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                },
                site: 0,
            };
            let step = (p.issue)(p.state, &mut r, 0);
            let now = (p.deliver)(p.state, &mut r, step.outcome).value;
            Some(crate::os::monotonic_ns().saturating_add(at.saturating_sub(now)))
        })
    }
}
pub(crate) unsafe fn step(request: *mut Request, token: u64, first: bool) -> Step {
    unsafe {
        let request = &mut *request;
        let n = &mut *request.path.cast_mut().cast::<NetworkRequest>();
        let drained = first
            && request.operation == RECV_BOUNDED
            && request.buffer.len < request.buffer.capacity
            && (*n.sockets).read_drained(request.handle);
        let deadline = if drained {
            let deadline = pending_deadline(n);
            // Even a drained reader gets one try at an already-expired deadline.
            if deadline.is_some_and(|at| at <= crate::os::monotonic_ns()) {
                let outcome = attempt(request, n, first).unwrap_or_else(|| signal(6));
                return finish(request, n, outcome);
            }
            deadline
        } else {
            if let Some(outcome) = attempt(request, n, first) {
                return finish(request, n, outcome);
            }
            let deadline = pending_deadline(n);
            if deadline.is_some_and(|at| at <= crate::os::monotonic_ns()) {
                return finish(request, n, signal(6));
            }
            deadline
        };
        let key = n.connecting.unwrap_or(request.handle);
        let write = matches!(request.operation, CONNECT | SEND_PAIR);
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
        (*n.sockets).disarm(key, matches!(r.operation, CONNECT | SEND_PAIR));
        finish(r, n, signal(if reason == ABORT_CANCEL { 7 } else { 6 })).outcome
    }
}
