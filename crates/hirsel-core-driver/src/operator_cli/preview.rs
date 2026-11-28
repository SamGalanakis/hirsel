//! Semantic facts projected from the operator protocol.
use crate::launcher::session::Grants;
use crate::operator::{Decision, DecisionStage, EntryChange, HeldBy, HeldFate, Migration, Preview};
use hirsel_core_ids::hex;

/// The decision a held request awaits, as `hirsel op decide` and State print it.
pub(super) fn decision_json(decision: &Decision) -> serde_json::Value {
    let stage = match decision.stage {
        DecisionStage::Retirement => "retirement",
        DecisionStage::Creation => "creation",
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
        "gate_toolchain": decision.gate_toolchain.as_deref().map(hex::encode),
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
            serde_json::json!({"identity": hex::encode(&e.identity), "change": change, "previous_revision":e.previous_revision.as_deref().map(hex::encode),"revision":e.revision.as_deref().map(hex::encode)})
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
