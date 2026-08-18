//! Persisted subagent processes and restart recovery.

use super::Storage;
use super::common::{collect_rows, parse_ts};
use crate::processes::ProcessRecord;
use crate::processes::ProcessStatus;
use chrono::Utc;
use hirsel_drivers::AgentKind;
use hirsel_drivers::SessionHandle;
use hirsel_drivers::SubagentEvent;
use rusqlite::params;
use rusqlite::types::Type;

impl Storage {
    pub async fn upsert_subagent_process(&self, record: &ProcessRecord) -> anyhow::Result<()> {
        let events = serde_json::to_string(&record.events)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO subagent_processes (
                id,
                agent,
                model,
                handle_id,
                handle_agent,
                prompt,
                cwd,
                external_id,
                status,
                events,
                started_ts,
                last_event_ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                agent = excluded.agent,
                model = excluded.model,
                handle_id = excluded.handle_id,
                handle_agent = excluded.handle_agent,
                prompt = excluded.prompt,
                cwd = excluded.cwd,
                external_id = excluded.external_id,
                status = excluded.status,
                events = excluded.events,
                started_ts = excluded.started_ts,
                last_event_ts = excluded.last_event_ts
            ",
            params![
                record.id,
                agent_kind_to_str(record.agent),
                record.model,
                record.handle.id,
                agent_kind_to_str(record.handle.agent),
                record.prompt,
                record.cwd,
                record.external_id,
                process_status_to_str(record.status),
                events,
                record.started_ts.to_rfc3339(),
                record.last_event_ts.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub async fn restore_subagent_processes_after_restart(
        &self,
    ) -> anyhow::Result<SubagentRestore> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let mut records = {
            let mut stmt = conn.prepare(
                "
                SELECT id, agent, model, handle_id, handle_agent, prompt, cwd, external_id,
                       status, events, started_ts, last_event_ts
                FROM subagent_processes
                ORDER BY started_ts ASC, id ASC
                ",
            )?;
            let rows = stmt.query_map([], subagent_process_from_row)?;
            collect_rows(rows)?
        };
        let mut abandoned = Vec::new();
        for record in &mut records {
            if record.status != ProcessStatus::Running {
                continue;
            }
            record.status = ProcessStatus::Abandoned;
            record.last_event_ts = now;
            abandoned.push(record.id.clone());
            conn.execute(
                "
                UPDATE subagent_processes
                SET status = 'abandoned',
                    last_event_ts = ?2
                WHERE id = ?1
                ",
                params![record.id, now.to_rfc3339()],
            )?;
        }
        Ok(SubagentRestore { records, abandoned })
    }
}

#[derive(Debug, Clone)]
pub struct SubagentRestore {
    pub records: Vec<ProcessRecord>,
    pub abandoned: Vec<String>,
}

fn subagent_process_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessRecord> {
    let agent: String = row.get(1)?;
    let handle_agent: String = row.get(4)?;
    let status: String = row.get(8)?;
    let events: String = row.get(9)?;
    let started_ts: String = row.get(10)?;
    let last_event_ts: String = row.get(11)?;
    Ok(ProcessRecord::restored(
        row.get(0)?,
        agent_kind_from_str(&agent)?,
        row.get(2)?,
        SessionHandle {
            id: row.get(3)?,
            agent: agent_kind_from_str(&handle_agent)?,
        },
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        process_status_from_str(&status)?,
        serde_json::from_str::<Vec<SubagentEvent>>(&events).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
        })?,
        parse_ts(&started_ts)?,
        parse_ts(&last_event_ts)?,
    ))
}

fn agent_kind_to_str(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
    }
}

fn agent_kind_from_str(value: &str) -> rusqlite::Result<AgentKind> {
    match value {
        "claude" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn process_status_to_str(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Running => "running",
        ProcessStatus::Done => "done",
        ProcessStatus::Failed => "failed",
        ProcessStatus::Interrupted => "interrupted",
        ProcessStatus::Abandoned => "abandoned",
    }
}

fn process_status_from_str(value: &str) -> rusqlite::Result<ProcessStatus> {
    match value {
        "running" => Ok(ProcessStatus::Running),
        "done" => Ok(ProcessStatus::Done),
        "failed" => Ok(ProcessStatus::Failed),
        "interrupted" => Ok(ProcessStatus::Interrupted),
        "abandoned" => Ok(ProcessStatus::Abandoned),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[cfg(test)]
mod tests;
