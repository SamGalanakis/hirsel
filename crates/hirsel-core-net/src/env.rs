//! Name policy and inert snapshot shared by launch, evaluation and replay.

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct EnvPatterns(Vec<String>);

impl EnvPatterns {
    pub const SYNTAX: &str = "expected --grant 'Env=<pattern>[,<pattern>]'";

    pub fn parse(text: &str) -> Option<Self> {
        Self::from_patterns(text.split(','))
    }

    pub fn from_patterns<'a>(patterns: impl Iterator<Item = &'a str>) -> Option<Self> {
        patterns
            .map(|pattern| {
                let prefix = pattern.strip_suffix('*');
                (pattern == "*" || valid_env_name(prefix.unwrap_or(pattern)))
                    .then(|| pattern.to_owned())
            })
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }

    pub fn text(&self) -> String {
        self.0.join(",")
    }

    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    pub fn covers(&self, name: &str) -> bool {
        valid_env_name(name) && self.0.iter().any(|pattern| covers(pattern, name))
    }

    pub fn intersect(&self, other: &Self) -> Self {
        // Each nonempty pairwise intersection is one of the input patterns.
        // Collect each such pattern once, even when many pairs produce it.
        let patterns: std::collections::BTreeSet<_> = self
            .0
            .iter()
            .filter(|pattern| other.covers_pattern(pattern))
            .chain(
                other
                    .0
                    .iter()
                    .filter(|pattern| self.covers_pattern(pattern)),
            )
            .collect();
        Self(patterns.into_iter().cloned().collect())
    }

    fn covers_pattern(&self, pattern: &str) -> bool {
        match pattern.strip_suffix('*') {
            Some(prefix) => self.0.iter().any(|p| {
                p.strip_suffix('*')
                    .is_some_and(|parent| prefix.starts_with(parent))
            }),
            None => self.covers(pattern),
        }
    }

    pub fn charge(&self) -> Option<u64> {
        self.0
            .iter()
            .try_fold(0u64, |n, pattern| n.checked_add(24 + pattern.len() as u64))?
            .checked_add(7)
            .map(|n| n & !7)
    }
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', ',', '*', '\0'])
}

fn covers(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == pattern,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvSnapshot {
    pub patterns: EnvPatterns,
    /// Sorted by UTF-8 name bytes. Values retain their original OS bytes.
    pub entries: Vec<(String, Vec<u8>)>,
}
