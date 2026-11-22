use crate::{Name, Pattern, RULE_SYNTAX, Rule, TlsRule, TlsTrust};
use std::collections::BTreeMap;

/// The path grammar shared by Dir and Unix endpoints.
pub fn valid_relative_path(path: &[u8]) -> bool {
    !path.contains(&0)
        && path
            .split(|&byte| byte == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[repr(u8)]
pub enum UnixDirection {
    Connect,
    Listen,
    Bind,
    Send,
}

impl UnixDirection {
    pub const ALL: [Self; 4] = [Self::Connect, Self::Listen, Self::Bind, Self::Send];

    pub fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL.get(usize::from(tag)).copied()
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Connect => "UnixConnect",
            Self::Listen => "UnixListen",
            Self::Bind => "UnixBind",
            Self::Send => "UnixSend",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct UnixRule {
    pub direction: UnixDirection,
    endpoint: Option<(String, String)>,
}

impl UnixRule {
    pub fn new(direction: UnixDirection, root: &str, path: &str) -> Option<Self> {
        if root.is_empty() || !valid_relative_path(path.as_bytes()) {
            return None;
        }
        Some(Self {
            direction,
            endpoint: Some((root.to_owned(), path.to_owned())),
        })
    }

    pub fn served(direction: UnixDirection) -> Self {
        Self {
            direction,
            endpoint: None,
        }
    }

    pub fn endpoint(&self) -> Option<(&str, &str)> {
        self.endpoint
            .as_ref()
            .map(|(root, path)| (root.as_str(), path.as_str()))
    }

    pub fn permits(&self, direction: UnixDirection, root: &str, path: &str) -> bool {
        self.direction == direction && self.endpoint().is_none_or(|addr| addr == (root, path))
    }

    fn intersect(&self, other: &Self) -> Option<Self> {
        if self.direction != other.direction {
            return None;
        }
        match (self.endpoint(), other.endpoint()) {
            (None, _) => Some(other.clone()),
            (_, None) => Some(self.clone()),
            (a, b) if a == b => Some(self.clone()),
            _ => None,
        }
    }
}

/// Root bindings are launch configuration. They have no rule or tape representation.
#[derive(Default)]
pub struct NetGrant {
    pub rules: Vec<GrantRule>,
    pub roots: BTreeMap<String, String>,
}

impl NetGrant {
    pub fn parse(rows: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, String> {
        let mut grant = Self::default();
        for row in rows {
            let text = row.as_ref();
            let mut words = text.split_whitespace();
            if words.next() == Some("UnixRoot") {
                let (Some(label), Some(path), None) = (words.next(), words.next(), words.next())
                else {
                    return Err(format!("expected --grant {RULE_SYNTAX}: {text}"));
                };
                if !path.starts_with('/') || text.contains('\0') || grant.roots.contains_key(label)
                {
                    return Err(format!("expected --grant {RULE_SYNTAX}: {text}"));
                }
                grant.roots.insert(label.to_owned(), path.to_owned());
            } else {
                grant.rules.push(GrantRule::parse(text)?);
            }
        }
        for rule in &grant.rules {
            if let GrantRule::Unix(rule) = rule
                && let Some((root, _)) = rule.endpoint()
                && !grant.roots.contains_key(root)
            {
                return Err(format!(
                    "expected --grant {RULE_SYNTAX}: missing UnixRoot {root}"
                ));
            }
        }
        Ok(grant)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ResolveRule(Box<[Pattern]>);

impl ResolveRule {
    pub fn new(patterns: &str) -> Option<Self> {
        Some(Self(Pattern::parse_list(patterns)?))
    }

    /// A served Net's root rule: every name.
    pub fn served() -> Self {
        Self(Box::new([Pattern::Any]))
    }

    /// A rule as stored in a rule row: `*` only as the served root wrote it.
    pub fn stored(patterns: &str) -> Option<Self> {
        if patterns == "*" {
            return Some(Self::served());
        }
        Self::new(patterns)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some("Resolve"), Some(patterns), None) => Self::new(patterns),
            _ => None,
        }
    }

    pub fn permits(&self, name: &Name) -> bool {
        self.0.iter().any(|pattern| pattern.covers(name))
    }

    fn intersect(&self, other: &Self) -> Option<Self> {
        let patterns = Pattern::intersection(&self.0, &other.0);
        (!patterns.is_empty()).then_some(Self(patterns))
    }

    pub fn pattern_text(&self) -> String {
        Pattern::list_text(&self.0)
    }
}

/// An immutable Net grant rule in the launcher and the runtime.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum GrantRule {
    Address(Rule),
    Tls(TlsRule),
    Resolve(ResolveRule),
    Unix(UnixRule),
}

impl GrantRule {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut words = text.split_whitespace();
        match words.next() {
            Some("Tls") => TlsRule::parse(text).map(Self::Tls),
            Some("Resolve") => ResolveRule::parse(text).map(Self::Resolve),
            Some(word) if word.starts_with("Unix") => {
                match (
                    UnixDirection::ALL
                        .into_iter()
                        .find(|dir| dir.word() == word),
                    words.next(),
                    words.next(),
                    words.next(),
                ) {
                    (Some(dir), Some(root), Some(path), None) if !text.contains('\0') => {
                        UnixRule::new(dir, root, path).map(Self::Unix)
                    }
                    _ => None,
                }
            }
            _ => return Rule::parse(text).map(Self::Address),
        }
        .ok_or_else(|| format!("expected --grant {RULE_SYNTAX}: {text}"))
    }

    pub fn intersect(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Address(a), Self::Address(b)) => a.intersect(*b).map(Self::Address),
            (Self::Tls(a), Self::Tls(b)) => a.intersect(b).map(Self::Tls),
            (Self::Resolve(a), Self::Resolve(b)) => a.intersect(b).map(Self::Resolve),
            (Self::Unix(a), Self::Unix(b)) => a.intersect(b).map(Self::Unix),
            _ => None,
        }
    }

    /// Whether this is a served Net's root rule, which no grant may carry.
    pub fn is_served(&self) -> bool {
        match self {
            Self::Address(_) => false,
            Self::Tls(rule) => rule.is_served(),
            Self::Resolve(rule) => rule.0.contains(&Pattern::Any),
            Self::Unix(rule) => rule.endpoint().is_none(),
        }
    }

    pub fn logical_size(&self) -> u64 {
        match self {
            Self::Address(_) => 37,
            Self::Tls(rule) => 37 + rule.pattern_bytes(),
            Self::Resolve(rule) => 37 + Pattern::list_bytes(&rule.0),
            Self::Unix(rule) => rule.endpoint().map_or(8, |(root, path)| {
                (8 + root.len() as u64 + path.len() as u64).next_multiple_of(8)
            }),
        }
    }
}

/// C grant row. TLS, Resolve and Unix use the length-delimited text field.
/// Unix uses `hi` as the split between label and path. A launch-only root row
/// uses the same split with a host path and never enters the runtime rule list.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuleView {
    pub address: Rule,
    pub patterns: *const u8,
    pub patterns_len: usize,
    pub trust: u8,
    pub roots: [u8; 32],
}

impl RuleView {
    pub fn address(address: Rule) -> Self {
        Self {
            address,
            patterns: std::ptr::null(),
            patterns_len: 0,
            trust: 0,
            roots: [0; 32],
        }
    }

    pub fn tls(rule: &TlsRule, patterns: &str) -> Self {
        let (first, last) = rule.ports();
        let (trust, roots) = match rule.trust() {
            TlsTrust::System => (0, [0; 32]),
            TlsTrust::Roots(roots) => (1, roots),
            TlsTrust::Any => (2, [0; 32]),
        };
        Self {
            address: Rule {
                dir: 2,
                family: 0,
                hi: 0,
                lo: 0,
                bits: 0,
                first,
                last,
            },
            patterns: patterns.as_ptr(),
            patterns_len: patterns.len(),
            trust,
            roots,
        }
    }

    pub fn resolve(patterns: &str) -> Self {
        Self {
            patterns: patterns.as_ptr(),
            patterns_len: patterns.len(),
            ..Self::address(Rule {
                dir: 3,
                family: 0,
                hi: 0,
                lo: 0,
                bits: 0,
                first: 0,
                last: 0,
            })
        }
    }

    pub fn unix(rule: &UnixRule, text: &str) -> Self {
        let split = rule.endpoint().map_or(0, |(root, _)| root.len());
        Self::unix_text(4, rule.direction as u8, split, text)
    }

    pub fn unix_root(label_len: usize, text: &str) -> Self {
        Self::unix_text(5, 0, label_len, text)
    }

    fn unix_text(dir: u8, family: u8, split: usize, text: &str) -> Self {
        Self {
            patterns: text.as_ptr(),
            patterns_len: text.len(),
            ..Self::address(Rule {
                dir,
                family,
                hi: split as u64,
                lo: 0,
                bits: 0,
                first: 0,
                last: 0,
            })
        }
    }

    pub fn is_unix_root(self) -> bool {
        self.address.dir == 5
    }

    /// # Safety
    /// The row pins `patterns_len` readable bytes at `patterns`.
    pub unsafe fn decode_unix_root(&self) -> Option<(&str, &str)> {
        if !self.is_unix_root() {
            return None;
        }
        let (label, path) = unsafe { self.text()? }.split_at_checked(self.address.hi as usize)?;
        (!label.is_empty()
            && path.starts_with('/')
            && !label.contains('\0')
            && !path.contains('\0'))
        .then_some((label, path))
    }

    unsafe fn text(&self) -> Option<&str> {
        if self.patterns_len == 0 {
            return Some("");
        }
        if self.patterns.is_null() {
            return None;
        }
        let patterns = unsafe { std::slice::from_raw_parts(self.patterns, self.patterns_len) };
        std::str::from_utf8(patterns).ok()
    }

    /// # Safety
    /// A text row pins `patterns_len` readable bytes at `patterns`.
    pub unsafe fn decode(self) -> Option<GrantRule> {
        if !matches!(self.address.dir, 2..=4) {
            return self
                .address
                .valid()
                .then_some(GrantRule::Address(self.address));
        }
        let patterns = unsafe { self.text()? };
        if self.address.dir == 4 {
            let direction = UnixDirection::from_tag(self.address.family)?;
            let (root, path) = patterns.split_at_checked(self.address.hi as usize)?;
            return if patterns.is_empty() {
                Some(GrantRule::Unix(UnixRule::served(direction)))
            } else {
                UnixRule::new(direction, root, path).map(GrantRule::Unix)
            };
        }
        if patterns.is_empty() {
            return None;
        }
        if self.address.dir == 3 {
            return ResolveRule::stored(patterns).map(GrantRule::Resolve);
        }
        let trust = match self.trust {
            0 => TlsTrust::System,
            1 => TlsTrust::Roots(self.roots),
            2 => TlsTrust::Any,
            _ => return None,
        };
        TlsRule::stored(patterns, self.address.first, self.address.last, trust).map(GrantRule::Tls)
    }
}
