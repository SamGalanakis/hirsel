use super::{Captured, Id, Rebound, Resolution};
use crate::package_lock::{make_rows, old_rows};
use crate::package_manifest::Selections;
use crate::reachability::authority::{Card, Cards, Verdict, names};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
struct RootVersion {
    version: String,
    root: String,
}

#[derive(Serialize)]
struct Change {
    id: String,
    before: Option<RootVersion>,
    after: RootVersion,
    diff: Diff,
}

#[derive(Default, Serialize)]
struct Diff {
    before: Option<Verdict>,
    after: Option<Verdict>,
    exports: BTreeMap<String, ExportDiff>,
}

#[derive(Default, Serialize)]
struct ExportDiff {
    change: Option<&'static str>,
    reached: Delta,
    takes: Delta,
    returns: Delta,
    provided: Delta,
    contained: Delta<String>,
    unresolved: Option<String>,
}

#[derive(Default, Serialize)]
struct Delta<T = &'static str> {
    added: Vec<T>,
    removed: Vec<T>,
}

impl Delta {
    fn of(before: u16, after: u16) -> Self {
        Self {
            added: names(after & !before),
            removed: names(before & !after),
        }
    }
}

impl Diff {
    fn between(before: Option<&Card>, after: &Card) -> Self {
        let mut diff = Self {
            before: before.map(Card::verdict),
            after: Some(after.verdict()),
            exports: BTreeMap::new(),
        };
        let empty = Card::default();
        let old = &before.unwrap_or(&empty).exports;
        let new = &after.exports;
        for name in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
            let left = old.get(name);
            let right = new.get(name);
            if left == right {
                continue;
            }
            let change = match (left, right) {
                (None, Some(_)) if before.is_some() => Some("added"),
                (Some(_), None) => Some("removed"),
                _ => None,
            };
            let empty = Default::default();
            let left = left.unwrap_or(&empty);
            let right = right.unwrap_or(&empty);
            diff.exports.insert(
                name.to_owned(),
                ExportDiff {
                    change,
                    reached: Delta::of(left.reached.kinds, right.reached.kinds),
                    takes: Delta::of(left.carried.takes, right.carried.takes),
                    returns: Delta::of(left.carried.returns, right.carried.returns),
                    provided: Delta::of(left.reached.provided, right.reached.provided),
                    contained: Delta {
                        added: right
                            .reached
                            .contained
                            .difference(&left.reached.contained)
                            .cloned()
                            .collect(),
                        removed: left
                            .reached
                            .contained
                            .difference(&right.reached.contained)
                            .cloned()
                            .collect(),
                    },
                    unresolved: right.reached.unresolved.clone(),
                },
            );
        }
        diff
    }

    fn print(&self) {
        for (name, delta) in &self.exports {
            let mut parts = Vec::new();
            parts.extend(delta.change.map(str::to_owned));
            for (kind, values) in [
                ("", &delta.reached),
                ("takes", &delta.takes),
                ("returns", &delta.returns),
                ("provides", &delta.provided),
            ] {
                append_delta(&mut parts, kind, values);
            }
            append_delta(&mut parts, "contained", &delta.contained);
            parts.extend(delta.unresolved.clone());
            if !parts.is_empty() {
                println!("  {name}: {}", parts.join(", "));
            }
        }
        if self.before != self.after {
            println!(
                "  verdict: {} -> {}",
                verdict(self.before.as_ref()),
                verdict(self.after.as_ref())
            );
        }
    }
}

fn append_delta<T: std::fmt::Display>(parts: &mut Vec<String>, label: &str, delta: &Delta<T>) {
    let label = if label.is_empty() {
        String::new()
    } else {
        format!("{label} ")
    };
    parts.extend(delta.added.iter().map(|item| format!("{label}+ {item}")));
    parts.extend(delta.removed.iter().map(|item| format!("{label}- {item}")));
}

fn verdict(value: Option<&Verdict>) -> String {
    match value {
        None => "unknown".into(),
        Some(Verdict::Status(status)) => (*status).into(),
        Some(Verdict::Kinds(kinds)) => kinds.join(", "),
    }
}

pub(super) struct Report {
    changes: BTreeMap<Id, Change>,
    written: BTreeMap<String, String>,
    removed: Vec<String>,
    rebound: Vec<Rebound>,
    tested: Vec<String>,
    fetched: BTreeSet<String>,
}

impl Report {
    pub(super) fn new(
        trial: &Resolution,
        before: &Selections,
        before_lock: Option<&Captured>,
        baseline: Option<&Resolution>,
        rebound: Vec<Rebound>,
        fetched: BTreeSet<String>,
    ) -> Self {
        let old = before_lock
            .and_then(|lock| old_rows(&lock.bytes))
            .or_else(|| baseline.map(make_rows))
            .unwrap_or_default();
        let next = make_rows(trial);
        let changes = trial
            .published
            .keys()
            .filter_map(|id| {
                let new = &next[id];
                let root = new.root?;
                let prior = old.get(id);
                if prior.is_some_and(|row| row.root == Some(root)) {
                    return None;
                }
                Some((
                    id.clone(),
                    Change {
                        id: id.to_string(),
                        before: prior.and_then(|row| {
                            Some(RootVersion {
                                version: row.version.clone()?,
                                root: row.root?.to_string(),
                            })
                        }),
                        after: RootVersion {
                            version: new.version.clone()?,
                            root: root.to_string(),
                        },
                        diff: Diff::default(),
                    },
                ))
            })
            .collect();
        let after = &trial.workspace.selections;
        let written = after
            .iter()
            .filter(|(id, value)| before.get(*id) != Some(*value))
            .map(|(id, (root, _))| (id.to_string(), root.to_string()))
            .collect();
        let removed = before
            .keys()
            .filter(|id| !after.contains_key(*id))
            .map(ToString::to_string)
            .collect();
        let mut tested = Vec::new();
        for (id, root) in &trial.published {
            let Some(library) = trial.libraries.get(root) else {
                continue;
            };
            for (child, edge) in library.content.bindings.values() {
                let Some(recorded) = edge.root() else {
                    continue;
                };
                if trial.published.get(child) == Some(&recorded) {
                    continue;
                }
                let version = match edge {
                    crate::package_content::Binding::Floor { minor, patch, .. } => {
                        Some(format!("{}.{minor}.{patch}", child.major))
                    }
                    _ => trial
                        .libraries
                        .get(&recorded)
                        .map(|lib| lib.content.version.to_string()),
                };
                let before = version.unwrap_or_else(|| recorded.to_string());
                let after = next
                    .get(child)
                    .and_then(|row| row.version.as_deref())
                    .unwrap_or("unknown");
                tested.push(format!(
                    "{id}: published against {child} {before}, built with {after}"
                ));
            }
        }
        tested.sort();
        tested.dedup();
        Self {
            changes,
            written,
            removed,
            rebound,
            tested,
            fetched,
        }
    }

    pub(super) fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    pub(super) fn matches_previous(&self, resolved: &Resolution) -> bool {
        self.changes.iter().all(|(id, change)| {
            change.before.as_ref().is_none_or(|before| {
                resolved
                    .published
                    .get(id)
                    .is_some_and(|root| root.to_string() == before.root)
            })
        })
    }

    pub(super) fn set_diffs(&mut self, before: &Cards, after: &Cards) {
        for (id, change) in &mut self.changes {
            let key = id.compiler_id();
            if let Some(card) = after.get(&key) {
                change.diff = Diff::between(before.get(&key), card);
            }
        }
    }

    pub(super) fn print(&self, json: bool, dry_run: bool, result: &str) {
        if json {
            println!(
                "{}",
                serde_json::json!({"result":result, "dry_run":dry_run,
                "rebound":self.rebound, "selections":{"written":self.written, "removed":self.removed},
                "changed": self.changes.values().collect::<Vec<_>>(),
                "tested": self.tested, "fetched":self.fetched})
            );
            return;
        }
        for change in self.changes.values() {
            let old = change.before.as_ref().map_or_else(
                || "unknown".into(),
                |old| format!("{} {}", old.version, old.root),
            );
            println!(
                "{}: {old} -> {} {}",
                change.id, change.after.version, change.after.root
            );
            change.diff.print();
        }
        for (id, root) in &self.written {
            println!("selected {id} {root}");
        }
        for id in &self.removed {
            println!("removed selection {id}");
        }
        for row in &self.rebound {
            println!("{}: {} -> {}", row.row, row.old, row.new);
        }
        for row in &self.tested {
            println!("{row}");
        }
        for source in &self.fetched {
            println!("hirsel: fetched {source}");
        }
        if result == "unchanged" {
            println!("hirsel: dependencies unchanged");
        }
    }
}
