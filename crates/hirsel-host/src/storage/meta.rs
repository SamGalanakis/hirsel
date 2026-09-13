//! `meta` key/value rows and the Agent tool-surface session state.

use super::{AgentSessionState, Storage};
use anyhow::Context;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

impl Storage {
    pub(crate) async fn reconcile_agent_tool_surface(
        &self,
        thread_id: u64,
        fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionState> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        super::threads::get(&tx, thread_id)?;
        let history_id: String =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
                r.get(0)
            })?;
        let reconciled = reconcile_session_profile(
            &tx,
            &agent_session_profile_key(thread_id),
            fingerprint,
            tool_names,
            "Agent",
        )?;
        let session_id = format!("thread-{history_id}-{thread_id}-g{}", reconciled.generation);
        tx.execute(
            "INSERT INTO thread_process_sessions(history_id,session_id,thread_id) VALUES(?1,?2,?3) ON CONFLICT(session_id) DO UPDATE SET history_id=excluded.history_id,thread_id=excluded.thread_id",
            params![history_id, session_id, thread_id],
        )?;
        tx.commit()?;

        Ok(AgentSessionState {
            session_id,
            rotated: reconciled.rotated,
            added_tools: reconciled.added_tools,
        })
    }

    /// Reconcile the immutable execution profile of a native coding worker.
    /// This namespace is intentionally distinct from the coordinator's tool
    /// surface: switching a Task between host and worker sessions can never
    /// reopen the other role's durable conversation.
    pub(crate) async fn reconcile_native_worker_profile(
        &self,
        thread_id: u64,
        fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionState> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        super::threads::get(&tx, thread_id)?;
        let history_id: String =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
                r.get(0)
            })?;
        let reconciled = reconcile_session_profile(
            &tx,
            &native_worker_session_profile_key(thread_id),
            fingerprint,
            tool_names,
            "native worker",
        )?;
        tx.commit()?;

        Ok(AgentSessionState {
            session_id: format!(
                "native-thread-{history_id}-{thread_id}-g{}",
                reconciled.generation
            ),
            rotated: reconciled.rotated,
            added_tools: reconciled.added_tools,
        })
    }

    /// The newest Task turn already represented in the reusable native session.
    /// Turn order remains stable when a predecessor reply is written after a
    /// later Owner request was accepted.
    pub(crate) async fn native_worker_conversation_turn_watermark(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<Option<u64>> {
        let c = self.conn.lock().await;
        super::threads::get(&c, thread_id)?;
        meta_value_from_conn(
            &c,
            &format!("thread:{thread_id}:native_worker_conversation_turn_watermark"),
        )?
        .map(|value| value.parse::<u64>())
        .transpose()
        .context("decode native worker conversation turn watermark")
    }

    pub(crate) async fn native_worker_unowned_message_watermark(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<Option<u64>> {
        let c = self.conn.lock().await;
        super::threads::get(&c, thread_id)?;
        meta_value_from_conn(
            &c,
            &format!("thread:{thread_id}:native_worker_unowned_message_watermark"),
        )?
        .map(|value| value.parse::<u64>())
        .transpose()
        .context("decode native worker unowned message watermark")
    }

    /// Advance only over the completed native turn and loose chat actually
    /// represented in its session. Future queued turns remain eligible for a
    /// later handoff even when their Owner message has a lower global chat id.
    pub(crate) async fn mark_native_worker_conversation_seen(
        &self,
        thread_id: u64,
        turn_id: u64,
        unowned_message_watermark: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::threads::get(&tx, thread_id)?;
        let turn = super::thread_activity::get(&tx, turn_id)?;
        anyhow::ensure!(
            turn.thread_id == thread_id && turn.finished_at.is_some(),
            "native worker watermark requires its terminal Task turn"
        );
        let turn_key = format!("thread:{thread_id}:native_worker_conversation_turn_watermark");
        let previous_turn = meta_value_from_conn(&tx, &turn_key)?
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("decode native worker conversation turn watermark")?;
        set_meta_value(
            &tx,
            &turn_key,
            &previous_turn
                .map_or(turn_id, |previous| previous.max(turn_id))
                .to_string(),
        )?;
        if let Some(represented) = unowned_message_watermark {
            let message_key = format!("thread:{thread_id}:native_worker_unowned_message_watermark");
            let previous_message = meta_value_from_conn(&tx, &message_key)?
                .map(|value| value.parse::<u64>())
                .transpose()
                .context("decode native worker unowned message watermark")?;
            set_meta_value(
                &tx,
                &message_key,
                &previous_message
                    .map_or(represented, |previous| previous.max(represented))
                    .to_string(),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Abandon the current native-worker generation after a direct turn drive
    /// returned without a settled report. Lash may already have durably
    /// accepted that input, so a future Hirsel turn must not reopen and drive
    /// the same physical session.
    pub(crate) async fn abandon_native_worker_session(
        &self,
        expected_history: &str,
        thread_id: u64,
        turn_id: u64,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let history_id: String =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |row| {
                row.get(0)
            })?;
        anyhow::ensure!(
            history_id == expected_history,
            "native worker session abandonment belongs to a previous history"
        );
        super::threads::get(&tx, thread_id)?;
        invalidate_native_worker_profile(&tx, thread_id, &format!("abandoned-turn:{turn_id}"))?;
        tx.commit()?;
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct SessionProfile {
    fingerprint: String,
    tool_names: Vec<String>,
    generation: u64,
}

struct ReconciledSessionProfile {
    generation: u64,
    rotated: bool,
    added_tools: Vec<String>,
}

fn agent_session_profile_key(thread_id: u64) -> String {
    format!("thread:{thread_id}:agent_session_profile")
}

fn native_worker_session_profile_key(thread_id: u64) -> String {
    format!("thread:{thread_id}:native_worker_session_profile")
}

fn reconcile_session_profile(
    conn: &Connection,
    key: &str,
    fingerprint: &str,
    tool_names: &[String],
    role: &str,
) -> anyhow::Result<ReconciledSessionProfile> {
    let previous = meta_value_from_conn(conn, key)?
        .map(|value| serde_json::from_str::<SessionProfile>(&value))
        .transpose()
        .with_context(|| format!("decode stored {role} session profile"))?;
    let rotated = previous
        .as_ref()
        .is_some_and(|profile| profile.fingerprint != fingerprint);
    let generation = if rotated {
        previous
            .as_ref()
            .map_or(0, |profile| profile.generation)
            .checked_add(1)
            .with_context(|| format!("{role} session generation overflow"))?
    } else {
        previous.as_ref().map_or(0, |profile| profile.generation)
    };
    let mut normalized_names = tool_names.to_vec();
    normalized_names.sort();
    normalized_names.dedup();
    let added_tools = if rotated {
        let previous_names = previous
            .as_ref()
            .map(|profile| profile.tool_names.iter().cloned().collect::<HashSet<_>>())
            .unwrap_or_default();
        normalized_names
            .iter()
            .filter(|name| !previous_names.contains(*name))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let profile = SessionProfile {
        fingerprint: fingerprint.to_string(),
        tool_names: normalized_names,
        generation,
    };
    set_meta_value(conn, key, &serde_json::to_string(&profile)?)?;
    Ok(ReconciledSessionProfile {
        generation,
        rotated,
        added_tools,
    })
}

pub(super) fn invalidate_native_worker_profile(
    conn: &Connection,
    thread_id: u64,
    fingerprint: &str,
) -> anyhow::Result<()> {
    let key = native_worker_session_profile_key(thread_id);
    let mut profile = meta_value_from_conn(conn, &key)?
        .map(|value| serde_json::from_str::<SessionProfile>(&value))
        .transpose()
        .context("decode stored native worker session profile")?
        .unwrap_or_default();
    profile.fingerprint = fingerprint.to_string();
    set_meta_value(conn, &key, &serde_json::to_string(&profile)?)?;
    Ok(())
}

fn meta_value_from_conn(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
}

fn set_meta_value(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "
        INSERT INTO meta (key, value) VALUES (?1, ?2)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
        params![key, value],
    )?;
    Ok(())
}
