//! A unit's replay against its own stream (docs/recording.md, "Replay of a
//! run with helpers"): each `UNIT_SPAWN` is checked and starts nothing, and
//! each helper end comes from the unit's own `TAKE` of it, at the take point
//! the recording wrote it.
use super::super::*;
use super::{Group, point};
use format::{Arrival, HelperOutcome};
use hirsel_core_layout::cross::{CASE, ELEMENT, PADDING, SEQ, elements, live, sequences};

mod arrive;
pub(in crate::tape) use arrive::{Receiver, set_across, word as arrived};

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

    /// A replayed waiting send's `ISSUE`, checked as `checked` checks an `OP`:
    /// consumes 1 event (the issue commit, matching recording) and returns the
    /// `ISSUE`'s 1-based frame position, or None once the replay has ended.
    fn checked_issue(&mut self, task: u64, op: u8, object: u64, args: &Args) -> Option<u64> {
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
            Frame::Issue {
                task: t,
                op: o,
                object: b,
                args: stored,
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
        Some(rep.taken)
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

/// Outcome of a replayed `CONN_SEND` or `CONN_TRY_SEND`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayedSend {
    /// Immediate outcome from an `OP` frame (0 Accepted, 1 Closed/Full, 2 Closed).
    Immediate(u64),
    /// Waiting send from a `CONN_SEND` `ISSUE` frame: its synthetic id in
    /// `LangUnit::sends`, resolved later by its settlement `TAKE` (tag 3).
    Waiting(u32),
}

/// Outcome of a recorded settlement `TAKE` (tag 3).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettlementOutcome {
    Accepted,
    Closed,
    Withdrawn,
}

/// A replayed send's `CONN_SEND` or `CONN_TRY_SEND`, checked as an issue
/// with the item its client sends: its recorded outcome (`OP`) or waiting id
/// (`ISSUE`), or None once the replay has ended. The send delivers nothing;
/// the receiving unit takes the item from its own `TAKE`.
pub(crate) fn send(
    group: Option<Group>,
    op: u8,
    item: Result<(u64, Vec<u8>), &'static str>,
) -> Option<ReplayedSend> {
    replaying(group, |live| {
        let (object, value) = match item {
            Ok(item) => item,
            Err(why) => {
                live.refuse_replay(why);
                return None;
            }
        };
        let canonical = format::Value::decode(&value)
            .map(|v| v.canonical())
            .unwrap_or_default();
        let task = group.map_or(0, |g| g.task);
        let args = Args::of_parts(&[&canonical]);
        if op == op::CONN_SEND
            && matches!(
                live.replayer.as_mut().unwrap().peek(),
                Ok(Frame::Issue { .. })
            )
        {
            let position = live.checked_issue(task, op, object, &args)?;
            live.takes.next_replay_send = live.takes.next_replay_send.wrapping_add(1).max(1);
            let id = live.takes.next_replay_send;
            live.takes.replay_sends.insert(position, (id, canonical));
            return Some(ReplayedSend::Waiting(id));
        }
        live.checked(task, op, object, &args)
            .map(ReplayedSend::Immediate)
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
    /// A helper's end: the handle's key, its result packed for the
    /// handle's result type or the causes it failed or was cancelled with,
    /// and the survivor charges its owner keeps.
    End {
        key: usize,
        outcome: Result<Vec<u8>, Vec<DueCause>>,
        survivors: Vec<format::Survivor>,
    },
    /// A cancellation request for the unit's root.
    Cancel,
    /// An item on the unit's own connection, by its address: its flat bytes.
    Item { conn: usize, flat: Vec<u8> },
    /// A notice for the unit's own connection, by its address.
    Notice { conn: usize, notice: Notice },
    /// A `Shared` node homed in this unit, by its address, to clean now.
    SharedRelease(usize),
    /// A kept survivor charge from an ended helper, to refund now.
    ReleaseSurvivor {
        payer: crate::limit::Payer,
        bytes: u64,
    },
    /// A waiting send's recorded settlement: its synthetic id and outcome.
    Settlement { id: u32, outcome: SettlementOutcome },
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
    registry: Option<&crate::host_file::Registry>,
) -> Option<Due> {
    let into = group.map(|g| Receiver {
        registry,
        provider: g.cell.provider,
        generation: g.generation,
    });
    replaying(group, |live| {
        let into = into.unwrap();
        live.fresh_takes();
        let (events, last, count) = (live.schedule.events, live.schedule.last, live.takes.count);
        let own = super::stream(live);
        let mut objects: Vec<u64> = super::conns(|c| {
            c.values()
                .filter(|e| e.stream == own && !e.stand_in)
                .map(|e| e.ordinal)
                .collect()
        });
        objects.extend(live.takes.clients.values().copied());
        let created = live.objects.len() as u64;
        let rep = live.replayer.as_mut().unwrap();
        let (gap, at, reached, is_refund) = match rep.peek() {
            Ok(Frame::Take {
                gap,
                point,
                count,
                body,
            }) => (
                *gap,
                *point,
                *count,
                matches!(body.arrival(), Arrival::Refund(_)),
            ),
            _ => return None,
        };
        // A drop's `CLOSE` recorded before the take comes first.
        if rep
            .closes
            .unmatched_before(rep.taken + 1, created, &objects)
        {
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
        // Refund takes are consumed at mailbox drains (`refund_due`), not unit pumps.
        if is_refund {
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
        let (ordinal, value, survivors) = match body.arrival() {
            Arrival::HelperEnd(ordinal, HelperOutcome::Returned(value), survivors) => {
                (ordinal, Ok(value.bytes().to_vec()), survivors)
            }
            Arrival::HelperEnd(
                ordinal,
                HelperOutcome::Failed(causes) | HelperOutcome::Cancelled(causes),
                survivors,
            ) => (ordinal, Err(due_causes(&causes)), survivors),
            Arrival::HelperEnd(_, HelperOutcome::Deadlock(_), _) => {
                unreachable!("HelperEnd arrival rejects Deadlock")
            }
            Arrival::Cancel => return Some(Due::Cancel),
            Arrival::Item(ordinal, value) => {
                let value = value.bytes().to_vec();
                return own_conn(live, &frame, ordinal, |live, conn| {
                    let (bytes, owners) = super::layout(conn).unwrap_or_default();
                    let bytes = bytes as usize;
                    let flat = {
                        let held =
                            &mut |kind, c: &mut format::Cursor<'_>| arrived(live, into, kind, c);
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
                if stream == super::stream(live) {
                    let notice = Notice::LastRelease { header, unused };
                    return own_conn(live, &frame, ordinal, |_, conn| {
                        Some(Due::Notice { conn, notice })
                    });
                }
                let name = format::SurvivorName {
                    kind: format::SurvivorKind::Connection,
                    stream,
                    ordinal,
                };
                if let Some((payer, bytes)) = live.takes.survivors.remove(&name)
                    && header.checked_add(unused).is_some_and(|r| r <= bytes)
                {
                    return Some(Due::ReleaseSurvivor { payer, bytes });
                }
                let rep = live.replayer.as_mut().unwrap();
                let fault = rep.compared(
                    Mismatch::Take,
                    &frame,
                    format_args!("no surviving connection ({stream}, {ordinal})"),
                );
                live.stop_replay(fault);
                return None;
            }
            Arrival::SharedRelease(stream, ordinal) => {
                if stream == super::stream(live) {
                    let node = super::nodes(|n| {
                        let key = n
                            .iter()
                            .find(|(_, e)| e.stream == own && e.ordinal == ordinal && !e.stand_in)
                            .map(|(&k, _)| k);
                        if let Some(key) = key {
                            n.remove(&key);
                        }
                        key
                    });
                    if let Some(node) = node {
                        live.takes.nodes.remove(&node);
                        return Some(Due::SharedRelease(node));
                    }
                    let rep = live.replayer.as_mut().unwrap();
                    let fault = rep.compared(
                        Mismatch::Take,
                        &frame,
                        format_args!("no Shared node with ordinal {ordinal}"),
                    );
                    live.stop_replay(fault);
                    return None;
                }
                let name = format::SurvivorName {
                    kind: format::SurvivorKind::Shared,
                    stream,
                    ordinal,
                };
                if let Some((payer, bytes)) = live.takes.survivors.remove(&name) {
                    return Some(Due::ReleaseSurvivor { payer, bytes });
                }
                let rep = live.replayer.as_mut().unwrap();
                let fault = rep.compared(
                    Mismatch::Take,
                    &frame,
                    format_args!("no surviving Shared node ({stream}, {ordinal})"),
                );
                live.stop_replay(fault);
                return None;
            }
            Arrival::Settlement(position, settlement) => {
                let Some((id, sent_canonical)) = live.takes.replay_sends.remove(&position) else {
                    let rep = live.replayer.as_mut().unwrap();
                    let fault = rep.compared(
                        Mismatch::Take,
                        &frame,
                        format_args!("no waiting send at position {position}"),
                    );
                    live.stop_replay(fault);
                    return None;
                };
                let outcome = match settlement {
                    format::Settlement::Accepted => SettlementOutcome::Accepted,
                    format::Settlement::Closed(returned) => {
                        if returned.canonical() != sent_canonical {
                            let rep = live.replayer.as_mut().unwrap();
                            let fault = rep.compared(
                                Mismatch::Take,
                                &frame,
                                format_args!("closed settlement item differs from sent item"),
                            );
                            live.stop_replay(fault);
                            return None;
                        }
                        SettlementOutcome::Closed
                    }
                    format::Settlement::Withdrawn => SettlementOutcome::Withdrawn,
                };
                return Some(Due::Settlement { id, outcome });
            }
            Arrival::Refund(_) => unreachable!("handled at drain"),
        };
        let mut seen = std::collections::BTreeSet::new();
        for s in &survivors {
            use format::SurvivorKind::{Connection, GroupQueue, Shared, UdpPool};
            if matches!(s.name.kind, Connection | Shared | UdpPool | GroupQueue)
                && (live.takes.survivors.contains_key(&s.name) || !seen.insert(s.name))
            {
                let rep = live.replayer.as_mut().unwrap();
                let fault = rep.compared(
                    Mismatch::Take,
                    &frame,
                    format_args!("duplicate survivor {:?}", s.name),
                );
                live.stop_replay(fault);
                return None;
            }
        }
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
                    let held = &mut |kind, c: &mut format::Cursor<'_>| arrived(live, into, kind, c);
                    let bytes = bytes as usize;
                    packed(&value, vec![0; bytes], (0, bytes), &owners, held)
                })
                .map(Ok),
            Err(causes) => key.map(|_| Err(causes)).ok_or(Unpacked::Layout),
        };
        match (key, outcome) {
            (Some(key), Ok(outcome)) => {
                live.takes.helpers.remove(&key);
                Some(Due::End {
                    key,
                    outcome,
                    survivors,
                })
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
            .find(|(_, e)| e.stream == own && e.ordinal == ordinal && !e.stand_in)
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

/// End the replay `Diverged(Take)` at the take frame just consumed.
pub(crate) fn diverged_take(group: Option<Group>, why: &'static str) {
    replaying(group, |live| {
        let rep = live.replayer.as_mut().unwrap();
        let at = rep.taken;
        rep.note_next(format_args!("{why}"));
        live.stop_replay(Fault::Diverged(Divergence {
            at,
            kind: Mismatch::Take,
        }));
    });
}

/// Keep a helper end's named survivor charges in the owner's replay state.
pub(crate) fn keep_survivors(
    group: Option<Group>,
    survivors: Vec<(format::SurvivorName, crate::limit::Payer, u64)>,
) {
    replaying(group, |live| {
        for (name, payer, bytes) in survivors {
            live.takes.survivors.insert(name, (payer, bytes));
        }
    });
}

/// Drain arrived socket `(key, origin)` pairs recorded during entry/take
/// unpacking so the unit can register their routes in `files.tcp_routes`.
pub(crate) fn take_arrived_sockets(group: Option<Group>) -> Vec<(u64, crate::net::TcpOrigin)> {
    replaying(group, |live| {
        std::mem::take(&mut live.takes.arrived_sockets)
    })
    .unwrap_or_default()
}

/// Refund any kept survivor charges still held when a replaying unit ends.
pub(crate) fn drain_survivors(group: Option<Group>) {
    let remaining: Vec<_> = replaying(group, |live| {
        std::mem::take(&mut live.takes.survivors)
            .into_values()
            .collect()
    })
    .unwrap_or_default();
    for (payer, bytes) in remaining {
        payer.settle_at_unit(bytes);
    }
}

/// Whether a replaying waiting send `id` settled `Withdrawn` at the next frame,
/// so the cancelled sender observes cancellation before taking the settlement.
pub(crate) fn withdraw_wins(group: Option<Group>, id: u32) -> bool {
    replaying(group, |live| {
        let Some((&pos, _)) = live
            .takes
            .replay_sends
            .iter()
            .find(|(_, (sid, _))| *sid == id)
        else {
            return false;
        };
        let rep = live.replayer.as_mut().unwrap();
        matches!(
            rep.peek(),
            Ok(Frame::Take { body, .. })
                if matches!(body.arrival(), Arrival::Settlement(p, format::Settlement::Withdrawn) if p == pos)
        )
    })
    .unwrap_or(false)
}

/// Whether the stream's next frame is an in-operation `LastRelease` take at
/// count 1 (right after a stand-in client's `CLOSE`) when `(stream, ordinal)`
/// is a kept surviving connection.
pub(crate) fn is_last_release_due(group: Option<Group>, stream: u32, ordinal: u64) -> bool {
    replaying(group, |live| {
        let name = format::SurvivorName {
            kind: format::SurvivorKind::Connection,
            stream,
            ordinal,
        };
        if !live.takes.survivors.contains_key(&name) {
            return false;
        }
        live.fresh_takes();
        let (events, last) = (live.schedule.events, live.schedule.last);
        let own = super::stream(live);
        let rep = live.replayer.as_mut().unwrap();
        let Ok(&Frame::Take {
            gap,
            point: format::TakePoint::Operation,
            count: 1,
            ref body,
        }) = rep.peek()
        else {
            return false;
        };
        last.checked_add(gap) == Some(events)
            && matches!(body.arrival(), Arrival::LastRelease(s, ..) if s != own)
    })
    .unwrap_or(false)
}

/// The next refund `TAKE` due at this mailbox drain, if any: `(limit, amount, frame)`.
pub(crate) fn refund_due(group: Group) -> Option<(u64, u64, Frame)> {
    let pump = super::binding();
    replaying(Some(group), |live| {
        live.fresh_takes();
        let (events, last, count) = (live.schedule.events, live.schedule.last, live.takes.count);
        let own = super::stream(live);
        let mut objects: Vec<u64> = super::conns(|c| {
            c.values()
                .filter(|e| e.stream == own && !e.stand_in)
                .map(|e| e.ordinal)
                .collect()
        });
        objects.extend(live.takes.clients.values().copied());
        let created = live.objects.len() as u64;
        let rep = live.replayer.as_mut().unwrap();
        let (gap, at, reached, limit, amount) = match rep.peek() {
            Ok(Frame::Take {
                gap,
                point,
                count,
                body,
            }) => match body.arrival() {
                Arrival::Refund(credit) => (*gap, *point, *count, credit.limit, credit.amount),
                _ => return None,
            },
            _ => return None,
        };
        if rep
            .closes
            .unmatched_before(rep.taken + 1, created, &objects)
        {
            return None;
        }
        let position = last.checked_add(gap)?;
        if position > events || (position == events && (at != point(pump) || reached > count)) {
            return None;
        }
        if position < events || reached < count {
            rep.note_next(format_args!(
                "TAKE(Refund) event={events} count={count} kind={}",
                u8::from(pump)
            ));
            let fault = rep.diverged_next(Mismatch::Take);
            live.stop_replay(fault);
            return None;
        }
        let Ok(frame) = rep.next() else {
            unreachable!("the peeked TAKE")
        };
        live.schedule.last = events;
        live.takes.position = rep.taken;
        Some((limit, amount, frame))
    })
    .flatten()
}

/// End the replay `Diverged(Take)` for a refund `TAKE` that could not apply.
pub(crate) fn refund_mismatch(group: Group, frame: &Frame, why: &'static str) {
    replaying(Some(group), |live| {
        let rep = live.replayer.as_mut().unwrap();
        let fault = rep.compared(Mismatch::Take, frame, format_args!("{why}"));
        live.stop_replay(fault);
    });
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
    flat: Vec<u8>,
    span: (usize, usize),
    owners: &[[u64; 4]],
    word: &mut dyn FnMut(u64, &mut format::Cursor<'_>) -> Result<u64, Unpacked>,
) -> Result<Vec<u8>, Unpacked> {
    let mut c = format::Cursor::new(value);
    let plain = c.bytes()?;
    let parts = c.varint()?;
    let out = packed_body(plain, parts, &mut c, flat, span, owners, word)?;
    if !c.done() {
        return Err(Unpacked::Layout);
    }
    Ok(out)
}

/// Unpack a value whose `plain` bytes and `parts` count have already been read
/// from `c`, leaving `c` positioned immediately after this value's last part.
pub(in crate::tape) fn packed_body(
    plain: &[u8],
    parts: u64,
    c: &mut format::Cursor<'_>,
    mut flat: Vec<u8>,
    (at, len): (usize, usize),
    owners: &[[u64; 4]],
    word: &mut dyn FnMut(u64, &mut format::Cursor<'_>) -> Result<u64, Unpacked>,
) -> Result<Vec<u8>, Unpacked> {
    if plain.len() != len {
        return Err(Unpacked::Layout);
    }
    flat.get_mut(at..at + len)
        .ok_or(Unpacked::Layout)?
        .copy_from_slice(plain);
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
    // Only padding and case guards within a list's elements: no owner words,
    // whose parts follow the list's and are not counted above.
    let owner = |r: &[u64; 4]| r[1] & !ELEMENT != PADDING && r[1] & CASE == 0;
    if held
        .iter()
        .any(|(row, inner)| row[1] == SEQ && inner.iter().any(owner))
    {
        return Err(Unpacked::Unsupported(HELD));
    }
    if held.len() as u64 != parts {
        return Err(Unpacked::Layout);
    }
    let mut lists = BTreeMap::new();
    for ([at, kind, element, _], _) in held {
        if kind != SEQ {
            let w = word(kind, c)?;
            let at = at as usize;
            flat.get_mut(at..at + 8)
                .ok_or(Unpacked::Layout)?
                .copy_from_slice(&w.to_le_bytes());
            continue;
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
    for (row, _) in sequences(&rows) {
        if let Some(elements) = lists.get(&row[0]) {
            flat.extend_from_slice(elements);
        }
    }
    Ok(flat)
}

const HELD: &str = "a value holding a list whose elements hold handles or functions";
