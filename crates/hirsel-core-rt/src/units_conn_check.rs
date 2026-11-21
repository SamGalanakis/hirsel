//! Connection and ledger cases for the interleaving checker.
//!
//! Units run the real ledger (`units::ledger`) on pool units: a unit parks on
//! its port when nothing has reached it, and a publish to a parked port wakes
//! it. Each case checks two laws.
//!
//! - Conservation, in the run: every accepted item is received or cleaned
//!   exactly once, in each sender's order, and every unit's run root is back
//!   to zero once its releases are taken. A broken law panics the unit, which
//!   fails the seed.
//! - Determinism, across the search (SEM-8): a unit's observations (every
//!   result and its records' balances after every operation) are a function
//!   of its inputs (the arrivals it takes, in order, and each send's decision
//!   at issue). Each passing seed reports every unit's stream of inputs and
//!   observations; two seeds whose streams first differ at anything but an
//!   input break the law. A room's balance is left out: it moves with other
//!   units' sends, and no limit sees it apart from those send decisions.
use super::{FLAG, PARAM, compute_until, draw, now, param, served, this_unit, wait_flag};
use crate::units::ledger::conn::{Client, Inbox, Received, Sent};
use crate::units::ledger::{NAMING, Shared, TakeInfo, Took, UnitCtx, Value};
use crate::units::wake_id;
use crate::units_lang::CrossSend;
use std::sync::atomic::Ordering;

/// Each unit's stream of inputs (`I`) and observations (`O`), by unit index.
pub(super) static OBS: [std::sync::Mutex<Vec<String>>; 4] =
    [const { std::sync::Mutex::new(Vec::new()) }; 4];

/// The streams as the child reports them on a pass.
pub(super) fn obs_text() -> String {
    let mut s = String::new();
    for (i, o) in OBS.iter().enumerate() {
        for e in o.lock().unwrap().iter() {
            s.push_str(&format!("OBS {i} {e}\n"));
        }
    }
    s
}

/// The determinism law across seeds: a trie of every reported stream per
/// unit. At each node, the next events seen so far are either all inputs, or
/// one observation; anything else is a unit that observed something its
/// inputs do not determine.
#[derive(Default)]
pub(super) struct Law {
    nodes: Vec<std::collections::HashMap<String, (usize, u64)>>,
    roots: std::collections::HashMap<u64, usize>,
}

impl Law {
    /// Add one passing seed's report; a violation names the earlier seed.
    pub(super) fn add(&mut self, seed: u64, text: &str) -> Option<String> {
        let mut streams: std::collections::BTreeMap<u64, Vec<&str>> = Default::default();
        for l in text.lines() {
            if let Some(r) = l.strip_prefix("OBS ") {
                let (u, e) = r.split_once(' ').unwrap();
                streams.entry(u.parse().unwrap()).or_default().push(e);
            }
        }
        for (u, evs) in streams {
            let root = *self.roots.entry(u).or_insert_with(|| {
                self.nodes.push(Default::default());
                self.nodes.len() - 1
            });
            let mut at = root;
            for (k, e) in evs.iter().enumerate() {
                let node = &self.nodes[at];
                if let Some(&(next, _)) = node.get(*e) {
                    at = next;
                    continue;
                }
                let input = e.starts_with("I ");
                if let Some((other, (_, s))) =
                    node.iter().find(|(o, _)| !input || !o.starts_with("I "))
                {
                    return Some(format!(
                        "unit {u}, event {k}: seed {seed} has `{e}` where seed {s} has `{other}` after the same inputs"
                    ));
                }
                self.nodes.push(Default::default());
                let next = self.nodes.len() - 1;
                self.nodes[at].insert(e.to_string(), (next, seed));
                at = next;
            }
        }
        None
    }
}

/// A slot for handing an object to another unit (as a sent value would).
struct Slot<T>(std::sync::Mutex<Option<T>>);
// SAFETY: one unit puts, another takes after a flag; the checker runs one
// thread at a time.
unsafe impl<T> Sync for Slot<T> {}
impl<T> Slot<T> {
    const fn new() -> Self {
        Slot(std::sync::Mutex::new(None))
    }
    fn put(&self, x: T) {
        *self.0.lock().unwrap() = Some(x);
    }
    fn take(&self) -> T {
        self.0.lock().unwrap().take().expect("an empty slot")
    }
}

static CLIENT: Slot<Client> = Slot::new();
static OTHER_CLIENT: Slot<Client> = Slot::new();
static HELPER: Slot<Box<UnitCtx>> = Slot::new();
static SHARE: Slot<Shared> = Slot::new();
/// Items the sender saw accepted, published before it drops its client.
static ACCEPTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A unit's ledger, recording its inputs and observations.
struct Cx {
    u: Box<UnitCtx>,
    ix: usize,
}

impl Cx {
    fn root(ix: usize, limit: u64) -> Cx {
        NAMING.store(true, Ordering::Relaxed);
        Self::on(ix, Box::new(UnitCtx::root_as(limit, &format!("U{ix}"))))
    }
    fn on(ix: usize, u: Box<UnitCtx>) -> Cx {
        u.bind(this_unit());
        Cx { u, ix }
    }
    fn ev(&self, s: String) {
        OBS[self.ix].lock().unwrap().push(s);
    }
    fn obs(&self, op: &str, res: impl std::fmt::Display) {
        let mut v = Vec::new();
        self.u.snapshot(&mut v);
        v.retain(|s| !s.contains("/room="));
        v.sort();
        self.ev(format!("O {op} {res} | {}", v.join(" ")));
    }
    fn wait(&mut self) -> Took {
        let (t, a) = self.u.wait();
        self.took(&a);
        t
    }
    fn took(&self, a: &TakeInfo) {
        let n = a.as_ref().expect("arrivals are named in the checker");
        self.ev(format!("I take {} [{}]", n.ident, n.content));
        self.obs("took", "");
    }
    fn send(&mut self, c: &Client, v: Value, blocking: bool) -> Sent {
        let r = self.u.send(c, v, blocking);
        self.sent(&r);
        r
    }
    fn sent(&self, r: &Sent) {
        let d = match r {
            Sent::Bounds => "Bounds",
            Sent::Closed(_) => "Closed",
            Sent::Accepted => "Accepted",
            Sent::Full(_) => "Full",
            Sent::Waiting(_) => "Waiting",
        };
        self.ev(format!("I send {d}"));
        self.obs("send", "");
    }
    fn recv(&mut self, i: &Inbox) -> Received {
        let r = self.u.receive(i);
        let s = match &r {
            Received::Ok(v) => format!("Ok {}", v.tag),
            other => format!("{other:?}"),
        };
        self.obs("recv", s);
        r
    }
    fn drop_value(&mut self, v: Value) {
        let r = self.u.drop_value(v);
        assert!(r.is_none(), "a plain value's drop made a cause");
        self.obs("drop", "");
    }
    fn used(&self) -> u64 {
        self.u.current_used()
    }
}

/// 1. A waiting send against a lock-free receive. Unit 1 sends `ITEMS` items
/// down a one-slot connection, waiting whenever it is full; unit 0 receives
/// them with a random pause between receives, then seals, cleans and takes
/// the last release. A sender that finds the slot taken sets the waiters
/// flag with a compare-exchange on the word it checked, so a receive that
/// frees the slot in between makes it retry instead of waiting unserved.
pub(super) const ITEMS: u64 = 5;
pub(super) fn wait_serve(id: u32) -> u8 {
    match id {
        0 => {
            let mut cx = Cx::root(0, 4096);
            let (inbox, client) = cx.u.conn_new(1, 8, 64).unwrap();
            cx.obs("conn_new", "");
            CLIENT.put(client);
            FLAG[0].store(true, Ordering::SeqCst);
            wake_id(1);
            let mut next = 1;
            loop {
                compute_until(now() + draw(0, param(0)));
                match cx.recv(&inbox) {
                    Received::Ok(v) => {
                        assert_eq!(v.tag, next, "items out of their sender's order");
                        next += 1;
                        cx.drop_value(v);
                    }
                    Received::Empty => {
                        cx.wait();
                    }
                    Received::Closed => break,
                    Received::Capacity(c) => panic!("receive refused: {c:?}"),
                }
            }
            assert_eq!(next, ITEMS + 1, "an accepted item was lost");
            retire(&mut cx, &inbox);
            served(0, "unit 0 received every item and its root is back to zero");
        }
        _ => {
            wait_flag(0);
            let client = CLIENT.take();
            let mut cx = Cx::root(1, 4096);
            for k in 1..=ITEMS {
                let mut v = cx.u.alloc_block(16).unwrap();
                v.tag = k;
                match cx.send(&client, v, true) {
                    Sent::Accepted => {}
                    Sent::Waiting(w) => settle(&mut cx, w),
                    other => panic!("send {k}: {other:?}"),
                }
            }
            cx.u.drop_client(client);
            cx.obs("drop_client", "");
            assert_eq!(cx.used(), 0, "the sender's root kept a charge");
            served(1, "unit 1's sends all settled and its root is back to zero");
        }
    }
    0
}

/// Wait for waiting send `w` to settle `Accepted`, taking what comes first.
fn settle(cx: &mut Cx, w: u32) {
    loop {
        match cx.wait() {
            Took::Accepted(i) if i == w => return,
            Took::Closed(i) | Took::Withdrawn(i) if i == w => panic!("send {w} settled otherwise"),
            _ => {}
        }
    }
}

/// Seal, take the seal answer, clean, and take the last release; the root
/// must then be back to zero. Returns the number of items cleaned.
fn retire(cx: &mut Cx, inbox: &Inbox) -> u64 {
    cx.u.seal(inbox);
    cx.obs("seal", "");
    while cx.wait() != Took::SealAnswer {}
    let cleaned = inbox.0.side().taken.len() as u64;
    let causes = cx.u.clean(inbox);
    assert!(causes.is_empty());
    cx.obs("clean", cleaned);
    while cx.wait() != Took::LastRelease {}
    assert!(
        inbox.0.side().taken.is_empty(),
        "an item came in after the cleaning"
    );
    assert_eq!(cx.used(), 0, "the owner's root kept a charge");
    cleaned
}

/// 2. A seal against a send accepted but not yet published. Unit 1 tries
/// `ITEMS` sends with pauses between them; unit 0 seals at a random moment,
/// receiving nothing, then cleans. An item whose slot was taken before the
/// seal but whose publish comes after it must still come in before the seal
/// answer, or the cleaning misses it: the seal counts the occupied slots that
/// have not come in, and the answer waits for them.
pub(super) fn seal_in_flight(id: u32) -> u8 {
    match id {
        0 => {
            let mut cx = Cx::root(0, 4096);
            let (inbox, client) = cx.u.conn_new(8, 8, 256).unwrap();
            cx.obs("conn_new", "");
            CLIENT.put(client);
            FLAG[0].store(true, Ordering::SeqCst);
            wake_id(1);
            compute_until(now() + param(0));
            let cleaned = retire(&mut cx, &inbox);
            assert_eq!(
                cleaned,
                ACCEPTED.load(Ordering::SeqCst),
                "an accepted item was not cleaned exactly once"
            );
            served(
                0,
                "unit 0 cleaned every accepted item and its root is back to zero",
            );
        }
        _ => {
            wait_flag(0);
            let client = CLIENT.take();
            let mut cx = Cx::root(1, 4096);
            let mut accepted = 0;
            for k in 1..=ITEMS {
                compute_until(now() + draw(0, param(1)));
                let mut v = cx.u.alloc_block(16).unwrap();
                v.tag = k;
                match cx.send(&client, v, false) {
                    Sent::Accepted => accepted += 1,
                    Sent::Closed(v) => cx.drop_value(v),
                    other => panic!("send {k}: {other:?}"),
                }
            }
            ACCEPTED.store(accepted, Ordering::SeqCst);
            cx.u.drop_client(client);
            cx.obs("drop_client", "");
            assert_eq!(cx.used(), 0, "the sender's root kept a charge");
            served(1, "unit 1's sends all decided and its root is back to zero");
        }
    }
    0
}

/// Two publishers and a taker share a connection. An item stays in its
/// slot while the owner reads it, even when the other publisher can admit.
pub(super) fn slot_reuse(id: u32) -> u8 {
    reuse(id, false)
}

/// A publisher may reserve another slot while the receiver is copying. The
/// old slot's flat bytes must remain reserved until the copy is complete.
pub(super) fn flat_slot_reuse(id: u32) -> u8 {
    reuse(id, true)
}

fn reuse(id: u32, flat: bool) -> u8 {
    if id == 0 {
        let mut cx = Cx::root(0, 4096);
        let cap = if param(1) == 0 { 2 } else { 23 };
        cx.ev(format!("I connection capacity {cap}"));
        let (inbox, client) = if flat {
            cx.u.conn_new_cross(cap, 4, 64, 4, &[])
        } else {
            cx.u.conn_new(cap, 8, 64)
        }
        .unwrap();
        cx.obs("conn_new", "");
        OTHER_CLIENT.put(client.clone_client());
        CLIENT.put(client);
        FLAG[0].store(true, Ordering::SeqCst);
        wake_id(1);
        wake_id(2);
        let mut next = [1, ITEMS + 1];
        loop {
            let (tag, value) = if flat {
                let tag = inbox.front_tag();
                let mut item = 0;
                let got = inbox.receive_cross(|bytes, src, _, _| {
                    assert_eq!(bytes, 0);
                    // SAFETY: the connection reserved this slot for this take.
                    item = u64::from(unsafe { src.cast::<u32>().read_unaligned() });
                    Ok::<_, ()>(())
                });
                cx.obs(
                    "recv",
                    got.map_or("Empty".to_string(), |r| {
                        r.unwrap();
                        format!("Ok {item}")
                    }),
                );
                if got.is_some() {
                    assert_eq!(Some(item), tag, "slot reuse changed the flat bytes");
                    (item, None)
                } else if inbox.0.side().closed {
                    break;
                } else {
                    cx.wait();
                    continue;
                }
            } else {
                match cx.recv(&inbox) {
                    Received::Ok(v) => (v.tag, Some(v)),
                    Received::Empty => {
                        cx.wait();
                        continue;
                    }
                    Received::Closed => break,
                    Received::Capacity(c) => panic!("receive refused: {c:?}"),
                }
            };
            assert!((1..=2 * ITEMS).contains(&tag), "slot lost its value");
            let sender = ((tag - 1) / ITEMS) as usize;
            assert_eq!(tag, next[sender], "slot reuse changed a sender's item");
            next[sender] += 1;
            if let Some(v) = value {
                cx.drop_value(v);
            }
        }
        assert_eq!(next, [ITEMS + 1, 2 * ITEMS + 1], "slot reuse lost an item");
        retire(&mut cx, &inbox);
        served(0, "both streams received in order");
    } else {
        wait_flag(0);
        let client = if id == 1 {
            CLIENT.take()
        } else {
            OTHER_CLIENT.take()
        };
        let mut cx = Cx::root(id as usize, 4096);
        for k in 1..=ITEMS {
            compute_until(now() + draw(0, param(0)));
            let tag = (id as u64 - 1) * ITEMS + k;
            let word = tag as u32;
            let sent = if flat {
                let mut source = CrossSend::new((&raw const word).cast());
                source.tag = tag;
                // SAFETY: the source stays live through the settlement wait below.
                let sent = unsafe { UnitCtx::send_cross(&client, source, 0, true, || &mut cx.u) };
                cx.sent(&sent);
                sent
            } else {
                let mut v = cx.u.alloc_block(16).unwrap();
                v.tag = tag;
                cx.send(&client, v, true)
            };
            match sent {
                Sent::Accepted => {}
                Sent::Waiting(send) => assert_eq!(cx.wait(), Took::Accepted(send)),
                other => panic!("send {k}: {other:?}"),
            }
        }
        cx.u.drop_client(client);
        cx.obs("drop_client", "");
        assert_eq!(cx.used(), 0, "the sender's root kept a charge");
        served(id as usize, "all items sent");
    }
    0
}

/// 3. A credit to a helper against the owner's end take. Unit 0 spawns a
/// helper, which runs on unit 1: it makes a shared node (charged to its own
/// root), hands the only share to unit 2, takes what has reached it and ends.
/// Unit 2 drops the share at a random moment, so the node's credit goes to
/// the helper's port before the helper ends, between its end and the owner's
/// end take, or after it. The end take installs the forward marker and
/// collects the port in one exchange, so a credit published in between is
/// forwarded, not stranded; the owner's root must come back to zero.
pub(super) fn helper_forward(id: u32) -> u8 {
    match id {
        0 => {
            let mut cx = Cx::root(0, 8192);
            let (_, h, _) = cx.u.spawn_helper(512, 0, None).map_err(|e| e.0).unwrap();
            cx.obs("spawn_helper", "");
            HELPER.put(h);
            FLAG[0].store(true, Ordering::SeqCst);
            wake_id(1);
            while cx.wait() != Took::HelperEnd {}
            while cx.used() != 0 {
                cx.wait();
            }
            served(0, "unit 0 took its helper's end and every credit");
        }
        1 => {
            wait_flag(0);
            let mut cx = Cx::on(1, HELPER.take());
            let s = cx.u.make_shared(64, None).map_err(|e| e.0).unwrap();
            cx.obs("make_shared", "");
            SHARE.put(s);
            FLAG[1].store(true, Ordering::SeqCst);
            wake_id(2);
            compute_until(now() + draw(0, param(0)));
            while let Some((_, a)) = cx.u.take() {
                cx.took(&a);
            }
            let _ = cx.u.end_helper(None, false);
            served(1, "the helper ended");
        }
        _ => {
            wait_flag(1);
            let s = SHARE.take();
            let mut cx = Cx::root(2, 1024);
            compute_until(now() + draw(0, param(1)));
            cx.u.drop_shared(s);
            cx.obs("drop_shared", "");
            served(2, "unit 2 dropped the last share");
        }
    }
    0
}

pub(super) fn params_wait_serve(r: &mut dyn FnMut(u64, u64) -> u64) {
    PARAM[0].store(r(0, 2_000), Ordering::Relaxed); // longest pause before a receive
}
pub(super) fn params_seal_in_flight(r: &mut dyn FnMut(u64, u64) -> u64) {
    PARAM[0].store(r(0, 4_000), Ordering::Relaxed); // the owner seals after this
    PARAM[1].store(r(0, 1_000), Ordering::Relaxed); // longest pause between sends
}
pub(super) fn params_helper_forward(r: &mut dyn FnMut(u64, u64) -> u64) {
    PARAM[0].store(r(0, 3_000), Ordering::Relaxed); // the helper works before ending
    PARAM[1].store(r(0, 6_000), Ordering::Relaxed); // unit 2 drops its share after this
}

/// An edge queued before the send belongs to the receiving registration.
#[test]
fn socket_crosses_pending_readiness() {
    use hirsel_core_net::Sockets;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixStream;

    let (socket, mut peer) = UnixStream::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    let mut sender = Sockets::default();
    let key = sender.insert(socket.into()).unwrap();
    let stale = sender.arm(key, false, 1).unwrap();
    sender.disarm(key, false);
    let source = sender.poller().unwrap();
    sender.dispatch_polled(source.poll(0).unwrap().unwrap(), |_| unreachable!());
    peer.write_all(b"x").unwrap();

    let moving = sender.detach(key).unwrap();
    let old = source.poll(0).unwrap().unwrap();
    assert_eq!(sender.dispatch_polled(old, |_| unreachable!()), 0);

    let (replacement, _peer) = UnixStream::pair().unwrap();
    let replacement = sender.insert(replacement.into()).unwrap();
    sender.arm(replacement, false, 2).unwrap();
    sender.dispatch(stale, libc::EPOLLIN as u32, |_| {
        panic!("stale readiness reached the sender")
    });

    let mut receiver = Sockets::default();
    let key = receiver.attach(moving).unwrap();
    receiver.arm(key, false, 3).unwrap();
    let mut ready = Vec::new();
    receiver.poll(0, |token| ready.push(token)).unwrap();
    assert_eq!(ready, [3]);
    // SAFETY: dup creates a new descriptor, which the test alone owns.
    let fd = unsafe { libc::dup(receiver.fd(key)) };
    assert!(fd >= 0);
    let mut socket = unsafe { UnixStream::from_raw_fd(fd) };
    let mut data = [0];
    socket.read_exact(&mut data).unwrap();
    assert_eq!(data, *b"x");
}
