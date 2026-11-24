//! The tape format, byte by byte (REPLAY-SPEC section 5): little-endian
//! fixed-width integers, canonical LEB128 varints, checked blocks, the header
//! (block 0's payload) and the frames of every later block.
//!
//! A tape file is outside input: every byte the reader hands on has passed its
//! block's xxh3-64 check, which covers the tape id, the block's flags, magic,
//! sequence number and length, so a block from another tape or another place
//! fails. Discovery is sequential from offset 0 and never resynchronises.
#[cfg(test)]
hirsel_guard::install!();

mod arguments;
mod blocks;
mod payload;
pub use arguments::Args;
pub use blocks::{
    Blocks, Header, HeaderTooLarge, ReadError, Stop, TAPE_ID_AT, block, run_route, seal,
};
pub use payload::{carries_payload, tls_connect_payload};

use std::hash::Hasher;
use twox_hash::XxHash3_64;

pub const FORMAT: u16 = 13;
pub const MAGIC: [u8; 3] = *b"HTB";
/// The tape's last block.
pub const END: u8 = 2;
/// Block flag: the recording was cut here (its writer could not keep up).
/// The block has an empty payload and ends the recording; a reader stops
/// with `Stop::Truncated`.
pub const TRUNCATED: u8 = 4;
/// The most payload a reader buffers before delivering (section 5.2).
pub const MAX_LEN: usize = 1 << 20;
/// A writer closes its block once this much payload is buffered.
pub const CLOSE_AT: usize = 64 * 1024;
/// A REPEAT cannot make a writer keep more frames than 64 KiB of CHOOSE.
pub const CLOSE_FRAMES: usize = CLOSE_AT / 3;
/// The most frames a reader accepts in one block, after expanding REPEAT.
pub const MAX_FRAMES: usize = MAX_LEN;
/// flags, magic, seq, len.
pub const BLOCK_HEAD: usize = 1 + 3 + 8 + 4;
pub const BLOCK_CHECK: usize = 8;
/// The most payload one frame carries; the rest continues in DATA frames.
pub const PAYLOAD_CHUNK: usize = 64 * 1024;
/// Arguments longer than this are stored as their xxh3-64 digest.
pub const ARGS_LITERAL: usize = 16;

pub const KIND_RUN: u8 = 1;
pub const KIND_CALL: u8 = 2;
pub const CHOOSER_FIFO: u8 = 0;
pub const CHOOSER_COMMIT: u8 = 2;

pub const F_ISSUE: u8 = 1;
pub const F_DONE: u8 = 2;
pub const F_OP: u8 = 3;
pub const F_WAKE: u8 = 4;
pub const F_CANCEL: u8 = 5;
pub const F_CHOOSE: u8 = 6;
pub const F_GRANT: u8 = 7;
pub const F_DATA: u8 = 9;
pub const F_END: u8 = 10;
pub const F_WAIT: u8 = 11;
/// A positive count of CHOOSE frames, each equal to the one two frames back.
pub const F_REPEAT: u8 = 12;
pub const F_RETIRED: u8 = 13;
/// The head bit saying a task varint follows.
pub const T: u8 = 0x10;

pub const OK: u8 = 0;
pub const ERROR: u8 = 1;
pub const ABORTED: u8 = 2;
pub const TIMED_OUT: u8 = 3;

/// END's status: the recording reached the end of the run.
pub const END_COMPLETE: u8 = 0;

/// END's outcome kinds (v2.3 R9).
pub const END_RETURNED: u8 = 0;
pub const END_FAILED: u8 = 1;
pub const END_CANCELLED: u8 = 2;
pub const END_DEADLOCK: u8 = 3;

/// What a complete END frame records: the domain's outcome kind and its
/// bytes in the argument encoding (v2.3 R9). Replay compares it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ending {
    pub kind: u8,
    pub outcome: Args,
}
impl Ending {
    /// `Returned`: a Call's returned bytes, or a Run's exit status as u32 LE.
    pub fn returned(bytes: &[u8]) -> Self {
        Self {
            kind: END_RETURNED,
            outcome: Args::of(bytes),
        }
    }
    /// A deadlocked domain records its total number of committed events.
    pub fn deadlock(events: u64) -> Self {
        let mut bytes = Vec::new();
        put_varint(&mut bytes, events);
        Self {
            kind: END_DEADLOCK,
            outcome: Args::Literal(bytes),
        }
    }

    /// The event count of a canonical Deadlock outcome.
    pub fn deadlock_events(&self) -> Option<u64> {
        if self.kind != END_DEADLOCK {
            return None;
        }
        let Args::Literal(bytes) = &self.outcome else {
            return None;
        };
        let mut cursor = Cursor::new(bytes);
        let count = cursor.varint().ok()?;
        cursor.done().then_some(count)
    }

    /// `Failed` or `Cancelled`: `varint count`, then per cause, primary
    /// first, its kind (core.hirsel order) and, for `Fail`, its text. Sites
    /// are not included.
    pub fn causes<'a>(kind: u8, causes: impl IntoIterator<Item = (u8, Option<&'a [u8]>)>) -> Self {
        let causes: Vec<_> = causes.into_iter().collect();
        let mut out = Vec::new();
        put_varint(&mut out, causes.len() as u64);
        for (k, text) in causes {
            out.push(k);
            if let Some(t) = text {
                put_bytes(&mut out, t);
            }
        }
        Self {
            kind,
            outcome: Args::of(&out),
        }
    }
}

pub fn xxh3(parts: &[&[u8]]) -> u64 {
    let mut h = XxHash3_64::with_seed(0);
    for p in parts {
        h.write(p);
    }
    h.finish()
}

/// A CHOOSE carries no task, so it needs no encoder state.
pub fn put_choose(out: &mut Vec<u8>, gap: u64, index: u64) {
    out.push(F_CHOOSE);
    put_varint(out, gap);
    put_varint(out, index);
}

pub fn put_repeat(out: &mut Vec<u8>, count: u64) {
    out.push(F_REPEAT);
    put_varint(out, count);
}

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
pub fn zz(d: i64) -> u64 {
    ((d << 1) ^ (d >> 63)) as u64
}
pub fn unzz(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}
fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

/// Bytes inside a validated block that break the grammar: `Corrupt`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Corrupt;

/// A cursor over one validated payload. Running out of bytes is `Corrupt`:
/// the block's length already said where the payload ends.
pub struct Cursor<'a> {
    pub bytes: &'a [u8],
    pub at: usize,
}
impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    pub fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
    pub fn u8(&mut self) -> Result<u8, Corrupt> {
        let b = *self.bytes.get(self.at).ok_or(Corrupt)?;
        self.at += 1;
        Ok(b)
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Corrupt> {
        let end = self.at.checked_add(n).ok_or(Corrupt)?;
        let b = self.bytes.get(self.at..end).ok_or(Corrupt)?;
        self.at = end;
        Ok(b)
    }
    pub fn fixed<const N: usize>(&mut self) -> Result<[u8; N], Corrupt> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    /// Canonical unsigned LEB128 (section 5.1): at most 10 bytes, no overlong
    /// final 0x00 byte, a 10th byte of at most 1.
    pub fn varint(&mut self) -> Result<u64, Corrupt> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            if i == 9 && b > 1 {
                return Err(Corrupt);
            }
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                if i > 0 && b == 0 {
                    return Err(Corrupt);
                }
                return Ok(v);
            }
        }
        Err(Corrupt)
    }
    pub fn bytes(&mut self) -> Result<&'a [u8], Corrupt> {
        let n = self.varint()?;
        self.take(usize::try_from(n).map_err(|_| Corrupt)?)
    }
}

/// Stream grants carry a single canonical terminal bit in a GRANT frame.
pub const fn is_stream_grant(kind: u8) -> bool {
    matches!(kind, 4..=6)
}

const SECRET_KIND: u8 = 11;
const EXEC_KIND: u8 = 13;
const CRED_KIND: u8 = 14;

/// Append the DATA frame continuing a payload with `chunk`.
pub fn put_data(out: &mut Vec<u8>, chunk: &[u8]) {
    out.push(F_DATA);
    put_bytes(out, chunk);
}

/// Whether the operation encodes an extra byte count in its outcome.
pub fn writes(op: u8) -> bool {
    use crate::op::*;
    matches!(
        op,
        WRITE | WRITE_AT | STDOUT | STDERR | RANDOM | NEXT_ENTRIES | DATAGRAM_RECV | UDP_RECV
    )
}

/// An outcome as the tape stores it. `value` is stored as given: the recorder
/// passes a clock reading as its zig-zag delta and a new handle as its ordinal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Out {
    pub status: u8,
    pub value: u64,
    /// Error tag and code, present exactly when `status` is ERROR.
    pub error: (u8, u64),
    pub written: u64,
    /// Delivered bytes, directory records, metadata, socket or TLS data.
    pub payload: Vec<u8>,
}
impl Out {
    /// The frame bytes, then the payload chunks its DATA frames continue with.
    fn encode<'a>(&'a self, op: u8, out: &mut Vec<u8>, rest: &mut Vec<&'a [u8]>) {
        out.push(self.status);
        put_varint(out, self.value);
        if self.status == ERROR {
            out.push(self.error.0);
            put_varint(out, self.error.1);
        }
        if writes(op) || (matches!(op, op::SEND_SECRET | op::SCRAM) && self.status != OK) {
            put_varint(out, self.written);
        }
        if carries_payload(op) {
            put_payload(out, &self.payload, rest);
        }
    }
    fn decode(op: u8, c: &mut Cursor) -> Result<Self, Corrupt> {
        let status = c.u8()?;
        if status > TIMED_OUT {
            return Err(Corrupt);
        }
        let value = c.varint()?;
        if matches!(op, op::MEDIA_P256_KEY | op::MEDIA_KEY_SHARE) && (status != OK || value == 0) {
            return Err(Corrupt);
        }
        if op == op::MEDIA_CAPTURE && status != OK && status != ERROR {
            return Err(Corrupt);
        }
        if op == op::MEDIA_AGREEMENT && (status != OK || value > 2) {
            return Err(Corrupt);
        }
        if op == op::SCRAM_VERIFY && (status != OK || value > 1) {
            return Err(Corrupt);
        }
        if op == op::SCRAM && (status == OK) != (value != 0) {
            return Err(Corrupt);
        }
        let error = if status == ERROR {
            (c.u8()?, c.varint()?)
        } else {
            (0, 0)
        };
        if op == op::SECRET_KDF
            && !((status == OK && value != 0) || (status == ERROR && error == (1, 0) && value == 0))
        {
            return Err(Corrupt);
        }
        if op == op::STDIN_READ
            && status != OK
            && (value != 0
                || (status == ERROR && (error.0 != 8 || u32::try_from(error.1).is_err())))
        {
            return Err(Corrupt);
        }
        if (error.0 == 10 && op != op::UNIX_LISTEN)
            || (error.0 == 11 && (op != op::LISTEN || error.1 != 0))
        {
            return Err(Corrupt);
        }
        let written = if writes(op) || (matches!(op, op::SEND_SECRET | op::SCRAM) && status != OK) {
            c.varint()?
        } else {
            0
        };
        let payload = if carries_payload(op) {
            let b = c.bytes()?;
            if b.len() > PAYLOAD_CHUNK {
                return Err(Corrupt);
            }
            if matches!(op, op::LISTEN | op::ACCEPT | op::LOCAL_ADDR | op::PEER_ADDR) {
                if status == OK {
                    let [family, rest @ ..] = b else {
                        return Err(Corrupt);
                    };
                    if b.len() != 19
                        || *family > 1
                        || (*family == 0 && rest[..8].iter().chain(&rest[12..16]).any(|&n| n != 0))
                    {
                        return Err(Corrupt);
                    }
                } else if !b.is_empty() {
                    return Err(Corrupt);
                }
            }
            if op == op::MKSTEMP
                && if status == OK {
                    !hirsel_core_ids::host_op::valid_temp_suffix(b)
                } else {
                    !b.is_empty()
                }
            {
                return Err(Corrupt);
            }
            if op == op::TLS_CONNECT {
                if status == OK {
                    tls_connect_payload(b)?;
                } else if !b.is_empty() {
                    return Err(Corrupt);
                }
            }
            b.to_vec()
        } else {
            vec![]
        };
        Ok(Self {
            status,
            value,
            error,
            written,
            payload,
        })
    }
    /// Total payload length determines whether DATA frames follow.
    fn needs(&self, op: u8) -> Result<usize, Corrupt> {
        match op {
            op::READ | op::READ_AT | op::RECV_BOUNDED | op::STDIN_READ if self.status == OK => {
                usize::try_from(self.value).map_err(|_| Corrupt)
            }
            op::RESOLVE if self.status == OK => usize::try_from(self.value)
                .ok()
                .and_then(|n| n.checked_mul(hirsel_core_net::IP_RECORD_LEN))
                .ok_or(Corrupt),
            op::RESOLVE if self.value != 0 => Err(Corrupt),
            op::DATAGRAM_RECV if self.status == OK => usize::try_from(self.written)
                .ok()
                .and_then(|count| count.checked_add(1))
                .ok_or(Corrupt),
            op::UDP_RECV if self.status == OK => usize::try_from(self.value)
                .ok()
                .and_then(|count| count.checked_mul(36))
                .and_then(|records| records.checked_add(usize::try_from(self.written).ok()?))
                .ok_or(Corrupt),
            op::UDP_RECV => Ok(0),
            op::DATAGRAM_RECV | op::RECV_BOUNDED | op::RESOLVE | op::STDIN_READ => Ok(0),
            op::RANDOM | op::NEXT_ENTRIES => usize::try_from(self.written).map_err(|_| Corrupt),
            _ => Ok(self.payload.len()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Frame {
    Wait {
        gap: u64,
        task: u64,
    },
    Issue {
        task: u64,
        op: u8,
        object: u64,
        args: Args,
    },
    Done {
        gap: u64,
        task: u64,
        out: Out,
    },
    Op {
        task: u64,
        op: u8,
        object: u64,
        args: Args,
        out: Out,
    },
    Wake {
        gap: u64,
        task: u64,
    },
    Cancel {
        gap: u64,
        task: u64,
    },
    Retired {
        gap: u64,
        task: u64,
        pool: u64,
    },
    Choose {
        gap: u64,
        index: u64,
    },
    /// Args carries the argument vector. Exec carries the logical profiles.
    Grant {
        gap: u64,
        kind: u8,
        args: Option<Vec<u8>>,
    },
    /// Physical output only. Replay sees the CHOOSE frames it represents.
    Repeat {
        count: u64,
    },
    End {
        gap: u64,
        ending: Ending,
    },
}

fn put_payload<'a>(out: &mut Vec<u8>, payload: &'a [u8], rest: &mut Vec<&'a [u8]>) {
    let first = payload.len().min(PAYLOAD_CHUNK);
    put_bytes(out, &payload[..first]);
    rest.extend(payload[first..].chunks(PAYLOAD_CHUNK));
}

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
    issued: std::collections::BTreeMap<u64, u8>,
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
    issued: std::collections::BTreeMap<u64, u8>,
    /// A frame whose payload still waits for DATA frames: (frame, bytes needed).
    open: Option<(Frame, usize)>,
    /// Return REPEAT frames as stored. By default they expand to CHOOSE.
    pub physical: bool,
}
impl Decoder {
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
            if self.open.is_some() && kind != F_DATA {
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
                    let out = Out::decode(op, &mut c)?;
                    let needs = out.needs(op)?;
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
                    let out = Out::decode(op, &mut c)?;
                    let needs = out.needs(op)?;
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
                    let Some((mut f, needs)) = self.open.take() else {
                        return Err(Corrupt);
                    };
                    if chunk.is_empty() || chunk.len() > PAYLOAD_CHUNK {
                        return Err(Corrupt);
                    }
                    f.continued_payload().extend_from_slice(chunk);
                    let Some(frame) = self.complete(f, needs)? else {
                        continue;
                    };
                    frame
                }
                F_END => {
                    let gap = c.varint()?;
                    let status = c.u8()?;
                    let kind = c.u8()?;
                    if !end || status != END_COMPLETE || kind > END_DEADLOCK {
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
            frames.push(frame);
        }
        if end && !matches!(frames.last(), Some(Frame::End { .. })) {
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
        self.open.is_some()
    }
}

/// The Args grant kind in the entry ABI.
pub const ARGS_KIND: u8 = 10;

pub use hirsel_core_ids::host_op as op;
