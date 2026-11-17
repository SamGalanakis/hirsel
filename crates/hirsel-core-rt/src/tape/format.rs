//! The tape format, byte by byte (REPLAY-SPEC v2 section 5): little-endian
//! fixed-width integers, canonical LEB128 varints, checked blocks, the header
//! (block 0's payload) and the frames of every later block.
//!
//! A tape file is outside input: every byte the reader hands on has passed its
//! block's xxh3-64 check, which covers the tape id, the block's flags, magic,
//! sequence number and length, so a block from another tape or another place
//! fails. Discovery is sequential from offset 0 and never resynchronises.
use std::hash::Hasher;
use twox_hash::XxHash3_64;

pub const FORMAT: u16 = 2;
pub const MAGIC: [u8; 3] = *b"HTB";
/// Block flags: closed for a durable barrier; the tape's last block.
pub const BARRIER: u8 = 1;
pub const END: u8 = 2;
/// The most payload a reader buffers before delivering (section 5.2).
pub const MAX_LEN: usize = 1 << 20;
/// A writer closes its block once this much payload is buffered.
pub const CLOSE_AT: usize = 64 * 1024;
/// flags, magic, seq, len.
pub const BLOCK_HEAD: usize = 1 + 3 + 8 + 4;
pub const BLOCK_CHECK: usize = 8;
/// The most payload one frame carries; the rest continues in DATA frames.
pub const PAYLOAD_CHUNK: usize = 64 * 1024;
/// Arguments longer than this are stored as their xxh3-64 digest.
pub const ARGS_LITERAL: usize = 16;

pub const KIND_RUN: u8 = 1;
pub const KIND_CALL: u8 = 2;
pub const MODE_RECORD: u8 = 1;
pub const MODE_DURABLE: u8 = 2;
pub const CHOOSER_FIFO: u8 = 0;

pub const F_ISSUE: u8 = 1;
pub const F_DONE: u8 = 2;
pub const F_OP: u8 = 3;
pub const F_WAKE: u8 = 4;
pub const F_CANCEL: u8 = 5;
pub const F_CHOOSE: u8 = 6;
pub const F_GRANT: u8 = 7;
pub const F_INCARNATION: u8 = 8;
pub const F_DATA: u8 = 9;
pub const F_END: u8 = 10;
pub const F_WAIT: u8 = 11;
/// The head bit saying a task varint follows.
pub const T: u8 = 0x10;

pub const OK: u8 = 0;
pub const ERROR: u8 = 1;
pub const ABORTED: u8 = 2;
pub const TIMED_OUT: u8 = 3;

/// END's status: the recording reached `tape.end`.
pub const END_COMPLETE: u8 = 0;

/// END's outcome kinds (v2.3 R9).
pub const END_RETURNED: u8 = 0;
pub const END_FAILED: u8 = 1;
pub const END_CANCELLED: u8 = 2;

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

/// Block 0's payload (section 5.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub kind: u8,
    pub mode: u8,
    pub tape_id: [u8; 16],
    pub identity: Vec<u8>,
    /// The code's revision: the entry's code revision for a Run tape, the
    /// called function's for a Call tape (format 2, gavel 7). No compiler,
    /// backend or runtime identity is in it.
    pub revision: [u8; 32],
    pub charge_policy: u32,
    pub limit: u64,
    pub bound: u64,
    pub chooser: u8,
    pub arg: Vec<u8>,
    pub grants: Vec<u8>,
}
impl Header {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.push(self.kind);
        out.push(self.mode);
        out.extend_from_slice(&self.tape_id);
        put_bytes(&mut out, &self.identity);
        out.extend_from_slice(&self.revision);
        out.extend_from_slice(&self.charge_policy.to_le_bytes());
        out.extend_from_slice(&self.limit.to_le_bytes());
        out.extend_from_slice(&self.bound.to_le_bytes());
        out.push(self.chooser);
        put_bytes(&mut out, &self.arg);
        put_varint(&mut out, self.grants.len() as u64);
        out.extend_from_slice(&self.grants);
        out
    }
    pub fn decode(payload: &[u8]) -> Result<Self, Corrupt> {
        let mut c = Cursor::new(payload);
        if u16::from_le_bytes(c.fixed()?) != FORMAT {
            return Err(Corrupt);
        }
        let kind = c.u8()?;
        let mode = c.u8()?;
        if !matches!(kind, KIND_RUN | KIND_CALL) || !matches!(mode, MODE_RECORD | MODE_DURABLE) {
            return Err(Corrupt);
        }
        let tape_id = c.fixed()?;
        let identity = c.bytes()?.to_vec();
        let revision = c.fixed()?;
        let charge_policy = u32::from_le_bytes(c.fixed()?);
        let limit = u64::from_le_bytes(c.fixed()?);
        let bound = u64::from_le_bytes(c.fixed()?);
        let chooser = c.u8()?;
        if chooser > 1 {
            return Err(Corrupt);
        }
        let arg = c.bytes()?.to_vec();
        let count = c.varint()?;
        let grants = c
            .take(usize::try_from(count).map_err(|_| Corrupt)?)?
            .to_vec();
        if (kind == KIND_CALL && !grants.is_empty()) || !c.done() {
            return Err(Corrupt);
        }
        Ok(Self {
            kind,
            mode,
            tape_id,
            identity,
            revision,
            charge_policy,
            limit,
            bound,
            chooser,
            arg,
            grants,
        })
    }
}
/// Where block 0's payload keeps the tape id: after format, kind and mode.
pub const TAPE_ID_AT: usize = 4;

/// One block's bytes: head, payload, then the check over the tape id and
/// everything before the check.
pub fn block(tape_id: &[u8; 16], flags: u8, seq: u64, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= MAX_LEN);
    let mut out = Vec::with_capacity(BLOCK_HEAD + payload.len() + BLOCK_CHECK);
    out.push(flags);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    let check = xxh3(&[tape_id, &out]);
    out.extend_from_slice(&check.to_le_bytes());
    out
}

/// Why reading stopped before the tape's END block (section 5.2's table).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    /// End of file exactly at a block boundary, with no END block read.
    Missing,
    /// A trailing partial block.
    Truncated,
    /// A complete block failing a check, or anything after an END block.
    Damaged(u64),
    /// A validated block whose bytes break the grammar.
    Corrupt,
}
impl From<Corrupt> for Stop {
    fn from(_: Corrupt) -> Self {
        Stop::Corrupt
    }
}

/// Sequential block discovery from offset 0 over a tape's bytes, which every
/// call passes in (the reader owns them; this is only the position).
pub struct Blocks {
    at: usize,
    seq: u64,
    tape_id: [u8; 16],
    ended: bool,
}
impl Blocks {
    /// Read block 0 and decode the header. Block 0 carries flags 0.
    pub fn open(bytes: &[u8]) -> Result<(Self, Header), Stop> {
        let (flags, payload, next) = frame(bytes, 0, 0)?;
        let Some(id) = payload.get(TAPE_ID_AT..TAPE_ID_AT + 16) else {
            return Err(Stop::Damaged(0));
        };
        let tape_id: [u8; 16] = id.try_into().unwrap();
        check(bytes, 0, payload.len(), &tape_id, 0)?;
        if flags != 0 {
            return Err(Stop::Damaged(0));
        }
        let header = Header::decode(payload)?;
        Ok((
            Self {
                at: next,
                seq: 1,
                tape_id,
                ended: false,
            },
            header,
        ))
    }
    /// The next validated block's flags and payload, `None` after the END
    /// block at the end of the file.
    pub fn next<'a>(&mut self, bytes: &'a [u8]) -> Result<Option<(u8, &'a [u8])>, Stop> {
        if self.ended {
            if self.at == bytes.len() {
                return Ok(None);
            }
            return Err(Stop::Damaged(self.seq));
        }
        let (flags, payload, next) = frame(bytes, self.at, self.seq)?;
        check(bytes, self.at, payload.len(), &self.tape_id, self.seq)?;
        self.at = next;
        self.seq += 1;
        self.ended = flags & END != 0;
        Ok(Some((flags, payload)))
    }
}
/// Block `k` at `at`: its flags, payload and the offset after it, with every
/// check but the checksum (which needs the tape id).
fn frame(bytes: &[u8], at: usize, k: u64) -> Result<(u8, &[u8], usize), Stop> {
    let rest = &bytes[at..];
    if rest.is_empty() {
        return Err(Stop::Missing);
    }
    if rest.len() < BLOCK_HEAD {
        return Err(Stop::Truncated);
    }
    let len = u32::from_le_bytes(rest[12..16].try_into().unwrap()) as usize;
    if len > MAX_LEN {
        return Err(Stop::Damaged(k));
    }
    if rest.len() < BLOCK_HEAD + len + BLOCK_CHECK {
        return Err(Stop::Truncated);
    }
    if rest[1..4] != MAGIC || rest[0] & !(BARRIER | END) != 0 {
        return Err(Stop::Damaged(k));
    }
    Ok((
        rest[0],
        &rest[BLOCK_HEAD..BLOCK_HEAD + len],
        at + BLOCK_HEAD + len + BLOCK_CHECK,
    ))
}
/// Block `k` at `at`: its sequence number is `k` and its check matches.
fn check(bytes: &[u8], at: usize, len: usize, id: &[u8; 16], k: u64) -> Result<(), Stop> {
    let body = &bytes[at..at + BLOCK_HEAD + len];
    let stored = u64::from_le_bytes(
        bytes[at + BLOCK_HEAD + len..at + BLOCK_HEAD + len + BLOCK_CHECK]
            .try_into()
            .unwrap(),
    );
    let seq = u64::from_le_bytes(body[4..12].try_into().unwrap());
    if seq != k || xxh3(&[id, body]) != stored {
        return Err(Stop::Damaged(k));
    }
    Ok(())
}

/// `args` of an ISSUE or OP frame: up to 16 literal bytes, else a digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Args {
    Literal(Vec<u8>),
    Digest(u64),
}
impl Args {
    pub fn of(bytes: &[u8]) -> Self {
        if bytes.len() <= ARGS_LITERAL {
            Args::Literal(bytes.to_vec())
        } else {
            Args::Digest(xxh3(&[bytes]))
        }
    }
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Args::Literal(b) => put_bytes(out, b),
            Args::Digest(d) => {
                out.push(ARGS_LITERAL as u8 + 1);
                out.extend_from_slice(&d.to_le_bytes());
            }
        }
    }
    fn decode(c: &mut Cursor) -> Result<Self, Corrupt> {
        let n = c.varint()?;
        if n as usize <= ARGS_LITERAL {
            Ok(Args::Literal(c.take(n as usize)?.to_vec()))
        } else if n as usize == ARGS_LITERAL + 1 {
            Ok(Args::Digest(u64::from_le_bytes(c.fixed()?)))
        } else {
            Err(Corrupt)
        }
    }
}

/// What an operation of a given op tag carries in its outcome.
pub fn writes(op: u8) -> bool {
    use crate::provider::*;
    matches!(op, WRITE | STDOUT | STDERR)
}
pub fn carries_payload(op: u8) -> bool {
    use crate::provider::*;
    matches!(op, READ | MKDTEMP)
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
    /// Read's bytes (all `value` of them, joined from DATA frames) and
    /// mkdtemp's suffix.
    pub payload: Vec<u8>,
}
impl Out {
    /// The frame bytes, then the DATA frames continuing the payload.
    fn encode(&self, op: u8, out: &mut Vec<u8>, rest: &mut Vec<Vec<u8>>) {
        out.push(self.status);
        put_varint(out, self.value);
        if self.status == ERROR {
            out.push(self.error.0);
            put_varint(out, self.error.1);
        }
        if writes(op) {
            put_varint(out, self.written);
        }
        if carries_payload(op) {
            let first = self.payload.len().min(PAYLOAD_CHUNK);
            put_bytes(out, &self.payload[..first]);
            for chunk in self.payload[first..].chunks(PAYLOAD_CHUNK) {
                let mut d = vec![F_DATA];
                put_bytes(&mut d, chunk);
                rest.push(d);
            }
        }
    }
    fn decode(op: u8, c: &mut Cursor) -> Result<Self, Corrupt> {
        let status = c.u8()?;
        if status > TIMED_OUT {
            return Err(Corrupt);
        }
        let value = c.varint()?;
        let error = if status == ERROR {
            (c.u8()?, c.varint()?)
        } else {
            (0, 0)
        };
        let written = if writes(op) { c.varint()? } else { 0 };
        let payload = if carries_payload(op) {
            let b = c.bytes()?;
            if b.len() > PAYLOAD_CHUNK {
                return Err(Corrupt);
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
    /// How many payload bytes this outcome needs in all: a successful read
    /// carries `value` bytes, everything else only its first chunk.
    fn needs(&self, op: u8) -> usize {
        if op == crate::provider::READ && self.status == OK {
            self.value as usize
        } else {
            self.payload.len()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Frame {
    Wait {
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
    Choose {
        gap: u64,
        index: u64,
    },
    /// Args carries the whole argument vector, each `varint len + bytes`.
    Grant {
        kind: u8,
        args: Option<Vec<u8>>,
    },
    Incarnation(u64),
    End(Ending),
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
        match f {
            Frame::Wait { task } => {
                out.push(F_WAIT);
                self.task(&mut out, 0, *task);
            }
            Frame::Issue {
                task,
                op,
                object,
                args,
            } => {
                out.push(F_ISSUE);
                self.task(&mut out, 0, *task);
                out.push(*op);
                put_varint(&mut out, *object);
                args.encode(&mut out);
                self.issued.insert(*task, *op);
            }
            Frame::Done { gap, task, out: o } => {
                out.push(F_DONE);
                put_varint(&mut out, *gap);
                self.task(&mut out, 0, *task);
                let op = self
                    .issued
                    .remove(task)
                    .expect("DONE follows its task's ISSUE");
                o.encode(op, &mut out, &mut rest);
            }
            Frame::Op {
                task,
                op,
                object,
                args,
                out: o,
            } => {
                out.push(F_OP);
                self.task(&mut out, 0, *task);
                out.push(*op);
                put_varint(&mut out, *object);
                args.encode(&mut out);
                o.encode(*op, &mut out, &mut rest);
            }
            Frame::Wake { gap, task } | Frame::Cancel { gap, task } => {
                out.push(if matches!(f, Frame::Wake { .. }) {
                    F_WAKE
                } else {
                    F_CANCEL
                });
                put_varint(&mut out, *gap);
                self.task(&mut out, 0, *task);
            }
            Frame::Choose { gap, index } => {
                out.push(F_CHOOSE);
                put_varint(&mut out, *gap);
                put_varint(&mut out, *index);
            }
            Frame::Grant { kind, args } => {
                out.push(F_GRANT);
                out.push(*kind);
                if *kind == crate::entry::ARGS {
                    put_bytes(
                        &mut out,
                        args.as_deref().expect("an Args grant carries its vector"),
                    );
                }
            }
            Frame::Incarnation(n) => {
                out.push(F_INCARNATION);
                put_varint(&mut out, *n);
            }
            Frame::End(e) => {
                out.push(F_END);
                out.push(END_COMPLETE);
                out.push(e.kind);
                e.outcome.encode(&mut out);
            }
        }
        (out, rest)
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
        while !c.done() {
            let head = c.u8()?;
            let kind = head & 0x0f;
            if head & 0xe0 != 0 {
                return Err(Corrupt);
            }
            let has_task = matches!(kind, F_ISSUE | F_DONE | F_OP | F_WAKE | F_CANCEL | F_WAIT);
            if head & T != 0 && !has_task {
                return Err(Corrupt);
            }
            if self.open.is_some() && kind != F_DATA {
                return Err(Corrupt);
            }
            let frame = match kind {
                F_WAIT => Frame::Wait {
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
                    let needs = out.needs(op);
                    let f = Frame::Done { gap, task, out };
                    if self.hold(f.clone(), needs)? {
                        continue;
                    }
                    f
                }
                F_OP => {
                    let task = self.task(head, &mut c)?;
                    let op = c.u8()?;
                    let object = c.varint()?;
                    let args = Args::decode(&mut c)?;
                    let out = Out::decode(op, &mut c)?;
                    let needs = out.needs(op);
                    let f = Frame::Op {
                        task,
                        op,
                        object,
                        args,
                        out,
                    };
                    if self.hold(f.clone(), needs)? {
                        continue;
                    }
                    f
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
                F_CHOOSE => Frame::Choose {
                    gap: c.varint()?,
                    index: c.varint()?,
                },
                F_GRANT => {
                    let kind = c.u8()?;
                    if kind > crate::entry::ARGS {
                        return Err(Corrupt);
                    }
                    let args = (kind == crate::entry::ARGS).then(|| c.bytes().map(<[u8]>::to_vec));
                    Frame::Grant {
                        kind,
                        args: args.transpose()?,
                    }
                }
                F_INCARNATION => Frame::Incarnation(c.varint()?),
                F_DATA => {
                    let chunk = c.bytes()?;
                    let Some((mut f, needs)) = self.open.take() else {
                        return Err(Corrupt);
                    };
                    let (Frame::Op { out, .. } | Frame::Done { out, .. }) = &mut f else {
                        unreachable!()
                    };
                    if chunk.is_empty() || chunk.len() > PAYLOAD_CHUNK {
                        return Err(Corrupt);
                    }
                    out.payload.extend_from_slice(chunk);
                    if self.hold(f.clone(), needs)? {
                        continue;
                    }
                    f
                }
                F_END => {
                    let status = c.u8()?;
                    let kind = c.u8()?;
                    if !end || status != END_COMPLETE || kind > END_CANCELLED {
                        return Err(Corrupt);
                    }
                    let outcome = Args::decode(&mut c)?;
                    if !c.done() {
                        return Err(Corrupt);
                    }
                    Frame::End(Ending { kind, outcome })
                }
                _ => return Err(Corrupt),
            };
            frames.push(frame);
        }
        if end && !matches!(frames.last(), Some(Frame::End(_))) {
            return Err(Corrupt);
        }
        Ok(frames)
    }
    /// Keep a frame whose payload is short of `needs` until its DATA frames
    /// arrive; true when it was kept.
    fn hold(&mut self, f: Frame, needs: usize) -> Result<bool, Corrupt> {
        let (Frame::Op { out, .. } | Frame::Done { out, .. }) = &f else {
            unreachable!()
        };
        match out.payload.len().cmp(&needs) {
            std::cmp::Ordering::Less => {
                self.open = Some((f, needs));
                Ok(true)
            }
            std::cmp::Ordering::Equal => Ok(false),
            std::cmp::Ordering::Greater => Err(Corrupt),
        }
    }
    /// A frame left waiting for its payload when the blocks stop.
    pub fn unfinished(&self) -> bool {
        self.open.is_some()
    }
}
