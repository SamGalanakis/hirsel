//! Monitor records and their projection to process info.

use super::Storage;
use super::common::{collect_rows, parse_ts, u64_from_row};
use crate::text::short_label;
use chrono::DateTime;
use chrono::Utc;
use hirsel_proto::ProcessInfo;
use hirsel_proto::ProcessKind;
use hirsel_proto::ProcessState;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

impl Storage {
    pub async fn create_monitor(
        &self,
        cmd: impl Into<String>,
        every_secs: u64,
        wake_on: MonitorWakeOn,
        pattern: Option<String>,
        label: impl Into<String>,
    ) -> anyhow::Result<MonitorRecord> {
        let now = Utc::now();
        let record = MonitorRecord {
            id: format!("mon-{}", Uuid::new_v4()),
            cmd: cmd.into(),
            every_secs: every_secs.max(30),
            wake_on,
            pattern,
            label: label.into(),
            created_ts: now,
            last_event_ts: now,
            last_run_ts: None,
            last_output: None,
            summary: None,
            cancelled_ts: None,
        };
        validate_monitor_record(&record)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO monitors (
                id,
                cmd,
                every_secs,
                wake_on,
                pattern,
                label,
                created_ts,
                last_event_ts,
                last_run_ts,
                last_output,
                summary,
                cancelled_ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, NULL)
            ",
            params![
                record.id,
                record.cmd,
                record.every_secs,
                monitor_wake_on_to_str(record.wake_on),
                record.pattern,
                record.label,
                record.created_ts.to_rfc3339(),
                record.last_event_ts.to_rfc3339(),
            ],
        )?;
        get_monitor(&conn, &record.id).map_err(Into::into)
    }

    pub async fn monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let conn = self.conn.lock().await;
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn active_monitors(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
                   last_run_ts, last_output, summary, cancelled_ts
            FROM monitors
            WHERE cancelled_ts IS NULL
            ORDER BY created_ts ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], monitor_from_row)?;
        collect_rows(rows)
    }

    pub async fn monitors_list(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
                   last_run_ts, last_output, summary, cancelled_ts
            FROM monitors
            ORDER BY created_ts ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], monitor_from_row)?;
        collect_rows(rows)
    }

    pub async fn cancel_monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "
            UPDATE monitors
            SET cancelled_ts = COALESCE(cancelled_ts, ?2),
                last_event_ts = ?2,
                summary = 'cancelled'
            WHERE id = ?1
            ",
            params![monitor_id, now.to_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn record_monitor_tick(
        &self,
        monitor_id: &str,
        last_output: String,
        summary: String,
    ) -> anyhow::Result<Option<MonitorRecord>> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "
            UPDATE monitors
            SET last_run_ts = ?2,
                last_event_ts = ?2,
                last_output = ?3,
                summary = ?4
            WHERE id = ?1 AND cancelled_ts IS NULL
            ",
            params![monitor_id, now.to_rfc3339(), last_output, summary],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn monitor_snapshot(&self) -> anyhow::Result<Vec<ProcessInfo>> {
        let mut records = self.monitors_list().await?;
        let mut active = Vec::new();
        let mut terminal = Vec::new();
        for record in records.drain(..) {
            if record.cancelled_ts.is_some() {
                terminal.push(record);
            } else {
                active.push(record);
            }
        }
        terminal.sort_by(|left, right| {
            left.last_event_ts
                .cmp(&right.last_event_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        if terminal.len() > 10 {
            terminal.drain(..terminal.len() - 10);
        }
        Ok(active
            .iter()
            .chain(terminal.iter())
            .map(monitor_process_info)
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorWakeOn {
    Changed,
    ExitZero,
    ExitNonzero,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonitorRecord {
    pub id: String,
    pub cmd: String,
    pub every_secs: u64,
    pub wake_on: MonitorWakeOn,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub label: String,
    pub created_ts: DateTime<Utc>,
    pub last_event_ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_ts: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_ts: Option<DateTime<Utc>>,
}

pub fn monitor_process_info(record: &MonitorRecord) -> ProcessInfo {
    ProcessInfo {
        id: record.id.clone(),
        kind: ProcessKind::Monitor,
        label: short_label(&record.label),
        agent: None,
        model: None,
        state: if record.cancelled_ts.is_some() {
            ProcessState::Cancelled
        } else {
            ProcessState::Running
        },
        started_ts: record.created_ts,
        last_event_ts: record.last_event_ts,
        // The client's monitor rows have no dedicated cmd/interval fields;
        // the summary carries them (see app/PROTOCOL.md v1.4 notes).
        summary: Some(match &record.summary {
            Some(summary) => format!("{} · every {}s — {summary}", record.cmd, record.every_secs),
            None => format!("{} · every {}s", record.cmd, record.every_secs),
        }),
    }
}

fn validate_monitor_record(record: &MonitorRecord) -> anyhow::Result<()> {
    if record.cmd.trim().is_empty() {
        anyhow::bail!("monitor cmd is required");
    }
    if record.label.trim().is_empty() {
        anyhow::bail!("monitor label is required");
    }
    if matches!(record.wake_on, MonitorWakeOn::Regex)
        && record.pattern.as_deref().is_none_or(str::is_empty)
    {
        anyhow::bail!("monitor pattern is required for regex wake_on");
    }
    Ok(())
}

fn get_monitor(conn: &Connection, monitor_id: &str) -> rusqlite::Result<MonitorRecord> {
    get_monitor_optional(conn, monitor_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

fn get_monitor_optional(
    conn: &Connection,
    monitor_id: &str,
) -> rusqlite::Result<Option<MonitorRecord>> {
    conn.query_row(
        "
        SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
               last_run_ts, last_output, summary, cancelled_ts
        FROM monitors
        WHERE id = ?1
        ",
        params![monitor_id],
        monitor_from_row,
    )
    .optional()
}

fn monitor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MonitorRecord> {
    let wake_on: String = row.get(3)?;
    let created_ts: String = row.get(6)?;
    let last_event_ts: String = row.get(7)?;
    let last_run_ts: Option<String> = row.get(8)?;
    let cancelled_ts: Option<String> = row.get(11)?;
    Ok(MonitorRecord {
        id: row.get(0)?,
        cmd: row.get(1)?,
        every_secs: u64_from_row(row, 2)?,
        wake_on: monitor_wake_on_from_str(&wake_on)?,
        pattern: row.get(4)?,
        label: row.get(5)?,
        created_ts: parse_ts(&created_ts)?,
        last_event_ts: parse_ts(&last_event_ts)?,
        last_run_ts: last_run_ts.as_deref().map(parse_ts).transpose()?,
        last_output: row.get(9)?,
        summary: row.get(10)?,
        cancelled_ts: cancelled_ts.as_deref().map(parse_ts).transpose()?,
    })
}

fn monitor_wake_on_to_str(wake_on: MonitorWakeOn) -> &'static str {
    match wake_on {
        MonitorWakeOn::Changed => "changed",
        MonitorWakeOn::ExitZero => "exit_zero",
        MonitorWakeOn::ExitNonzero => "exit_nonzero",
        MonitorWakeOn::Regex => "regex",
    }
}

fn monitor_wake_on_from_str(value: &str) -> rusqlite::Result<MonitorWakeOn> {
    match value {
        "changed" => Ok(MonitorWakeOn::Changed),
        "exit_zero" => Ok(MonitorWakeOn::ExitZero),
        "exit_nonzero" => Ok(MonitorWakeOn::ExitNonzero),
        "regex" => Ok(MonitorWakeOn::Regex),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[cfg(test)]
mod tests;
