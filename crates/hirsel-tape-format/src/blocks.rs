//! Checked blocks and the tape header in block zero.
use super::{
    BLOCK_CHECK, BLOCK_HEAD, CHOOSER_COMMIT, CHOOSER_FIFO, CRED_KIND, Corrupt, Cursor, END, FORMAT,
    KIND_CALL, KIND_RUN, MAGIC, MAX_LEN, SECRET_KIND, TRUNCATED, put_bytes, put_varint, xxh3,
};
use hirsel_core_net::{
    EnvPatterns, EnvSnapshot, GrantRule, ResolveRule, Rule, RuleRow, TlsRule, TlsTrust,
    UnixDirection, UnixRule,
};

#[cfg(test)]
mod tests;

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
    /// TLS-7 `Cred` grants, after the Secret grants.
    pub cred_grants: Vec<hirsel_core_net::CredGrant>,
    pub env: Option<EnvSnapshot>,
}
/// The complete encoded header does not fit in block zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderTooLarge;

const NET_KIND: u8 = 1;
const ENV_KIND: u8 = 7;
const NET_RULE_LEN: usize = 23;
/// NET-18: an address rule with the `Public` reach is its 23-byte row after
/// this tag. An untagged row is an `Any` rule.
const PUBLIC_RULE: u8 = 8;

fn read_net_rule(bytes: &[u8], public: bool) -> Result<Rule, Corrupt> {
    RuleRow {
        dir: bytes[0],
        family: bytes[1],
        hi: u64::from_le_bytes(bytes[2..10].try_into().unwrap()),
        lo: u64::from_le_bytes(bytes[10..18].try_into().unwrap()),
        bits: bytes[18],
        reach: u8::from(public),
        first: u16::from_le_bytes(bytes[19..21].try_into().unwrap()),
        last: u16::from_le_bytes(bytes[21..23].try_into().unwrap()),
    }
    .decode()
    .ok_or(Corrupt)
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
            Some(4) => {
                c.u8()?;
                let direction = UnixDirection::from_tag(c.u8()?).ok_or(Corrupt)?;
                let mut read_text = || {
                    let len = usize::try_from(c.varint()?).map_err(|_| Corrupt)?;
                    std::str::from_utf8(c.take(len)?).map_err(|_| Corrupt)
                };
                let root = read_text()?;
                let path = read_text()?;
                rules.push(GrantRule::Unix(
                    UnixRule::new(direction, root, path).ok_or(Corrupt)?,
                ));
            }
            tag => {
                let public = tag == Some(&PUBLIC_RULE);
                if public {
                    c.u8()?;
                }
                let rule = read_net_rule(c.take(NET_RULE_LEN)?, public)?;
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
    out.extend(net_snapshot(rules)?);
    Ok(out)
}

/// A `Net` capability's rules as the header's grant snapshot writes them
/// after the `Net` kind byte: the rule count, then each rule. A capability
/// snapshot (docs/recording.md, "Snapshots", tag 5) holds these bytes, and
/// there a narrowing may leave no rule.
pub fn net_snapshot(rules: &[GrantRule]) -> Result<Vec<u8>, HeaderTooLarge> {
    let mut out = Vec::new();
    put_varint(&mut out, rules.len() as u64);
    for rule in rules {
        match rule {
            GrantRule::Address(rule) => {
                let rule = RuleRow::from(*rule);
                if rule.reach == hirsel_core_net::Reach::Public as u8 {
                    out.push(PUBLIC_RULE);
                }
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
                    // Only a served Net's root holds Any, and a tape never
                    // records one: the reader refuses it as Corrupt.
                    TlsTrust::Any => out.push(2),
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
            GrantRule::Unix(rule) => {
                let (root, path) = rule.endpoint().ok_or(HeaderTooLarge)?;
                out.extend_from_slice(&[4, rule.direction as u8]);
                for text in [root, path] {
                    put_varint(&mut out, text.len() as u64);
                    out.extend_from_slice(text.as_bytes());
                }
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

/// An `Env` capability as the header's grant snapshot writes it after the
/// `Env` kind byte: its patterns' text, then each covered entry's name and
/// value in name order.
pub fn env_snapshot(env: &EnvSnapshot) -> Result<Vec<u8>, HeaderTooLarge> {
    let mut out = Vec::new();
    put_bytes(&mut out, env.patterns.text().as_bytes());
    put_varint(&mut out, env.entries.len() as u64);
    for (name, value) in &env.entries {
        if out
            .len()
            .saturating_add(name.len())
            .saturating_add(value.len())
            > MAX_LEN
        {
            return Err(HeaderTooLarge);
        }
        put_bytes(&mut out, name.as_bytes());
        put_bytes(&mut out, value);
    }
    Ok(out)
}

/// A `Cred` capability as the header's grant snapshot writes it after the
/// `Cred` kind byte: its parameter, source and policy.
pub fn cred_snapshot(grant: &hirsel_core_net::CredGrant) -> Result<Vec<u8>, HeaderTooLarge> {
    let mut out = Vec::new();
    for field in [&grant.parameter, &grant.source, &grant.policy] {
        if out.len().saturating_add(field.len()) > MAX_LEN {
            return Err(HeaderTooLarge);
        }
        put_bytes(&mut out, field.as_bytes());
    }
    Ok(out)
}

fn read_env(c: &mut Cursor<'_>) -> Result<EnvSnapshot, Corrupt> {
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
    while c.bytes.get(c.at) == Some(&SECRET_KIND) {
        c.u8()?;
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

fn read_cred_grants(c: &mut Cursor<'_>) -> Result<Vec<hirsel_core_net::CredGrant>, Corrupt> {
    let mut grants = Vec::<hirsel_core_net::CredGrant>::new();
    while !c.done() {
        if c.u8()? != CRED_KIND {
            return Err(Corrupt);
        }
        let mut field = || std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt);
        let grant =
            hirsel_core_net::CredGrant::new(field()?, field()?, field()?).map_err(|_| Corrupt)?;
        if grants.iter().any(|old| old.parameter == grant.parameter) {
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
        let mut len = 2 + 1 + 16 + 32 + 4 + 8 + 8 + 1;
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
            grants.extend(env_snapshot(env)?);
        }
        for grant in &self.secret_grants {
            grants.push(SECRET_KIND);
            for field in [&grant.parameter, &grant.source, &grant.policy] {
                if grants.len().saturating_add(field.len()) > MAX_LEN {
                    return Err(HeaderTooLarge);
                }
                put_bytes(&mut grants, field.as_bytes());
            }
        }
        for grant in &self.cred_grants {
            grants.push(CRED_KIND);
            grants.extend(cred_snapshot(grant)?);
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
        if (kind == KIND_CALL && !grants.is_empty()) || !c.done() {
            return Err(Corrupt);
        }
        let mut grants = Cursor::new(grants);
        let net_rules = if grants.bytes.first() == Some(&NET_KIND) {
            grants.u8()?;
            read_net_rules(&mut grants)?
        } else {
            vec![]
        };
        let env = if grants.bytes.get(grants.at) == Some(&ENV_KIND) {
            grants.u8()?;
            Some(read_env(&mut grants)?)
        } else {
            None
        };
        let secret_grants = read_secret_grants(&mut grants)?;
        let cred_grants = read_cred_grants(&mut grants)?;
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
            cred_grants,
            env,
        })
    }
}
/// Where block 0's payload keeps the tape id: after format and kind.
pub const TAPE_ID_AT: usize = 3;

/// One block's bytes: head, payload, then the check over the tape id and
/// everything before the check.
pub fn block(tape_id: &[u8; 16], flags: u8, seq: u64, payload: &[u8]) -> Vec<u8> {
    stream_block(tape_id, flags, 0, seq, payload)
}

pub fn stream_block(
    tape_id: &[u8; 16],
    flags: u8,
    stream: u32,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    assert!(payload.len() <= MAX_LEN);
    let mut out = Vec::with_capacity(BLOCK_HEAD + payload.len() + BLOCK_CHECK);
    out.push(flags);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&stream.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    let check = xxh3(&[tape_id, &out]);
    out.extend_from_slice(&check.to_le_bytes());
    out
}

/// Seal a block filled in place: `bytes` holds `BLOCK_HEAD` reserved bytes
/// and then the payload. Writes the head and appends the check, giving the
/// same bytes as `stream_block`.
pub fn seal(tape_id: &[u8; 16], flags: u8, stream: u32, seq: u64, bytes: &mut Vec<u8>) {
    let len = bytes.len() - BLOCK_HEAD;
    assert!(len <= MAX_LEN);
    bytes[0] = flags;
    bytes[1..4].copy_from_slice(&MAGIC);
    bytes[4..8].copy_from_slice(&stream.to_le_bytes());
    bytes[8..16].copy_from_slice(&seq.to_le_bytes());
    bytes[16..20].copy_from_slice(&(len as u32).to_le_bytes());
    let check = xxh3(&[tape_id, bytes]);
    bytes.extend_from_slice(&check.to_le_bytes());
}

/// Why reading stopped before the tape's END block (section 5.2's table).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    /// End of file exactly at a block boundary, with no END block read.
    Missing,
    /// A trailing partial block, or a TRUNCATED block.
    Truncated,
    /// A complete block failing a check, or anything after an END block.
    Damaged(u64),
    /// A validated block whose bytes break the grammar.
    Corrupt,
    /// A checked header for a different tape format.
    UnsupportedFormat(u16),
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
        if stream(&self.bytes) != 0 || sequence(&self.bytes) != self.seq {
            return Err(Stop::Damaged(self.seq).into());
        }
        check_digest(&self.bytes, &self.tape_id, self.seq)?;
        self.seq += 1;
        let flags = self.bytes[0];
        if flags & TRUNCATED != 0 {
            return Err(Stop::Truncated.into());
        }
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

pub(super) fn header_payload(bytes: &[u8]) -> &[u8] {
    &bytes[BLOCK_HEAD..bytes.len() - BLOCK_CHECK]
}

pub(super) fn header_block<E>(
    read: &mut impl FnMut(&mut [u8]) -> Result<usize, E>,
    format: Option<u16>,
) -> Result<Vec<u8>, ReadError<E>> {
    let mut bytes = Vec::new();
    read_block(&mut bytes, 0, read)?;
    check_head(&bytes, 0)?;
    if bytes[0] != 0 || stream(&bytes) != 0 || sequence(&bytes) != 0 {
        return Err(Stop::Damaged(0).into());
    }
    let payload = header_payload(&bytes);
    let tape_id = payload
        .get(TAPE_ID_AT..TAPE_ID_AT + 16)
        .ok_or(Stop::Damaged(0))?;
    check_digest(&bytes, tape_id.try_into().unwrap(), 0)?;
    if let Some(format) = format {
        let version = u16::from_le_bytes(payload[..2].try_into().unwrap());
        if version != format {
            return Err(Stop::UnsupportedFormat(version).into());
        }
    }
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

    let bytes = header_block(&mut read, Some(FORMAT))?;
    route(header_payload(&bytes)).map_err(|e| Stop::from(e).into())
}

pub(super) fn read_block<E>(
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
    let len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
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

pub(super) fn check_head(bytes: &[u8], seq: u64) -> Result<(), Stop> {
    let flags = bytes[0];
    if bytes[1..4] != MAGIC
        || !matches!(flags, 0 | END | TRUNCATED)
        || (flags == TRUNCATED && bytes.len() != BLOCK_HEAD + BLOCK_CHECK)
    {
        return Err(Stop::Damaged(seq));
    }
    Ok(())
}

pub(super) fn check_digest(bytes: &[u8], id: &[u8; 16], seq: u64) -> Result<(), Stop> {
    let (body, stored) = bytes.split_at(bytes.len() - BLOCK_CHECK);
    if xxh3(&[id, body]) != u64::from_le_bytes(stored.try_into().unwrap()) {
        return Err(Stop::Damaged(seq));
    }
    Ok(())
}

pub(super) fn stream(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[4..8].try_into().unwrap())
}

pub(super) fn sequence(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[8..16].try_into().unwrap())
}

pub(super) fn capability_snapshot(kind: u8, bytes: &[u8]) -> Result<(), Corrupt> {
    let mut c = Cursor::new(bytes);
    match kind {
        NET_KIND => {
            read_net_rules(&mut c)?;
        }
        ENV_KIND => {
            read_env(&mut c)?;
        }
        CRED_KIND => {
            let mut field = || std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt);
            hirsel_core_net::CredGrant::new(field()?, field()?, field()?).map_err(|_| Corrupt)?;
        }
        4..=6 => {
            if c.u8()? > 1 {
                return Err(Corrupt);
            }
        }
        super::EXEC_KIND => return super::exec_snapshot_rules(bytes).map(|_| ()),
        _ => {}
    }
    c.done().then_some(()).ok_or(Corrupt)
}
