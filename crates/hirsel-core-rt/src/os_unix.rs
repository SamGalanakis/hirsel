//! AF_UNIX operations. Bind runs on a thread with a private cwd. Connect and
//! send-to keep the host's pin on the endpoint until the operation settles.
use crate::{
    host::{
        path::{DIRECTORY_HANDLE, Refuse},
        unix::{self, Endpoint},
    },
    net::DENIED,
    os::{OsProvider, open_beneath},
    os_net::{Due, adopt_socket, deadline, error, retry, signal},
    provider::*,
    unix_net::UnixRequest,
};
use hirsel_core_net::Sockets;
use std::{
    io,
    mem::offset_of,
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
};

type Result<T> = std::result::Result<T, i32>;

pub(crate) struct Operations {
    pub step: unsafe fn(&OsProvider, &mut Request, u64, bool) -> Step,
    pub abort: unsafe fn(&OsProvider, &mut Request, u64, u8) -> Outcome,
}

pub(crate) static OPERATIONS: Operations = Operations { step, abort };

#[cfg(test)]
thread_local! {
    pub(crate) static AFTER_RESOLVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// SAFETY: the launcher pins each row and the text it designates for this call.
pub(crate) unsafe fn configure_roots(
    os: &mut OsProvider,
    rows: &[hirsel_core_net::RuleView],
) -> std::result::Result<(), crate::entry::StartupError> {
    use crate::entry::StartupError;
    use hirsel_core_net::GrantRule;
    use std::{collections::BTreeMap, fs::OpenOptions, os::unix::fs::OpenOptionsExt};

    let mut roots = BTreeMap::new();
    for row in rows.iter().filter(|row| row.is_unix_root()) {
        let (label, path) = unsafe { row.decode_unix_root() }.ok_or(StartupError::NetRules)?;
        if roots.insert(label, path).is_some() {
            return Err(StartupError::NetRules);
        }
    }
    for row in rows.iter().filter(|row| row.is_unix()) {
        let Some((label, _)) = (match unsafe { row.decode() } {
            Some(GrantRule::Unix(rule)) => rule
                .endpoint()
                .map(|(label, path)| (label.to_owned(), path.to_owned())),
            _ => None,
        }) else {
            return Err(StartupError::NetRules);
        };
        if !roots.contains_key(label.as_str()) {
            return Err(StartupError::NetRules);
        }
    }
    for (label, path) in roots {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(DIRECTORY_HANDLE)
            .open(path)?;
        os.unix_roots.insert(label.to_owned(), directory.into());
    }
    Ok(())
}

/// The environment naming the launcher's operator listener: `PPID:3:UID`.
pub(crate) const OPERATOR_ENV: &str = "HIRSEL_OPERATOR_LISTEN";
const OPERATOR_FD: i32 = 3;

/// Take custody of the operator listener the launcher passed at descriptor 3, with the
/// one peer UID its connections may have (docs/spec/launch.md "Starting the host").
/// Metadata left by a launcher that is not this process's parent is not this process's.
/// # Safety
/// Call during startup, before any other thread can access the environment, and before
/// any open can reuse descriptor 3.
pub(crate) unsafe fn receive_operator() -> io::Result<Option<(OwnedFd, u32)>> {
    let Some(value) = std::env::var_os(OPERATOR_ENV) else {
        return Ok(None);
    };
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    let fields: Vec<u32> = value
        .to_str()
        .ok_or_else(invalid)?
        .split(':')
        .map(|f| f.parse().map_err(|_| invalid()))
        .collect::<io::Result<_>>()?;
    let [parent, fd, uid] = fields[..] else {
        return Err(invalid());
    };
    // SAFETY: the environment is read before any other thread starts.
    unsafe { std::env::remove_var(OPERATOR_ENV) };
    // SAFETY: getppid has no failure.
    if parent != unsafe { libc::getppid() } as u32 {
        return Ok(None);
    }
    if fd != OPERATOR_FD as u32 || std::env::var_os("LISTEN_PID").is_some() {
        return Err(invalid());
    }
    let option = |name| {
        let mut value = 0i32;
        let mut len = size_of::<i32>() as libc::socklen_t;
        // SAFETY: value and len are live and sized for an int option.
        let rc = unsafe {
            libc::getsockopt(
                OPERATOR_FD,
                libc::SOL_SOCKET,
                name,
                (&raw mut value).cast(),
                &mut len,
            )
        };
        if rc < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(value)
        }
    };
    if crate::host::sys::socket_domain(OPERATOR_FD)? != libc::AF_UNIX
        || option(libc::SO_TYPE)? != libc::SOCK_STREAM
        || !crate::host::sys::listening(OPERATOR_FD)?
    {
        return Err(invalid());
    }
    // SAFETY: descriptor 3 is the parent's listener, checked above; ownership moves here
    // and the descriptor stops passing through any later exec.
    unsafe {
        retry(|| libc::fcntl(OPERATOR_FD, libc::F_SETFD, libc::FD_CLOEXEC) as isize)
            .map_err(io::Error::from_raw_os_error)?;
        Ok(Some((OwnedFd::from_raw_fd(OPERATOR_FD), uid)))
    }
}

/// Whether `listener` is bound at the socket `root`/`path` names: the same inode.
fn bound_at(listener: &OwnedFd, root: BorrowedFd<'_>, path: &str) -> bool {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    // SAFETY: integer-only address storage, sized by len.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_un>() as libc::socklen_t;
    if unsafe { libc::getsockname(listener.as_raw_fd(), (&raw mut addr).cast(), &mut len) } < 0 {
        return false;
    }
    let bytes: Vec<u8> = addr.sun_path
        [..(len as usize).saturating_sub(offset_of!(libc::sockaddr_un, sun_path))]
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    if bytes.first() != Some(&b'/') {
        return false;
    }
    let Ok(bound) = std::fs::symlink_metadata(std::ffi::OsStr::from_bytes(&bytes)) else {
        return false;
    };
    let Ok(target) = pinned_socket(root, path) else {
        return false;
    };
    target
        .identity()
        .is_ok_and(|identity| identity == (bound.dev(), bound.ino()))
}

/// Adopt the launcher's operator listener for a UNIX_LISTEN that names its socket.
fn adopt_operator(
    os: &OsProvider,
    n: &UnixRequest,
    sockets: &mut Sockets,
    listener: OwnedFd,
    uid: u32,
) -> Outcome {
    let Some(root) = os.unix_roots.get(n.root) else {
        return DENIED;
    };
    if !bound_at(&listener, root.as_fd(), n.path) {
        return DENIED;
    }
    let flags = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe {
            libc::fcntl(
                listener.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            )
        } < 0
    {
        return error(host(io::Error::last_os_error()));
    }
    match unsafe { adopt_socket(sockets, listener) } {
        Ok(key) => {
            *os.operator.lock().unwrap() = Some((key, uid));
            success(key)
        }
        Err(code) => error(host(code)),
    }
}

/// The one peer UID listener `key` admits, when it is the adopted operator listener.
pub(crate) fn admitted(os: &OsProvider, key: u64) -> Option<u32> {
    match *os.operator.lock().unwrap() {
        Some((listener, uid)) if listener == key => Some(uid),
        _ => None,
    }
}

fn host(code: io::Error) -> i32 {
    code.raw_os_error().unwrap_or(libc::EIO)
}

fn address(path: &[u8]) -> (libc::sockaddr_un, libc::socklen_t) {
    // SAFETY: sockaddr_un is an integer-only structure.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, byte) in addr.sun_path.iter_mut().zip(path.iter().copied()) {
        *dst = byte as libc::c_char;
    }
    let len = offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    (addr, len as libc::socklen_t)
}

fn socket(kind: i32) -> Result<OwnedFd> {
    // SAFETY: success returns one fresh descriptor.
    let fd = retry(|| crate::host::sys::socket(libc::AF_UNIX, kind))?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

fn pinned_parent<'p>(root: BorrowedFd<'_>, path: &'p str) -> io::Result<(OwnedFd, &'p str)> {
    let (parent, name) = path.rsplit_once('/').unwrap_or((".", path));
    let dir = open_beneath(
        root.as_raw_fd(),
        parent.as_bytes(),
        DIRECTORY_HANDLE,
        Refuse::Symlinks,
    )?;
    Ok((dir, name))
}

pub(crate) fn pinned_socket(root: BorrowedFd<'_>, path: &str) -> io::Result<Endpoint> {
    let (parent, name) =
        pinned_parent(root, path).map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
    Endpoint::pin(parent, name)
}

pub(crate) fn private_bind(fd: BorrowedFd<'_>, parent: OwnedFd, name: &str) -> Result<()> {
    let leaf = std::ffi::CString::new(name).map_err(|_| libc::EINVAL)?;
    let fd = fd.as_raw_fd();
    let bind = move || {
        // SAFETY: fd remains borrowed by the caller until the bind returns.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        hirsel_core_net::unix::bind_in(fd, parent.as_fd(), &leaf).map_err(host)
    };
    if crate::units::in_unit() {
        // Its own transient thread, since the bind changes that thread's cwd.
        crate::units::off_slot(crate::units::HostCall::PeerWait, bind)
    } else {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .spawn_scoped(scope, bind)
                .map_err(host)?
                .join()
                .unwrap()
        })
    }
}

pub(crate) fn bound(
    os: &OsProvider,
    n: &UnixRequest,
    sockets: &mut Sockets,
    listen: bool,
    after_bind: impl FnOnce(i32, i32) -> Result<()>,
) -> Outcome {
    let Some(root) = os.unix_roots.get(n.root) else {
        return DENIED;
    };
    let Ok((parent, name)) = pinned_parent(root.as_fd(), n.path) else {
        return DENIED;
    };
    let make = || {
        socket(if listen {
            libc::SOCK_STREAM
        } else {
            libc::SOCK_DGRAM
        })
    };
    let fd = match make() {
        Ok(fd) => fd,
        Err(code) => return error(code),
    };
    let raw = fd.as_raw_fd();
    let key = match unsafe { adopt_socket(sockets, fd) } {
        Ok(key) => key,
        Err(code) => return error(host(code)),
    };
    // SAFETY: the registry owns raw until this operation takes its release path.
    let result = private_bind(unsafe { BorrowedFd::borrow_raw(raw) }, parent, name);
    let failure = match result {
        Err(libc::EADDRINUSE) => signal(4),
        Err(code) => error(code),
        Ok(()) => {
            if listen && let Err(code) = after_bind(raw, i32::from(n.backlog)) {
                Outcome {
                    error: IoError {
                        tag: 10,
                        code: code as u32,
                    },
                    ..Outcome::default()
                }
            } else {
                return success(key);
            }
        }
    };
    sockets.release(key);
    failure
}

fn success(key: u64) -> Outcome {
    Outcome {
        value: key,
        ..Outcome::default()
    }
}

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

fn endpoint(os: &OsProvider, n: &UnixRequest) -> io::Result<Endpoint> {
    let root = os
        .unix_roots
        .get(n.root)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
    pinned_socket(root.as_fd(), n.path)
}

fn connect(fd: i32, target: &Endpoint) -> Result<isize> {
    unix::reach(target, |path| {
        let (addr, len) = address(path);
        // SAFETY: fd and its pinned target remain live through the syscall.
        retry(|| unsafe {
            libc::connect(fd, (&addr as *const libc::sockaddr_un).cast(), len) as isize
        })
    })
}

/// Accept one connection; on a listener that admits only `uid`, a connection from any
/// other UID is closed and the next one tried.
pub(crate) fn accept(sockets: &mut Sockets, fd: i32, uid: Option<u32>) -> Result<Outcome> {
    let accepted = loop {
        // SAFETY: no peer address is requested. Success returns a fresh descriptor.
        let accepted = retry(|| unsafe {
            crate::host::sys::accept(fd, std::ptr::null_mut(), std::ptr::null_mut())
        })?;
        let accepted = unsafe { OwnedFd::from_raw_fd(accepted as i32) };
        if uid.is_none_or(|uid| crate::host::sys::peer_uid(accepted.as_raw_fd()) == Some(uid)) {
            break accepted;
        }
    };
    unsafe { adopt_socket(sockets, accepted) }
        .map(success)
        .map_err(host)
}

fn send(fd: i32, buf: &Buffer, target: Option<&Endpoint>) -> Result<Outcome> {
    // SAFETY: the caller pins the offered prefix for the send. Linux queues an
    // AF_UNIX datagram atomically, including an empty datagram.
    unsafe {
        if let Some(target) = target {
            unix::reach(target, |path| {
                let (addr, len) = address(path);
                retry(|| {
                    libc::sendto(
                        fd,
                        buf.data.cast(),
                        buf.len,
                        libc::MSG_NOSIGNAL,
                        (&addr as *const libc::sockaddr_un).cast(),
                        len,
                    )
                })
            })?;
        } else {
            retry(|| libc::send(fd, buf.data.cast(), buf.len, libc::MSG_NOSIGNAL))?;
        }
    }
    Ok(Outcome::default())
}

fn recv(fd: i32, buf: &mut Buffer) -> Result<Outcome> {
    let room = buf.capacity.saturating_sub(buf.len);
    let data = if room == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: when there is room the caller owns the pinned buffer.
        unsafe { buf.data.add(buf.len) }
    };
    // SAFETY: `data` is the buffer's spare room.
    let len = retry(|| unsafe { crate::host::sys::receive_packet(fd, data, room) })? as usize;
    let written = room.min(len);
    buf.len += written;
    Ok(Outcome {
        value: len as u64,
        written: written as u64,
        kind: u8::from(len > room),
        ..Outcome::default()
    })
}

fn poll_retry(sockets: &mut Sockets) -> Result<u64> {
    // Linux gives no writable notification on an unconnected sender when
    // the receiver gains room, including a stream connect with a full backlog.
    let timer = crate::host::sys::timer().map_err(host)?;
    unsafe { adopt_socket(sockets, timer) }.map_err(host)
}

fn tick(fd: i32) {
    let _ = retry(|| crate::host::sys::tick(fd));
}

fn finish(n: &mut UnixRequest, outcome: Outcome) -> Step {
    // SAFETY: provider callbacks hold the commit token through registry access.
    let sockets = unsafe { &mut *n.sockets };
    if let Some(timer) = n.retry.take() {
        sockets.disarm(timer, false);
        sockets.release(timer);
    }
    if let Some(key) = n.connecting.take()
        && outcome.error != IoError::OK
    {
        sockets.release(key);
    }
    drop(n.target.take());
    done(outcome)
}

/// SAFETY: the adapter pins the request extension, buffer and registry.
pub(crate) unsafe fn step(os: &OsProvider, request: &mut Request, token: u64, first: bool) -> Step {
    let n = unsafe { &mut *request.path.cast_mut().cast::<UnixRequest>() };
    let sockets = unsafe { &mut *n.sockets };
    let op = request.operation;
    if first {
        if op == UNIX_LISTEN
            && let Some((listener, uid)) = os.inherited_unix.lock().unwrap().take()
        {
            return done(adopt_operator(os, n, sockets, listener, uid));
        }
        if matches!(op, UNIX_LISTEN | DATAGRAM_BIND) {
            return done(bound(os, n, sockets, op == UNIX_LISTEN, |fd, backlog| {
                retry(|| unsafe { libc::listen(fd, backlog) as isize }).map(|_| ())
            }));
        }
        if matches!(op, UNIX_CONNECT | DATAGRAM_CONNECT | DATAGRAM_SEND_TO) {
            n.target = match endpoint(os, n) {
                Ok(fd) => Some(fd),
                // ENOENT at the leaf is a host error. Resolution of an unsafe
                // parent or a final non-socket is always Denied.
                Err(e) => return done(error(host(e))),
            };
            #[cfg(test)]
            AFTER_RESOLVE.with_borrow_mut(|after| {
                if let Some(after) = after.take() {
                    after();
                }
            });
        }
        if matches!(op, UNIX_CONNECT | DATAGRAM_OPEN) {
            let kind = if op == UNIX_CONNECT {
                libc::SOCK_STREAM
            } else {
                libc::SOCK_DGRAM
            };
            let key = match socket(kind)
                .and_then(|fd| unsafe { adopt_socket(sockets, fd) }.map_err(host))
            {
                Ok(key) => key,
                Err(e) => return finish(n, error(e)),
            };
            if op == DATAGRAM_OPEN {
                return done(success(key));
            }
            n.connecting = Some(key);
        }
    }
    let key = n.connecting.unwrap_or(request.handle);
    let writable = op == DATAGRAM_SEND;
    if !first {
        let interest = n.retry.unwrap_or(key);
        sockets.disarm(interest, n.retry.is_none() && writable);
        if let Some(timer) = n.retry {
            tick(sockets.fd(timer));
        }
    }
    let fd = sockets.fd(key);
    let attempt = match op {
        UNIX_CONNECT | DATAGRAM_CONNECT => connect(fd, n.target.as_ref().unwrap()).map(|_| {
            if op == UNIX_CONNECT {
                success(key)
            } else {
                Outcome::default()
            }
        }),
        UNIX_ACCEPT => accept(sockets, fd, admitted(os, request.handle)),
        DATAGRAM_SEND | DATAGRAM_SEND_TO => send(fd, &request.buffer, n.target.as_ref()),
        DATAGRAM_RECV => recv(fd, &mut request.buffer),
        _ => unreachable!(),
    };
    match attempt {
        Ok(outcome) => return finish(n, outcome),
        Err(libc::EAGAIN) if op != DATAGRAM_CONNECT => {}
        Err(e) => return finish(n, error(e)),
    }
    let due = unsafe { deadline(n.until, token) };
    if due == Due::Passed {
        return finish(n, signal(6));
    }
    if matches!(op, UNIX_CONNECT | DATAGRAM_SEND_TO) && n.retry.is_none() {
        n.retry = match poll_retry(sockets) {
            Ok(timer) => Some(timer),
            Err(e) => return finish(n, error(e)),
        };
    }
    if let Err(e) = sockets.arm(n.retry.unwrap_or(key), n.retry.is_none() && writable, token) {
        return finish(n, error(host(e)));
    }
    Step {
        kind: STEP_PENDING,
        deadline_ns: due.host().unwrap_or(0),
        ..Step::default()
    }
}

/// SAFETY: the adapter pins the request extension and the operation is pending.
pub(crate) unsafe fn abort(
    os: &OsProvider,
    request: &mut Request,
    token: u64,
    reason: u8,
) -> Outcome {
    if reason == ABORT_DEADLINE {
        let step = unsafe { step(os, request, token, false) };
        if step.kind == STEP_DONE {
            return step.outcome;
        }
    }
    let n = unsafe { &mut *request.path.cast_mut().cast::<UnixRequest>() };
    let sockets = unsafe { &mut *n.sockets };
    let key = n.connecting.unwrap_or(request.handle);
    if n.retry.is_none() {
        sockets.disarm(key, request.operation == DATAGRAM_SEND);
    }
    finish(n, signal(if reason == ABORT_DEADLINE { 6 } else { 7 })).outcome
}
