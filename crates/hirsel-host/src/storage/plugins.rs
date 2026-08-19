//! Plugin enable state, setting values, and per-plugin KV.
//!
//! Three flat tables keyed by plugin id. Values are stored as JSON text so a
//! setting or KV entry keeps its type across a restart. Nothing here masks
//! secrets — masking is a presentation concern and lives at the management
//! API boundary, which is the only layer that knows a key's declared kind.

use std::collections::BTreeMap;

use rusqlite::params;
use serde_json::{Map, Value};

use super::Storage;
use super::common::collect_rows;

impl Storage {
    /// Persisted enable flags. A plugin absent from the map has never been
    /// seen; the caller defaults it to enabled on first sight.
    pub async fn plugin_enabled_flags(&self) -> anyhow::Result<BTreeMap<String, bool>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT plugin_id, enabled FROM plugin_state")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let enabled: i64 = row.get(1)?;
            Ok((id, enabled != 0))
        })?;
        Ok(collect_rows::<(String, bool)>(rows)?.into_iter().collect())
    }

    pub async fn set_plugin_enabled(&self, plugin_id: &str, enabled: bool) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO plugin_state (plugin_id, enabled) VALUES (?1, ?2)
             ON CONFLICT(plugin_id) DO UPDATE SET enabled = excluded.enabled",
            params![plugin_id, i64::from(enabled)],
        )?;
        Ok(())
    }

    /// Stored setting values for one plugin. Declared defaults are *not*
    /// folded in here — the caller owns the descriptors.
    pub async fn plugin_settings(&self, plugin_id: &str) -> anyhow::Result<Map<String, Value>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT key, value FROM plugin_settings WHERE plugin_id = ?1 ORDER BY key")?;
        let rows = stmt.query_map(params![plugin_id], decode_entry)?;
        Ok(collect_rows::<(String, Value)>(rows)?.into_iter().collect())
    }

    /// Merge `values` into the plugin's stored settings and return the merged
    /// result. Keys absent from `values` keep their stored value.
    pub async fn merge_plugin_settings(
        &self,
        plugin_id: &str,
        values: &Map<String, Value>,
    ) -> anyhow::Result<Map<String, Value>> {
        {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            for (key, value) in values {
                tx.execute(
                    "INSERT INTO plugin_settings (plugin_id, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT(plugin_id, key) DO UPDATE SET value = excluded.value",
                    params![plugin_id, key, serde_json::to_string(value)?],
                )?;
            }
            tx.commit()?;
        }
        self.plugin_settings(plugin_id).await
    }

    pub async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> anyhow::Result<Option<Value>> {
        let conn = self.conn.lock().await;
        let stored: Option<String> = conn
            .query_row(
                "SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
                params![plugin_id, key],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        stored
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(Into::into)
    }

    pub async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &Value,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO plugin_kv (plugin_id, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(plugin_id, key) DO UPDATE SET value = excluded.value",
            params![plugin_id, key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    pub async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2",
            params![plugin_id, key],
        )?;
        Ok(())
    }

    pub async fn plugin_kv_entries(&self, plugin_id: &str) -> anyhow::Result<Vec<(String, Value)>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT key, value FROM plugin_kv WHERE plugin_id = ?1 ORDER BY key")?;
        let rows = stmt.query_map(params![plugin_id], decode_entry)?;
        collect_rows(rows)
    }
}

fn decode_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Value)> {
    let key: String = row.get(0)?;
    let value: String = row.get(1)?;
    let value = serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok((key, value))
}
