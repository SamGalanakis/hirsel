//! Stateful frame encoding and decoding, including DATA continuations.
use super::units::{carrier, put_carrier};
use super::*;
use std::collections::BTreeMap;

impl Frame {
    fn continued_payload(&mut self) -> &mut Vec<u8> {
        match self {
            Self::Done { out, .. } | Self::Op { out, .. } => &mut out.payload,
            Self::Grant {
                args: Some(payload),
                ..
            } => payload,
            _ => unreachable!(),
        }
    }
}

/// Encodes frames with the T bit's previous-task compression. DATA
/// continuations are produced here and never handed in by a caller.
#[derive(Default)]
pub struct Encoder {
    task: Option<u64>,
    /// The op outstanding per task, for a DONE's outcome shape.
    issued: BTreeMap<u64, u8>,
    positions: BTreeMap<u64, u64>,
    position: u64,
}
impl Encoder {
    /// A block's first frame that carries a task writes it (T set), so each
    /// block's frames read without the blocks before it.
    pub fn new_block(&mut self) {
        self.task = None;
    }
    fn task(&mut self, out: &mut Vec<u8>, head_at: usize, task: u64) {
        if self.task != Some(task) {
            out[head_at] |= T;
            put_varint(out, task);
            self.task = Some(task);
        }
    }
    /// The frame's bytes and the DATA frames that continue its payload. A
    /// writer may close its block between them, never inside one.
    pub fn frame(&mut self, f: &Frame) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut out = Vec::new();
        let mut rest = Vec::new();
        self.frame_into(f, &mut out, &mut rest);
        let data = rest
            .into_iter()
            .map(|chunk| {
                let mut d = Vec::new();
                put_data(&mut d, chunk);
                d
            })
            .collect();
        (out, data)
    }
    /// `frame`, appending the frame's bytes to `out` (a block being filled)
    /// and pushing the payload chunks of its DATA frames to `rest`, borrowed
    /// from the frame; `put_data` writes each where it falls.
    pub fn frame_into<'f>(&mut self, f: &'f Frame, out: &mut Vec<u8>, rest: &mut Vec<&'f [u8]>) {
        let start = out.len();
        self.position += if let Frame::Repeat { count } = f {
            *count
        } else {
            1
        };
        match f {
            Frame::Wait { gap, task } => {
                out.push(F_WAIT);
                put_varint(out, *gap);
                self.task(out, start, *task);
            }
            Frame::Issue {
                task,
                op,
                object,
                args,
            } => {
                out.push(F_ISSUE);
                self.task(out, start, *task);
                out.push(*op);
                put_varint(out, *object);
                args.encode(out);
                self.issued.insert(*task, *op);
                if *op == op::CONN_SEND {
                    self.positions.insert(self.position, *task);
                }
            }
            Frame::Done { gap, task, out: o } => {
                out.push(F_DONE);
                put_varint(out, *gap);
                self.task(out, start, *task);
                let op = self
                    .issued
                    .remove(task)
                    .expect("DONE follows its task's ISSUE");
                o.encode(op, out, rest);
            }
            Frame::Op {
                task,
                op,
                object,
                args,
                out: o,
            } => {
                out.push(F_OP);
                self.task(out, start, *task);
                out.push(*op);
                put_varint(out, *object);
                args.encode(out);
                o.encode(*op, out, rest);
            }
            Frame::Wake { gap, task } | Frame::Cancel { gap, task } => {
                out.push(if matches!(f, Frame::Wake { .. }) {
                    F_WAKE
                } else {
                    F_CANCEL
                });
                put_varint(out, *gap);
                self.task(out, start, *task);
            }
            Frame::Retired { gap, task, pool } => {
                out.push(F_RETIRED);
                put_varint(out, *gap);
                self.task(out, start, *task);
                put_varint(out, *pool);
            }
            Frame::Choose { gap, index } => put_choose(out, *gap, *index),
            Frame::Grant { gap, kind, args } => {
                out.push(F_GRANT);
                put_varint(out, *gap);
                out.push(*kind);
                if *kind == ARGS_KIND {
                    put_bytes(
                        out,
                        args.as_deref().expect("an Args grant carries its vector"),
                    );
                } else if *kind == EXEC_KIND {
                    let snapshot = args.as_deref().expect("an Exec grant carries its profiles");
                    put_varint(out, snapshot.len() as u64);
                    put_payload(out, snapshot, rest);
                } else if is_stream_grant(*kind) {
                    let [terminal] = args
                        .as_deref()
                        .expect("a stream grant carries its terminal bit")
                    else {
                        unreachable!("a stream grant carries one byte")
                    };
                    out.push(*terminal);
                }
            }
            Frame::Repeat { count } => put_repeat(out, *count),
            Frame::Take {
                gap,
                point,
                count,
                body,
            } => {
                out.push(F_TAKE);
                put_varint(out, *gap);
                out.push(point.tag());
                put_varint(out, *count);
                put_carrier(out, body.bytes(), rest);
                if let Some(position) = body.settlement() {
                    let task = self
                        .positions
                        .remove(&position)
                        .expect("TAKE follows its CONN_SEND ISSUE");
                    self.issued.remove(&task);
                }
            }
            Frame::HelperEnd { gap, ending } => {
                out.push(F_END);
                put_varint(out, *gap);
                out.push(END_COMPLETE);
                out.push(ending.kind());
                put_carrier(out, ending.body(), rest);
            }
            Frame::End { gap, ending: e } => {
                out.push(F_END);
                put_varint(out, *gap);
                out.push(END_COMPLETE);
                out.push(e.kind);
                e.outcome.encode(out);
            }
        }
    }
}

/// Decodes the frames of successive validated payloads. State crosses blocks:
/// the previous task, each task's outstanding op, an unfinished payload.
#[derive(Default)]
pub struct Decoder {
    task: Option<u64>,
    issued: BTreeMap<u64, u8>,
    positions: BTreeMap<u64, u64>,
    position: u64,
    helper: bool,
    entry: Option<Entry>,
    carrier: Option<OpenCarrier>,
    /// A frame whose payload still waits for DATA frames: (frame, bytes needed).
    open: Option<(Frame, usize)>,
    /// Return REPEAT frames as stored. By default they expand to CHOOSE.
    pub physical: bool,
}

enum CarrierKind {
    Entry {
        parent: u32,
        spawn: u64,
    },
    Take {
        gap: u64,
        point: TakePoint,
        count: u64,
    },
    End {
        gap: u64,
        kind: u8,
    },
}

struct OpenCarrier {
    kind: CarrierKind,
    total: usize,
    bytes: Vec<u8>,
}

impl OpenCarrier {
    fn start(kind: CarrierKind, c: &mut Cursor<'_>) -> Result<Self, Corrupt> {
        let (total, first) = carrier(c)?;
        Ok(Self {
            kind,
            total,
            bytes: first.to_vec(),
        })
    }

    fn add(&mut self, chunk: &[u8]) -> Result<(), Corrupt> {
        if chunk.len() > self.total - self.bytes.len() {
            return Err(Corrupt);
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }
}

impl Decoder {
    pub(super) fn helper(payload: &[u8]) -> Result<Self, Corrupt> {
        let mut decoder = Self {
            helper: true,
            ..Self::default()
        };
        let mut c = Cursor::new(payload);
        let kind = CarrierKind::Entry {
            parent: c.u32()?,
            spawn: c.u64()?,
        };
        let pending = OpenCarrier::start(kind, &mut c)?;
        if !c.done() {
            return Err(Corrupt);
        }
        decoder.complete_carrier(pending)?;
        Ok(decoder)
    }

    pub fn take_entry(&mut self) -> Option<Entry> {
        self.entry.take()
    }

    fn complete_carrier(&mut self, pending: OpenCarrier) -> Result<Option<Frame>, Corrupt> {
        if pending.bytes.len() != pending.total {
            self.carrier = Some(pending);
            return Ok(None);
        }
        let OpenCarrier { kind, bytes, .. } = pending;
        match kind {
            CarrierKind::Entry { parent, spawn } => {
                self.entry = Some(Entry::decode(parent, spawn, bytes)?);
                Ok(None)
            }
            CarrierKind::Take { gap, point, count } => {
                let body = TakeBody::decode(bytes)?;
                if let Some(position) = body.settlement() {
                    let task = self.positions.remove(&position).ok_or(Corrupt)?;
                    self.issued.remove(&task).ok_or(Corrupt)?;
                }
                Ok(Some(Frame::Take {
                    gap,
                    point,
                    count,
                    body,
                }))
            }
            CarrierKind::End { gap, kind } => {
                let ending = HelperEnding::decode(kind, bytes)?;
                Ok(Some(Frame::HelperEnd { gap, ending }))
            }
        }
    }

    fn task(&mut self, head: u8, c: &mut Cursor) -> Result<u64, Corrupt> {
        if head & T != 0 {
            let t = c.varint()?;
            self.task = Some(t);
            Ok(t)
        } else {
            self.task.ok_or(Corrupt)
        }
    }
    /// Every complete frame of `payload`, in order. `end` says the block is
    /// the END block, whose last frame must be END and whose only END it is.
    pub fn block(&mut self, payload: &[u8], end: bool) -> Result<Vec<Frame>, Corrupt> {
        let mut c = Cursor::new(payload);
        let mut frames = Vec::new();
        // A block's first task-carrying frame names its task (Encoder::new_block).
        self.task = None;
        let mut chooses = [None, None];
        let mut count = 0;
        while !c.done() {
            let head = c.u8()?;
            let kind = head & 0x0f;
            if head & 0xe0 != 0 {
                return Err(Corrupt);
            }
            let has_task = matches!(
                kind,
                F_ISSUE | F_DONE | F_OP | F_WAKE | F_CANCEL | F_WAIT | F_RETIRED
            );
            if head & T != 0 && !has_task {
                return Err(Corrupt);
            }
            if self.unfinished() && kind != F_DATA {
                return Err(Corrupt);
            }
            if kind == F_REPEAT {
                let n = c.varint()?;
                let [Some(two_back), Some(one_back)] = chooses else {
                    return Err(Corrupt);
                };
                if n == 0 || n > (MAX_FRAMES - count) as u64 {
                    return Err(Corrupt);
                }
                count += n as usize;
                self.position = self.position.checked_add(n).ok_or(Corrupt)?;
                if self.physical {
                    frames.push(Frame::Repeat { count: n });
                } else {
                    let pair = [two_back, one_back];
                    for i in 0..n {
                        let (gap, index) = pair[(i % 2) as usize];
                        frames.push(Frame::Choose { gap, index });
                    }
                }
                chooses = [None, None];
                continue;
            }
            count += 1;
            if count > MAX_FRAMES {
                return Err(Corrupt);
            }
            if kind != F_CHOOSE {
                chooses = [None, None];
            }
            let frame = match kind {
                F_WAIT => Frame::Wait {
                    gap: c.varint()?,
                    task: self.task(head, &mut c)?,
                },
                F_ISSUE => {
                    let task = self.task(head, &mut c)?;
                    let op = c.u8()?;
                    let object = c.varint()?;
                    let args = Args::decode(&mut c)?;
                    if matches!(
                        op,
                        op::UNIT_SPAWN
                            | op::CONN_TRY_SEND
                            | op::UNIT_CANCEL
                            | op::PORT_REPLY
                            | op::PORT_FILL
                    ) || call_part(op)
                        || (op == op::CONN_SEND && args.recorded_spare().is_some())
                    {
                        return Err(Corrupt);
                    }
                    if op == op::CONN_SEND {
                        self.positions
                            .insert(self.position.checked_add(1).ok_or(Corrupt)?, task);
                    }
                    if self.issued.insert(task, op).is_some() {
                        return Err(Corrupt);
                    }
                    Frame::Issue {
                        task,
                        op,
                        object,
                        args,
                    }
                }
                F_DONE => {
                    let gap = c.varint()?;
                    let task = self.task(head, &mut c)?;
                    let op = self.issued.remove(&task).ok_or(Corrupt)?;
                    if op == op::CONN_SEND {
                        return Err(Corrupt);
                    }
                    let (out, needs) = Out::decode(op, &mut c)?;
                    let f = Frame::Done { gap, task, out };
                    let Some(frame) = self.complete(f, needs)? else {
                        continue;
                    };
                    frame
                }
                F_OP => {
                    let task = self.task(head, &mut c)?;
                    let op = c.u8()?;
                    let object = c.varint()?;
                    let args = Args::decode(&mut c)?;
                    if (matches!(
                        op,
                        op::UNIT_SPAWN | op::CONN_SEND | op::CONN_TRY_SEND | op::UNIT_CANCEL
                    ) && args.recorded_spare().is_some())
                        || op == op::PORT_NEXT
                        || call_part(op)
                        || (op == op::UNIT_SPAWN && object != 0)
                        || (op == op::UNIT_CANCEL && args != Args::Literal(Vec::new()))
                    {
                        return Err(Corrupt);
                    }
                    let (out, needs) = Out::decode(op, &mut c)?;
                    let f = Frame::Op {
                        task,
                        op,
                        object,
                        args,
                        out,
                    };
                    let Some(frame) = self.complete(f, needs)? else {
                        continue;
                    };
                    frame
                }
                F_WAKE => {
                    let gap = c.varint()?;
                    Frame::Wake {
                        gap,
                        task: self.task(head, &mut c)?,
                    }
                }
                F_CANCEL => {
                    let gap = c.varint()?;
                    Frame::Cancel {
                        gap,
                        task: self.task(head, &mut c)?,
                    }
                }
                F_RETIRED => {
                    let gap = c.varint()?;
                    let task = self.task(head, &mut c)?;
                    let pool = c.varint()?;
                    if pool < 2 {
                        return Err(Corrupt);
                    }
                    Frame::Retired { gap, task, pool }
                }
                F_CHOOSE => {
                    let (gap, index) = (c.varint()?, c.varint()?);
                    chooses = [chooses[1], Some((gap, index))];
                    Frame::Choose { gap, index }
                }
                F_GRANT => {
                    let gap = c.varint()?;
                    let kind = c.u8()?;
                    if kind > SECRET_KIND && !matches!(kind, EXEC_KIND | CRED_KIND) {
                        return Err(Corrupt);
                    }
                    if kind == EXEC_KIND {
                        let needs = usize::try_from(c.varint()?).map_err(|_| Corrupt)?;
                        let chunk = c.bytes()?;
                        if chunk.is_empty() || chunk.len() > PAYLOAD_CHUNK {
                            return Err(Corrupt);
                        }
                        let f = Frame::Grant {
                            gap,
                            kind,
                            args: Some(chunk.to_vec()),
                        };
                        let Some(frame) = self.complete(f, needs)? else {
                            continue;
                        };
                        self.position = self.position.checked_add(1).ok_or(Corrupt)?;
                        frames.push(frame);
                        continue;
                    }
                    let args = if kind == ARGS_KIND {
                        Some(c.bytes()?.to_vec())
                    } else if is_stream_grant(kind) {
                        let terminal = c.u8()?;
                        if terminal > 1 {
                            return Err(Corrupt);
                        }
                        Some(vec![terminal])
                    } else {
                        None
                    };
                    Frame::Grant { gap, kind, args }
                }
                F_DATA => {
                    let chunk = c.bytes()?;
                    if chunk.is_empty() || chunk.len() > PAYLOAD_CHUNK {
                        return Err(Corrupt);
                    }
                    if let Some(mut pending) = self.carrier.take() {
                        pending.add(chunk)?;
                        let Some(frame) = self.complete_carrier(pending)? else {
                            continue;
                        };
                        frame
                    } else {
                        let (mut f, needs) = self.open.take().ok_or(Corrupt)?;
                        f.continued_payload().extend_from_slice(chunk);
                        let Some(frame) = self.complete(f, needs)? else {
                            continue;
                        };
                        frame
                    }
                }
                F_TAKE => {
                    let kind = CarrierKind::Take {
                        gap: c.varint()?,
                        point: TakePoint::read(&mut c)?,
                        count: c.varint()?,
                    };
                    let pending = OpenCarrier::start(kind, &mut c)?;
                    let Some(frame) = self.complete_carrier(pending)? else {
                        continue;
                    };
                    frame
                }
                F_END => {
                    let gap = c.varint()?;
                    let status = c.u8()?;
                    let kind = c.u8()?;
                    if status != END_COMPLETE || kind > END_DEADLOCK {
                        return Err(Corrupt);
                    }
                    if self.helper {
                        let pending = OpenCarrier::start(CarrierKind::End { gap, kind }, &mut c)?;
                        let Some(frame) = self.complete_carrier(pending)? else {
                            continue;
                        };
                        if !end || !c.done() {
                            return Err(Corrupt);
                        }
                        self.position = self.position.checked_add(1).ok_or(Corrupt)?;
                        frames.push(frame);
                        continue;
                    }
                    if !end {
                        return Err(Corrupt);
                    }
                    let ending = Ending {
                        kind,
                        outcome: Args::decode(&mut c)?,
                    };
                    if !c.done() || (kind == END_DEADLOCK && ending.deadlock_events().is_none()) {
                        return Err(Corrupt);
                    }
                    Frame::End { gap, ending }
                }
                _ => return Err(Corrupt),
            };
            if matches!(frame, Frame::HelperEnd { .. }) && (!end || !c.done()) {
                return Err(Corrupt);
            }
            self.position = self.position.checked_add(1).ok_or(Corrupt)?;
            frames.push(frame);
        }
        if end
            && !matches!(
                frames.last(),
                Some(Frame::End { .. } | Frame::HelperEnd { .. })
            )
        {
            return Err(Corrupt);
        }
        Ok(frames)
    }
    /// Keep a frame whose payload is short of `needs` until its DATA frames arrive.
    fn complete(&mut self, mut f: Frame, needs: usize) -> Result<Option<Frame>, Corrupt> {
        match f.continued_payload().len().cmp(&needs) {
            std::cmp::Ordering::Less => {
                self.open = Some((f, needs));
                Ok(None)
            }
            std::cmp::Ordering::Equal => Ok(Some(f)),
            std::cmp::Ordering::Greater => Err(Corrupt),
        }
    }
    /// A frame left waiting for its payload when the blocks stop.
    pub fn unfinished(&self) -> bool {
        self.open.is_some() || self.carrier.is_some()
    }
}

fn call_part(op: u8) -> bool {
    matches!(
        op,
        op::CALL_WORD | op::CALL_PAYLOAD | op::CALL_PAYLOAD_PREFIX
    )
}
