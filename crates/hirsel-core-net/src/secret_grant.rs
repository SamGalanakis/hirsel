use crate::{Name, Pattern};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum SecretUse {
    #[default]
    None,
    Verify,
    Mac,
}

impl SecretUse {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "verify" => Some(Self::Verify),
            "mac" => Some(Self::Mac),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Verify => "verify",
            Self::Mac => "mac",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SecretPolicy {
    pub patterns: Arc<[Pattern]>,
    pub usage: SecretUse,
}

impl SecretPolicy {
    pub fn parse(text: &str) -> Option<Self> {
        let (patterns, word) = text.rsplit_once(' ').unwrap_or(("", text));
        let usage = SecretUse::parse(word).unwrap_or_default();
        let patterns = if usage == SecretUse::None {
            text
        } else {
            patterns
        };
        let patterns = if patterns.is_empty() && usage != SecretUse::None {
            if text != word {
                return None;
            }
            Arc::from([])
        } else {
            let patterns = Pattern::parse_list(patterns)?;
            if patterns.iter().any(|p| {
                matches!(p, Pattern::Exact(Name::Dns(name)) if SecretUse::parse(name).is_some())
            }) {
                return None;
            }
            patterns.into()
        };
        Some(Self { patterns, usage })
    }

    /// The policy covering `name` alone, with no use: a Secret whose policy
    /// covers a stream's verified name, narrowed to it (TLS-5, virtual
    /// streams).
    pub fn exactly(name: &Name) -> Self {
        Self {
            patterns: Arc::from([Pattern::Exact(name.clone())]),
            usage: SecretUse::None,
        }
    }

    pub fn intersect(&self, requested: &Self) -> Option<Self> {
        let patterns = Pattern::intersection(&self.patterns, &requested.patterns);
        let usage = self.usage.min(requested.usage);
        if patterns.is_empty() && usage == SecretUse::None {
            return None;
        }
        Some(Self {
            patterns: patterns.into(),
            usage,
        })
    }

    pub fn text(&self) -> String {
        let mut text = Pattern::list_text(&self.patterns);
        if self.usage != SecretUse::None {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(self.usage.word());
        }
        text
    }
}

/// The public part of a Secret grant, shared by startup and the tape header.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SecretGrant {
    pub parameter: String,
    pub source: String,
    pub patterns: String,
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
    pub const SYNTAX: &str = "expected --grant 'Secret:<parameter>=env:VAR|file:PATH to <policy>' or 'Secret:<parameter>=env:VAR|file:PATH mac|verify'";

    pub fn logical_size(patterns: &[Pattern], len: u64) -> Option<u64> {
        Pattern::list_bytes(patterns)
            .checked_add(len)?
            .checked_add(39)
            .map(|n| n & !7)
    }

    pub fn parse(spec: &str) -> Option<Self> {
        let (parameter, value) = spec.strip_prefix("Secret:")?.split_once('=')?;
        let (source, policy) = value.rsplit_once(" to ").or_else(|| {
            value
                .rsplit_once(' ')
                .filter(|&(_, word)| SecretUse::parse(word).is_some())
        })?;
        Self::new(parameter, source, policy)
    }

    pub fn new(parameter: &str, source: &str, patterns: &str) -> Option<Self> {
        if parameter.is_empty() {
            return None;
        }
        SecretSource::parse(source)?;
        Some(Self {
            parameter: parameter.to_owned(),
            source: source.to_owned(),
            patterns: SecretPolicy::parse(patterns)?.text(),
        })
    }
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&key, data).as_ref().try_into().unwrap()
}

pub fn verify_hmac_sha256(key: &[u8], data: &[u8], tag: &[u8]) -> bool {
    if tag.len() != 32 {
        return false;
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::verify(&key, data, tag).is_ok()
}
