//! The operator client's part of the deploy preview (launch.md "Deploy preview"): a
//! person's decision on a request's grant diff, and the preview's JSON.

use super::{Approvals, Error, Exchange, class_name, hex, one, state_json, unexpected};
use crate::launcher::session::Grants;
use crate::operator::{Answer, Ask, EntryChange, Phase, Preview};
use std::path::Path;

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
    let triggers: Vec<_> = preview
        .triggers
        .iter()
        .map(|t| {
            serde_json::json!({
                "class": CLASSES[t.class as usize], "kind": KINDS[t.kind as usize],
                "path": hex(&t.path), "schema": hex(&t.schema), "count": t.count,
                "earliest_due": t.earliest_due,
            })
        })
        .collect();
    serde_json::json!({"grants": grants_json(&preview.grants), "entries": entries, "triggers": triggers})
}
