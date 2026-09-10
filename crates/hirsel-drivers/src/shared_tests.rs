use futures_util::StreamExt;
use tokio::time::{Duration, timeout};

use crate::{
    DriverError, FakeDriver, SpawnSpec, SubagentDriver, SubagentEvent, TerminalOutcome,
    shared::EventHub, test_support::scoped_launch,
};

#[test]
fn scoped_launch_rejects_invalid_paths_and_catalog_without_disclosing_them() {
    let mut launch = scoped_launch();
    launch.validate().unwrap();
    let debug = format!("{launch:?}");
    assert!(!debug.contains("hirsel-test-only"));
    assert!(!debug.contains("bridge.sock"));
    launch.socket_path = "sensitive-relative-socket".into();
    let error = launch.validate().unwrap_err().to_string();
    assert!(!error.contains("sensitive-relative-socket"));
    for tools in [vec![], vec!["x", "x"], vec![""], vec![" x"], vec!["x\n"]] {
        let mut launch = scoped_launch();
        launch.expected_tools = tools.into_iter().map(str::to_owned).collect();
        assert!(launch.validate().is_err());
    }
}

#[test]
fn scoped_bridge_arguments_preserve_spaces_as_literal_arguments() {
    let mut launch = scoped_launch();
    launch.socket_path = "/private bridge/socket name".into();
    launch.capability_file = "/private bridge/capability name".into();
    assert_eq!(
        launch.bridge_args(),
        vec![
            std::ffi::OsString::from("thread-tool-bridge"),
            "--socket".into(),
            "/private bridge/socket name".into(),
            "--cap-file".into(),
            "/private bridge/capability name".into(),
        ]
    );
}

#[test]
fn spawn_spec_requires_scoped_bridge_on_wire() {
    let spec = SpawnSpec {
        agent: crate::AgentKind::Claude,
        model: None,
        variant: None,
        prompt: "test".into(),
        cwd: "/tmp".into(),
        fake_fixture: None,
        scoped_mcp: scoped_launch(),
    };
    let mut value = serde_json::to_value(&spec).unwrap();
    assert_eq!(
        serde_json::from_value::<SpawnSpec>(value.clone()).unwrap(),
        spec
    );
    value.as_object_mut().unwrap().remove("scoped_mcp");
    assert!(serde_json::from_value::<SpawnSpec>(value).is_err());
}

#[test]
fn cli_environment_removes_hirsel_authority_but_keeps_provider_auth_configuration() {
    let mut command = tokio::process::Command::new("unused");
    command.env("HIRSEL_TOKEN", "fixture-owner-token");
    command.env("HIRSEL_EXECUTION_BINDING", "fixture-binding");
    command.env("OPENAI_API_KEY", "fixture-provider-token");
    command.env("HOME", "/fixture-home");
    command.env("CODEX_HOME", "/fixture-codex");
    crate::shared::sanitize_hirsel_environment(&mut command);
    let vars = command
        .as_std()
        .get_envs()
        .collect::<std::collections::HashMap<_, _>>();
    for name in ["HIRSEL_TOKEN", "HIRSEL_EXECUTION_BINDING"] {
        assert_eq!(vars[std::ffi::OsStr::new(name)], None);
    }
    assert_eq!(
        vars[std::ffi::OsStr::new("OPENAI_API_KEY")],
        Some(std::ffi::OsStr::new("fixture-provider-token"))
    );
    assert_eq!(
        vars[std::ffi::OsStr::new("HOME")],
        Some(std::ffi::OsStr::new("/fixture-home"))
    );
    assert_eq!(
        vars[std::ffi::OsStr::new("CODEX_HOME")],
        Some(std::ffi::OsStr::new("/fixture-codex"))
    );
}

#[tokio::test]
async fn lagged_and_late_subscribers_receive_full_output_before_single_terminal() {
    let hub = EventHub::new(1);
    let mut early = hub.stream().unwrap();
    hub.emit(SubagentEvent::Progress {
        summary: "first".into(),
    })
    .unwrap();
    assert!(matches!(
        early.next().await,
        Some(SubagentEvent::Progress { .. })
    ));
    let full = format!("{}the end", "é".repeat(30_000));
    for index in 0..8 {
        hub.emit(SubagentEvent::Progress {
            summary: index.to_string(),
        })
        .unwrap();
    }
    let terminal = TerminalOutcome::Done {
        summary: "bounded".into(),
    };
    hub.complete(terminal.clone(), Some(full.clone())).unwrap();
    hub.complete(TerminalOutcome::Interrupted, Some("wrong output".into()))
        .unwrap();
    hub.emit(SubagentEvent::Progress {
        summary: "too late".into(),
    })
    .unwrap();
    for stream in [early, hub.stream().unwrap()] {
        let events = timeout(Duration::from_secs(1), stream.collect::<Vec<_>>())
            .await
            .unwrap();
        let outputs = events
            .iter()
            .filter_map(|event| match event {
                SubagentEvent::AssistantOutput { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(outputs, vec![&full]);
        assert_eq!(
            events.last(),
            Some(&SubagentEvent::Terminal {
                outcome: terminal.clone()
            })
        );
        assert!(matches!(
            events[events.len() - 2],
            SubagentEvent::AssistantOutput { .. }
        ));
    }
    timeout(Duration::from_secs(1), hub.wait_terminal())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_and_cancellation_race_cannot_emit_output_after_terminal() {
    for _ in 0..32 {
        let hub = EventHub::new(1);
        let gate = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let finish = {
            let hub = hub.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                hub.complete(
                    TerminalOutcome::Done {
                        summary: "done".into(),
                    },
                    Some("full".into()),
                )
                .unwrap();
            })
        };
        let cancel = {
            let hub = hub.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.wait().await;
                hub.complete(TerminalOutcome::Interrupted, None).unwrap();
            })
        };
        gate.wait().await;
        finish.await.unwrap();
        cancel.await.unwrap();
        let events = hub.stream().unwrap().collect::<Vec<_>>().await;
        match events.as_slice() {
            [
                SubagentEvent::Terminal {
                    outcome: TerminalOutcome::Interrupted,
                },
            ] => {}
            [
                SubagentEvent::AssistantOutput { text },
                SubagentEvent::Terminal {
                    outcome: TerminalOutcome::Done { .. },
                },
            ] => assert_eq!(text, "full"),
            _ => panic!("output and terminal did not commit together: {events:?}"),
        }
    }
}

#[tokio::test]
async fn empty_or_duplicate_output_never_invents_another_assistant_message() {
    let hub = EventHub::new(1);
    hub.emit(SubagentEvent::AssistantOutput {
        text: String::new(),
    })
    .unwrap();
    hub.emit(SubagentEvent::AssistantOutput {
        text: "actual".into(),
    })
    .unwrap();
    hub.complete(
        TerminalOutcome::Failed {
            reason: "error".into(),
        },
        Some("duplicate".into()),
    )
    .unwrap();
    assert_eq!(
        hub.stream().unwrap().collect::<Vec<_>>().await,
        vec![
            SubagentEvent::AssistantOutput {
                text: "actual".into()
            },
            SubagentEvent::Terminal {
                outcome: TerminalOutcome::Failed {
                    reason: "error".into()
                }
            },
        ]
    );
}

#[tokio::test]
async fn fake_final_output_is_full_but_summary_bounded_and_dead_controls_reject() {
    let fixture = tempfile::NamedTempFile::new().unwrap();
    let full = "é".repeat(30_000);
    std::fs::write(
        fixture.path(),
        serde_json::to_vec(&serde_json::json!({
            "delay_ms": 0, "progress": [], "assistant_output": full,
            "terminal": {"status":"done", "summary": full},
        }))
        .unwrap(),
    )
    .unwrap();
    let driver = FakeDriver::default();
    let handle = driver
        .spawn(SpawnSpec {
            agent: crate::AgentKind::Codex,
            model: Some("chosen".into()),
            variant: Some("high".into()),
            prompt: "test".into(),
            cwd: "/tmp".into(),
            fake_fixture: Some(fixture.path().into()),
            scoped_mcp: scoped_launch(),
        })
        .await
        .unwrap();
    let events = driver.events(&handle).unwrap().collect::<Vec<_>>().await;
    assert_eq!(events[1], SubagentEvent::AssistantOutput { text: full });
    let SubagentEvent::Terminal {
        outcome: TerminalOutcome::Done { summary },
    } = &events[2]
    else {
        panic!("missing terminal")
    };
    assert_eq!(summary.chars().count(), 24_000);
    assert!(matches!(
        driver.prompt(&handle, "later".into()).await,
        Err(DriverError::SessionClosed)
    ));
    assert!(matches!(
        driver.interrupt(&handle).await,
        Err(DriverError::SessionClosed)
    ));
    driver.retire(&handle).await.unwrap();
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn mcp_fixture_serves_preflight_and_provider_connections_without_capability_reads() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::TempDir::new().unwrap();
    let launch = crate::test_support::scoped_mcp_fixture(
        dir.path(),
        &["threads_context", "threads_delegate"],
    );
    std::fs::write(
        dir.path().join("scoped_mcp.json"),
        serde_json::to_vec(&serde_json::json!({
            "tools": [
                {"name":"threads_context", "inputSchema":{"type":"object"}},
                {"name":"threads_delegate", "inputSchema":{"type":"object"}},
            ],
            "page_size": 1,
            "responses": {"threads_context":{"thread_id":27}},
        }))
        .unwrap(),
    )
    .unwrap();
    // No capability file or host socket exists: this is strictly an offline peer.
    assert!(!launch.capability_file.exists());
    for _ in 0..2 {
        let mut child = tokio::process::Command::new(&launch.host_executable)
            .args(launch.bridge_args())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for request in [
            serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2024-11-05"}}),
            serde_json::json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
            serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
            serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"tools/list", "params":{"cursor":"1"}}),
            serde_json::json!({"jsonrpc":"2.0", "id":4, "method":"tools/call", "params":{"name":"threads_context", "arguments":{}}}),
            serde_json::json!({"jsonrpc":"2.0", "id":5, "method":"tools/call", "params":{"name":"owner_connector", "arguments":{}}}),
        ] {
            let mut line = serde_json::to_vec(&request).unwrap();
            line.push(b'\n');
            stdin.write_all(&line).await.unwrap();
        }
        drop(stdin);
        let output = timeout(Duration::from_secs(2), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let replies = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(replies.len(), 5);
        assert_eq!(replies[0]["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(replies[1]["result"]["tools"][0]["name"], "threads_context");
        assert_eq!(replies[1]["result"]["nextCursor"], "1");
        assert_eq!(replies[2]["result"]["tools"][0]["name"], "threads_delegate");
        assert!(replies[2]["result"].get("nextCursor").is_none());
        let context: serde_json::Value =
            serde_json::from_str(replies[3]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(context["thread_id"], 27);
        assert_eq!(replies[4]["error"]["code"], -32602);
    }
}
