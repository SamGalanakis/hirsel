//! The operator client's part of the deploy preview (launch.md "Deploy preview"): a
//! person's decision on a request's grant diff and held runs, and the preview's JSON.

use super::{Approvals, Error, Exchange, class_name, hex, one, state_json, unexpected};
use crate::launcher::session::Grants;
use crate::operator::{Answer, Ask, EntryChange, Fates, HeldBy, HeldFate, Phase, Preview};
use std::path::Path;

/// One `--held` or `--held-entry` value into the deploy's fates (launch.md "The operator
/// socket"): entries stay sorted by path, and a path never holds '=', so the fate follows
/// the last one.
pub(super) fn held_fate(held: &mut Fates, all: bool, value: &str) -> Result<(), Error> {
    let (name, path, fate) = if all {
        ("--held", None, value)
    } else {
        let (path, fate) = value
            .rsplit_once('=')
            .filter(|(path, _)| !path.is_empty())
            .ok_or("--held-entry takes PATH=release|cancel")?;
        ("--held-entry", Some(path), fate)
    };
    let fate = match fate {
        "release" => HeldFate::Release,
        "cancel" => HeldFate::Cancel,
        _ => return Err(format!("{name} takes release or cancel").into()),
    };
    match path {
        None if held.all.is_some() => Err("--held is given twice".into()),
        None => {
            held.all = Some(fate);
            Ok(())
        }
        Some(path) => match held
            .entries
            .binary_search_by(|(p, _)| p.as_slice().cmp(path.as_bytes()))
        {
            Ok(_) => Err(format!("--held-entry names {path} twice").into()),
            Err(at) => {
                held.entries.insert(at, (path.as_bytes().to_vec(), fate));
                Ok(())
            }
        },
    }
}

/// One `--approve-grant` line: `+LINE` for an authority the release gains, `-LINE` for one
/// it loses.
pub(super) fn approve_grant(diff: &mut Grants, value: &str) -> Result<(), Error> {
    match value.split_at_checked(1) {
        Some(("+", line)) if !line.is_empty() => diff.added.push(line.into()),
        Some(("-", line)) if !line.is_empty() => diff.removed.push(line.into()),
        _ => return Err("--approve-grant takes +LINE or -LINE".into()),
    }
    Ok(())
}

/// A person's decision on request d, which awaits one (launch.md "Deploy preview"): a
/// refusal names the diff the host shows; an approval names its lines and follows the
/// repeated admission to its outcome.
pub(super) fn decide(
    state: &Path,
    d: Vec<u8>,
    decision: Grants,
    refuse: bool,
) -> Result<i32, Error> {
    let none = Approvals {
        foreign: Vec::new(),
        exec: Vec::new(),
    };
    if refuse {
        let diff = match one(state, &Ask::Status(d.clone()))? {
            Some(Answer::State(_, _, _, Phase::AwaitDecision(diff))) => diff,
            Some(Answer::State(d, group, _, phase)) => {
                eprintln!("deploy: the request awaits no decision");
                println!("{}", state_json(&d, &group, &phase));
                return Ok(1);
            }
            Some(Answer::Error(class, detail)) => {
                eprintln!("deploy: {}: {detail}", class_name(class));
                return Ok(1);
            }
            other => return Ok(unexpected("deploy", &d, other)),
        };
        let ask = Ask::Decide(d.clone(), diff, false);
        return Exchange::new(state, d, "deploy", None, none).run(ask);
    }
    let ask = Ask::Decide(d.clone(), decision.clone(), true);
    let mut exchange = Exchange::new(state, d, "deploy", None, none);
    exchange.decision = Some(decision);
    exchange.run(ask)
}

pub(super) fn grants_json(grants: &Grants) -> serde_json::Value {
    serde_json::json!({"added": grants.added, "removed": grants.removed})
}

pub(super) fn preview_json(preview: &Preview) -> serde_json::Value {
    const CLASSES: [&str; 5] = ["cancelling", "held", "gone", "changed", "unchanged"];
    const KINDS: [&str; 4] = ["compute", "transact", "query", "granted"];
    let entries: Vec<_> = preview
        .entries
        .iter()
        .map(|e| {
            let change = match e.change {
                EntryChange::Added => "added",
                EntryChange::Removed => "removed",
                EntryChange::Changed => "changed",
                EntryChange::Unchanged => "unchanged",
            };
            serde_json::json!({"identity": hex(&e.identity), "change": change})
        })
        .collect();
    let rows: Vec<_> = preview
        .triggers
        .rows
        .iter()
        .map(|t| {
            let hold = t.hold.map(|hold| match hold {
                HeldBy::Cut => serde_json::json!({"by": "cut"}),
                HeldBy::Revision(same) => serde_json::json!({"by": "revision", "same_code": same}),
                HeldBy::Request(same) => serde_json::json!({"by": "request", "same_code": same}),
            });
            serde_json::json!({
                "class": CLASSES[t.class as usize], "kind": KINDS[t.kind as usize],
                "path": entry_path(&t.path), "schema": hex(&t.schema), "hold": hold,
                "fate": t.fate.map(HeldFate::name), "count": t.count,
                "earliest_due": t.earliest_due,
            })
        })
        .collect();
    let unmatched: Vec<_> = preview
        .triggers
        .unmatched
        .iter()
        .map(|p| entry_path(p))
        .collect();
    serde_json::json!({
        "grants": grants_json(&preview.grants), "entries": entries,
        "triggers": {"rows": rows, "unmatched": unmatched},
    })
}

/// An entry path as `--held-entry` takes it: its text, or hex when it is not UTF-8.
fn entry_path(path: &[u8]) -> String {
    std::str::from_utf8(path).map_or_else(|_| hex(path), str::to_owned)
}
