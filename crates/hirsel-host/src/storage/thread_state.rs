//! Durable material state, independent from Thread metadata and conversation activity.
use super::{ThreadCaller, threads};
use hirsel_proto::ThreadState;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub(crate) const MAX_HEADLINE_BYTES: usize = 240;
pub(crate) const MAX_FINDINGS: usize = 32;
pub(crate) const MAX_FINDINGS_BYTES: usize = 8_192;
pub(crate) const MAX_ARTIFACTS: usize = 100;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StateActor {
    pub(crate) kind: &'static str,
    pub(crate) thread_id: Option<u64>,
    pub(crate) turn_id: Option<u64>,
}

impl StateActor {
    pub(crate) fn owner() -> Self {
        Self {
            kind: "owner",
            thread_id: None,
            turn_id: None,
        }
    }

    pub(crate) fn thread(caller: &ThreadCaller) -> Self {
        Self {
            kind: "thread",
            thread_id: Some(caller.thread_id),
            turn_id: Some(caller.turn_id),
        }
    }

    pub(crate) fn host() -> Self {
        Self {
            kind: "host",
            thread_id: None,
            turn_id: None,
        }
    }
}

pub(crate) fn normalize_headline(headline: &str) -> String {
    headline
        .split([' ', '\t', '\n', '\u{b}', '\u{c}', '\r'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn validate_headline(headline: &str) -> anyhow::Result<String> {
    let normalized = normalize_headline(headline);
    anyhow::ensure!(!normalized.is_empty(), "headline must not be empty");
    anyhow::ensure!(
        normalized.split(' ').count() <= 12,
        "headline must contain at most 12 words"
    );
    anyhow::ensure!(
        normalized.len() <= MAX_HEADLINE_BYTES,
        "headline must contain at most {MAX_HEADLINE_BYTES} bytes"
    );
    Ok(normalized)
}

pub(crate) fn validate_findings(findings: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        findings.len() <= MAX_FINDINGS,
        "state may contain at most {MAX_FINDINGS} findings"
    );
    anyhow::ensure!(
        findings.iter().all(|finding| !finding.trim().is_empty()),
        "findings must not be empty"
    );
    let encoded = serde_json::to_vec(findings)?;
    anyhow::ensure!(
        encoded.len() <= MAX_FINDINGS_BYTES,
        "encoded findings may contain at most {MAX_FINDINGS_BYTES} bytes"
    );
    Ok(())
}

pub(crate) fn get(c: &Connection, thread_id: u64) -> anyhow::Result<ThreadState> {
    let mut state = c.query_row(
        "SELECT revision,headline,own_headline,findings_json,checkpoint_at,steering_revision FROM thread_state WHERE thread_id=?1",
        [thread_id],
        |row| {
            Ok(ThreadState {
                revision: row.get(0)?,
                headline: row.get(1)?,
                own_headline: row.get(2)?,
                findings: serde_json::from_str(&row.get::<_, String>(3)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                artifact_ids: Vec::new(),
                checkpoint_at: row
                    .get::<_, Option<String>>(4)?
                    .map(|value| super::common::parse_ts(&value))
                    .transpose()?,
                steering_revision: row.get(5)?,
            })
        },
    )?;
    state.artifact_ids = c
        .prepare("SELECT artifact_id FROM thread_state_artifacts WHERE thread_id=?1 ORDER BY artifact_id")?
        .query_map([thread_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(state)
}

fn record_change(
    c: &Connection,
    thread_id: u64,
    before: &ThreadState,
    after: &ThreadState,
    actor: StateActor,
    cause: &str,
) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO thread_state_changes(thread_id,state_revision,before_json,after_json,actor_kind,actor_thread_id,actor_turn_id,cause,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            thread_id,
            after.revision,
            serde_json::to_string(before)?,
            serde_json::to_string(after)?,
            actor.kind,
            actor.thread_id,
            actor.turn_id,
            cause,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    let change_id = c.last_insert_rowid() as u64;
    super::thread_changes::record_deliveries(
        c,
        change_id,
        thread_id,
        before,
        after,
        actor.thread_id,
        actor.turn_id,
    )?;
    Ok(())
}

pub(crate) fn replace_headline(
    c: &Connection,
    thread_id: u64,
    headline: &str,
    actor: StateActor,
    cause: &str,
) -> anyhow::Result<bool> {
    let before = get(c, thread_id)?;
    if before.headline == headline {
        return Ok(false);
    }
    c.execute(
        "UPDATE thread_state SET headline=?2,revision=revision+1 WHERE thread_id=?1",
        params![thread_id, headline],
    )?;
    let after = get(c, thread_id)?;
    record_change(c, thread_id, &before, &after, actor, cause)?;
    Ok(true)
}

pub(crate) fn touch(
    c: &Connection,
    thread_id: u64,
    actor: StateActor,
    cause: &str,
    steering: bool,
) -> anyhow::Result<()> {
    let before = get(c, thread_id)?;
    c.execute(
        "UPDATE thread_state SET revision=revision+1,steering_revision=steering_revision+?2 WHERE thread_id=?1",
        params![thread_id, u8::from(steering)],
    )?;
    let after = get(c, thread_id)?;
    record_change(c, thread_id, &before, &after, actor, cause)?;
    crate::thread_rollups::refresh_ancestors(c, thread_id, actor)?;
    Ok(())
}

pub(crate) fn update(
    c: &Connection,
    caller: &ThreadCaller,
    thread_id: u64,
    expected_revision: u64,
    headline: &str,
    findings: Option<&[String]>,
    artifact_ids: Option<&[u64]>,
) -> anyhow::Result<Value> {
    anyhow::ensure!(
        threads::get(c, thread_id)?.kind == hirsel_proto::ThreadKind::Task,
        "material state can only be checkpointed on a Task"
    );
    let before = get(c, thread_id)?;
    if before.revision != expected_revision {
        return Ok(json!({
            "thread_id": thread_id,
            "conflict": true,
            "expected_state_revision": expected_revision,
            "actual_state_revision": before.revision,
            "state": before,
        }));
    }
    let own_headline = validate_headline(headline)?;
    let findings = findings.unwrap_or(&before.findings);
    validate_findings(findings)?;
    let artifact_ids = artifact_ids.unwrap_or(&before.artifact_ids);
    let mut artifact_ids = artifact_ids.to_vec();
    artifact_ids.sort_unstable();
    artifact_ids.dedup();
    anyhow::ensure!(
        artifact_ids.len() <= MAX_ARTIFACTS,
        "state may reference at most {MAX_ARTIFACTS} artifacts"
    );
    for artifact_id in &artifact_ids {
        super::thread_scope::authorize_artifact(c, caller.thread_id, *artifact_id)?;
    }
    let has_children: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads WHERE parent_thread_id=?1 AND archived_at IS NULL)",
        [thread_id],
        |row| row.get(0),
    )?;
    c.execute(
        "UPDATE thread_state SET revision=revision+1,own_headline=?2,headline=CASE WHEN ?3 THEN headline ELSE ?2 END,findings_json=?4,checkpoint_at=?5 WHERE thread_id=?1",
        params![
            thread_id,
            own_headline,
            has_children,
            serde_json::to_string(findings)?,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    c.execute(
        "DELETE FROM thread_state_artifacts WHERE thread_id=?1",
        [thread_id],
    )?;
    for artifact_id in artifact_ids {
        c.execute(
            "INSERT INTO thread_state_artifacts(thread_id,artifact_id) VALUES(?1,?2)",
            params![thread_id, artifact_id],
        )?;
    }
    if has_children {
        let headline = crate::thread_rollups::headline(c, thread_id)?;
        c.execute(
            "UPDATE thread_state SET headline=?2 WHERE thread_id=?1",
            params![thread_id, headline],
        )?;
    }
    let after = get(c, thread_id)?;
    record_change(
        c,
        thread_id,
        &before,
        &after,
        StateActor::thread(caller),
        "threads.state",
    )?;
    let ancestors =
        crate::thread_rollups::refresh_ancestors(c, thread_id, StateActor::thread(caller))?;
    let mut threads = vec![threads::get(c, thread_id)?];
    for id in ancestors {
        threads.push(threads::get(c, id)?);
    }
    Ok(json!({"thread_id":thread_id,"state":after,"threads":threads}))
}

pub(crate) fn touch_artifact_references(
    c: &Connection,
    artifact_id: u64,
    actor: StateActor,
) -> anyhow::Result<Vec<u64>> {
    let ids = c
        .prepare(
            "SELECT thread_id FROM thread_state_artifacts WHERE artifact_id=?1 ORDER BY thread_id",
        )?
        .query_map([artifact_id], |row| row.get::<_, u64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        touch(c, *id, actor, "artifact_revision", false)?;
    }
    Ok(ids)
}

#[cfg(test)]
#[path = "thread_state_tests.rs"]
mod tests;
