//! Bounded agent history with one coherent cursor for independent collections.
use super::{Storage, ThreadCaller, ThreadRef, chat, thread_activity, thread_scope, threads};
use hirsel_proto::{ChatMessage, Thread, ThreadActivity, ThreadBrief, ThreadTurn};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThreadReadCursor {
    pub messages_before: Option<u64>,
    pub turns_before: Option<u64>,
    pub activities_before: Option<u64>,
}
#[derive(Serialize)]
pub(crate) struct ScopedThreadDetail {
    pub history_id: String,
    pub reference_url: String,
    pub related_items: Vec<hirsel_proto::ThreadRelatedItem>,
    pub thread: Thread,
    pub brief: ThreadBrief,
    pub messages: Vec<ChatMessage>,
    pub turns: Vec<ThreadTurn>,
    pub activities: Vec<ThreadActivity>,
    pub next_cursor: Option<ThreadReadCursor>,
}
fn page_ids(
    c: &Connection,
    table: &str,
    id: u64,
    before: Option<u64>,
    limit: u64,
) -> anyhow::Result<(Vec<u64>, Option<u64>)> {
    let Some(before) = before else {
        return Ok((vec![], None));
    };
    let mut ids = c
        .prepare(&format!(
            "SELECT id FROM {table} WHERE thread_id=?1 AND id<?2 ORDER BY id DESC LIMIT ?3"
        ))?
        .query_map(params![id, before.min(i64::MAX as u64), limit + 1], |r| {
            r.get::<_, u64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = ids.len() > limit as usize;
    ids.truncate(limit as usize);
    ids.reverse();
    let next = more.then(|| ids[0]);
    Ok((ids, next))
}
pub(super) fn brief(c: &Connection, thread_id: u64) -> anyhow::Result<ThreadBrief> {
    let row=c.query_row("SELECT id,json_extract(data,'$.brief') FROM thread_activities WHERE thread_id=?1 AND kind='delegation_received' ORDER BY id DESC LIMIT 1",[thread_id],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?))).optional()?;
    let Some((id, text)) = row else {
        return Ok(ThreadBrief {
            text: String::new(),
            artifact_ids: vec![],
        });
    };
    let artifact_ids = c
        .prepare(
            "SELECT artifact_id FROM activity_artifacts WHERE activity_id=?1 ORDER BY artifact_id",
        )?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ThreadBrief { text, artifact_ids })
}
impl Storage {
    pub(crate) async fn scoped_thread_read(
        &self,
        caller: &ThreadCaller,
        reference: &ThreadRef,
        cursor: Option<ThreadReadCursor>,
        limit: u64,
    ) -> anyhow::Result<ScopedThreadDetail> {
        anyhow::ensure!((1..=100).contains(&limit), "read limit must be 1..100");
        let c = self.conn.lock().await;
        thread_scope::validate_caller(&c, caller)?;
        let id = thread_scope::resolve(&c, caller.thread_id, reference)?;
        let start = Some(i64::MAX as u64);
        let cursor = cursor.unwrap_or(ThreadReadCursor {
            messages_before: start,
            turns_before: start,
            activities_before: start,
        });
        let (m, messages_before) =
            page_ids(&c, "chat_messages", id, cursor.messages_before, limit)?;
        let (t, turns_before) = page_ids(&c, "thread_turns", id, cursor.turns_before, limit)?;
        let (a, activities_before) =
            page_ids(&c, "thread_activities", id, cursor.activities_before, limit)?;
        let messages = m
            .into_iter()
            .map(|id| chat::get_chat_message(&c, id).map_err(Into::into))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let turns = t
            .into_iter()
            .map(|id| thread_activity::get(&c, id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let activities = a
            .into_iter()
            .map(|id| thread_activity::activity(&c, id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let next_cursor =
            if messages_before.is_none() && turns_before.is_none() && activities_before.is_none() {
                None
            } else {
                Some(ThreadReadCursor {
                    messages_before,
                    turns_before,
                    activities_before,
                })
            };
        Ok(ScopedThreadDetail {
            history_id: caller.history_id.clone(),
            reference_url: thread_scope::reference_url(&caller.history_id, id),
            related_items: super::thread_related::list_scoped(&c, id, caller.thread_id)?,
            thread: threads::get(&c, id)?,
            brief: brief(&c, id)?,
            messages,
            turns,
            activities,
            next_cursor,
        })
    }
}
