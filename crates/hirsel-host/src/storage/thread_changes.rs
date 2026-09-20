//! Outside-change provenance and immutable per-turn admission snapshots.
use super::{Storage, chat, thread_activity, thread_scope, threads};
use hirsel_proto::{
    ChatMessage, TaskFocus, Thread, ThreadBrief, ThreadChange, ThreadChangeDigest,
    ThreadTurnContext,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DIGEST_CHANGES: usize = 32;
const MAX_DIGEST_BYTES: usize = 8 * 1024;

/// Everything the Host contributes to one provider input. The owner request
/// remains in `thread_requests`; this snapshot freezes every mutable lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TurnAdmissionContext {
    pub(crate) thread: Thread,
    pub(crate) brief: ThreadBrief,
    pub(crate) conversation: Vec<ChatMessage>,
    pub(crate) mentioned_thread_ids: Vec<u64>,
    pub(crate) artifact_references: Vec<Value>,
    pub(crate) focus: Option<TaskFocus>,
    pub(crate) changes: ThreadChangeDigest,
}

fn top_level_space(c: &Connection, thread_id: u64) -> anyhow::Result<Option<(u64, String)>> {
    c.query_row(
        "WITH RECURSIVE ancestors(id,parent_thread_id,kind,title) AS (
            SELECT id,parent_thread_id,kind,title FROM threads WHERE id=?1
            UNION ALL
            SELECT t.id,t.parent_thread_id,t.kind,t.title FROM threads t
            JOIN ancestors a ON a.parent_thread_id=t.id
         ) SELECT id,title FROM ancestors WHERE parent_thread_id IS NULL AND kind='space' LIMIT 1",
        [thread_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

fn artifact_ids(state: &hirsel_proto::ThreadState) -> impl Iterator<Item = u64> + '_ {
    state.artifact_ids.iter().copied()
}

fn artifact_reference_threads(c: &Connection, artifact_id: u64) -> anyhow::Result<Vec<u64>> {
    Ok(c.prepare(
        "SELECT m.thread_id FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id WHERE r.artifact_id=?1
         UNION SELECT a.thread_id FROM activity_artifacts r JOIN thread_activities a ON a.id=r.activity_id WHERE r.artifact_id=?1
         UNION SELECT id FROM threads WHERE showcased_artifact_id=?1
         UNION SELECT thread_id FROM thread_state_artifacts WHERE artifact_id=?1 ORDER BY 1",
    )?
    .query_map([artifact_id], |row| row.get(0))?
    .collect::<rusqlite::Result<_>>()?)
}

fn coalesced_activity(
    c: &Connection,
    chat_thread_id: u64,
    source_space_id: u64,
    source_space_title: &str,
    change_id: u64,
    actor_turn_id: Option<u64>,
) -> anyhow::Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO thread_change_cursors(chat_thread_id,consumed_change_id) VALUES(?1,0)",
        [chat_thread_id],
    )?;
    let consumed: u64 = c.query_row(
        "SELECT consumed_change_id FROM thread_change_cursors WHERE chat_thread_id=?1",
        [chat_thread_id],
        |row| row.get(0),
    )?;
    let key = format!("outside-change:{chat_thread_id}:{source_space_id}:{consumed}");
    let existing = c
        .query_row(
            "SELECT activity_id FROM thread_activity_keys WHERE key=?1",
            [&key],
            |row| row.get::<_, u64>(0),
        )
        .optional()?;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(activity_id) = existing {
        let previous: String = c.query_row(
            "SELECT data FROM thread_activities WHERE id=?1",
            [activity_id],
            |row| row.get(0),
        )?;
        let previous: Value = serde_json::from_str(&previous)?;
        let count = previous["change_count"].as_u64().unwrap_or(1) + 1;
        let text = format!("Changed by {source_space_title} · {count} updates");
        c.execute(
            "UPDATE thread_activities SET data=?2 WHERE id=?1",
            params![
                activity_id,
                serde_json::to_string(&json!({
                    "text": text,
                    "source_space_id": source_space_id,
                    "source_space_title": source_space_title,
                    "change_count": count,
                    "through_change_id": change_id,
                    "actor_turn_id": actor_turn_id,
                }))?
            ],
        )?;
    } else {
        let text = format!("Changed by {source_space_title} · 1 update");
        c.execute(
            "INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,NULL,'outside_change',?2,?3)",
            params![chat_thread_id, serde_json::to_string(&json!({
                "text": text,
                "source_space_id": source_space_id,
                "source_space_title": source_space_title,
                "change_count": 1,
                "through_change_id": change_id,
                "actor_turn_id": actor_turn_id,
            }))?, now],
        )?;
        let activity_id = c.last_insert_rowid() as u64;
        c.execute(
            "INSERT INTO thread_activity_keys(key,activity_id) VALUES(?1,?2)",
            params![key, activity_id],
        )?;
    }
    Ok(())
}

/// Record every top-level Space whose existing ancestry or explicit artifact
/// association is affected. This is provenance only; admission rechecks reach.
pub(crate) fn record_deliveries(
    c: &Connection,
    change_id: u64,
    changed_thread_id: u64,
    before: &hirsel_proto::ThreadState,
    after: &hirsel_proto::ThreadState,
    actor_thread_id: Option<u64>,
    actor_turn_id: Option<u64>,
) -> anyhow::Result<()> {
    let Some(actor_thread_id) = actor_thread_id else {
        return Ok(());
    };
    let Some((source_space_id, source_space_title)) = top_level_space(c, actor_thread_id)? else {
        return Ok(());
    };
    let mut affected = BTreeSet::new();
    if let Some((id, _)) = top_level_space(c, changed_thread_id)? {
        affected.insert(id);
    }
    for artifact_id in artifact_ids(before)
        .chain(artifact_ids(after))
        .collect::<BTreeSet<_>>()
    {
        for thread_id in artifact_reference_threads(c, artifact_id)? {
            if let Some((id, _)) = top_level_space(c, thread_id)? {
                affected.insert(id);
            }
        }
    }
    affected.remove(&source_space_id);
    for chat_thread_id in affected {
        let inserted = c.execute(
            "INSERT OR IGNORE INTO thread_change_deliveries(chat_thread_id,change_id) VALUES(?1,?2)",
            params![chat_thread_id, change_id],
        )? > 0;
        if inserted {
            coalesced_activity(
                c,
                chat_thread_id,
                source_space_id,
                &source_space_title,
                change_id,
                actor_turn_id,
            )?;
        }
    }
    Ok(())
}

fn shares_current_artifact(
    c: &Connection,
    chat_thread_id: u64,
    before: &hirsel_proto::ThreadState,
    after: &hirsel_proto::ThreadState,
) -> anyhow::Result<bool> {
    for artifact_id in artifact_ids(before)
        .chain(artifact_ids(after))
        .collect::<BTreeSet<_>>()
    {
        let shared: bool = c.query_row(
            "WITH RECURSIVE scope(id) AS (
                SELECT ?1 UNION ALL SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id
             ) SELECT EXISTS(
                SELECT 1 FROM message_artifacts r JOIN chat_messages m ON m.id=r.message_id JOIN scope s ON s.id=m.thread_id WHERE r.artifact_id=?2
                UNION SELECT 1 FROM activity_artifacts r JOIN thread_activities a ON a.id=r.activity_id JOIN scope s ON s.id=a.thread_id WHERE r.artifact_id=?2
                UNION SELECT 1 FROM threads t JOIN scope s ON s.id=t.id WHERE t.showcased_artifact_id=?2
                UNION SELECT 1 FROM thread_state_artifacts r JOIN scope s ON s.id=r.thread_id WHERE r.artifact_id=?2
             )",
            params![chat_thread_id, artifact_id],
            |row| row.get(0),
        )?;
        if shared {
            return Ok(true);
        }
    }
    Ok(false)
}

fn change_from_values(
    c: &Connection,
    raw: &(u64, u64, String, String, String, u64, u64, String, String),
) -> anyhow::Result<ThreadChange> {
    let before: hirsel_proto::ThreadState = serde_json::from_str(&raw.3)?;
    let after: hirsel_proto::ThreadState = serde_json::from_str(&raw.4)?;
    let (source_space_id, source_space_title) = top_level_space(c, raw.6)?
        .ok_or_else(|| anyhow::anyhow!("outside change actor has no Space"))?;
    Ok(ThreadChange {
        change_id: raw.0,
        thread_id: raw.1,
        thread_title: raw.2.clone(),
        state_revision: raw.5,
        before_headline: before.headline,
        after_headline: after.headline,
        cause: raw.7.clone(),
        source_space_id,
        source_space_title,
        created_at: super::common::parse_ts(&raw.8)?,
    })
}

fn digest(
    c: &Connection,
    chat_thread_id: u64,
    after_change_id: u64,
    limit: usize,
) -> anyhow::Result<ThreadChangeDigest> {
    let limit = limit.clamp(1, MAX_DIGEST_CHANGES);
    let mut statement = c.prepare(
        "SELECT s.id,s.thread_id,t.title,s.before_json,s.after_json,s.state_revision,s.actor_thread_id,s.cause,s.created_at
         FROM thread_change_deliveries d JOIN thread_state_changes s ON s.id=d.change_id JOIN threads t ON t.id=s.thread_id
         WHERE d.chat_thread_id=?1 AND d.change_id>?2 AND s.actor_thread_id IS NOT NULL
         ORDER BY d.change_id",
    )?;
    let rows = statement.query_map(params![chat_thread_id, after_change_id], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, u64>(5)?,
            row.get::<_, u64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    let mut coalesced = BTreeMap::<u64, ThreadChange>::new();
    let mut through = after_change_id;
    let mut has_more = false;
    let mut raw_count = 0_usize;
    for raw in rows {
        let raw = raw?;
        let before: hirsel_proto::ThreadState = serde_json::from_str(&raw.3)?;
        let after: hirsel_proto::ThreadState = serde_json::from_str(&raw.4)?;
        let changed_space = top_level_space(c, raw.1)?.map(|(id, _)| id);
        let associated = changed_space == Some(chat_thread_id)
            || shares_current_artifact(c, chat_thread_id, &before, &after)?;
        let visible = associated && thread_scope::authorize(c, chat_thread_id, raw.1).is_ok();
        if !visible {
            through = raw.0;
            continue;
        }
        if raw_count == limit {
            has_more = true;
            break;
        }
        let synthetic = change_from_values(c, &raw)?;
        let mut candidate = coalesced.clone();
        candidate
            .entry(raw.1)
            .and_modify(|change| {
                let before_headline = change.before_headline.clone();
                *change = synthetic.clone();
                change.before_headline = before_headline;
            })
            .or_insert(synthetic);
        let candidate_digest = ThreadChangeDigest {
            through_change_id: raw.0,
            changes: candidate.values().cloned().collect(),
            has_more: false,
        };
        if serde_json::to_vec(&candidate_digest)?.len() > MAX_DIGEST_BYTES {
            has_more = true;
            break;
        }
        coalesced = candidate;
        through = raw.0;
        raw_count += 1;
    }
    if !has_more {
        has_more = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_change_deliveries WHERE chat_thread_id=?1 AND change_id>?2)",
            params![chat_thread_id, through],
            |row| row.get(0),
        )?;
    }
    Ok(ThreadChangeDigest {
        through_change_id: through,
        changes: coalesced.into_values().collect(),
        has_more,
    })
}

pub(super) fn latest_context(
    c: &Connection,
    thread_id: u64,
) -> anyhow::Result<Option<ThreadTurnContext>> {
    c.query_row(
        "SELECT x.turn_id,x.context_json,x.through_change_id,x.consumed_at
         FROM thread_turn_contexts x JOIN thread_turns t ON t.id=x.turn_id
         WHERE t.thread_id=?1 ORDER BY x.turn_id DESC LIMIT 1",
        [thread_id],
        |row| {
            Ok(ThreadTurnContext {
                turn_id: row.get(0)?,
                context: serde_json::from_str(&row.get::<_, String>(1)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        error.into(),
                    )
                })?,
                through_change_id: row.get(2)?,
                consumed_at: row
                    .get::<_, Option<String>>(3)?
                    .map(|value| super::common::parse_ts(&value))
                    .transpose()?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

impl Storage {
    /// Freeze mutable conversation, state, focus, artifact and outside-change
    /// context before any executor receives this turn.
    pub(crate) async fn accepted_turn_context(
        &self,
        history: &str,
        turn_id: u64,
    ) -> anyhow::Result<TurnAdmissionContext> {
        let mut guard = self.conn.lock().await;
        let tx = guard.transaction()?;
        thread_scope::validate_history(&tx, history)?;
        if let Some(encoded) = tx
            .query_row(
                "SELECT context_json FROM thread_turn_contexts WHERE turn_id=?1",
                [turn_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            tx.commit()?;
            return Ok(serde_json::from_str(&encoded)?);
        }
        let turn = thread_activity::get(&tx, turn_id)?;
        let thread = threads::get(&tx, turn.thread_id)?;
        let mut conversation = Vec::new();
        let before_id = turn.owner_message_id.unwrap_or(i64::MAX as u64);
        let mut ids = tx
            .prepare("SELECT id FROM chat_messages WHERE thread_id=?1 AND id<?2 ORDER BY id DESC LIMIT 30")?
            .query_map(params![turn.thread_id, before_id], |row| row.get::<_, u64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.reverse();
        for id in ids {
            conversation.push(chat::get_chat_message(&tx, id)?);
        }
        let accepted_message = turn
            .owner_message_id
            .map(|message_id| chat::get_chat_message(&tx, message_id))
            .transpose()?;
        let artifact_references = if let Some(message_id) = turn.owner_message_id {
            tx.prepare("SELECT a.id,a.title FROM message_artifacts r JOIN artifacts a ON a.id=r.artifact_id WHERE r.message_id=?1 ORDER BY a.id")?
                .query_map([message_id], |row| Ok(json!({"artifact_id":row.get::<_,u64>(0)?,"title":row.get::<_,String>(1)?})))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let mentioned_thread_ids = accepted_message
            .as_ref()
            .map(|message| message.mentions.clone())
            .unwrap_or_default();
        let focus = accepted_message.and_then(|message| message.focus);
        let consumed = tx
            .query_row(
                "SELECT consumed_change_id FROM thread_change_cursors WHERE chat_thread_id=?1",
                [turn.thread_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let changes = if thread.kind == hirsel_proto::ThreadKind::Space {
            digest(&tx, turn.thread_id, consumed, MAX_DIGEST_CHANGES)?
        } else {
            ThreadChangeDigest {
                through_change_id: consumed,
                ..ThreadChangeDigest::default()
            }
        };
        let context = TurnAdmissionContext {
            thread,
            brief: super::thread_read::brief(&tx, turn.thread_id)?,
            conversation,
            mentioned_thread_ids,
            artifact_references,
            focus,
            changes,
        };
        tx.execute(
            "INSERT INTO thread_turn_contexts(turn_id,context_json,through_change_id,consumed_at) VALUES(?1,?2,?3,NULL)",
            params![turn_id, serde_json::to_string(&context)?, context.changes.through_change_id],
        )?;
        tx.commit()?;
        Ok(context)
    }

    pub(crate) async fn thread_changes(
        &self,
        caller: &super::ThreadCaller,
        after_change_id: u64,
        limit: usize,
    ) -> anyhow::Result<ThreadChangeDigest> {
        let c = self.conn.lock().await;
        thread_scope::validate_caller(&c, caller)?;
        anyhow::ensure!(
            threads::get(&c, caller.thread_id)?.kind == hirsel_proto::ThreadKind::Space,
            "changes are delivered to Space chats"
        );
        digest(&c, caller.thread_id, after_change_id, limit)
    }

    pub(crate) async fn outside_change_activities(
        &self,
        actor_turn_id: u64,
    ) -> anyhow::Result<Vec<hirsel_proto::ThreadActivity>> {
        let c = self.conn.lock().await;
        let ids = c
            .prepare("SELECT id FROM thread_activities WHERE kind='outside_change' AND json_extract(data,'$.actor_turn_id')=?1 ORDER BY id")?
            .query_map([actor_turn_id], |row| row.get::<_, u64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| super::thread_activity::activity(&c, id))
            .collect()
    }
}

pub(crate) fn consume_completed_context(
    c: &Connection,
    turn_id: u64,
    thread_id: u64,
) -> anyhow::Result<()> {
    let through = c
        .query_row(
            "SELECT through_change_id FROM thread_turn_contexts WHERE turn_id=?1 AND consumed_at IS NULL",
            [turn_id],
            |row| row.get::<_, u64>(0),
        )
        .optional()?;
    let Some(through) = through else {
        return Ok(());
    };
    c.execute(
        "INSERT INTO thread_change_cursors(chat_thread_id,consumed_change_id) VALUES(?1,?2)
         ON CONFLICT(chat_thread_id) DO UPDATE SET consumed_change_id=max(consumed_change_id,excluded.consumed_change_id)",
        params![thread_id, through],
    )?;
    c.execute(
        "UPDATE thread_turn_contexts SET consumed_at=?2 WHERE turn_id=?1",
        params![turn_id, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "thread_changes_tests.rs"]
mod tests;
