//! A unit's replay against its own stream (docs/recording.md, "Replay of a
//! run with helpers"): each `UNIT_SPAWN` is checked and starts nothing, and
//! each helper end comes from the unit's own `TAKE` of it, at the take point
//! the recording wrote it.
use super::super::*;
use super::{Group, point};
use format::{Arrival, HelperOutcome};
use hirsel_core_layout::cross::{CASE, ELEMENT, PADDING, SEQ, elements, live, sequences};

mod arrive;
pub(in crate::tape) use arrive::word as arrived;

impl Live {
    pub(super) fn replaying(&self) -> bool {
        self.mode == Mode::Replay && self.fault.is_none() && self.replayer.is_some()
    }

    /// End the replay at `fault`, as a provider issue's replay does.
    fn stop_replay(&mut self, fault: Fault) {
        self.fault = Some(refuse(fault));
        self.abort_replay(fault, 0);
    }

    /// End the replay at what this build does not replay: `replay: refused:
    /// WHY`, exit 2, as a refusal at open. It ends every unit's replay.
    pub(in super::super) fn refuse_replay(&mut self, why: &str) {
        self.fault = Some(refuse(Fault::Corrupt));
        if let Some(abort) = self.replay_abort {
            let line = format!("replay: refused: {why}\n");
            // SAFETY: `abort.stderr` is the launcher's stderr fd.
            unsafe {
                crate::os::diagnostic(abort.stderr, line.as_ptr(), line.len());
                libc::_exit(2);
            }
        }
    }
}

pub(super) fn replaying<R>(group: Option<Group>, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
    group?
        .with(|live| live.replaying().then(|| f(live)))
        .flatten()
}

impl Live {
    /// A drop's `CLOSE`, matched by its object wherever it fell among the
    /// stream's frames, as a provider close's is.
    pub(super) fn closed(&mut self, object: u64, args: &Args) {
        let rep = self.replayer.as_mut().unwrap();
        let closed = crate::tape::close_replay::CloseKey::new(op::CLOSE, object, args)
            .and_then(|key| rep.close(key));
        match closed {
            // As writing it does, the `CLOSE` starts the take points afresh.
            Ok(_) => {
                self.takes.count = 0;
                self.takes.events = self.schedule.events;
                self.takes.position = self.replayer.as_ref().unwrap().taken;
            }
            Err(fault) => self.stop_replay(fault),
        }
    }

    /// A replayed unit operation, checked as a provider issue is: the
    /// stream's next frame is its `OP`, with this task, code, object and
    /// arguments. Its outcome value, or None once the replay has ended.
    pub(super) fn checked(&mut self, task: u64, op: u8, object: u64, args: &Args) -> Option<u64> {
        self.unit_commit(task);
        let rep = self.replayer.as_mut().unwrap();
        let observed = IssueEvidence {
            task,
            op,
            object,
            args,
        };
        let frame = match rep.next() {
            Ok(frame) => frame,
            Err(fault) => {
                if let Fault::Diverged(_) = fault {
                    rep.note_next(format_args!("{observed}"));
                }
                self.stop_replay(fault);
                return None;
            }
        };
        let mismatch = match &frame {
            Frame::Op {
                task: t,
                op: o,
                object: b,
                args: stored,
                ..
            } => {
                if *t != task {
                    Some(Mismatch::Schedule)
                } else if *o != op {
                    Some(Mismatch::Op)
                } else if *b != object {
                    Some(Mismatch::Object)
                } else if stored.operands() != args.operands() {
                    Some(Mismatch::Arg)
                } else {
                    None
                }
            }
            Frame::End { .. } => Some(Mismatch::Missing),
            _ => Some(Mismatch::Schedule),
        };
        if let Some(kind) = mismatch {
            let fault = rep.compared(kind, &frame, format_args!("{observed}"));
            self.stop_replay(fault);
            return None;
        }
        let Frame::Op { out, .. } = frame else {
            unreachable!("a matched unit operation is an OP")
        };
        self.schedule.events += 1;
        Some(out.value)
    }
}

/// A replayed `UNIT_SPAWN` with its arguments, or why this build does not
/// replay it: checked as a provider issue is. The helper's handle takes its
/// ordinal, and the helper starts nothing: its stream replays as its own
/// unit. A delivered stream the tape does not hold is that child's verdict
/// `Truncated at 0`, unless the unit replays alone; the spawner goes on.
pub(crate) fn spawn(group: Option<Group>, key: usize, args: Result<Vec<u8>, &'static str>) {
    replaying(group, |live| {
        let args = match args {
            Ok(args) => Args::of_parts(&[&args]),
            Err(why) => return live.refuse_replay(why),
        };
        let task = group.map_or(0, |g| g.task);
        let Some(value) = live.checked(task, op::UNIT_SPAWN, 0, &args) else {
            return;
        };
        let ordinal = live.unit_object();
        live.takes.helpers.insert(key, ordinal);
        let rep = live.replayer.as_mut().unwrap();
        let index = rep.spawns;
        rep.spawns += 1;
        if rep.alone {
            return;
        }
        let held = u32::try_from(value).is_ok_and(|s| rep.streams.contains_key(&s));
        if held || live.replay_abort.is_none() {
            return;
        }
        // The missing stream's verdict names the child, at its position 0.
        let child = format!("{}.{index}", rep.unit);
        let line = format_verdict(Fault::Truncated(0), 0, &child);
        super::verdicts::unstarted(&child, line);
    });
}

/// A replayed `UNIT_CANCEL` on helper `key`, checked as an issue. The
/// helper's own stream holds its cancel take, so the request reaches nothing.
pub(crate) fn cancel(group: Option<Group>, key: usize) {
    replaying(group, |live| {
        let Some(&ordinal) = live.takes.helpers.get(&key) else {
            return;
        };
        let task = group.map_or(0, |g| g.task);
        let args = Args::Literal(Vec::new());
        live.checked(task, op::UNIT_CANCEL, ordinal, &args);
    });
}

/// A replayed send's `CONN_SEND` or `CONN_TRY_SEND`, checked as an issue
/// with the item its client sends: its recorded outcome, or None once the
/// replay has ended. The send delivers nothing; the receiving unit takes
/// the item from its own `TAKE`.
pub(crate) fn send(
    group: Option<Group>,
    op: u8,
    item: Result<(u64, Vec<u8>), &'static str>,
) -> Option<u64> {
    replaying(group, |live| {
        let (object, value) = match item {
            Ok(item) => item,
            Err(why) => {
                live.refuse_replay(why);
                return None;
            }
        };
        if let Ok(Frame::Issue { .. }) = live.replayer.as_mut().unwrap().peek() {
            live.refuse_replay("a send that waits");
            return None;
        }
        let canonical = format::Value::decode(&value)
            .map(|v| v.canonical())
            .unwrap_or_default();
        let task = group.map_or(0, |g| g.task);
        live.checked(task, op, object, &Args::of_parts(&[&canonical]))
    })
    .flatten()
}

/// What a recorded notice for one of the unit's own connections is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Notice {
    Close,
    SealAnswer,
    /// With the connection's header and the unused room past it, as the
    /// recorder writes them from the retained charge.
    LastRelease {
        header: u64,
        unused: u64,
    },
}

/// A cause of a helper's recorded failure, as the owner's join delivers it.
pub(crate) struct DueCause {
    pub(crate) kind: u8,
    pub(crate) site: u64,
    /// `Fail`'s text and its `String` capacity (0 for static text).
    pub(crate) message: Vec<u8>,
    pub(crate) capacity: u64,
    /// `Capacity`'s requested and available bytes.
    pub(crate) bytes: (u64, u64),
}

/// What a recorded `TAKE` delivers now.
pub(crate) enum Due {
    /// A helper's end: the handle's key, and its result packed for the
    /// handle's result type, or the causes it failed or was cancelled with.
    End {
        key: usize,
        outcome: Result<Vec<u8>, Vec<DueCause>>,
    },
    /// A cancellation request for the unit's root.
    Cancel,
    /// An item on the unit's own connection, by its address: its flat bytes.
    Item { conn: usize, flat: Vec<u8> },
    /// A notice for the unit's own connection, by its address.
    Notice { conn: usize, notice: Notice },
}

fn due_causes(causes: &[format::Cause<'_>]) -> Vec<DueCause> {
    causes
        .iter()
        .map(|c| {
            let mut cause = DueCause {
                kind: c.kind,
                site: c.site,
                message: Vec::new(),
                capacity: 0,
                bytes: (0, 0),
            };
            match c.detail {
                format::CauseDetail::Fail { message, capacity } => {
                    cause.message = message.to_vec();
                    cause.capacity = capacity;
                }
                format::CauseDetail::Capacity {
                    requested,
                    available,
                } => cause.bytes = (requested, available),
                format::CauseDetail::None => {}
            }
            cause
        })
        .collect()
}

/// The `TAKE` due at this take point (`pump`: a scheduler pump, kind 1), if
/// the stream's next frame is one: what it delivers. `layout` gives a helper
/// handle's result layout by its key. A take this point passed, one naming
/// no helper of the unit, or a result of another layout ends the replay
/// `Diverged(Take)`.
pub(crate) fn due(
    group: Option<Group>,
    pump: bool,
    layout: impl Fn(usize) -> Option<(u64, Vec<[u64; 4]>)>,
    grants: Option<&crate::host_file::RunGrants>,
) -> Option<Due> {
    replaying(group, |live| {
        live.fresh_takes();
        let (events, last, count) = (live.schedule.events, live.schedule.last, live.takes.count);
        let own = super::stream(live);
        let mut objects: Vec<u64> = super::conns(|c| {
            c.values()
                .filter(|e| e.stream == own)
                .map(|e| e.ordinal)
                .collect()
        });
        objects.extend(live.takes.clients.values().copied());
        let rep = live.replayer.as_mut().unwrap();
        let Ok(&Frame::Take {
            gap,
            point: at,
            count: reached,
            ..
        }) = rep.peek()
        else {
            return None;
        };
        // A drop's `CLOSE` recorded before the take comes first.
        if rep.closes.unmatched_before(rep.taken + 1, &objects) {
            return None;
        }
        let position = last.checked_add(gap)?;
        if position > events || (position == events && (at != point(pump) || reached > count)) {
            return None;
        }
        if position < events || reached < count {
            rep.note_next(format_args!(
                "TAKE event={events} count={count} kind={}",
                u8::from(pump)
            ));
            let fault = rep.diverged_next(Mismatch::Take);
            live.stop_replay(fault);
            return None;
        }
        let Ok(frame) = rep.next() else {
            unreachable!("the peeked TAKE")
        };
        let Frame::Take { body, .. } = &frame else {
            unreachable!("the peeked TAKE")
        };
        live.schedule.last = events;
        live.takes.position = rep.taken;
        let (ordinal, value) = match body.arrival() {
            Arrival::HelperEnd(ordinal, HelperOutcome::Returned(value), survivors) => {
                if !survivors.is_empty() {
                    live.refuse_replay("a helper end that hands its owner survivors");
                    return None;
                }
                (ordinal, Ok(value.bytes().to_vec()))
            }
            Arrival::HelperEnd(
                ordinal,
                HelperOutcome::Failed(causes) | HelperOutcome::Cancelled(causes),
                survivors,
            ) => {
                if !survivors.is_empty() {
                    live.refuse_replay("a helper end that hands its owner survivors");
                    return None;
                }
                (ordinal, Err(due_causes(&causes)))
            }
            Arrival::Cancel => return Some(Due::Cancel),
            Arrival::Item(ordinal, value) => {
                let value = value.bytes().to_vec();
                return own_conn(live, &frame, ordinal, |live, conn| {
                    let (bytes, owners) = super::layout(conn).unwrap_or_default();
                    let bytes = bytes as usize;
                    let flat = {
                        let held =
                            &mut |kind, c: &mut format::Cursor<'_>| arrived(live, grants, kind, c);
                        packed(&value, vec![0; bytes], (0, bytes), &owners, held)
                    };
                    match flat {
                        Ok(flat) => Some(Due::Item { conn, flat }),
                        Err(Unpacked::Unsupported(why)) => {
                            live.refuse_replay(why);
                            None
                        }
                        Err(Unpacked::Layout) => {
                            let rep = live.replayer.as_mut().unwrap();
                            let fault = rep.compared(
                                Mismatch::Take,
                                &frame,
                                format_args!("an item of another layout"),
                            );
                            live.stop_replay(fault);
                            None
                        }
                    }
                });
            }
            Arrival::Close(ordinal) | Arrival::SealAnswer(ordinal) => {
                let notice = if matches!(body.arrival(), Arrival::Close(_)) {
                    Notice::Close
                } else {
                    Notice::SealAnswer
                };
                return own_conn(live, &frame, ordinal, |_, conn| {
                    Some(Due::Notice { conn, notice })
                });
            }
            Arrival::LastRelease(stream, ordinal, header, unused) => {
                if stream != super::stream(live) {
                    live.refuse_replay("a last release of another unit's connection");
                    return None;
                }
                let notice = Notice::LastRelease { header, unused };
                return own_conn(live, &frame, ordinal, |_, conn| {
                    Some(Due::Notice { conn, notice })
                });
            }
            other => {
                live.refuse_replay(unsupported(&other));
                return None;
            }
        };
        let key = live
            .takes
            .helpers
            .iter()
            .find_map(|(&key, &o)| (o == ordinal).then_some(key));
        let outcome = match value {
            Ok(value) => key
                .and_then(&layout)
                .ok_or(Unpacked::Layout)
                .and_then(|(bytes, owners)| {
                    let held =
                        &mut |kind, c: &mut format::Cursor<'_>| arrived(live, grants, kind, c);
                    let bytes = bytes as usize;
                    packed(&value, vec![0; bytes], (0, bytes), &owners, held)
                })
                .map(Ok),
            Err(causes) => key.map(|_| Err(causes)).ok_or(Unpacked::Layout),
        };
        match (key, outcome) {
            (Some(key), Ok(outcome)) => {
                live.takes.helpers.remove(&key);
                Some(Due::End { key, outcome })
            }
            (_, Err(Unpacked::Unsupported(why))) => {
                live.refuse_replay(why);
                None
            }
            (key, _) => {
                let rep = live.replayer.as_mut().unwrap();
                let fault = match key {
                    None => rep.compared(
                        Mismatch::Take,
                        &frame,
                        format_args!("no helper with ordinal {ordinal}"),
                    ),
                    Some(_) => rep.compared(
                        Mismatch::Take,
                        &frame,
                        format_args!("a result of another layout"),
                    ),
                };
                live.stop_replay(fault);
                None
            }
        }
    })
    .flatten()
}

/// The unit's own connection with `ordinal`, as `then` delivers it; a take
/// naming no connection of the unit ends the replay `Diverged(Take)`.
fn own_conn(
    live: &mut Live,
    frame: &Frame,
    ordinal: u64,
    then: impl FnOnce(&mut Live, usize) -> Option<Due>,
) -> Option<Due> {
    let own = super::stream(live);
    let conn = super::conns(|c| {
        c.iter()
            .find(|(_, e)| e.stream == own && e.ordinal == ordinal)
            .map(|(&k, _)| k)
    });
    match conn {
        Some(conn) => then(live, conn),
        None => {
            let rep = live.replayer.as_mut().unwrap();
            let fault = rep.compared(
                Mismatch::Take,
                frame,
                format_args!("no connection with ordinal {ordinal}"),
            );
            live.stop_replay(fault);
            None
        }
    }
}

/// End the replay at what this build does not replay of a take.
pub(crate) fn refuse_take(group: Option<Group>, why: &'static str) {
    replaying(group, |live| live.refuse_replay(why));
}

fn unsupported(arrival: &Arrival<'_>) -> &'static str {
    match arrival {
        Arrival::Item(..) => "an item take",
        Arrival::Close(_) => "a close take",
        Arrival::SealAnswer(_) => "a seal answer take",
        Arrival::Settlement(..) => "a settlement take",
        Arrival::LastRelease(..) => "a last release take",
        Arrival::SharedRelease(..) => "a Shared release take",
        Arrival::Cancel => "a cancel take",
        Arrival::Refund(_) => "a refund take",
        Arrival::HelperEnd(..) => "a helper end take",
    }
}

pub(in crate::tape) enum Unpacked {
    /// The value does not have the receiving type's layout.
    Layout,
    Unsupported(&'static str),
}

impl From<format::Corrupt> for Unpacked {
    fn from(_: format::Corrupt) -> Self {
        Unpacked::Layout
    }
}

/// A value's wire form (`helper::value`) packed into `flat` as a crossing
/// packs it: its `len` plain bytes at `at`, each owner row of `owners` (offsets in
/// `flat`) filled, then each list's elements in owner-row order. A list's
/// length is written in its row; every other owner word is what `word` reads
/// from its part, given the row's kind.
pub(in crate::tape) fn packed(
    value: &[u8],
    mut flat: Vec<u8>,
    (at, len): (usize, usize),
    owners: &[[u64; 4]],
    word: &mut dyn FnMut(u64, &mut format::Cursor<'_>) -> Result<u64, Unpacked>,
) -> Result<Vec<u8>, Unpacked> {
    let mut c = format::Cursor::new(value);
    let plain = c.bytes()?;
    if plain.len() != len {
        return Err(Unpacked::Layout);
    }
    flat.get_mut(at..at + len)
        .ok_or(Unpacked::Layout)?
        .copy_from_slice(plain);
    let parts = c.varint()?;
    let rows = live(owners, &|at, width| {
        let mut word = [0u8; 8];
        let at = at as usize;
        if let Some(tag) = flat.get(at..at + width as usize) {
            word[..tag.len()].copy_from_slice(tag);
        }
        u64::from_le_bytes(word)
    });
    let mut held = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let inner = elements(&rows[i + 1..]);
        if rows[i][1] != PADDING {
            held.push((rows[i], inner));
        }
        i += 1 + inner.len();
    }
    held.sort_by_key(|o| o.0[0]);
    if held.len() as u64 != parts {
        return Err(Unpacked::Layout);
    }
    let mut lists = BTreeMap::new();
    for ([at, kind, element, _], inner) in held {
        if kind != SEQ {
            let w = word(kind, &mut c)?;
            let at = at as usize;
            flat.get_mut(at..at + 8)
                .ok_or(Unpacked::Layout)?
                .copy_from_slice(&w.to_le_bytes());
            continue;
        }
        // Only padding and case guards within the elements: no owner words.
        let owner = |r: &[u64; 4]| r[1] & !ELEMENT != PADDING && r[1] & CASE == 0;
        if inner.iter().any(owner) {
            return Err(Unpacked::Unsupported(HELD));
        }
        if c.u8() != Ok(0) {
            return Err(Unpacked::Layout);
        }
        let elements = c.bytes().map_err(|_| Unpacked::Layout)?;
        let len = (elements.len() as u64)
            .checked_div(element)
            .filter(|len| len * element == elements.len() as u64)
            .ok_or(Unpacked::Layout)?;
        let at = at as usize;
        flat.get_mut(at + 8..at + 16)
            .ok_or(Unpacked::Layout)?
            .copy_from_slice(&len.to_le_bytes());
        lists.insert(at as u64, elements);
    }
    if !c.done() {
        return Err(Unpacked::Layout);
    }
    for (row, _) in sequences(owners) {
        if let Some(elements) = lists.get(&row[0]) {
            flat.extend_from_slice(elements);
        }
    }
    Ok(flat)
}

const HELD: &str = "a value holding a list whose elements hold handles or functions";
