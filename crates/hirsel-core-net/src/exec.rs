//! EXEC-2's logical authority, independent of the native images and directories.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod policy;
pub mod served;
pub use crate::host::exec_api::*;
pub use policy::{GRANT_SYNTAX, Policy, Profile, Request};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgToken {
    Lit(Vec<u8>),
    Any,
    Rest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Form {
    prefix: Vec<ArgToken>,
    rest: bool,
}

impl Form {
    pub fn new(mut tokens: Vec<ArgToken>) -> Option<Self> {
        let rest = matches!(tokens.last(), Some(ArgToken::Rest));
        if rest {
            tokens.pop();
        }
        if tokens.iter().any(|token| match token {
            ArgToken::Lit(bytes) => bytes.contains(&0),
            ArgToken::Any => false,
            ArgToken::Rest => true,
        }) {
            return None;
        }
        Some(Self {
            prefix: tokens,
            rest,
        })
    }

    pub fn tokens(&self) -> impl Iterator<Item = &ArgToken> {
        self.prefix
            .iter()
            .chain(self.rest.then_some(&ArgToken::Rest))
    }

    pub fn len(&self) -> usize {
        self.prefix.len() + usize::from(self.rest)
    }

    pub fn is_empty(&self) -> bool {
        self.prefix.is_empty() && !self.rest
    }

    fn permits_length(&self, n: usize) -> bool {
        self.prefix.len() == n || self.rest && self.prefix.len() < n
    }

    pub fn permits(&self, args: &[&[u8]]) -> bool {
        self.permits_length(args.len())
            && self
                .prefix
                .iter()
                .zip(args)
                .all(|(token, arg)| match token {
                    ArgToken::Lit(literal) => literal == arg,
                    ArgToken::Any => true,
                    ArgToken::Rest => unreachable!(),
                })
    }

    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let len = self.prefix.len().max(other.prefix.len());
        if !self.permits_length(len) || !other.permits_length(len) {
            return None;
        }
        let mut prefix = Vec::with_capacity(len);
        for i in 0..len {
            let a = self.prefix.get(i).unwrap_or(&ArgToken::Any);
            let b = other.prefix.get(i).unwrap_or(&ArgToken::Any);
            prefix.push(match (a, b) {
                (ArgToken::Any, token) | (token, ArgToken::Any) => token.clone(),
                _ if a == b => a.clone(),
                _ => return None,
            });
        }
        Some(Self {
            prefix,
            rest: self.rest && other.rest,
        })
    }

    fn charge(&self) -> u128 {
        8 + self
            .tokens()
            .map(|token| {
                8 + match token {
                    ArgToken::Lit(bytes) => bytes.len() as u128,
                    _ => 0,
                }
            })
            .sum::<u128>()
    }
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && !key.as_bytes().iter().any(|byte| matches!(byte, 0 | b'='))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.as_bytes().iter().any(|byte| matches!(byte, 0 | b','))
}

#[derive(Clone, Debug)]
pub struct Names(BTreeSet<String>);

impl Names {
    pub fn parse(text: &str) -> Option<Self> {
        let mut names = BTreeSet::new();
        if !text.is_empty() {
            for name in text.split(',') {
                if !valid_name(name) || !names.insert(name.to_owned()) {
                    return None;
                }
            }
        }
        Some(Self(names))
    }
}

pub enum ExecRule {
    Programs(Names),
    Form(String, Form),
    EnvKeys(String, Names),
    Dirs(String, Names),
}

#[derive(Clone)]
enum Selection<T> {
    Any,
    Only(T),
}

impl<T> Selection<T> {
    fn permits(&self, permits: impl FnOnce(&T) -> bool) -> bool {
        match self {
            Self::Any => true,
            Self::Only(only) => permits(only),
        }
    }

    fn charge(&self, charge: impl FnOnce(&T) -> u128) -> u128 {
        match self {
            Self::Any => 8,
            Self::Only(only) => charge(only),
        }
    }
}

impl Selection<BTreeSet<String>> {
    fn intersect(&mut self, other: &Names) {
        *self = Self::Only(match self {
            Self::Any => other.0.clone(),
            Self::Only(only) => only.intersection(&other.0).cloned().collect(),
        });
    }

    fn charge_names(&self) -> u128 {
        self.charge(|names| names.iter().map(|name| 8 + name.len() as u128).sum())
    }
}

#[derive(Clone)]
pub struct ProfileRules {
    forms: Selection<Vec<Form>>,
    keys: Selection<BTreeSet<String>>,
    labels: Selection<BTreeSet<String>>,
    fixed: Arc<BTreeSet<String>>,
}

impl ProfileRules {
    pub fn host(
        forms: Vec<Form>,
        keys: BTreeSet<String>,
        labels: BTreeSet<String>,
        fixed: BTreeSet<String>,
    ) -> Self {
        Self {
            forms: Selection::Only(forms),
            keys: Selection::Only(keys),
            labels: Selection::Only(labels),
            fixed: Arc::new(fixed),
        }
    }

    fn universal() -> Self {
        Self {
            forms: Selection::Any,
            keys: Selection::Any,
            labels: Selection::Any,
            fixed: Arc::default(),
        }
    }

    fn denied(&self) -> bool {
        !self.forms.permits(|forms| !forms.is_empty())
            || !self.labels.permits(|labels| !labels.is_empty())
    }

    fn permits(&self, args: &[&[u8]], keys: &[&str], label: &str) -> bool {
        self.forms
            .permits(|forms| forms.iter().any(|form| form.permits(args)))
            && self
                .keys
                .permits(|allowed| keys.iter().all(|key| allowed.contains(*key)))
            && self.labels.permits(|labels| labels.contains(label))
    }

    fn charge(&self) -> u128 {
        self.forms
            .charge(|forms| forms.iter().map(Form::charge).sum())
            + self.keys.charge_names()
            + self.labels.charge_names()
            + 8 * self.fixed.len() as u128
    }
}

#[derive(Clone)]
pub struct ExecRules {
    profiles: BTreeMap<String, ProfileRules>,
    remainder: bool,
}

impl ExecRules {
    pub fn host(profiles: BTreeMap<String, ProfileRules>) -> Self {
        Self {
            profiles,
            remainder: false,
        }
    }

    pub fn universal() -> Self {
        Self {
            profiles: BTreeMap::new(),
            remainder: true,
        }
    }

    pub fn permits(&self, program: &str, args: &[&[u8]], keys: &[&str], label: &str) -> bool {
        let mut seen = BTreeSet::new();
        if !valid_name(program)
            || !valid_name(label)
            || args.iter().any(|arg| arg.contains(&0))
            || keys.iter().any(|key| !valid_key(key) || !seen.insert(*key))
        {
            return false;
        }
        self.profiles
            .get(program)
            .map_or(self.remainder, |rules| rules.permits(args, keys, label))
    }

    pub fn narrow(&self, rule: &ExecRule) -> Option<Self> {
        let mut narrowed = self.clone();
        let (program, update): (&str, _) = match rule {
            ExecRule::Programs(names) => {
                narrowed.profiles.retain(|name, _| names.0.contains(name));
                if self.remainder {
                    for name in &names.0 {
                        narrowed
                            .profiles
                            .entry(name.clone())
                            .or_insert_with(ProfileRules::universal);
                    }
                }
                narrowed.remainder = false;
                narrowed.profiles.retain(|_, profile| !profile.denied());
                return Some(narrowed);
            }
            ExecRule::Form(program, _)
            | ExecRule::EnvKeys(program, _)
            | ExecRule::Dirs(program, _) => (program, rule),
        };
        if !valid_name(program) {
            return None;
        }
        if let ExecRule::EnvKeys(_, keys) = update
            && keys.0.iter().any(|key| !valid_key(key))
        {
            return None;
        }
        if narrowed.remainder {
            narrowed
                .profiles
                .entry(program.to_owned())
                .or_insert_with(ProfileRules::universal);
        }
        if let Some(profile) = narrowed.profiles.get_mut(program) {
            match update {
                ExecRule::Form(_, form) => {
                    profile.forms = Selection::Only(match &profile.forms {
                        Selection::Any => vec![form.clone()],
                        Selection::Only(forms) => {
                            forms.iter().filter_map(|old| old.intersect(form)).collect()
                        }
                    });
                }
                ExecRule::EnvKeys(_, keys) => profile.keys.intersect(keys),
                ExecRule::Dirs(_, labels) => profile.labels.intersect(labels),
                ExecRule::Programs(_) => unreachable!(),
            }
        }
        if narrowed.remainder {
            for profile in narrowed
                .profiles
                .values_mut()
                .filter(|profile| profile.denied())
            {
                *profile = ProfileRules::host(
                    Vec::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                );
            }
        } else {
            narrowed.profiles.retain(|_, profile| !profile.denied());
        }
        Some(narrowed)
    }

    pub fn charge(&self) -> Option<u64> {
        let bytes = 32
            + u128::from(self.remainder) * 8
            + self
                .profiles
                .iter()
                .map(|(name, profile)| 8 + name.len() as u128 + profile.charge())
                .sum::<u128>();
        u64::try_from((bytes + 7) & !7).ok()
    }
}

pub struct LogicalLaunch<'a> {
    pub program: &'a str,
    pub args: Vec<&'a [u8]>,
    pub env: Vec<(&'a str, &'a [u8])>,
    pub dir: &'a str,
    pub path: &'a str,
    pub piped: [bool; 3],
}

impl LogicalLaunch<'_> {
    fn valid(&self) -> bool {
        let fits = |len| u32::try_from(len).is_ok();
        fits(self.program.len())
            && fits(self.dir.len())
            && fits(self.path.len())
            && fits(self.args.len())
            && fits(self.env.len())
            && self
                .args
                .iter()
                .all(|arg| fits(arg.len()) && !arg.contains(&0))
            && self.env.iter().all(|(key, value)| {
                fits(key.len()) && fits(value.len()) && valid_key(key) && !value.contains(&0)
            })
            && self.env.windows(2).all(|pair| pair[0].0 != pair[1].0)
            && (self.path.is_empty() || crate::valid_relative_path(self.path.as_bytes()))
    }

    pub fn permitted(&self, rules: &ExecRules) -> bool {
        if !self.valid() {
            return false;
        }
        let keys: Vec<_> = self.env.iter().map(|(key, _)| *key).collect();
        rules.permits(self.program, &self.args, &keys, self.dir)
    }
}

impl LogicalLaunch<'_> {
    pub fn served_payloads(&self) -> [Vec<u8>; 2] {
        fn encode<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
            let mut bytes = Vec::new();
            for part in parts {
                bytes.extend_from_slice(&(part.len() as u32).to_le_bytes());
                bytes.extend_from_slice(part);
            }
            bytes
        }
        [
            encode(self.args.iter().copied()),
            encode(self.env.iter().flat_map(|&(k, v)| [k.as_bytes(), v])),
        ]
    }
}
