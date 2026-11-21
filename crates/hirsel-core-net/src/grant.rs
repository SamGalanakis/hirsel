use crate::{Name, Pattern, RULE_SYNTAX, Rule, TlsRule, TlsTrust};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ResolveRule(Box<[Pattern]>);

impl ResolveRule {
    pub fn new(patterns: &str) -> Option<Self> {
        Some(Self(Pattern::parse_list(patterns)?))
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
}

impl GrantRule {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.split_whitespace().next() {
            Some("Tls") => TlsRule::parse(text).map(Self::Tls),
            Some("Resolve") => ResolveRule::parse(text).map(Self::Resolve),
            _ => return Rule::parse(text).map(Self::Address),
        }
        .ok_or_else(|| format!("expected --grant {RULE_SYNTAX}: {text}"))
    }

    pub fn intersect(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Address(a), Self::Address(b)) => a.intersect(*b).map(Self::Address),
            (Self::Tls(a), Self::Tls(b)) => a.intersect(b).map(Self::Tls),
            (Self::Resolve(a), Self::Resolve(b)) => a.intersect(b).map(Self::Resolve),
            _ => None,
        }
    }

    pub fn logical_size(&self) -> u64 {
        37 + match self {
            Self::Address(_) => 0,
            Self::Tls(rule) => rule.pattern_bytes(),
            Self::Resolve(rule) => Pattern::list_bytes(&rule.0),
        }
    }
}

/// C grant row. Directions 2 (TLS) and 3 (Resolve) use length-delimited UTF-8
/// patterns. Only TLS carries a trust pin.
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

    /// # Safety
    /// A TLS or Resolve row pins patterns_len readable bytes at patterns.
    pub unsafe fn decode(self) -> Option<GrantRule> {
        if !matches!(self.address.dir, 2 | 3) {
            return self
                .address
                .valid()
                .then_some(GrantRule::Address(self.address));
        }
        if self.patterns_len == 0 || self.patterns.is_null() {
            return None;
        }
        let patterns = unsafe { std::slice::from_raw_parts(self.patterns, self.patterns_len) };
        let patterns = std::str::from_utf8(patterns).ok()?;
        if self.address.dir == 3 {
            return ResolveRule::new(patterns).map(GrantRule::Resolve);
        }
        let trust = match self.trust {
            0 => TlsTrust::System,
            1 => TlsTrust::Roots(self.roots),
            _ => return None,
        };
        TlsRule::new(patterns, self.address.first, self.address.last, trust).map(GrantRule::Tls)
    }
}
