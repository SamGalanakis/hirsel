use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use hirsel_drivers::{AgentKind, SessionHandle, SubagentEvent, TerminalOutcome};
use hirsel_proto::{ProcessInfo, ProcessKind, ProcessState};
use serde::Serialize;
use uuid::Uuid;

const SUMMARY_BROADCAST_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Default)]
pub struct ProcessStore {
    inner: Arc<Mutex<HashMap<String, ProcessRecord>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessRecord {
    pub id: String,
    pub agent: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub handle: SessionHandle,
    pub prompt: String,
    pub cwd: String,
    pub external_id: Option<String>,
    pub status: ProcessStatus,
    pub events: Vec<SubagentEvent>,
    pub started_ts: DateTime<Utc>,
    pub last_event_ts: DateTime<Utc>,
    #[serde(skip)]
    last_upsert_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Done,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone)]
pub struct RecordedProcessUpdate {
    pub info: ProcessInfo,
    pub should_broadcast: bool,
}

impl ProcessStore {
    pub fn insert(
        &self,
        agent: AgentKind,
        model: Option<String>,
        handle: SessionHandle,
        prompt: String,
        cwd: String,
    ) -> anyhow::Result<String> {
        let id = format!("proc-{}", Uuid::new_v4());
        self.insert_with_id(id, agent, model, handle, prompt, cwd)
    }

    pub fn insert_with_id(
        &self,
        id: String,
        agent: AgentKind,
        model: Option<String>,
        handle: SessionHandle,
        prompt: String,
        cwd: String,
    ) -> anyhow::Result<String> {
        let record = ProcessRecord {
            id: id.clone(),
            agent,
            model,
            handle,
            prompt,
            cwd,
            external_id: None,
            status: ProcessStatus::Running,
            events: Vec::new(),
            started_ts: Utc::now(),
            last_event_ts: Utc::now(),
            last_upsert_at: None,
        };
        self.lock()?.insert(id.clone(), record);
        Ok(id)
    }

    pub fn push_event(
        &self,
        process_id: &str,
        event: SubagentEvent,
    ) -> anyhow::Result<Option<RecordedProcessUpdate>> {
        let mut inner = self.lock()?;
        if let Some(record) = inner.get_mut(process_id) {
            let previous_status = record.status;
            match &event {
                SubagentEvent::Started { external_id } => {
                    record.external_id = Some(external_id.clone());
                }
                SubagentEvent::Terminal { outcome } => {
                    record.status = match outcome {
                        TerminalOutcome::Done { .. } => ProcessStatus::Done,
                        TerminalOutcome::Failed { .. } => ProcessStatus::Failed,
                        TerminalOutcome::Interrupted => ProcessStatus::Interrupted,
                    };
                }
                SubagentEvent::Progress { .. } => {}
            }
            record.events.push(event);
            record.last_event_ts = Utc::now();
            let state_changed = record.status != previous_status;
            let now = Instant::now();
            let should_broadcast = state_changed
                || record
                    .last_upsert_at
                    .map(|last| now.duration_since(last) >= SUMMARY_BROADCAST_INTERVAL)
                    .unwrap_or(true);
            if should_broadcast {
                record.last_upsert_at = Some(now);
            }
            return Ok(Some(RecordedProcessUpdate {
                info: process_info(record),
                should_broadcast,
            }));
        }
        Ok(None)
    }

    pub fn list(&self) -> anyhow::Result<Vec<ProcessRecord>> {
        let mut values = self.lock()?.values().cloned().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(values)
    }

    pub fn info(&self, process_id: &str) -> anyhow::Result<Option<ProcessInfo>> {
        Ok(self.lock()?.get(process_id).map(process_info))
    }

    pub fn snapshot(&self) -> anyhow::Result<Vec<ProcessInfo>> {
        let mut non_terminal = Vec::new();
        let mut terminal = Vec::new();
        for record in self.lock()?.values() {
            if matches!(record.status, ProcessStatus::Running) {
                non_terminal.push(record.clone());
            } else {
                terminal.push(record.clone());
            }
        }
        non_terminal.sort_by(|left, right| {
            left.started_ts
                .cmp(&right.started_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        terminal.sort_by(|left, right| {
            left.last_event_ts
                .cmp(&right.last_event_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        if terminal.len() > 10 {
            terminal.drain(..terminal.len() - 10);
        }
        Ok(non_terminal
            .iter()
            .chain(terminal.iter())
            .map(process_info)
            .collect())
    }

    pub fn get(&self, process_id: &str) -> anyhow::Result<Option<ProcessRecord>> {
        Ok(self.lock()?.get(process_id).cloned())
    }

    pub fn recent_events(&self, process_id: &str) -> anyhow::Result<Vec<SubagentEvent>> {
        Ok(self
            .lock()?
            .get(process_id)
            .map(|record| {
                record
                    .events
                    .iter()
                    .rev()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn reset(&self) -> anyhow::Result<()> {
        self.lock()?.clear();
        Ok(())
    }

    fn lock(&self) -> anyhow::Result<MutexGuard<'_, HashMap<String, ProcessRecord>>> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("process store lock was poisoned"))
    }
}

fn process_info(record: &ProcessRecord) -> ProcessInfo {
    ProcessInfo {
        id: record.id.clone(),
        kind: ProcessKind::Subagent,
        label: short_label(&record.prompt),
        agent: Some(agent_kind_name(record.agent).to_string()),
        model: record.model.clone(),
        state: process_state(record.status),
        started_ts: record.started_ts,
        last_event_ts: record.last_event_ts,
        summary: record.events.last().map(subagent_event_summary),
    }
}

fn process_state(status: ProcessStatus) -> ProcessState {
    match status {
        ProcessStatus::Running => ProcessState::Running,
        ProcessStatus::Done => ProcessState::Done,
        ProcessStatus::Failed => ProcessState::Failed,
        ProcessStatus::Interrupted => ProcessState::Cancelled,
    }
}

fn subagent_event_summary(event: &SubagentEvent) -> String {
    match event {
        SubagentEvent::Started { external_id } => short_label(&format!("started {external_id}")),
        SubagentEvent::Progress { summary } => short_label(summary),
        SubagentEvent::Terminal { outcome } => match outcome {
            TerminalOutcome::Done { summary } => short_label(summary),
            TerminalOutcome::Failed { reason } => short_label(reason),
            TerminalOutcome::Interrupted => "interrupted".to_string(),
        },
    }
}

fn agent_kind_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
    }
}

fn short_label(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 80;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}
