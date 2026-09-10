use super::*;
use futures_util::StreamExt;
use tempfile::TempDir;

const PEER: &str = include_str!("../fixtures/codex_app_server.py");

struct Peer {
    directory: TempDir,
}

impl Peer {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("peer.py"), PEER).unwrap();
        let helper = directory.path().join("host.py");
        std::fs::write(&helper, include_str!("../fixtures/codex_mcp_host.py")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(directory.path().join("capability"), "fixture-capability").unwrap();
        Self { directory }
    }

    fn launch(&self) -> crate::ScopedMcpLaunch {
        crate::ScopedMcpLaunch {
            host_executable: self.directory.path().join("host.py"),
            socket_path: self.directory.path().join("bridge.sock"),
            capability_file: self.directory.path().join("capability"),
            expected_tools: vec![
                "threads_context".into(),
                "threads_delegate".into(),
                "threads_report".into(),
            ],
        }
    }

    async fn spawn(&self, driver: &CodexDriver, mode: &str) -> DriverResult<SessionHandle> {
        self.spawn_with_timeout(driver, mode, Duration::from_secs(3))
            .await
    }

    async fn spawn_with_timeout(
        &self,
        driver: &CodexDriver,
        mode: &str,
        control_timeout: Duration,
    ) -> DriverResult<SessionHandle> {
        let mut command = Command::new("python3");
        command
            .arg(self.directory.path().join("peer.py"))
            .arg(mode)
            .arg(self.directory.path());
        driver
            .spawn_command(
                SpawnSpec {
                    agent: AgentKind::Codex,
                    model: None,
                    variant: None,
                    prompt: "initial task".into(),
                    cwd: self.directory.path().to_path_buf(),
                    fake_fixture: None,
                    scoped_mcp: self.launch(),
                },
                command,
                control_timeout,
            )
            .await
    }

    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.directory.path().join("requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn wait_pid(&self) -> u32 {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(self.directory.path().join("pid"))
                    && let Ok(pid) = text.parse()
                {
                    break pid;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
}

async fn wait_dead(pid: u32) {
    timeout(Duration::from_secs(3), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Ok(stat)
                    if stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, tail)| tail.starts_with("Z ")) =>
                {
                    break;
                }
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("owned fixture process survived cleanup");
}

async fn next_terminal(events: &mut EventStream) -> TerminalOutcome {
    timeout(Duration::from_secs(3), async {
        while let Some(event) = events.next().await {
            if let SubagentEvent::Terminal { outcome } = event {
                return outcome;
            }
        }
        panic!("stream closed without terminal");
    })
    .await
    .expect("terminal never arrived")
}

#[tokio::test]
async fn native_handshake_is_ordered_and_stderr_is_drained_before_initialization() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "stderr").await.unwrap();
    let requests = peer.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "initialize",
            "initialized",
            "config/read",
            "thread/start",
            "mcpServerStatus/list",
            "turn/start"
        ]
    );
    assert_eq!(requests[3]["params"]["approvalPolicy"], "never");
    assert_eq!(requests[3]["params"]["sandbox"], "danger-full-access");
    driver.retire(&handle).await.unwrap();
    wait_dead(peer.wait_pid().await).await;
}

#[tokio::test]
async fn every_startup_rejection_and_malformed_output_roll_back_the_owned_process() {
    for mode in [
        "reject-initialize",
        "reject-thread",
        "reject-turn",
        "malformed",
    ] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let error = peer.spawn(&driver, mode).await.unwrap_err();
        assert!(
            error.to_string().contains(if mode == "malformed" {
                "protocol error"
            } else {
                "rejected"
            }),
            "{mode}: {error}"
        );
        wait_dead(peer.wait_pid().await).await;
    }
}

#[tokio::test]
async fn startup_timeout_and_cancellation_kill_the_owned_process() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let error = peer
        .spawn_with_timeout(&driver, "hang-startup", Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(matches!(error, DriverError::RequestTimeout(_)));
    wait_dead(peer.wait_pid().await).await;

    let peer = Arc::new(Peer::new());
    let spawn_peer = peer.clone();
    let task = tokio::spawn(async move {
        spawn_peer
            .spawn(&CodexDriver::default(), "hang-startup")
            .await
    });
    let pid = peer.wait_pid().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    wait_dead(pid).await;
}

#[tokio::test]
async fn child_traffic_cannot_change_the_root_turn_result_or_steering_target() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "child").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            match events.next().await.unwrap() {
                SubagentEvent::Started { external_id } => assert_eq!(external_id, "root"),
                SubagentEvent::Progress { summary } if summary == "root barrier" => break,
                event => panic!("child traffic leaked: {event:?}"),
            }
        }
    })
    .await
    .unwrap();
    driver
        .prompt(&handle, "steer the same run".into())
        .await
        .unwrap();
    driver.interrupt(&handle).await.unwrap();
    assert_eq!(
        next_terminal(&mut events).await,
        TerminalOutcome::Done {
            summary: "ROOT RESULT".into()
        }
    );
    let requests = peer.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "turn/start")
            .count(),
        1
    );
    assert_eq!(requests[6]["method"], "turn/steer");
    assert_eq!(requests[6]["params"]["expectedTurnId"], "turn-a");
    assert!(matches!(
        driver.prompt(&handle, "after completion".into()).await,
        Err(DriverError::NoActiveTurn)
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn native_control_errors_are_returned_to_the_matching_caller() {
    for mode in ["control-errors", "out-of-order"] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer.spawn(&driver, mode).await.unwrap();
        let (steer, interrupt) = tokio::join!(
            driver.prompt(&handle, "follow-up".into()),
            driver.interrupt(&handle)
        );
        assert!(
            steer
                .unwrap_err()
                .to_string()
                .contains("turn/steer rejected")
        );
        if mode == "control-errors" {
            assert!(
                interrupt
                    .unwrap_err()
                    .to_string()
                    .contains("turn/interrupt rejected")
            );
        } else {
            interrupt.unwrap();
        }
        wait_dead(peer.wait_pid().await).await;
        assert!(
            driver
                .prompt(&handle, "after interrupt".into())
                .await
                .is_err()
        );
        assert!(driver.interrupt(&handle).await.is_err());
        let events = driver.events(&handle).unwrap().collect::<Vec<_>>().await;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SubagentEvent::Terminal { .. }))
                .count(),
            1
        );
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn control_deadline_and_transport_closure_fail_pending_requests_and_settle() {
    for mode in ["hang-control", "close-control"] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer
            .spawn_with_timeout(&driver, mode, Duration::from_secs(2))
            .await
            .unwrap();
        let mut events = driver.events(&handle).unwrap();
        let error = driver
            .prompt(&handle, "follow-up".into())
            .await
            .unwrap_err();
        if mode == "hang-control" {
            assert!(matches!(error, DriverError::RequestTimeout(_)));
        }
        assert!(matches!(
            next_terminal(&mut events).await,
            TerminalOutcome::Failed { .. }
        ));
        wait_dead(peer.wait_pid().await).await;
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn clean_exit_and_inherited_pipes_cannot_leave_a_run_unsettled() {
    for mode in ["exit-zero", "inherited-pipes"] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer.spawn(&driver, mode).await.unwrap();
        let mut events = driver.events(&handle).unwrap();
        assert!(matches!(
            next_terminal(&mut events).await,
            TerminalOutcome::Failed { .. }
        ));
        if mode == "inherited-pipes" {
            let pid = std::fs::read_to_string(peer.directory.path().join("descendant"))
                .unwrap()
                .parse()
                .unwrap();
            wait_dead(pid).await;
        }
        driver.retire(&handle).await.unwrap();
    }
}

#[tokio::test]
async fn a_result_before_eof_remains_the_single_terminal_outcome() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "done").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    assert_eq!(
        next_terminal(&mut events).await,
        TerminalOutcome::Done {
            summary: "root result".into()
        }
    );
    assert!(events.next().await.is_none());
    wait_dead(peer.wait_pid().await).await;
    driver.retire(&handle).await.unwrap();
    let session_events = driver.events(&handle);
    assert!(matches!(
        session_events,
        Err(DriverError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn steering_acknowledgement_must_name_the_same_active_turn() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "wrong-steer-turn").await.unwrap();
    let error = driver.prompt(&handle, "same run".into()).await.unwrap_err();
    assert!(error.to_string().contains("different or missing turn id"));
    let mut events = driver.events(&handle).unwrap();
    assert!(matches!(
        next_terminal(&mut events).await,
        TerminalOutcome::Failed { .. }
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn unsupported_native_requests_are_rejected_without_stealing_a_client_response() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "server-request").await.unwrap();
    driver.prompt(&handle, "same run".into()).await.unwrap();
    let requests = peer.requests();
    let response = requests
        .iter()
        .find(|value| value.get("error").is_some())
        .unwrap();
    assert_eq!(response["id"], 6);
    assert_eq!(response["error"]["code"], -32601);
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn native_reply_backpressure_does_not_block_direct_child_exit_supervision() {
    let peer = Peer::new();
    let driver = Arc::new(CodexDriver::default());
    let handle = peer.spawn(&driver, "reply-backpressure").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    let writer_driver = Arc::clone(&driver);
    let writer_handle = handle.clone();
    let writer = tokio::spawn(async move {
        writer_driver
            .prompt(&writer_handle, "x".repeat(2 * 1024 * 1024))
            .await
    });
    let session = driver.sessions.get(&handle).unwrap();
    timeout(Duration::from_secs(3), async {
        while session.stdin.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture writer never blocked");
    std::fs::write(peer.directory.path().join("send-native-request"), "").unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            match events.next().await.unwrap() {
                SubagentEvent::Progress { summary }
                    if summary.starts_with("unsupported codex server request:") =>
                {
                    break;
                }
                SubagentEvent::Terminal { outcome } => panic!("fixture ended early: {outcome:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("supervisor did not receive native request");
    std::fs::write(peer.directory.path().join("exit-now"), "").unwrap();
    timeout(Duration::from_secs(2), async {
        assert!(matches!(
            next_terminal(&mut events).await,
            TerminalOutcome::Failed { .. }
        ));
        assert!(writer.await.unwrap().is_err());
        driver.retire(&handle).await.unwrap();
    })
    .await
    .expect("blocked response delayed exit supervision");
    wait_dead(peer.wait_pid().await).await;
    let descendant = std::fs::read_to_string(peer.directory.path().join("descendant"))
        .unwrap()
        .parse()
        .unwrap();
    wait_dead(descendant).await;
}

#[tokio::test]
async fn failed_native_reply_still_drains_a_valid_final_result() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "closed-stdin-final").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    assert_eq!(
        next_terminal(&mut events).await,
        TerminalOutcome::Done {
            summary: "last root result".into()
        }
    );
    assert!(events.next().await.is_none());
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn scoped_preflight_rejects_inherited_connectors_and_wrong_catalog_before_a_turn() {
    for mode in [
        "unsafe-config",
        "unsafe-status",
        "null-status",
        "wrong-tools",
        "missing-layers",
        "missing-bridge",
        "bad-cursor",
        "unsafe-config-warning",
    ] {
        let peer = Peer::new();
        let error = peer.spawn(&CodexDriver::default(), mode).await.unwrap_err();
        assert!(!error.to_string().contains("SECRET_CANARY"));
        assert!(
            !peer.requests().iter().any(|r| r["method"] == "turn/start"),
            "{mode}"
        );
        wait_dead(peer.wait_pid().await).await;
    }
}

#[tokio::test]
async fn supplied_mcp_helper_is_launched_and_recursive_tools_are_called() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "scoped-recursive").await.unwrap();
    let mut events = driver.events(&handle).unwrap();
    let mut output = None;
    while let Some(event) = events.next().await {
        match event {
            SubagentEvent::AssistantOutput { text } => output = Some(text),
            SubagentEvent::Terminal { outcome } => {
                assert!(matches!(outcome, TerminalOutcome::Done { .. }))
            }
            _ => {}
        }
    }
    assert!(output.unwrap().contains("thread_id"));
    let calls = std::fs::read_to_string(peer.directory.path().join("tool-calls")).unwrap();
    let names = calls
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["name"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["threads_context", "threads_delegate", "threads_report"]
    );
    driver.retire(&handle).await.unwrap();
    let helper = std::fs::read_to_string(peer.directory.path().join("helper-pid"))
        .unwrap()
        .parse()
        .unwrap();
    wait_dead(helper).await;
}

#[tokio::test]
async fn complete_output_precedes_one_terminal_and_is_never_the_bounded_summary() {
    for mode in [
        "long-output",
        "failed-final",
        "empty-done",
        "missing-status",
        "invalid-status",
    ] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer.spawn(&driver, mode).await.unwrap();
        let events = driver.events(&handle).unwrap().collect::<Vec<_>>().await;
        let outputs = events
            .iter()
            .filter_map(|e| match e {
                SubagentEvent::AssistantOutput { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        let outcomes = events
            .iter()
            .filter_map(|e| match e {
                SubagentEvent::Terminal { outcome } => Some(outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 1, "{mode}");
        assert!(matches!(
            events.last(),
            Some(SubagentEvent::Terminal { .. })
        ));
        if matches!(mode, "long-output" | "failed-final") {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].len(), 30_000);
        } else {
            assert!(outputs.is_empty());
        }
        match mode {
            "long-output" => assert!(
                matches!(outcomes[0], TerminalOutcome::Done { summary } if summary.len() < 30_000)
            ),
            "empty-done" => assert!(
                matches!(outcomes[0], TerminalOutcome::Done { summary } if summary.is_empty())
            ),
            _ => assert!(matches!(outcomes[0], TerminalOutcome::Failed { .. })),
        }
        assert!(driver.interrupt(&handle).await.is_err());
        assert!(driver.prompt(&handle, "stale".into()).await.is_err());
        driver.retire(&handle).await.unwrap();
    }
}

#[test]
fn scoped_command_preserves_selection_and_disables_native_context_sources() {
    let fixture = tempfile::tempdir().unwrap();
    let task = SpawnSpec {
        agent: AgentKind::Codex,
        model: Some("gpt-6-astra".into()),
        variant: Some("ultra".into()),
        prompt: "accepted context".into(),
        cwd: "/tmp".into(),
        fake_fixture: None,
        scoped_mcp: crate::test_support::scoped_mcp_fixture(fixture.path(), &["threads_context"]),
    };
    let mut command = Command::new("codex");
    config::configure_command(&mut command, &task);
    let args = command
        .as_std()
        .get_args()
        .map(|s| s.to_str().unwrap())
        .collect::<Vec<_>>();
    for required in [
        "app-server",
        "--stdio",
        "model=\"gpt-6-astra\"",
        "model_reasoning_effort=\"ultra\"",
        "agents.enabled=false",
        "features.apps=false",
        "features.plugins=false",
        "memories.use_memories=false",
        "memories.generate_memories=false",
    ] {
        assert!(args.contains(&required), "missing {required}");
    }
    let inherited = BTreeSet::from(["dot.name".into(), "quote\"name".into()]);
    let request = config::thread_start_request(&task, &inherited, "unique_bridge");
    assert_eq!(
        request["params"]["config"]["mcp_servers"]["dot.name"]["enabled"],
        false
    );
    assert_eq!(
        request["params"]["config"]["mcp_servers"]["quote\"name"]["enabled"],
        false
    );
    assert_eq!(
        request["params"]["config"]["mcp_servers"]["unique_bridge"]["enabled"],
        true
    );
}

#[tokio::test]
async fn scoped_catalog_preflight_consumes_all_pages_before_turn_start() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer.spawn(&driver, "paged-status").await.unwrap();
    let requests = peer.requests();
    let methods = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        &methods[4..],
        ["mcpServerStatus/list", "mcpServerStatus/list", "turn/start"]
    );
    assert!(matches!(
        next_terminal(&mut driver.events(&handle).unwrap()).await,
        TerminalOutcome::Done { .. }
    ));
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn interrupt_acknowledgment_without_terminal_times_out_and_retires_owned_processes() {
    let peer = Peer::new();
    let driver = CodexDriver::default();
    let handle = peer
        .spawn_with_timeout(&driver, "interrupt-ack-only", Duration::from_millis(500))
        .await
        .unwrap();
    let mut events = driver.events(&handle).unwrap();
    assert!(matches!(
        driver.interrupt(&handle).await,
        Err(DriverError::RequestTimeout(_))
    ));
    assert!(matches!(
        next_terminal(&mut events).await,
        TerminalOutcome::Failed { .. }
    ));
    wait_dead(peer.wait_pid().await).await;
    driver.retire(&handle).await.unwrap();
}

#[tokio::test]
async fn intermediate_messages_and_malformed_completion_never_invent_final_output() {
    for mode in [
        "commentary-eof",
        "commentary-failed",
        "unknown-eof",
        "unknown-failed",
        "commentary-done",
        "malformed-completion",
    ] {
        let peer = Peer::new();
        let driver = CodexDriver::default();
        let handle = peer.spawn(&driver, mode).await.unwrap();
        let events = timeout(
            Duration::from_secs(3),
            driver.events(&handle).unwrap().collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SubagentEvent::AssistantOutput { .. })),
            "{mode}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SubagentEvent::Terminal { .. }))
                .count(),
            1,
            "{mode}"
        );
        if mode == "commentary-done" {
            assert!(
                matches!(events.last(), Some(SubagentEvent::Terminal { outcome: TerminalOutcome::Done { summary } }) if summary.is_empty())
            );
        } else {
            assert!(
                matches!(
                    events.last(),
                    Some(SubagentEvent::Terminal {
                        outcome: TerminalOutcome::Failed { .. }
                    })
                ),
                "{mode}"
            );
        }
        if mode == "malformed-completion" {
            wait_dead(peer.wait_pid().await).await;
        }
        driver.retire(&handle).await.unwrap();
    }
}
