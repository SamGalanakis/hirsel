use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use hirsel_drivers::{DriverError, DriverResult, EventStream};
use tokio::sync::Notify;

#[derive(Default)]
struct HandoffDriver {
    fail_events: bool,
    fail_retire: bool,
    live: AtomicBool,
    retire_calls: AtomicUsize,
    retired: Notify,
}

#[async_trait]
impl SubagentDriver for HandoffDriver {
    async fn spawn(&self, spec: SpawnSpec) -> DriverResult<SessionHandle> {
        self.live.store(true, Ordering::SeqCst);
        Ok(SessionHandle {
            id: "handoff-session".into(),
            agent: spec.agent,
        })
    }
    async fn prompt(&self, _: &SessionHandle, _: String) -> DriverResult<()> {
        Ok(())
    }
    async fn interrupt(&self, _: &SessionHandle) -> DriverResult<()> {
        Ok(())
    }
    async fn retire(&self, _: &SessionHandle) -> DriverResult<()> {
        self.live.store(false, Ordering::SeqCst);
        self.retire_calls.fetch_add(1, Ordering::SeqCst);
        self.retired.notify_one();
        if self.fail_retire {
            Err(DriverError::MissingPipe("injected retirement error"))
        } else {
            Ok(())
        }
    }
    fn events(&self, _: &SessionHandle) -> DriverResult<EventStream> {
        if self.fail_events {
            Err(DriverError::MissingPipe(
                "injected event subscription error",
            ))
        } else {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }
}

fn spec(cwd: &std::path::Path) -> SpawnSpec {
    SpawnSpec {
        agent: AgentKind::Codex,
        model: None,
        variant: None,
        prompt: "host handoff probe".into(),
        cwd: cwd.to_owned(),
        fake_fixture: None,
    }
}

async fn tools_fixture() -> (ToolSuite, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::storage::Storage::open(dir.path()).await.unwrap();
    let (broadcaster, _) = tokio::sync::broadcast::channel(128);
    let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = crate::BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views =
        crate::templates::ViewManager::new(templates, broadcaster.clone(), broadcast_log.clone());
    let config = crate::host_config::ConfigStore::load(
        dir.path().join("hirsel.toml"),
        dir.path(),
        std::path::Path::new("/docs/hirsel-config.md"),
        &crate::host_config::EnvBootstrap::default(),
    )
    .await
    .unwrap();
    let tools = ToolSuite::new(
        crate::tools::ToolsConfig {
            driver_mode: crate::config::DriverMode::Fake,
            fake_fixture: None,
            subagent_models: crate::subagent_models::SubagentModelState::load(config),
        },
        storage,
        broadcaster,
        broadcast_log,
        crate::processes::ProcessStore::default(),
        pushes,
        views,
    );
    (tools, dir)
}

#[tokio::test]
async fn terminal_handoff_subscription_failure_retires_without_publishing_a_record() {
    let (tools, dir) = tools_fixture().await;
    let driver = Arc::new(HandoffDriver {
        fail_events: true,
        fail_retire: true,
        ..Default::default()
    });
    let error = tools
        .spawn_subagent_with_driver(
            driver.clone(),
            spec(dir.path()),
            "failed-subscription".into(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected event subscription error")
    );
    assert!(!driver.live.load(Ordering::SeqCst));
    assert_eq!(driver.retire_calls.load(Ordering::SeqCst), 1);
    assert!(tools.subagents_list().unwrap().is_empty());
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    let rows: usize = conn
        .query_row("SELECT COUNT(*) FROM subagent_processes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn terminal_handoff_persistence_failure_retires_and_keeps_only_failed_history() {
    let (tools, dir) = tools_fixture().await;
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_running_subagent BEFORE INSERT ON subagent_processes WHEN NEW.status = 'running' BEGIN SELECT RAISE(ABORT, 'injected running persistence failure'); END;").unwrap();
    let driver = Arc::new(HandoffDriver::default());
    let error = tools
        .spawn_subagent_with_driver(
            driver.clone(),
            spec(dir.path()),
            "failed-persistence".into(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected running persistence failure")
    );
    assert!(!driver.live.load(Ordering::SeqCst));
    assert_eq!(driver.retire_calls.load(Ordering::SeqCst), 1);
    let record = tools
        .subagents_process("failed-persistence")
        .unwrap()
        .unwrap();
    assert_eq!(record.status, crate::processes::ProcessStatus::Failed);
    let persisted: String = conn
        .query_row(
            "SELECT status FROM subagent_processes WHERE id = 'failed-persistence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, "failed");
}

#[tokio::test]
async fn terminal_handoff_cancellation_finishes_retirement_and_persistence() {
    let (tools, dir) = tools_fixture().await;
    let driver = Arc::new(HandoffDriver::default());
    let handle = driver.spawn(spec(dir.path())).await.unwrap();
    tools
        .processes
        .insert_with_id(
            "cancelled-handoff".into(),
            AgentKind::Codex,
            None,
            handle.clone(),
            "cancel".into(),
            dir.path().to_string_lossy().into_owned(),
        )
        .unwrap();
    let record = tools.processes.get("cancelled-handoff").unwrap().unwrap();
    tools
        .storage
        .upsert_subagent_process(&record)
        .await
        .unwrap();
    // Cancel the owning future at the boundary after a host record exists but
    // before the event pump takes ownership. This is the production guard.
    let startup = SubagentStartup {
        tools: tools.clone(),
        driver: driver.clone(),
        handle: Some(handle),
        process_id: Some("cancelled-handoff".into()),
    };
    let (ready, started) = tokio::sync::oneshot::channel();
    let owner = tokio::spawn(async move {
        let _startup = startup;
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    started.await.unwrap();
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), driver.retired.notified())
        .await
        .unwrap();
    assert!(!driver.live.load(Ordering::SeqCst));
    assert_eq!(driver.retire_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        tools
            .subagents_process("cancelled-handoff")
            .unwrap()
            .unwrap()
            .status,
        crate::processes::ProcessStatus::Failed
    );
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let status: String = conn
                .query_row(
                    "SELECT status FROM subagent_processes WHERE id = 'cancelled-handoff'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if status == "failed" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
