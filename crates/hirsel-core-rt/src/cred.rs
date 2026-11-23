//! TLS-7 credential grants. The provider alone contacts the credential source,
//! on a helper thread, and seals the secret access key as it parses the reply:
//! no byte of the key is ever written to program memory. The key id, session
//! token and expiry are public and reach the program's strings.
use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    net::{Address, NetworkRequest, deadline},
    os_net::{Due, Operations, deadline as due},
    provider::*,
    secret::{Origin, Secret},
    tasks::Outside,
};
use hirsel_core_net::{CredGrant, CredSource, SecretPolicy};
use std::{
    io::{Read, Write},
    net::{SocketAddr, SocketAddrV4, TcpStream},
    os::fd::OwnedFd,
    ptr,
    sync::{Arc, Mutex},
    time::Duration,
};
use zeroize::Zeroizing;

/// A `Cred` grant as the run holds it: its record, parsed source and policy,
/// and the provider handle its fetches carry (a tape's grant handle, or 0).
pub(crate) struct Granted {
    pub(crate) record: CredGrant,
    source: CredSource,
    policy: SecretPolicy,
    handle: u64,
}

impl Granted {
    pub(crate) fn read([name, source, policy]: [&[u8]; 3]) -> Option<Self> {
        let text = |view| std::str::from_utf8(view).ok();
        Self::bound(
            CredGrant::new(text(name)?, text(source)?, text(policy)?).ok()?,
            0,
        )
    }

    /// The grant behind a tape handle. Parsing reads nothing from the source.
    pub(crate) fn bound(record: CredGrant, handle: u64) -> Option<Self> {
        Some(Self {
            source: CredSource::parse(&record.source).ok()?,
            policy: SecretPolicy::parse(&record.policy)?,
            record,
            handle,
        })
    }
}

/// The outcome error tags (`CredError` case plus one; `core_ops` lowers them).
const ROOM: u8 = 1;
const NET: u8 = 2;
const STATUS: u8 = 4;
const MALFORMED: u8 = 5;
/// The designation's admission was refused; the source raises it (MEM-21).
const REFUSED: u8 = 6;
/// NetError tags carried in `written` under NET.
const NET_REFUSED: u64 = 2;
const NET_UNREACHABLE: u64 = 3;
const NET_RESET: u64 = 5;
const NET_TIMED_OUT: u64 = 6;
const NET_ABORTED: u64 = 7;
const NET_OTHER: u64 = 8;

/// The most bytes the provider receives in one reply.
const REPLY_MAX: usize = 64 * 1024;

#[repr(C)]
pub(crate) struct CredRequest {
    pub network: NetworkRequest,
    source: CredSource,
    /// The spare capacities of `key_id` and `token` at issue, for the tape.
    pub(crate) spares: [u64; 2],
    job: Option<Job>,
    /// A delivered success, until the adapter takes it.
    pub(crate) fetched: Option<Fetched>,
}

pub(crate) struct Fetched {
    pub(crate) key_id: Zeroizing<Vec<u8>>,
    pub(crate) token: Zeroizing<Vec<u8>>,
    /// Absent on replay: the designation has no bytes behind it.
    pub(crate) secret: Option<Arc<Zeroizing<Vec<u8>>>>,
    pub(crate) expires_ns: u64,
}

pub(crate) static OPERATIONS: Operations = Operations {
    step,
    abort,
    adopt_listener: |_, _: OwnedFd| unreachable!("a credential fetch cannot adopt a listener"),
};

type Answer = Result<Fetched, Outcome>;

struct Job {
    completion: Arc<Mutex<Completion>>,
}

struct Completion {
    active: Option<Outside>,
    answer: Option<Answer>,
}

impl Drop for Job {
    fn drop(&mut self) {
        // A late reply finds no one waiting; its buffers are zeroed as it drops.
        self.completion.lock().unwrap().active.take();
    }
}

fn failure(tag: u8, value: u64, code: u32, written: u64) -> Outcome {
    Outcome {
        value,
        error: IoError { tag, code },
        written,
        ..Outcome::default()
    }
}

fn net(tag: u64) -> Outcome {
    failure(NET, 0, 0, tag)
}

fn done(outcome: Outcome) -> Step {
    Step {
        outcome,
        ..Step::default()
    }
}

unsafe fn step(request: *mut Request, token: u64, first: bool) -> Step {
    // SAFETY: CRED_FETCH pins its adapter extension through delivery.
    let cred = unsafe { &mut *(*request).path.cast_mut().cast::<CredRequest>() };
    // SAFETY: the adapter pins the optional clock for the request.
    let due = unsafe { due(cred.network.until, token) };
    if due == Due::Passed {
        cred.job.take();
        return done(net(NET_TIMED_OUT));
    }
    let deadline = due.host();
    if first {
        let mut submit = Submit {
            token,
            deadline,
            source: cred.source,
        };
        // SAFETY: submission is synchronous and hands the helper owned data.
        let outcome = unsafe {
            crate::tasks::host_operation((&raw mut submit).cast(), request, submit_on_stack)
        };
        if outcome.error != IoError::OK {
            return done(outcome);
        }
    } else {
        let job = cred.job.as_ref().unwrap();
        let answer = job.completion.lock().unwrap().answer.take();
        if let Some(answer) = answer {
            cred.job.take();
            return done(match answer {
                Ok(fetched) => {
                    cred.fetched = Some(fetched);
                    Outcome::default()
                }
                Err(outcome) => outcome,
            });
        }
    }
    Step {
        kind: STEP_PENDING,
        deadline_ns: deadline.unwrap_or(0),
        ..Step::default()
    }
}

struct Submit {
    token: u64,
    deadline: Option<u64>,
    source: CredSource,
}

unsafe extern "C" fn submit_on_stack(state: *mut u8, request: *mut Request) -> Outcome {
    // SAFETY: step pins the arguments until this callback returns.
    let (submit, cred) = unsafe {
        (
            &mut *state.cast::<Submit>(),
            &mut *(*request).path.cast_mut().cast::<CredRequest>(),
        )
    };
    let (token, deadline, source) = (submit.token, submit.deadline, submit.source);
    let completion = Arc::new(Mutex::new(Completion {
        active: Some(Outside::begin()),
        answer: None,
    }));
    let result = completion.clone();
    let blocking = crate::units::blocking();
    let helper = std::thread::Builder::new()
        .name("hirsel-cred".into())
        .spawn(move || {
            let answer = match source {
                CredSource::Imds(at) => imds(at, deadline),
            };
            let mut result = result.lock().unwrap();
            if let Some(active) = result.active.take() {
                result.answer = Some(answer);
                active.complete(token);
            }
        });
    drop(blocking);
    match helper {
        Ok(helper) => {
            drop(helper);
            cred.job = Some(Job { completion });
            Outcome::default()
        }
        Err(error) => failure(
            NET,
            0,
            error.raw_os_error().unwrap_or(libc::EAGAIN) as u32,
            NET_OTHER,
        ),
    }
}

unsafe fn abort(request: *mut Request, _: u64, reason: u8) -> Outcome {
    // SAFETY: the scheduler exclusively owns the pinned adapter during abort.
    let cred = unsafe { &mut *(*request).path.cast_mut().cast::<CredRequest>() };
    cred.job.take();
    net(if reason == ABORT_CANCEL {
        NET_ABORTED
    } else {
        NET_TIMED_OUT
    })
}

/// The time left before the host deadline, for the helper's socket calls.
fn remaining(deadline: Option<u64>) -> Result<Option<Duration>, Outcome> {
    let Some(at) = deadline else {
        return Ok(None);
    };
    let now = crate::os::monotonic_ns();
    if at <= now {
        return Err(net(NET_TIMED_OUT));
    }
    Ok(Some(Duration::from_nanos(at - now)))
}

fn io_failure(error: &std::io::Error) -> Outcome {
    use std::io::ErrorKind::*;
    match error.kind() {
        ConnectionRefused => net(NET_REFUSED),
        NetworkUnreachable | HostUnreachable | AddrNotAvailable => net(NET_UNREACHABLE),
        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof => net(NET_RESET),
        TimedOut | WouldBlock => net(NET_TIMED_OUT),
        _ => failure(NET, 0, error.raw_os_error().unwrap_or(0) as u32, NET_OTHER),
    }
}

/// One HTTP/1.1 exchange on its own connection. The reply lands in a
/// fixed buffer of `REPLY_MAX` bytes that never grows and is zeroed on drop.
/// Returns the buffer and the body's range.
fn exchange(
    at: SocketAddrV4,
    request: &[u8],
    deadline: Option<u64>,
) -> Result<(Zeroizing<Vec<u8>>, std::ops::Range<usize>), Outcome> {
    let address = SocketAddr::V4(at);
    let mut stream = match remaining(deadline)? {
        Some(left) => TcpStream::connect_timeout(&address, left),
        None => TcpStream::connect(address),
    }
    .map_err(|e| io_failure(&e))?;
    let left = remaining(deadline)?;
    stream
        .set_write_timeout(left)
        .and_then(|()| stream.write_all(request))
        .map_err(|e| io_failure(&e))?;
    let mut reply = Zeroizing::new(vec![0u8; REPLY_MAX + 1]);
    let mut len = 0;
    loop {
        stream
            .set_read_timeout(remaining(deadline)?)
            .map_err(|e| io_failure(&e))?;
        match stream.read(&mut reply[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_failure(&e)),
        }
        if len > REPLY_MAX {
            return Err(failure(MALFORMED, 0, 0, 0));
        }
    }
    let head_end = find(&reply[..len], b"\r\n\r\n").ok_or(failure(MALFORMED, 0, 0, 0))?;
    let head = &reply[..head_end];
    let status = head
        .strip_prefix(b"HTTP/1.1 ")
        .or_else(|| head.strip_prefix(b"HTTP/1.0 "))
        .and_then(|rest| rest.get(..3))
        .and_then(|code| std::str::from_utf8(code).ok()?.parse::<u16>().ok())
        .ok_or(failure(MALFORMED, 0, 0, 0))?;
    if status != 200 {
        return Err(failure(STATUS, u64::from(status), 0, 0));
    }
    let mut body = head_end + 4..len;
    for line in head.split(|&b| b == b'\n').skip(1) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let (name, value) = (&line[..colon], line[colon + 1..].trim_ascii());
        if name.eq_ignore_ascii_case(b"transfer-encoding") {
            return Err(failure(MALFORMED, 0, 0, 0));
        }
        if name.eq_ignore_ascii_case(b"content-length") {
            let n = std::str::from_utf8(value)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|&n| n <= body.len())
                .ok_or(failure(MALFORMED, 0, 0, 0))?;
            body.end = body.start + n;
        }
    }
    Ok((reply, body))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A request text built in a buffer of exact capacity, zeroed on drop.
fn request(parts: &[&[u8]]) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(parts.iter().map(|p| p.len()).sum()));
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

/// IMDSv2: a session token, the role name, then the role's credentials.
fn imds(at: SocketAddrV4, deadline: Option<u64>) -> Answer {
    let host = Zeroizing::new(format!("Host: {}\r\n", at.ip()).into_bytes());
    let put = request(&[
        b"PUT /latest/api/token HTTP/1.1\r\n",
        &host,
        b"X-aws-ec2-metadata-token-ttl-seconds: 21600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ]);
    let (reply, body) = exchange(at, &put, deadline)?;
    let session = &reply[body];
    if session.is_empty() || !session.iter().all(|b| b.is_ascii_graphic()) {
        return Err(failure(MALFORMED, 0, 0, 0));
    }
    const ROLES: &[u8] = b"/latest/meta-data/iam/security-credentials/";
    let get = |path: &[&[u8]]| {
        let mut parts: Vec<&[u8]> = vec![b"GET "];
        parts.extend_from_slice(path);
        parts.extend_from_slice(&[
            b" HTTP/1.1\r\n",
            &host,
            b"X-aws-ec2-metadata-token: ",
            session,
            b"\r\nConnection: close\r\n\r\n",
        ]);
        request(&parts)
    };
    let (roles, body) = exchange(at, &get(&[ROLES]), deadline)?;
    let listing = &roles[body];
    let role = listing
        .split(|&b| b == b'\n')
        .next()
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|role| {
            !role.is_empty()
                && role
                    .iter()
                    .all(|&b| b.is_ascii_alphanumeric() || b"+=,.@_-".contains(&b))
        })
        .ok_or(failure(MALFORMED, 0, 0, 0))?;
    let (reply, body) = exchange(at, &get(&[ROLES, role]), deadline)?;
    let json = &reply[body];
    if member(json, b"Code")? != b"Success" {
        return Err(failure(MALFORMED, 0, 0, 0));
    }
    let secret = member(json, b"SecretAccessKey")?;
    if secret.is_empty() {
        return Err(failure(MALFORMED, 0, 0, 0));
    }
    let fetched = Fetched {
        key_id: exact(member(json, b"AccessKeyId")?),
        token: exact(member(json, b"Token")?),
        expires_ns: expiry(member(json, b"Expiration")?)?,
        secret: Some(Arc::new(exact(secret))),
    };
    Ok(fetched)
}

/// A copy in storage of exactly its length, so it never reallocates.
fn exact(bytes: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(bytes.len()));
    out.extend_from_slice(bytes);
    out
}

/// The string value of a JSON object member, which must hold no escape.
fn member<'a>(json: &'a [u8], name: &[u8]) -> Result<&'a [u8], Outcome> {
    let malformed = failure(MALFORMED, 0, 0, 0);
    let mut at = 0;
    while let Some(found) = find(&json[at..], name) {
        let start = at + found;
        at = start + name.len();
        if start == 0 || json[start - 1] != b'"' || json.get(at) != Some(&b'"') {
            continue;
        }
        let rest = json[at + 1..].trim_ascii_start();
        let Some(rest) = rest.strip_prefix(b":") else {
            continue;
        };
        let rest = rest
            .trim_ascii_start()
            .strip_prefix(b"\"")
            .ok_or(malformed)?;
        let end = rest.iter().position(|&b| b == b'"').ok_or(malformed)?;
        let value = &rest[..end];
        if value.contains(&b'\\') {
            return Err(malformed);
        }
        return Ok(value);
    }
    Err(malformed)
}

/// `YYYY-MM-DDTHH:MM:SSZ` as nanoseconds since the Unix epoch.
fn expiry(text: &[u8]) -> Result<u64, Outcome> {
    let malformed = failure(MALFORMED, 0, 0, 0);
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    if text.len() != shape.len()
        || !text.iter().zip(shape).all(|(&b, &s)| {
            if s == b'd' {
                b.is_ascii_digit()
            } else {
                b == s
            }
        })
    {
        return Err(malformed);
    }
    let n = |r: std::ops::Range<usize>| {
        text[r]
            .iter()
            .fold(0i64, |v, &d| v * 10 + i64::from(d - b'0'))
    };
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let (hh, mm, ss) = (n(11..13), n(14..16), n(17..19));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 59 {
        return Err(malformed);
    }
    // Days from the civil date (H. Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(seconds)
        .ok()
        .and_then(|s| s.checked_mul(1_000_000_000))
        .ok_or(malformed)
}

fn spare(buffer: &Buffer) -> usize {
    buffer.capacity.saturating_sub(buffer.len)
}

/// # Safety
/// The buffer is the caller's live String carrier with `bytes.len()` spare.
unsafe fn append(buffer: &mut Buffer, bytes: &[u8]) {
    // SAFETY: checked spare capacity in the caller's own storage.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.data.add(buffer.len), bytes.len());
    }
    buffer.len += bytes.len();
}

/// # Safety
/// Live run, a Cred grant and optional Clock grant, and the source's exclusively
/// borrowed String carriers.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_cred_fetch(
    context: *mut Context,
    cred: u64,
    key_id: *mut Buffer,
    token: *mut Buffer,
    until_kind: u8,
    clock: u64,
    deadline_ns: u64,
    site: u64,
) -> Outcome {
    // SAFETY: the source pins the request and both carriers through delivery.
    unsafe {
        // A describe run's inert designation has no grant behind it; its
        // provider denies the request before any source is used.
        let grant = (hirsel_core_capability_handle(context, cred) as *const Granted).as_ref();
        let (key_id, token) = (&mut *key_id, &mut *token);
        let mut fetch = CredRequest {
            network: NetworkRequest {
                send_tail: ptr::null(),
                address: Address::default(),
                sockets: ptr::null_mut(),
                until: deadline(context, until_kind, clock, deadline_ns),
                backlog: 0,
                connecting: None,
                denied: false,
                direction: 0,
                os: &OPERATIONS,
                tls: ptr::null_mut(),
                tls_send_pending: false,
                max_receive: 0,
            },
            source: grant.map_or(CredSource::Imds(SocketAddrV4::new(0.into(), 0)), |g| {
                g.source
            }),
            spares: [spare(key_id) as u64, spare(token) as u64],
            job: None,
            fetched: None,
        };
        let mut request = Request {
            operation: CRED_FETCH,
            mode: 0,
            handle: grant.map_or(0, |g| g.handle),
            path: (&raw mut fetch).cast(),
            path_len: size_of::<CredRequest>(),
            buffer: Buffer {
                data: ptr::null_mut(),
                len: 0,
                capacity: 0,
            },
            site,
        };
        let provider = hirsel_core_capability_provider(context, cred);
        let outcome = crate::provider::hirsel_core_host(context, provider, &mut request);
        if outcome.status != 0 || outcome.error != IoError::OK {
            return outcome;
        }
        let (Some(fetched), Some(grant)) = (fetch.fetched.take(), grant) else {
            crate::process::stop(b"CredFetch")
        };
        if spare(key_id) < fetched.key_id.len() || spare(token) < fetched.token.len() {
            return failure(
                ROOM,
                fetched.key_id.len() as u64,
                0,
                fetched.token.len() as u64,
            );
        }
        // A tape designates the key by its delivery position, so concurrent
        // fetches keep their origins on replay; without one it is 0.
        let origin = Origin::Fetched(outcome.value);
        let designated = crate::secret::designate(
            context,
            Secret {
                origin,
                policy: grant.policy.clone(),
                bytes: fetched.secret,
                seal: None,
            },
        );
        if designated.error != IoError::OK {
            return Outcome {
                error: IoError {
                    tag: REFUSED,
                    code: 0,
                },
                ..designated
            };
        }
        append(key_id, &fetched.key_id);
        append(token, &fetched.token);
        Outcome {
            value: designated.value,
            written: fetched.expires_ns,
            ..Outcome::default()
        }
    }
}
