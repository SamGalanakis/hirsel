//! Fixture-driven driver used by tests and offline runs.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::time::{Duration, sleep};
use uuid::Uuid;

use crate::{
    shared::{EventHub, lock, short_line},
    types::{
        DriverError, DriverResult, EventStream, SessionHandle, SpawnSpec, SubagentDriver,
        SubagentEvent, TerminalOutcome,
    },
};

#[derive(Default)]
pub struct FakeDriver {
    sessions: Mutex<HashMap<String, Arc<FakeSession>>>,
    spawned: Mutex<Vec<SpawnSpec>>,
}

struct FakeSession {
    events: Arc<EventHub>,
    interrupted: AtomicBool,
    terminal_sent: AtomicBool,
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
            interrupted: AtomicBool::new(false),
            terminal_sent: AtomicBool::new(false),
        });
        lock(&self.sessions)?.insert(handle.id.clone(), session.clone());

        tokio::spawn(async move {
            let _ = events.emit(SubagentEvent::Started {
                external_id: fixture.external_id,
            });
            for progress in fixture.progress {
                if fixture.delay_ms > 0 {
                    sleep(Duration::from_millis(fixture.delay_ms)).await;
                }
                if session.interrupted.load(Ordering::SeqCst) {
                    if !session.terminal_sent.swap(true, Ordering::SeqCst) {
                        let _ = events.emit(SubagentEvent::Terminal {
                            outcome: TerminalOutcome::Interrupted,
                        });
                    }
                    return;
                }
                let _ = events.emit(SubagentEvent::Progress {
                    summary: short_line(progress),
                });
            }
            if fixture.delay_ms > 0 {
                sleep(Duration::from_millis(fixture.delay_ms)).await;
            }
            if session.interrupted.load(Ordering::SeqCst) {
                if !session.terminal_sent.swap(true, Ordering::SeqCst) {
                    let _ = events.emit(SubagentEvent::Terminal {
                        outcome: TerminalOutcome::Interrupted,
                    });
                }
            } else if !session.terminal_sent.swap(true, Ordering::SeqCst) {
                let _ = events.emit(SubagentEvent::Terminal {
                    outcome: fixture.terminal,
                });
            }
        });

        Ok(handle)
    }

    async fn prompt(&self, handle: &SessionHandle, text: String) -> DriverResult<()> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        let _ = session.events.emit(SubagentEvent::Progress {
            summary: short_line(format!("prompt: {text}")),
        });
        Ok(())
    }

    async fn interrupt(&self, handle: &SessionHandle) -> DriverResult<()> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        session.interrupted.store(true, Ordering::SeqCst);
        if !session.terminal_sent.swap(true, Ordering::SeqCst) {
            let _ = session.events.emit(SubagentEvent::Terminal {
                outcome: TerminalOutcome::Interrupted,
            });
        }
        Ok(())
    }

    async fn retire(&self, handle: &SessionHandle) -> DriverResult<()> {
        lock(&self.sessions)?.remove(&handle.id);
        Ok(())
    }

    fn events(&self, handle: &SessionHandle) -> DriverResult<EventStream> {
        let sessions = lock(&self.sessions)?;
        let session = sessions
            .get(&handle.id)
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))?;
        session.events.stream()
    }
}

impl FakeDriver {
    pub fn spawned_specs(&self) -> DriverResult<Vec<SpawnSpec>> {
        Ok(lock(&self.spawned)?.clone())
    }
}
