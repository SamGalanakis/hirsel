//! mc-units-proto (throwaway prototype, never for main): K deterministic units
//! in one process, each a one-worker run on its own pinned OS thread, and
//! bounded crossings between them.
//!
//! Launch: `HIRSEL_UNITS=K` runs main K times, one thread per unit, with two
//! extra trailing arguments: the unit id and K. `HIRSEL_UNIT_BASE=b` numbers
//! the units b..b+K (a lone replayed unit keeps its recorded id).
//! `HIRSEL_UNIT_CPUS=c0,c1,..` pins unit i to ci.
//!
//! Crossing: a program reaches it through Net with the reserved IPv4 block
//! 127.77.0.0/16. `listen(127.77.U.0:P)` makes unit U's crossing listener P;
//! `connect(127.77.U.0:P)` from another unit makes a port to it. A port is
//! two bounded byte rings carrying whole messages (one `send_pair` is one
//! message). A send is admitted only if the whole message fits under the
//! ring bound and the receiver's byte charge cap; otherwise it waits
//! (backpressure). The bytes are copied into the ring, published under the
//! ring lock, and the receiver's eventfd is written only if its task armed a
//! wait. Each endpoint's eventfd sits in the unit's own epoll registry, so
//! the receiving unit orders the delivery among its own events.
//!
//! Recording: `HIRSEL_XTAPE_DIR=d` makes unit u write `d/unit-u.xtape`:
//! every accepted port and every delivered message with its sender unit,
//! per-port sequence number, unit-local delivery number and bytes.
//! Replay: `HIRSEL_XREPLAY=file` (with K=1) answers accepts and receives from
//! that file in the recorded delivery order and discards sends.
use crate::net::Address;
use crate::provider::Buffer;
use hirsel_core_net::{Crossing, Sockets};
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    fs::File,
    io::{BufWriter, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const MAGIC: u32 = 0x7f4d_0000;
/// Ring bound per direction, in bytes.
fn ring_cap() -> usize {
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("HIRSEL_XRING")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256 * 1024)
    })
}
/// Receiver byte charge cap: total bytes queued toward one unit.
const UNIT_CHARGE_CAP: u64 = 64 << 20;
const MAX_UNITS: usize = 256;
static CHARGED: [AtomicU64; MAX_UNITS] = [const { AtomicU64::new(0) }; MAX_UNITS];
pub(crate) static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4]; // msgs, bytes, wakes, full-waits

thread_local! {
    static UNIT: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn current() -> u32 {
    UNIT.get()
}

pub(crate) fn magic(a: &Address) -> Option<u32> {
    (a.family == 0 && (a.lo as u32) & 0xffff_0000 == MAGIC).then(|| ((a.lo as u32) >> 8) & 0xff)
}

fn eventfd() -> OwnedFd {
    // SAFETY: a new nonblocking, close-on-exec eventfd.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        crate::process::stop(b"CrossingEventfd");
    }
    unsafe { OwnedFd::from_raw_fd(fd) }
}
fn dup(fd: &OwnedFd) -> OwnedFd {
    fd.try_clone().unwrap_or_else(|_| crate::process::stop(b"CrossingDup"))
}
fn notify(fd: &OwnedFd) {
    STATS[2].fetch_add(1, Ordering::Relaxed);
    let one = 1u64;
    // SAFETY: eight-byte write to a live eventfd.
    unsafe { libc::write(fd.as_raw_fd(), (&one as *const u64).cast(), 8) };
}

// ---------------------------------------------------------------- recording

struct Log(Mutex<BufWriter<File>>);
static LOGS: Mutex<Vec<(u32, Arc<Log>)>> = Mutex::new(Vec::new());

fn log_for(unit: u32) -> Option<Arc<Log>> {
    let dir = std::env::var_os("HIRSEL_XTAPE_DIR")?;
    let mut logs = LOGS.lock().unwrap();
    if let Some((_, l)) = logs.iter().find(|(u, _)| *u == unit) {
        return Some(l.clone());
    }
    let path = std::path::Path::new(&dir).join(format!("unit-{unit}.xtape"));
    let file = File::create(path).unwrap_or_else(|_| crate::process::stop(b"CrossingTape"));
    let log = Arc::new(Log(Mutex::new(BufWriter::with_capacity(1 << 20, file))));
    logs.push((unit, log.clone()));
    Some(log)
}
const REC_ACCEPT: u8 = 1;
const REC_MSG: u8 = 2;

pub(crate) fn finish_unit(unit: u32) {
    let logs = LOGS.lock().unwrap();
    for (u, l) in logs.iter() {
        if *u == unit {
            let _ = l.0.lock().unwrap().flush();
        }
    }
}

// ------------------------------------------------------------------- replay

struct ReplayMsg {
    delivery: u64,
    bytes: Vec<u8>,
}
struct Replay {
    accepts: Mutex<VecDeque<(u32, u32, u16)>>, // local index, peer unit, listener port
    queues: Mutex<HashMap<u32, VecDeque<ReplayMsg>>>,
    next_delivery: AtomicU64,
    /// Endpoint eventfds by local index, to wake a waiter whose turn came.
    waiters: Mutex<HashMap<u32, OwnedFd>>,
    sent: AtomicU64,
}
fn replay() -> Option<&'static Replay> {
    static R: OnceLock<Option<Replay>> = OnceLock::new();
    R.get_or_init(|| {
        let path = std::env::var_os("HIRSEL_XREPLAY")?;
        let mut data = Vec::new();
        File::open(path)
            .and_then(|mut f| f.read_to_end(&mut data))
            .unwrap_or_else(|_| crate::process::stop(b"CrossingReplayOpen"));
        let mut accepts = VecDeque::new();
        let mut queues: HashMap<u32, VecDeque<ReplayMsg>> = HashMap::new();
        let mut at = 0;
        let u32_at = |d: &[u8], i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap());
        let u64_at = |d: &[u8], i: usize| u64::from_le_bytes(d[i..i + 8].try_into().unwrap());
        while at < data.len() {
            match data[at] {
                REC_ACCEPT => {
                    let local = u32_at(&data, at + 1);
                    let peer = u32_at(&data, at + 5);
                    let port = u16::from_le_bytes([data[at + 9], data[at + 10]]);
                    accepts.push_back((local, peer, port));
                    at += 11;
                }
                REC_MSG => {
                    let local = u32_at(&data, at + 1);
                    let _sender = u32_at(&data, at + 5);
                    let _seq = u64_at(&data, at + 9);
                    let delivery = u64_at(&data, at + 17);
                    let len = u32_at(&data, at + 25) as usize;
                    let bytes = data[at + 29..at + 29 + len].to_vec();
                    queues.entry(local).or_default().push_back(ReplayMsg { delivery, bytes });
                    at += 29 + len;
                }
                _ => crate::process::stop(b"CrossingReplayDamaged"),
            }
        }
        Some(Replay {
            accepts: Mutex::new(accepts),
            queues: Mutex::new(queues),
            next_delivery: AtomicU64::new(0),
            waiters: Mutex::new(HashMap::new()),
            sent: AtomicU64::new(0),
        })
    })
    .as_ref()
}

// --------------------------------------------------------------------- hub

struct Listener {
    queue: Mutex<VecDeque<Arc<Link>>>,
    notify: OwnedFd,
    port: u16,
}
impl Crossing for Listener {
    fn shutdown_write(&self) {}
    fn close(&self) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

static LISTENERS: Mutex<Vec<((u32, u16), Arc<Listener>)>> = Mutex::new(Vec::new());

struct Msg {
    seq: u64,
    len: u32,
    left: u32,
    recorded: bool,
}
#[derive(Default)]
struct Ring {
    bytes: VecDeque<u8>,
    msgs: VecDeque<Msg>,
    rx_waiting: bool,
    tx_waiting: bool,
    closed: bool,
    seq: u64,
}
struct Link {
    /// dirs[s] carries side s to side 1-s.
    dirs: [Mutex<Ring>; 2],
    /// notify[s] wakes side s's endpoint.
    notify: [OwnedFd; 2],
    units: [u32; 2],
    /// The accepting side's eventfd, taken by its accept.
    pending: Mutex<Option<OwnedFd>>,
}

struct Endpoint {
    link: Option<Arc<Link>>,
    side: usize,
    local: u32,
    unit: u32,
    log: Option<Arc<Log>>,
    deliveries: Arc<AtomicU64>,
}
impl Crossing for Endpoint {
    fn shutdown_write(&self) {
        if let Some(link) = &self.link {
            let mut ring = link.dirs[self.side].lock().unwrap();
            ring.closed = true;
            let wake = std::mem::take(&mut ring.rx_waiting);
            drop(ring);
            if wake {
                notify(&link.notify[1 - self.side]);
            }
        }
    }
    fn close(&self) {
        self.shutdown_write();
        if let Some(link) = &self.link {
            let mut ring = link.dirs[1 - self.side].lock().unwrap();
            ring.closed = true;
            let wake = std::mem::take(&mut ring.tx_waiting);
            drop(ring);
            if wake {
                notify(&link.notify[1 - self.side]);
            }
        }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Per-registry state: the next local endpoint index and delivery counter.
struct UnitState {
    next_local: u32,
    deliveries: Arc<AtomicU64>,
}
thread_local! {
    static STATE: std::cell::RefCell<Option<UnitState>> = const { std::cell::RefCell::new(None) };
}
fn next_local() -> (u32, Arc<AtomicU64>) {
    STATE.with_borrow_mut(|s| {
        let s = s.get_or_insert_with(|| UnitState {
            next_local: 0,
            deliveries: Arc::new(AtomicU64::new(0)),
        });
        let l = s.next_local;
        s.next_local += 1;
        (l, s.deliveries.clone())
    })
}

fn ok(value: u64) -> crate::provider::Outcome {
    crate::provider::Outcome {
        value,
        ..Default::default()
    }
}

/// LISTEN on a reserved address. Returns the registry key.
fn listen(sockets: &mut Sockets, a: Address, unit: u32) -> crate::provider::Outcome {
    sockets.unit = Some(unit);
    let fd = eventfd();
    let listener = Arc::new(Listener {
        queue: Mutex::new(VecDeque::new()),
        notify: dup(&fd),
        port: a.port,
    });
    let key = sockets
        .insert(fd)
        .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
    if replay().is_none() {
        LISTENERS
            .lock()
            .unwrap()
            .push(((unit, a.port), listener.clone()));
    }
    sockets.attach_crossing(key, listener);
    ok(key)
}

fn connect(sockets: &mut Sockets, a: Address, unit: u32) -> crate::provider::Outcome {
    let me = sockets.unit.unwrap_or_else(current);
    let fd = eventfd();
    let (local, deliveries) = next_local();
    let link = if let Some(r) = replay() {
        r.waiters.lock().unwrap().insert(local, dup(&fd));
        None
    } else {
        let listener = LISTENERS
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| *k == (unit, a.port))
            .map(|(_, l)| l.clone());
        let Some(listener) = listener else {
            return crate::os_net::error(libc::ECONNREFUSED);
        };
        let peer = eventfd();
        let link = Arc::new(Link {
            dirs: [Mutex::new(Ring::default()), Mutex::new(Ring::default())],
            notify: [dup(&fd), dup(&peer)],
            units: [me, unit],
            pending: Mutex::new(Some(peer)),
        });
        listener.queue.lock().unwrap().push_back(link.clone());
        notify(&listener.notify);
        Some(link)
    };
    let key = sockets
        .insert(fd)
        .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
    let log = log_for(me);
    sockets.attach_crossing(
        key,
        Arc::new(Endpoint {
            link,
            side: 0,
            local,
            unit: me,
            log,
            deliveries,
        }),
    );
    ok(key)
}

/// ACCEPT on a crossing listener: None while no port is queued.
fn accept(
    sockets: &mut Sockets,
    listener: &Listener,
    peer_address: &mut Address,
) -> Option<crate::provider::Outcome> {
    let me = sockets.unit.unwrap_or_else(current);
    if let Some(r) = replay() {
        let mut accepts = r.accepts.lock().unwrap();
        let (local, peer, _port) = *accepts.front()?;
        let _ = listener.port;
        accepts.pop_front();
        drop(accepts);
        let (l, deliveries) = next_local();
        debug_assert_eq!(l, local);
        let fd = eventfd();
        r.waiters.lock().unwrap().insert(l, dup(&fd));
        let key = sockets.insert(fd).unwrap();
        sockets.attach_crossing(
            key,
            Arc::new(Endpoint {
                link: None,
                side: 1,
                local: l,
                unit: me,
                log: None,
                deliveries,
            }),
        );
        *peer_address = Address {
            family: 0,
            hi: 0,
            lo: u64::from(MAGIC | (peer << 8)),
            port: 0,
        };
        return Some(ok(key));
    }
    let link = listener.queue.lock().unwrap().pop_front()?;
    let fd = link.pending.lock().unwrap().take().unwrap();
    let (local, deliveries) = next_local();
    let log = log_for(me);
    if let Some(log) = &log {
        let mut w = log.0.lock().unwrap();
        let mut rec = [0u8; 11];
        rec[0] = REC_ACCEPT;
        rec[1..5].copy_from_slice(&local.to_le_bytes());
        rec[5..9].copy_from_slice(&link.units[0].to_le_bytes());
        rec[9..11].copy_from_slice(&listener.port.to_le_bytes());
        let _ = w.write_all(&rec);
    }
    *peer_address = Address {
        family: 0,
        hi: 0,
        lo: u64::from(MAGIC | (link.units[0] << 8)),
        port: 0,
    };
    let key = sockets.insert(fd).unwrap();
    sockets.attach_crossing(
        key,
        Arc::new(Endpoint {
            link: Some(link),
            side: 1,
            local,
            unit: me,
            log,
            deliveries,
        }),
    );
    Some(ok(key))
}

/// SEND_PAIR on a crossing endpoint: one whole message, or None to wait.
fn send(e: &Endpoint, prefix: &[u8], tail: &[u8]) -> Option<crate::provider::Outcome> {
    let len = prefix.len() + tail.len();
    let Some(link) = &e.link else {
        replay().unwrap().sent.fetch_add(len as u64, Ordering::Relaxed);
        return Some(ok(len as u64));
    };
    if len == 0 {
        return Some(ok(0));
    }
    if len > ring_cap() {
        return Some(crate::os_net::error(libc::EMSGSIZE));
    }
    let receiver = link.units[1 - e.side] as usize % MAX_UNITS;
    let mut ring = link.dirs[e.side].lock().unwrap();
    if ring.closed {
        return Some(crate::os_net::signal(5));
    }
    // Admission: the ring bound and the receiver's charge cap.
    if ring.bytes.len() + len > ring_cap()
        || CHARGED[receiver].load(Ordering::Relaxed) + len as u64 > UNIT_CHARGE_CAP
    {
        ring.tx_waiting = true;
        STATS[3].fetch_add(1, Ordering::Relaxed);
        return None;
    }
    CHARGED[receiver].fetch_add(len as u64, Ordering::Relaxed);
    ring.bytes.extend(prefix);
    ring.bytes.extend(tail);
    let seq = ring.seq;
    ring.seq += 1;
    ring.msgs.push_back(Msg {
        seq,
        len: len as u32,
        left: len as u32,
        recorded: false,
    });
    let wake = std::mem::take(&mut ring.rx_waiting);
    drop(ring);
    STATS[0].fetch_add(1, Ordering::Relaxed);
    STATS[1].fetch_add(len as u64, Ordering::Relaxed);
    if wake {
        notify(&link.notify[1 - e.side]);
    }
    Some(ok(len as u64))
}

/// RECV_BOUNDED: at most the rest of the front message, 0 at end, None to wait.
/// A delivery consumed by a scheduler retry, stamped when its task resumes.
enum Unobserved {
    Record { log: Arc<Log>, deliveries: Arc<AtomicU64>, head: [u8; 29], bytes: Vec<u8> },
    Replay { next: u64 },
}
thread_local! {
    static UNOBSERVED: std::cell::RefCell<Vec<(usize, Unobserved)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn stamp(log: &Log, deliveries: &AtomicU64, mut head: [u8; 29], bytes: &[u8]) {
    let delivery = deliveries.fetch_add(1, Ordering::Relaxed);
    head[17..25].copy_from_slice(&delivery.to_le_bytes());
    let mut w = log.0.lock().unwrap();
    let _ = w.write_all(&head);
    let _ = w.write_all(bytes);
}

fn replay_advance(next: u64) {
    let r = replay().unwrap();
    r.next_delivery.store(next + 1, Ordering::Relaxed);
    let queues = r.queues.lock().unwrap();
    let waiters = r.waiters.lock().unwrap();
    for (local, q) in queues.iter() {
        if q.front().is_some_and(|m| m.delivery == next + 1)
            && let Some(fd) = waiters.get(local)
        {
            notify(fd);
        }
    }
}

/// The task that issued `request` resumed: it observes its delivery now.
pub(crate) fn observed(request: usize) {
    let found = UNOBSERVED.with_borrow_mut(|u| {
        let i = u.iter().position(|(r, _)| *r == request)?;
        Some(u.swap_remove(i).1)
    });
    match found {
        Some(Unobserved::Record { log, deliveries, head, bytes }) => stamp(&log, &deliveries, head, &bytes),
        Some(Unobserved::Replay { next }) => replay_advance(next),
        None => {}
    }
}

fn recv(e: &Endpoint, b: &mut Buffer, request: usize, in_task: bool) -> Option<crate::provider::Outcome> {
    let spare = b.capacity.saturating_sub(b.len);
    if spare == 0 {
        return Some(ok(0));
    }
    let Some(link) = &e.link else {
        let r = replay().unwrap();
        let mut queues = r.queues.lock().unwrap();
        let Some(q) = queues.get_mut(&e.local) else {
            return Some(ok(0));
        };
        let Some(front) = q.front_mut() else {
            return Some(ok(0));
        };
        let next = r.next_delivery.load(Ordering::Relaxed);
        if front.delivery != u64::MAX && front.delivery > next {
            return None; // another endpoint's delivery comes first
        }
        if front.delivery != u64::MAX && front.delivery < next {
            return None; // an earlier delivery is consumed but not yet observed
        }
        let n = spare.min(front.bytes.len());
        // SAFETY: the buffer has `spare` writable bytes after len.
        unsafe { std::ptr::copy_nonoverlapping(front.bytes.as_ptr(), b.data.add(b.len), n) };
        b.len += n;
        front.bytes.drain(..n);
        let advanced = front.delivery == next;
        if advanced {
            // Started: later chunks of this message need no turn.
            front.delivery = u64::MAX;
        }
        if front.bytes.is_empty() {
            q.pop_front();
        }
        drop(queues);
        if advanced {
            // The turn passes when the task observes this delivery.
            if in_task {
                replay_advance(next);
            } else {
                UNOBSERVED.with_borrow_mut(|u| u.push((request, Unobserved::Replay { next })));
            }
        }
        return Some(ok(n as u64));
    };
    let from = 1 - e.side;
    let mut ring = link.dirs[from].lock().unwrap();
    let Some(front) = ring.msgs.front_mut() else {
        if ring.closed {
            return Some(ok(0));
        }
        ring.rx_waiting = true;
        return None;
    };
    let n = spare.min(front.left as usize);
    let first = !front.recorded;
    front.recorded = true;
    let (seq, len) = (front.seq, front.len as usize);
    front.left -= n as u32;
    let done = front.left == 0;
    if first && let Some(log) = &e.log {
        let mut head = [0u8; 29];
        head[0] = REC_MSG;
        head[1..5].copy_from_slice(&e.local.to_le_bytes());
        head[5..9].copy_from_slice(&link.units[from].to_le_bytes());
        head[9..17].copy_from_slice(&seq.to_le_bytes());
        head[25..29].copy_from_slice(&(len as u32).to_le_bytes());
        let (a, bb) = ring.bytes.as_slices();
        let take_a = a.len().min(len);
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(&a[..take_a]);
        bytes.extend_from_slice(&bb[..len - take_a]);
        if in_task {
            stamp(log, &e.deliveries, head, &bytes);
        } else {
            let pending = Unobserved::Record { log: log.clone(), deliveries: e.deliveries.clone(), head, bytes };
            UNOBSERVED.with_borrow_mut(|u| u.push((request, pending)));
        }
    } else if first {
        e.deliveries.fetch_add(1, Ordering::Relaxed);
    }
    let mut copied = 0;
    {
        let (a, bb) = ring.bytes.as_slices();
        let from_a = a.len().min(n);
        // SAFETY: the buffer has `spare` >= n writable bytes after len.
        unsafe {
            std::ptr::copy_nonoverlapping(a.as_ptr(), b.data.add(b.len), from_a);
            std::ptr::copy_nonoverlapping(bb.as_ptr(), b.data.add(b.len + from_a), n - from_a);
        }
        copied += n;
    }
    ring.bytes.drain(..copied);
    if done {
        ring.msgs.pop_front();
    }
    let wake = std::mem::take(&mut ring.tx_waiting);
    drop(ring);
    let receiver = e.unit as usize % MAX_UNITS;
    CHARGED[receiver].fetch_sub(n as u64, Ordering::Relaxed);
    b.len += n;
    if wake {
        notify(&link.notify[from]);
    }
    Some(ok(n as u64))
}

/// Dispatch a socket attempt that names a crossing. None: not a crossing.
/// Some(None): wait. Some(Some(outcome)): done.
/// # Safety
/// The request and its network request are the live, pinned attempt.
pub(crate) unsafe fn attempt(
    request: &mut crate::provider::Request,
    n: &mut crate::net::NetworkRequest,
    key: u64,
    in_task: bool,
) -> Option<Option<crate::provider::Outcome>> {
    use crate::provider::{ACCEPT, CONNECT, LISTEN, RECV_BOUNDED, SEND_PAIR};
    let request_id = request as *mut crate::provider::Request as usize;
    unsafe {
        let sockets = &mut *n.sockets;
        match request.operation {
            LISTEN => {
                let unit = magic(&n.address)?;
                Some(Some(listen(sockets, n.address, unit)))
            }
            CONNECT => {
                let unit = magic(&n.address)?;
                Some(Some(connect(sockets, n.address, unit)))
            }
            ACCEPT => {
                let c = sockets.crossing(key)?.clone();
                let listener = c.as_any().downcast_ref::<Listener>()?;
                let mut peer = n.address;
                let out = accept(sockets, listener, &mut peer);
                n.address = peer;
                Some(out)
            }
            RECV_BOUNDED | SEND_PAIR => {
                let c = sockets.crossing(key)?.clone();
                let e = c.as_any().downcast_ref::<Endpoint>()?;
                if request.operation == RECV_BOUNDED {
                    Some(recv(e, &mut request.buffer, request_id, in_task))
                } else {
                    let b = &request.buffer;
                    let t = &*n.send_tail;
                    let prefix = std::slice::from_raw_parts(b.data, b.len);
                    let tail = if t.len == 0 {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(t.data, t.len)
                    };
                    Some(send(e, prefix, tail))
                }
            }
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ launch

pub(crate) fn spin() -> bool {
    static SPIN: OnceLock<bool> = OnceLock::new();
    *SPIN.get_or_init(|| std::env::var_os("HIRSEL_XSPIN").is_some())
}

pub(crate) fn count() -> Option<usize> {
    std::env::var("HIRSEL_UNITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&k: &usize| k >= 1)
}

/// The process's affinity before units pin themselves, for helper threads.
pub(crate) static ORIGINAL_MASK: OnceLock<libc::cpu_set_t> = OnceLock::new();

pub(crate) fn restore_mask() {
    if let Some(mask) = ORIGINAL_MASK.get() {
        // SAFETY: a valid set for the calling thread.
        unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), mask) };
    }
}

struct SendPtr<T>(T);
unsafe impl<T> Send for SendPtr<T> {}

/// Run main once per unit, each on its own thread; the worst status wins.
pub(crate) fn launch(k: usize, input: &crate::entry::OsConfig, run: impl Fn(&crate::entry::OsConfig) -> u8 + Sync) -> u8 {
    // SAFETY: zeroed is the empty set; the call fills it for this thread.
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut mask) };
    let _ = ORIGINAL_MASK.set(mask);
    let base: u32 = std::env::var("HIRSEL_UNIT_BASE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let cpus: Vec<usize> = std::env::var("HIRSEL_UNIT_CPUS")
        .ok()
        .map(|v| v.split(',').filter_map(|c| c.trim().parse().ok()).collect())
        .unwrap_or_default();
    let total = k as u32;
    let statuses: Vec<u8> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..k)
            .map(|i| {
                let unit = base + i as u32;
                let cpu = cpus.get(i).copied();
                let input = SendPtr(input as *const crate::entry::OsConfig);
                let run = &run;
                std::thread::Builder::new()
                    .name(format!("hirsel-unit-{unit}"))
                    .stack_size(16 << 20)
                    .spawn_scoped(scope, move || {
                        let input = input;
                        UNIT.set(unit);
                        if let Some(cpu) = cpu {
                            // SAFETY: a one-CPU set for the calling thread.
                            unsafe {
                                let mut set: libc::cpu_set_t = std::mem::zeroed();
                                libc::CPU_SET(cpu, &mut set);
                                libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set);
                            }
                        }
                        // SAFETY: the launcher's configuration outlives the scope.
                        let input = unsafe { &*input.0 };
                        let extra = [unit.to_string(), total.to_string()];
                        let mut views: Vec<crate::process::View> = (0..input.args_len)
                            .map(|j| unsafe { *input.args.add(j) })
                            .collect();
                        for e in &extra {
                            views.push(crate::process::View {
                                data: e.as_ptr(),
                                len: e.len(),
                            });
                        }
                        let config = crate::entry::OsConfig {
                            args: views.as_ptr(),
                            args_len: views.len(),
                            ..*input
                        };
                        let status = run(&config);
                        finish_unit(unit);
                        status
                    })
                    .unwrap()
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or(101)).collect()
    });
    if std::env::var_os("HIRSEL_XSTATS").is_some() {
        let r = replay().map_or(0, |r| r.sent.load(Ordering::Relaxed));
        eprintln!(
            "crossing-stats msgs={} bytes={} wakes={} full_waits={} replay_sent={}",
            STATS[0].load(Ordering::Relaxed),
            STATS[1].load(Ordering::Relaxed),
            STATS[2].load(Ordering::Relaxed),
            STATS[3].load(Ordering::Relaxed),
            r
        );
    }
    statuses.into_iter().max().unwrap_or(0)
}
