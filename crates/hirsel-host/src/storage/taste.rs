//! Taste decisions recorded from judgment answers.

use super::Storage;
use super::common::{collect_rows, parse_ts};
use super::events::get_ping_optional;
use chrono::DateTime;
use chrono::Utc;
use rusqlite::params;
use serde::Serialize;

impl Storage {
    pub async fn record_taste_rule(
        &self,
        event_id: u64,
        choice: Option<&str>,
        rule: impl Into<String>,
    ) -> anyhow::Result<TasteDecision> {
        let rule = rule.into();
        if rule.trim().is_empty() {
            anyhow::bail!("taste rule must not be empty");
        }
        let conn = self.conn.lock().await;
        if get_ping_optional(&conn, event_id)?.is_none() {
            anyhow::bail!("unknown event: {event_id}");
        }
        let ts = Utc::now();
        conn.execute(
            "INSERT INTO taste_decisions (event_id, choice, rule, ts) VALUES (?1, ?2, ?3, ?4)",
            params![event_id, choice, rule, ts.to_rfc3339()],
        )?;
        Ok(TasteDecision {
            id: conn.last_insert_rowid() as u64,
            event_id,
            choice: choice.map(str::to_string),
            rule,
            ts,
        })
    }

    pub async fn taste_decisions(&self) -> anyhow::Result<Vec<TasteDecision>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, event_id, choice, rule, ts FROM taste_decisions ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], taste_decision_from_row)?;
        collect_rows(rows)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TasteDecision {
    pub id: u64,
    pub event_id: u64,
    pub choice: Option<String>,
    pub rule: String,
    pub ts: DateTime<Utc>,
}

fn taste_decision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TasteDecision> {
    let ts: String = row.get(4)?;
    Ok(TasteDecision {
        id: row.get(0)?,
        event_id: row.get(1)?,
        choice: row.get(2)?,
        rule: row.get(3)?,
        ts: parse_ts(&ts)?,
    })
}
