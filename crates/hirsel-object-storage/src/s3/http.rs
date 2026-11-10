//! HTTP/1.1 exchanges over persistent connections, driven by readiness on the
//! caller's thread.
//!
//! A [`Slot`] owns one connection and every buffer an exchange on it needs:
//! the request head, the received bytes and a body buffer, each allocated once
//! and reused. [`Driver::run`] drives up to [`SLOTS`] exchanges at once with
//! nonblocking sockets and `poll(2)`; it starts no thread. Responses are framed
//! by `Content-Length` or `chunked` only (no trailers, no read-until-close), the
//! head is capped at [`HEAD_CAP`] bytes, and anything else is malformed.
//!
//! Structure follows the request/response state machine of the httparse and
//! hyper `proto::h1` decoders (head, length-delimited body, chunk size line,
//! chunk data, chunk CRLF, last chunk), cut down to the framings S3 uses. TLS is
//! rustls 0.23 driven as `rustls::Stream` does (`src/stream.rs`, `complete_io`):
//! `write_tls` while it wants to write, `read_tls` then `process_new_packets`,
//! then read plaintext.

use rustls::ClientConnection;
use std::io::{ErrorKind, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Connections a provider keeps, and so the most exchanges in flight.
pub(crate) const SLOTS: usize = 8;
/// The largest response head.
pub(crate) const HEAD_CAP: usize = 16 << 10;
/// Received bytes buffered per connection.
const IN_CAP: usize = 64 << 10;
/// The largest body read into a connection's own buffer (listings, errors).
pub(crate) const SCRATCH_BODY_CAP: usize = 16 << 20;
/// The largest object a GET returns.
pub(crate) const OBJECT_CAP: usize = 1 << 30;
/// A pooled connection idle longer than this is closed rather than reused:
/// servers close idle keep-alive connections (commonly after 5 s or more).
const IDLE_MAX: Duration = Duration::from_secs(4);
/// The longest ETag kept.
pub(crate) const ETAG_CAP: usize = 128;

/// Where and how to connect.
pub(crate) struct Endpoint {
    pub addrs: Vec<SocketAddr>,
    pub tls: Option<(
        Arc<rustls::ClientConfig>,
        rustls::pki_types::ServerName<'static>,
    )>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

/// Why an exchange did not produce a response. Every one leaves it unknown
/// whether the server applied the request.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Fail {
    ConnectTimeout,
    Connect,
    Timeout,
    /// The connection closed or reset before the response was complete.
    Lost,
    /// A response violated the framing rules or a cap.
    Malformed(&'static str),
    Tls,
}

impl Fail {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Fail::ConnectTimeout => "connect timed out",
            Fail::Connect => "connect failed",
            Fail::Timeout => "request timed out",
            Fail::Lost => "connection lost before the response completed",
            Fail::Malformed(what) => what,
            Fail::Tls => "TLS failure",
        }
    }
}

/// One response's head facts; its body is in the slot or owned.
#[derive(Debug)]
pub(crate) struct Response {
    pub status: u16,
    etag: [u8; ETAG_CAP],
    etag_len: usize,
    /// The body of a response whose request asked to own it (a GET's 200).
    pub owned: Option<Vec<u8>>,
}

impl Response {
    pub(crate) fn etag(&self) -> Option<&str> {
        (self.etag_len > 0).then(|| std::str::from_utf8(&self.etag[..self.etag_len]).unwrap())
    }
}

/// One request to drive: its head (already in the slot's `head` buffer), its
/// body, whether it may be resent on a fresh connection when a reused one fails
/// before any response byte, and whether a 2xx body belongs to the caller.
#[derive(Clone, Copy)]
pub(crate) struct Request<'a> {
    pub body: &'a [u8],
    pub idempotent: bool,
    pub own_body: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Phase {
    Connecting,
    Sending,
    Head,
    Body(usize),
    ChunkSize,
    ChunkData(usize),
    ChunkEnd,
    LastChunk,
    Done,
}

struct Live {
    sock: TcpStream,
    tls: Option<Box<ClientConnection>>,
    used: Instant,
}

/// A connection and the buffers its exchanges reuse.
pub(crate) struct Slot {
    live: Option<Live>,
    /// The request head the caller writes before [`Driver::run`].
    pub head: Vec<u8>,
    input: Vec<u8>,
    start: usize,
    end: usize,
    /// The body of a response not owned by the caller.
    pub body: Vec<u8>,
    // The exchange in progress.
    phase: Phase,
    sent: usize,
    deadline: Instant,
    reused: bool,
    retried: bool,
    received: bool,
    close: bool,
    status: u16,
    etag: [u8; ETAG_CAP],
    etag_len: usize,
    owned: Option<Vec<u8>>,
    want_write: bool,
}

impl Slot {
    fn new() -> Self {
        Slot {
            live: None,
            head: Vec::with_capacity(2048),
            input: vec![0; IN_CAP],
            start: 0,
            end: 0,
            body: Vec::with_capacity(8 << 10),
            phase: Phase::Done,
            sent: 0,
            deadline: Instant::now(),
            reused: false,
            retried: false,
            received: false,
            close: false,
            status: 0,
            etag: [0; ETAG_CAP],
            etag_len: 0,
            owned: None,
            want_write: false,
        }
    }
}

/// The connections of one provider.
pub(crate) struct Driver {
    pub endpoint: Endpoint,
    pub slots: Vec<Slot>,
    /// Which address to try next.
    next_addr: usize,
    /// Requests sent, counting resends.
    pub requests: u64,
}

enum Step {
    Wait,
    Done(Result<Response, Fail>),
}

impl Driver {
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Driver {
            endpoint,
            slots: (0..SLOTS).map(|_| Slot::new()).collect(),
            next_addr: 0,
            requests: 0,
        }
    }

    /// Drive `n` requests, at most [`SLOTS`] at a time. `prepare(i, head)`
    /// writes request `i`'s head into `head` and returns its [`Request`];
    /// `finish(i, result, body)` receives its outcome, with the slot's body
    /// buffer for a response that does not own its body.
    pub(crate) fn run<'a>(
        &mut self,
        n: usize,
        mut prepare: impl FnMut(usize, &mut Vec<u8>) -> Request<'a>,
        mut finish: impl FnMut(usize, Result<Response, Fail>, &[u8]),
    ) {
        let mut assigned: [Option<(usize, Request<'a>)>; SLOTS] = [None; SLOTS];
        let mut next = 0;
        loop {
            for s in 0..SLOTS {
                if assigned[s].is_none() && next < n {
                    self.slots[s].head.clear();
                    let request = prepare(next, &mut self.slots[s].head);
                    self.begin(s, false);
                    assigned[s] = Some((next, request));
                    next += 1;
                }
            }
            if assigned.iter().all(Option::is_none) {
                return;
            }
            let mut progressed = false;
            for s in 0..SLOTS {
                let Some((i, request)) = assigned[s] else {
                    continue;
                };
                match self.step(s, &request) {
                    Step::Wait => {}
                    Step::Done(Err(fail))
                        if request.idempotent
                            && self.slots[s].reused
                            && !self.slots[s].received
                            && !self.slots[s].retried =>
                    {
                        // A pooled connection the server had closed: resend once
                        // on a fresh one.
                        self.drop_live(s);
                        let _ = fail;
                        self.begin(s, true);
                        progressed = true;
                    }
                    Step::Done(result) => {
                        if result.is_err() || self.slots[s].close {
                            self.drop_live(s);
                        } else if let Some(live) = &mut self.slots[s].live {
                            live.used = Instant::now();
                        }
                        finish(i, result, &self.slots[s].body);
                        assigned[s] = None;
                        progressed = true;
                    }
                }
            }
            if progressed {
                continue;
            }
            self.wait(&assigned);
        }
    }

    /// A driver with no connections, standing in while the real one is
    /// borrowed out of its client.
    pub(crate) fn placeholder() -> Self {
        Driver {
            endpoint: Endpoint {
                addrs: Vec::new(),
                tls: None,
                connect_timeout: Duration::ZERO,
                request_timeout: Duration::ZERO,
            },
            slots: Vec::new(),
            next_addr: 0,
            requests: 0,
        }
    }

    fn drop_live(&mut self, s: usize) {
        self.slots[s].live = None;
    }

    /// Start an exchange on slot `s`: reuse its connection when it is still
    /// open and quiet, else start connecting.
    fn begin(&mut self, s: usize, retry: bool) {
        let now = Instant::now();
        let slot = &mut self.slots[s];
        slot.retried = retry;
        slot.sent = 0;
        slot.received = false;
        slot.close = false;
        slot.status = 0;
        slot.etag_len = 0;
        slot.owned = None;
        slot.body.clear();
        slot.start = 0;
        slot.end = 0;
        slot.want_write = false;
        self.requests += 1;
        let reusable = slot
            .live
            .as_ref()
            .is_some_and(|l| now.duration_since(l.used) < IDLE_MAX && quiet(&l.sock));
        if reusable {
            slot.reused = true;
            slot.phase = Phase::Sending;
            slot.deadline = now + self.endpoint.request_timeout;
            if let Some(tls) = &mut slot.live.as_mut().unwrap().tls {
                // rustls buffers the request until the connection can take it.
                let _ = tls.writer().write_all(&slot.head);
            }
            return;
        }
        slot.live = None;
        slot.reused = false;
        slot.deadline = now + self.endpoint.connect_timeout;
        let addr = self.endpoint.addrs[self.next_addr % self.endpoint.addrs.len()];
        match connect_start(addr) {
            Ok(sock) => {
                slot.live = Some(Live {
                    sock,
                    tls: None,
                    used: now,
                });
                slot.phase = Phase::Connecting;
            }
            Err(_) => {
                self.next_addr += 1;
                slot.phase = Phase::Connecting;
            }
        }
    }

    /// Advance slot `s` as far as it goes without blocking.
    fn step(&mut self, s: usize, request: &Request<'_>) -> Step {
        let now = Instant::now();
        let endpoint = &self.endpoint;
        let slot = &mut self.slots[s];
        if now >= slot.deadline {
            return Step::Done(Err(if slot.phase == Phase::Connecting {
                Fail::ConnectTimeout
            } else {
                Fail::Timeout
            }));
        }
        loop {
            match slot.phase {
                Phase::Connecting => {
                    let Some(live) = &mut slot.live else {
                        self.next_addr += 1;
                        return Step::Done(Err(Fail::Connect));
                    };
                    if !writable(&live.sock) {
                        slot.want_write = true;
                        return Step::Wait;
                    }
                    match live.sock.take_error() {
                        Ok(None) => {}
                        _ => {
                            self.next_addr += 1;
                            return Step::Done(Err(Fail::Connect));
                        }
                    }
                    let _ = live.sock.set_nodelay(true);
                    if let Some((config, name)) = &endpoint.tls {
                        match ClientConnection::new(config.clone(), name.clone()) {
                            Ok(mut tls) => {
                                let _ = tls.writer().write_all(&slot.head);
                                live.tls = Some(Box::new(tls));
                            }
                            Err(_) => return Step::Done(Err(Fail::Tls)),
                        }
                    }
                    slot.deadline = now + endpoint.request_timeout;
                    slot.phase = Phase::Sending;
                }
                Phase::Sending => {
                    let live = slot.live.as_mut().unwrap();
                    if let Some(tls) = &mut live.tls {
                        if slot.sent == 0 && !request.body.is_empty() {
                            let _ = tls.writer().write_all(request.body);
                        }
                        slot.sent = 1;
                        slot.phase = Phase::Head;
                        continue;
                    }
                    let total = slot.head.len() + request.body.len();
                    while slot.sent < total {
                        let (h, b) = if slot.sent < slot.head.len() {
                            (&slot.head[slot.sent..], request.body)
                        } else {
                            (&[][..], &request.body[slot.sent - slot.head.len()..])
                        };
                        match live
                            .sock
                            .write_vectored(&[IoSlice::new(h), IoSlice::new(b)])
                        {
                            Ok(0) => return Step::Done(Err(Fail::Lost)),
                            Ok(k) => slot.sent += k,
                            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                                slot.want_write = true;
                                return Step::Wait;
                            }
                            Err(e) if e.kind() == ErrorKind::Interrupted => {}
                            Err(_) => return Step::Done(Err(Fail::Lost)),
                        }
                    }
                    slot.phase = Phase::Head;
                }
                Phase::Done => {
                    if slot.start != slot.end {
                        // Bytes past the response: the connection is unusable.
                        slot.close = true;
                    }
                    return Step::Done(Ok(Response {
                        status: slot.status,
                        etag: slot.etag,
                        etag_len: slot.etag_len,
                        owned: slot.owned.take(),
                    }));
                }
                _ => {
                    match parse(slot, request) {
                        Err(fail) => return Step::Done(Err(fail)),
                        Ok(true) => continue,
                        Ok(false) => {}
                    }
                    match fill(slot) {
                        Ok(true) => {}
                        Ok(false) => return Step::Wait,
                        Err(fail) => return Step::Done(Err(fail)),
                    }
                }
            }
        }
    }

    /// Block until a slot in flight can progress or its deadline passes.
    fn wait(&mut self, assigned: &[Option<(usize, Request<'_>)>; SLOTS]) {
        let mut fds = [libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }; SLOTS];
        let now = Instant::now();
        let mut soonest = Duration::from_secs(3600);
        for (s, fd) in fds.iter_mut().enumerate() {
            if assigned[s].is_none() {
                continue;
            }
            let slot = &self.slots[s];
            soonest = soonest.min(slot.deadline.saturating_duration_since(now));
            let Some(live) = &slot.live else {
                soonest = Duration::ZERO;
                continue;
            };
            fd.fd = live.sock.as_raw_fd();
            let tls_writes = live.tls.as_ref().is_some_and(|t| t.wants_write());
            fd.events = match slot.phase {
                Phase::Connecting | Phase::Sending => libc::POLLOUT,
                _ if tls_writes || slot.want_write => libc::POLLIN | libc::POLLOUT,
                _ => libc::POLLIN,
            };
        }
        let ms = soonest.as_millis().min(i32::MAX as u128) as i32;
        let ms = if soonest > Duration::ZERO && ms == 0 {
            1
        } else {
            ms
        };
        // SAFETY: `fds` is a valid array of SLOTS pollfd records; entries with a
        // negative descriptor are ignored.
        unsafe { libc::poll(fds.as_mut_ptr(), SLOTS as libc::nfds_t, ms) };
        for slot in &mut self.slots {
            slot.want_write = false;
        }
    }
}

/// Whether a pooled socket has nothing to read and no hangup: a server that
/// closed it, or sent anything unsolicited, makes it unusable.
fn quiet(sock: &TcpStream) -> bool {
    let mut fd = libc::pollfd {
        fd: sock.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one valid pollfd record, zero timeout.
    unsafe { libc::poll(&mut fd, 1, 0) == 0 }
}

fn writable(sock: &TcpStream) -> bool {
    let mut fd = libc::pollfd {
        fd: sock.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one valid pollfd record, zero timeout.
    unsafe { libc::poll(&mut fd, 1, 0) > 0 }
}

/// A nonblocking socket with its connect to `addr` started.
fn connect_start(addr: SocketAddr) -> std::io::Result<TcpStream> {
    let family = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    // SAFETY: plain socket creation; the descriptor is owned by the TcpStream
    // made from it below, or closed on failure.
    let fd = unsafe {
        libc::socket(
            family,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh socket this function owns.
    let sock = unsafe { TcpStream::from_raw_fd(fd) };
    let (storage, len) = sockaddr(addr);
    // SAFETY: `storage` holds a sockaddr of `len` bytes for the socket's family.
    let r = unsafe { libc::connect(fd, (&raw const storage).cast(), len) };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(e);
        }
    }
    Ok(sock)
}

fn sockaddr(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: an all-zero sockaddr_storage is valid.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let len = match addr {
        SocketAddr::V4(a) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: a.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(a.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: sockaddr_storage is larger than and aligned for sockaddr_in.
            unsafe { std::ptr::write((&raw mut storage).cast(), sin) };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(a) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: a.port().to_be(),
                sin6_flowinfo: a.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: a.ip().octets(),
                },
                sin6_scope_id: a.scope_id(),
            };
            // SAFETY: sockaddr_storage is larger than and aligned for sockaddr_in6.
            unsafe { std::ptr::write((&raw mut storage).cast(), sin6) };
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, len as libc::socklen_t)
}

/// Read what is available into the slot's input. `Ok(true)`: bytes arrived;
/// `Ok(false)`: nothing yet.
fn fill(slot: &mut Slot) -> Result<bool, Fail> {
    if slot.start == slot.end {
        slot.start = 0;
        slot.end = 0;
    } else if slot.end == slot.input.len() {
        slot.input.copy_within(slot.start..slot.end, 0);
        slot.end -= slot.start;
        slot.start = 0;
    }
    if slot.end == slot.input.len() {
        return Err(Fail::Malformed("response line or head exceeds the buffer"));
    }
    let live = slot.live.as_mut().ok_or(Fail::Lost)?;
    let room = &mut slot.input[slot.end..];
    let got = match &mut live.tls {
        None => loop {
            match live.sock.read(room) {
                Ok(0) => return Err(Fail::Lost),
                Ok(k) => break k,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(Fail::Lost),
            }
        },
        Some(tls) => loop {
            // Send whatever the session holds (handshake, request).
            while tls.wants_write() {
                match tls.write_tls(&mut live.sock) {
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        slot.want_write = true;
                        break;
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => return Err(Fail::Lost),
                }
            }
            match tls.reader().read(room) {
                Ok(0) => return Err(Fail::Lost),
                Ok(k) => break k,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Err(Fail::Lost),
                Err(_) => return Err(Fail::Tls),
            }
            match tls.read_tls(&mut live.sock) {
                Ok(0) => return Err(Fail::Lost),
                Ok(_) => {
                    if tls.process_new_packets().is_err() {
                        // Send the alert rustls queued, best effort.
                        let _ = tls.write_tls(&mut live.sock);
                        return Err(Fail::Tls);
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(Fail::Lost),
            }
        },
    };
    slot.end += got;
    slot.received = true;
    Ok(true)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn trim(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(s, |e| e + 1);
    &b[s..e]
}

fn decimal(b: &[u8]) -> Option<usize> {
    if b.is_empty() || b.len() > 15 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(b.iter().fold(0, |n, d| n * 10 + (d - b'0') as usize))
}

/// Parse what the slot's input holds. `Ok(true)`: the phase advanced;
/// `Ok(false)`: more bytes are needed.
fn parse(slot: &mut Slot, request: &Request<'_>) -> Result<bool, Fail> {
    let avail = &slot.input[slot.start..slot.end];
    match slot.phase {
        Phase::Head => {
            let Some(at) = find(avail, b"\r\n\r\n") else {
                return if avail.len() > HEAD_CAP {
                    Err(Fail::Malformed("response head exceeds its cap"))
                } else {
                    Ok(false)
                };
            };
            if at > HEAD_CAP {
                return Err(Fail::Malformed("response head exceeds its cap"));
            }
            let head = &avail[..at];
            let mut lines = head
                .split(|&b| b == b'\n')
                .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
            let status_line = lines.next().unwrap_or_default();
            let (version, rest) = match status_line {
                [b'H', b'T', b'T', b'P', b'/', b'1', b'.', v, b' ', rest @ ..] => (*v, rest),
                _ => return Err(Fail::Malformed("malformed status line")),
            };
            if !matches!(version, b'0' | b'1') || rest.len() < 3 {
                return Err(Fail::Malformed("malformed status line"));
            }
            let status = decimal(&rest[..3])
                .filter(|s| (100..600).contains(s))
                .ok_or(Fail::Malformed("malformed status code"))? as u16;
            let mut length = None;
            let mut chunked = false;
            let mut close = version == b'0';
            let mut etag_len = 0;
            for line in lines {
                let Some(colon) = line.iter().position(|&b| b == b':') else {
                    return Err(Fail::Malformed("malformed header line"));
                };
                let (name, value) = (&line[..colon], trim(&line[colon + 1..]));
                if name.eq_ignore_ascii_case(b"content-length") {
                    let n = decimal(value).ok_or(Fail::Malformed("malformed Content-Length"))?;
                    if length.is_some_and(|l| l != n) {
                        return Err(Fail::Malformed("conflicting Content-Length"));
                    }
                    length = Some(n);
                } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
                    if !value.eq_ignore_ascii_case(b"chunked") {
                        return Err(Fail::Malformed("unsupported Transfer-Encoding"));
                    }
                    chunked = true;
                } else if name.eq_ignore_ascii_case(b"connection") {
                    if value
                        .split(|&b| b == b',')
                        .any(|t| trim(t).eq_ignore_ascii_case(b"close"))
                    {
                        close = true;
                    }
                } else if name.eq_ignore_ascii_case(b"etag") {
                    if value.len() > ETAG_CAP
                        || value.is_empty()
                        || !value.iter().all(|b| (0x21..0x7f).contains(b))
                    {
                        return Err(Fail::Malformed("malformed ETag"));
                    }
                    slot.etag[..value.len()].copy_from_slice(value);
                    etag_len = value.len();
                }
            }
            slot.start += at + 4;
            if status < 200 {
                // An interim response (100 Continue): the real one follows.
                return Ok(true);
            }
            if chunked && length.is_some() {
                return Err(Fail::Malformed("both Content-Length and chunked"));
            }
            slot.status = status;
            slot.etag_len = etag_len;
            slot.close = close;
            let own = request.own_body && (200..300).contains(&status);
            let cap = if own { OBJECT_CAP } else { SCRATCH_BODY_CAP };
            slot.phase = if status == 204 || status == 304 {
                Phase::Done
            } else if chunked {
                Phase::ChunkSize
            } else if let Some(n) = length {
                if n > cap {
                    return Err(Fail::Malformed("response body exceeds its cap"));
                }
                if own {
                    slot.owned = Some(Vec::with_capacity(n));
                }
                Phase::Body(n)
            } else {
                return Err(Fail::Malformed(
                    "response body has neither Content-Length nor chunked framing",
                ));
            };
            if own && slot.owned.is_none() {
                slot.owned = Some(Vec::new());
            }
            Ok(true)
        }
        Phase::Body(0) => {
            slot.phase = Phase::Done;
            Ok(true)
        }
        Phase::Body(left) | Phase::ChunkData(left) => {
            if avail.is_empty() {
                return Ok(false);
            }
            let take = left.min(avail.len());
            let cap = if slot.owned.is_some() {
                OBJECT_CAP
            } else {
                SCRATCH_BODY_CAP
            };
            let into = slot.owned.as_mut().unwrap_or(&mut slot.body);
            if into.len() + take > cap {
                return Err(Fail::Malformed("response body exceeds its cap"));
            }
            into.extend_from_slice(&avail[..take]);
            slot.start += take;
            slot.phase = match slot.phase {
                Phase::Body(_) => Phase::Body(left - take),
                _ if left == take => Phase::ChunkEnd,
                _ => Phase::ChunkData(left - take),
            };
            Ok(true)
        }
        Phase::ChunkSize => {
            let Some(at) = find(avail, b"\r\n") else {
                return if avail.len() > 1024 {
                    Err(Fail::Malformed("chunk size line exceeds its cap"))
                } else {
                    Ok(false)
                };
            };
            let line = &avail[..at];
            let digits = line.split(|&b| b == b';').next().unwrap_or_default();
            let digits = trim(digits);
            if digits.is_empty() || digits.len() > 15 || !digits.iter().all(u8::is_ascii_hexdigit) {
                return Err(Fail::Malformed("malformed chunk size"));
            }
            let size = digits.iter().fold(0usize, |n, d| {
                n * 16 + (*d as char).to_digit(16).unwrap() as usize
            });
            slot.start += at + 2;
            slot.phase = if size == 0 {
                Phase::LastChunk
            } else {
                Phase::ChunkData(size)
            };
            Ok(true)
        }
        Phase::ChunkEnd | Phase::LastChunk => {
            if avail.len() < 2 {
                return Ok(false);
            }
            if &avail[..2] != b"\r\n" {
                return Err(Fail::Malformed(if slot.phase == Phase::LastChunk {
                    "chunked trailers are not accepted"
                } else {
                    "malformed chunk end"
                }));
            }
            slot.start += 2;
            slot.phase = if slot.phase == Phase::LastChunk {
                Phase::Done
            } else {
                Phase::ChunkSize
            };
            Ok(true)
        }
        Phase::Connecting | Phase::Sending | Phase::Done => Ok(true),
    }
}
