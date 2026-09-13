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

/// Poison this Thread's Agent session fingerprint so the next reconcile rotates
/// the generation. A turn whose side effects may already have landed must not be
/// continued inside the session that started it.
pub(super) fn invalidate_agent_session_profile(
    conn: &Connection,
    thread_id: u64,
    fingerprint: &str,
) -> anyhow::Result<()> {
    let key = agent_session_profile_key(thread_id);
    let mut profile = meta_value_from_conn(conn, &key)?
        .map(|value| serde_json::from_str::<SessionProfile>(&value))
        .transpose()
        .context("decode stored Agent session profile")?
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
