use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use hirsel_drivers::{AgentKind, SessionHandle, SubagentEvent, TerminalOutcome};
use serde::Serialize;
use uuid::Uuid;

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
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Done,
    Failed,
    Interrupted,
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
        };
        self.lock()?.insert(id.clone(), record);
        Ok(id)
    }

    pub fn push_event(&self, process_id: &str, event: SubagentEvent) -> anyhow::Result<()> {
        let mut inner = self.lock()?;
        if let Some(record) = inner.get_mut(process_id) {
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
        }
        Ok(())
    }

    pub fn list(&self) -> anyhow::Result<Vec<ProcessRecord>> {
        let mut values = self.lock()?.values().cloned().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(values)
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
