use hirsel_core_ids::DefPath;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{path::PathBuf, process::Command};

mod sites;
use sites::Sites;

const MODEL_FAILURE_KINDS: [&str; 4] = ["Violated", "Stuck", "Unwitnessed", "Handler"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LawRow {
    pub callee: String,
    pub site: String,
    pub class: String,
    pub numbers: Vec<u64>,
    pub bound: Option<String>,
    pub per_build: bool,
    pub witnessed: BTreeMap<String, u64>,
    pub(crate) kind: Option<u64>,
}

impl LawRow {
    pub(crate) fn rank(&self) -> u8 {
        class_rank(&self.class)
    }

    fn strength(&self) -> (u8, &[u64], bool, &str, &Option<String>, Option<u64>) {
        (
            self.rank(),
            &self.numbers,
            !self.per_build,
            &self.class,
            &self.bound,
            self.kind,
        )
    }

    fn display(&self) -> String {
        let [first, rest @ ..] = &self.numbers[..] else {
            return self.class.clone();
        };
        let second = rest.first().copied().unwrap_or(0);
        let counts = match self.class.as_str() {
            "truncated" | "held" | "vacuous" if self.callee == "all" => {
                format!("run {first}, discarded {second}")
            }
            "gave up" => format!("{first} cases, {second} rejected"),
            "sampled" | "vacuous" => format!("{first} cases"),
            "unbounded" => format!("at {first}"),
            "closed" => format!("{first}, {second}, {}", rest[1]),
            "failed" => {
                let kind = MODEL_FAILURE_KINDS[self.kind.unwrap() as usize];
                if self.per_build {
                    kind.into()
                } else {
                    format!("{kind}, {first}")
                }
            }
            _ => String::new(),
        };
        let summary = match (&self.bound, counts.is_empty()) {
            (Some(bound), true) => bound.clone(),
            (Some(bound), false) => format!("{counts}; {bound}"),
            (None, _) => counts,
        };
        let mut text = if self.class == "to depth" {
            format!("to depth {first}")
        } else {
            self.class.clone()
        };
        if !summary.is_empty() {
            text.push_str(&format!(" ({summary})"));
        }
        if self.per_build {
            text.push_str(" (per build)");
        }
        text
    }
}

pub(crate) fn class_rank(class: &str) -> u8 {
    match class {
        "falsified" | "failed" | "diverged" => 0,
        "gave up" | "unbounded" | "incomplete" => 1,
        "vacuous" => 2,
        "truncated" | "to depth" => 3,
        "held" | "sampled" | "closed" | "conforms" => 4,
        _ => 5,
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum LawReport {
    #[default]
    Unobserved,
    Foreign,
    Observed(Vec<LawRow>),
}

impl LawReport {
    pub(crate) fn rows(&self) -> &[LawRow] {
        match self {
            Self::Observed(rows) => rows,
            _ => &[],
        }
    }

    pub(crate) fn json_rows(&self) -> serde_json::Value {
        serde_json::Value::Array(self.rows().iter().map(|row| serde_json::json!({
            "callee": row.callee, "site": row.site, "class": row.class, "numbers": row.numbers,
            "bound": row.bound, "per_build": row.per_build, "witnessed": row.witnessed,
        })).collect())
    }

    pub(crate) fn merge(&mut self, next: Self) {
        *self = match (std::mem::take(self), next) {
            (Self::Observed(rows), Self::Observed(next)) => {
                let mut all = BTreeMap::new();
                if rows
                    .into_iter()
                    .chain(next)
                    .try_for_each(|row| combine(&mut all, row))
                    .is_some()
                {
                    Self::Observed(all.into_values().collect())
                } else {
                    Self::Unobserved
                }
            }
            (Self::Foreign, Self::Foreign) => Self::Foreign,
            _ => Self::Unobserved,
        };
    }

    pub(crate) fn strength(&self) -> &str {
        match self {
            Self::Unobserved => "unobserved",
            Self::Foreign => "unobserved (FFI-8)",
            Self::Observed(rows) => rows
                .iter()
                .filter(|row| row.callee != "sometimes")
                .min_by(|a, b| a.strength().cmp(&b.strength()))
                .map_or("none", |row| row.class.as_str()),
        }
    }

    pub(crate) fn print(&self) {
        let Self::Observed(rows) = self else { return };
        let mut witnessed = false;
        for row in rows {
            if row.callee == "sometimes" {
                for (label, count) in &row.witnessed {
                    witnessed = true;
                    println!("  sometimes {} {count}", display_label(label));
                }
            } else {
                let site = row.site.rsplit('/').next().unwrap();
                print!("  {}  {}  {}", row.callee, site, row.display());
                for (label, count) in &row.witnessed {
                    witnessed = true;
                    print!("; witnessed {} {count}", display_label(label));
                }
                println!();
            }
        }
        if !witnessed {
            println!("  no reach declared");
        }
    }
}

fn display_label(label: &str) -> std::borrow::Cow<'_, str> {
    if label.is_empty() || label.chars().any(|c| c.is_whitespace() || c.is_control()) {
        format!("{label:?}").into()
    } else {
        label.into()
    }
}

fn unhex(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

pub(super) struct LawCapture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    sites: Sites,
}

impl LawCapture {
    pub(super) fn attach(
        command: &mut Command,
        product: &crate::CheckProduct,
        test: &DefPath,
        limit: usize,
    ) -> Option<Self> {
        let sites = Sites::for_test(product, test, limit).ok()?;
        let directory = tempfile::tempdir().ok()?;
        let path = directory.path().join("laws");
        command.env("HIRSEL_LAWS", &path);
        Some(Self {
            _directory: directory,
            path,
            sites,
        })
    }

    pub(super) fn report(&self) -> LawReport {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|summary| parse(&summary, &self.sites))
            .map_or(LawReport::Unobserved, LawReport::Observed)
    }
}

fn parse(summary: &str, places: &Sites) -> Option<Vec<LawRow>> {
    let body = summary
        .strip_prefix("hirsel-laws-observation 2\n")?
        .strip_suffix("end\n")?;
    let mut rows = BTreeMap::<(String, String), LawRow>::new();
    for line in body.lines() {
        let mut fields = line.split('\t');
        let row = match fields.next()? {
            "witness" => {
                let count = fields.next()?.parse::<u64>().ok()?;
                let label = unhex(fields.next()?)?;
                LawRow {
                    callee: "sometimes".into(),
                    site: label.clone(),
                    class: "witnessed".into(),
                    numbers: vec![count],
                    bound: None,
                    per_build: false,
                    witnessed: BTreeMap::from([(label, count)]),
                    kind: None,
                }
            }
            "row" => prop_row(fields.by_ref(), places)?,
            "model" => model_row(fields.by_ref(), places)?,
            _ => return None,
        };
        if fields.next().is_some() {
            return None;
        }
        combine(&mut rows, row)?;
    }
    Some(rows.into_values().collect())
}

fn combine(rows: &mut BTreeMap<(String, String), LawRow>, mut row: LawRow) -> Option<()> {
    let key = (row.callee.clone(), row.site.clone());
    if let Some(old) = rows.get_mut(&key) {
        if row.strength() < old.strength() {
            std::mem::swap(old, &mut row);
        }
        for (label, count) in row.witnessed {
            let total = old.witnessed.entry(label).or_default();
            *total = total.checked_add(count)?;
        }
        if old.callee == "sometimes" {
            old.numbers[0] = old.witnessed[&old.site];
        }
    } else {
        rows.insert(key, row);
    }
    Some(())
}

fn model_row<'a>(mut fields: impl Iterator<Item = &'a str>, places: &Sites) -> Option<LawRow> {
    let callee = fields.next()?;
    let location = unhex(fields.next()?)?;
    let (definition, ordinal) = places.get(callee)?.get(&location)?;
    let class = fields.next()?;
    let mut numbers = fields
        .next()?
        .split(',')
        .map(str::parse)
        .collect::<Result<Vec<u64>, _>>()
        .ok()?;
    let bound = fields.next()?;
    let bound = (!bound.is_empty()).then(|| bound.to_owned());
    let kind = match (class, numbers.as_slice()) {
        ("closed", [_, _, _]) | ("to depth", [_]) => None,
        ("failed", [_, kind]) if *kind < MODEL_FAILURE_KINDS.len() as u64 => numbers.pop(),
        _ => return None,
    };
    let mut witnessed = BTreeMap::<String, u64>::new();
    for field in fields {
        let (count, label) = field.split_once('=')?;
        let total = witnessed.entry(unhex(label)?).or_default();
        *total = total.checked_add(count.parse().ok()?)?;
    }
    Some(LawRow {
        callee: callee.into(),
        site: format!("{definition}#{ordinal}"),
        class: class.into(),
        numbers,
        bound,
        per_build: kind == Some(3),
        witnessed,
        kind,
    })
}

fn prop_row<'a>(mut fields: impl Iterator<Item = &'a str>, places: &Sites) -> Option<LawRow> {
    let callee = fields.next()?;
    let location = unhex(fields.next()?)?;
    let (definition, ordinal): &(DefPath, u32) = places.get(callee)?.get(&location)?;
    let class = fields.next()?;
    let mut word = || fields.next()?.parse::<u64>().ok();
    let (first, second, failure, cases, _max_choices, max_rejects, _gen_steps) = (
        word()?,
        word()?,
        word()?,
        word()?,
        word()?,
        word()?,
        word()?,
    );
    let (numbers, bound, per_build) = match class {
        "held" => (vec![first, second], None, false),
        "vacuous" if callee == "all" => (vec![first, second], None, false),
        "vacuous" | "sampled" => (vec![first], Some(format!("cases={cases}")), false),
        "truncated" if first + second == cases => {
            (vec![first, second], Some(format!("cases={cases}")), false)
        }
        "truncated" => (
            vec![first, second],
            Some("max_choices/gen_steps".into()),
            true,
        ),
        "gave up" => (
            vec![first, second],
            Some(format!("max_rejects={max_rejects}")),
            false,
        ),
        "unbounded" => (vec![first], None, false),
        "falsified" => {
            let bound = match failure {
                3 => Some("case_steps".into()),
                4 => Some("case_bytes".into()),
                _ => None,
            };
            let per_build = bound.is_some();
            (vec![first], bound, per_build)
        }
        _ => return None,
    };
    Some(LawRow {
        callee: callee.into(),
        site: format!("{definition}#{ordinal}"),
        class: class.into(),
        numbers,
        bound,
        per_build,
        witnessed: BTreeMap::new(),
        kind: (class == "falsified").then_some(failure),
    })
}
