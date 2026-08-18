//! `meta` key/value rows and the Agent tool-surface session state.

use super::{
    AGENT_SESSION_GENERATION_META_KEY, AgentSessionState, Storage,
    TOOL_SURFACE_FINGERPRINT_META_KEY, TOOL_SURFACE_NAMES_META_KEY,
};
use anyhow::Context;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use std::collections::HashSet;

impl Storage {
    pub(crate) async fn reconcile_agent_tool_surface(
        &self,
        fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionState> {
        let mut normalized_names = tool_names.to_vec();
        normalized_names.sort();
        normalized_names.dedup();
        let encoded_names = serde_json::to_string(&normalized_names)?;

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let previous_fingerprint = meta_value_from_conn(&tx, TOOL_SURFACE_FINGERPRINT_META_KEY)?;
        let previous_names = meta_value_from_conn(&tx, TOOL_SURFACE_NAMES_META_KEY)?
            .map(|value| serde_json::from_str::<Vec<String>>(&value))
            .transpose()
            .context("decode stored Agent tool surface names")?
            .unwrap_or_default();
        let generation = meta_value_from_conn(&tx, AGENT_SESSION_GENERATION_META_KEY)?
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

        set_meta_value(&tx, TOOL_SURFACE_FINGERPRINT_META_KEY, fingerprint)?;
        set_meta_value(&tx, TOOL_SURFACE_NAMES_META_KEY, &encoded_names)?;
        if let Some(generation) = next_generation {
            set_meta_value(
                &tx,
                AGENT_SESSION_GENERATION_META_KEY,
                &generation.to_string(),
            )?;
        }
        tx.commit()?;

        Ok(AgentSessionState {
            session_id: next_generation
                .map(|generation| format!("agent-g{generation}"))
                .unwrap_or_else(|| "agent".to_string()),
            rotated,
            added_tools,
        })
    }

    #[cfg(test)]
    pub(crate) async fn meta_value(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().await;
        meta_value_from_conn(&conn, key).map_err(Into::into)
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
