use crate::{RULE_SYNTAX, Rule, TlsRule, TlsTrust};

/// An immutable Net grant rule in the launcher and the runtime.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum GrantRule {
    Address(Rule),
    Tls(TlsRule),
}

impl GrantRule {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.split_whitespace().next() == Some("Tls") {
            TlsRule::parse(text)
                .map(Self::Tls)
                .ok_or_else(|| format!("expected --grant {RULE_SYNTAX}: {text}"))
        } else {
            Rule::parse(text).map(Self::Address)
        }
    }

    pub fn intersect(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Address(a), Self::Address(b)) => a.intersect(*b).map(Self::Address),
            (Self::Tls(a), Self::Tls(b)) => a.intersect(b).map(Self::Tls),
            _ => None,
        }
    }

    pub fn logical_size(&self) -> u64 {
        37 + match self {
            Self::Address(_) => 0,
            Self::Tls(rule) => rule.pattern_bytes(),
        }
    }
}

/// C grant row. Direction 2 uses count-delimited UTF-8 patterns and a pinned
/// trust. The original address fields and offsets are unchanged.
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

    /// # Safety
    /// A TLS row pins patterns_len readable bytes at patterns.
    pub unsafe fn decode(self) -> Option<GrantRule> {
        if self.address.dir != 2 {
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
        let trust = match self.trust {
            0 => TlsTrust::System,
            1 => TlsTrust::Roots(self.roots),
            _ => return None,
        };
        TlsRule::new(patterns, self.address.first, self.address.last, trust).map(GrantRule::Tls)
    }
}
