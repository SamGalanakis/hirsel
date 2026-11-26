//! A unit's replay against its own stream (docs/recording.md, "Replay of a
//! run with helpers"): each `UNIT_SPAWN` is checked and starts nothing, and
//! each helper end comes from the unit's own `TAKE` of it, at the take point
//! the recording wrote it.
use super::super::*;
use super::{Group, point};
use format::{Arrival, HelperOutcome};
use hirsel_core_layout::cross::{CASE, ELEMENT, PADDING, SEQ, elements, live, sequences};

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
    /// WHY`, exit 2, as a refusal at open.
    fn refuse_replay(&mut self, why: &str) {
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

    /// The end of a full replay that left a started helper's stream unread:
    /// refused, since this build replays no helper stream.
    pub(in super::super) fn refuse_unreplayed(&mut self) {
        if self.replayer.as_ref().is_some_and(|r| r.unreplayed) {
            self.refuse_replay(
                "a helper's stream is not replayed in this build: replay the root alone with \
                 --unit root",
            );
        }
    }
}

pub(super) fn replaying<R>(group: Option<Group>, f: impl FnOnce(&mut Live) -> R) -> Option<R> {
    group?
        .with(|live| live.replaying().then(|| f(live)))
        .flatten()
}

/// A replayed `UNIT_SPAWN` with its arguments, or why this build does not
/// replay it: checked as a provider issue is. The helper's handle takes its
/// ordinal, and the helper starts nothing. A delivered stream the tape does
/// not hold is that child's `Truncated at 0`, unless the unit replays alone.
pub(crate) fn spawn(group: Option<Group>, key: usize, args: Result<Vec<u8>, &'static str>) {
    replaying(group, |live| {
        let args = match args {
            Ok(args) => Args::of_parts(&[&args]),
            Err(why) => return live.refuse_replay(why),
        };
        let task = group.map_or(0, |g| g.task);
        live.unit_commit(task);
        let rep = live.replayer.as_mut().unwrap();
        let observed = IssueEvidence {
            task,
            op: op::UNIT_SPAWN,
            object: 0,
            args: &args,
        };
        let frame = match rep.next() {
            Ok(frame) => frame,
            Err(fault) => {
                if let Fault::Diverged(_) = fault {
                    rep.note_next(format_args!("{observed}"));
                }
                return live.stop_replay(fault);
            }
        };
        let mismatch = match &frame {
            Frame::Op {
                task: t,
                op,
                object,
                args: stored,
                ..
            } => {
                if *t != task {
                    Some(Mismatch::Schedule)
                } else if *op != op::UNIT_SPAWN {
                    Some(Mismatch::Op)
                } else if *object != 0 {
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
            return live.stop_replay(fault);
        }
        let Frame::Op { out, .. } = frame else {
            unreachable!("a matched UNIT_SPAWN is an OP")
        };
        live.schedule.events += 1;
        let ordinal = live.unit_object();
        live.takes.helpers.insert(key, ordinal);
        let rep = live.replayer.as_mut().unwrap();
        let index = rep.spawns;
        rep.spawns += 1;
        if rep.alone {
            return;
        }
        let held = u32::try_from(out.value).is_ok_and(|s| rep.streams.contains_key(&s));
        if held {
            rep.unreplayed = true;
            return;
        }
        // The missing stream's verdict names the child, at its position 0.
        let child = format!("{}.{index}", rep.unit);
        live.fault = Some(refuse(Fault::Truncated(0)));
        if let Some(abort) = live.replay_abort {
            let line = format!("{}\n", format_verdict(Fault::Truncated(0), 0, &child));
            // SAFETY: `abort.stderr` is the launcher's stderr fd.
            unsafe {
                crate::os::diagnostic(abort.stderr, line.as_ptr(), line.len());
                libc::_exit(1);
            }
        }
    });
}

/// A helper's end that a recorded `TAKE` delivers now: the handle's key and
/// its result, packed for the handle's result type.
pub(crate) struct DueEnd {
    pub(crate) key: usize,
    pub(crate) packed: Vec<u8>,
}

/// The `TAKE` due at this take point (`pump`: a scheduler pump, kind 1), if
/// the stream's next frame is one: the end it delivers. `layout` gives a
/// helper handle's result layout by its key. A take this point passed, one
/// naming no helper of the unit, or a result of another layout ends the
/// replay `Diverged(Take)`.
pub(crate) fn due(
    group: Option<Group>,
    pump: bool,
    layout: impl Fn(usize) -> Option<(u64, Vec<[u64; 4]>)>,
) -> Option<DueEnd> {
    replaying(group, |live| {
        live.fresh_takes();
        let (events, last, count) = (live.schedule.events, live.schedule.last, live.takes.count);
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
                (ordinal, value.bytes().to_vec())
            }
            Arrival::HelperEnd(..) => {
                live.refuse_replay("a failed or cancelled helper's end");
                return None;
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
        let packed = key
            .and_then(&layout)
            .ok_or(Unpacked::Layout)
            .and_then(|(bytes, owners)| packed(&value, bytes, &owners));
        match (key, packed) {
            (Some(key), Ok(packed)) => {
                live.takes.helpers.remove(&key);
                Some(DueEnd { key, packed })
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

enum Unpacked {
    /// The value does not have the receiving type's layout.
    Layout,
    Unsupported(&'static str),
}

/// A value's wire form (`helper::value`) packed as a crossing packs it for
/// the type of `bytes` flat bytes with `owners`: the flat bytes with each
/// list's length, then each list's elements in owner-row order.
fn packed(value: &[u8], bytes: u64, owners: &[[u64; 4]]) -> Result<Vec<u8>, Unpacked> {
    let mut c = format::Cursor::new(value);
    let plain = c.bytes().map_err(|_| Unpacked::Layout)?;
    if plain.len() as u64 != bytes {
        return Err(Unpacked::Layout);
    }
    let mut flat = plain.to_vec();
    let parts = c.varint().map_err(|_| Unpacked::Layout)?;
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
            return Err(Unpacked::Unsupported(HELD));
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

const HELD: &str = "a helper result holding a handle or a function";
