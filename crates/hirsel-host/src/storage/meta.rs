//! `meta` key/value rows and the Agent tool-surface session state.

use super::{AgentSessionState, Storage};
use anyhow::Context;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use std::collections::HashSet;

impl Storage {
    pub(crate) async fn reconcile_agent_tool_surface(
        &self,
        thread_id: u64,
        fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionState> {
        let mut normalized_names = tool_names.to_vec();
        normalized_names.sort();
        normalized_names.dedup();
        let encoded_names = serde_json::to_string(&normalized_names)?;

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        super::threads::get(&tx, thread_id)?;
        let fingerprint_key = format!("thread:{thread_id}:tool_fingerprint");
        let names_key = format!("thread:{thread_id}:tool_names");
        let generation_key = format!("thread:{thread_id}:session_generation");
        let history_id: String =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
                r.get(0)
            })?;
        let previous_fingerprint = meta_value_from_conn(&tx, &fingerprint_key)?;
        let previous_names = meta_value_from_conn(&tx, &names_key)?
            .map(|value| serde_json::from_str::<Vec<String>>(&value))
            .transpose()
            .context("decode stored Agent tool surface names")?
            .unwrap_or_default();
        let generation = meta_value_from_conn(&tx, &generation_key)?
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("decode stored Agent session generation")?;

        let rotated = previous_fingerprint
            .as_deref()
            .is_some_and(|previous| previous != fingerprint);
        let next_generation = if rotated {
            Some(
                generation
                    .unwrap_or(0)
                    .checked_add(1)
                    .context("Agent session generation overflow")?,
            )
        } else {
            generation
        };
        let added_tools = if rotated {
            let previous_names = previous_names.into_iter().collect::<HashSet<_>>();
            normalized_names
                .iter()
                .filter(|name| !previous_names.contains(*name))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        set_meta_value(&tx, &fingerprint_key, fingerprint)?;
        set_meta_value(&tx, &names_key, &encoded_names)?;
        if let Some(generation) = next_generation {
            set_meta_value(&tx, &generation_key, &generation.to_string())?;
        }
        tx.commit()?;

        Ok(AgentSessionState {
            session_id: format!(
                "thread-{history_id}-{thread_id}-g{}",
                next_generation.unwrap_or(0)
            ),
            rotated,
            added_tools,
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
        let mut normalized_names = tool_names.to_vec();
        normalized_names.sort();
        normalized_names.dedup();
        let encoded_names = serde_json::to_string(&normalized_names)?;

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        super::threads::get(&tx, thread_id)?;
        let fingerprint_key = format!("thread:{thread_id}:native_worker_fingerprint");
        let names_key = format!("thread:{thread_id}:native_worker_tool_names");
        let generation_key = format!("thread:{thread_id}:native_worker_generation");
        let history_id: String =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
                r.get(0)
            })?;
        let previous_fingerprint = meta_value_from_conn(&tx, &fingerprint_key)?;
        let previous_names = meta_value_from_conn(&tx, &names_key)?
            .map(|value| serde_json::from_str::<Vec<String>>(&value))
            .transpose()
            .context("decode stored native worker tool names")?
            .unwrap_or_default();
        let generation = meta_value_from_conn(&tx, &generation_key)?
            .map(|value| value.parse::<u64>())
            .transpose()
            .context("decode stored native worker session generation")?;
        let rotated = previous_fingerprint
            .as_deref()
            .is_some_and(|previous| previous != fingerprint);
        let next_generation = if rotated {
            Some(
                generation
                    .unwrap_or(0)
                    .checked_add(1)
                    .context("native worker session generation overflow")?,
            )
        } else {
            generation
        };
        let previous_names = previous_names.into_iter().collect::<HashSet<_>>();
        let added_tools = if rotated {
            normalized_names
                .iter()
                .filter(|name| !previous_names.contains(*name))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        set_meta_value(&tx, &fingerprint_key, fingerprint)?;
        set_meta_value(&tx, &names_key, &encoded_names)?;
        if let Some(generation) = next_generation {
            set_meta_value(&tx, &generation_key, &generation.to_string())?;
        }
        tx.commit()?;

        Ok(AgentSessionState {
            session_id: format!(
                "native-thread-{history_id}-{thread_id}-g{}",
                next_generation.unwrap_or(0)
            ),
            rotated,
            added_tools,
        })
    }

    /// The newest Task message already represented in the reusable native
    /// session. Keeping this cursor separate from the profile generation lets
    /// the same session receive only conversation written by another backend.
    pub(crate) async fn native_worker_conversation_watermark(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<Option<u64>> {
        let c = self.conn.lock().await;
        super::threads::get(&c, thread_id)?;
        meta_value_from_conn(
            &c,
            &format!("thread:{thread_id}:native_worker_conversation_watermark"),
        )?
        .map(|value| value.parse::<u64>())
        .transpose()
        .context("decode native worker conversation watermark")
    }

    /// Advance the native session cursor only after its terminal projection is
    /// durable. Failed direct drives abandon their session and deliberately do
    /// not call this, so their accepted input is available to the next handoff.
    pub(crate) async fn mark_native_worker_conversation_seen(
        &self,
        thread_id: u64,
    ) -> anyhow::Result<()> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        super::threads::get(&tx, thread_id)?;
        let latest = tx.query_row(
            "SELECT MAX(id) FROM chat_messages WHERE thread_id=?1",
            [thread_id],
            |row| row.get::<_, Option<u64>>(0),
        )?;
        if let Some(latest) = latest {
            set_meta_value(
                &tx,
                &format!("thread:{thread_id}:native_worker_conversation_watermark"),
                &latest.to_string(),
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
        set_meta_value(
            &tx,
            &format!("thread:{thread_id}:native_worker_fingerprint"),
            &format!("abandoned-turn:{turn_id}"),
        )?;
        tx.commit()?;
        Ok(())
    }
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
