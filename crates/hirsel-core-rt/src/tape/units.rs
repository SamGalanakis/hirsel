//! Unit recordings: take points and their `TAKE` frames, the unit operation
//! frames, and the object ordinals of connections, clients, helper handles
//! and `Shared` nodes (docs/recording.md, "Unit recordings").
use super::schedule::Group;
use super::*;
use crate::net::CrossingSockets;
use crate::units_lang::Cross;
use format::{TakeBody, TakePoint};
use hirsel_core_layout::cross::{CAPABILITY, CLIENT, MEDIA, SECRET, SHARED, SOCKET};
use item_grants::Granted;
use std::collections::HashMap;

pub(crate) mod crossing;
pub(crate) mod item_grants;
pub(crate) mod replay;
pub(crate) mod verdicts;

/// One unit's take-point count and the ordinals of its unit objects.
#[derive(Default)]
pub(super) struct Takes {
    /// In-operation take-point positions reached since the unit's last event
    /// or non-`TAKE` frame, which `events` and `position` name.
    count: u64,
    events: u64,
    position: u64,
    /// Client ordinal by handle address.
    pub(super) clients: HashMap<usize, u64>,
    /// `Shared` ordinal by node address: its `shared.new`, or its latest
    /// arrival in this unit.
    pub(super) nodes: HashMap<usize, u64>,
    /// Helper handle ordinal by the handle's key.
    helpers: HashMap<usize, u64>,
    /// Each waiting send: its `ISSUE` position and the item as a value.
    sends: HashMap<u32, (u64, Vec<u8>)>,
    /// The unit's seals and unwraps so far: each designation's ordinal in
    /// the unit's own order (recording.md, "Designations across units").
    seals: u64,
    unwraps: u64,
    /// The key object and registry entry charges the unit tracked so far.
    made: u64,
}

/// What every unit of the run knows of a connection made while recording: the
/// stream that made it and its ordinal there, the item's layout, and its
/// retained charge once cleaned. Keyed by the connection's address; a newer
/// connection at the same address replaces the entry. A replay's stand-in
/// for a client that arrived from another unit has an entry of its own,
/// naming the connection it stands for, which no take of that stream finds.
struct Conn {
    stream: u32,
    ordinal: u64,
    cap: u64,
    room: u64,
    logical: u64,
    bytes: u64,
    owners: Vec<[u64; 4]>,
    header: u64,
    retained: u64,
    stand_in: bool,
}

static CONNS: Mutex<Option<HashMap<usize, Conn>>> = Mutex::new(None);

/// What every unit of the run knows of a `Shared` node made while recording:
/// its home stream and its ordinal there, and its payload's layout. Keyed by
/// the node's address; a newer node at the same address replaces the entry.
struct Node {
    stream: u32,
    ordinal: u64,
    bytes: u64,
    owners: Vec<[u64; 4]>,
}

static NODES: Mutex<Option<HashMap<usize, Node>>> = Mutex::new(None);

fn nodes<R>(f: impl FnOnce(&mut HashMap<usize, Node>) -> R) -> R {
    f(NODES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new))
}

/// `shared.new`: the node's ordinal in its home stream, and its payload's
/// layout for the snapshots that carry it out.
pub(crate) fn shared_new(group: Option<Group>, node: usize, payload: &Cross) {
    recording(group, |live| {
        let ordinal = live.unit_object();
        live.takes.nodes.insert(node, ordinal);
        let entry = Node {
            stream: stream(live),
            ordinal,
            bytes: payload.bytes,
            owners: payload.owners().to_vec(),
        };
        nodes(|n| n.insert(node, entry));
    });
}

/// A node's payload layout: its flat bytes and owner rows.
pub(super) fn node_layout(node: u64) -> Option<(u64, Vec<[u64; 4]>)> {
    nodes(|n| n.get(&(node as usize)).map(|e| (e.bytes, e.owners.clone())))
}

/// A `Shared` snapshot up to its payload, which the value writer appends:
/// kind 1, the holder's ordinal here, the home stream and the node's ordinal
/// there.
pub(super) fn shared_snapshot(node: u64, ordinal: u64) -> Result<Vec<u8>, &'static str> {
    let (stream, of) =
        nodes(|n| n.get(&(node as usize)).map(|e| (e.stream, e.ordinal))).ok_or(UNNAMED_NODE)?;
    let mut out = vec![1];
    put_varint(&mut out, ordinal);
    out.extend_from_slice(&stream.to_le_bytes());
    put_varint(&mut out, of);
    Ok(out)
}

pub(super) const UNNAMED_NODE: &str = "a Shared node the recording did not name";

fn conns<R>(f: impl FnOnce(&mut HashMap<usize, Conn>) -> R) -> R {
    f(CONNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new))
}

impl Live {
    fn recording(&self) -> bool {
        self.mode == Mode::Record && self.fault.is_none() && self.recorder.is_some()
    }

    /// Start a new count when an event or another frame came since the last:
    /// one written, or one a replay took.
    fn fresh_takes(&mut self) {
        let position = match (&self.recorder, &self.replayer) {
            (Some(r), _) => r.encoder.position(),
            (None, Some(r)) => r.taken,
            (None, None) => 0,
        };
        let t = &mut self.takes;
        if t.events != self.schedule.events || t.position != position {
            t.count = 0;
            t.events = self.schedule.events;
            t.position = position;
        }
    }

    pub(super) fn unit_object(&mut self) -> u64 {
        self.objects.push(Object {
            inner: None,
            open_resource: false,
        });
        self.objects.len() as u64
    }

    fn take(&mut self, point: TakePoint, body: Vec<u8>) {
        self.fresh_takes();
        let gap = self.schedule.events - self.schedule.last;
        self.schedule.last = self.schedule.events;
        let count = self.takes.count;
        let rec = self.recorder.as_mut().unwrap();
        match TakeBody::decode(body) {
            Ok(body) => {
                let _ = rec.frame(&Frame::Take {
                    gap,
                    point,
                    count,
                    body,
                });
            }
            Err(_) => rec.cut("a take whose body does not decode"),
        }
        self.takes.position = rec.encoder.position();
    }

    fn cut(&mut self, why: &'static str) {
        self.recorder.as_mut().unwrap().cut(why);
    }

    /// A unit operation's commit, as a host operation's.
    fn unit_commit(&mut self, task: u64) {
        let position = self.schedule.events;
        self.commit(task, crate::tasks::group_index);
        self.schedule.last = position;
    }

    fn unit_op(&mut self, task: u64, op: u8, object: u64, args: Args, value: u64) {
        self.unit_commit(task);
        let out = Out {
            status: OK,
            value,
            ..Out::default()
        };
        let frame = Frame::Op {
            task,
            op,
            object,
            args,
            out,
        };
        let _ = self.recorder.as_mut().unwrap().frame(&frame);
        self.schedule.events += 1;
    }
}

fn recording<R>(group: Option<Group>, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
    group?
        .with(|live| live.recording().then(|| f(live)))
        .flatten()
}

/// What a unit's recording and its replay both number: its object ordinals
/// and the items its sends hold.
fn noting<R>(group: Option<Group>, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
    group?
        .with(|live| (live.recording() || live.replaying()).then(|| f(live)))
        .flatten()
}

/// The unit's stream number: its recorder's, or the stream its replay reads.
pub(super) fn stream(live: &Live) -> u32 {
    match (&live.recorder, &live.replayer) {
        (Some(r), _) => r.stream,
        (None, Some(r)) => r.stream,
        (None, None) => 0,
    }
}

/// The running unit's stream: 0 for the root or no tape.
pub(crate) fn current_stream() -> u32 {
    crate::tasks::current_group()
        .and_then(|group| group.with(|live| stream(live)))
        .unwrap_or(0)
}

/// The running unit's stream and its next seal (or unwrap) ordinal, counted
/// in record and replay alike, so a replay designates as its recording did.
pub(crate) fn next_designation(unwrap: bool) -> (u32, u64) {
    let group = crate::tasks::current_group();
    group
        .and_then(|group| {
            group.with(|live| {
                let stream = stream(live);
                let count = if unwrap {
                    &mut live.takes.unwraps
                } else {
                    &mut live.takes.seals
                };
                *count += 1;
                (stream, *count - 1)
            })
        })
        .unwrap_or((0, 0))
}

/// The running unit's stream and the number of its next tracked key object
/// or registry entry charge, counted in record and replay alike: the name
/// of its helper-end survivor (recording.md, "Unit stream format").
pub(crate) fn next_made() -> Option<(u32, u64)> {
    crate::tasks::current_group()?.with(|live| {
        live.takes.made += 1;
        (stream(live), live.takes.made - 1)
    })
}

/// The unit's code reached an in-operation take point, counted in record
/// and replay alike.
pub(crate) fn reach(group: Option<Group>) {
    let _ = replay::replaying(group, |live| {
        live.fresh_takes();
        live.takes.count += 1;
    });
    recording(group, |live| {
        live.fresh_takes();
        live.takes.count += 1;
    });
}

thread_local! {
    /// Set while the scheduler binds a task: its drain is a pump's (kind 1).
    static BINDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run the scheduler's drain at a task's bind, whose takes are kind 1.
pub(crate) fn at_bind(drain: impl FnOnce()) {
    BINDING.set(true);
    drain();
    BINDING.set(false);
}

/// The unit posting a refund now: its recording cell, if it has one.
pub(crate) fn poster() -> Option<usize> {
    crate::tasks::current_group().map(|g| ptr::from_ref(g.cell) as usize)
}

/// A refund another unit posted, taken at this unit's drain: the drained
/// limit's ordinal and the amount (tag 9). Kind 0 at an in-operation drain,
/// whose position the drain site reached; kind 1 at a task's bind.
pub(crate) fn refund(from: usize, ordinal: u64, bytes: u64) {
    let group = crate::tasks::current_group();
    if group.is_some_and(|g| ptr::from_ref(g.cell) as usize == from) {
        // Another task's post in this unit: the replay recomputes it.
        return;
    }
    recording(group, |live| {
        let mut body = vec![9];
        put_varint(&mut body, ordinal);
        body.extend_from_slice(&bytes.to_le_bytes());
        live.take(point(BINDING.get()), body);
    });
}

fn point(pump: bool) -> TakePoint {
    if pump {
        TakePoint::Pump
    } else {
        TakePoint::Operation
    }
}

/// `conn.new`: the connection's ordinal, then its first client's.
pub(crate) fn conn_new(
    group: Option<Group>,
    conn: usize,
    client: usize,
    item: &Cross,
    (cap, room, logical): (u64, u64, u64),
) {
    noting(group, |live| {
        let ordinal = live.unit_object();
        let entry = Conn {
            stream: stream(live),
            ordinal,
            cap,
            room,
            logical,
            bytes: item.bytes,
            owners: item.owners().to_vec(),
            header: 0,
            retained: 0,
            stand_in: false,
        };
        conns(|c| c.insert(conn, entry));
        let client_ordinal = live.unit_object();
        live.takes.clients.insert(client, client_ordinal);
    });
}

/// `conn.client`: a new client ordinal.
pub(crate) fn client_new(group: Option<Group>, client: usize) {
    noting(group, |live| {
        let ordinal = live.unit_object();
        live.takes.clients.insert(client, ordinal);
    });
}

/// A drop's `CLOSE`: written, or in a replay checked.
fn close_frame(live: &mut Live, object: u64) {
    let args = Args::Literal(Vec::new());
    if live.replaying() {
        live.closed(object, &args);
        return;
    }
    let frame = Frame::Op {
        task: 0,
        op: op::CLOSE,
        object,
        args,
        out: Out::default(),
    };
    let _ = live.recorder.as_mut().unwrap().frame(&frame);
}

/// A client's drop: its `CLOSE`.
pub(crate) fn client_drop(group: Option<Group>, client: usize) {
    noting(group, |live| {
        if let Some(ordinal) = live.takes.clients.remove(&client) {
            close_frame(live, ordinal);
        }
    });
}

/// An inbox's drop: its `CLOSE`.
pub(crate) fn inbox_drop(group: Option<Group>, conn: usize) {
    noting(group, |live| {
        let own = stream(live);
        if let Some(ordinal) = conns(|c| {
            c.get(&conn)
                .filter(|e| e.stream == own && !e.stand_in)
                .map(|e| e.ordinal)
        }) {
            close_frame(live, ordinal);
        }
    });
}

/// A cleaned connection's retained charge, which its last release names.
pub(crate) fn retained(conn: usize, (header, retained): (u64, u64)) {
    conns(|c| {
        if let Some(e) = c.get_mut(&conn) {
            e.header = header;
            e.retained = retained;
        }
    });
}

/// The capability and `Secret` words of an item, by row kind, in the order
/// its value holds them: what `item_grants::resolve` reads before any cell
/// is locked.
/// # Safety
/// `item` holds the connection's flat item; `content` its packed sequences.
pub(crate) unsafe fn item_words(
    conn: usize,
    item: *const u8,
    content: Option<*const u8>,
) -> Vec<(u64, u64)> {
    let Some((bytes, owners)) = layout(conn) else {
        return Vec::new();
    };
    let x = Cross {
        bytes,
        owners: owners.as_ptr(),
        count: owners.len() as u64,
        at: 0,
        len: bytes,
    };
    // SAFETY: by the contract.
    unsafe { helper::capability_words(item, &x, content) }
}

/// A send's item as a value, from the sender's live item; with the client's
/// ordinal, or why the send is not recorded. `grants` are its capability
/// words' grants.
/// # Safety
/// `item` is the client's connection's initialized item.
pub(crate) unsafe fn send_item(
    group: Option<Group>,
    client: usize,
    conn: usize,
    item: *const u8,
    grants: &[Granted],
    sockets: Option<&CrossingSockets>,
) -> Option<Result<(u64, Vec<u8>), &'static str>> {
    noting(group, |live| {
        let Some(&ordinal) = live.takes.clients.get(&client) else {
            return Err(UNNAMED);
        };
        let layout = layout(conn).ok_or(UNNAMED)?;
        // The canonical form leaves out each snapshot's ordinal; a returned
        // item names the sender's own clients.
        let mut handle = |kind, word: u64| match kind {
            CLIENT => client_snapshot(
                word,
                live.takes
                    .clients
                    .get(&(word as usize))
                    .copied()
                    .unwrap_or(0),
            ),
            SHARED => shared_snapshot(
                word,
                live.takes.nodes.get(&(word as usize)).copied().unwrap_or(0),
            ),
            CAPABILITY | SECRET => item_grants::sent(grants, kind, word),
            // SAFETY: the caller pins and owns the sending item.
            MEDIA => unsafe { crate::media::snapshot(word, None) },
            SOCKET => crossing::sent(sockets, word, None),
            _ => Err(CAPABILITY_ITEM),
        };
        // SAFETY: by the contract.
        unsafe { item_value(&layout, item, None, &mut handle) }.map(|v| (ordinal, v))
    })
}

/// A connection's item layout: its flat bytes and owner rows.
fn layout(conn: usize) -> Option<(u64, Vec<[u64; 4]>)> {
    conns(|c| c.get(&conn).map(|e| (e.bytes, e.owners.clone())))
}

/// A client's snapshot as a value part holds it after its tag: kind 0, the
/// ordinal, the stream that made its connection and the connection's ordinal
/// there, then `cap`, room `b`, the item's logical size, its flat bytes and
/// its owner rows, from which a replay builds the client's stand-in.
pub(super) fn client_snapshot(word: u64, ordinal: u64) -> Result<Vec<u8>, &'static str> {
    let conn = crate::units_lang::takes::client_conn(word);
    conns(|c| {
        let e = c.get(&conn).ok_or(UNNAMED)?;
        let mut out = vec![0];
        put_varint(&mut out, ordinal);
        out.extend_from_slice(&e.stream.to_le_bytes());
        put_varint(&mut out, e.ordinal);
        for word in [e.cap, e.room, e.logical, e.bytes] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        put_varint(&mut out, e.owners.len() as u64);
        for row in &e.owners {
            for &word in row {
                put_varint(&mut out, word);
            }
        }
        Ok(out)
    })
}

/// A replay's stand-in for a client that arrived from another unit: its own
/// connection `conn`, with the layout of the connection it stands for, and
/// its client `client`, numbered `ordinal` in the receiving unit.
pub(super) fn stand_in(live: &mut Live, conn: usize, client: usize, ordinal: u64, of: StandIn) {
    live.takes.clients.insert(client, ordinal);
    let entry = Conn {
        stream: of.stream,
        ordinal: of.ordinal,
        cap: of.cap,
        room: of.room,
        logical: of.logical,
        bytes: of.bytes,
        owners: of.owners,
        header: 0,
        retained: 0,
        stand_in: true,
    };
    conns(|c| c.insert(conn, entry));
}

/// The connection a client snapshot names, with its item's layout.
pub(super) struct StandIn {
    pub(super) stream: u32,
    pub(super) ordinal: u64,
    pub(super) cap: u64,
    pub(super) room: u64,
    pub(super) logical: u64,
    pub(super) bytes: u64,
    pub(super) owners: Vec<[u64; 4]>,
}

const UNNAMED: &str = "a send on a client the recording did not name";

/// # Safety
/// `item` holds the connection's flat item; `content` its packed sequences.
unsafe fn item_value(
    (bytes, owners): &(u64, Vec<[u64; 4]>),
    item: *const u8,
    content: Option<*const u8>,
    handle: &mut dyn FnMut(u64, u64) -> Result<Vec<u8>, &'static str>,
) -> Result<Vec<u8>, &'static str> {
    let x = Cross {
        bytes: *bytes,
        owners: owners.as_ptr(),
        count: owners.len() as u64,
        at: 0,
        len: *bytes,
    };
    // SAFETY: forwarded.
    unsafe { helper::value(item, &x, content, handle) }
}

const CAPABILITY_ITEM: &str = "an item holds a handle whose snapshot this build does not write";

/// A handle that arrives in a value, an item, a helper's entry or its result,
/// as the receiving unit's snapshot: a client or `Shared` holder takes a new
/// ordinal at its take, a capability its grant's, designated in
/// `designations` once the value is written, and a `Secret` a new ordinal
/// with its designation unchanged, a TCP socket or listener a new ordinal,
/// which its attach in the unit at cell `at` names (`sockets` carries it).
#[allow(clippy::too_many_arguments)]
pub(super) fn arrive(
    live: &mut Live,
    at: usize,
    generation: u32,
    grants: &[Granted],
    designations: &mut Vec<item_grants::Designation>,
    sockets: Option<&CrossingSockets>,
    kind: u64,
    word: u64,
) -> Result<Vec<u8>, &'static str> {
    match kind {
        CLIENT => {
            let o = live.unit_object();
            live.takes.clients.insert(word as usize, o);
            client_snapshot(word, o)
        }
        SHARED => {
            let o = live.unit_object();
            live.takes.nodes.insert(word as usize, o);
            shared_snapshot(word, o)
        }
        CAPABILITY | SECRET => {
            item_grants::arrived(live, generation, grants, kind, word, designations)
        }
        MEDIA => {
            let ordinal = live.unit_object();
            // SAFETY: the arriving value transfers exclusive ownership.
            unsafe { crate::media::snapshot(word, Some(ordinal)) }
        }
        SOCKET => crossing::arrived(live, at, generation, sockets, word),
        _ => Err(CAPABILITY_ITEM),
    }
}

/// A send, or why it is not recorded: `CONN_SEND` or `CONN_TRY_SEND` as an
/// `OP` with its outcome, or a waiting `CONN_SEND`'s `ISSUE` (`waiting` its id).
pub(crate) fn send(
    group: Option<Group>,
    op: u8,
    item: Result<(u64, Vec<u8>), &'static str>,
    outcome: Option<u64>,
    waiting: Option<u32>,
) {
    recording(group, |live| {
        let (object, value) = match item {
            Ok(item) => item,
            Err(why) => return live.cut(why),
        };
        let canonical = format::Value::decode(&value)
            .map(|v| v.canonical())
            .unwrap_or_default();
        let args = Args::of_parts(&[&canonical]);
        let task = group.map_or(0, |g| g.task);
        if let Some(value) = outcome {
            live.unit_op(task, op, object, args, value);
            return;
        }
        let Some(id) = waiting else { return };
        live.unit_commit(task);
        let frame = Frame::Issue {
            task,
            op,
            object,
            args,
        };
        let rec = live.recorder.as_mut().unwrap();
        let _ = rec.frame(&frame);
        let position = rec.encoder.position();
        live.takes.sends.insert(id, (position, value));
    });
}

/// `UNIT_SPAWN`: the helper handle's ordinal; the `OP` names its stream.
pub(crate) fn spawn(group: Option<Group>, key: usize, helper_stream: u32, args: Option<&[u8]>) {
    recording(group, |live| {
        let Some(args) = args else {
            return live.cut("a spawn whose helper's entry is not recorded");
        };
        let args = Args::of_parts(&[args]);
        let ordinal = live.unit_object();
        live.takes.helpers.insert(key, ordinal);
        let task = group.map_or(0, |g| g.task);
        live.unit_op(task, op::UNIT_SPAWN, 0, args, u64::from(helper_stream));
    });
}

/// `UNIT_CANCEL` on a helper handle.
pub(crate) fn cancel(group: Option<Group>, key: usize) {
    recording(group, |live| {
        if let Some(&ordinal) = live.takes.helpers.get(&key) {
            let task = group.map_or(0, |g| g.task);
            live.unit_op(task, op::UNIT_CANCEL, ordinal, Args::Literal(Vec::new()), 0);
        }
    });
}

/// The arrivals a language unit takes, as its `TAKE` bodies need them.
pub(crate) enum Arrival<'a> {
    Item {
        conn: usize,
        flat: *const u8,
        content: *const u8,
        /// Its capability words' grants, and the receiving unit's registry,
        /// which designates each on this unit's cell.
        grants: &'a [Granted],
        registry: *mut crate::host_file::Registry,
        /// The item's sockets in transit.
        sockets: Option<&'a CrossingSockets>,
    },
    Close(usize),
    SealAnswer(usize),
    /// A waiting send's settlement: 0 `Accepted`, 1 `Closed`, 2 `Withdrawn`.
    Settlement(u32, u8),
    HelperEnd {
        key: usize,
        outcome: &'a Result<super::helper::EndTake, &'static str>,
        /// The owner's registry, where a capability in the result is
        /// designated: reached only from the owner's thread.
        registry: *mut crate::host_file::Registry,
        /// Kept connections and `Shared` nodes by address, with their amounts.
        survivors: &'a [(usize, u64)],
        /// Kept `Secret` designations, sealed bytes and UDP pools by name:
        /// kind, origin kind, stream, ordinal, amount.
        charges: &'a [(u8, Option<u8>, u32, u64, u64)],
        /// Why other survivors cannot be named, if there are any.
        unnamed: Option<&'static str>,
    },
    LastRelease(usize),
    /// A `Shared` node's last release, by the node's address.
    SharedRelease(usize),
    Cancel,
    /// What this build does not take into a recording.
    Unrecorded(&'static str),
}

/// A unit's cell as a socket's arrival names it.
fn cell_at(group: Option<Group>) -> usize {
    group.map_or(0, |g| g.cell.provider.state as usize)
}

/// One `TAKE`, at a scheduler pump or the in-operation point last reached.
/// # Safety
/// An item's `flat` and `content` are its connection's live packed item, and
/// its `registry` the receiving unit's, reached only from that unit's thread.
pub(crate) unsafe fn take(group: Option<Group>, pump: bool, arrival: Arrival<'_>) {
    recording(group, |live| {
        let own = stream(live);
        let ordinal = |conn: usize| {
            conns(|c| {
                c.get(&conn)
                    .filter(|e| e.stream == own && !e.stand_in)
                    .map(|e| e.ordinal)
            })
        };
        let mut body = Vec::new();
        match arrival {
            Arrival::Item {
                conn,
                flat,
                content,
                grants,
                registry,
                sockets,
            } => {
                let generation = group.map_or(0, |g| g.generation);
                let at = cell_at(group);
                let mut designations = Vec::new();
                let value = ordinal(conn).zip(layout(conn)).map(|(ordinal, layout)| {
                    let mut handle = |kind, word: u64| {
                        arrive(
                            live,
                            at,
                            generation,
                            grants,
                            &mut designations,
                            sockets,
                            kind,
                            word,
                        )
                    };
                    // SAFETY: by the contract.
                    (ordinal, unsafe {
                        item_value(&layout, flat, Some(content), &mut handle)
                    })
                });
                match value {
                    Some((ordinal, Ok(value))) => {
                        body.push(0);
                        put_varint(&mut body, ordinal);
                        body.extend(value);
                        if let Some(group) = group.filter(|_| !designations.is_empty()) {
                            // SAFETY: the receiving unit's own registry, which
                            // only its thread reaches (the arrival's contract).
                            item_grants::designate(group, unsafe { &mut *registry }, &designations);
                        }
                    }
                    Some((_, Err(why))) => return live.cut(why),
                    None => return live.cut(UNNAMED_INBOX),
                }
            }
            Arrival::Close(conn) | Arrival::SealAnswer(conn) => {
                let Some(ordinal) = ordinal(conn) else {
                    return live.cut(UNNAMED_INBOX);
                };
                body.push(if matches!(arrival, Arrival::Close(_)) {
                    1
                } else {
                    2
                });
                put_varint(&mut body, ordinal);
            }
            Arrival::Settlement(id, outcome) => {
                let Some((position, value)) = live.takes.sends.remove(&id) else {
                    return;
                };
                body.push(3);
                put_varint(&mut body, position);
                body.push(outcome);
                if outcome == 1 {
                    body.extend(value);
                }
            }
            Arrival::HelperEnd {
                key,
                outcome,
                registry,
                survivors,
                charges,
                unnamed,
            } => {
                let Some(ordinal) = live.takes.helpers.remove(&key) else {
                    return;
                };
                if let Some(why) = unnamed {
                    return live.cut(why);
                }
                let end = match outcome {
                    Ok(end) => end,
                    Err(why) => return live.cut(why),
                };
                body.push(4);
                put_varint(&mut body, ordinal);
                body.push(end.kind);
                if end.handles.is_empty() {
                    body.extend_from_slice(&end.body);
                } else {
                    // Each handle in the result takes its ordinal here, in the
                    // order the helper wrote them, each 0 in its END.
                    let generation = group.map_or(0, |g| g.generation);
                    let mut designations = Vec::new();
                    let mut ordinals = Vec::with_capacity(end.handles.len());
                    let at = cell_at(group);
                    for &(kind, word) in &end.handles {
                        if let Err(why) = arrive(
                            live,
                            at,
                            generation,
                            &end.grants,
                            &mut designations,
                            end.sockets.as_ref(),
                            kind,
                            word,
                        ) {
                            return live.cut(why);
                        }
                        ordinals.push(live.objects.len() as u64);
                    }
                    let value = format::Value::decode(&end.body).expect("a written result");
                    body.extend(value.with_ordinals(&ordinals));
                    if let Some(group) =
                        group.filter(|_| !designations.is_empty() && !registry.is_null())
                    {
                        // SAFETY: the owner's own registry (the arrival's contract).
                        item_grants::designate(group, unsafe { &mut *registry }, &designations);
                    }
                }
                // Each survivor by its address: a `Shared` node (kind 1),
                // whose entry ends at its release, else a connection (kind
                // 0); then the named charges. Written in (kind, origin kind,
                // stream, ordinal, amount) order, which no address decides.
                let mut named = Vec::with_capacity(survivors.len() + charges.len());
                for &(key, amount) in survivors {
                    let node = nodes(|n| n.get(&key).map(|e| (1u8, e.stream, e.ordinal)));
                    let conn = || conns(|c| c.get(&key).map(|e| (0u8, e.stream, e.ordinal)));
                    let Some((kind, stream, ordinal)) = node.or_else(conn) else {
                        return live.cut(UNNAMED_SURVIVOR);
                    };
                    named.push((kind, None, stream, ordinal, amount));
                }
                named.extend_from_slice(charges);
                named.sort_unstable();
                put_varint(&mut body, named.len() as u64);
                for (kind, origin, stream, ordinal, amount) in named {
                    body.push(kind);
                    body.extend(origin);
                    body.extend_from_slice(&stream.to_le_bytes());
                    put_varint(&mut body, ordinal);
                    body.extend_from_slice(&amount.to_le_bytes());
                }
            }
            Arrival::LastRelease(conn) => {
                let Some((stream, ordinal, header, retained)) = conns(|c| {
                    c.get(&conn)
                        .map(|e| (e.stream, e.ordinal, e.header, e.retained))
                }) else {
                    return live.cut(UNNAMED_INBOX);
                };
                body.push(6);
                body.extend_from_slice(&stream.to_le_bytes());
                put_varint(&mut body, ordinal);
                body.extend_from_slice(&header.to_le_bytes());
                body.extend_from_slice(&retained.saturating_sub(header).to_le_bytes());
            }
            Arrival::SharedRelease(node) => {
                // The release ends the node: nothing names it after.
                let Some((stream, ordinal)) =
                    nodes(|n| n.remove(&node).map(|e| (e.stream, e.ordinal)))
                else {
                    return live.cut(UNNAMED_NODE);
                };
                live.takes.nodes.remove(&node);
                body.push(7);
                body.extend_from_slice(&stream.to_le_bytes());
                put_varint(&mut body, ordinal);
            }
            Arrival::Cancel => body.push(8),
            Arrival::Unrecorded(why) => return live.cut(why),
        }
        live.take(point(pump), body);
    });
}

const UNNAMED_INBOX: &str = "a take for a connection the recording did not name";
const UNNAMED_SURVIVOR: &str =
    "a helper end keeps a connection or Shared node the recording did not name";
