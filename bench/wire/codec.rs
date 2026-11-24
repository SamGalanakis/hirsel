//! WIRE-SPEC §3 and core/std/api/codec.md. Values are LE; logical keys are BE.
use crate::{MAX_FRAME, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub identity: Vec<u8>,
    pub args_fp: u64,
    pub result_fp: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Ready(u32),
    Resolve(u32, Vec<u8>),
    Gone,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub requested: u64,
    pub available: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bad {
    pub kind: u32,
    pub at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    NoEntry,
    Malformed(Bad),
    Overloaded,
    Deadline,
    Budget(Room),
    NotAllowed,
    RequestMismatch,
    HistoryUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    Raised(u32),
    OutOfBytes(Room),
    OutOfTime(u64),
    Gone,
    Conflict(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Returned(Vec<u8>),
    Failed(Fault),
    Refused(Refusal),
    Abandoned,
    InDoubt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub stream: u32,
    pub p: u64,
    pub outcome: Outcome,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub slot: u16,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub sub: u32,
    pub value: Vec<u8>,
    pub rows: Vec<Row>,
}

impl Group {
    pub fn board(sub: u32, rows: Vec<Row>) -> Self {
        Self {
            sub,
            value: Vec::new(),
            rows,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub issuer: u8,
    pub subject: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Hello(Option<Vec<u8>>, Vec<Want>),
    Call(u32, u64, u32, u32, Vec<u8>),
    Subscribe(u32, u32, Vec<u8>, Option<Vec<u8>>),
    Unsubscribe(u32),
    Welcome(Caller, u32, u64, Vec<u8>, String, u64),
    Placed(Entry),
    Batch(u64, Vec<u32>, Vec<Group>, Vec<Done>),
    Ended(u32, Outcome),
    Reset(u64, Vec<Done>),
}

trait Wire: Sized {
    fn put(&self, b: &mut Vec<u8>);
    fn get(r: &mut Reader<'_>) -> Result<Self>;
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.bytes.len() - self.at {
            return Err("truncated codec value".into());
        }
        let bytes = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(bytes)
    }
}
macro_rules! integer {
    ($t:ty, $n:literal) => {
        impl Wire for $t {
            fn put(&self, b: &mut Vec<u8>) {
                b.extend(self.to_le_bytes());
            }
            fn get(r: &mut Reader<'_>) -> Result<Self> {
                Ok(Self::from_le_bytes(r.take($n)?.try_into()?))
            }
        }
    };
}
integer!(u8, 1);
integer!(u16, 2);
integer!(u32, 4);
integer!(u64, 8);
impl<T: Wire> Wire for Vec<T> {
    fn put(&self, b: &mut Vec<u8>) {
        u32::try_from(self.len())
            .expect("wire count fits u32")
            .put(b);
        for x in self {
            x.put(b);
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self> {
        let n = u32::get(r)? as usize;
        // Every Wire element used here occupies at least one byte. Check before allocating.
        if n > r.bytes.len() - r.at {
            return Err("invalid codec count".into());
        }
        (0..n).map(|_| T::get(r)).collect()
    }
}
impl<T: Wire> Wire for Option<T> {
    fn put(&self, b: &mut Vec<u8>) {
        match self {
            None => 0u32.put(b),
            Some(x) => {
                1u32.put(b);
                x.put(b);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self> {
        match u32::get(r)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(r)?)),
            _ => Err("unknown Option tag".into()),
        }
    }
}
impl Wire for String {
    fn put(&self, b: &mut Vec<u8>) {
        u32::try_from(self.len())
            .expect("wire string fits u32")
            .put(b);
        b.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self> {
        Ok(String::from_utf8(Vec::<u8>::get(r)?)?)
    }
}
macro_rules! record {
    ($t:ident { $($field:ident),* }) => { impl Wire for $t {
        fn put(&self, b: &mut Vec<u8>) { $(self.$field.put(b);)* }
        fn get(r: &mut Reader<'_>) -> Result<Self> { Ok(Self { $($field: Wire::get(r)?),* }) }
    }};
}
record!(Want {
    identity,
    args_fp,
    result_fp
});
record!(Room {
    requested,
    available
});
record!(Caller { issuer, subject });
record!(Bad { kind, at });
record!(Row { slot, key, value });
record!(Group { sub, value, rows });
record!(Done { stream, p, outcome });
macro_rules! sum {
    ($t:ident { $($tag:literal => $variant:ident $(($($field:ident),+))?),* $(,)? }) => { impl Wire for $t {
        fn put(&self, b: &mut Vec<u8>) { match self { $(Self::$variant $(($($field),+))? => { ($tag as u32).put(b); $($($field.put(b);)+)? }),* } }
        fn get(r: &mut Reader<'_>) -> Result<Self> { match u32::get(r)? { $($tag => Ok(Self::$variant $(( $( { let $field = Wire::get(r)?; $field } ),+ ))?)),*, _ => Err(concat!("unknown ", stringify!($t), " tag").into()) } }
    }};
}
sum!(Entry { 0 => Ready(slot), 1 => Resolve(slot, args), 2 => Gone });
sum!(Refusal { 0 => NoEntry, 1 => Malformed(bad), 2 => Overloaded, 3 => Deadline, 4 => Budget(room), 5 => NotAllowed, 6 => RequestMismatch, 7 => HistoryUnavailable });
sum!(Fault { 0 => Raised(kind), 1 => OutOfBytes(room), 2 => OutOfTime(ticks), 3 => Gone, 4 => Conflict(index) });
sum!(Outcome { 0 => Returned(bytes), 1 => Failed(fault), 2 => Refused(refusal), 3 => Abandoned, 4 => InDoubt });
sum!(Frame {
    0 => Hello(credential, wants), 1 => Call(stream, p, slot, deadline, arg),
    2 => Subscribe(sub, slot, arg, resume), 3 => Unsubscribe(sub),
    4 => Welcome(caller, q, budget, executable, target, n_place), 5 => Placed(entry),
    6 => Batch(version, complete, groups, done), 7 => Ended(sub, outcome), 8 => Reset(version, done),
});

pub fn encode(frame: &Frame) -> Vec<u8> {
    let mut b = Vec::new();
    frame.put(&mut b);
    b
}
pub fn decode(bytes: &[u8]) -> Result<Frame> {
    if bytes.len() > MAX_FRAME {
        return Err("frame exceeds max_frame".into());
    }
    let mut r = Reader { bytes, at: 0 };
    let f = Frame::get(&mut r)?;
    if r.at != bytes.len() {
        return Err("trailing codec bytes".into());
    }
    Ok(f)
}

/// A Hello file lets W4/W6 supply the real generated identities and fingerprints.
pub fn wants_file(path: &std::path::Path) -> Result<Vec<Want>> {
    match decode(&std::fs::read(path)?)? {
        Frame::Hello(None, wants) if wants.len() == 2 => Ok(wants),
        _ => Err("wants file must be Hello(None, [toggle, board])".into()),
    }
}
pub fn checkbox_wants() -> Vec<Want> {
    fn fp(op: u8) -> u64 {
        [3, 0, 0, 0, 0, op]
            .iter()
            .fold(0xc15d213aa4d7a795, |mut f, b| {
                f ^= u64::from(*b);
                for _ in 0..8 {
                    f = (f >> 1) ^ (0xc15d213aa4d7a795 & 0u64.wrapping_sub(f & 1));
                }
                f
            })
    }
    vec![
        Want {
            identity: b"\x01checkbox@1/main/toggle".to_vec(),
            args_fp: fp(5),
            result_fp: fp(1),
        },
        // Cover is erased into rows, and its nominal schema is supplied by --wants for Hirsel.
        Want {
            identity: b"\x01checkbox@1/main/board".to_vec(),
            args_fp: fp(0),
            result_fp: 0,
        },
    ]
}

pub fn chunk_row(chunk: u32, bitmap: &[u8]) -> Row {
    let mut key = 0u32.to_be_bytes().to_vec();
    key.extend(chunk.to_be_bytes());
    let mut value = 64u32.to_le_bytes().to_vec();
    let at = chunk as usize * 64;
    let end = (at + 64).min(bitmap.len());
    value.extend(&bitmap[at..end]);
    value.resize(68, 0);
    Row {
        slot: 0,
        key,
        value: Some(value),
    }
}

/// The table id is assigned by the place; clients only need its chunk key suffix.
pub fn apply_row(image: &mut [u8], row: &Row) -> Result<u32> {
    if row.slot != 0 || row.key.len() != 8 {
        return Err("checkbox row key must be table-id + u32-BE".into());
    }
    let chunk = u32::from_be_bytes(row.key[4..].try_into()?);
    if chunk >= crate::CHUNKS {
        return Err("checkbox chunk out of range".into());
    }
    let at = chunk as usize * 64;
    let end = (at + 64).min(image.len());
    match &row.value {
        None => image[at..end].fill(0),
        Some(v) => {
            if v.len() != 68
                || v[..4] != 64u32.to_le_bytes()
                || v[4 + end - at..].iter().any(|b| *b != 0)
            {
                return Err("invalid checkbox chunk value or tail".into());
            }
            image[at..end].copy_from_slice(&v[4..4 + end - at]);
        }
    }
    Ok(chunk)
}
