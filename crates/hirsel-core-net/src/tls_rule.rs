//! TLS-3 verification names, pinned trust and rule intersection.

use std::net::IpAddr;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum Name {
    Ip(IpAddr),
    Dns(Box<str>),
}

impl Name {
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.strip_suffix('.').unwrap_or(name);
        if let Ok(ip) = name.parse() {
            return Some(Self::Ip(ip));
        }
        if name.len() > 253
            || !name.split('.').all(|label| {
                let bytes = label.as_bytes();
                bytes.len() <= 63
                    && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
                    && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
                    && bytes
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            })
        {
            return None;
        }
        // Match rustls's DNS/IP distinction. The final label cannot look
        // like a decimal or hexadecimal IPv4 component.
        let name = name.to_ascii_lowercase();
        let suffix = name.rsplit('.').next().unwrap();
        if suffix.bytes().all(|b| b.is_ascii_digit())
            || suffix
                .strip_prefix("0x")
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return None;
        }
        Some(Self::Dns(name.into_boxed_str()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum Pattern {
    Exact(Name),
    Wildcard(Box<str>),
    /// Every name (`*`, NET-18). Never a `tls_send` pattern (TLS-5).
    Any,
}

impl Pattern {
    pub fn parse_list(patterns: &str) -> Option<Box<[Self]>> {
        patterns.split(',').map(Self::parse).collect()
    }

    pub fn parse(pattern: &str) -> Option<Self> {
        if pattern == "*" {
            Some(Self::Any)
        } else if let Some(suffix) = pattern.strip_prefix("*.") {
            let Name::Dns(suffix) = Name::parse(suffix)? else {
                return None;
            };
            Some(Self::Wildcard(suffix))
        } else {
            Name::parse(pattern).map(Self::Exact)
        }
    }

    pub fn intersection(left: &[Self], right: &[Self]) -> Box<[Self]> {
        // A nonempty pairwise intersection always equals one of its inputs.
        // Visit each input pattern once so overlaps do not multiply storage.
        let covered = |pattern: &Pattern, rules: &[Pattern]| {
            rules.iter().any(|rule| pattern.is_subset_of(rule))
        };
        left.iter()
            .filter(|pattern| covered(pattern, right))
            .chain(
                right
                    .iter()
                    .filter(|pattern| covered(pattern, left) && !left.contains(pattern)),
            )
            .cloned()
            .collect()
    }

    pub fn list_text(patterns: &[Self]) -> String {
        use std::fmt::Write as _;
        let mut text = String::new();
        for pattern in patterns {
            if !text.is_empty() {
                text.push(',');
            }
            match pattern {
                Pattern::Exact(Name::Ip(ip)) => write!(text, "{ip}").unwrap(),
                Pattern::Exact(Name::Dns(name)) => text.push_str(name),
                Pattern::Wildcard(suffix) => {
                    text.push_str("*.");
                    text.push_str(suffix);
                }
                Pattern::Any => text.push('*'),
            }
        }
        text
    }

    pub fn list_bytes(patterns: &[Self]) -> u64 {
        patterns
            .iter()
            .map(|pattern| match pattern {
                Pattern::Exact(Name::Dns(name)) | Pattern::Wildcard(name) => name.len() as u64 + 32,
                Pattern::Exact(Name::Ip(_)) | Pattern::Any => 32,
            })
            .sum()
    }

    fn includes_suffix(name: &str, suffix: &str) -> bool {
        name.strip_suffix(suffix)
            .is_some_and(|before| before.ends_with('.'))
    }

    pub fn covers(&self, name: &Name) -> bool {
        match (self, name) {
            (Self::Any, _) => true,
            (Self::Exact(exact), name) => exact == name,
            (Self::Wildcard(suffix), Name::Dns(name)) => Self::includes_suffix(name, suffix),
            (Self::Wildcard(_), Name::Ip(_)) => false,
        }
    }

    pub(crate) fn is_subset_of(&self, other: &Self) -> bool {
        match (self, other) {
            (_, Self::Any) => true,
            (Self::Any, _) => false,
            (Self::Exact(name), pattern) => pattern.covers(name),
            (Self::Wildcard(a), Self::Wildcard(b)) => a == b || Self::includes_suffix(a, b),
            (Self::Wildcard(_), Self::Exact(_)) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum TlsTrust {
    System,
    Roots([u8; 32]),
    /// Whatever the caller pins: only a served Net's root holds it, since
    /// its server answers the handshake. No grant text can name it.
    Any,
}

impl TlsTrust {
    pub fn parse(trust: &str) -> Option<Self> {
        if trust == "System" {
            return Some(Self::System);
        }
        let digest = trust.strip_prefix("Roots=sha256:")?;
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self::Roots(bytes))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct TlsRule {
    patterns: Box<[Pattern]>,
    first: u16,
    last: u16,
    trust: TlsTrust,
}

impl TlsRule {
    pub fn new(patterns: &str, first: u16, last: u16, trust: TlsTrust) -> Option<Self> {
        if first > last {
            return None;
        }
        Some(Self {
            patterns: Pattern::parse_list(patterns)?,
            first,
            last,
            trust,
        })
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let ("Tls", patterns, ports, trust) =
            (words.next()?, words.next()?, words.next()?, words.next()?)
        else {
            return None;
        };
        if words.next().is_some() {
            return None;
        }
        let (first, last) = ports.split_once('-')?;
        Self::new(
            patterns,
            first.parse().ok()?,
            last.parse().ok()?,
            TlsTrust::parse(trust)?,
        )
    }

    /// A served Net's root rule: every name, port and trust.
    pub fn served() -> Self {
        Self {
            patterns: Box::new([Pattern::Any]),
            first: 0,
            last: u16::MAX,
            trust: TlsTrust::Any,
        }
    }

    /// A rule as stored in a rule row: trust `Any` only as the served root
    /// wrote it.
    pub fn stored(patterns: &str, first: u16, last: u16, trust: TlsTrust) -> Option<Self> {
        if patterns == "*" && trust == TlsTrust::Any {
            return ((first, last) == (0, u16::MAX)).then(Self::served);
        }
        if trust == TlsTrust::Any {
            return None;
        }
        Self::new(patterns, first, last, trust)
    }

    /// Whether this is a served Net's root rule, which no grant may carry:
    /// its trust is `Any`, which no grant text can name.
    pub fn is_served(&self) -> bool {
        self.trust == TlsTrust::Any
    }

    pub fn permits(&self, name: &Name, port: u16, trust: TlsTrust) -> bool {
        (self.trust == trust || self.trust == TlsTrust::Any)
            && (self.first..=self.last).contains(&port)
            && self.patterns.iter().any(|pattern| pattern.covers(name))
    }

    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let first = self.first.max(other.first);
        let last = self.last.min(other.last);
        let trust = match (self.trust, other.trust) {
            (TlsTrust::Any, trust) | (trust, TlsTrust::Any) => trust,
            (a, b) if a == b => a,
            _ => return None,
        };
        if first > last {
            return None;
        }
        let patterns = Pattern::intersection(&self.patterns, &other.patterns);
        (!patterns.is_empty()).then_some(Self {
            patterns,
            first,
            last,
            trust,
        })
    }
}

impl TlsRule {
    pub fn ports(&self) -> (u16, u16) {
        (self.first, self.last)
    }

    pub fn trust(&self) -> TlsTrust {
        self.trust
    }

    pub fn pattern_text(&self) -> String {
        Pattern::list_text(&self.patterns)
    }

    /// Accommodate the retained names as well as the fixed rule charge.
    pub fn pattern_bytes(&self) -> u64 {
        Pattern::list_bytes(&self.patterns)
    }
}
