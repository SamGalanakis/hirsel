use crate::{Name, Pattern};
use std::sync::Arc;

/// The uses of a Secret other than `tls_send` (TLS-5), as a set. `mac`
/// permits verification too, so its set holds `VERIFY` as well, and an
/// intersection is the bitwise and.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SecretUse(u8);

impl SecretUse {
    pub const VERIFY: Self = Self(1);
    pub const MAC: Self = Self(2 | 1);
    pub const SIGN_ED25519: Self = Self(4);
    pub const SIGN_P256: Self = Self(8);
    pub const SIGN_RS256: Self = Self(16);
    pub const WRAP: Self = Self(32);
    pub const MTLS_CLIENT_ID: Self = Self(64);
    pub const KDF: Self = Self(128);

    /// Each use word in its canonical order. A set prints `mac` alone for
    /// `MAC`, which implies `verify`.
    const WORDS: [(&str, Self); 8] = [
        ("mac", Self::MAC),
        ("verify", Self::VERIFY),
        ("kdf", Self::KDF),
        ("sign(ed25519)", Self::SIGN_ED25519),
        ("sign(p256)", Self::SIGN_P256),
        ("sign(rs256)", Self::SIGN_RS256),
        ("wrap", Self::WRAP),
        ("mtls_client_id", Self::MTLS_CLIENT_ID),
    ];

    pub fn permits(self, needed: Self) -> bool {
        self.0 & needed.0 == needed.0
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn word(word: &str) -> Option<Self> {
        Self::WORDS
            .iter()
            .find(|(w, _)| *w == word)
            .map(|&(_, u)| u)
    }
}

/// A Secret's typed set of uses (TLS-5): `tls_send(<patterns>)`, `mac`,
/// `verify`, `kdf`, `sign(<alg>)`, `wrap` and `mtls_client_id`, written in that
/// order, separated by single spaces. A DNS name appears only inside
/// `tls_send(...)`, so no name can be read as a use.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SecretPolicy {
    /// The `tls_send` patterns (TLS-3 syntax); empty without `tls_send`.
    pub send: Arc<[Pattern]>,
    pub uses: SecretUse,
}

impl SecretPolicy {
    pub fn parse(text: &str) -> Option<Self> {
        let mut send: Option<Box<[Pattern]>> = None;
        let mut uses = SecretUse::default();
        for token in text.split(' ') {
            if let Some(patterns) = token
                .strip_prefix("tls_send(")
                .and_then(|t| t.strip_suffix(')'))
            {
                if send.is_some() {
                    return None;
                }
                send = Some(Pattern::parse_list(patterns)?);
            } else {
                let word = SecretUse::word(token)?;
                // A word given twice, or `verify` beside `mac`, is not canonical.
                if uses.0 & word.0 != 0 {
                    return None;
                }
                uses.0 |= word.0;
            }
        }
        let send: Arc<[Pattern]> = send.map_or_else(|| Arc::from([]), Into::into);
        if send.is_empty() && uses.is_empty() {
            return None;
        }
        Some(Self { send, uses })
    }

    /// The policy that sends to `name` alone, with no other use: a Secret
    /// whose policy covers a stream's verified name, narrowed to it (TLS-5,
    /// virtual streams).
    pub fn exactly(name: &Name) -> Self {
        Self {
            send: Arc::from([Pattern::Exact(name.clone())]),
            uses: SecretUse::default(),
        }
    }

    /// The requested policy of a derived key. A parent with `kdf` may
    /// derive keys for MAC, verification or further derivation.
    pub fn derived(&self, requested: &str) -> Option<Self> {
        if !self.uses.permits(SecretUse::KDF) {
            return None;
        }
        let child = Self::parse(requested)?;
        let allowed = SecretUse(SecretUse::MAC.0 | SecretUse::KDF.0);
        (child.send.is_empty() && allowed.permits(child.uses)).then_some(child)
    }

    pub fn intersect(&self, requested: &Self) -> Option<Self> {
        let send = Pattern::intersection(&self.send, &requested.send);
        let uses = SecretUse(self.uses.0 & requested.uses.0);
        if send.is_empty() && uses.is_empty() {
            return None;
        }
        Some(Self {
            send: send.into(),
            uses,
        })
    }

    /// Whether every name and use this policy permits, `bound` permits too.
    pub fn within(&self, bound: &Self) -> bool {
        bound.uses.permits(self.uses)
            && self
                .send
                .iter()
                .all(|p| bound.send.iter().any(|q| p.is_subset_of(q)))
    }

    pub fn text(&self) -> String {
        let mut words = Vec::new();
        let send;
        if !self.send.is_empty() {
            send = format!("tls_send({})", Pattern::list_text(&self.send));
            words.push(send.as_str());
        }
        let mut left = self.uses.0;
        for (word, u) in SecretUse::WORDS {
            if left & u.0 == u.0 {
                words.push(word);
                left &= !u.0;
            }
        }
        words.join(" ")
    }

    /// The CLI's policy text: canonical, or `to <patterns>` (one token) for
    /// `tls_send(<patterns>)`, then any other uses.
    pub(crate) fn cli(text: &str) -> Option<Self> {
        match text.strip_prefix("to ") {
            Some(rest) => {
                let (patterns, more) = rest
                    .split_once(' ')
                    .map_or((rest, None), |(p, m)| (p, Some(m)));
                let canonical = format!("tls_send({patterns})");
                Self::parse(&more.map_or(canonical.clone(), |m| format!("{canonical} {m}")))
            }
            None => Self::parse(text),
        }
    }
}

/// The public part of a Secret grant, shared by startup and the tape header.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SecretGrant {
    pub parameter: String,
    pub source: String,
    /// The canonical policy text.
    pub policy: String,
}

pub enum SecretSource<'a> {
    Env(&'a str),
    File(&'a str),
}

impl<'a> SecretSource<'a> {
    pub fn parse(source: &'a str) -> Option<Self> {
        let (kind, value) = source.split_once(':')?;
        if value.is_empty() || value.contains('\0') {
            return None;
        }
        match kind {
            "env" if !value.contains('=') => Some(Self::Env(value)),
            "file" => Some(Self::File(value)),
            _ => None,
        }
    }
}

impl SecretGrant {
    pub const SYNTAX: &str = "expected --grant 'Secret:<parameter>=env:VAR|file:PATH <uses>', <uses> being tls_send(<patterns>), mac, verify, kdf, sign(<alg>), wrap or mtls_client_id separated by spaces, with 'to <patterns>' for tls_send(<patterns>)";

    pub fn logical_size(patterns: &[Pattern], len: u64) -> Option<u64> {
        Pattern::list_bytes(patterns)
            .checked_add(len)?
            .checked_add(39)
            .map(|n| n & !7)
    }

    /// `Secret:<parameter>=<source> <policy>`: the policy is the longest
    /// space-separated suffix that parses, so a file path may hold spaces.
    pub fn parse(spec: &str) -> Option<Self> {
        let (parameter, value) = spec.strip_prefix("Secret:")?.split_once('=')?;
        value
            .match_indices(' ')
            .find_map(|(at, _)| Some((&value[..at], SecretPolicy::cli(&value[at + 1..])?)))
            .and_then(|(source, policy)| Self::new(parameter, source, &policy.text()))
    }

    pub fn new(parameter: &str, source: &str, policy: &str) -> Option<Self> {
        if parameter.is_empty() {
            return None;
        }
        SecretSource::parse(source)?;
        Some(Self {
            parameter: parameter.to_owned(),
            source: source.to_owned(),
            policy: SecretPolicy::parse(policy)?.text(),
        })
    }
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&key, data).as_ref().try_into().unwrap()
}

pub fn derive_hmac_sha256(key: &[u8], prefix: &[u8], data: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
    // RFC 2104 hashes a key longer than one block before using it in HMAC.
    // Hash the two slices separately so the full prefixed key needs no allocation.
    const BLOCK: usize = 64;
    let key = if prefix.len() <= BLOCK && key.len() <= BLOCK - prefix.len() {
        let mut combined = zeroize::Zeroizing::new([0; BLOCK]);
        combined[..prefix.len()].copy_from_slice(prefix);
        combined[prefix.len()..prefix.len() + key.len()].copy_from_slice(key);
        ring::hmac::Key::new(
            ring::hmac::HMAC_SHA256,
            &combined[..prefix.len() + key.len()],
        )
    } else {
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(prefix);
        digest.update(key);
        ring::hmac::Key::new(ring::hmac::HMAC_SHA256, digest.finish().as_ref())
    };
    zeroize::Zeroizing::new(ring::hmac::sign(&key, data).as_ref().to_vec())
}

pub fn verify_hmac_sha256(key: &[u8], data: &[u8], tag: &[u8]) -> bool {
    if tag.len() != 32 {
        return false;
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::verify(&key, data, tag).is_ok()
}
