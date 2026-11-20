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
#[path = "unit_memory.rs"]
mod memory;
pub(crate) use memory::Memory;
use memory::UnitStack;
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
const MAX_UNITS: usize = 65536;
static CHARGED: [AtomicU64; MAX_UNITS] = [const { AtomicU64::new(0) }; MAX_UNITS];
pub(crate) static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4]; // msgs, bytes, wakes, full-waits

thread_local! {
    static UNIT: Cell<u32> = const { Cell::new(0) };
}

/// The running unit's id: the untied unit on this worker, else this thread's.
#[inline(never)]
pub(crate) fn current() -> u32 {
    if untied() {
        let u = cur();
        // SAFETY: a running unit's record is never freed.
        return if u.is_null() { NONE } else { unsafe { (*u).id } };
    }
    UNIT.get()
}

/// Untied: a 16-bit unit id is the low half of 127.77.0.0/16.
pub(crate) fn magic(a: &Address) -> Option<u32> {
    if a.family != 0 || (a.lo as u32) & 0xffff_0000 != MAGIC {
        return None;
    }
    Some(if untied() { (a.lo as u32) & 0xffff } else { ((a.lo as u32) >> 8) & 0xff })
}
fn encode(unit: u32) -> u64 {
    u64::from(if untied() { MAGIC | unit } else { MAGIC | (unit << 8) })
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

enum Pend {
    Link(Arc<Link>),
    /// Untied front door: an accepted TCP connection handed to its session unit.
    Fd(OwnedFd),
}
struct Listener {
    queue: Mutex<VecDeque<Pend>>,
    notify: OwnedFd,
    port: u16,
    /// Untied: the listening unit and its key's reactor identity (0 before listen).
    target: Mutex<(u32, u64)>,
}
impl Crossing for Listener {
    fn shutdown_write(&self) {}
    fn close(&self) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

static LISTENERS: Mutex<Vec<((u32, u16), Arc<Listener>)>> = Mutex::new(Vec::new());
/// Untied listeners by (unit, port), made by whichever side comes first.
static ULISTENERS: Mutex<Option<HashMap<(u32, u16), Arc<Listener>>>> = Mutex::new(None);
fn listener_for(unit: u32, port: u16) -> Arc<Listener> {
    let mut map = ULISTENERS.lock().unwrap();
    map.get_or_insert_with(HashMap::new)
        .entry((unit, port))
        .or_insert_with(|| {
            Arc::new(Listener {
                queue: Mutex::new(VecDeque::new()),
                notify: eventfd(),
                port,
                target: Mutex::new((NONE, 0)),
            })
        })
        .clone()
}
/// A key's reactor identity, as `Sockets::dispatch` takes it.
fn ident(key: u64) -> u64 {
    ((key >> 2) & 0xffff_ffff) | ((key >> 34) << 32)
}
fn wake_listener(l: &Listener) {
    if untied() {
        let (unit, id) = *l.target.lock().unwrap();
        xnotify(unit, id);
    } else {
        notify(&l.notify);
    }
}
/// Wake side `s` of a link.
fn wake_side(link: &Link, s: usize) {
    if untied() {
        xnotify(link.units[s], link.idents[s].load(Ordering::Acquire));
    } else {
        notify(&link.notify[s]);
    }
}

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
    /// Untied: each side's key identity in its unit's reactor (0 before attach).
    idents: [AtomicU64; 2],
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
                wake_side(link, 1 - self.side);
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
                wake_side(link, 1 - self.side);
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
    if untied() {
        // SAFETY: only the unit's own (single) running worker touches these.
        let u = unsafe { &*cur() };
        let l = u.next_local.get();
        u.next_local.set(l + 1);
        return (l, u.deliveries.clone());
    }
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
    if untied() {
        let listener = listener_for(unit, a.port);
        let key = sockets
            .insert(fd)
            .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
        *listener.target.lock().unwrap() = (unit, ident(key));
        sockets.attach_crossing(key, listener);
        return ok(key);
    }
    let listener = Arc::new(Listener {
        queue: Mutex::new(VecDeque::new()),
        notify: dup(&fd),
        port: a.port,
        target: Mutex::new((NONE, 0)),
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
    } else if untied() {
        // Unit 0xffff: make a child unit now; it serves port `a.port`.
        let unit = if unit == SPAWN { spawn_child(me) } else { unit };
        let listener = listener_for(unit, a.port);
        let peer = eventfd();
        let link = Arc::new(Link {
            dirs: [Mutex::new(Ring::default()), Mutex::new(Ring::default())],
            notify: [dup(&fd), dup(&peer)],
            units: [me, unit],
            idents: [AtomicU64::new(0), AtomicU64::new(0)],
            pending: Mutex::new(Some(peer)),
        });
        // Queued once this side's key exists, below.
        Some((listener, link))
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
            idents: [AtomicU64::new(0), AtomicU64::new(0)],
            pending: Mutex::new(Some(peer)),
        });
        listener.queue.lock().unwrap().push_back(Pend::Link(link.clone()));
        notify(&listener.notify);
        Some((listener, link))
    };
    let key = sockets
        .insert(fd)
        .unwrap_or_else(|| crate::process::stop(b"HostHandles"));
    let link = link.map(|(listener, link)| {
        link.idents[0].store(ident(key), Ordering::Release);
        if untied() {
            listener.queue.lock().unwrap().push_back(Pend::Link(link.clone()));
            wake_listener(&listener);
        }
        link
    });
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
            lo: encode(peer),
            port: 0,
        };
        return Some(ok(key));
    }
    let link = match listener.queue.lock().unwrap().pop_front()? {
        Pend::Link(link) => link,
        Pend::Fd(fd) => {
            // A front-door connection: an ordinary TCP key from here on.
            *peer_address = Address { family: 0, hi: 0, lo: 0x7f00_0001, port: 0 };
            return Some(ok(sockets.insert(fd).unwrap()));
        }
    };
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
        lo: encode(link.units[0]),
        port: 0,
    };
    let key = sockets.insert(fd).unwrap();
    link.idents[1].store(ident(key), Ordering::Release);
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
        wake_side(link, 1 - e.side);
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

fn push_unobserved(request: usize, pending: Unobserved) {
    if untied() {
        // SAFETY: the running unit's record; only its worker touches it.
        unsafe { (*cur()).unobserved.borrow_mut().push((request, pending)) };
    } else {
        UNOBSERVED.with_borrow_mut(|u| u.push((request, pending)));
    }
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
    let take = |u: &mut Vec<(usize, Unobserved)>| {
        let i = u.iter().position(|(r, _)| *r == request)?;
        Some(u.swap_remove(i).1)
    };
    let found = if untied() {
        // SAFETY: the running unit's record; only its worker touches it.
        unsafe { take(&mut (*cur()).unobserved.borrow_mut()) }
    } else {
        UNOBSERVED.with_borrow_mut(take)
    };
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
                push_unobserved(request, Unobserved::Replay { next });
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
            push_unobserved(request, pending);
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
        wake_side(link, from);
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
        // mc-units-untied (law 2's scripted input): with HIRSEL_XGATE a crossing
        // receive or accept never completes at issue; its arrival always enters
        // the unit's order through the event path (one self-notify), so where
        // an arrival lands does not depend on when the peer ran.
        if in_task
            && matches!(request.operation, RECV_BOUNDED | ACCEPT)
            && untied()
            && xgate()
            && sockets.crossing(key).is_some()
        {
            xnotify(current(), ident(key));
            return Some(None);
        }
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

/// The calling unit's id (0 outside units).
pub(crate) fn unit() -> u32 {
    current()
}

fn xgate() -> bool {
    static GATE: OnceLock<bool> = OnceLock::new();
    *GATE.get_or_init(|| std::env::var_os("HIRSEL_XGATE").is_some())
}

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
    if let Some(w) = untied_workers() {
        return launch_untied(k, w, input, &run);
    }
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

// ================================================================== untied
//
// mc-units-untied (throwaway prototype): `HIRSEL_UNTIED=W` with
// `HIRSEL_UNITS=K` runs the K initial units, and every unit made later, on W
// worker threads. A unit is one order of events: its whole run (entry chain,
// scheduler, tasks, reactor, budget) lives on its own stack and heap records,
// and a worker runs it until it has nothing runnable or its slice ends. Then
// the unit parks (switches back to the worker's host stack) or is requeued.
// At most one worker is inside a unit (checked on every entry). An idle
// worker steals a whole unit from another worker's queue.
//
// Wakes: a crossing send queues a synthetic reactor event on the receiver
// and makes it runnable (no eventfd, no syscall unless a worker must be
// woken). A sync helper's ready is routed to its issuing unit only. Socket
// readiness arrives through one pool poller that holds every unit's sockets
// (edge-triggered, tagged with the owning unit): an idle worker blocks in it,
// or a busy or searching worker polls it when no worker is blocked there.
// Each event joins the owning unit's arrival queue in the order it reached the
// unit, and the unit takes the queue at its next switch. Timers of parked
// units sit in one shared heap.
//
// Idle workers: a searcher watches the queued count for a few microseconds,
// then parks on its own handle. A wake hands one parked worker the searching
// role, so a burst of newly runnable units wakes one worker at a time.
use std::cell::{RefCell as URefCell, UnsafeCell};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize};

/// mc-units-untied: a thread-local Cell whose every access computes the slot
/// address inside a call that is never inlined, so no per-thread address
/// computed before a unit moves to another worker is used after it.
pub(crate) struct Moved<T: 'static>(pub(crate) &'static std::thread::LocalKey<Cell<T>>);
impl<T: Copy> Moved<T> {
    #[inline(never)]
    pub(crate) fn get(&'static self) -> T {
        self.0.get()
    }
}
impl<T> Moved<T> {
    #[inline(never)]
    pub(crate) fn set(&'static self, v: T) {
        self.0.set(v)
    }
    #[inline(never)]
    pub(crate) fn replace(&'static self, v: T) -> T {
        self.0.replace(v)
    }
}
impl<T: Default> Moved<T> {
    #[inline(never)]
    pub(crate) fn take(&'static self) -> T {
        self.0.take()
    }
}
macro_rules! moved_tls {
    ($vis:vis static $name:ident: $t:ty = $init:expr;) => {
        $vis static $name: $crate::units::Moved<$t> = {
            thread_local! { static INNER: std::cell::Cell<$t> = const { std::cell::Cell::new($init) }; }
            $crate::units::Moved(&INNER)
        };
    };
}
pub(crate) use moved_tls;

pub(crate) const NONE: u32 = u32::MAX;
/// Connecting to this unit makes a child unit.
const SPAWN: u32 = 0xffff;
/// The front door hands each accepted connection to its session unit here.
const HANDOFF_PORT: u16 = 7;
const ROLE_INITIAL: u32 = 0;
const ROLE_SESSION: u32 = 1;
const ROLE_CHILD: u32 = 2;
const WAKE_TAG: u64 = u64::MAX;
const FRONT_TAG: u64 = u64::MAX - 1;

static UNTIED_ON: AtomicBool = AtomicBool::new(false);
#[inline]
pub(crate) fn untied() -> bool {
    UNTIED_ON.load(Ordering::Relaxed)
}
fn untied_workers() -> Option<usize> {
    std::env::var("HIRSEL_UNTIED")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&w: &usize| w >= 1)
}
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Counters, printed at exit under HIRSEL_XSTATS.
pub(crate) static USTATS: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
const S_RESUMES: usize = 0;
const S_PARKS: usize = 1;
const S_YIELDS: usize = 2;
const S_STEALS: usize = 3;
const S_OS_WAKES: usize = 4;
const S_GPOLLS: usize = 5;
const S_GPOLL_BLOCKS: usize = 6;
const S_XNOTIFY: usize = 7;
const S_TIMER_FIRES: usize = 8;
const S_CREATED: usize = 9;
const S_CREATE_NS: usize = 10;
const S_READY_TO_PARK_NS: usize = 11;
const S_GPOLL_EVENTS: usize = 12;
const S_NOTIFIED_REQUEUE: usize = 13;
const S_LIFO: usize = 14;
fn bump(i: usize) {
    USTATS[i].fetch_add(1, Ordering::Relaxed);
}

const PARKED: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;
const NOTIFIED: u8 = 3;
const DONE: u8 = 4;

#[derive(Clone, Copy)]
enum After {
    Run,
    Wait(Option<u64>),
    Yield,
    Done,
}

pub(crate) struct Unit {
    id: u32,
    sp: UnsafeCell<*mut u8>,
    /// The running worker's host stack slot.
    host: UnsafeCell<*mut *mut u8>,
    stack: UnsafeCell<Option<UnitStack>>,
    state: AtomicU8,
    inside: AtomicBool,
    after: Cell<After>,
    next_local: Cell<u32>,
    deliveries: Arc<AtomicU64>,
    unobserved: URefCell<Vec<(usize, Unobserved)>>,
    children: Cell<u32>,
    /// Readiness for this unit's keys, as (reactor identity, epoll flags):
    /// crossing events and the shared poller's socket edges. The unit's
    /// scheduler takes them at its next turn, in this order.
    xready: Mutex<Vec<(u64, u32)>>,
    /// Set with the lock held whenever `xready` gains an entry, so an empty
    /// take costs one load.
    xpending: AtomicBool,
    /// The running unit's batch buffer, swapped with `xready` at a take.
    xspare: UnsafeCell<Vec<(u64, u32)>>,
    status: AtomicU8,
    stop_fd: Cell<i32>,
    slice_start: Cell<u64>,
    queued_ns: AtomicU64,
    created_ns: u64,
    first_park: AtomicBool,
    /// The unit's scheduler-less waiter (replaces the thread's ALONE).
    pub(crate) alone: std::sync::OnceLock<Arc<crate::tasks::Readies>>,
    pub(crate) alone_depth: Cell<usize>,
    _views: Vec<crate::process::View>,
    _extra: Vec<String>,
    config: crate::entry::OsConfig,
}
// SAFETY: the Cells are touched only by the one worker inside the unit;
// entry and exit are ordered by the state word and the queue locks.
unsafe impl Sync for Unit {}
unsafe impl Send for Unit {}

static TABLE: [AtomicPtr<Unit>; 65536] = [const { AtomicPtr::new(std::ptr::null_mut()) }; 65536];
fn unit_ref(id: u32) -> Option<&'static Unit> {
    let p = TABLE.get(id as usize)?.load(Ordering::Acquire);
    // SAFETY: unit records are leaked; a published pointer stays valid.
    unsafe { p.as_ref() }
}

thread_local! {
    static CUR: Cell<*mut Unit> = const { Cell::new(std::ptr::null_mut()) };
    static WORKER_IX: Cell<usize> = const { Cell::new(usize::MAX) };
}
/// The unit running on this worker. Never inlined: a unit resumes on any
/// worker, so no caller may keep this thread's slot address across a park.
#[inline(never)]
pub(crate) fn cur() -> *mut Unit {
    CUR.get()
}
#[inline(never)]
fn worker_ix() -> usize {
    WORKER_IX.get()
}
/// A per-unit identity for code that asks "is this my own domain" by thread.
#[inline(never)]
pub(crate) fn identity() -> usize {
    cur().addr()
}

#[derive(Clone, Copy)]
struct UP(*const Unit);
// SAFETY: unit records are leaked and shared by design.
unsafe impl Send for UP {}

struct Pool {
    w: usize,
    k: u32,
    locals: Vec<Mutex<VecDeque<UP>>>,
    /// Per worker: the unit a unit running there last woke. Not stealable;
    /// run next by its worker, at most LIFO_RUNS times in a row (as Tokio's
    /// LIFO slot), so a request and its reply stay on one core.
    lifo: Vec<AtomicPtr<Unit>>,
    lifo_runs: Vec<AtomicU32>,
    lifo_on: bool,
    injector: Mutex<VecDeque<UP>>,
    memory: Memory,
    /// Units in the local queues and the injector. The LIFO slots are not
    /// counted: only their own worker takes them.
    queued: AtomicUsize,
    searching: AtomicUsize,
    /// Parked workers, most recent last, and their count for a lock-free check.
    idle: Mutex<Vec<usize>>,
    nsleep: AtomicUsize,
    /// Per worker: its thread, and whether a wake handed it the search role.
    threads: Vec<OnceLock<std::thread::Thread>>,
    handed: Vec<AtomicBool>,
    poll: Mutex<()>,
    poller_blocked: AtomicBool,
    poll_until: AtomicU64,
    gep: i32,
    gwake: i32,
    _shared: Arc<hirsel_core_net::Poller>,
    timers: Mutex<BinaryHeap<Reverse<(u64, u32)>>>,
    next_timer: AtomicU64,
    live: AtomicUsize,
    stop: AtomicBool,
    front: i32,
    next_session: AtomicU32,
    last_poll: AtomicU64,
    spin_ns: u64,
    slice_ns: u64,
    poll_every_ns: u64,
    spin_poll_ns: u64,
    queue_waits: Option<QueueWaits>,
}
static POOL: OnceLock<Pool> = OnceLock::new();
fn pool() -> &'static Pool {
    POOL.get().unwrap()
}
pub(crate) fn memory() -> Option<&'static Memory> {
    if !untied() {
        return None;
    }
    let p = pool();
    (!p.stop.load(Ordering::Acquire)).then_some(&p.memory)
}
struct RunFn(&'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync));
static RUN: OnceLock<RunFn> = OnceLock::new();
struct Base(crate::entry::OsConfig);
// SAFETY: the launcher pins the configuration for the whole launch.
unsafe impl Send for Base {}
unsafe impl Sync for Base {}
static BASE: OnceLock<Base> = OnceLock::new();
static WORST: AtomicU8 = AtomicU8::new(0);

fn now() -> u64 {
    crate::os::monotonic_ns()
}

// Optional timing for the fairness experiment. Maxima are kept per worker
// and 10 ms bucket so setup and teardown can be excluded after the run.
const QUEUE_BUCKET_NS: u64 = 10_000_000;
struct QueueWaits {
    origin_ns: u64,
    workers: Vec<Vec<AtomicU64>>,
}

impl QueueWaits {
    fn new(w: usize) -> Self {
        Self {
            origin_ns: now(),
            workers: (0..w)
                .map(|_| (0..6000).map(|_| AtomicU64::new(0)).collect())
                .collect(),
        }
    }

    fn record(&self, ix: usize, queued: u64, started: u64) {
        let bucket = (started - self.origin_ns) / QUEUE_BUCKET_NS;
        if queued != 0 && let Some(max) = self.workers[ix].get(bucket as usize) {
            max.fetch_max(started - queued, Ordering::Relaxed);
        }
    }

    fn report(&self) {
        for (i, _) in self.workers[0].iter().enumerate() {
            let max = self.workers.iter().map(|w| w[i].load(Ordering::Relaxed)).max().unwrap();
            if max != 0 {
                eprintln!("queue-wait {} {max}", self.origin_ns + i as u64 * QUEUE_BUCKET_NS);
            }
        }
    }
}

fn mark_queued(p: &Pool, u: &Unit) {
    if p.queue_waits.is_some() && (p.k..0x8000).contains(&u.id) {
        u.queued_ns.store(now(), Ordering::Relaxed);
    }
}

/// Make unit `id` and queue it. The record and its stack are made here; its
/// program starts on its first run.
fn create_unit(id: u32, role: u32) -> &'static Unit {
    let t0 = now();
    let p = pool();
    let base = &BASE.get().unwrap().0;
    let extra = vec![id.to_string(), p.k.to_string(), role.to_string()];
    let mut views: Vec<crate::process::View> =
        // SAFETY: the base argument views are pinned for the launch.
        (0..base.args_len).map(|j| unsafe { *base.args.add(j) }).collect();
    for e in &extra {
        views.push(crate::process::View { data: e.as_ptr(), len: e.len() });
    }
    let config = crate::entry::OsConfig { args: views.as_ptr(), args_len: views.len(), ..*base };
    let stack = p.memory.take_unit_stack();
    let top = stack.top();
    let u: &'static Unit = Box::leak(Box::new(Unit {
        id,
        sp: UnsafeCell::new(std::ptr::null_mut()),
        host: UnsafeCell::new(std::ptr::null_mut()),
        stack: UnsafeCell::new(Some(stack)),
        state: AtomicU8::new(QUEUED),
        inside: AtomicBool::new(false),
        after: Cell::new(After::Run),
        next_local: Cell::new(0),
        deliveries: Arc::new(AtomicU64::new(0)),
        unobserved: URefCell::new(Vec::new()),
        children: Cell::new(0),
        xready: Mutex::new(Vec::new()),
        xpending: AtomicBool::new(false),
        xspare: UnsafeCell::new(Vec::new()),
        status: AtomicU8::new(0),
        stop_fd: Cell::new(2),
        slice_start: Cell::new(0),
        queued_ns: AtomicU64::new(0),
        created_ns: t0,
        first_park: AtomicBool::new(false),
        alone: std::sync::OnceLock::new(),
        alone_depth: Cell::new(0),
        _views: views,
        _extra: extra,
        config,
    }));
    // SAFETY: the frame is written at the top of the unit's own stack.
    unsafe {
        let words = hirsel_stack::initial_frame(unit_entry, (u as *const Unit).cast_mut().cast(), std::ptr::null_mut());
        let start = top.sub(16 + size_of_val(&words));
        std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), start, size_of_val(&words));
        *u.sp.get() = start;
    }
    if !TABLE[id as usize]
        .compare_exchange(std::ptr::null_mut(), (u as *const Unit).cast_mut(), Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        crate::process::stop(b"UnitIdTaken");
    }
    p.live.fetch_add(1, Ordering::SeqCst);
    bump(S_CREATED);
    USTATS[S_CREATE_NS].fetch_add(now() - t0, Ordering::Relaxed);
    enqueue(p, u);
    u
}

/// A child of the calling unit: id 0x8000 + parent*32 + n, a function of the
/// parent's own order only.
fn spawn_child(parent: u32) -> u32 {
    // SAFETY: the running unit's record; only its worker touches it.
    let u = unsafe { &*cur() };
    let n = u.children.get();
    u.children.set(n + 1);
    let id = 0x8000 + parent * 32 + n;
    if n >= 32 || id >= SPAWN {
        crate::process::stop(b"UnitIdSpace");
    }
    create_unit(id, ROLE_CHILD);
    id
}

unsafe extern "C" fn unit_entry(a: *mut u8, _b: *mut u8) -> ! {
    // SAFETY: the record outlives its stack; see create_unit.
    unsafe {
        let u = &*(a as *const Unit);
        let run = RUN.get().unwrap().0;
        let status = run(&u.config);
        finish_unit(u.id);
        u.status.store(status, Ordering::Relaxed);
        u.after.set(After::Done);
        hirsel_stack::switch(u.sp.get(), **u.host.get());
        std::process::abort()
    }
}

/// Switch from the running unit back to its worker. Returns when a worker
/// (any) resumes the unit.
fn leave(after: After) {
    // SAFETY: called on the unit's own stack while it runs.
    unsafe {
        let u = &*cur();
        crate::tasks::check_idle_tls();
        u.after.set(after);
        hirsel_stack::switch(u.sp.get(), **u.host.get());
    }
}

/// The running unit has nothing runnable: park until a wake or `deadline`.
pub(crate) fn park(deadline: Option<u64>) {
    bump(S_PARKS);
    // SAFETY: called on the unit's own stack while it runs.
    let u = unsafe { &*cur() };
    if !u.first_park.swap(true, Ordering::Relaxed) {
        USTATS[S_READY_TO_PARK_NS].fetch_add(now() - u.created_ns, Ordering::Relaxed);
    }
    leave(After::Wait(deadline));
}

/// The running unit's record, if untied and inside a unit.
pub(crate) fn running() -> Option<&'static Unit> {
    if !untied() {
        return None;
    }
    let u = cur();
    // SAFETY: a live unit record (records are never freed).
    (!u.is_null()).then(|| unsafe { &*u })
}
pub(crate) fn unit_id(u: &Unit) -> u32 {
    u.id
}

/// Requeue the running unit if its slice is over and another unit waits.
#[inline]
pub(crate) fn maybe_yield() {
    let p = pool();
    if p.queued.load(Ordering::Relaxed) == 0 && p.lifo[worker_ix()].load(Ordering::Relaxed).is_null() {
        return;
    }
    // SAFETY: called on the unit's own stack while it runs.
    let u = unsafe { &*cur() };
    if now().saturating_sub(u.slice_start.get()) < p.slice_ns {
        return;
    }
    bump(S_YIELDS);
    leave(After::Yield);
}

/// Publish readiness to the next entry even when the unit already has a
/// pending entry. Only the PARKED transition publishes a new queue item.
pub(crate) fn wake(u: &Unit) {
    let prior = u.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
        match state {
            PARKED => Some(QUEUED),
            RUNNING => Some(NOTIFIED),
            QUEUED | NOTIFIED => Some(state),
            _ => None,
        }
    });
    if prior == Ok(PARKED) {
        enqueue(pool(), u);
    }
}
pub(crate) fn wake_id(id: u32) {
    if let Some(u) = unit_ref(id) {
        wake(u);
    }
}

/// A crossing event for unit `unit`'s key `id`: queue it and wake the unit.
fn xnotify(unit: u32, id: u64) {
    if id == 0 {
        return;
    }
    let Some(u) = unit_ref(unit) else { return };
    push_arrival(u, id, (libc::EPOLLIN | libc::EPOLLOUT) as u32);
    bump(S_XNOTIFY);
    wake(u);
}
fn push_arrival(u: &Unit, id: u64, flags: u32) {
    let mut q = u.xready.lock().unwrap();
    q.push((id, flags));
    u.xpending.store(true, Ordering::Release);
}
/// Readiness queued for the running unit, as (reactor identity, flags), in
/// the order it reached the unit. A push after the check wakes the unit, so
/// it is taken at the unit's next switch.
pub(crate) fn take_xready(mut each: impl FnMut(u64, u32)) {
    // SAFETY: called by the running unit's scheduler.
    let u = unsafe { &*cur() };
    if !u.xpending.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: only the running unit touches its spare buffer.
    let batch = unsafe { &mut *u.xspare.get() };
    {
        let mut q = u.xready.lock().unwrap();
        u.xpending.store(false, Ordering::Relaxed);
        std::mem::swap(&mut *q, batch);
    }
    for (id, flags) in batch.drain(..) {
        each(id, flags);
    }
}

/// The running unit's id, for the shared poller's tags.
fn shared_unit() -> Option<u32> {
    let u = cur();
    // SAFETY: a live unit record.
    (!u.is_null()).then(|| unsafe { (*u).id })
}

const LIFO_RUNS: u32 = 3;
fn enqueue(p: &Pool, u: &Unit) {
    mark_queued(p, u);
    let ix = worker_ix();
    if ix < p.w && p.lifo_on && !cur().is_null() {
        // Woken by the unit running on this worker: it runs here next.
        let prev = p.lifo[ix].swap((u as *const Unit).cast_mut(), Ordering::AcqRel);
        if !prev.is_null() {
            p.locals[ix].lock().unwrap().push_back(UP(prev));
            p.queued.fetch_add(1, Ordering::SeqCst);
            notify_worker(p);
        }
        return;
    }
    if ix < p.w {
        p.locals[ix].lock().unwrap().push_back(UP(u));
    } else {
        p.injector.lock().unwrap().push_back(UP(u));
    }
    p.queued.fetch_add(1, Ordering::SeqCst);
    notify_worker(p);
}

/// Wake a sleeping worker only if none is awake and looking for work.
fn notify_worker(p: &Pool) {
    if p.searching.load(Ordering::SeqCst) > 0 {
        return;
    }
    if p.nsleep.load(Ordering::SeqCst) > 0 {
        let mut idle = p.idle.lock().unwrap();
        if p.searching.load(Ordering::SeqCst) == 0
            && let Some(w) = idle.pop()
        {
            p.nsleep.fetch_sub(1, Ordering::SeqCst);
            // The woken worker searches on this count; later wakes in the
            // same burst see a searcher and leave the others parked.
            p.searching.fetch_add(1, Ordering::SeqCst);
            p.handed[w].store(true, Ordering::Release);
            drop(idle);
            p.threads[w].get().unwrap().unpark();
            bump(S_OS_WAKES);
            return;
        }
    }
    if p.poller_blocked.load(Ordering::SeqCst) {
        write_wake(p);
    }
}
fn write_wake(p: &Pool) {
    bump(S_OS_WAKES);
    let one = 1u64;
    // SAFETY: eight-byte write to a live eventfd.
    unsafe { libc::write(p.gwake, (&one as *const u64).cast(), 8) };
}

fn add_timer(p: &Pool, at: u64, id: u32) {
    p.timers.lock().unwrap().push(Reverse((at, id)));
    p.next_timer.fetch_min(at, Ordering::SeqCst);
    if p.poller_blocked.load(Ordering::SeqCst) && at < p.poll_until.load(Ordering::SeqCst) {
        write_wake(p);
    }
}
fn fire_timers(p: &Pool) {
    let next = p.next_timer.load(Ordering::SeqCst);
    if next == u64::MAX {
        return;
    }
    let t = now();
    if next > t {
        return;
    }
    let mut due = Vec::new();
    {
        let mut heap = p.timers.lock().unwrap();
        while let Some(&Reverse((at, id))) = heap.peek() {
            if at > t {
                break;
            }
            heap.pop();
            due.push(id);
        }
        let next = heap.peek().map_or(u64::MAX, |r| r.0.0);
        p.next_timer.store(next, Ordering::SeqCst);
    }
    for id in due {
        bump(S_TIMER_FIRES);
        wake_id(id);
    }
}

fn poll_global(p: &Pool, timeout: i32) {
    let mut evs: [libc::epoll_event; 128] = [libc::epoll_event { events: 0, u64: 0 }; 128];
    bump(S_GPOLLS);
    if timeout != 0 {
        bump(S_GPOLL_BLOCKS);
    }
    // SAFETY: a live epoll and a writable event array.
    let n = unsafe { libc::epoll_wait(p.gep, evs.as_mut_ptr(), evs.len() as i32, timeout) };
    p.last_poll.store(now(), Ordering::Relaxed);
    for e in evs.iter().take(n.max(0) as usize) {
        let tag = e.u64;
        if tag == WAKE_TAG {
            let mut v = 0u64;
            // SAFETY: nonblocking eventfd read.
            unsafe { libc::read(p.gwake, (&mut v as *mut u64).cast(), 8) };
        } else if tag == FRONT_TAG {
            accept_front(p);
        } else {
            USTATS[S_GPOLL_EVENTS].fetch_add(1, Ordering::Relaxed);
            let unit = (tag >> hirsel_core_net::SHARED_UNIT_SHIFT) as u32;
            if let Some(u) = unit_ref(unit) {
                let ident = tag & ((1 << hirsel_core_net::SHARED_UNIT_SHIFT) - 1);
                push_arrival(u, ident, e.events);
                wake(u);
            }
        }
    }
    fire_timers(p);
}

fn accept_front(p: &Pool) {
    loop {
        // SAFETY: nonblocking accept on the live front listener.
        let fd = unsafe {
            libc::accept4(p.front, std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC)
        };
        if fd < 0 {
            return;
        }
        let one = 1i32;
        // SAFETY: a fresh connected socket.
        unsafe {
            libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, (&one as *const i32).cast(), 4);
        }
        let id = p.next_session.fetch_add(1, Ordering::SeqCst);
        if id >= 0x8000 {
            crate::process::stop(b"UnitIdSpace");
        }
        if std::env::var_os("HIRSEL_UNTIED_TRACE_FRONT").is_some() {
            // SAFETY: getpeername into a local sockaddr.
            let port = unsafe {
                let mut a: libc::sockaddr_in = std::mem::zeroed();
                let mut l = size_of::<libc::sockaddr_in>() as u32;
                libc::getpeername(fd, (&mut a as *mut libc::sockaddr_in).cast(), &mut l);
                u16::from_be(a.sin_port)
            };
            eprintln!("front id={id} fd={fd} peer={port}");
        }
        // SAFETY: accept4 returned a descriptor nothing else owns.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        listener_for(id, HANDOFF_PORT).queue.lock().unwrap().push_back(Pend::Fd(owned));
        create_unit(id, ROLE_SESSION);
    }
}

/// The next unit for worker `ix`, taken off the queues and the count.
fn find(p: &Pool, ix: usize) -> Option<UP> {
    if !p.lifo[ix].load(Ordering::Relaxed).is_null() {
        let l = p.lifo[ix].swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !l.is_null() {
            if p.lifo_runs[ix].load(Ordering::Relaxed) < LIFO_RUNS {
                p.lifo_runs[ix].fetch_add(1, Ordering::Relaxed);
                bump(S_LIFO);
                return Some(UP(l));
            }
            p.locals[ix].lock().unwrap().push_back(UP(l));
            p.queued.fetch_add(1, Ordering::SeqCst);
        }
    }
    p.lifo_runs[ix].store(0, Ordering::Relaxed);
    if p.queued.load(Ordering::SeqCst) == 0 {
        fire_timers(p);
        if p.queued.load(Ordering::SeqCst) == 0 {
            return None;
        }
    }
    let taken = |u: UP| {
        p.queued.fetch_sub(1, Ordering::SeqCst);
        Some(u)
    };
    if let Some(u) = p.locals[ix].lock().unwrap().pop_front() {
        return taken(u);
    }
    if let Some(u) = p.injector.lock().unwrap().pop_front() {
        return taken(u);
    }
    for j in 1..p.w {
        let v = (ix + j) % p.w;
        if let Ok(mut q) = p.locals[v].try_lock()
            && let Some(u) = q.pop_back()
        {
            drop(q);
            bump(S_STEALS);
            return taken(u);
        }
    }
    None
}

/// Poll the shared poller without blocking if no worker is blocked in it
/// and the pool has not polled for `every` nanoseconds.
fn poll_due(p: &Pool, t: u64, every: u64) {
    if !p.poller_blocked.load(Ordering::Relaxed)
        && t.saturating_sub(p.last_poll.load(Ordering::Relaxed)) > every
        && let Ok(_g) = p.poll.try_lock()
    {
        poll_global(p, 0);
    }
}

/// Run one queued unit until it parks, yields or ends.
unsafe fn run_unit(p: &Pool, ix: usize, up: UP, host: &mut *mut u8) {
    // SAFETY: the dequeued record is live; this worker alone dequeued it.
    unsafe {
        let u = &*up.0;
        // Acquire readiness published while this entry was already queued.
        u.state.swap(RUNNING, Ordering::SeqCst);
        // Law 1: never two workers inside one unit.
        if u.inside.swap(true, Ordering::SeqCst) {
            eprintln!("untied: law 1 violated: unit {} entered twice", u.id);
            std::process::abort();
        }
        bump(S_RESUMES);
        CUR.set(up.0.cast_mut());
        let prev_fd = crate::fiber::set_stop_fd(u.stop_fd.get());
        *u.host.get() = host as *mut *mut u8;
        let started = now();
        if let Some(waits) = &p.queue_waits {
            waits.record(ix, u.queued_ns.load(Ordering::Relaxed), started);
        }
        u.slice_start.set(started);
        hirsel_stack::switch(host as *mut *mut u8, *u.sp.get());
        u.stop_fd.set(crate::fiber::set_stop_fd(prev_fd));
        CUR.set(std::ptr::null_mut());
        let after = u.after.replace(After::Run);
        u.inside.store(false, Ordering::SeqCst);
        match after {
            After::Done => {
                WORST.fetch_max(u.status.load(Ordering::Relaxed), Ordering::Relaxed);
                u.state.store(DONE, Ordering::SeqCst);
                // The terminal switch returned to the host stack. DONE
                // rejects further wakes, and nothing can resume this stack.
                p.memory.put_unit_stack((*u.stack.get()).take().unwrap());
                if p.live.fetch_sub(1, Ordering::SeqCst) == 1 {
                    p.stop.store(true, Ordering::SeqCst);
                    let parked = std::mem::take(&mut *p.idle.lock().unwrap());
                    for w in parked {
                        p.nsleep.fetch_sub(1, Ordering::SeqCst);
                        p.threads[w].get().unwrap().unpark();
                    }
                    write_wake(p);
                }
            }
            After::Yield => {
                u.state.swap(QUEUED, Ordering::SeqCst);
                mark_queued(p, u);
                p.injector.lock().unwrap().push_back(up);
                p.queued.fetch_add(1, Ordering::SeqCst);
                notify_worker(p);
            }
            After::Wait(deadline) => {
                if let Some(at) = deadline {
                    add_timer(p, at, u.id);
                }
                if u
                    .state
                    .compare_exchange(RUNNING, PARKED, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    // Woken while it ran: run it again.
                    bump(S_NOTIFIED_REQUEUE);
                    u.state.swap(QUEUED, Ordering::SeqCst);
                    mark_queued(p, u);
                    p.locals[ix].lock().unwrap().push_back(up);
                    p.queued.fetch_add(1, Ordering::SeqCst);
                }
            }
            After::Run => crate::process::stop(b"UnitLeftWithoutReason"),
        }
        // Busy workers poll now and then when no idle worker is blocked in
        // the poller.
        if !p.poller_blocked.load(Ordering::Relaxed) {
            poll_due(p, now(), p.poll_every_ns);
        }
    }
}

fn worker_main(p: &'static Pool, ix: usize) {
    crate::fiber::arm();
    WORKER_IX.set(ix);
    let mut host: *mut u8 = std::ptr::null_mut();
    let _ = p.threads[ix].set(std::thread::current());
    // A wake handed this worker the search role (already counted).
    let mut handed = false;
    loop {
        if !handed && let Some(u) = find(p, ix) {
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if p.stop.load(Ordering::SeqCst) && p.queued.load(Ordering::SeqCst) == 0 {
            if handed {
                p.searching.fetch_sub(1, Ordering::SeqCst);
            }
            break;
        }
        // Search: at most half the workers at once. A searcher watches the
        // queued count without taking queue locks, and polls only when no
        // worker is blocked in the poller.
        let mut got = None;
        if std::mem::take(&mut handed) || 2 * p.searching.fetch_add(1, Ordering::SeqCst) < p.w {
            let t0 = now();
            let mut i = 0u32;
            loop {
                if p.queued.load(Ordering::SeqCst) > 0
                    && let Some(u) = find(p, ix)
                {
                    got = Some(u);
                    break;
                }
                i = i.wrapping_add(1);
                if i % 64 == 0 {
                    let t = now();
                    if t - t0 > p.spin_ns || p.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    poll_due(p, t, p.spin_poll_ns);
                }
                std::hint::spin_loop();
            }
        }
        p.searching.fetch_sub(1, Ordering::SeqCst);
        if let Some(u) = got {
            // The last searcher to find work wakes another if work remains.
            if p.queued.load(Ordering::SeqCst) > 0 {
                notify_worker(p);
            }
            // SAFETY: a dequeued unit.
            unsafe { run_unit(p, ix, u, &mut host) };
            continue;
        }
        if p.stop.load(Ordering::SeqCst) && p.queued.load(Ordering::SeqCst) == 0 {
            break;
        }
        match p.poll.try_lock() {
            Ok(_g) => {
                p.poller_blocked.store(true, Ordering::SeqCst);
                if p.queued.load(Ordering::SeqCst) == 0 && !p.stop.load(Ordering::SeqCst) {
                    let nt = p.next_timer.load(Ordering::SeqCst);
                    p.poll_until.store(nt, Ordering::SeqCst);
                    let timeout = if nt == u64::MAX {
                        -1
                    } else {
                        nt.saturating_sub(now()).div_ceil(1_000_000).min(i32::MAX as u64) as i32
                    };
                    poll_global(p, timeout);
                }
                p.poller_blocked.store(false, Ordering::SeqCst);
            }
            Err(_) => {
                let mut idle = p.idle.lock().unwrap();
                p.nsleep.fetch_add(1, Ordering::SeqCst);
                if p.queued.load(Ordering::SeqCst) != 0 || p.stop.load(Ordering::SeqCst) {
                    p.nsleep.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                idle.push(ix);
                drop(idle);
                loop {
                    std::thread::park();
                    if p.handed[ix].swap(false, Ordering::AcqRel) {
                        handed = true;
                        break;
                    }
                    if p.stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    }
}

fn launch_untied(
    k: usize,
    w: usize,
    input: &crate::entry::OsConfig,
    run: &(dyn Fn(&crate::entry::OsConfig) -> u8 + Sync),
) -> u8 {
    // One poller for every unit's descriptors; its wake eventfd is WAKE_TAG.
    let shared = hirsel_core_net::share_poller(shared_unit).unwrap_or_else(|_| crate::process::stop(b"HostPoll"));
    let (gep, gwake) = (shared.raw_fd(), shared.wake_fd());
    let front = match std::env::var("HIRSEL_FRONT").ok().and_then(|v| v.parse::<u16>().ok()) {
        // SAFETY: a fresh nonblocking listener on 127.0.0.1:port.
        Some(port) => unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
            let one = 1i32;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, (&one as *const i32).cast(), 4);
            let mut a: libc::sockaddr_in = std::mem::zeroed();
            a.sin_family = libc::AF_INET as u16;
            a.sin_port = port.to_be();
            a.sin_addr.s_addr = u32::from_be_bytes([127, 0, 0, 1]).to_be();
            if libc::bind(fd, (&a as *const libc::sockaddr_in).cast(), size_of::<libc::sockaddr_in>() as u32) != 0
                || libc::listen(fd, 4096) != 0
            {
                crate::process::stop(b"FrontListen");
            }
            let mut ev = libc::epoll_event { events: libc::EPOLLIN as u32, u64: FRONT_TAG };
            libc::epoll_ctl(gep, libc::EPOLL_CTL_ADD, fd, &mut ev);
            fd
        },
        None => -1,
    };
    let _ = POOL.set(Pool {
        w,
        k: k as u32,
        locals: (0..w).map(|_| Mutex::new(VecDeque::new())).collect(),
        lifo: (0..w).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(),
        lifo_runs: (0..w).map(|_| AtomicU32::new(0)).collect(),
        lifo_on: env_u64("HIRSEL_UNTIED_LIFO", 1) != 0,
        injector: Mutex::new(VecDeque::new()),
        memory: Memory::new(),
        queued: AtomicUsize::new(0),
        searching: AtomicUsize::new(0),
        idle: Mutex::new(Vec::with_capacity(w)),
        nsleep: AtomicUsize::new(0),
        threads: (0..w).map(|_| OnceLock::new()).collect(),
        handed: (0..w).map(|_| AtomicBool::new(false)).collect(),
        poll: Mutex::new(()),
        poller_blocked: AtomicBool::new(false),
        poll_until: AtomicU64::new(u64::MAX),
        gep,
        gwake,
        _shared: shared,
        timers: Mutex::new(BinaryHeap::new()),
        next_timer: AtomicU64::new(u64::MAX),
        live: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        front,
        next_session: AtomicU32::new(k as u32),
        last_poll: AtomicU64::new(0),
        spin_ns: env_u64("HIRSEL_UNTIED_SPIN_US", 5) * 1000,
        spin_poll_ns: env_u64("HIRSEL_UNTIED_SPIN_POLL_US", 20) * 1000,
        slice_ns: env_u64("HIRSEL_UNTIED_SLICE_US", 500) * 1000,
        poll_every_ns: env_u64("HIRSEL_UNTIED_POLL_US", 100) * 1000,
        queue_waits: (env_u64("HIRSEL_UNTIED_QUEUE_WAIT", 0) != 0).then(|| QueueWaits::new(w)),
    });
    // SAFETY: the launch joins every worker before `run` goes out of scope.
    let run: &'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync) = unsafe { std::mem::transmute(run) };
    let _ = RUN.set(RunFn(run));
    let _ = BASE.set(Base(*input));
    UNTIED_ON.store(true, Ordering::SeqCst);
    let p = pool();
    let memory_thread = p.memory.start();
    for i in 0..k {
        create_unit(i as u32, ROLE_INITIAL);
    }
    std::thread::scope(|scope| {
        for ix in 0..w {
            std::thread::Builder::new()
                .name(format!("hirsel-w{ix}"))
                .stack_size(1 << 20)
                .spawn_scoped(scope, move || worker_main(p, ix))
                .unwrap();
        }
    });
    drop(memory_thread);
    // Every recording block is in its file before the run reports.
    crate::tape::writer::flush();
    if let Some(waits) = &p.queue_waits {
        waits.report();
    }
    if std::env::var_os("HIRSEL_XSTATS").is_some() {
        p.memory.report();
        let g = |i: usize| USTATS[i].load(Ordering::Relaxed);
        eprintln!(
            "untied-stats workers={} units={} resumes={} parks={} yields={} steals={} os_wakes={} gpolls={} gpoll_blocks={} gpoll_events={} xnotify={} timer_fires={} notified_requeue={} create_ns_avg={} ready_to_park_ns_avg={} token_acquires={} msgs={} bytes={} full_waits={} unit_epoll_waits={} lifo={} tape_writes={} tape_bytes={} tape_payload={} tape_skipped={} tape_worker_ns={} tape_truncated={} tape_writer_ns={} tape_writer_calls={} tape_write_failed={}",
            w,
            g(S_CREATED),
            g(S_RESUMES),
            g(S_PARKS),
            g(S_YIELDS),
            g(S_STEALS),
            g(S_OS_WAKES),
            g(S_GPOLLS),
            g(S_GPOLL_BLOCKS),
            g(S_GPOLL_EVENTS),
            g(S_XNOTIFY),
            g(S_TIMER_FIRES),
            g(S_NOTIFIED_REQUEUE),
            g(S_CREATE_NS) / g(S_CREATED).max(1),
            g(S_READY_TO_PARK_NS) / g(S_CREATED).max(1),
            crate::sequencer::ACQUIRES.load(Ordering::Relaxed),
            STATS[0].load(Ordering::Relaxed),
            STATS[1].load(Ordering::Relaxed),
            STATS[3].load(Ordering::Relaxed),
            hirsel_core_net::EPOLL_WAITS.load(Ordering::Relaxed),
            g(S_LIFO),
            crate::tape::TAPE_COUNTS[0].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[1].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[2].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[3].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[4].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[5].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[6].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[7].load(Ordering::Relaxed),
            crate::tape::TAPE_COUNTS[8].load(Ordering::Relaxed),
        );
    }
    if crate::sequencer::ACQUIRES.load(Ordering::Relaxed) != 0 {
        eprintln!("untied: law 4 violated: the cross-worker ordering token was taken");
        return 101;
    }
    WORST.load(Ordering::Relaxed)
}
