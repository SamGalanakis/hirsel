//! Package names, aliases and versions (packages.md section 1).

use std::cmp::Ordering;
use std::fmt;

/// `[a-z][a-z0-9_]*`, at most 64 bytes: a `NAME`, a bare name and an alias.
pub(crate) fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 1 to `max` bytes of `[a-z0-9-]` that do not start or end with `-`.
fn label(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.len())
        && !text.starts_with('-')
        && !text.ends_with('-')
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn host(host: &str) -> bool {
    let labels: Vec<_> = host.split('.').collect();
    host.len() <= 253
        && (2..=127).contains(&labels.len())
        && labels.iter().all(|l| label(l, 63))
        && !labels
            .last()
            .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
}

/// `HOST/NAME` or `HOST/TENANT/NAME`.
pub(crate) fn scoped(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    name.len() <= 255
        && match parts[..] {
            [h, n] => host(h) && identifier(n),
            [h, t, n] => host(h) && label(t, 64) && identifier(n),
            _ => false,
        }
}

/// `local/NAME`.
pub(crate) fn local(name: &str) -> bool {
    name.strip_prefix("local/").is_some_and(identifier)
}

/// A name in one of section 1's three forms: scoped, local or bare.
pub(crate) fn any(name: &str) -> bool {
    scoped(name) || local(name) || identifier(name)
}

/// An alias: an identifier other than `core`.
pub(crate) fn alias(alias: &str) -> bool {
    identifier(alias) && alias != "core"
}

/// `MAJOR.MINOR.PATCH[-PRE]` (section 1).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct Version {
    pub(crate) major: u32,
    pub(crate) minor: u32,
    pub(crate) patch: u32,
    /// Empty for none.
    pub(crate) pre: String,
}

fn number(text: &str) -> Option<u32> {
    if text.is_empty()
        || !text.bytes().all(|b| b.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return None;
    }
    text.parse().ok()
}

/// Whether `pre` is a prerelease of section 1: identifiers of `[0-9A-Za-z-]`, joined by `.`.
pub(crate) fn prerelease(pre: &str) -> bool {
    pre.len() <= 255
        && pre.split('.').all(|id| {
            (1..=64).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !(id.len() > 1 && id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()))
        })
}

impl Version {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, pre),
            None => (text, ""),
        };
        if text.contains('-') && !prerelease(pre) {
            return None;
        }
        let mut parts = core.split('.');
        let version = Self {
            major: number(parts.next()?)?,
            minor: number(parts.next()?)?,
            patch: number(parts.next()?)?,
            pre: pre.to_owned(),
        };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

impl Ord for Version {
    /// Semantic Versioning 2.0.0 precedence.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    // Numeric identifiers have no leading zero, so a longer one is larger.
                    let id = |s: &str| match s.bytes().all(|b| b.is_ascii_digit()) {
                        true => (false, s.len(), s.to_owned()),
                        false => (true, 0, s.to_owned()),
                    };
                    let a = self.pre.split('.').map(id);
                    let b = other.pre.split('.').map(id);
                    a.cmp(b)
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
