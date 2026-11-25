//! The report of a profile (`docs/coverage.md`, "Report"): the fields it
//! derives from the profile, the text, the diff against a base and the
//! targets that narrow the uncovered list.
use super::{Profile, Site};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Rows each section of the text report lists.
const TEXT_ROWS: usize = 20;

/// The report of a profile.
pub(crate) struct Report {
    /// The profile document with the fields derived from it: what
    /// `--coverage-out` and `hirsel coverage -o` write, the `--json`
    /// document's top-level `coverage`, and what `hirsel coverage --json`
    /// prints.
    pub document: Value,
    pub text: String,
}

/// `--coverage-targets` ranges: one `{"file", "begin_line", "end_line"}`
/// object on each line, inclusive and 1-based; other fields are ignored.
#[derive(Clone, Debug, Default)]
pub(crate) struct Targets(Vec<(String, u64, u64)>);

impl Targets {
    pub(crate) fn read(text: &str) -> Result<Self, String> {
        let mut targets = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let bad = |why: &str| format!("target line {}: {why}", index + 1);
            let row: Value = serde_json::from_str(line).map_err(|e| bad(&e.to_string()))?;
            let file = row["file"].as_str().ok_or_else(|| bad("no file"))?;
            let [Some(begin), Some(end)] =
                [&row["begin_line"], &row["end_line"]].map(Value::as_u64)
            else {
                return Err(bad("no begin_line and end_line"));
            };
            targets.push((file.to_owned(), begin, end));
        }
        Ok(Self(targets))
    }

    /// Whether `file`'s lines `first..=last` overlap a target. A target that
    /// names no directory matches by the file's final path component.
    fn hits(&self, file: &str, first: u64, last: u64) -> bool {
        self.0.iter().any(|(target, begin, end)| {
            let name = if target.contains('/') {
                file
            } else {
                file.rsplit('/').next().unwrap_or(file)
            };
            name == target && first <= *end && *begin <= last
        })
    }

    fn site(&self, site: &Site) -> bool {
        let place = &site.place;
        match (&place.file, place.line) {
            (Some(file), Some(line)) => self.hits(file, line, place.end_line.unwrap_or(line)),
            _ => false,
        }
    }
}

/// The diff of two profiles by site key ("Diff").
pub(crate) struct Diff {
    pub base: String,
    pub selection_changed: bool,
    pub before: usize,
    pub after: usize,
    pub gained: Vec<String>,
    pub lost: Vec<String>,
    pub changed_functions: Vec<String>,
}

impl Diff {
    /// `new` against `base`. A function whose `src` differs between them is
    /// changed, and its sites are left out of every other field.
    pub(crate) fn of(base: &Profile, new: &Profile) -> Self {
        let changed_functions: Vec<String> = new
            .functions
            .iter()
            .filter(|(f, function)| {
                base.functions
                    .get(*f)
                    .is_some_and(|b| b.src != function.src)
            })
            .map(|(f, _)| f.clone())
            .collect();
        let reached = |profile: &Profile| -> BTreeSet<String> {
            profile
                .sites
                .iter()
                .filter(|(_, s)| s.reached() > 0 && !changed_functions.contains(&s.function))
                .map(|(key, _)| key.clone())
                .collect()
        };
        let (before, after) = (reached(base), reached(new));
        Self {
            base: base.digest(),
            selection_changed: base.digest() != new.digest(),
            before: before.len(),
            after: after.len(),
            gained: after.difference(&before).cloned().collect(),
            lost: before.difference(&after).cloned().collect(),
            changed_functions,
        }
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "base": self.base, "selection_changed": self.selection_changed,
            "reached": {"before": self.before, "after": self.after},
            "gained": self.gained, "lost": self.lost, "changed_functions": self.changed_functions,
        })
    }

    /// `since base  +G sites, -L   N functions changed`, and `selection
    /// differs` when the selections differ.
    pub(crate) fn line(&self) -> String {
        let mut line = format!(
            "since base  +{} sites, -{}   {} functions changed",
            self.gained.len(),
            self.lost.len(),
            self.changed_functions.len()
        );
        if self.selection_changed {
            line.push_str("   selection differs");
        }
        line
    }
}

impl Profile {
    /// The report: the document with its derived fields, and its text.
    /// `targets` narrows and marks the uncovered list; `base` adds a diff.
    pub(crate) fn report(&self, targets: Option<&Targets>, base: Option<&Profile>) -> Report {
        let entered = |function: &str| {
            self.sites
                .get(&format!("{function}#e"))
                .map_or(0, Site::reached)
        };
        let count = |entry: bool, filter: &dyn Fn(&Site) -> bool| {
            let (mut reached, mut total) = (0u64, 0u64);
            for site in self
                .sites
                .values()
                .filter(|s| s.entry() == entry && filter(s))
            {
                total += 1;
                reached += u64::from(site.reached() > 0);
            }
            json!({"reached": reached, "total": total})
        };
        // By function: its arms reached and in all, and whether a target
        // covers one of its sites.
        let mut arms = BTreeMap::<&str, (u64, u64)>::new();
        let mut targeted = BTreeSet::<&str>::new();
        for site in self.sites.values() {
            if targets.is_some_and(|t| t.site(site)) {
                targeted.insert(&site.function);
            }
            if !site.entry() {
                let held = arms.entry(&site.function).or_default();
                held.0 += u64::from(site.reached() > 0);
                held.1 += 1;
            }
        }
        let files: BTreeSet<&str> = self
            .sites
            .values()
            .filter_map(|s| s.place.file.as_deref())
            .collect();
        let files: Vec<Value> = files
            .iter()
            .map(|name| {
                let here = |s: &Site| s.place.file.as_deref() == Some(*name);
                json!({"file": name, "functions": count(true, &here), "arms": count(false, &here)})
            })
            .collect();
        let mut document = self.json();
        for row in document["functions"].as_array_mut().unwrap() {
            let definition = row["definition"].as_str().unwrap().to_owned();
            row["entered_runs"] = entered(&definition).into();
            let (reached, total) = arms.get(definition.as_str()).copied().unwrap_or_default();
            row["arms"] = json!({"reached": reached, "total": total});
        }
        let mut uncovered = Vec::new();
        for (key, site) in self.sites.iter().filter(|(_, s)| s.reached() == 0) {
            let entered_runs = entered(&site.function);
            let mut row = site.row(key);
            let target = if site.entry() {
                row["state"] = "never entered".into();
                row["arms"] = arms.get(site.function.as_str()).map_or(0, |a| a.1).into();
                targeted.contains(site.function.as_str())
            } else if entered_runs == 0 {
                continue;
            } else {
                // The arms of one decision are the keys up to their last `:`.
                let decision = &key[..=key.rfind(':').unwrap()];
                let taken = self
                    .sites
                    .range::<str, _>((
                        std::ops::Bound::Included(decision),
                        std::ops::Bound::Unbounded,
                    ))
                    .take_while(|(k, _)| k.starts_with(decision))
                    .any(|(_, s)| s.reached() > 0);
                row["state"] = if taken {
                    "other arm taken"
                } else {
                    "decision not reached"
                }
                .into();
                row["entered_runs"] = entered_runs.into();
                targets.is_some_and(|t| t.site(site))
            };
            if targets.is_some() {
                if !target {
                    continue;
                }
                row["target"] = true.into();
            }
            uncovered.push((entered_runs, key, row));
        }
        uncovered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let totals = json!({
            "functions": count(true, &|_| true),
            "arms": count(false, &|_| true),
            "unreferenced": self.unreferenced.len(),
        });
        let diff = base.map(|base| Diff::of(base, self));
        let text = self.text(&document, &totals, &files, &uncovered, diff.as_ref());
        document["totals"] = totals;
        document["files"] = files.into();
        document["uncovered"] = uncovered.into_iter().map(|(_, _, row)| row).collect();
        if let Some(diff) = &diff {
            document["diff"] = diff.json();
        }
        Report { document, text }
    }

    fn text(
        &self,
        document: &Value,
        totals: &Value,
        files: &[Value],
        uncovered: &[(u64, &String, Value)],
        diff: Option<&Diff>,
    ) -> String {
        let fraction = |v: &Value| format!("{}/{}", v["reached"], v["total"]);
        let mut text = String::new();
        let builds: Vec<String> = self
            .builds()
            .iter()
            .map(|b| {
                format!(
                    "{} {}",
                    b.engine,
                    if b.optimize { "release" } else { "dev" }
                )
            })
            .collect();
        let scope: Vec<&str> = self.scope.iter().map(String::as_str).collect();
        let selection = &document["selection"];
        writeln!(
            text,
            "coverage  {}  {}  {} tests, {} runs",
            scope.join(" "),
            builds.join(", "),
            selection["tests"],
            selection["runs"]
        )
        .unwrap();
        writeln!(
            text,
            "  functions {}   arms {}   {} unreferenced",
            fraction(&totals["functions"]),
            fraction(&totals["arms"]),
            totals["unreferenced"]
        )
        .unwrap();
        if self.unobserved_runs > 0 {
            writeln!(text, "  {} runs unobserved", self.unobserved_runs).unwrap();
        }
        for file in files {
            writeln!(
                text,
                "  {:<24} functions {}   arms {}",
                file["file"].as_str().unwrap(),
                fraction(&file["functions"]),
                fraction(&file["arms"])
            )
            .unwrap();
        }
        let (never, arms): (Vec<_>, Vec<_>) = uncovered
            .iter()
            .partition(|(_, _, row)| row["kind"] == "entry");
        let at = |row: &Value, col: bool| {
            let mut at = format!("{}:{}", row["file"].as_str().unwrap_or("?"), row["line"]);
            if col {
                write!(at, ":{}", row["col"]).unwrap();
            }
            at
        };
        if !never.is_empty() {
            writeln!(
                text,
                "never entered ({} of {}; --json lists all)",
                never.len(),
                totals["functions"]["total"]
            )
            .unwrap();
            for (_, _, row) in never.iter().take(TEXT_ROWS) {
                writeln!(
                    text,
                    "  {:<36} {}",
                    row["definition"].as_str().unwrap(),
                    at(row, false)
                )
                .unwrap();
            }
            more(&mut text, never.len());
        }
        if !arms.is_empty() {
            writeln!(text, "entered, arm not reached (most-entered first)").unwrap();
            for (entered, _, row) in arms.iter().take(TEXT_ROWS) {
                writeln!(
                    text,
                    "  {}   {}   {}   {}   entered by {entered} runs",
                    row["definition"].as_str().unwrap(),
                    at(row, true),
                    row["arm"].as_str().unwrap(),
                    row["state"].as_str().unwrap()
                )
                .unwrap();
            }
            more(&mut text, arms.len());
        }
        if let Some(diff) = diff {
            writeln!(text, "{}", diff.line()).unwrap();
        }
        let only = self.only();
        for (test, reached) in self.reached_by_test() {
            let (functions, arms) = reached.iter().fold((0, 0), |(f, a), k| {
                if self.sites[*k].entry() {
                    (f + 1, a)
                } else {
                    (f, a + 1)
                }
            });
            writeln!(
                text,
                "  {test}  coverage {functions} fn, {arms} arms, {} only",
                only[test]
            )
            .unwrap();
        }
        text
    }
}

fn more(text: &mut String, rows: usize) {
    if rows > TEXT_ROWS {
        writeln!(text, "  ... {} more", rows - TEXT_ROWS).unwrap();
    }
}
