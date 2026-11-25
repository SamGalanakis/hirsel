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
mod exec;
mod frames;
#[cfg(test)]
mod generator_tests;
mod index;
mod payload;
mod units;
pub use arguments::Args;
pub use blocks::{
    Blocks, Header, HeaderTooLarge, ReadError, Stop, TAPE_ID_AT, block, cred_snapshot,
    env_snapshot, net_snapshot, run_route, seal, stream_block,
};
pub use exec::exec_snapshot_rules;
pub use frames::{Decoder, Encoder};
pub use index::{Index, StreamBlocks};
pub use payload::{carries_payload, tls_accept_payload, tls_connect_payload, tls_hello_payload};
pub use units::{
    Arrival, CanonicalValue, Cause, CauseDetail, Credit, Entry, EntryBody, HelperEnding,
    HelperOutcome, Settlement, Survivor, TakeBody, TakePoint, Value,
};

use std::hash::Hasher;
use twox_hash::XxHash3_64;

pub const FORMAT: u16 = 14;
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
/// flags, magic, stream, seq, len.
pub const BLOCK_HEAD: usize = 1 + 3 + 4 + 8 + 4;
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
pub const F_TAKE: u8 = 14;
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
    pub fn u32(&mut self) -> Result<u32, Corrupt> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }
    pub fn u64(&mut self) -> Result<u64, Corrupt> {
        Ok(u64::from_le_bytes(self.fixed()?))
    }
    /// Canonical unsigned LEB128 with at most 10 bytes.
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
/// The nine call words that lead a delivered call's `PORT_NEXT` payload.
const PORT_CALL_WORDS: usize = 9 * 8;
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
        WRITE
            | WRITE_AT
            | STDOUT
            | STDERR
            | RANDOM
            | NEXT_ENTRIES
            | DATAGRAM_RECV
            | UDP_RECV
            | PORT_NEXT
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
        if op == op::PORT_NEXT {
            units::put_carrier(out, &self.payload, rest);
        } else if carries_payload(op) {
            put_payload(out, &self.payload, rest);
        }
    }
    fn decode(op: u8, c: &mut Cursor) -> Result<(Self, usize), Corrupt> {
        let status = c.u8()?;
        if status > TIMED_OUT || (op == op::PORT_NEXT && status != OK) {
            return Err(Corrupt);
        }
        let value = c.varint()?;
        let lit_max = match op {
            op::UNIT_SPAWN if value == 0 => return Err(Corrupt),
            op::UNIT_SPAWN => u64::from(u32::MAX),
            op::CONN_SEND => 1,
            op::CONN_TRY_SEND => 2,
            op::UNIT_CANCEL => 0,
            _ => u64::MAX,
        };
        if lit_max != u64::MAX && (status != OK || value > lit_max) {
            return Err(Corrupt);
        }
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
        let port_total = (op == op::PORT_NEXT)
            .then(|| usize::try_from(c.varint()?).map_err(|_| Corrupt))
            .transpose()?;
        // `written` 0 is a `next` answered `None`; 1 is a call whose payload
        // leads with its nine words.
        if let Some(total) = port_total
            && match written {
                0 => value != 0 || total != 0,
                1 => total < PORT_CALL_WORDS,
                _ => true,
            }
        {
            return Err(Corrupt);
        }
        let payload = if port_total.is_some() || carries_payload(op) {
            let b = c.bytes()?;
            if b.len() > PAYLOAD_CHUNK
                || port_total.is_some_and(|total| b.len() > total || b.is_empty() != (total == 0))
            {
                return Err(Corrupt);
            }
            if matches!(op, op::LISTEN | op::ACCEPT | op::LOCAL_ADDR | op::PEER_ADDR) {
                if status == OK {
                    payload::address(b)?;
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
            if matches!(op, op::TLS_CONNECT | op::TLS_HELLO | op::TLS_ACCEPT) {
                if status == OK {
                    let valid = match op {
                        op::TLS_CONNECT => tls_connect_payload(b).is_ok(),
                        op::TLS_HELLO => tls_hello_payload(b).is_ok(),
                        _ => tls_accept_payload(b).is_ok(),
                    };
                    if !valid {
                        return Err(Corrupt);
                    }
                } else if !b.is_empty() {
                    return Err(Corrupt);
                }
            }
            b.to_vec()
        } else {
            vec![]
        };
        let out = Self {
            status,
            value,
            error,
            written,
            payload,
        };
        let total = match port_total {
            Some(total) => total,
            None => out.needs(op)?,
        };
        Ok((out, total))
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
    Take {
        gap: u64,
        point: TakePoint,
        count: u64,
        body: TakeBody,
    },
    End {
        gap: u64,
        ending: Ending,
    },
    HelperEnd {
        gap: u64,
        ending: HelperEnding,
    },
}

fn put_payload<'a>(out: &mut Vec<u8>, payload: &'a [u8], rest: &mut Vec<&'a [u8]>) {
    let first = payload.len().min(PAYLOAD_CHUNK);
    put_bytes(out, &payload[..first]);
    rest.extend(payload[first..].chunks(PAYLOAD_CHUNK));
}

/// The Args grant kind in the entry ABI.
pub const ARGS_KIND: u8 = 10;

pub use hirsel_core_ids::host_op as op;
