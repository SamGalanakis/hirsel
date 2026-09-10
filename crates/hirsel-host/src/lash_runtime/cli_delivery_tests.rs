use super::*;
use hirsel_drivers::{AgentKind, DriverResult, EventStream};
use std::sync::atomic::{AtomicBool, Ordering};

struct TerminalPeer {
    failed: bool,
    retired: AtomicBool,
}

struct ToolCallingPeer {
    retired: AtomicBool,
}
#[async_trait::async_trait]
impl SubagentDriver for ToolCallingPeer {
    async fn spawn(&self, spec: SpawnSpec) -> DriverResult<SessionHandle> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let capability = tokio::fs::read_to_string(&spec.scoped_mcp.capability_file)
            .await
            .unwrap();
        let mut stream = tokio::net::UnixStream::connect(&spec.scoped_mcp.socket_path)
            .await
            .unwrap();
        let invocation = json!({
            "capability": capability,
            "bridge_instance": "timeline-failure-peer",
            "invocation_id": "read",
            "request": {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "threads_context", "arguments": {}}
            }
        });
        stream
            .write_all(format!("{invocation}\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        tokio::io::BufReader::new(stream)
            .read_line(&mut response)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["result"]["isError"], false, "{response}");
        Ok(SessionHandle {
            id: "tool-calling-peer".into(),
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
        self.retired.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn events(&self, _: &SessionHandle) -> DriverResult<EventStream> {
        Ok(Box::pin(futures_util::stream::iter([
            SubagentEvent::AssistantOutput {
                text: "must not be published".into(),
            },
            SubagentEvent::Terminal {
                outcome: TerminalOutcome::Done {
                    summary: "fixture complete".into(),
                },
            },
        ])))
    }
}
#[async_trait::async_trait]
impl SubagentDriver for TerminalPeer {
    async fn spawn(&self, spec: SpawnSpec) -> DriverResult<SessionHandle> {
        assert!(!spec.scoped_mcp.expected_tools.is_empty());
        Ok(SessionHandle {
            id: "terminal-peer".into(),
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
        self.retired.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn events(&self, _: &SessionHandle) -> DriverResult<EventStream> {
        let outcome = if self.failed {
            TerminalOutcome::Failed {
                reason: "fixture failure".into(),
            }
        } else {
            TerminalOutcome::Done {
                summary: "fixture complete".into(),
            }
        };
        Ok(Box::pin(futures_util::stream::iter([
            SubagentEvent::AssistantOutput {
                text: "Retain this exact final output".into(),
            },
            SubagentEvent::Terminal { outcome },
        ])))
    }
}
async fn request(state: &crate::AppState) -> OwnerTurn {
    let thread = state
        .storage
        .create_thread(
            "delivery-root",
            "Delivery",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let turn = state
        .storage
        .queue_thread_turn(thread.id, None)
        .await
        .unwrap();
    OwnerTurn {
        history_id: state.storage.history_id().await.unwrap(),
        turn_id: Some(turn.id),
        thread_id: thread.id,
        thread_action: None,
        message_id: None,
        report_triggered: false,
        client_id: "delivery-input".into(),
        body: "Test delivery".into(),
        anchor: None,
        attachments: vec![],
        mode: hirsel_proto::SendMode::Send,
    }
}
fn start(
    state: &crate::AppState,
    request: OwnerTurn,
    failed: bool,
) -> (
    Arc<TerminalPeer>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let peer = Arc::new(TerminalPeer {
        failed,
        retired: AtomicBool::new(false),
    });
    let work = CliTurn::new(request.turn_id.unwrap(), peer.clone());
    let tools = state.tools.clone();
    let task = tokio::spawn(async move {
        work.run(
            &tools,
            request,
            crate::storage::ThreadExecution::Cli {
                agent: AgentKind::Claude,
                model: "fixture".into(),
                variant: "fixture".into(),
                cwd: std::env::temp_dir(),
            },
            Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await
    });
    (peer, task)
}
async fn wait_pending(
    peer: &TerminalPeer,
    task: &tokio::task::JoinHandle<anyhow::Result<()>>,
    attempts: &std::sync::atomic::AtomicUsize,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while (!peer.retired.load(Ordering::SeqCst) || attempts.load(Ordering::SeqCst) == 0)
            && !task.is_finished()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        attempts.load(Ordering::SeqCst) > 0,
        "real SQLite failure must be observed"
    );
    assert!(
        peer.retired.load(Ordering::SeqCst),
        "provider must retire despite storage failure"
    );
    assert!(
        !task.is_finished(),
        "terminal must remain pending until durable commit"
    );
}
#[tokio::test]
async fn terminal_delivery_retries_real_sql_failures_and_replays_exactly_once() {
    for failed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::build_state(crate::tests::test_config(dir.path()))
            .await
            .unwrap();
        let request = request(&state).await;
        if failed {
            state
                .storage
                .append_thread_activity(
                    request.thread_id,
                    request.turn_id,
                    "execution_failed",
                    &json!({"reason":"earlier agent activity"}),
                )
                .await
                .unwrap();
        }
        let history = request.history_id.clone();
        let turn_id = request.turn_id.unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
        let trigger = if failed {
            "CREATE TRIGGER fail_delivery BEFORE INSERT ON thread_activities WHEN NEW.kind='execution_failed' BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected transient activity failure'); END"
        } else {
            "CREATE TRIGGER fail_delivery BEFORE UPDATE OF finished_at ON thread_turns WHEN NEW.finished_at IS NOT NULL BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected transient completion failure'); END"
        };
        let attempts = state.storage.track_completion_failures().await.unwrap();
        conn.execute_batch(trigger).unwrap();
        let (peer, task) = start(&state, request, failed);
        wait_pending(&peer, &task, &attempts).await;
        assert_eq!(
            state.storage.thread_turn(turn_id).await.unwrap().state,
            ThreadTurnState::Running
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM chat_messages WHERE author='agent'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
            0
        );
        conn.execute_batch("DROP TRIGGER fail_delivery").unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let terminal = if failed {
            ThreadTurnState::Failed
        } else {
            ThreadTurnState::Completed
        };
        let turn = state.storage.thread_turn(turn_id).await.unwrap();
        assert_eq!(turn.state, terminal);
        // Re-enter after a lost caller receipt: stored completion wins, no new output/activity.
        let replay = state
            .storage
            .complete_thread_turn_with_failure(
                &history,
                turn_id,
                terminal,
                Some(("must not replace".into(), vec![])),
                failed.then_some("fixture failure"),
            )
            .await
            .unwrap();
        assert_eq!(replay.turn.agent_message_id, turn.agent_message_id);
        assert_eq!(replay.failure_activity.is_some(), failed);
        if let Some(activity) = replay.failure_activity {
            assert_eq!(activity.data["reason"], "fixture failure");
        }
        assert_eq!(
            replay.message.unwrap().body,
            "Retain this exact final output"
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM chat_messages WHERE author='agent'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM thread_activities WHERE kind='execution_failed'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
            2 * u64::from(failed)
        );
    }
}

#[tokio::test]
async fn cli_tool_telemetry_retains_integrity_failure_until_failed_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let failed_request = request(&state).await;
    let turn_id = failed_request.turn_id.unwrap();
    let attempts = state.storage.track_completion_failures().await.unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_timeline_event BEFORE INSERT ON thread_turn_events WHEN NEW.seq=1 BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected CLI timeline event failure'); END;
         CREATE TRIGGER fail_timeline_terminal BEFORE UPDATE OF finished_at ON thread_turns WHEN NEW.finished_at IS NOT NULL BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected CLI timeline terminal failure'); END;",
    )
    .unwrap();
    let peer = Arc::new(ToolCallingPeer {
        retired: AtomicBool::new(false),
    });
    let work = CliTurn::new(turn_id, peer.clone());
    let tools = state.tools.clone();
    let task = tokio::spawn(async move {
        work.run(
            &tools,
            failed_request,
            crate::storage::ThreadExecution::Cli {
                agent: AgentKind::Claude,
                model: "fixture".into(),
                variant: "fixture".into(),
                cwd: std::env::temp_dir(),
            },
            Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while (!peer.retired.load(Ordering::SeqCst) || attempts.load(Ordering::SeqCst) < 2)
            && !task.is_finished()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "both the ToolDone append and terminal failure write must reach SQLite"
    );
    assert!(peer.retired.load(Ordering::SeqCst));
    assert!(!task.is_finished());
    assert_eq!(
        state.storage.thread_turn(turn_id).await.unwrap().state,
        ThreadTurnState::Running
    );
    assert!(
        state
            .tools
            .turn_timeline_integrity_failure(turn_id)
            .is_some_and(|reason| reason.contains("CLI timeline event failure"))
    );
    conn.execute_batch("DROP TRIGGER fail_timeline_event; DROP TRIGGER fail_timeline_terminal;")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let detail = state
        .storage
        .thread_detail(
            state.storage.thread_turn(turn_id).await.unwrap().thread_id,
            None,
            30,
        )
        .await
        .unwrap();
    let turn = detail.turns.iter().find(|turn| turn.id == turn_id).unwrap();
    assert_eq!(turn.state, ThreadTurnState::Failed);
    assert!(turn.agent_message_id.is_none());
    assert!(
        state
            .tools
            .turn_timeline_integrity_failure(turn_id)
            .is_none()
    );
    assert!(detail.messages.is_empty());
    assert_eq!(detail.turn_timelines[0].events.len(), 1);
    assert!(matches!(
        detail.turn_timelines[0].events[0].event,
        hirsel_proto::TurnEventKind::ToolStart { .. }
    ));
    assert!(detail.activities.iter().any(|activity| {
        activity.kind == "execution_failed"
            && activity.data["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("CLI timeline event failure"))
    }));
    assert!(state.broadcast_log.recent().iter().all(|frame| {
        !matches!(frame, hirsel_proto::HostToClient::ThreadTurn { turn } if turn.id == turn_id && turn.state == ThreadTurnState::Completed)
            && !matches!(frame, hirsel_proto::HostToClient::Msg { message } if message.body == "must not be published")
    }));

    let unaffected = request(&state).await;
    let unaffected_id = unaffected.turn_id.unwrap();
    let (_peer, task) = start(&state, unaffected, false);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .storage
            .thread_turn(unaffected_id)
            .await
            .unwrap()
            .state,
        ThreadTurnState::Completed
    );
    state.tools.record_turn_timeline_integrity(
        unaffected_id,
        "must be cleared with the old history".into(),
    );
    state.agent.reset_history().await.unwrap();
    assert!(
        state
            .tools
            .turn_timeline_integrity_failure(unaffected_id)
            .is_none()
    );
}

#[tokio::test]
async fn terminal_delivery_retry_cannot_write_into_replaced_history() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let old = request(&state).await;
    let old_id = old.turn_id.unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
    let attempts = state.storage.track_completion_failures().await.unwrap();
    conn.execute_batch("CREATE TRIGGER fail_delivery BEFORE UPDATE OF finished_at ON thread_turns WHEN NEW.finished_at IS NOT NULL BEGIN SELECT terminal_delivery_probe(); SELECT RAISE(FAIL,'injected transient completion failure'); END").unwrap();
    let (peer, task) = start(&state, old, true);
    wait_pending(&peer, &task, &attempts).await;
    state.storage.reset().await.unwrap();
    let replacement = request(&state).await;
    assert_eq!(replacement.turn_id, Some(old_id));
    conn.execute_batch("DROP TRIGGER fail_delivery").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        state.storage.thread_turn(old_id).await.unwrap().state,
        ThreadTurnState::Queued
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM chat_messages WHERE author='agent'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM thread_activities WHERE kind='execution_failed'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
}
