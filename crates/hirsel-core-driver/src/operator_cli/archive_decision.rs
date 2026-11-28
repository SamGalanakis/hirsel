use super::{
    archive::Archive,
    finite::{Envelope, Failure},
    pitr_decision::StorePlan,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
pub(super) struct Expiry {
    pub(super) attempt: String,
    archive: Archive,
    cutoff_kind: String,
    cutoff: String,
    approved: FloorPlan,
}

#[derive(Serialize, Deserialize)]
struct PreviewHold {
    child: String,
    base: String,
    until_ms: String,
    stamp: String,
}

#[derive(Serialize, Deserialize)]
struct FloorPlan {
    #[serde(flatten)]
    boundary: Floor,
    // Decisions saved before Preview holds retain their original plan digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limiting_holds: Option<Vec<PreviewHold>>,
}

#[derive(Serialize, Deserialize, PartialEq)]
struct Floor {
    etag: String,
    previous: String,
    floor: String,
    previous_base: String,
    proposed_base: String,
    deleted: String,
}

pub(super) fn plan(
    archive: Archive,
    kind: &str,
    cutoff: u64,
    env: &mut Envelope<'_>,
) -> Result<(), Failure> {
    let cutoff = cutoff.to_string();
    let raw = archive.query("plan-expiry", &[kind.into(), cutoff.clone()], false)?;
    let approved = serde_json::from_value(raw).map_err(Failure::damaged)?;
    let plan = StorePlan::Expiry(Expiry {
        attempt: super::pitr::attempt()?,
        archive,
        cutoff_kind: kind.into(),
        cutoff,
        approved,
    });
    env.contract = "plan";
    super::pitr_decision::save_plan(&plan)?;
    plan.held(env);
    Ok(())
}
impl Expiry {
    pub(super) fn held(&self, digest: &str, env: &mut Envelope<'_>) {
        let id = format!("store/{}", self.attempt);
        env.target = json!({"kind":"archive","archive":self.archive});
        env.operation = json!({"kind":"store","id":id,"request_id":self.attempt});
        env.result = json!({"plan_digest":digest,"archive":self.archive,"cutoff_kind":self.cutoff_kind,"cutoff":self.cutoff,"plan":self.approved});
        env.outcome = json!({"kind":"AwaitDecision","effect":"none"});
        env.next = vec![json!({"method":"op decide","operation":id})];
    }
    pub(super) fn apply(&self) -> Result<Value, Failure> {
        let args = [
            self.cutoff_kind.clone(),
            self.cutoff.clone(),
            self.approved.boundary.etag.clone(),
            self.approved.boundary.floor.clone(),
            self.approved.boundary.previous.clone(),
        ];
        let raw = self.archive.query("apply-expiry", &args, true)?;
        let mut floor: Floor = serde_json::from_value(raw).map_err(Failure::damaged)?;
        let deleted = std::mem::replace(&mut floor.deleted, "0".into());
        if floor != self.approved.boundary {
            return Err(Failure::damaged(
                "expiry response differs from the approved floor",
            ));
        }
        Ok(
            json!({"archive":self.archive,"floor":floor.floor,"previous":floor.previous,"approved_etag":floor.etag,"deleted":deleted,"attempt":self.attempt}),
        )
    }
}
