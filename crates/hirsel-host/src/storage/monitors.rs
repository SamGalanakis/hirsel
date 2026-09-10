//! Monitor records and their projection to process info.

use super::Storage;
use super::common::{collect_rows, parse_ts, u64_from_row};
use crate::text::short_label;
use chrono::DateTime;
use chrono::Utc;
use hirsel_proto::ProcessInfo;
use hirsel_proto::ProcessKind;
use hirsel_proto::ProcessState;
use regex::Regex;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Type;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use uuid::Uuid;

impl Storage {
    pub async fn create_monitor(
        &self,
        thread_id: u64,
        cmd: impl Into<String>,
        every_secs: u64,
        condition: MonitorCondition,
        label: impl Into<String>,
    ) -> anyhow::Result<MonitorRecord> {
        let now = Utc::now();
        let record = MonitorRecord {
            thread_id,
            id: format!("mon-{}", Uuid::new_v4()),
            cmd: cmd.into(),
            every_secs: every_secs.max(30),
            condition,
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
        super::threads::get(&conn, thread_id)?;
        insert_monitor(&conn, &record)
    }

    pub(crate) async fn create_scoped_monitor(
        &self,
        caller: &super::ThreadCaller,
        cmd: String,
        every_secs: u64,
        condition: MonitorCondition,
        label: String,
    ) -> anyhow::Result<MonitorRecord> {
        let now = Utc::now();
        let record = MonitorRecord {
            thread_id: caller.thread_id,
            id: format!("mon-{}", Uuid::new_v4()),
            cmd,
            every_secs: every_secs.max(30),
            condition,
            label,
            created_ts: now,
            last_event_ts: now,
            last_run_ts: None,
            last_output: None,
            summary: None,
            cancelled_ts: None,
        };
        validate_monitor_record(&record)?;
        let conn = self.conn.lock().await;
        super::thread_scope::validate_caller(&conn, caller)?;
        insert_monitor(&conn, &record)
    }
}
fn insert_monitor(conn: &Connection, record: &MonitorRecord) -> anyhow::Result<MonitorRecord> {
    conn.execute(
        "
            INSERT INTO monitors (
                thread_id, id,
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
            VALUES (?9, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, NULL)
            ",
        params![
            record.id,
            record.cmd,
            record.every_secs,
            record.condition.wake_on(),
            record.condition.pattern(),
            record.label,
            record.created_ts.to_rfc3339(),
            record.last_event_ts.to_rfc3339(),
            record.thread_id,
        ],
    )?;
    get_monitor(conn, &record.id).map_err(Into::into)
}
impl Storage {
    pub async fn monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let conn = self.conn.lock().await;
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn active_monitors(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
                   last_run_ts, last_output, summary, cancelled_ts, thread_id
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
                   last_run_ts, last_output, summary, cancelled_ts, thread_id
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "wake_on", content = "pattern", rename_all = "snake_case")]
pub enum MonitorCondition {
    Changed,
    ExitZero,
    ExitNonzero,
    Regex(MonitorRegex),
}

impl MonitorCondition {
    pub fn parse(wake_on: &str, pattern: Option<String>) -> Result<Self, MonitorConditionError> {
        match wake_on {
            "changed" => Self::without_pattern(Self::Changed, pattern),
            "exit_zero" => Self::without_pattern(Self::ExitZero, pattern),
            "exit_nonzero" => Self::without_pattern(Self::ExitNonzero, pattern),
            "regex" => {
                let pattern = pattern.ok_or(MonitorConditionError::MissingRegexPattern)?;
                Ok(Self::Regex(MonitorRegex::new(pattern)?))
            }
            other => Err(MonitorConditionError::UnknownWakeOn(other.to_string())),
        }
    }

    fn without_pattern(
        condition: Self,
        pattern: Option<String>,
    ) -> Result<Self, MonitorConditionError> {
        if pattern.is_some() {
            return Err(MonitorConditionError::UnexpectedPattern);
        }
        Ok(condition)
    }

    pub fn wake_on(&self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::ExitZero => "exit_zero",
            Self::ExitNonzero => "exit_nonzero",
            Self::Regex(_) => "regex",
        }
    }

    pub fn pattern(&self) -> Option<&str> {
        match self {
            Self::Regex(pattern) => Some(pattern.as_str()),
            Self::Changed | Self::ExitZero | Self::ExitNonzero => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MonitorConditionError {
    #[error("monitor pattern is required for regex wake_on")]
    MissingRegexPattern,
    #[error("monitor pattern is only allowed for regex wake_on")]
    UnexpectedPattern,
    #[error("invalid monitor regex: {0}")]
    InvalidRegex(#[from] regex::Error),
    #[error("wake_on must be changed, exit_zero, exit_nonzero, or regex, got `{0}`")]
    UnknownWakeOn(String),
}

#[derive(Debug, Clone)]
pub struct MonitorRegex(Regex);

impl MonitorRegex {
    fn new(pattern: String) -> Result<Self, MonitorConditionError> {
        if pattern.is_empty() {
            return Err(MonitorConditionError::MissingRegexPattern);
        }
        Ok(Self(Regex::new(&pattern)?))
    }

    pub(crate) fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq for MonitorRegex {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for MonitorRegex {}

impl Serialize for MonitorRegex {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MonitorRegex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        MonitorRegex::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonitorRecord {
    pub thread_id: u64,
    pub id: String,
    pub cmd: String,
    pub every_secs: u64,
    #[serde(flatten)]
    pub condition: MonitorCondition,
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
        thread_id: record.thread_id,
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
               last_run_ts, last_output, summary, cancelled_ts, thread_id
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
    let pattern: Option<String> = row.get(4)?;
    let created_ts: String = row.get(6)?;
    let last_event_ts: String = row.get(7)?;
    let last_run_ts: Option<String> = row.get(8)?;
    let cancelled_ts: Option<String> = row.get(11)?;
    Ok(MonitorRecord {
        thread_id: row.get(12)?,
        id: row.get(0)?,
        cmd: row.get(1)?,
        every_secs: u64_from_row(row, 2)?,
        condition: MonitorCondition::parse(&wake_on, pattern).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(error))
        })?,
        label: row.get(5)?,
        created_ts: parse_ts(&created_ts)?,
        last_event_ts: parse_ts(&last_event_ts)?,
        last_run_ts: last_run_ts.as_deref().map(parse_ts).transpose()?,
        last_output: row.get(9)?,
        summary: row.get(10)?,
        cancelled_ts: cancelled_ts.as_deref().map(parse_ts).transpose()?,
    })
}

#[cfg(test)]
mod tests;

impl Storage {
    pub(crate) async fn scoped_monitors(
        &self,
        caller: &super::ThreadCaller,
    ) -> anyhow::Result<Vec<MonitorRecord>> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_caller(&c, caller)?;
        let ids=c.prepare("WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id) SELECT m.id FROM monitors m JOIN scope s ON s.id=m.thread_id ORDER BY m.created_ts,m.id LIMIT 100")?.query_map([caller.thread_id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| get_monitor(&c, &id).map_err(Into::into))
            .collect()
    }
    pub(crate) async fn cancel_scoped_monitor(
        &self,
        caller: &super::ThreadCaller,
        id: &str,
    ) -> anyhow::Result<MonitorRecord> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_caller(&c, caller)?;
        let record = get_monitor(&c, id)?;
        super::thread_scope::authorize(&c, caller.thread_id, record.thread_id)?;
        c.execute("UPDATE monitors SET cancelled_ts=COALESCE(cancelled_ts,?2),last_event_ts=?2,summary='cancelled' WHERE id=?1",params![id,Utc::now().to_rfc3339()])?;
        Ok(get_monitor(&c, id)?)
    }
}

impl Storage {
    pub(crate) async fn background_monitor(
        &self,
        history: &str,
        thread_id: u64,
        id: &str,
    ) -> anyhow::Result<Option<MonitorRecord>> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, history)?;
        let record = get_monitor_optional(&c, id)?;
        anyhow::ensure!(
            record.as_ref().is_none_or(|r| r.thread_id == thread_id),
            "monitor is unavailable in this Thread"
        );
        Ok(record)
    }
    pub(crate) async fn record_background_monitor_tick(
        &self,
        history: &str,
        thread_id: u64,
        id: &str,
        output: String,
        summary: String,
    ) -> anyhow::Result<Option<MonitorRecord>> {
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, history)?;
        let record = get_monitor_optional(&c, id)?;
        anyhow::ensure!(
            record.as_ref().is_none_or(|r| r.thread_id == thread_id),
            "monitor is unavailable in this Thread"
        );
        c.execute("UPDATE monitors SET last_run_ts=?2,last_event_ts=?2,last_output=?3,summary=?4 WHERE id=?1 AND cancelled_ts IS NULL",params![id,Utc::now().to_rfc3339(),output,summary])?;
        get_monitor_optional(&c, id).map_err(Into::into)
    }
}
