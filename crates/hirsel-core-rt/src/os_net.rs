//! Linux try-first socket attempts. Go's SO_ERROR + getpeername sequence rejects
//! spurious writable connect events; combined interests live in core-net.
use crate::{
    entry::SocketActivation,
    net::{Address, NetworkRequest, RulesStorage},
    os_tls::system_roots,
    provider::*,
    tls::{HandshakeState, TlsRequest},
};
use std::{
    env, io,
    mem::{size_of, zeroed},
    net::{SocketAddr, TcpListener},
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
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
    operator: crate::os_unix::receive_operator,
};

pub(crate) const LISTEN_FD: i32 = 3;
static LISTENER_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Take custody of the single socket passed to this process by its launcher.
/// # Safety
/// Matching activation metadata must designate a descriptor this process
/// inherited at exec and has not otherwise taken into ownership. Receive
/// before any other thread can access the process environment.
unsafe fn receive_listener() -> io::Result<Option<OwnedFd>> {
    let Some(pid) = env::var_os("LISTEN_PID") else {
        return Ok(None);
    };
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    let pid: u32 = pid
        .to_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(invalid)?;
    if pid != std::process::id() {
        return Ok(None);
    }
    let count = env::var_os("LISTEN_FDS")
        .and_then(|n| n.to_str()?.parse::<u32>().ok())
        .ok_or_else(invalid)?;
    if count != 1 {
        return Err(invalid());
    }
    if LISTENER_RECEIVED.swap(true, Ordering::AcqRel) {
        return Ok(None);
    }
    // SAFETY: the launcher assigned this fd to this PID. Verify it exists
    // before construction, and stop it from passing through any child exec.
    unsafe {
        retry(|| libc::fcntl(LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) as isize)
            .map_err(io::Error::from_raw_os_error)?;
        let listener = OwnedFd::from_raw_fd(LISTEN_FD);
        // SAFETY: the launch receives the socket before any other thread can
        // access the environment. Leave no activation metadata for children.
        for name in [
            "LISTEN_PID",
            "LISTEN_PIDFDID",
            "LISTEN_FDS",
            "LISTEN_FDNAMES",
        ] {
            env::remove_var(name);
        }
        Ok(Some(listener))
    }
}

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
fn socket_option(fd: BorrowedFd<'_>, level: i32, name: i32) -> Result<i32, i32> {
    let mut value: i32 = 0;
    let mut len = size_of::<i32>() as libc::socklen_t;
    // SAFETY: fd is borrowed and both option outputs are writable.
    retry(|| unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            level,
            name,
            (&raw mut value).cast(),
            &mut len,
        ) as isize
    })?;
    Ok(value)
}

fn inherited_address(listener: &TcpListener) -> Result<Address, i32> {
    let fd = listener.as_fd();
    if socket_option(fd, libc::SOL_SOCKET, libc::SO_TYPE)? != libc::SOCK_STREAM
        || socket_option(fd, libc::SOL_SOCKET, libc::SO_ACCEPTCONN)? != 1
        || socket_option(fd, libc::SOL_SOCKET, libc::SO_PROTOCOL)? != libc::IPPROTO_TCP
    {
        return Err(libc::EINVAL);
    }

    match listener
        .local_addr()
        .map_err(|e| e.raw_os_error().unwrap_or(libc::EINVAL))?
    {
        SocketAddr::V4(a) => Ok(Address {
            family: 0,
            hi: 0,
            lo: u64::from(u32::from(*a.ip())),
            port: a.port(),
        }),
        SocketAddr::V6(a) => {
            // A dual-stack wildcard also listens on IPv4, outside a V6 rule.
            if a.scope_id() != 0
                || (a.ip().is_unspecified()
                    && socket_option(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY)? == 0)
            {
                return Err(libc::EACCES);
            }
            let ip = u128::from(*a.ip());
            Ok(Address {
                family: 1,
                hi: (ip >> 64) as u64,
                lo: ip as u64,
                port: a.port(),
            })
        }
    }
}

unsafe fn adopt_listener(request: *mut Request, fd: OwnedFd) -> Step {
    // SAFETY: only the OS LISTEN issue callback supplies a live request here.
    let request = unsafe { &mut *request };
    let n = unsafe { &mut *request.path.cast_mut().cast::<NetworkRequest>() };
    let listener = TcpListener::from(fd);
    let adopted = (|| {
        let address = inherited_address(&listener)?;
        if !unsafe { crate::net::permits_address(request.handle, &address, 1) } {
            return Err(libc::EACCES);
        }
        listener
            .set_nonblocking(true)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        let value = unsafe { register_socket(n, listener.into()) }?;
        n.address = address;
        Ok(value)
    })();
    let outcome = match adopted {
        Ok(value) => Outcome {
            value,
            ..Outcome::default()
        },
        Err(code) => error(code),
    };
    Step {
        outcome,
        ..Step::default()
    }
}

unsafe fn register_socket(n: &mut NetworkRequest, owned: OwnedFd) -> Result<u64, i32> {
    // SAFETY: the request pins the registry through issue.
    unsafe { adopt_socket(n.sockets, owned) }.map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
}

pub(crate) unsafe fn adopt_socket(
    sockets: *mut hirsel_core_net::Sockets,
    owned: OwnedFd,
) -> io::Result<u64> {
    // SAFETY: callers hold the sequencer and the run pins the socket registry.
    unsafe {
        let poller = (*sockets).poller()?;
        crate::tasks::attach_net(sockets, poller);
        Ok((*sockets)
            .insert(owned)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles")))
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
        register_socket(n, owned)
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
                    events: libc::POLLIN | libc::POLLRDHUP,
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
                let rc = retry(|| {
                    if let Some(tail) = tail {
                        if pipe {
                            // The run ignores SIGPIPE. One writev commits
                            // a prefix across both offered slices.
                            let parts = [
                                libc::iovec {
                                    iov_base: b.data.cast(),
                                    iov_len: b.len,
                                },
                                libc::iovec {
                                    iov_base: tail.data.cast(),
                                    iov_len: tail.len,
                                },
                            ];
                            libc::writev(fd, parts.as_ptr(), parts.len() as i32)
                        } else {
                            send_pair(fd, b, tail)
                        }
                    } else if pipe {
                        libc::read(fd, b.data.add(b.len).cast(), b.capacity - b.len)
                    } else {
                        libc::recv(fd, b.data.add(b.len).cast(), b.capacity - b.len, 0)
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
        // This is host scheduling, outside the program's clock reads.
        #[cfg(feature = "record")]
        let Some((p, clock)) = (if crate::tape::owns(&p) {
            crate::tape::source(&p, clock)
        } else {
            Some((p, clock))
        }) else {
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
            let cache = (*s.cache).get();
            let roots = s.roots.take().unwrap_or_else(|| {
                if let Some(roots) = cache.system.get() {
                    return roots.clone();
                }
                let _blocking = crate::units::blocking();
                cache.system.get_or_init(system_roots).clone()
            });
            let stream = {
                let _blocking = crate::units::blocking();
                cache
                    .client_config(roots, std::mem::take(&mut s.alpn), s.cacheable)
                    .and_then(|config| crate::os_tls::Stream::new(s.name.take().unwrap(), config))
            };
            match stream {
                Ok(stream) => s.stream = Some(stream),
                Err(outcome) => return tls_finish(tls, key, outcome),
            }
        } else {
            sockets.disarm(key, s.pending_write);
        }
        let stream = s.stream.as_mut().unwrap();
        match stream.handshake(sockets.fd(key)) {
            Ok(Ok(())) => {
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
            Ok(Err(write)) => {
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
            Err(outcome) => tls_finish(tls, key, outcome),
        }
    }
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
