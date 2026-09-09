use std::{path::PathBuf, sync::Arc};

use futures_util::StreamExt;
use hirsel_drivers::{
    AgentKind, SessionHandle, SpawnSpec, SubagentDriver, SubagentEvent, TerminalOutcome,
};

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
        self.spawn_subagent_with_driver(
            self.driver_for(agent),
            SpawnSpec {
                agent,
                model,
                variant,
                prompt,
                cwd,
                fake_fixture: self.config.fake_fixture.clone(),
            },
            process_id,
        )
        .await
    }

    async fn spawn_subagent_with_driver(
        &self,
        driver: Arc<dyn SubagentDriver>,
        spec: SpawnSpec,
        process_id: String,
    ) -> anyhow::Result<SpawnedProcess> {
        let handle = driver.spawn(spec.clone()).await?;
        let mut startup = SubagentStartup {
            tools: self.clone(),
            driver: Arc::clone(&driver),
            handle: Some(handle.clone()),
            process_id: None,
        };
        let prepare = async {
            // Subscribe before publishing any host record. A provider that
            // finished during spawn is replayed by the driver's event hub.
            let events = driver.events(&handle)?;
            self.processes.insert_with_id(
                process_id.clone(),
                spec.agent,
                spec.model.clone(),
                handle.clone(),
                spec.prompt,
                spec.cwd.to_string_lossy().into_owned(),
            )?;
            startup.process_id = Some(process_id.clone());
            if let Some(record) = self.processes.get(&process_id)? {
                self.storage.upsert_subagent_process(&record).await?;
            }
            if let Some(process) = self.processes.info(&process_id)? {
                self.broadcast_process_upsert(process);
            }
            Ok::<_, anyhow::Error>(events)
        }
        .await;
        let mut events = match prepare {
            Ok(events) => events,
            Err(error) => {
                if let Some(cleanup) =
                    startup.rollback(format!("Sub-agent startup failed: {error}"))
                {
                    let _ = cleanup.await;
                }
                return Err(error);
            }
        };
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
        // The pump now owns retirement; no await separates installation from
        // disarming startup rollback.
        startup.handle.take();
        Ok(SpawnedProcess {
            process_id,
            model: spec.model,
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

/// Acquired driver ownership lasts through every fallible host handoff step.
/// Cancellation uses the same rollback as an ordinary error; its cleanup task
/// is independent of the cancelled caller so retirement can finish.
struct SubagentStartup {
    tools: ToolSuite,
    driver: Arc<dyn SubagentDriver>,
    handle: Option<SessionHandle>,
    process_id: Option<String>,
}

impl SubagentStartup {
    fn rollback(&mut self, reason: String) -> Option<tokio::task::JoinHandle<()>> {
        let handle = self.handle.take()?;
        let driver = Arc::clone(&self.driver);
        let tools = self.tools.clone();
        let update = self.process_id.as_deref().and_then(|process_id| {
            match tools.processes.push_event(process_id, SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed { reason },
            }) {
                Ok(update) => update,
                Err(error) => {
                    tracing::warn!(%error, process_id, "failed to record Sub-agent startup rollback");
                    None
                }
            }
        });
        if let Some(update) = &update {
            tools.broadcast_process_upsert(update.info.clone());
        }
        Some(tokio::spawn(async move {
            if let Err(error) = driver.retire(&handle).await {
                tracing::warn!(%error, handle_id = %handle.id, "failed to retire Sub-agent after startup failure");
            }
            if let Some(update) = update
                && let Err(error) = tools.storage.upsert_subagent_process(&update.record).await
            {
                tracing::warn!(%error, "failed to persist Sub-agent startup rollback");
            }
        }))
    }
}

impl Drop for SubagentStartup {
    fn drop(&mut self) {
        self.rollback("Sub-agent startup was cancelled before host handoff".into());
    }
}

#[cfg(test)]
#[path = "subagent_handoff_tests.rs"]
mod handoff_tests;
