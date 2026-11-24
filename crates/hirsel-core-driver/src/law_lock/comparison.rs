use super::{Lock, label_key, site_key};
use crate::test_run::{LawReport, LawRow, class_rank};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

pub(crate) struct Baseline(BTreeMap<String, Expected>);

#[derive(Default)]
struct Expected {
    rows: BTreeMap<String, Row>,
    witnesses: Vec<String>,
}

struct Row {
    class: String,
    kind: Option<String>,
    counts: Option<Vec<u64>>,
}

impl Row {
    fn parse(text: &str) -> Option<Self> {
        let (class, counts) = text.split_once(':')?;
        let (name, kind) = class
            .split_once('/')
            .map_or((class, None), |(name, kind)| (name, Some(kind.to_owned())));
        let class = name.replace('-', " ");
        if class_rank(&class) > 4 {
            return None;
        }
        let counts = if counts == "per-build" {
            None
        } else {
            Some(
                counts
                    .split('/')
                    .map(|n| n.parse().ok())
                    .collect::<Option<Vec<_>>>()?,
            )
        };
        Some(Self {
            class,
            kind,
            counts,
        })
    }

    fn text(&self) -> String {
        let mut out = self.class.clone();
        if let Some(kind) = &self.kind {
            out.push('/');
            out.push_str(kind);
        }
        out.push(' ');
        if let Some(counts) = &self.counts {
            out.push_str(
                &counts
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        } else {
            out.push_str("(per build)");
        }
        out
    }

    fn red(&self, actual: &LawRow) -> Option<&'static str> {
        if self.counts.is_none() {
            return (actual.rank() == 0).then_some("violates");
        }
        if actual.per_build {
            return Some("cannot be compared with");
        }
        if class_rank(&self.class) == 0 {
            let kind = actual.kind.map(|kind| kind.to_string());
            return (actual.class != self.class || kind != self.kind).then_some("differs from");
        }
        let weaker = actual.rank() < class_rank(&self.class);
        let fewer = actual.class == self.class
            && matches!(
                self.class.as_str(),
                "held" | "sampled" | "closed" | "conforms" | "to depth"
            )
            && actual.numbers.first() < self.counts.as_ref().and_then(|numbers| numbers.first());
        (weaker || fewer).then_some("below")
    }
}

impl Baseline {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
            .ok_or_else(|| io::Error::other(format!("{}: invalid law baseline", path.display())))
    }

    fn parse(text: &str) -> Option<Self> {
        let lock = Lock::parse(text)?;
        let mut expected = BTreeMap::new();
        for (test, line) in lock.0 {
            let mut parsed = Expected::default();
            if !matches!(line.rows.as_str(), "-" | "pending") {
                for record in line.rows.split(';') {
                    let (site, fields) = record.split_once(':')?;
                    if site == "sometimes" {
                        parsed.witnesses.push(fields.to_owned());
                    } else if !site.contains('#')
                        || parsed
                            .rows
                            .insert(site.to_owned(), Row::parse(fields)?)
                            .is_some()
                    {
                        return None;
                    }
                }
            }
            expected.insert(test, parsed);
        }
        Some(Self(expected))
    }

    pub(crate) fn compare(&self, observed: &BTreeMap<String, LawReport>) -> Vec<String> {
        let mut reds = Vec::new();
        for (test, expected) in &self.0 {
            let Some(observed) = observed.get(test) else {
                reds.push(format!("{test}: missing test in law comparison"));
                continue;
            };
            let mut rows = BTreeMap::new();
            let mut witnesses = BTreeMap::new();
            for row in observed.rows() {
                if row.callee == "sometimes" {
                    witnesses.extend(
                        row.witnessed
                            .iter()
                            .map(|(label, count)| (label_key(label), *count)),
                    );
                } else {
                    rows.insert(site_key(test, row), row);
                }
            }
            for (site, want) in &expected.rows {
                match rows.get(site.as_str()) {
                    None => reds.push(format!(
                        "{test} {site}: missing row, baseline {}",
                        want.text()
                    )),
                    Some(actual) => {
                        if let Some(reason) = want.red(actual) {
                            let got = Row {
                                class: actual.class.clone(),
                                kind: actual.kind.map(|kind| kind.to_string()),
                                counts: (!actual.per_build).then(|| actual.numbers.clone()),
                            };
                            reds.push(format!(
                                "{test} {site}: {} {reason} {}",
                                got.text(),
                                want.text()
                            ));
                        }
                    }
                }
            }
            for label in &expected.witnesses {
                if witnesses.get(label).copied().unwrap_or(0) == 0 {
                    reds.push(format!(
                        "{test} sometimes:{label}: witnessed 0, baseline witnessed"
                    ));
                }
            }
        }
        reds
    }
}
