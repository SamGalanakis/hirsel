//! The operator client's part of the deploy preview (launch.md "Deploy preview"): the
//! operator's decision on a held request (`hirsel decide`), and the preview's JSON.

use super::{
    Approvals, Error, Exchange, class_name, one, or_unavailable, sort_grants, state_json,
    unexpected,
};
use crate::cli::{Invocation, OptionId as Opt};
use crate::launcher::session::Grants;
use crate::operator::{
    Answer, Ask, Decision, DecisionStage, EntryChange, Fates, HeldBy, HeldFate, Migration, Phase,
    Preview, Verdict,
};
use hirsel_core_ids::hex;
use std::path::PathBuf;

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

/// `hirsel decide --state S --request-id HEX [--approve-grant +LINE|-LINE ...]
/// [--held release|cancel] [--held-entry PATH=release|cancel ...] [--refuse]`: the
/// operator's decision on a request that awaits one (launch.md "Self-editing" "Holds and
/// decisions"). The client reads the decision the host shows; an approval must name each
/// line of its grant diff, and give fates exactly when it shows a held group or an unmatched
/// path. The Decide carries the shown decision back, so a host that moved on refuses it; an
/// approval follows the repeated admission to its outcome.
pub(crate) fn decide(invocation: Invocation) -> Result<i32, Error> {
    or_unavailable("decide", decide_on(invocation))
}

fn decide_on(invocation: Invocation) -> Result<i32, Error> {
    let (mut state, mut d) = (None, None);
    let mut approved = Grants::default();
    let mut refuse = false;
    let mut fates = Fates::default();
    let mut fated = false;
    for (option, value) in invocation.options {
        let text = value.unwrap_or_default();
        match option {
            Opt::HostState => state = Some(PathBuf::from(text)),
            Opt::RequestId => d = Some(text),
            Opt::ApproveGrant => approve_grant(&mut approved, &text)?,
            Opt::Refuse => refuse = true,
            Opt::HeldAll | Opt::HeldEntry => {
                held_fate(&mut fates, option == Opt::HeldAll, &text)?;
                fated = true;
            }
            _ => return Err("unexpected decide option".into()),
        }
    }
    if invocation.positional.is_some() || !invocation.arguments.is_empty() {
        return Err("decide takes no recipe and no program arguments".into());
    }
    let state = state.ok_or("decide requires --state DIR")?;
    let d = hex::decode(&d.ok_or("decide requires --request-id HEX")?)
        .filter(|d| d.len() == 16)
        .ok_or("--request-id takes 32 hex digits")?;
    sort_grants(&mut approved);
    if refuse && (fated || approved != Grants::default()) {
        return Err("--refuse takes no --approve-grant, --held or --held-entry".into());
    }
    let held = match one(&state, &Ask::Status(d.clone()))? {
        Some(Answer::State(_, _, _, Phase::AwaitDecision(held))) => held,
        Some(Answer::State(d, group, _, phase)) => {
            eprintln!("decide: the request awaits no decision");
            println!("{}", state_json(&d, &group, &phase));
            return Ok(1);
        }
        Some(Answer::Error(class, detail)) => {
            eprintln!("decide: {}: {detail}", class_name(class));
            return Ok(1);
        }
        other => return Ok(unexpected("decide", &d, other)),
    };
    let none = Approvals {
        foreign: Vec::new(),
        exec: Vec::new(),
    };
    if refuse {
        let ask = Ask::Decide(d.clone(), held, Verdict::Refuse);
        return Exchange::new(&state, d, "decide", None, none).run(ask);
    }
    let refusal = if approved != held.grants {
        Some("the --approve-grant lines are not the grant diff the request awaits")
    } else if held.shows_held() && !fated {
        Some("the decision shows held runs; give their fates with --held or --held-entry")
    } else if !held.shows_held() && fated {
        Some("the decision shows no held run, so it takes no --held or --held-entry")
    } else {
        None
    };
    if let Some(why) = refusal {
        eprintln!("decide: refused: {why}");
        println!("{}", decision_json(&held));
        return Ok(2);
    }
    let ask = Ask::Decide(d.clone(), held, Verdict::Approve(fates));
    Exchange::new(&state, d, "decide", None, none).run(ask)
}

/// The decision a held request awaits, as `hirsel decide` and State print it.
pub(super) fn decision_json(decision: &Decision) -> serde_json::Value {
    let stage = match decision.stage {
        DecisionStage::Admission => "admission",
        DecisionStage::Prepared => "prepared",
    };
    let migration = decision.migration.as_ref().map(migration_json);
    let approvals = decision.approvals.as_ref().map(|a| {
        serde_json::json!({"recipe": hex::encode(&a.r), "foreign": a.foreign, "exec": a.exec, "text": a.text})
    });
    let triggers = preview_json(&Preview {
        triggers: decision.triggers.clone(),
        ..Preview::default()
    })["triggers"]
        .clone();
    serde_json::json!({
        "tree": decision.tree.as_deref().map(hex::encode),
        "root": decision.root.as_deref().map(hex::encode),
        "stage": stage,
        "grants": grants_json(&decision.grants),
        "approvals": approvals,
        "migration": migration,
        "triggers": triggers,
    })
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
            serde_json::json!({"identity": hex::encode(&e.identity), "change": change})
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
                "path": entry_path(&t.path), "schema": hex::encode(&t.schema), "hold": hold,
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
        "migration": preview.migration.as_ref().map(migration_json),
    })
}

fn migration_json(m: &Migration) -> serde_json::Value {
    serde_json::json!({"digest": hex::encode(&m.digest), "changes": m.changes})
}

/// An entry path as `--held-entry` takes it: its text, or hex when it is not UTF-8.
fn entry_path(path: &[u8]) -> String {
    std::str::from_utf8(path).map_or_else(|_| hex::encode(path), str::to_owned)
}
