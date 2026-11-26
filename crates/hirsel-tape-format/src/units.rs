//! The format-14 bodies carried by helper headers, TAKE and helper END.
use super::{Corrupt, Cursor, PAYLOAD_CHUNK, put_payload, put_varint};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TakePoint {
    Operation,
    Pump,
}

impl TakePoint {
    pub(super) fn read(c: &mut Cursor<'_>) -> Result<Self, Corrupt> {
        match c.u8()? {
            0 => Ok(Self::Operation),
            1 => Ok(Self::Pump),
            _ => Err(Corrupt),
        }
    }

    pub(super) fn tag(self) -> u8 {
        match self {
            Self::Operation => 0,
            Self::Pump => 1,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum Arrival<'a> {
    Item(u64, Value<'a>),
    Close(u64),
    SealAnswer(u64),
    Settlement(u64, Settlement<'a>),
    HelperEnd(u64, HelperOutcome<'a>, Vec<Survivor>),
    /// Making stream, connection ordinal there, retained header, unused room.
    LastRelease(u32, u64, u64, u64),
    /// Home stream and node ordinal there.
    SharedRelease(u32, u64),
    Cancel,
    Refund(Credit),
}

#[derive(Debug, Eq, PartialEq)]
pub enum Settlement<'a> {
    Accepted,
    Closed(Value<'a>),
    Withdrawn,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Credit {
    pub limit: u64,
    pub amount: u64,
}

/// Every survivor kind the format admits, in tag order.
pub(crate) const SURVIVOR_KINDS: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7];

#[derive(Debug, Eq, PartialEq)]
pub struct Survivor {
    pub kind: u8,
    pub origin_kind: Option<u8>,
    /// The stream that made it or is its home; its ordinal is that stream's.
    pub stream: u32,
    pub ordinal: u64,
    pub amount: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Cause<'a> {
    pub kind: u8,
    pub site: u64,
    pub detail: CauseDetail<'a>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum CauseDetail<'a> {
    None,
    Fail(&'a [u8]),
    Capacity { requested: u64, available: u64 },
}

#[derive(Debug, Eq, PartialEq)]
pub enum HelperOutcome<'a, ResultValue = Value<'a>> {
    Returned(ResultValue),
    Failed(Vec<Cause<'a>>),
    Cancelled(Vec<Cause<'a>>),
    Deadlock(u64),
}

/// A value's checked wire bytes. Part order and the layout of plain bytes
/// are interpreted only once the receiver knows their type.
#[derive(Debug, Eq, PartialEq)]
pub struct Value<'a>(&'a [u8]);

impl<'a> Value<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Corrupt> {
        decode(bytes, read_value)
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.0
    }

    pub fn canonical(&self) -> Vec<u8> {
        let mut c = Cursor::new(self.0);
        let mut canonical = Vec::new();
        let mut start = 0;
        scan_value(&mut c, true, |ordinal_start, ordinal_end| {
            canonical.extend_from_slice(&self.0[start..ordinal_start]);
            start = ordinal_end;
        })
        .ok()
        .expect("a checked value");
        canonical.extend_from_slice(&self.0[start..]);
        canonical
    }

    /// This value with its snapshot ordinals, in the order they are written,
    /// replaced by `ordinals`: the receiving unit's form of a value whose
    /// sender wrote ordinal 0 in each.
    pub fn with_ordinals(&self, ordinals: &[u64]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len());
        let mut start = 0;
        let mut next = ordinals.iter();
        scan_value(
            &mut Cursor::new(self.0),
            true,
            |ordinal_start, ordinal_end| {
                out.extend_from_slice(&self.0[start..ordinal_start]);
                put_varint(&mut out, *next.next().expect("an ordinal per snapshot"));
                start = ordinal_end;
            },
        )
        .ok()
        .expect("a checked value");
        out.extend_from_slice(&self.0[start..]);
        out
    }
}

/// A crossed value without receiver-assigned snapshot ordinals.
#[derive(Debug, Eq, PartialEq)]
pub struct CanonicalValue<'a>(&'a [u8]);

impl<'a> CanonicalValue<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.0
    }
}

fn read_canonical_value<'a>(c: &mut Cursor<'a>) -> Result<CanonicalValue<'a>, Corrupt> {
    let start = c.at;
    scan_value(c, false, |_, _| {})?;
    Ok(CanonicalValue(&c.bytes[start..c.at]))
}

fn read_value<'a>(c: &mut Cursor<'a>) -> Result<Value<'a>, Corrupt> {
    let start = c.at;
    scan_value(c, true, |_, _| {})?;
    Ok(Value(&c.bytes[start..c.at]))
}

fn scan_value(
    c: &mut Cursor<'_>,
    with_ordinals: bool,
    mut skip_ordinal: impl FnMut(usize, usize),
) -> Result<(), Corrupt> {
    c.bytes()?;
    let mut left = c.varint()?;
    let mut parents = Vec::new();
    loop {
        if left == 0 {
            let Some(parent) = parents.pop() else {
                return Ok(());
            };
            left = parent;
            continue;
        }
        left -= 1;
        match c.u8()? {
            0 => {
                c.bytes()?;
            }
            2 => {
                if c.bytes()?.first() != Some(&1) {
                    return Err(Corrupt);
                }
            }
            1 => {
                let kind = c.u8()?;
                if with_ordinals {
                    let ordinal = c.at;
                    c.varint()?;
                    skip_ordinal(ordinal, c.at);
                }
                if snapshot(c, kind)? {
                    c.bytes()?;
                    if left != 0 {
                        parents.push(left);
                    }
                    left = c.varint()?;
                }
            }
            _ => return Err(Corrupt),
        }
    }
}

fn tag(c: &mut Cursor<'_>, max: u8) -> Result<u8, Corrupt> {
    let tag = c.u8()?;
    (tag <= max).then_some(tag).ok_or(Corrupt)
}

fn address(c: &mut Cursor<'_>) -> Result<(), Corrupt> {
    super::payload::address(c.take(19)?)
}

fn origin(c: &mut Cursor<'_>) -> Result<(), Corrupt> {
    c.u32()?;
    c.varint()?;
    Ok(())
}

fn media_origin(c: &mut Cursor<'_>) -> Result<(), Corrupt> {
    c.u32()?;
    if c.varint()? == 0 {
        c.varint()?;
    }
    Ok(())
}

fn profile(c: &mut Cursor<'_>) -> Result<u8, Corrupt> {
    let family = tag(c, 7)?;
    tag(c, if family >= 6 { 0 } else { 2 })?;
    Ok(family)
}

fn grant(c: &mut Cursor<'_>) -> Result<u8, Corrupt> {
    let kind = c.u8()?;
    matches!(kind, 0..=9 | 13 | 14)
        .then_some(kind)
        .ok_or(Corrupt)
}

/// Returns true when a Shared snapshot continues with a value.
fn snapshot(c: &mut Cursor<'_>, kind: u8) -> Result<bool, Corrupt> {
    match kind {
        0 => {
            origin(c)?;
            c.take(16)?;
        }
        1 => {
            origin(c)?;
            return Ok(true);
        }
        2 => {
            tag(c, 1)?;
            address(c)?;
            if tag(c, 1)? == 1 {
                super::tls_connect_payload(c.bytes()?)?;
            }
            tag(c, 3)?;
        }
        3 | 4 => {
            tag(c, 0)?;
            address(c)?;
            if kind == 4 && tag(c, 1)? == 1 {
                address(c)?;
            }
        }
        5 => {
            let kind = grant(c)?;
            super::blocks::capability_snapshot(kind, c.bytes()?)?;
        }
        6 => {
            grant(c)?;
            c.u32()?;
        }
        7 => {
            tag(c, 6)?;
            origin(c)?;
            let text = std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt)?;
            let policy = hirsel_core_net::SecretPolicy::parse(text).ok_or(Corrupt)?;
            if policy.text() != text {
                return Err(Corrupt);
            }
        }
        8 => {
            media_origin(c)?;
            tag(c, 1)?;
        }
        9 => {
            media_origin(c)?;
            let family = profile(c)?;
            let side = tag(c, 3)?;
            tag(c, 1)?;
            c.take(16)?;
            tag(c, 31)?;
            tag(c, 1)?;
            for _ in 0..c.varint()? {
                tag(c, 4)?;
                c.u64()?;
            }
            if family == 7 && side == 2 {
                let kid = c.u64()?;
                let bits = tag(c, hirsel_core_net::SframeDirection::MAX_BITS)?;
                if kid & ((1 << bits) - 1) != 0 {
                    return Err(Corrupt);
                }
            }
        }
        10..=12 => {
            media_origin(c)?;
            profile(c)?;
            tag(c, 4)?;
            match kind {
                10 => {
                    c.take(16)?;
                    if tag(c, 1)? == 1 {
                        c.u64()?;
                    }
                }
                12 => {
                    tag(c, 1)?;
                }
                _ => {}
            }
        }
        _ => return Err(Corrupt),
    }
    Ok(false)
}

fn credit(c: &mut Cursor<'_>) -> Result<Credit, Corrupt> {
    Ok(Credit {
        limit: c.varint()?,
        amount: c.u64()?,
    })
}

fn causes<'a>(c: &mut Cursor<'a>) -> Result<Vec<Cause<'a>>, Corrupt> {
    let count = c.varint()?;
    if count == 0 {
        return Err(Corrupt);
    }
    (0..count)
        .map(|_| {
            let kind = tag(c, 7)?;
            let site = c.u64()?;
            let detail = match kind {
                4 => CauseDetail::Fail(c.bytes()?),
                6 => CauseDetail::Capacity {
                    requested: c.u64()?,
                    available: c.u64()?,
                },
                _ => CauseDetail::None,
            };
            Ok(Cause { kind, site, detail })
        })
        .collect()
}

fn outcome<'a, V>(
    c: &mut Cursor<'a>,
    kind: u8,
    value: impl FnOnce(&mut Cursor<'a>) -> Result<V, Corrupt>,
) -> Result<HelperOutcome<'a, V>, Corrupt> {
    Ok(match kind {
        0 => HelperOutcome::Returned(value(c)?),
        1 => HelperOutcome::Failed(causes(c)?),
        2 => HelperOutcome::Cancelled(causes(c)?),
        3 => HelperOutcome::Deadlock(c.varint()?),
        _ => return Err(Corrupt),
    })
}

fn read_arrival<'a>(c: &mut Cursor<'a>) -> Result<Arrival<'a>, Corrupt> {
    Ok(match c.u8()? {
        0 => Arrival::Item(c.varint()?, read_value(c)?),
        1 => Arrival::Close(c.varint()?),
        2 => Arrival::SealAnswer(c.varint()?),
        3 => Arrival::Settlement(
            c.varint()?,
            match c.u8()? {
                0 => Settlement::Accepted,
                1 => Settlement::Closed(read_value(c)?),
                2 => Settlement::Withdrawn,
                _ => return Err(Corrupt),
            },
        ),
        4 => {
            let ordinal = c.varint()?;
            let kind = tag(c, 2)?;
            let outcome = outcome(c, kind, read_value)?;
            let survivors = (0..c.varint()?)
                .map(|_| {
                    let kind = c.u8()?;
                    if !SURVIVOR_KINDS.contains(&kind) {
                        return Err(Corrupt);
                    }
                    let origin_kind = matches!(kind, 2 | 7).then(|| tag(c, 6)).transpose()?;
                    Ok(Survivor {
                        kind,
                        origin_kind,
                        stream: c.u32()?,
                        ordinal: c.varint()?,
                        amount: c.u64()?,
                    })
                })
                .collect::<Result<_, Corrupt>>()?;
            Arrival::HelperEnd(ordinal, outcome, survivors)
        }
        6 => Arrival::LastRelease(c.u32()?, c.varint()?, c.u64()?, c.u64()?),
        7 => Arrival::SharedRelease(c.u32()?, c.varint()?),
        8 => Arrival::Cancel,
        9 => Arrival::Refund(credit(c)?),
        _ => return Err(Corrupt),
    })
}

fn decode<'a, T>(
    bytes: &'a [u8],
    read: impl FnOnce(&mut Cursor<'a>) -> Result<T, Corrupt>,
) -> Result<T, Corrupt> {
    let mut c = Cursor::new(bytes);
    let value = read(&mut c)?;
    if !c.done() {
        return Err(Corrupt);
    }
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TakeBody(Vec<u8>);

impl TakeBody {
    pub fn arrival(&self) -> Arrival<'_> {
        decode(&self.0, read_arrival).ok().expect("a checked TAKE")
    }

    pub fn decode(bytes: Vec<u8>) -> Result<Self, Corrupt> {
        decode(&bytes, read_arrival)?;
        Ok(Self(bytes))
    }

    pub(super) fn settlement(&self) -> Option<u64> {
        let mut c = Cursor::new(&self.0);
        (c.u8().ok()? == 3).then(|| c.varint().expect("a checked settlement"))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelperEnding {
    kind: u8,
    body: Vec<u8>,
}

impl HelperEnding {
    pub fn decode(kind: u8, body: Vec<u8>) -> Result<Self, Corrupt> {
        decode(&body, |c| outcome(c, kind, read_canonical_value))?;
        Ok(Self { kind, body })
    }

    pub fn kind(&self) -> u8 {
        self.kind
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn outcome(&self) -> HelperOutcome<'_, CanonicalValue<'_>> {
        decode(&self.body, |c| outcome(c, self.kind, read_canonical_value))
            .ok()
            .expect("a checked helper END")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub parent: u32,
    pub spawn: u64,
    body: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct EntryBody<'a> {
    pub descriptor: &'a [u8],
    pub n: u64,
    pub bound: Option<u64>,
    pub value: Value<'a>,
}

fn read_entry<'a>(c: &mut Cursor<'a>) -> Result<EntryBody<'a>, Corrupt> {
    let descriptor = c.bytes()?;
    if descriptor.first() != Some(&1) {
        return Err(Corrupt);
    }
    let n = c.u64()?;
    let bound = if tag(c, 1)? == 1 {
        Some(c.u64()?)
    } else {
        None
    };
    Ok(EntryBody {
        descriptor,
        n,
        bound,
        value: read_value(c)?,
    })
}

impl Entry {
    pub fn decode(parent: u32, spawn: u64, body: Vec<u8>) -> Result<Self, Corrupt> {
        decode(&body, read_entry)?;
        Ok(Self {
            parent,
            spawn,
            body,
        })
    }

    pub fn body(&self) -> EntryBody<'_> {
        decode(&self.body, read_entry)
            .ok()
            .expect("a checked helper entry")
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Vec<&[u8]> {
        out.extend_from_slice(&self.parent.to_le_bytes());
        out.extend_from_slice(&self.spawn.to_le_bytes());
        let mut rest = Vec::new();
        put_carrier(out, &self.body, &mut rest);
        rest
    }
}

pub(super) fn put_carrier<'a>(out: &mut Vec<u8>, body: &'a [u8], rest: &mut Vec<&'a [u8]>) {
    put_varint(out, body.len() as u64);
    put_payload(out, body, rest);
}

pub(super) fn carrier<'a>(c: &mut Cursor<'a>) -> Result<(usize, &'a [u8]), Corrupt> {
    let total = usize::try_from(c.varint()?).map_err(|_| Corrupt)?;
    let first = c.bytes()?;
    if first.is_empty() || first.len() > total.min(PAYLOAD_CHUNK) {
        return Err(Corrupt);
    }
    Ok((total, first))
}
