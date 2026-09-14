use std::{io::Write, process::Stdio};

use serde_json::json;
use tokio::{
    process::Command,
    time::{Duration, timeout},
};

use crate::{
    claude::claude_terminal_outcome,
    codex::{codex_agent_message, codex_terminal_outcome},
    shared::drain_stderr,
    types::TerminalOutcome,
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
async fn drains_spawned_cli_stderr_without_deadlock() {
    let mut blocked_child = stderr_writer_fixture();
    let blocked_stderr = blocked_child.stderr.take().unwrap();

    match timeout(Duration::from_millis(100), blocked_child.wait()).await {
        Err(_) => kill_and_reap(&mut blocked_child).await,
        Ok(Ok(status)) => panic!(
            "the fixture must exceed pipe capacity and block without a stderr reader, but exited with {status}"
        ),
        Ok(Err(error)) => {
            kill_and_reap(&mut blocked_child).await;
            panic!("failed to wait for undrained stderr fixture: {error}");
        }
    }
    drop(blocked_stderr);

    let mut child = stderr_writer_fixture();
    let stderr = child.stderr.take().unwrap();
    let drain = tokio::spawn(drain_stderr(stderr));

    let status = match timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            kill_and_reap(&mut child).await;
            drain.await.unwrap();
            panic!("failed to wait for stderr fixture: {error}");
        }
        Err(_) => {
            kill_and_reap(&mut child).await;
            drain.await.unwrap();
            panic!("stderr drain did not let the fixture exit within two seconds");
        }
    };

    drain.await.unwrap();
    assert!(status.success());
}

fn stderr_writer_fixture() -> tokio::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "tests::write_stderr_fixture",
            "--nocapture",
        ])
        .env("HIRSEL_STDERR_FIXTURE_BYTES", (2 * 1024 * 1024).to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn kill_and_reap(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    timeout(Duration::from_secs(1), child.wait())
        .await
        .expect("owned stderr fixture did not reap after kill")
        .expect("failed to reap owned stderr fixture");
}

#[test]
#[ignore = "spawned as a hermetic stderr writer fixture"]
fn write_stderr_fixture() {
    let byte_count = std::env::var("HIRSEL_STDERR_FIXTURE_BYTES")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let chunk = [b'x'; 64 * 1024];
    let mut stderr = std::io::stderr().lock();
    for _ in 0..byte_count / chunk.len() {
        stderr.write_all(&chunk).unwrap();
    }
    stderr
        .write_all(&chunk[..byte_count % chunk.len()])
        .unwrap();
}
