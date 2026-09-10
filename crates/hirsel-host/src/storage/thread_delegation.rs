//! Atomic child acceptance and upward reports. The request table is the outbox.
use super::{Storage, ThreadCaller, thread_activity, thread_scope};
use hirsel_proto::ThreadTurn;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Delegation {
    pub title: String,
    pub brief: String,
    pub artifact_ids: Vec<u64>,
    pub child_thread_id: Option<u64>,
    pub execution: Option<super::ThreadExecution>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DelegatedTurn {
    pub thread_id: u64,
    pub turn_id: u64,
}

pub(super) fn enqueue(
    c: &Connection,
    thread_id: u64,
    client_id: &str,
    body: &str,
    requester_thread_id: Option<u64>,
    requester_turn_id: Option<u64>,
    report_triggered: bool,
) -> anyhow::Result<u64> {
    c.execute("INSERT INTO thread_turns(thread_id,requester_thread_id,requester_turn_id,state,started_at) VALUES(?1,?2,?3,'queued',?4)",params![thread_id,requester_thread_id,requester_turn_id,chrono::Utc::now().to_rfc3339()])?;
    let turn_id = c.last_insert_rowid() as u64;
    super::thread_execution::capture(c, thread_id, turn_id, None)?;
    let history_id: String =
        c.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?;
    let payload = json!({"history_id":history_id,"turn_id":turn_id,"thread_id":thread_id,"thread_action":null,"message_id":null,"client_id":client_id,"body":body,"anchor":null,"attachments":[],"mode":"send","report_triggered":report_triggered});
    c.execute(
        "INSERT INTO thread_requests(client_id,payload) VALUES(?1,?2)",
        params![client_id, serde_json::to_string(&payload)?],
    )?;
    Ok(turn_id)
}
fn link_artifacts(c: &Connection, activity_id: u64, ids: &[u64]) -> anyhow::Result<()> {
    for id in ids {
        c.execute(
            "INSERT OR IGNORE INTO activity_artifacts(activity_id,artifact_id) VALUES(?1,?2)",
            params![activity_id, id],
        )?;
    }
    Ok(())
}
impl Storage {
    pub(crate) async fn delegate_thread(
        &self,
        caller: &ThreadCaller,
        operation_id: &str,
        assignment: &Delegation,
        invocation: &serde_json::Value,
    ) -> anyhow::Result<DelegatedTurn> {
        anyhow::ensure!(
            !assignment.title.trim().is_empty() && !assignment.brief.trim().is_empty(),
            "delegation requires title and brief"
        );
        anyhow::ensure!(
            assignment.artifact_ids.len() <= 100,
            "too many artifact references"
        );
        anyhow::ensure!(
            !operation_id.is_empty(),
            "delegation requires execution operation identity"
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_caller(&tx, caller)?;
        for id in &assignment.artifact_ids {
            thread_scope::authorize_artifact(&tx, caller.thread_id, *id)?;
        }
        let payload = serde_json::to_string(invocation)?;
        if let Some((old,thread_id,turn_id))=tx.query_row("SELECT payload,child_thread_id,child_turn_id FROM thread_delegations WHERE requester_turn_id=?1 AND operation_id=?2",params![caller.turn_id,operation_id],|r|Ok((r.get::<_,String>(0)?,r.get(1)?,r.get(2)?))).optional()? {
            anyhow::ensure!(old==payload,"delegation operation payload changed");
            return Ok(DelegatedTurn{thread_id,turn_id});
        }
        let child = if let Some(id) = assignment.child_thread_id {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1 AND parent_thread_id=?2)",
                params![id, caller.thread_id],
                |r| r.get(0),
            )?;
            anyhow::ensure!(valid, "dispatch requires a direct child Thread");
            id
        } else {
            let now = chrono::Utc::now().to_rfc3339();
            tx.execute("INSERT INTO threads(parent_thread_id,title,description,instrument,attention,read,created_at,updated_at,revision) VALUES(?1,?2,'','{}','quiet',0,?3,?3,1)",params![caller.thread_id,assignment.title.trim(),now])?;
            tx.last_insert_rowid() as u64
        };
        if let Some(execution) = &assignment.execution {
            tx.execute("INSERT INTO thread_execution_preferences(thread_id,config) VALUES(?1,?2) ON CONFLICT(thread_id) DO UPDATE SET config=excluded.config",params![child,serde_json::to_string(execution)?])?;
        }
        let request_id = format!("delegation:{}:{operation_id}", caller.turn_id);
        let turn_id = enqueue(
            &tx,
            child,
            &request_id,
            &assignment.brief,
            Some(caller.thread_id),
            Some(caller.turn_id),
            false,
        )?;
        let data = json!({"requester_thread_id":caller.thread_id,"requester_turn_id":caller.turn_id,"brief":assignment.brief});
        tx.execute("INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,'delegation_received',?3,?4)",params![child,turn_id,serde_json::to_string(&data)?,chrono::Utc::now().to_rfc3339()])?;
        link_artifacts(&tx, tx.last_insert_rowid() as u64, &assignment.artifact_ids)?;
        tx.execute("INSERT INTO thread_delegations(requester_turn_id,operation_id,payload,child_thread_id,child_turn_id) VALUES(?1,?2,?3,?4,?5)",params![caller.turn_id,operation_id,payload,child,turn_id])?;
        tx.commit()?;
        Ok(DelegatedTurn {
            thread_id: child,
            turn_id,
        })
    }
    pub(crate) async fn report_thread_progress(
        &self,
        caller: &ThreadCaller,
        operation_id: &str,
        summary: &str,
        artifact_ids: &[u64],
    ) -> anyhow::Result<u64> {
        anyhow::ensure!(
            !summary.trim().is_empty(),
            "report summary must not be empty"
        );
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_caller(&tx, caller)?;
        for id in artifact_ids {
            thread_scope::authorize_artifact(&tx, caller.thread_id, *id)?;
        }
        let turn = thread_activity::get(&tx, caller.turn_id)?;
        anyhow::ensure!(
            turn.requester_thread_id.is_some(),
            "this turn has no requesting parent"
        );
        let id = report(&tx, &turn, operation_id, "progress", summary, artifact_ids)?;
        tx.commit()?;
        Ok(id)
    }
}

/// Caller holds the completion transaction. The receipt survives outbox consumption.
pub(super) fn report(
    c: &Connection,
    turn: &ThreadTurn,
    operation_id: &str,
    status: &str,
    summary: &str,
    artifact_ids: &[u64],
) -> anyhow::Result<u64> {
    let parent = turn
        .requester_thread_id
        .ok_or_else(|| anyhow::anyhow!("turn has no requesting parent"))?;
    let payload = serde_json::to_string(
        &json!({"status":status,"summary":summary,"artifact_ids":artifact_ids}),
    )?;
    if let Some((old,id))=c.query_row("SELECT payload,activity_id FROM thread_reports WHERE child_turn_id=?1 AND operation_id=?2",params![turn.id,operation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?))).optional()? {
        anyhow::ensure!(old==payload,"report operation payload changed");return Ok(id);
    }
    // Validate source authority before the destination reference can grant it.
    for id in artifact_ids {
        thread_scope::authorize_artifact(c, turn.thread_id, *id)?;
    }
    let seq: u64 = c.query_row(
        "SELECT COALESCE(MAX(report_seq),0)+1 FROM thread_reports WHERE child_turn_id=?1",
        [turn.id],
        |r| r.get(0),
    )?;
    let data = json!({"child_thread_id":turn.thread_id,"child_turn_id":turn.id,"requester_turn_id":turn.requester_turn_id,"report_seq":seq,"status":status,"summary":summary});
    c.execute("INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,NULL,'child_report',?2,?3)",params![parent,serde_json::to_string(&data)?,chrono::Utc::now().to_rfc3339()])?;
    let activity_id = c.last_insert_rowid() as u64;
    link_artifacts(c, activity_id, artifact_ids)?;
    c.execute("INSERT INTO thread_reports(child_turn_id,operation_id,report_seq,payload,activity_id) VALUES(?1,?2,?3,?4,?5)",params![turn.id,operation_id,seq,payload,activity_id])?;
    let request_id = format!("child-report:{}:{seq}", turn.id);
    // The parent sees concise provenance, never the child's private transcript.
    let body = format!(
        "Child Thread #{} report ({status}):\n{summary}\nArtifact references: {}",
        turn.thread_id,
        serde_json::to_string(artifact_ids)?
    );
    let ancestor = threads_parent(c, parent)?;
    enqueue(c, parent, &request_id, &body, ancestor, None, true)?;
    c.execute(
        "UPDATE threads SET read=0,revision=revision+1,updated_at=?2 WHERE id=?1",
        params![parent, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(activity_id)
}

fn threads_parent(c: &Connection, id: u64) -> anyhow::Result<Option<u64>> {
    Ok(c.query_row(
        "SELECT parent_thread_id FROM threads WHERE id=?1",
        [id],
        |r| r.get(0),
    )?)
}

impl Storage {
    pub(crate) async fn delegation_receipt(
        &self,
        caller: &ThreadCaller,
        operation_id: &str,
        invocation: &serde_json::Value,
    ) -> anyhow::Result<Option<DelegatedTurn>> {
        let c = self.conn.lock().await;
        thread_scope::validate_caller(&c, caller)?;
        let row=c.query_row("SELECT payload,child_thread_id,child_turn_id FROM thread_delegations WHERE requester_turn_id=?1 AND operation_id=?2",params![caller.turn_id,operation_id],|r|Ok((r.get::<_,String>(0)?,r.get(1)?,r.get(2)?))).optional()?;
        row.map(|(payload, thread_id, turn_id)| {
            anyhow::ensure!(
                payload == serde_json::to_string(invocation)?,
                "delegation operation payload changed"
            );
            Ok(DelegatedTurn { thread_id, turn_id })
        })
        .transpose()
    }
}
