//! Fixture-driven driver used by tests and offline runs.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::time::{Duration, sleep};
use uuid::Uuid;

use crate::{
    shared::{EventHub, SessionRegistry, lock, short_line, terminal_message},
    types::{
        DriverError, DriverResult, EventStream, SessionHandle, SpawnSpec, SubagentDriver,
        SubagentEvent, TerminalOutcome,
    },
};

#[derive(Default)]
pub struct FakeDriver {
    sessions: SessionRegistry<FakeSession>,
    spawned: Mutex<Vec<SpawnSpec>>,
}

struct FakeSession {
    events: Arc<EventHub>,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeFixture {
    #[serde(default = "default_fake_external_id")]
    external_id: String,
    #[serde(default = "default_fake_progress")]
    progress: Vec<String>,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default = "default_fake_terminal")]
    terminal: TerminalOutcome,
    #[serde(default)]
    assistant_output: Option<String>,
}

fn default_fake_external_id() -> String {
    "fake-external-session".to_string()
}

fn default_fake_progress() -> Vec<String> {
    vec![
        "fake driver started".to_string(),
        "fake driver working".to_string(),
    ]
}

fn default_fake_terminal() -> TerminalOutcome {
    TerminalOutcome::Done {
        summary: "fake driver completed".to_string(),
    }
}

impl Default for FakeFixture {
    fn default() -> Self {
        Self {
            external_id: default_fake_external_id(),
            progress: default_fake_progress(),
            delay_ms: 10,
            terminal: default_fake_terminal(),
            assistant_output: Some("fake driver completed".into()),
        }
    }
}

#[async_trait]
impl SubagentDriver for FakeDriver {
    async fn spawn(&self, task: SpawnSpec) -> DriverResult<SessionHandle> {
        lock(&self.spawned)?.push(task.clone());
        let fixture = match task.fake_fixture {
            Some(path) => serde_json::from_str(&tokio::fs::read_to_string(path).await?)?,
            None => FakeFixture::default(),
        };
        let handle = SessionHandle {
            id: Uuid::new_v4().to_string(),
            agent: task.agent,
        };
        let events = EventHub::new(128);
        let session = Arc::new(FakeSession {
            events: events.clone(),
        });
        self.sessions.insert(handle.id.clone(), session.clone())?;

        tokio::spawn(async move {
            let _ = events.emit(SubagentEvent::Started {
                external_id: fixture.external_id,
            });
            for progress in fixture.progress {
                if fixture.delay_ms > 0 {
                    sleep(Duration::from_millis(fixture.delay_ms)).await;
                }
                if events.is_terminal() {
                    return;
                }
                let _ = events.emit(SubagentEvent::Progress {
                    summary: short_line(progress),
                });
            }
            if fixture.delay_ms > 0 {
                sleep(Duration::from_millis(fixture.delay_ms)).await;
            }
            let outcome = match fixture.terminal {
                TerminalOutcome::Done { summary } => TerminalOutcome::Done {
                    summary: terminal_message(summary),
                },
                TerminalOutcome::Failed { reason } => TerminalOutcome::Failed {
                    reason: terminal_message(reason),
                },
                TerminalOutcome::Interrupted => TerminalOutcome::Interrupted,
            };
            let _ = events.complete(outcome, fixture.assistant_output);
        });

        Ok(handle)
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        if session.events.is_terminal() {
            return Err(DriverError::SessionClosed);
        }
        session.events.emit(SubagentEvent::Progress {
            summary: short_line(format!("prompt: {text}")),
        })
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let session = self.sessions.get(handle)?;
        if session.events.is_terminal() {
            return Err(DriverError::SessionClosed);
        }
        session
            .events
            .complete(TerminalOutcome::Interrupted, None)?;
        session.events.wait_terminal().await
    }

    async fn retire(&self, handle: &SessionHandle) -> DriverResult<()> {
        if let Some(session) = self.sessions.remove(handle)? {
            session
                .events
                .complete(TerminalOutcome::Interrupted, None)?;
        }
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let session = self.sessions.get(handle)?;
        session.events.stream()
    }
}

impl FakeDriver {
    pub fn spawned_specs(&self) -> DriverResult<Vec<SpawnSpec>> {
        Ok(lock(&self.spawned)?.clone())
    }
}
