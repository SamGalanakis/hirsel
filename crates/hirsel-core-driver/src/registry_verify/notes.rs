use super::{Error, Hash, decimal, decode_base64, tiles};
use hirsel_core_net::verify_ed25519;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;

const FRESHNESS: u64 = 86_400;
const NOTE_LIMIT: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Log {
    Release,
    Status,
}

impl Log {
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Self::Release => "log",
            Self::Status => "status",
        }
    }
}

/// Operator names come from the local trust configuration independently of note key names.
pub(crate) enum Witnesses<'a> {
    Preview,
    Pinned([(&'a str, &'a str); 2]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    name: String,
    id: [u8; 4],
    public: [u8; 32],
}

impl Key {
    fn parse(text: &str, kind: u8) -> Option<Self> {
        let (name, rest) = text.split_once('+')?;
        let (id, data) = rest.split_once('+')?;
        if !key_name(name)
            || id.len() != 8
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return None;
        }
        let data = decode_base64(data)?;
        let (&tag, public) = data.split_first()?;
        if tag != kind {
            return None;
        }
        let public = public.try_into().ok()?;
        let digest = Sha256::new()
            .chain_update(name)
            .chain_update(b"\n")
            .chain_update(&data)
            .finalize();
        let id: [u8; 4] = hirsel_core_ids::hex::decode(id)?.try_into().ok()?;
        (digest[..4] == id).then(|| Self {
            name: name.into(),
            id,
            public,
        })
    }

    fn matches(&self, name: &str, id: [u8; 4]) -> bool {
        self.name == name && self.id == id
    }
}

fn key_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '+')
}

#[derive(Debug, PartialEq, Eq)]
struct Policy {
    authority: String,
    log: Log,
    origin: String,
    signer: Key,
    namespace: Option<Hash>,
    witnesses: Option<[(String, Key); 2]>,
}

#[derive(Clone)]
pub(crate) struct Trust(Arc<Policy>);

impl Trust {
    pub(crate) fn new(
        authority: &str,
        log: Log,
        signer: &str,
        witnesses: Witnesses<'_>,
    ) -> Result<Self, Error> {
        let origin = format!("{authority}/{}", log.prefix());
        let signer = Key::parse(signer, 1).ok_or(Error::Trust)?;
        let witnesses = match witnesses {
            Witnesses::Preview => None,
            Witnesses::Pinned([(a, a_key), (b, b_key)]) => {
                let a_key = Key::parse(a_key, 4).ok_or(Error::Trust)?;
                let b_key = Key::parse(b_key, 4).ok_or(Error::Trust)?;
                if a.is_empty()
                    || b.is_empty()
                    || a == b
                    || a_key.public == b_key.public
                    || a_key.public == signer.public
                    || b_key.public == signer.public
                    || a_key.matches(&b_key.name, b_key.id)
                    || a_key.matches(&signer.name, signer.id)
                    || b_key.matches(&signer.name, signer.id)
                {
                    return Err(Error::Trust);
                }
                let mut pins = [(a.to_owned(), a_key), (b.to_owned(), b_key)];
                pins.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
                Some(pins)
            }
        };
        Ok(Self(Arc::new(Policy {
            authority: authority.into(),
            log,
            origin,
            signer,
            namespace: None,
            witnesses,
        })))
    }

    pub(crate) fn in_namespace(mut self, digest: Hash) -> Self {
        Arc::get_mut(&mut self.0)
            .expect("new registry trust")
            .namespace = Some(digest);
        self
    }

    pub(crate) fn accept(
        &self,
        note: &[u8],
        previous: Option<&Checkpoint>,
        now: u64,
        mut read_tile: impl FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Result<Checkpoint, Error> {
        self.authenticate(note)?
            .accept_after(previous, now, &mut read_tile)
    }

    pub(crate) fn owns(&self, checkpoint: &Checkpoint) -> bool {
        self.0 == checkpoint.policy
    }

    /// Checks signatures without requiring a historical envelope to be fresh.
    pub(crate) fn authenticate(&self, note: &[u8]) -> Result<Checkpoint, Error> {
        if note.len() > NOTE_LIMIT || note.iter().any(|&byte| byte < b' ' && byte != b'\n') {
            return Err(Error::Note);
        }
        let envelope = std::str::from_utf8(note).map_err(|_| Error::Note)?;
        let (body, signatures) = envelope.rsplit_once("\n\n").ok_or(Error::Note)?;
        let text = &envelope.as_bytes()[..body.len() + 1];
        let mut lines = body.split('\n');
        if lines.next() != Some(&self.0.origin) {
            return Err(Error::Origin);
        }
        let size = lines.next().and_then(decimal).ok_or(Error::Note)?;
        let root: Hash = lines
            .next()
            .and_then(decode_base64)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Error::Note)?;
        if lines.next().is_some() || (size == 0 && root != tiles::empty_root()) {
            return Err(Error::Note);
        }
        let mut seen = BTreeSet::new();
        let mut signed = false;
        let mut times = [None; 2];
        for line in signatures
            .strip_suffix('\n')
            .ok_or(Error::Note)?
            .split('\n')
        {
            let (name, data) = line
                .strip_prefix("— ")
                .and_then(|line| line.split_once(' '))
                .ok_or(Error::Note)?;
            if !key_name(name) {
                return Err(Error::Note);
            }
            let data = decode_base64(data).ok_or(Error::Note)?;
            let (id, signature) = data.split_first_chunk::<4>().ok_or(Error::Note)?;
            if signature.is_empty() || !seen.insert((name, *id)) {
                return Err(Error::Note);
            }
            if self.0.signer.matches(name, *id) {
                if !verify_ed25519(&self.0.signer.public, text, signature) {
                    return Err(Error::Signature);
                }
                signed = true;
            }
            if let Some(witnesses) = &self.0.witnesses {
                for ((_, key), time) in witnesses.iter().zip(&mut times) {
                    if key.matches(name, *id) {
                        let (timestamp, signature) =
                            signature.split_first_chunk::<8>().ok_or(Error::Signature)?;
                        let timestamp = u64::from_be_bytes(*timestamp);
                        let mut input = format!("cosignature/v1\ntime {timestamp}\n").into_bytes();
                        input.extend_from_slice(text);
                        if !verify_ed25519(&key.public, &input, signature) {
                            return Err(Error::Signature);
                        }
                        *time = Some(timestamp);
                    }
                }
            }
        }
        if !signed {
            return Err(Error::Signature);
        }
        let times = match self.0.witnesses {
            None => None,
            Some(_) => match times {
                [Some(a), Some(b)] => Some([a, b]),
                _ => return Err(Error::WitnessQuorum),
            },
        };
        Ok(Checkpoint {
            policy: self.0.clone(),
            size,
            root,
            times,
            note: note.into(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    policy: Arc<Policy>,
    size: u64,
    root: Hash,
    times: Option<[u64; 2]>,
    note: Arc<[u8]>,
}

impl Checkpoint {
    pub(crate) fn log(&self) -> Log {
        self.policy.log
    }

    pub(crate) fn authority(&self) -> &str {
        &self.policy.authority
    }

    pub(crate) fn same_trust(&self, other: &Self) -> bool {
        self.policy == other.policy
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn root(&self) -> Hash {
        self.root
    }

    pub(crate) fn note(&self) -> &[u8] {
        &self.note
    }

    pub(crate) fn is_preview(&self) -> bool {
        self.times.is_none()
    }

    pub(crate) fn witness_times(&self) -> Option<[u64; 2]> {
        self.times
    }

    pub(crate) fn accept_after(
        &self,
        previous: Option<&Self>,
        now: u64,
        mut read_tile: impl FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Result<Self, Error> {
        if let Some(previous) = previous {
            if self.policy != previous.policy {
                return Err(Error::Trust);
            }
            if self.size < previous.size
                || (self.size == previous.size && self.root != previous.root)
            {
                return Err(conflict(previous, self));
            }
        }
        self.check_fresh(now)?;
        if let Some(previous) = previous {
            self.extends(previous, &mut read_tile)?;
        }
        Ok(self.clone())
    }

    pub(crate) fn extends(
        &self,
        previous: &Self,
        mut read_tile: impl FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        if !self.same_trust(previous) {
            return Err(Error::Trust);
        }
        if self.size < previous.size
            || !tiles::consistent(
                previous.size,
                previous.root,
                self.size,
                self.root,
                &mut read_tile,
            )?
        {
            return Err(conflict(previous, self));
        }
        Ok(())
    }

    pub(crate) fn check_fresh(&self, now: u64) -> Result<(), Error> {
        if self.times.is_some_and(|times| {
            times
                .into_iter()
                .any(|time| time > now || now - time > FRESHNESS)
        }) {
            return Err(Error::WitnessTime);
        }
        Ok(())
    }
}

pub(super) fn conflict(prior: &Checkpoint, candidate: &Checkpoint) -> Error {
    Error::Conflict {
        prior: prior.note().into(),
        candidate: candidate.note().into(),
    }
}
