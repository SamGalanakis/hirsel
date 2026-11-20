use crate::Pattern;

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
    pub const SYNTAX: &str =
        "expected --grant 'Secret:<parameter>=env:VAR|file:PATH to <patterns>'";

    pub fn logical_size(patterns: &[Pattern], len: u64) -> Option<u64> {
        Pattern::list_bytes(patterns)
            .checked_add(len)?
            .checked_add(39)
            .map(|n| n & !7)
    }

    pub fn parse(spec: &str) -> Option<Self> {
        let (parameter, value) = spec.strip_prefix("Secret:")?.split_once('=')?;
        let (source, patterns) = value.rsplit_once(" to ")?;
        Self::new(parameter, source, patterns)
    }

    pub fn new(parameter: &str, source: &str, patterns: &str) -> Option<Self> {
        if parameter.is_empty() {
            return None;
        }
        SecretSource::parse(source)?;
        Some(Self {
            parameter: parameter.to_owned(),
            source: source.to_owned(),
            patterns: Pattern::list_text(&Pattern::parse_list(patterns)?),
        })
    }
}
