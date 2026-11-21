//! The tape format, byte by byte (REPLAY-SPEC section 5): little-endian
//! fixed-width integers, canonical LEB128 varints, checked blocks, the header
//! (block 0's payload) and the frames of every later block.
//!
//! A tape file is outside input: every byte the reader hands on has passed its
//! block's xxh3-64 check, which covers the tape id, the block's flags, magic,
//! sequence number and length, so a block from another tape or another place
//! fails. Discovery is sequential from offset 0 and never resynchronises.
use hirsel_core_net::{EnvPatterns, EnvSnapshot, GrantRule, ResolveRule, Rule, TlsRule, TlsTrust};
use std::hash::Hasher;
use twox_hash::XxHash3_64;

pub const FORMAT: u16 = 8;
pub const MAGIC: [u8; 3] = *b"HTB";
/// The tape's last block.
pub const END: u8 = 2;
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

/// Protected entry input. Tape diagnostics must not print it.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct MapHashKey(pub [u8; 16]);

impl std::fmt::Debug for MapHashKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Block 0's payload (section 5.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub kind: u8,
    pub tape_id: [u8; 16],
    pub identity: Vec<u8>,
    /// The code's revision: the entry's code revision for a Run tape, the
    /// called function's for a Call tape. No compiler,
    /// backend or runtime identity is in it.
    pub revision: [u8; 32],
    pub charge_policy: u32,
    pub limit: u64,
    pub bound: u64,
    pub chooser: u8,
    pub arg: Vec<u8>,
    /// Inert snapshot of the Run's configured Net rules. Empty for a Call.
    pub net_rules: Vec<GrantRule>,
    pub secret_grants: Vec<hirsel_core_net::SecretGrant>,
    pub env: Option<EnvSnapshot>,
    pub map_hash_key: MapHashKey,
}
/// The complete encoded header does not fit in block zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderTooLarge;

const NET_KIND: u8 = 1;
/// Stream grants carry a single canonical terminal bit in a GRANT frame.
pub const fn is_stream_grant(kind: u8) -> bool {
    matches!(kind, 4..=6)
}

const ENV_KIND: u8 = 7;
const SECRET_KIND: u8 = 11;
const NET_RULE_LEN: usize = 23;

fn read_net_rule(bytes: &[u8]) -> Rule {
    Rule {
        dir: bytes[0],
        family: bytes[1],
        hi: u64::from_le_bytes(bytes[2..10].try_into().unwrap()),
        lo: u64::from_le_bytes(bytes[10..18].try_into().unwrap()),
        bits: bytes[18],
        first: u16::from_le_bytes(bytes[19..21].try_into().unwrap()),
        last: u16::from_le_bytes(bytes[21..23].try_into().unwrap()),
    }
}

fn read_tls_rule(c: &mut Cursor<'_>) -> Result<TlsRule, Corrupt> {
    let trust = match c.u8()? {
        0 => TlsTrust::System,
        1 => TlsTrust::Roots(c.fixed()?),
        _ => return Err(Corrupt),
    };
    let first = u16::from_le_bytes(c.fixed()?);
    let last = u16::from_le_bytes(c.fixed()?);
    TlsRule::new(&read_patterns(c)?, first, last, trust).ok_or(Corrupt)
}

fn read_patterns(c: &mut Cursor<'_>) -> Result<String, Corrupt> {
    let count = u16::from_le_bytes(c.fixed()?);
    let mut patterns = String::new();
    for i in 0..count {
        let len = usize::from(c.u8()?);
        let pattern = std::str::from_utf8(c.take(len)?).map_err(|_| Corrupt)?;
        if pattern.is_empty() || pattern.contains(',') {
            return Err(Corrupt);
        }
        if i != 0 {
            patterns.push(',');
        }
        patterns.push_str(pattern);
    }
    Ok(patterns)
}

fn read_net_rules(c: &mut Cursor<'_>) -> Result<Vec<GrantRule>, Corrupt> {
    c.u8()?;
    let count = usize::try_from(c.varint()?).map_err(|_| Corrupt)?;
    let mut rules = Vec::new();
    for _ in 0..count {
        match c.bytes.get(c.at) {
            Some(2) => {
                c.u8()?;
                rules.push(GrantRule::Tls(read_tls_rule(c)?));
            }
            Some(3) => {
                c.u8()?;
                let rule = ResolveRule::new(&read_patterns(c)?).ok_or(Corrupt)?;
                rules.push(GrantRule::Resolve(rule));
            }
            _ => {
                let rule = read_net_rule(c.take(NET_RULE_LEN)?);
                if !rule.valid() {
                    return Err(Corrupt);
                }
                rules.push(GrantRule::Address(rule));
            }
        }
    }
    if count == 0 {
        return Err(Corrupt);
    }
    Ok(rules)
}

fn write_net_rules(rules: &[GrantRule]) -> Result<Vec<u8>, HeaderTooLarge> {
    if rules.is_empty() {
        return Ok(vec![]);
    }
    let mut out = vec![NET_KIND];
    put_varint(&mut out, rules.len() as u64);
    for rule in rules {
        match rule {
            GrantRule::Address(rule) => {
                out.extend_from_slice(&[rule.dir, rule.family]);
                out.extend_from_slice(&rule.hi.to_le_bytes());
                out.extend_from_slice(&rule.lo.to_le_bytes());
                out.push(rule.bits);
                out.extend_from_slice(&rule.first.to_le_bytes());
                out.extend_from_slice(&rule.last.to_le_bytes());
            }
            GrantRule::Tls(rule) => {
                out.push(2);
                match rule.trust() {
                    TlsTrust::System => out.push(0),
                    TlsTrust::Roots(digest) => {
                        out.push(1);
                        out.extend_from_slice(&digest);
                    }
                }
                let (first, last) = rule.ports();
                out.extend_from_slice(&first.to_le_bytes());
                out.extend_from_slice(&last.to_le_bytes());
                write_patterns(&mut out, &rule.pattern_text())?;
            }
            GrantRule::Resolve(rule) => {
                out.push(3);
                write_patterns(&mut out, &rule.pattern_text())?;
            }
        }
        if out.len() > MAX_LEN {
            return Err(HeaderTooLarge);
        }
    }
    Ok(out)
}

fn write_patterns(out: &mut Vec<u8>, text: &str) -> Result<(), HeaderTooLarge> {
    let patterns = text.split(',');
    let count = u16::try_from(patterns.clone().count()).map_err(|_| HeaderTooLarge)?;
    out.extend_from_slice(&count.to_le_bytes());
    for pattern in patterns {
        out.push(u8::try_from(pattern.len()).map_err(|_| HeaderTooLarge)?);
        out.extend_from_slice(pattern.as_bytes());
        if out.len() > MAX_LEN {
            return Err(HeaderTooLarge);
        }
    }
    Ok(())
}

fn read_env(c: &mut Cursor<'_>) -> Result<EnvSnapshot, Corrupt> {
    c.u8()?;
    let text = std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt)?;
    let patterns = EnvPatterns::parse(text).ok_or(Corrupt)?;
    let count = c.varint()?;
    let mut entries = Vec::<(String, Vec<u8>)>::new();
    for _ in 0..count {
        let name = std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt)?;
        if !patterns.covers(name) || entries.last().is_some_and(|row| row.0.as_str() >= name) {
            return Err(Corrupt);
        }
        entries.push((name.to_owned(), c.bytes()?.to_vec()));
    }
    Ok(EnvSnapshot { patterns, entries })
}

fn read_secret_grants(c: &mut Cursor<'_>) -> Result<Vec<hirsel_core_net::SecretGrant>, Corrupt> {
    let mut grants = Vec::new();
    while !c.done() {
        if c.u8()? != SECRET_KIND {
            return Err(Corrupt);
        }
        let mut field = || std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt);
        let grant =
            hirsel_core_net::SecretGrant::new(field()?, field()?, field()?).ok_or(Corrupt)?;
        if grants
            .iter()
            .any(|old: &hirsel_core_net::SecretGrant| old.parameter == grant.parameter)
        {
            return Err(Corrupt);
        }
        grants.push(grant);
    }
    Ok(grants)
}

fn varint_len(value: usize) -> usize {
    (usize::BITS - value.leading_zeros()).div_ceil(7).max(1) as usize
}

impl Header {
    pub fn encoded_len(
        identity: usize,
        arg: usize,
        grants: usize,
    ) -> Result<usize, HeaderTooLarge> {
        let mut len = 2 + 1 + 16 + 32 + 4 + 8 + 8 + 1 + 16;
        for field in [identity, arg, grants] {
            if field > MAX_LEN {
                return Err(HeaderTooLarge);
            }
            len += field + varint_len(field);
        }
        if len > MAX_LEN {
            return Err(HeaderTooLarge);
        }
        Ok(len)
    }

    pub fn encode(&self, revision: [u8; 32]) -> Result<Vec<u8>, HeaderTooLarge> {
        let mut grants = write_net_rules(&self.net_rules)?;
        if let Some(env) = &self.env {
            grants.push(ENV_KIND);
            put_bytes(&mut grants, env.patterns.text().as_bytes());
            put_varint(&mut grants, env.entries.len() as u64);
            for (name, value) in &env.entries {
                if grants
                    .len()
                    .saturating_add(name.len())
                    .saturating_add(value.len())
                    > MAX_LEN
                {
                    return Err(HeaderTooLarge);
                }
                put_bytes(&mut grants, name.as_bytes());
                put_bytes(&mut grants, value);
            }
        }
        for grant in &self.secret_grants {
            grants.push(SECRET_KIND);
            for field in [&grant.parameter, &grant.source, &grant.patterns] {
                if grants.len().saturating_add(field.len()) > MAX_LEN {
                    return Err(HeaderTooLarge);
                }
                put_bytes(&mut grants, field.as_bytes());
            }
        }
        let len = Self::encoded_len(self.identity.len(), self.arg.len(), grants.len())?;
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.push(self.kind);
        out.extend_from_slice(&self.tape_id);
        put_bytes(&mut out, &self.identity);
        out.extend_from_slice(&revision);
        out.extend_from_slice(&self.charge_policy.to_le_bytes());
        out.extend_from_slice(&self.limit.to_le_bytes());
        out.extend_from_slice(&self.bound.to_le_bytes());
        out.push(self.chooser);
        put_bytes(&mut out, &self.arg);
        put_bytes(&mut out, &grants);
        out.extend_from_slice(&self.map_hash_key.0);
        Ok(out)
    }
    pub fn decode(payload: &[u8]) -> Result<Self, Corrupt> {
        if payload.len() > MAX_LEN {
            return Err(Corrupt);
        }
        let mut c = Cursor::new(payload);
        if u16::from_le_bytes(c.fixed()?) != FORMAT {
            return Err(Corrupt);
        }
        let kind = c.u8()?;
        if !matches!(kind, KIND_RUN | KIND_CALL) {
            return Err(Corrupt);
        }
        let tape_id = c.fixed()?;
        let identity = c.bytes()?;
        let revision = c.fixed()?;
        let charge_policy = u32::from_le_bytes(c.fixed()?);
        let limit = u64::from_le_bytes(c.fixed()?);
        let bound = u64::from_le_bytes(c.fixed()?);
        let chooser = c.u8()?;
        if !matches!(chooser, CHOOSER_FIFO | CHOOSER_COMMIT) {
            return Err(Corrupt);
        }
        let arg = c.bytes()?;
        let grants = c.bytes()?;
        let map_hash_key = MapHashKey(c.fixed()?);
        if (kind == KIND_CALL && !grants.is_empty()) || !c.done() {
            return Err(Corrupt);
        }
        let mut grants = Cursor::new(grants);
        let net_rules = if grants.bytes.first() == Some(&NET_KIND) {
            read_net_rules(&mut grants)?
        } else {
            vec![]
        };
        let env = if grants.bytes.get(grants.at) == Some(&ENV_KIND) {
            Some(read_env(&mut grants)?)
        } else {
            None
        };
        let secret_grants = read_secret_grants(&mut grants)?;
        if let Some(env) = &env
            && secret_grants.iter().any(|grant| {
                matches!(
                    hirsel_core_net::SecretSource::parse(&grant.source),
                    Some(hirsel_core_net::SecretSource::Env(name)) if env.patterns.covers(name)
                )
            })
        {
            return Err(Corrupt);
        }
        Ok(Self {
            kind,
            tape_id,
            identity: identity.to_vec(),
            revision,
            charge_policy,
            limit,
            bound,
            chooser,
            arg: arg.to_vec(),
            net_rules,
            secret_grants,
            env,
            map_hash_key,
        })
    }
}
/// Where block 0's payload keeps the tape id: after format and kind.
pub const TAPE_ID_AT: usize = 3;

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

#[derive(Debug)]
pub enum ReadError<E> {
    Source(E),
    Format(Stop),
}
impl<E> From<Stop> for ReadError<E> {
    fn from(stop: Stop) -> Self {
        Self::Format(stop)
    }
}

type Block<'a> = (u8, &'a [u8]);

/// Sequential block discovery. The reader reuses storage for one checked
/// block and never reads beyond that block until the next call.
pub struct Blocks {
    bytes: Vec<u8>,
    seq: u64,
    tape_id: [u8; 16],
    ended: bool,
}
impl Blocks {
    /// Read and validate block zero before requesting any later bytes.
    pub fn open<E>(
        mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
    ) -> Result<(Self, Header), ReadError<E>> {
        let bytes = header_block(&mut read, Some(FORMAT))?;
        let payload = header_payload(&bytes);
        let header = Header::decode(payload).map_err(Stop::from)?;
        let tape_id = header.tape_id;
        Ok((
            Self {
                bytes,
                seq: 1,
                tape_id,
                ended: false,
            },
            header,
        ))
    }

    /// Allocation that can be retained for the next block after its payload is consumed.
    pub fn buffer_capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// Release storage between blocks without changing the read position.
    pub fn discard_buffer(&mut self) {
        self.bytes = Vec::new();
    }

    /// Read the next checked block, or confirm EOF after END.
    pub fn next<E>(
        &mut self,
        mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
    ) -> Result<Option<Block<'_>>, ReadError<E>> {
        if self.ended {
            return if read(&mut [0]).map_err(ReadError::Source)? == 0 {
                Ok(None)
            } else {
                Err(Stop::Damaged(self.seq).into())
            };
        }
        read_block(&mut self.bytes, self.seq, &mut read)?;
        check_head(&self.bytes, self.seq)?;
        check_digest(&self.bytes, &self.tape_id, self.seq)?;
        self.seq += 1;
        let flags = self.bytes[0];
        self.ended = flags & END != 0;
        Ok(Some((
            flags,
            &self.bytes[BLOCK_HEAD..self.bytes.len() - BLOCK_CHECK],
        )))
    }
}

fn fill<E>(
    mut bytes: &mut [u8],
    read: &mut impl FnMut(&mut [u8]) -> Result<usize, E>,
) -> Result<usize, ReadError<E>> {
    let len = bytes.len();
    while !bytes.is_empty() {
        let n = read(bytes).map_err(ReadError::Source)?;
        if n == 0 {
            break;
        }
        bytes = &mut bytes[n..];
    }
    Ok(len - bytes.len())
}

fn header_payload(bytes: &[u8]) -> &[u8] {
    &bytes[BLOCK_HEAD..bytes.len() - BLOCK_CHECK]
}

fn header_block<E>(
    read: &mut impl FnMut(&mut [u8]) -> Result<usize, E>,
    format: Option<u16>,
) -> Result<Vec<u8>, ReadError<E>> {
    let mut bytes = Vec::new();
    read_block(&mut bytes, 0, read)?;
    check_head(&bytes, 0)?;
    if bytes[0] != 0 {
        return Err(Stop::Damaged(0).into());
    }
    let payload = header_payload(&bytes);
    if let (Some(format), Some(version)) = (format, payload.get(..2))
        && version != format.to_le_bytes()
    {
        return Err(Stop::Corrupt.into());
    }
    let tape_id = payload
        .get(TAPE_ID_AT..TAPE_ID_AT + 16)
        .ok_or(Stop::Damaged(0))?;
    check_digest(&bytes, tape_id.try_into().unwrap(), 0)?;
    Ok(bytes)
}

/// The fixed prefix of block 0 used to choose a pinned executable. The image
/// validates the full header and frames with its own runtime before replay.
pub fn run_route<E>(
    mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
) -> Result<([u8; 16], Vec<u8>), ReadError<E>> {
    fn route(payload: &[u8]) -> Result<([u8; 16], Vec<u8>), Corrupt> {
        let mut c = Cursor::new(payload);
        c.take(2)?; // Runtime-owned format version.
        if c.u8()? != KIND_RUN {
            return Err(Corrupt);
        }
        let tape_id = c.fixed()?;
        c.bytes()?; // Identity.
        c.take(32 + 4 + 8 + 8 + 1)?; // Revision, charge policy, limits, chooser.
        Ok((tape_id, c.bytes()?.to_vec()))
    }

    let bytes = header_block(&mut read, None)?;
    route(header_payload(&bytes)).map_err(|e| Stop::from(e).into())
}

fn read_block<E>(
    bytes: &mut Vec<u8>,
    seq: u64,
    read: &mut impl FnMut(&mut [u8]) -> Result<usize, E>,
) -> Result<(), ReadError<E>> {
    if bytes.len() < BLOCK_HEAD {
        bytes.resize(BLOCK_HEAD, 0);
    }
    match fill(&mut bytes[..BLOCK_HEAD], read)? {
        0 => return Err(Stop::Missing.into()),
        BLOCK_HEAD => {}
        _ => return Err(Stop::Truncated.into()),
    }
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    if len > MAX_LEN {
        return Err(Stop::Damaged(seq).into());
    }
    let size = BLOCK_HEAD + len + BLOCK_CHECK;
    bytes.reserve_exact(size.saturating_sub(bytes.len()));
    bytes.resize(size, 0);
    if fill(&mut bytes[BLOCK_HEAD..], read)? != len + BLOCK_CHECK {
        return Err(Stop::Truncated.into());
    }
    Ok(())
}

fn check_head(bytes: &[u8], seq: u64) -> Result<(), Stop> {
    if bytes[1..4] != MAGIC
        || bytes[0] & !END != 0
        || u64::from_le_bytes(bytes[4..12].try_into().unwrap()) != seq
    {
        return Err(Stop::Damaged(seq));
    }
    Ok(())
}

fn check_digest(bytes: &[u8], id: &[u8; 16], seq: u64) -> Result<(), Stop> {
    let (body, stored) = bytes.split_at(bytes.len() - BLOCK_CHECK);
    if xxh3(&[id, body]) != u64::from_le_bytes(stored.try_into().unwrap()) {
        return Err(Stop::Damaged(seq));
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
        Self::of_parts(&[bytes])
    }
    pub fn of_parts(parts: &[&[u8]]) -> Self {
        let len = parts
            .iter()
            .fold(0usize, |len, part| len.saturating_add(part.len()));
        if len <= ARGS_LITERAL {
            let mut bytes = Vec::with_capacity(len);
            for part in parts {
                bytes.extend_from_slice(part);
            }
            Args::Literal(bytes)
        } else {
            Args::Digest(xxh3(parts))
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

/// Whether the operation encodes an extra byte count in its outcome.
pub fn writes(op: u8) -> bool {
    use crate::op::*;
    matches!(
        op,
        WRITE | WRITE_AT | STDOUT | STDERR | RANDOM | NEXT_ENTRIES
    )
}
/// One family byte followed by the 16 IP octets.
pub const RESOLVED_IP_LEN: usize = 17;

pub fn carries_payload(op: u8) -> bool {
    use crate::op::*;
    matches!(
        op,
        READ | READ_AT
            | MKDTEMP
            | RANDOM
            | LISTEN
            | ACCEPT
            | RECV_BOUNDED
            | TLS_CONNECT
            | SECRET_MAC
            | RESOLVE
            | STDIN_READ
            | NEXT_ENTRIES
            | STAT
            | STAT_FILE
    )
}

/// A successful TLS_CONNECT carries a length-prefixed ALPN, the fixed peer
/// digest and a length-prefixed channel binding (zero length when absent).
pub fn tls_connect_payload(bytes: &[u8]) -> Result<(&[u8], &[u8; 32], &[u8]), Corrupt> {
    let (&alpn_len, rest) = bytes.split_first().ok_or(Corrupt)?;
    let (alpn, rest) = rest
        .split_at_checked(usize::from(alpn_len))
        .ok_or(Corrupt)?;
    let (peer, rest) = rest.split_first_chunk::<32>().ok_or(Corrupt)?;
    let (&binding_len, binding) = rest.split_first().ok_or(Corrupt)?;
    if !matches!(binding_len, 0 | 32 | 48 | 64) || usize::from(binding_len) != binding.len() {
        return Err(Corrupt);
    }
    Ok((alpn, peer, binding))
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
    /// The frame bytes, then the DATA frames continuing the payload.
    fn encode(&self, op: u8, out: &mut Vec<u8>, rest: &mut Vec<Vec<u8>>) {
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
        if op == op::STDIN_READ
            && status != OK
            && (value != 0
                || (status == ERROR && (error.0 != 8 || u32::try_from(error.1).is_err())))
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
            if matches!(op, op::LISTEN | op::ACCEPT) {
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
                .and_then(|n| n.checked_mul(RESOLVED_IP_LEN))
                .ok_or(Corrupt),
            op::RESOLVE if self.value != 0 => Err(Corrupt),
            op::RECV_BOUNDED | op::RESOLVE | op::STDIN_READ => Ok(0),
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
    Choose {
        gap: u64,
        index: u64,
    },
    /// Args carries the whole argument vector, each `varint len + bytes`.
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
            Frame::Wait { gap, task } => {
                out.push(F_WAIT);
                put_varint(&mut out, *gap);
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
            Frame::Choose { gap, index } => put_choose(&mut out, *gap, *index),
            Frame::Grant { gap, kind, args } => {
                out.push(F_GRANT);
                put_varint(&mut out, *gap);
                out.push(*kind);
                if *kind == ARGS_KIND {
                    put_bytes(
                        &mut out,
                        args.as_deref().expect("an Args grant carries its vector"),
                    );
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
            Frame::Repeat { count } => put_repeat(&mut out, *count),
            Frame::End { gap, ending: e } => {
                out.push(F_END);
                put_varint(&mut out, *gap);
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
            let has_task = matches!(kind, F_ISSUE | F_DONE | F_OP | F_WAKE | F_CANCEL | F_WAIT);
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
                F_CHOOSE => {
                    let (gap, index) = (c.varint()?, c.varint()?);
                    chooses = [chooses[1], Some((gap, index))];
                    Frame::Choose { gap, index }
                }
                F_GRANT => {
                    let gap = c.varint()?;
                    let kind = c.u8()?;
                    if kind > SECRET_KIND {
                        return Err(Corrupt);
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
                    let (Frame::Op { out, .. } | Frame::Done { out, .. }) = &mut f else {
                        unreachable!()
                    };
                    if chunk.is_empty() || chunk.len() > PAYLOAD_CHUNK {
                        return Err(Corrupt);
                    }
                    out.payload.extend_from_slice(chunk);
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
    fn complete(&mut self, f: Frame, needs: usize) -> Result<Option<Frame>, Corrupt> {
        let (Frame::Op { out, .. } | Frame::Done { out, .. }) = &f else {
            unreachable!()
        };
        match out.payload.len().cmp(&needs) {
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
