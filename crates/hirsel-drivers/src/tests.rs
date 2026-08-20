use std::process::Stdio;

use futures_util::StreamExt;
use serde_json::json;
use tokio::{
    process::Command,
    time::{Duration, sleep, timeout},
};

use crate::{
    claude::{ClaudeCodeDriver, claude_terminal_outcome},
    codex::{CodexDriver, codex_agent_message, codex_terminal_outcome},
    fake::FakeDriver,
    shared::drain_stderr,
    types::{
        AgentKind, DriverError, SessionHandle, SpawnSpec, SubagentDriver, SubagentEvent,
        TerminalOutcome,
    },
};

#[test]
fn claude_terminal_preserves_long_final_message() {
    let final_message = format!("{}the actual ending", "research findings ".repeat(20));

    let outcome = claude_terminal_outcome(&json!({
        "type": "result",
        "is_error": false,
        "result": final_message,
    }));

    assert_eq!(
        outcome,
        TerminalOutcome::Done {
            summary: final_message,
        }
    );
}

#[test]
fn claude_failure_preserves_long_final_message() {
    let final_message = format!("{}the actual ending", "failure details ".repeat(20));

    let outcome = claude_terminal_outcome(&json!({
        "type": "result",
        "is_error": true,
        "terminal_reason": "failed",
        "result": final_message,
    }));

    assert_eq!(
        outcome,
        TerminalOutcome::Failed {
            reason: format!("failed: {final_message}"),
        }
    );
}

#[test]
fn terminal_message_cap_is_explicit_and_character_safe() {
    let final_message = "é".repeat(24_001);

    let outcome = claude_terminal_outcome(&json!({
        "type": "result",
        "is_error": false,
        "result": final_message,
    }));
    let TerminalOutcome::Done { summary } = outcome else {
        panic!("expected done outcome");
    };

    assert_eq!(summary.chars().count(), 24_000);
    assert!(summary.ends_with("…[truncated by hirsel at 24k chars]"));
}

#[test]
fn codex_terminal_uses_last_completed_agent_message() {
    let final_message = format!("{}the actual ending", "codex report ".repeat(30));
    let item = json!({
        "method": "item/completed",
        "params": {
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": final_message,
            }
        }
    });
    let terminal = json!({
        "method": "turn/completed",
        "params": { "turn": { "status": "completed" } }
    });
    let last_agent_message = codex_agent_message(&item).map(str::to_string);

    assert_eq!(
        codex_terminal_outcome(&terminal, last_agent_message.as_deref()),
        Some(TerminalOutcome::Done {
            summary: final_message,
        })
    );
}

#[tokio::test]
async fn fake_driver_emits_started_progress_and_done() {
    let driver = FakeDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Claude,
            model: None,
            variant: None,
            prompt: "fix it".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: None,
        })
        .await
        .unwrap();
    let mut events = driver.events(&handle).unwrap();

    let first = events.next().await.unwrap();
    assert!(matches!(first, SubagentEvent::Started { .. }));
    let mut saw_progress = false;
    loop {
        let event = timeout(Duration::from_secs(1), events.next())
            .await
            .unwrap()
            .unwrap();
        match event {
            SubagentEvent::Progress { .. } => saw_progress = true,
            SubagentEvent::Terminal {
                outcome: TerminalOutcome::Done { .. },
            } => break,
            other => panic!("unexpected fake event: {other:?}"),
        }
    }
    assert!(saw_progress);
}

#[tokio::test]
async fn fake_driver_records_requested_model() {
    let driver = FakeDriver::default();
    let _handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Codex,
            model: Some("gpt-test-model".to_string()),
            variant: Some("high".to_string()),
            prompt: "fix it".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: None,
        })
        .await
        .unwrap();

    let specs = driver.spawned_specs().unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].model.as_deref(), Some("gpt-test-model"));
    assert_eq!(specs[0].variant.as_deref(), Some("high"));
}

#[tokio::test]
async fn fake_driver_interrupt_is_terminal() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        fixture.path(),
        serde_json::to_string(&json!({
            "external_id": "fake-long",
            "progress": ["one", "two"],
            "delay_ms": 200,
            "terminal": { "status": "done", "summary": "should not happen" }
        }))
        .unwrap(),
    )
    .unwrap();
    let driver = FakeDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Codex,
            model: None,
            variant: None,
            prompt: "wait".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: Some(fixture.path().to_path_buf()),
        })
        .await
        .unwrap();
    let mut events = driver.events(&handle).unwrap();
    let _ = events.next().await.unwrap();
    driver.interrupt(&handle).await.unwrap();

    let event = timeout(Duration::from_secs(1), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        event,
        SubagentEvent::Terminal {
            outcome: TerminalOutcome::Interrupted
        }
    );
}

#[tokio::test]
async fn fake_driver_rejects_unknown_sessions() {
    let driver = FakeDriver::default();
    let handle = SessionHandle {
        id: "missing-session".to_string(),
        agent: AgentKind::Codex,
    };

    assert!(matches!(
        driver.prompt(&handle, "keep going".to_string()).await,
        Err(DriverError::SessionNotFound(id)) if id == handle.id
    ));
    assert!(matches!(
        driver.interrupt(&handle).await,
        Err(DriverError::SessionNotFound(id)) if id == handle.id
    ));
    assert!(matches!(
        driver.events(&handle),
        Err(DriverError::SessionNotFound(id)) if id == handle.id
    ));
}

#[tokio::test]
async fn fake_driver_retire_ends_an_interrupted_session() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        fixture.path(),
        serde_json::to_string(&json!({
            "external_id": "fake-retired",
            "progress": ["one", "two"],
            "delay_ms": 200,
            "terminal": { "status": "done", "summary": "should not happen" }
        }))
        .unwrap(),
    )
    .unwrap();
    let driver = FakeDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Claude,
            model: None,
            variant: None,
            prompt: "wait".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: Some(fixture.path().to_path_buf()),
        })
        .await
        .unwrap();
    let mut events = driver.events(&handle).unwrap();
    assert!(matches!(
        events.next().await,
        Some(SubagentEvent::Started { .. })
    ));

    driver.interrupt(&handle).await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), events.next())
            .await
            .unwrap(),
        Some(SubagentEvent::Terminal {
            outcome: TerminalOutcome::Interrupted,
        })
    );
    driver.retire(&handle).await.unwrap();

    assert!(matches!(
        driver.events(&handle),
        Err(DriverError::SessionNotFound(id)) if id == handle.id
    ));
}

#[tokio::test]
async fn fake_driver_replays_instant_terminal_to_late_subscriber() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        fixture.path(),
        serde_json::to_string(&json!({
            "external_id": "instant",
            "progress": [],
            "delay_ms": 0,
            "terminal": { "status": "done", "summary": "instant done" }
        }))
        .unwrap(),
    )
    .unwrap();
    let driver = FakeDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Claude,
            model: None,
            variant: None,
            prompt: "instant".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: Some(fixture.path().to_path_buf()),
        })
        .await
        .unwrap();
    sleep(Duration::from_millis(50)).await;
    let mut events = driver.events(&handle).unwrap();

    let first = timeout(Duration::from_secs(1), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first,
        SubagentEvent::Started {
            external_id: "instant".to_string()
        }
    );
    let terminal = timeout(Duration::from_secs(1), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        terminal,
        SubagentEvent::Terminal {
            outcome: TerminalOutcome::Done {
                summary: "instant done".to_string()
            }
        }
    );
}

#[tokio::test]
async fn drains_spawned_cli_stderr_without_deadlock() {
    let mut child = Command::new("bash")
        .arg("-lc")
        .arg("for _ in $(seq 1 20000); do printf 1234567890 >&2; done")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let drain = tokio::spawn(drain_stderr(stderr));

    let status = timeout(Duration::from_secs(2), child.wait())
        .await
        .unwrap()
        .unwrap();

    assert!(status.success());
    drain.await.unwrap();
}

#[tokio::test]
#[ignore = "requires the real claude CLI and may spend model tokens"]
async fn claude_code_driver_real_cli_smoke() {
    let driver = ClaudeCodeDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Claude,
            model: None,
            variant: None,
            prompt: "Reply with exactly: driver-smoke".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: None,
        })
        .await
        .unwrap();
    let mut events = driver.events(&handle).unwrap();
    while let Some(event) = events.next().await {
        if matches!(event, SubagentEvent::Terminal { .. }) {
            return;
        }
    }
    panic!("claude CLI exited without a terminal event");
}

#[tokio::test]
#[ignore = "requires the real codex CLI and may spend model tokens"]
async fn codex_driver_real_cli_smoke() {
    let driver = CodexDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: AgentKind::Codex,
            model: None,
            variant: None,
            prompt: "Reply with exactly: driver-smoke".to_string(),
            cwd: std::env::current_dir().unwrap(),
            fake_fixture: None,
        })
        .await
        .unwrap();
    let mut events = driver.events(&handle).unwrap();
    while let Some(event) = events.next().await {
        if matches!(event, SubagentEvent::Terminal { .. }) {
            return;
        }
    }
    panic!("codex CLI exited without a terminal event");
}
