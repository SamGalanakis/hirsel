//! Unit recordings: take points and their `TAKE` frames, the unit operation
//! frames, and the object ordinals of connections, clients and helper handles
//! (docs/recording.md, "Unit recordings").
use super::schedule::Group;
use super::*;
use crate::units_lang::Cross;
use format::{TakeBody, TakePoint};
use hirsel_core_layout::cross::CLIENT;
use std::collections::HashMap;

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
    /// Helper handle ordinal by the handle's key.
    helpers: HashMap<usize, u64>,
    /// Each waiting send: its `ISSUE` position and the item as a value.
    sends: HashMap<u32, (u64, Vec<u8>)>,
}

/// What every unit of the run knows of a connection made while recording: the
/// stream that made it and its ordinal there, the item's layout, and its
/// retained charge once cleaned. Keyed by the connection's address; a newer
/// connection at the same address replaces the entry.
struct Conn {
    stream: u32,
    ordinal: u64,
    cap: u64,
    room: u64,
    bytes: u64,
    owners: Vec<[u64; 4]>,
    header: u64,
    retained: u64,
}

static CONNS: Mutex<Option<HashMap<usize, Conn>>> = Mutex::new(None);

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

    /// Start a new count when an event or another frame came since the last.
    fn fresh_takes(&mut self) {
        let position = self.recorder.as_ref().map_or(0, |r| r.encoder.position());
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

fn stream(live: &Live) -> u32 {
    live.recorder.as_ref().map_or(0, |r| r.stream)
}

/// The unit's code reached an in-operation take point.
pub(crate) fn reach(group: Option<Group>) {
    recording(group, |live| {
        live.fresh_takes();
        live.takes.count += 1;
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
    cap: u64,
    room: u64,
) {
    recording(group, |live| {
        let ordinal = live.unit_object();
        let entry = Conn {
            stream: stream(live),
            ordinal,
            cap,
            room,
            bytes: item.bytes,
            owners: item.owners().to_vec(),
            header: 0,
            retained: 0,
        };
        conns(|c| c.insert(conn, entry));
        let client_ordinal = live.unit_object();
        live.takes.clients.insert(client, client_ordinal);
    });
}

/// `conn.client`: a new client ordinal.
pub(crate) fn client_new(group: Option<Group>, client: usize) {
    recording(group, |live| {
        let ordinal = live.unit_object();
        live.takes.clients.insert(client, ordinal);
    });
}

fn close_frame(live: &mut Live, object: u64) {
    let frame = Frame::Op {
        task: 0,
        op: op::CLOSE,
        object,
        args: Args::Literal(Vec::new()),
        out: Out::default(),
    };
    let _ = live.recorder.as_mut().unwrap().frame(&frame);
}

/// A client's drop: its `CLOSE`.
pub(crate) fn client_drop(group: Option<Group>, client: usize) {
    recording(group, |live| {
        if let Some(ordinal) = live.takes.clients.remove(&client) {
            close_frame(live, ordinal);
        }
    });
}

/// An inbox's drop: its `CLOSE`.
pub(crate) fn inbox_drop(group: Option<Group>, conn: usize) {
    recording(group, |live| {
        let own = stream(live);
        if let Some(ordinal) =
            conns(|c| c.get(&conn).filter(|e| e.stream == own).map(|e| e.ordinal))
        {
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

/// A send's item as a value, from the sender's live item; with the client's
/// ordinal, or why the send is not recorded.
/// # Safety
/// `item` is the client's connection's initialized item.
pub(crate) unsafe fn send_item(
    group: Option<Group>,
    client: usize,
    conn: usize,
    item: *const u8,
) -> Option<Result<(u64, Vec<u8>), &'static str>> {
    recording(group, |live| {
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
/// there, then `cap` and room `b`.
pub(super) fn client_snapshot(word: u64, ordinal: u64) -> Result<Vec<u8>, &'static str> {
    let conn = crate::units_lang::takes::client_conn(word);
    let (stream, of, cap, room) =
        conns(|c| c.get(&conn).map(|e| (e.stream, e.ordinal, e.cap, e.room))).ok_or(UNNAMED)?;
    let mut out = vec![0];
    put_varint(&mut out, ordinal);
    out.extend_from_slice(&stream.to_le_bytes());
    put_varint(&mut out, of);
    out.extend_from_slice(&cap.to_le_bytes());
    out.extend_from_slice(&room.to_le_bytes());
    Ok(out)
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

const CAPABILITY_ITEM: &str =
    "an item holds a capability, whose snapshot this build does not write";

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
    },
    Close(usize),
    SealAnswer(usize),
    /// A waiting send's settlement: 0 `Accepted`, 1 `Closed`, 2 `Withdrawn`.
    Settlement(u32, u8),
    HelperEnd {
        key: usize,
        outcome: &'a Result<(u8, Vec<u8>), &'static str>,
        /// Kept connections by address, with their amounts.
        survivors: &'a [(usize, u64)],
        /// Why other survivors cannot be named, if there are any.
        unnamed: Option<&'static str>,
    },
    LastRelease(usize),
    Cancel,
    /// What this build does not take into a recording.
    Unrecorded(&'static str),
}

/// One `TAKE`, at a scheduler pump or the in-operation point last reached.
/// # Safety
/// An item's `flat` and `content` are its connection's live packed item.
pub(crate) unsafe fn take(group: Option<Group>, pump: bool, arrival: Arrival<'_>) {
    recording(group, |live| {
        let own = stream(live);
        let ordinal =
            |conn: usize| conns(|c| c.get(&conn).filter(|e| e.stream == own).map(|e| e.ordinal));
        let mut body = Vec::new();
        match arrival {
            Arrival::Item {
                conn,
                flat,
                content,
            } => {
                let value = ordinal(conn).zip(layout(conn)).map(|(ordinal, layout)| {
                    // A client that arrives takes its ordinal at its take.
                    let mut handle = |kind, word: u64| match kind {
                        CLIENT => {
                            let o = live.unit_object();
                            live.takes.clients.insert(word as usize, o);
                            client_snapshot(word, o)
                        }
                        _ => Err(CAPABILITY_ITEM),
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
                match outcome {
                    0 => put_varint(&mut body, 0),
                    1 => body.extend(value),
                    _ => {}
                }
            }
            Arrival::HelperEnd {
                key,
                outcome,
                survivors,
                unnamed,
            } => {
                let Some(ordinal) = live.takes.helpers.remove(&key) else {
                    return;
                };
                if let Some(why) = unnamed {
                    return live.cut(why);
                }
                let (kind, outcome) = match outcome {
                    Ok(outcome) => outcome,
                    Err(why) => return live.cut(why),
                };
                body.push(4);
                put_varint(&mut body, ordinal);
                body.push(*kind);
                body.extend_from_slice(outcome);
                put_varint(&mut body, survivors.len() as u64);
                for &(conn, amount) in survivors {
                    let Some((stream, ordinal)) =
                        conns(|c| c.get(&conn).map(|e| (e.stream, e.ordinal)))
                    else {
                        return live.cut(UNNAMED_INBOX);
                    };
                    body.push(0);
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
            Arrival::Cancel => body.push(8),
            Arrival::Unrecorded(why) => return live.cut(why),
        }
        live.take(point(pump), body);
    });
}

const UNNAMED_INBOX: &str = "a take for a connection the recording did not name";
