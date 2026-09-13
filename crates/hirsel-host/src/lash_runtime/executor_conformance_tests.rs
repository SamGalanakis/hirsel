//! Backend conformance for the host-owned executor contract.
//!
//! Each table entry translates the same scripts through a backend's real adapter and then hands
//! the resulting events to `TurnIngest`. Adding a fourth backend means adding one adapter entry;
//! the durable, live, activity, message, and terminal assertions remain shared.

use super::*;
use hirsel_drivers::{
    AgentKind, FakeDriver, ScopedMcpLaunch, SpawnSpec, SubagentDriver, SubagentEvent,
};
use lash::{TurnActivity, TurnEvent};
use lash_core::{ToolCallOutput, ToolCallRecord, ToolFailure, ToolFailureClass};

use super::{cli_turn::cli_executor_event, native_worker::native_executor_event};

#[derive(Clone, Copy, Debug)]
enum Backend {
    Host,
    NativeWorker,
    Cli,
}

impl Backend {
    const ALL: [Self; 3] = [Self::Host, Self::NativeWorker, Self::Cli];

    fn name(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::NativeWorker => "native-worker",
            Self::Cli => "cli",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Success,
    Failure,
    Cancellation,
}

impl Scenario {
    const ALL: [Self; 3] = [Self::Success, Self::Failure, Self::Cancellation];

    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancellation => "cancellation",
        }
    }
}

#[derive(Debug, PartialEq)]
struct Projection {
    timeline: Vec<Value>,
    tool_activities: Vec<Value>,
    tool_calls: Vec<ToolCallSummary>,
    state: hirsel_proto::ThreadTurnState,
    broadcasts: Vec<Value>,
}

fn ok_output() -> ToolCallOutput {
    ToolCallOutput::success(json!({"status": 0, "stdout": "ok"}))
}

fn error_output() -> ToolCallOutput {
    ToolCallOutput::failure(ToolFailure::tool(
        ToolFailureClass::Execution,
        "fixture_error",
        "fixture failed",
    ))
}

fn tool_records() -> Vec<ToolCallRecord> {
    vec![
        ToolCallRecord {
            call_id: Some("tool-a".into()),
            tool: "shell_run".into(),
            args: json!({"cmd": "true"}),
            output: ok_output(),
            duration_ms: 1,
        },
        ToolCallRecord {
            call_id: Some("tool-b".into()),
            tool: "read_file".into(),
            args: json!({"path": "missing"}),
            output: error_output(),
            duration_ms: 2,
        },
    ]
}

fn stream_events(scenario: Scenario) -> Vec<TurnEvent> {
    let mut events = vec![TurnEvent::TurnStarted {
        turn_id: "executor-conformance-turn".into(),
    }];
    match scenario {
        Scenario::Success => {
            events.extend([
                TurnEvent::ReasoningDelta {
                    text: "check ".into(),
                },
                TurnEvent::ReasoningDelta {
                    text: "facts".into(),
                },
                TurnEvent::AssistantProseDelta {
                    text: "running tools".into(),
                },
                TurnEvent::ToolCallStarted {
                    call_id: Some("tool-a".into()),
                    name: "shell_run".into(),
                    args: json!({"cmd": "true"}),
                    graph_key: None,
                    parent_call_id: None,
                },
                TurnEvent::ToolCallCompleted {
                    call_id: Some("tool-a".into()),
                    name: "shell_run".into(),
                    args: json!({"cmd": "true"}),
                    output: ok_output(),
                    duration_ms: 1,
                    graph_key: None,
                    parent_call_id: None,
                },
                TurnEvent::ToolCallStarted {
                    call_id: Some("tool-b".into()),
                    name: "read_file".into(),
                    args: json!({"path": "missing"}),
                    graph_key: None,
                    parent_call_id: None,
                },
                TurnEvent::ToolCallCompleted {
                    call_id: Some("tool-b".into()),
                    name: "read_file".into(),
                    args: json!({"path": "missing"}),
                    output: error_output(),
                    duration_ms: 2,
                    graph_key: None,
                    parent_call_id: None,
                },
                TurnEvent::AssistantProseDelta {
                    text: "finished".into(),
                },
            ]);
        }
        Scenario::Failure | Scenario::Cancellation => {
            events.push(TurnEvent::ToolCallStarted {
                call_id: Some("tool-a".into()),
                name: "shell_run".into(),
                args: json!({"cmd": "sleep 30"}),
                graph_key: None,
                parent_call_id: None,
            });
        }
    }
    events
}

fn remote_event(event: TurnEvent) -> RemoteTurnEvent {
    lash::remote::usage::RemoteTurnActivity::from_core(1, TurnActivity::independent(event))
        .unwrap()
        .event
}

fn remote_tool_output(output: ToolCallOutput) -> Value {
    let remote = remote_event(TurnEvent::ToolCallCompleted {
        call_id: Some("wire-shape".into()),
        name: "fixture".into(),
        args: json!({}),
        output,
        duration_ms: 0,
        graph_key: None,
        parent_call_id: None,
    });
    let RemoteTurnEvent::ToolCallCompleted { output, .. } = remote else {
        unreachable!("constructed a tool completion")
    };
    output
}

fn host_adapter(scenario: Scenario) -> Vec<ExecutorEvent> {
    stream_events(scenario)
        .into_iter()
        .filter_map(|event| {
            let remote = scripted_host_event(remote_event(event));
            host_executor_event(&remote)
        })
        .collect()
}

fn native_adapter(scenario: Scenario) -> Vec<ExecutorEvent> {
    stream_events(scenario)
        .into_iter()
        .enumerate()
        .filter_map(|(index, event)| {
            native_executor_event((index + 1) as u64, TurnActivity::independent(event)).unwrap()
        })
        .collect()
}

fn terminal_projection_for(scenario: Scenario) -> (ExecutorTerminalOutcome, Option<String>) {
    let output = match scenario {
        Scenario::Success => super::tests::test_turn_output(
            lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
                text: "final answer".into(),
            }),
            "final answer",
            tool_records(),
        ),
        Scenario::Failure => super::tests::test_turn_output(
            lash::TurnOutcome::Stopped(lash::TurnStop::RuntimeError),
            "",
            Vec::new(),
        ),
        Scenario::Cancellation => super::tests::test_turn_output(
            lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled {
                evidence: lash::TurnCancellationEvidence::internal("fixture"),
            }),
            "",
            Vec::new(),
        ),
    };
    let (outcome, text, _) = lash_terminal_projection(Some(&output));
    (outcome, text)
}

async fn cli_adapter(scenario: Scenario) -> Vec<ExecutorEvent> {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().join("executor-contract.json");
    let events = stream_events(scenario)
        .into_iter()
        .skip(1)
        .filter_map(|event| match event {
            TurnEvent::ReasoningDelta { text } => Some(SubagentEvent::ReasoningDelta {
                text: text.to_string(),
            }),
            TurnEvent::AssistantProseDelta { text } => Some(SubagentEvent::ProseDelta {
                text: text.to_string(),
            }),
            TurnEvent::ToolCallStarted {
                call_id: Some(call_id),
                name,
                args,
                ..
            } => Some(SubagentEvent::ToolStarted {
                call_id,
                name,
                args,
            }),
            TurnEvent::ToolCallCompleted {
                call_id: Some(call_id),
                name,
                output,
                ..
            } => Some(SubagentEvent::ToolCompleted {
                call_id,
                name,
                ok: output.is_success(),
                output: remote_tool_output(output),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    let terminal = match scenario {
        Scenario::Success => json!({"status": "done", "summary": "done"}),
        Scenario::Failure => {
            json!({"status": "failed", "reason": "Lash executor stopped: RuntimeError"})
        }
        Scenario::Cancellation => json!({"status": "interrupted"}),
    };
    std::fs::write(
        &fixture,
        serde_json::to_vec(&json!({
            "external_id": "executor-conformance-cli",
            "progress": [],
            "events": events,
            "delay_ms": 0,
            "terminal": terminal,
            "assistant_output": matches!(scenario, Scenario::Success).then_some("final answer"),
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
            prompt: "executor conformance".into(),
            cwd: dir.path().to_path_buf(),
            fake_fixture: Some(fixture),
            scoped_mcp: ScopedMcpLaunch {
                host_executable: dir.path().join("host"),
                socket_path: dir.path().join("bridge.sock"),
                capability_file: dir.path().join("capability"),
                expected_tools: vec!["threads_context".into()],
            },
        })
        .await
        .unwrap();
    let mut stream = driver.events(&handle).unwrap();
    let mut adapted = Vec::new();
    while let Some(event) = stream.next().await {
        if let Some(mut event) = cli_executor_event(event) {
            if matches!(scenario, Scenario::Cancellation)
                && matches!(event, ExecutorEvent::Terminal { .. })
            {
                event = ExecutorEvent::Terminal {
                    outcome: ExecutorTerminalOutcome::Cancelled,
                };
            }
            adapted.push(event);
        }
    }
    driver.retire(&handle).await.unwrap();
    adapted
}

async fn adapter_events(backend: Backend, scenario: Scenario) -> Vec<ExecutorEvent> {
    let mut events = match backend {
        Backend::Host => host_adapter(scenario),
        Backend::NativeWorker => native_adapter(scenario),
        Backend::Cli => return cli_adapter(scenario).await,
    };
    let (outcome, final_text) = terminal_projection_for(scenario);
    if let Some(text) = final_text {
        events.push(ExecutorEvent::Final { text });
    }
    events.push(ExecutorEvent::Terminal { outcome });
    events
}

fn normalized_activity(kind: &str, data: &Value) -> Value {
    if kind == "execution_started" {
        json!({"kind": kind})
    } else {
        json!({"kind": kind, "data": data})
    }
}

fn normalized_broadcasts(log: &BroadcastLog, thread_id: u64, turn_id: u64) -> Vec<Value> {
    log.recent()
        .into_iter()
        .filter_map(|frame| match frame {
            HostToClient::TurnEvent {
                thread_id: observed_thread,
                turn_id: observed_turn,
                event,
                ..
            } if observed_thread == thread_id && observed_turn == turn_id => {
                Some(json!({"type": "turn_event", "event": event}))
            }
            HostToClient::AgentActivity {
                thread_id: observed_thread,
                turn_id: observed_turn,
                state,
                text,
            } if observed_thread == thread_id && observed_turn == turn_id => {
                Some(json!({"type": "agent_activity", "state": state, "text": text}))
            }
            HostToClient::ThreadActivity { activity }
                if activity.thread_id == thread_id && activity.turn_id == Some(turn_id) =>
            {
                Some(json!({
                    "type": "thread_activity",
                    "activity": normalized_activity(&activity.kind, &activity.data),
                }))
            }
            HostToClient::Msg { message } if message.thread_id == thread_id => Some(json!({
                "type": "message",
                "body": message.body,
                "tool_calls": message.tool_calls,
            })),
            HostToClient::ThreadTurn { turn } if turn.id == turn_id => Some(json!({
                "type": "thread_turn",
                "state": turn.state,
                "has_message": turn.agent_message_id.is_some(),
            })),
            _ => None,
        })
        .collect()
}

async fn project(backend: Backend, scenario: Scenario) -> Projection {
    let (executor, storage, log, _dir) = super::tests::test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let mut ingest = TurnIngest::new(
        caller.thread_id,
        caller.turn_id,
        json!({"agent": "executor-conformance"}),
    );
    for event in adapter_events(backend, scenario).await {
        ingest.accept(&executor.tools, event).await.unwrap();
    }
    ingest.flush(&executor.tools).await.unwrap();
    let outcome = ingest.terminal().cloned().unwrap();
    let final_text = ingest.final_text().map(str::to_owned);
    let tool_calls = ingest.tool_calls().to_vec();
    let history_id = storage.history_id().await.unwrap();
    let turn = TurnIngest::complete(
        &executor.tools,
        &history_id,
        caller.turn_id,
        outcome,
        final_text,
        tool_calls,
    )
    .await
    .unwrap();
    let detail = storage
        .thread_detail(caller.thread_id, None, 30)
        .await
        .unwrap();
    let timeline = detail
        .turn_timelines
        .iter()
        .find(|timeline| timeline.turn_id == caller.turn_id)
        .unwrap()
        .events
        .iter()
        .map(|event| serde_json::to_value(&event.event).unwrap())
        .collect();
    let tool_activities = detail
        .activities
        .iter()
        .filter(|activity| {
            activity.turn_id == Some(caller.turn_id) && activity.kind == "tool_completed"
        })
        .map(|activity| activity.data.clone())
        .collect();
    let tool_calls = turn
        .agent_message_id
        .and_then(|id| detail.messages.iter().find(|message| message.id == id))
        .map(|message| message.tool_calls.clone())
        .unwrap_or_default();
    Projection {
        timeline,
        tool_activities,
        tool_calls,
        state: turn.state,
        broadcasts: normalized_broadcasts(&log, caller.thread_id, caller.turn_id),
    }
}

#[tokio::test]
async fn every_executor_matches_the_shared_turn_contract() {
    for scenario in Scenario::ALL {
        let expected = project(Backend::Host, scenario).await;
        for backend in Backend::ALL.into_iter().skip(1) {
            let actual = project(backend, scenario).await;
            assert_eq!(
                actual,
                expected,
                "{} backend diverged for {} script",
                backend.name(),
                scenario.name()
            );
        }
        match scenario {
            Scenario::Success => {
                assert_eq!(expected.state, hirsel_proto::ThreadTurnState::Completed);
                assert_eq!(
                    expected
                        .timeline
                        .iter()
                        .map(|event| event["kind"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    [
                        "reasoning",
                        "prose",
                        "tool_start",
                        "tool_done",
                        "tool_start",
                        "tool_done",
                        "prose",
                    ]
                );
                assert_eq!(expected.tool_activities.len(), 2);
                assert_eq!(expected.tool_calls.len(), 2);
                assert!(expected.tool_calls[0].ok);
                assert!(!expected.tool_calls[1].ok);
            }
            Scenario::Failure => {
                assert_eq!(expected.state, hirsel_proto::ThreadTurnState::Failed);
                assert_eq!(expected.timeline.len(), 1);
                assert_eq!(expected.timeline[0]["kind"], "tool_start");
                assert!(expected.tool_activities.is_empty());
                assert!(expected.tool_calls.is_empty());
            }
            Scenario::Cancellation => {
                assert_eq!(expected.state, hirsel_proto::ThreadTurnState::Cancelled);
                assert!(
                    expected
                        .timeline
                        .iter()
                        .all(|event| event["kind"] != "tool_done")
                );
                assert!(expected.tool_activities.is_empty());
                assert!(expected.tool_calls.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn replayed_activity_is_not_broadcast_twice() {
    let (executor, storage, log, _dir) = super::tests::test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let tool = ToolCallSummary {
        id: "tool-a".into(),
        name: "shell_run".into(),
        ok: true,
    };
    for _ in 0..2 {
        TurnIngest::record_tool_completion(
            &executor.tools,
            (caller.thread_id, caller.turn_id),
            &tool,
        )
        .await
        .unwrap();
    }
    let persisted = storage
        .thread_detail(caller.thread_id, None, 30)
        .await
        .unwrap()
        .activities
        .into_iter()
        .filter(|activity| {
            activity.turn_id == Some(caller.turn_id) && activity.kind == "tool_completed"
        })
        .count();
    let broadcast = log
        .recent()
        .into_iter()
        .filter(|frame| {
            matches!(frame, HostToClient::ThreadActivity { activity }
                if activity.turn_id == Some(caller.turn_id) && activity.kind == "tool_completed")
        })
        .count();
    assert_eq!(persisted, 1);
    assert_eq!(broadcast, 1);
}
