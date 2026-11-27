//! Hirsel value-codec encoder and decoder for `runner.Frame` (tags 0..=11),
//! `Outcome`, and all EMB-4 callback payload types.
//!
//! Matches `core/programs/runner/runner.hirsel`, `core/programs/runner/tests/codec_frames.mjs`,
//! and `docs/spec/embed.md` (EMB-4) byte-for-byte.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeErr(pub &'static str);

impl std::fmt::Display for DecodeErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

pub struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            out: Vec::with_capacity(cap),
        }
    }

    pub fn put_u8(&mut self, v: u8) {
        self.out.push(v);
    }

    pub fn put_bool(&mut self, v: bool) {
        self.out.push(u8::from(v));
    }

    pub fn put_u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_u64(&mut self, v: u64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_bytes(&mut self, b: &[u8]) {
        self.put_u32(b.len() as u32);
        self.out.extend_from_slice(b);
    }

    pub fn put_str(&mut self, s: &str) {
        self.put_bytes(s.as_bytes());
    }

    pub fn put_option<T>(&mut self, opt: Option<&T>, f: impl FnOnce(&mut Self, &T)) {
        match opt {
            None => self.put_u32(0),
            Some(v) => {
                self.put_u32(1);
                f(self, v);
            }
        }
    }

    pub fn put_list<T>(&mut self, list: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.put_u32(list.len() as u32);
        for item in list {
            f(self, item);
        }
    }
}

pub struct Reader<'a> {
    src: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Self { src, at: 0 }
    }

    pub fn pos(&self) -> usize {
        self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeErr> {
        let end = self.at.checked_add(n).ok_or(DecodeErr("overflow"))?;
        let slice = self.src.get(self.at..end).ok_or(DecodeErr("truncated"))?;
        self.at = end;
        Ok(slice)
    }

    pub fn take_u8(&mut self) -> Result<u8, DecodeErr> {
        Ok(self.take(1)?[0])
    }

    pub fn take_bool(&mut self) -> Result<bool, DecodeErr> {
        match self.take_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeErr("invalid bool")),
        }
    }

    pub fn take_u16(&mut self) -> Result<u16, DecodeErr> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn take_u32(&mut self) -> Result<u32, DecodeErr> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn take_u64(&mut self) -> Result<u64, DecodeErr> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn take_bytes(&mut self) -> Result<Vec<u8>, DecodeErr> {
        let len = self.take_u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    pub fn take_str(&mut self) -> Result<String, DecodeErr> {
        let raw = self.take_bytes()?;
        String::from_utf8(raw).map_err(|_| DecodeErr("invalid utf-8"))
    }

    pub fn take_option<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, DecodeErr>,
    ) -> Result<Option<T>, DecodeErr> {
        match self.take_u32()? {
            0 => Ok(None),
            1 => Ok(Some(f(self)?)),
            _ => Err(DecodeErr("invalid option tag")),
        }
    }

    pub fn take_list<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Result<T, DecodeErr>,
    ) -> Result<Vec<T>, DecodeErr> {
        let n = self.take_u32()? as usize;
        // Guard against absurd counts from corrupt frames.
        if n > self.src.len().saturating_sub(self.at) {
            return Err(DecodeErr("truncated list"));
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }

    pub fn done(&self) -> Result<(), DecodeErr> {
        if self.at == self.src.len() {
            Ok(())
        } else {
            Err(DecodeErr("trailing bytes"))
        }
    }
}

// ---------------------------------------------------------------------------
// Wire protocol types (runner.Frame, tags 0..=11)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct StreamId {
    pub t: u64,
    pub n: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Caller {
    pub issuer: u8,
    pub subject: Vec<u8>,
}

impl Caller {
    pub fn encode(&self, w: &mut Writer) {
        w.put_u8(self.issuer);
        w.put_bytes(&self.subject);
    }

    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(5 + self.subject.len());
        self.encode(&mut w);
        w.out
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeErr> {
        Ok(Self {
            issuer: r.take_u8()?,
            subject: r.take_bytes()?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Want {
    pub identity: Vec<u8>,
    pub args_fp: u64,
    pub result_fp: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    Ready(u32),
    Resolve(u32, Vec<u8>),
    Gone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Room {
    pub requested: u64,
    pub available: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bad {
    pub kind: u32,
    pub at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    NoEntry,
    Malformed(Bad),
    Overloaded,
    Deadline,
    Budget(Room),
    NotAllowed,
    RequestMismatch,
    HistoryUnavailable,
    StreamFuture(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    Raised(u32),
    OutOfBytes(Room),
    OutOfTime(u64),
    Gone,
    Conflict(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Returned(Vec<u8>),
    Failed(Fault),
    Refused(Refusal),
    Abandoned,
    InDoubt,
}

impl Outcome {
    pub fn encode(&self, w: &mut Writer) {
        match self {
            Self::Returned(b) => {
                w.put_u32(0);
                w.put_bytes(b);
            }
            Self::Failed(f) => {
                w.put_u32(1);
                match f {
                    Fault::Raised(code) => {
                        w.put_u32(0);
                        w.put_u32(*code);
                    }
                    Fault::OutOfBytes(r) => {
                        w.put_u32(1);
                        w.put_u64(r.requested);
                        w.put_u64(r.available);
                    }
                    Fault::OutOfTime(steps) => {
                        w.put_u32(2);
                        w.put_u64(*steps);
                    }
                    Fault::Gone => w.put_u32(3),
                    Fault::Conflict(s) => {
                        w.put_u32(4);
                        w.put_str(s);
                    }
                }
            }
            Self::Refused(r) => {
                w.put_u32(2);
                match r {
                    Refusal::NoEntry => w.put_u32(0),
                    Refusal::Malformed(b) => {
                        w.put_u32(1);
                        w.put_u32(b.kind);
                        w.put_u64(b.at);
                    }
                    Refusal::Overloaded => w.put_u32(2),
                    Refusal::Deadline => w.put_u32(3),
                    Refusal::Budget(rm) => {
                        w.put_u32(4);
                        w.put_u64(rm.requested);
                        w.put_u64(rm.available);
                    }
                    Refusal::NotAllowed => w.put_u32(5),
                    Refusal::RequestMismatch => w.put_u32(6),
                    Refusal::HistoryUnavailable => w.put_u32(7),
                    Refusal::StreamFuture(p) => {
                        w.put_u32(8);
                        w.put_u64(*p);
                    }
                }
            }
            Self::Abandoned => w.put_u32(3),
            Self::InDoubt => w.put_u32(4),
        }
    }

    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.encode(&mut w);
        w.out
    }

    pub fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeErr> {
        match r.take_u32()? {
            0 => Ok(Self::Returned(r.take_bytes()?)),
            1 => {
                let f = match r.take_u32()? {
                    0 => Fault::Raised(r.take_u32()?),
                    1 => Fault::OutOfBytes(Room {
                        requested: r.take_u64()?,
                        available: r.take_u64()?,
                    }),
                    2 => Fault::OutOfTime(r.take_u64()?),
                    3 => Fault::Gone,
                    4 => Fault::Conflict(r.take_str()?),
                    _ => return Err(DecodeErr("invalid fault tag")),
                };
                Ok(Self::Failed(f))
            }
            2 => {
                let rf = match r.take_u32()? {
                    0 => Refusal::NoEntry,
                    1 => Refusal::Malformed(Bad {
                        kind: r.take_u32()?,
                        at: r.take_u64()?,
                    }),
                    2 => Refusal::Overloaded,
                    3 => Refusal::Deadline,
                    4 => Refusal::Budget(Room {
                        requested: r.take_u64()?,
                        available: r.take_u64()?,
                    }),
                    5 => Refusal::NotAllowed,
                    6 => Refusal::RequestMismatch,
                    7 => Refusal::HistoryUnavailable,
                    8 => Refusal::StreamFuture(r.take_u64()?),
                    _ => return Err(DecodeErr("invalid refusal tag")),
                };
                Ok(Self::Refused(rf))
            }
            3 => Ok(Self::Abandoned),
            4 => Ok(Self::InDoubt),
            _ => Err(DecodeErr("invalid outcome tag")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub slot: u16,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Group {
    pub sub: u32,
    pub value: Vec<u8>,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyedDone {
    pub stream: StreamId,
    pub p: u64,
    pub outcome: Outcome,
    /// Exact wire bytes of `outcome` for zero-drift passthrough to host callbacks.
    pub raw_outcome: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Part {
    pub sub: u32,
    pub complete: bool,
    pub you: Option<u32>,
    pub incarnation: (u64, u64),
    pub revision: u64,
    pub snapshot: bool,
    pub joined: Vec<(u32, Caller)>,
    pub states: Vec<(u32, Option<Vec<u8>>)>,
    pub msgs: Vec<(u32, bool, Vec<u8>)>,
    pub left: Vec<u32>,
    pub gap: u32,
    pub lost: u32,
    pub full: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Frame {
    Hello(Option<Vec<u8>>, Vec<Want>),
    Call(StreamId, u64, u32, u32, Vec<u8>),
    Subscribe(u32, u32, Vec<u8>, Option<Vec<u8>>),
    Unsubscribe(u32),
    Welcome {
        caller: Caller,
        q: u32,
        budget: u64,
        digest: Vec<u8>,
        target: String,
        n_place: u64,
        now_ms: u64,
        retention_ms: u64,
    },
    Placed(Entry),
    Batch {
        version: u64,
        complete: Vec<u32>,
        groups: Vec<Group>,
        outcomes: Vec<KeyedDone>,
    },
    Ended(u32, Outcome, Vec<u8>),
    Reset(u64, Vec<KeyedDone>),
    Set(u32, Option<Vec<u8>>),
    Send(u32, Option<u32>, Vec<u8>),
    Live(Vec<Part>),
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Self::Hello(cred, wants) => {
                w.put_u32(0);
                w.put_option(cred.as_ref(), |w, b| w.put_bytes(b));
                w.put_list(wants, |w, want| {
                    w.put_bytes(&want.identity);
                    w.put_u64(want.args_fp);
                    w.put_u64(want.result_fp);
                });
            }
            Self::Call(stream, p, want, deadline_ms, args) => {
                w.put_u32(1);
                w.put_u64(stream.t);
                w.put_u64(stream.n);
                w.put_u64(*p);
                w.put_u32(*want);
                w.put_u32(*deadline_ms);
                w.put_bytes(args);
            }
            Self::Subscribe(sub, want, args, resume) => {
                w.put_u32(2);
                w.put_u32(*sub);
                w.put_u32(*want);
                w.put_bytes(args);
                w.put_option(resume.as_ref(), |w, b| w.put_bytes(b));
            }
            Self::Unsubscribe(sub) => {
                w.put_u32(3);
                w.put_u32(*sub);
            }
            Self::Welcome {
                caller,
                q,
                budget,
                digest,
                target,
                n_place,
                now_ms,
                retention_ms,
            } => {
                w.put_u32(4);
                caller.encode(&mut w);
                w.put_u32(*q);
                w.put_u64(*budget);
                w.put_bytes(digest);
                w.put_str(target);
                w.put_u64(*n_place);
                w.put_u64(*now_ms);
                w.put_u64(*retention_ms);
            }
            Self::Placed(entry) => {
                w.put_u32(5);
                match entry {
                    Entry::Ready(i) => {
                        w.put_u32(0);
                        w.put_u32(*i);
                    }
                    Entry::Resolve(i, b) => {
                        w.put_u32(1);
                        w.put_u32(*i);
                        w.put_bytes(b);
                    }
                    Entry::Gone => w.put_u32(2),
                }
            }
            Self::Batch {
                version,
                complete,
                groups,
                outcomes,
            } => {
                w.put_u32(6);
                w.put_u64(*version);
                w.put_list(complete, |w, &s| w.put_u32(s));
                w.put_list(groups, |w, g| {
                    w.put_u32(g.sub);
                    w.put_bytes(&g.value);
                    w.put_list(&g.rows, |w, row| {
                        w.put_u16(row.slot);
                        w.put_bytes(&row.key);
                        w.put_option(row.value.as_ref(), |w, v| w.put_bytes(v));
                    });
                });
                w.put_list(outcomes, |w, d| {
                    w.put_u64(d.stream.t);
                    w.put_u64(d.stream.n);
                    w.put_u64(d.p);
                    d.outcome.encode(w);
                });
            }
            Self::Ended(sub, outcome, _) => {
                w.put_u32(7);
                w.put_u32(*sub);
                outcome.encode(&mut w);
            }
            Self::Reset(version, outcomes) => {
                w.put_u32(8);
                w.put_u64(*version);
                w.put_list(outcomes, |w, d| {
                    w.put_u64(d.stream.t);
                    w.put_u64(d.stream.n);
                    w.put_u64(d.p);
                    d.outcome.encode(w);
                });
            }
            Self::Set(sub, state) => {
                w.put_u32(9);
                w.put_u32(*sub);
                w.put_option(state.as_ref(), |w, b| w.put_bytes(b));
            }
            Self::Send(sub, to, msg) => {
                w.put_u32(10);
                w.put_u32(*sub);
                w.put_option(to.as_ref(), |w, &p| w.put_u32(p));
                w.put_bytes(msg);
            }
            Self::Live(parts) => {
                w.put_u32(11);
                w.put_list(parts, |w, p| {
                    w.put_u32(p.sub);
                    w.put_bool(p.complete);
                    w.put_option(p.you.as_ref(), |w, &id| w.put_u32(id));
                    w.put_u64(p.incarnation.0);
                    w.put_u64(p.incarnation.1);
                    w.put_u64(p.revision);
                    w.put_bool(p.snapshot);
                    w.put_list(&p.joined, |w, (id, caller)| {
                        w.put_u32(*id);
                        caller.encode(w);
                    });
                    w.put_list(&p.states, |w, (id, st)| {
                        w.put_u32(*id);
                        w.put_option(st.as_ref(), |w, b| w.put_bytes(b));
                    });
                    w.put_list(&p.msgs, |w, (id, direct, msg)| {
                        w.put_u32(*id);
                        w.put_bool(*direct);
                        w.put_bytes(msg);
                    });
                    w.put_list(&p.left, |w, &id| w.put_u32(id));
                    w.put_u32(p.gap);
                    w.put_u32(p.lost);
                    w.put_bool(p.full);
                });
            }
        }
        w.out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeErr> {
        let mut r = Reader::new(bytes);
        let frame = match r.take_u32()? {
            0 => {
                let cred = r.take_option(|r| r.take_bytes())?;
                let wants = r.take_list(|r| {
                    Ok(Want {
                        identity: r.take_bytes()?,
                        args_fp: r.take_u64()?,
                        result_fp: r.take_u64()?,
                    })
                })?;
                Self::Hello(cred, wants)
            }
            1 => {
                let t = r.take_u64()?;
                let n = r.take_u64()?;
                let p = r.take_u64()?;
                let want = r.take_u32()?;
                let deadline_ms = r.take_u32()?;
                let args = r.take_bytes()?;
                Self::Call(StreamId { t, n }, p, want, deadline_ms, args)
            }
            2 => {
                let sub = r.take_u32()?;
                let want = r.take_u32()?;
                let args = r.take_bytes()?;
                let resume = r.take_option(|r| r.take_bytes())?;
                Self::Subscribe(sub, want, args, resume)
            }
            3 => Self::Unsubscribe(r.take_u32()?),
            4 => Self::Welcome {
                caller: Caller::decode(&mut r)?,
                q: r.take_u32()?,
                budget: r.take_u64()?,
                digest: r.take_bytes()?,
                target: r.take_str()?,
                n_place: r.take_u64()?,
                now_ms: r.take_u64()?,
                retention_ms: r.take_u64()?,
            },
            5 => {
                let entry = match r.take_u32()? {
                    0 => Entry::Ready(r.take_u32()?),
                    1 => Entry::Resolve(r.take_u32()?, r.take_bytes()?),
                    2 => Entry::Gone,
                    _ => return Err(DecodeErr("invalid placed entry tag")),
                };
                Self::Placed(entry)
            }
            6 => {
                let version = r.take_u64()?;
                let complete = r.take_list(|r| r.take_u32())?;
                let groups = r.take_list(|r| {
                    let sub = r.take_u32()?;
                    let value = r.take_bytes()?;
                    let rows = r.take_list(|r| {
                        Ok(Row {
                            slot: r.take_u16()?,
                            key: r.take_bytes()?,
                            value: r.take_option(|r| r.take_bytes())?,
                        })
                    })?;
                    Ok(Group { sub, value, rows })
                })?;
                let outcomes = r.take_list(decode_keyed_done)?;
                Self::Batch {
                    version,
                    complete,
                    groups,
                    outcomes,
                }
            }
            7 => {
                let sub = r.take_u32()?;
                let start = r.pos();
                let outcome = Outcome::decode(&mut r)?;
                let raw = bytes[start..r.pos()].to_vec();
                Self::Ended(sub, outcome, raw)
            }
            8 => {
                let version = r.take_u64()?;
                let outcomes = r.take_list(decode_keyed_done)?;
                Self::Reset(version, outcomes)
            }
            9 => {
                let sub = r.take_u32()?;
                let state = r.take_option(|r| r.take_bytes())?;
                Self::Set(sub, state)
            }
            10 => {
                let sub = r.take_u32()?;
                let to = r.take_option(|r| r.take_u32())?;
                let msg = r.take_bytes()?;
                Self::Send(sub, to, msg)
            }
            11 => {
                let parts = r.take_list(|r| {
                    Ok(Part {
                        sub: r.take_u32()?,
                        complete: r.take_bool()?,
                        you: r.take_option(|r| r.take_u32())?,
                        incarnation: (r.take_u64()?, r.take_u64()?),
                        revision: r.take_u64()?,
                        snapshot: r.take_bool()?,
                        joined: r.take_list(|r| Ok((r.take_u32()?, Caller::decode(r)?)))?,
                        states: r.take_list(|r| {
                            Ok((r.take_u32()?, r.take_option(|r| r.take_bytes())?))
                        })?,
                        msgs: r
                            .take_list(|r| Ok((r.take_u32()?, r.take_bool()?, r.take_bytes()?)))?,
                        left: r.take_list(|r| r.take_u32())?,
                        gap: r.take_u32()?,
                        lost: r.take_u32()?,
                        full: r.take_bool()?,
                    })
                })?;
                Self::Live(parts)
            }
            _ => return Err(DecodeErr("invalid frame tag")),
        };
        r.done()?;
        Ok(frame)
    }
}

fn decode_keyed_done(r: &mut Reader<'_>) -> Result<KeyedDone, DecodeErr> {
    let t = r.take_u64()?;
    let n = r.take_u64()?;
    let p = r.take_u64()?;
    let start = r.pos();
    let outcome = Outcome::decode(r)?;
    let raw_outcome = r.src[start..r.pos()].to_vec();
    Ok(KeyedDone {
        stream: StreamId { t, n },
        p,
        outcome,
        raw_outcome,
    })
}

pub use super::codec_embed::*;
