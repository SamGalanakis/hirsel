use std::path::PathBuf;

use futures_util::StreamExt;
use hirsel_drivers::{AgentKind, SessionHandle, SpawnSpec, SubagentEvent};

use super::{ProcessTerminal, SpawnedProcess, ToolSuite, publish_process_upsert};

impl ToolSuite {
    pub async fn subagents_spawn(
        &self,
        agent: AgentKind,
        model: Option<String>,
        variant: Option<String>,
        prompt: impl Into<String>,
        cwd: PathBuf,
    ) -> anyhow::Result<SpawnedProcess> {
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        self.subagents_spawn_with_process_id(agent, model, variant, prompt, cwd, process_id)
            .await
    }

    pub async fn subagents_spawn_with_process_id(
        &self,
        agent: AgentKind,
        model: Option<String>,
        variant: Option<String>,
        prompt: impl Into<String>,
        cwd: PathBuf,
        process_id: String,
    ) -> anyhow::Result<SpawnedProcess> {
        let resolved = self
            .subagent_models
            .resolve(agent, model.as_deref(), variant.as_deref())?;
        let model = Some(resolved.model_id);
        let variant = Some(resolved.variant);
        let prompt = prompt.into();
        let driver = self.driver_for(agent);
        let handle = driver
            .spawn(SpawnSpec {
                agent,
                model: model.clone(),
                variant,
                prompt: prompt.clone(),
                cwd: cwd.clone(),
                fake_fixture: self.config.fake_fixture.clone(),
            })
            .await?;
        let process_id = self.processes.insert_with_id(
            process_id,
            agent,
            model.clone(),
            handle.clone(),
            prompt,
            cwd.to_string_lossy().into_owned(),
        )?;
        if let Some(record) = self.processes.get(&process_id)? {
            self.storage.upsert_subagent_process(&record).await?;
        }
        if let Some(process) = self.processes.info(&process_id)? {
            self.broadcast_process_upsert(process);
        }
        let mut events = driver.events(&handle)?;
        let processes = self.processes.clone();
        let storage = self.storage.clone();
        let broadcaster = self.broadcaster.clone();
        let broadcast_log = self.broadcast_log.clone();
        let terminal_events = self.terminal_events.clone();
        let driver_for_task = driver.clone();
        let process_id_for_task = process_id.clone();
        let handle_for_task = handle.clone();
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let terminal = match &event {
                    SubagentEvent::Terminal { outcome } => Some(outcome.clone()),
                    _ => None,
                };
                match processes.push_event(&process_id_for_task, event) {
                    Ok(Some(update)) => {
                        if let Err(error) = storage.upsert_subagent_process(&update.record).await {
                            tracing::warn!(%error, "failed to persist Sub-agent process");
                        }
                        if update.should_broadcast {
                            publish_process_upsert(&broadcast_log, &broadcaster, update.info);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "failed to record Sub-agent event"),
                }
                if let Some(outcome) = terminal {
                    if let Err(error) = driver_for_task.retire(&handle_for_task).await {
                        tracing::warn!(%error, "failed to retire Sub-agent driver session");
                    }
                    terminal_events.publish(ProcessTerminal {
                        process_id: process_id_for_task.clone(),
                        handle: handle_for_task.clone(),
                        outcome,
                    });
                    break;
                }
            }
        });
        Ok(SpawnedProcess {
            process_id,
            model,
            handle,
        })
    }

    pub async fn subagents_prompt(
        &self,
        handle: &SessionHandle,
        text: String,
    ) -> anyhow::Result<()> {
        self.driver_for(handle.agent).prompt(handle, text).await?;
        Ok(())
    }

    pub async fn subagents_prompt_process(
        &self,
        process_id: &str,
        text: String,
    ) -> anyhow::Result<()> {
        let record = self
            .processes
            .get(process_id)?
            .ok_or_else(|| anyhow::anyhow!("Sub-agent process not found: {process_id}"))?;
        self.subagents_prompt(&record.handle, text).await
    }

    pub async fn subagents_interrupt(&self, handle: &SessionHandle) -> anyhow::Result<()> {
        self.driver_for(handle.agent).interrupt(handle).await?;
        Ok(())
    }

    pub async fn subagents_interrupt_process(&self, process_id: &str) -> anyhow::Result<()> {
        let record = self
            .processes
            .get(process_id)?
            .ok_or_else(|| anyhow::anyhow!("Sub-agent process not found: {process_id}"))?;
        self.subagents_interrupt(&record.handle).await
    }

    pub async fn subagents_abandon_process(&self, process_id: &str) -> anyhow::Result<()> {
        let record = self
            .processes
            .get(process_id)?
            .ok_or_else(|| anyhow::anyhow!("Sub-agent process not found: {process_id}"))?;
        let driver = self.driver_for(record.handle.agent);
        if let Err(error) = driver.interrupt(&record.handle).await {
            tracing::debug!(%error, process_id, "Sub-agent interrupt during abandon failed");
        }
        driver.retire(&record.handle).await?;
        if let Some(update) = self.processes.abandon(process_id)? {
            self.storage.upsert_subagent_process(&update.record).await?;
            if update.should_broadcast {
                self.broadcast_process_upsert(update.info);
            }
        }
        Ok(())
    }

    pub fn subagents_list(&self) -> anyhow::Result<Vec<crate::processes::ProcessRecord>> {
        self.processes.list()
    }

    pub fn subagents_progress(&self, process_id: &str) -> anyhow::Result<Vec<SubagentEvent>> {
        self.processes.recent_events(process_id)
    }

    pub fn subagents_process(
        &self,
        process_id: &str,
    ) -> anyhow::Result<Option<crate::processes::ProcessRecord>> {
        self.processes.get(process_id)
    }
}
