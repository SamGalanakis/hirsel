use std::collections::BTreeMap;

use crate::{
    processes::{ProcessRecord, ProcessStatus, ProcessStore},
    storage::{
        AGENT_SESSION_GENERATION_META_KEY, Storage, TOOL_SURFACE_FINGERPRINT_META_KEY,
        TOOL_SURFACE_NAMES_META_KEY,
    },
    tools::{ShellRunOutput, ToolsConfig},
};
use chrono::Utc;
use hirsel_drivers::{SessionHandle, SubagentEvent};
use hirsel_proto::{Blob, ChatAuthor, Ping, PingStatus};
use lash_core::{
    ProcessExecutionEnvRef, ProcessIdentity, ProcessInput, ProcessOriginator, SessionScope,
    TriggerInputBinding, TriggerSubscriptionRecord,
};

#[test]
fn terminal_payload_keeps_full_text_for_wake_and_wait() {
    let full_summary = format!("{}the actual ending", "research findings ".repeat(20));
    let (_, payload) = terminal_event_payload(&TerminalOutcome::Done {
        summary: full_summary.clone(),
    });

    assert_eq!(
        payload["text"],
        format!("Sub-agent completed: {full_summary}")
    );
    assert_eq!(payload["await_output"]["value"]["summary"], full_summary);

    let outcome: ProcessAwaitOutput =
        serde_json::from_value(payload["await_output"].clone()).unwrap();
    let wait_payload = subagents_wait_result("proc-1", &outcome).unwrap();
    assert_eq!(wait_payload["outcome"]["value"]["summary"], full_summary);
}

use super::timers::*;
use super::*;

#[test]
fn observation_resubscribe_backoff_grows_and_resets() {
    let mut backoff = ObservationRetryBackoff::default();
    let first = backoff.next_delay();
    assert!(backoff.next_delay() > first);
    backoff.reset();
    assert_eq!(backoff.next_delay(), first);
}

fn test_turn_output(
    outcome: lash::TurnOutcome,
    safe_text: &str,
    tool_calls: Vec<lash_core::ToolCallRecord>,
) -> lash::TurnOutput {
    lash::TurnOutput {
        result: lash::TurnReport {
            state: lash_core::SessionSnapshot::new(SessionPolicy::new(lash::TurnBudget::Unbounded)),
            outcome,
            acceptance: None,
            assistant_output: lash::turn::AssistantOutput {
                safe_text: safe_text.to_string(),
                raw_text: safe_text.to_string(),
                state: if safe_text.is_empty() {
                    lash_core::facade_support::OutputState::EmptyOutput
                } else {
                    lash_core::facade_support::OutputState::Usable
                },
            },
            usage: lash_core::TokenUsage::default(),
            children_usage: Vec::new(),
            llm_calls: Vec::new(),
            tool_calls,
            execution: lash::TurnExecutionMetrics::default(),
            errors: Vec::new(),
        },
        activities: Vec::new(),
    }
}

#[test]
fn timeline_flushes_prose_before_tool_events() {
    let broadcast_log = BroadcastLog::default();
    let (broadcaster, _) = broadcast::channel(16);
    let mut timeline = TurnTimelineBridge::default();

    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::ModelRequestStarted {
            protocol_iteration: 0,
        }),
        &broadcast_log,
        &broadcaster,
    );
    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::AssistantProseDelta {
            text: "I will ".to_string(),
        }),
        &broadcast_log,
        &broadcaster,
    );
    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::AssistantProseDelta {
            text: "check now.".to_string(),
        }),
        &broadcast_log,
        &broadcaster,
    );
    assert!(turn_events(&broadcast_log).is_empty());

    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::ToolCallStarted {
            call_id: Some("call-1".to_string()),
            name: "shell_run".to_string(),
            args: serde_json::json!({ "cmd": "true" }),
            graph_key: None,
            parent_call_id: None,
        }),
        &broadcast_log,
        &broadcaster,
    );
    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::ToolCallCompleted {
            call_id: Some("call-1".to_string()),
            name: "shell_run".to_string(),
            args: serde_json::json!({ "cmd": "true" }),
            output: serde_json::json!({
                "outcome": {
                    "status": "success",
                    "payload": {
                        "status": 0,
                        "stdout": "",
                        "stderr": "",
                        "timed_out": false
                    }
                }
            }),
            duration_ms: 12,
            graph_key: None,
            parent_call_id: None,
        }),
        &broadcast_log,
        &broadcaster,
    );

    let events = turn_events(&broadcast_log);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].0, 1);
    assert_eq!(
        events[0].1,
        TurnEventKind::Prose {
            text: "I will check now.".to_string()
        }
    );
    assert_eq!(events[1].0, 2);
    assert_eq!(
        events[1].1,
        TurnEventKind::ToolStart {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            summary: Some("cmd: true".to_string())
        }
    );
    assert_eq!(events[2].0, 3);
    assert_eq!(
        events[2].1,
        TurnEventKind::ToolDone {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            ok: true,
            summary: Some("ok status 0".to_string())
        }
    );
}

#[test]
fn code_blocks_stream_full_source_and_pair_with_their_completion() {
    let broadcast_log = BroadcastLog::default();
    let (broadcaster, _) = broadcast::channel(16);
    let mut timeline = TurnTimelineBridge::default();
    let source = "const x = await shell.run({ cmd: \"true\" });\nfinish(x);";

    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::CodeBlockStarted {
            language: "typescript".to_string(),
            code: source.to_string(),
            graph_key: None,
        }),
        &broadcast_log,
        &broadcaster,
    );
    timeline.observe(
        &remote_turn_activity(RemoteTurnEvent::CodeBlockCompleted {
            language: "typescript".to_string(),
            output: "ok".to_string(),
            error: None,
            success: true,
            duration_ms: 42,
            tool_call_ids: vec!["call-1".to_string()],
            graph_key: None,
        }),
        &broadcast_log,
        &broadcaster,
    );

    let events = turn_events(&broadcast_log);
    assert_eq!(events.len(), 2);
    // The full program is carried verbatim — never through the 120-char
    // summary path that tool rows use.
    assert_eq!(
        events[0].1,
        TurnEventKind::CodeStart {
            id: "code:1".to_string(),
            language: "typescript".to_string(),
            code: source.to_string(),
            truncated: false,
        }
    );
    assert_eq!(
        events[1].1,
        TurnEventKind::CodeDone {
            id: "code:1".to_string(),
            ok: true,
            summary: Some("42ms".to_string()),
        }
    );
}

#[test]
fn oversized_code_block_is_clipped_and_flagged() {
    let long = "a".repeat(TURN_EVENT_CODE_BYTES + 10);
    let (clipped, truncated) = clamp_code(&long);
    assert!(truncated);
    assert_eq!(clipped.len(), TURN_EVENT_CODE_BYTES);
    let (kept, truncated) = clamp_code("short");
    assert!(!truncated);
    assert_eq!(kept, "short");
}

#[test]
fn failed_code_block_summary_is_condensed() {
    let summary = code_done_summary(false, Some("Error: boom\n  at line 3\n"), 7).unwrap();
    assert_eq!(summary, "7ms Error: boom");
    assert!(summary.chars().count() <= TURN_EVENT_SUMMARY_CHARS);
}

#[test]
fn cancelled_turn_materializes_checkpointed_chat_and_completed_tools() {
    let output = test_turn_output(
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled {
            evidence: lash::TurnCancellationEvidence::internal("test"),
        }),
        "I checked the durable state.",
        vec![
            lash_core::ToolCallRecord {
                call_id: Some("completed".to_string()),
                tool: "shell_run".to_string(),
                args: serde_json::json!({ "cmd": "true" }),
                output: lash_core::ToolCallOutput::success(serde_json::json!({
                    "status": 0
                })),
                duration_ms: 1,
            },
            lash_core::ToolCallRecord {
                call_id: Some("in-flight".to_string()),
                tool: "shell_run".to_string(),
                args: serde_json::json!({ "cmd": "sleep 30" }),
                output: lash_core::ToolCallOutput::cancelled(lash_core::ToolCancellation::runtime(
                    "turn cancelled",
                )),
                duration_ms: 2,
            },
        ],
    );

    let (body, tool_calls) =
        turn_chat_payload(&output).expect("cancelled checkpoint should become Chat");
    assert_eq!(body, "I checked the durable state.\n\n— interrupted");
    assert_eq!(
        tool_calls,
        vec![ToolCallSummary {
            name: "shell_run".to_string(),
            ok: true,
        }]
    );
}

#[test]
fn completion_winning_cancel_race_keeps_one_normal_terminal_payload() {
    let output = test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: "Completed normally.".to_string(),
        }),
        "Completed normally.",
        Vec::new(),
    );

    let (body, tool_calls) =
        turn_chat_payload(&output).expect("finished turn should become one Chat payload");
    assert_eq!(body, "Completed normally.");
    assert!(tool_calls.is_empty());
    assert!(!body.contains("interrupted"));
}

#[tokio::test]
async fn finished_tool_only_turn_persists_completed_tools() {
    let (executor, storage, _broadcast_log, _dir) = test_event_executor().await;
    let output = test_turn_output(
        lash::TurnOutcome::Finished(lash::TurnFinish::AssistantMessage {
            text: String::new(),
        }),
        "",
        vec![lash_core::ToolCallRecord {
            call_id: Some("completed".to_string()),
            tool: "events_judgment".to_string(),
            args: serde_json::json!({ "question": "Which release path?" }),
            output: lash_core::ToolCallOutput::success(serde_json::json!({
                "event_id": 1
            })),
            duration_ms: 1,
        }],
    );

    assert!(
        materialize_turn_chat(&executor.tools, &output)
            .await
            .unwrap()
    );

    let messages = storage.all_chat().await.unwrap();
    let persisted = messages.last().expect("tool-only Agent Chat row");
    assert_eq!(persisted.author, ChatAuthor::Agent);
    assert!(persisted.body.is_empty());
    assert_eq!(
        persisted.tool_calls,
        vec![ToolCallSummary {
            name: "events_judgment".to_string(),
            ok: true,
        }]
    );
}

#[test]
fn tool_arg_summaries_are_condensed_and_not_json() {
    let summary = condense_args(
        "shell_run",
        &serde_json::json!({
            "cmd": "printf '{\"raw\":true}' && echo done",
            "timeout_secs": 30
        }),
    )
    .unwrap();

    assert!(summary.starts_with("cmd: printf"));
    assert!(!summary.contains("{\""));
    assert!(!summary.contains('{'));
    assert!(!summary.contains('}'));
    assert!(summary.chars().count() <= TURN_EVENT_SUMMARY_CHARS);
}

#[test]
fn tool_result_summaries_include_status_and_error_hint() {
    let ok = condense_result(
        "shell_run",
        &serde_json::json!({ "cmd": "true" }),
        &serde_json::json!({
            "outcome": {
                "status": "success",
                "payload": {
                    "status": 0,
                    "stdout": "",
                    "stderr": "",
                    "timed_out": false
                }
            }
        }),
    )
    .unwrap();
    assert_eq!(ok, "ok status 0");

    let err = condense_result(
        "shell_run",
        &serde_json::json!({ "cmd": "bad" }),
        &serde_json::json!({
            "outcome": {
                "status": "failure",
                "payload": {
                    "message": "failed with {\"raw\":true}"
                }
            }
        }),
    )
    .unwrap();
    assert_eq!(err, "err failed with \"raw\":true");
    assert!(!err.contains("{\""));
}

#[tokio::test]
async fn owner_turn_input_notes_all_attachments_and_references_images() {
    let dir = tempfile::tempdir().unwrap();
    let text_path = dir.path().join("text-blob");
    let image_path = dir.path().join("image-blob");
    tokio::fs::write(&text_path, b"hello").await.unwrap();
    tokio::fs::write(&image_path, [137, 80, 78, 71])
        .await
        .unwrap();
    let text = stored_blob("text-1", "note.txt", "text/plain", 5, text_path);
    let image = stored_blob("image-1", "tiny.png", "image/png", 4, image_path);
    let turn = OwnerTurn {
        message_id: 1,
        client_id: "client-1".to_string(),
        body: "see attached".to_string(),
        anchor: None,
        attachments: vec![text.clone(), image.clone()],
        mentioned_pings: Vec::new(),
        mode: SendMode::Send,
        task_action: None,
    };

    let rendered = owner_turn_text(&turn);
    assert!(rendered.contains(&format!(
        "[attachment stored at {}: note.txt (text/plain, 5 bytes)]",
        text.path.display()
    )));
    assert!(rendered.contains(&format!(
        "[attachment stored at {}: tiny.png (image/png, 4 bytes)]",
        image.path.display()
    )));

    let input = owner_turn_input(&turn).await.unwrap();
    assert_eq!(input.items.len(), 2);
    assert!(matches!(input.items[0], InputItem::Text { .. }));
    let InputItem::Attachment {
        source: lash::direct::AttachmentSource::Inline { media_type, bytes },
    } = &input.items[1]
    else {
        panic!("the image attachment travels inline on the item");
    };
    assert_eq!(media_type.as_str(), "image/png");
    assert_eq!(bytes.as_slice(), &[137, 80, 78, 71]);
}

#[test]
fn owner_turn_text_expands_mentioned_ping_context() {
    let turn = OwnerTurn {
        message_id: 2,
        client_id: "mention-1".to_string(),
        body: "What changed?".to_string(),
        anchor: None,
        attachments: Vec::new(),
        mentioned_pings: vec![Ping {
            id: 7,
            kind: hirsel_proto::EventKind::Judgment,
            source: hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            name: "release-choice".to_string(),
            description: "Choose the release channel".to_string(),
            ui: json!({
                "type": "card",
                "children": [{ "type": "text", "text": "Longer details" }]
            }),
            anchor: 3,
            requires_response: true,
            quick_replies: Vec::new(),
            status: PingStatus::Done,
            read: true,
            archived: false,
            snoozed_until: None,
            archived_at: None,
            fork_sc: None,
            ts: Utc::now(),
        }],
        mode: SendMode::Send,
        task_action: None,
    };

    assert_eq!(
        owner_turn_text(&turn),
        "What changed?\n[mentioned ping @release-choice (ping_id 7, done, requires_response=true, anchor 3): Choose the release channel]"
    );
}

#[test]
fn agent_host_section_references_runtime_config_and_docs_paths() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::tests::test_config(dir.path());
    let section = agent_host_section(&config);
    assert!(section.contains(config.config_path.to_str().unwrap()));
    assert!(section.contains(config.docs_path.to_str().unwrap()));
    assert!(section.contains("## Host configuration"));
}

#[test]
fn tool_surface_fingerprint_uses_names_not_argument_schemas() {
    let first = vec![tool_definition(
        "test.events_notify",
        "events_notify",
        "Notify",
        json!({
            "type": "object",
            "required": ["message"],
            "properties": { "message": { "type": "string" } }
        }),
        json!({ "type": "object" }),
        ["events"],
        "notify",
    )];
    let argument_only_change = vec![tool_definition(
        "test.events_notify",
        "events_notify",
        "Notify with an evolved schema",
        json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "message": { "type": "string" },
                "quiet": { "type": "boolean" }
            }
        }),
        json!({ "type": "object" }),
        ["events"],
        "notify",
    )];
    let mut name_set_change = argument_only_change.clone();
    name_set_change.push(tool_definition(
        "test.events_archive",
        "events_archive",
        "Archive",
        json!({ "type": "object" }),
        json!({ "type": "object" }),
        ["events"],
        "archive",
    ));

    let first = agent_tool_surface(&first).unwrap();
    let argument_only_change = agent_tool_surface(&argument_only_change).unwrap();
    let name_set_change = agent_tool_surface(&name_set_change).unwrap();

    assert_eq!(first.fingerprint, argument_only_change.fingerprint);
    assert_eq!(first.tool_names, vec!["events.notify"]);
    assert_ne!(first.fingerprint, name_set_change.fingerprint);
    assert_eq!(
        name_set_change.tool_names,
        vec!["events.archive", "events.notify"]
    );
}

#[tokio::test]
async fn session_surface_bootstrap_stores_rotates_emits_and_seeds() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let storage = state.storage.clone();
    let initial_definitions = vec![tool_definition(
        "test.events_notify",
        "events_notify",
        "Notify",
        json!({ "type": "object" }),
        json!({ "type": "object" }),
        ["events"],
        "notify",
    )];
    let initial_surface = agent_tool_surface(&initial_definitions).unwrap();

    let first_boot = state
        .tools
        .prepare_agent_session(&initial_surface.fingerprint, &initial_surface.tool_names)
        .await
        .unwrap();
    assert_eq!(first_boot.session_id, "agent");
    assert_eq!(first_boot.handoff_seed, None);
    assert_eq!(
        storage
            .meta_value(TOOL_SURFACE_FINGERPRINT_META_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some(initial_surface.fingerprint.as_str())
    );
    assert_eq!(
        storage
            .meta_value(TOOL_SURFACE_NAMES_META_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some("[\"events.notify\"]")
    );
    assert_eq!(
        storage
            .meta_value(AGENT_SESSION_GENERATION_META_KEY)
            .await
            .unwrap(),
        None
    );
    assert!(!state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event } if event.name == "session-rotated"
    )));

    let reopened_storage = Storage::open(dir.path()).await.unwrap();
    let persisted_boot = reopened_storage
        .reconcile_agent_tool_surface(&initial_surface.fingerprint, &initial_surface.tool_names)
        .await
        .unwrap();
    assert_eq!(persisted_boot.session_id, "agent");
    assert!(!persisted_boot.rotated);
    assert!(persisted_boot.added_tools.is_empty());

    let stable_boot = state
        .tools
        .prepare_agent_session(&initial_surface.fingerprint, &initial_surface.tool_names)
        .await
        .unwrap();
    assert_eq!(stable_boot, first_boot);
    assert!(!state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event } if event.name == "session-rotated"
    )));

    let owner = storage
        .append_chat(ChatAuthor::Owner, "owner turn", None)
        .await
        .unwrap();
    storage
        .append_chat(ChatAuthor::Agent, "The release is ready.", None)
        .await
        .unwrap();
    storage
        .create_event(
            hirsel_proto::EventKind::Judgment,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            "release-channel",
            "Choose stable or beta",
            json!({ "type": "text", "text": "Choose stable or beta" }),
            owner.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut changed_definitions = initial_definitions;
    changed_definitions.push(tool_definition(
        "test.events_archive",
        "events_archive",
        "Archive",
        json!({ "type": "object" }),
        json!({ "type": "object" }),
        ["events"],
        "archive",
    ));
    let changed_surface = agent_tool_surface(&changed_definitions).unwrap();
    let rotated = state
        .tools
        .prepare_agent_session(&changed_surface.fingerprint, &changed_surface.tool_names)
        .await
        .unwrap();

    assert_eq!(rotated.session_id, "agent-g1");
    let seed = rotated.handoff_seed.unwrap();
    assert!(seed.starts_with(
        "Session rotated by the host to pick up new tools: events.archive. Prior conversation summary follows."
    ));
    assert!(seed.contains("- owner: owner turn"));
    assert!(seed.contains("- agent: The release is ready."));
    assert!(seed.contains("- [judgment] release-channel: Choose stable or beta"));
    let guidance = agent_guidance_with_handoff("base guidance".to_string(), Some(&seed));
    assert!(guidance.starts_with("base guidance\n\n## Session handoff\n\n"));
    assert!(guidance.ends_with(&seed));
    assert_eq!(
        storage
            .meta_value(AGENT_SESSION_GENERATION_META_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some("1")
    );
    let emitted = storage
        .all_pings()
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.name == "session-rotated")
        .unwrap();
    assert_eq!(emitted.name, "session-rotated");
    assert_eq!(emitted.kind, hirsel_proto::EventKind::Info);
    assert_eq!(
        emitted.source.kind,
        hirsel_proto::EventSourceKind::Scheduled
    );
    assert_eq!(emitted.source.r#ref.as_deref(), Some("agent-g1"));
    assert!(emitted.description.contains("events.archive"));
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event } if event.id == emitted.id
    )));

    state.broadcast_log.clear();
    let stable_generation = state
        .tools
        .prepare_agent_session(&changed_surface.fingerprint, &changed_surface.tool_names)
        .await
        .unwrap();
    assert_eq!(stable_generation.session_id, "agent-g1");
    assert_eq!(stable_generation.handoff_seed, None);
    assert!(!state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event } if event.name == "session-rotated"
    )));
}

#[test]
fn event_tool_schemas_teach_keyless_judgments_and_bound_option_count() {
    let definitions = hirsel_tool_definitions(&crate::subagent_models::registry_catalog());
    let judgment = definitions
        .iter()
        .find(|definition| definition.name() == "events_judgment")
        .unwrap();
    let validator =
        jsonschema::JSONSchema::compile(judgment.contract.input_schema.canonical()).unwrap();
    let options = |count: usize| {
        (0..count)
            .map(|index| {
                json!({
                    "label": format!("Option {}", index + 1),
                    "detail": format!("Tradeoff {}", index + 1)
                })
            })
            .collect::<Vec<_>>()
    };

    assert!(
        validator
            .validate(&json!({
                "question": "Which release path?",
                "options": options(2)
            }))
            .is_ok()
    );
    for count in [1, 5] {
        assert!(
            validator
                .validate(&json!({
                    "question": "Which release path?",
                    "options": options(count)
                }))
                .is_err()
        );
    }
    assert!(judgment.description().contains("events.judgment({"));
    assert!(judgment.description().contains("Supply 2–4 options"));
    assert!(judgment.description().contains("only paraphrases it"));

    let archive = definitions
        .iter()
        .find(|definition| definition.name() == "events_archive")
        .unwrap();
    assert!(archive.description().contains("Sam's feed hides it"));
    assert!(archive.description().contains("snoozed Event"));
    let clear = definitions
        .iter()
        .find(|definition| definition.name() == "events_clear")
        .unwrap();
    assert!(clear.description().contains("clear my feed"));
    assert!(clear.description().contains("snoozed judgments"));
    let clear_validator =
        jsonschema::JSONSchema::compile(clear.contract.input_schema.canonical()).unwrap();
    assert!(clear_validator.validate(&json!({})).is_ok());
    assert!(clear_validator.validate(&json!({ "all": true })).is_err());

    let alias = definitions
        .iter()
        .find(|definition| definition.name() == "pings_send")
        .unwrap();
    assert!(
        alias
            .description()
            .contains("deprecated: use events.judgment / events.notify")
    );
}

#[tokio::test]
async fn cancelled_turn_persists_and_broadcasts_the_normal_chat_shape() {
    let (executor, storage, broadcast_log, _dir) = test_event_executor().await;
    broadcast_log.clear();
    let output = test_turn_output(
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled {
            evidence: lash::TurnCancellationEvidence::internal("test"),
        }),
        "The completed check passed.",
        vec![lash_core::ToolCallRecord {
            call_id: Some("completed".to_string()),
            tool: "shell_run".to_string(),
            args: serde_json::json!({ "cmd": "true" }),
            output: lash_core::ToolCallOutput::success(serde_json::json!({ "status": 0 })),
            duration_ms: 1,
        }],
    );

    assert!(
        materialize_turn_chat(&executor.tools, &output)
            .await
            .unwrap()
    );

    let messages = storage.all_chat().await.unwrap();
    let persisted = messages.last().expect("persisted partial Agent message");
    assert_eq!(persisted.author, ChatAuthor::Agent);
    assert_eq!(
        persisted.body,
        "The completed check passed.\n\n— interrupted"
    );
    assert_eq!(
        persisted.tool_calls,
        vec![ToolCallSummary {
            name: "shell_run".to_string(),
            ok: true,
        }]
    );
    let broadcasts = broadcast_log.recent();
    assert_eq!(
        broadcasts
            .iter()
            .filter(|frame| matches!(frame, HostToClient::Msg { .. }))
            .count(),
        1
    );
    assert!(broadcasts.iter().any(|frame| matches!(
        frame,
        HostToClient::Msg { message, sc: None } if message == persisted
    )));
}

async fn test_process_registry(dir: &std::path::Path) -> Arc<dyn lash::process::ProcessRegistry> {
    Arc::new(
        lash_sqlite_store::SqliteProcessRegistry::open(
            &dir.join("test-processes.db"),
            dir.join("test-sessions"),
        )
        .await
        .unwrap(),
    )
}

async fn test_event_executor() -> (HirselToolExecutor, Storage, BroadcastLog, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let storage = Storage::open(&path).await.unwrap();
    let owner = storage
        .append_chat(ChatAuthor::Owner, "owner turn", None)
        .await
        .unwrap();
    let (broadcaster, _) = broadcast::channel(16);
    let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views =
        crate::templates::ViewManager::new(templates, broadcaster.clone(), broadcast_log.clone());
    let config_store = crate::host_config::ConfigStore::load(
        path.join("hirsel.toml"),
        &path,
        std::path::Path::new("/docs/hirsel-config.md"),
        &crate::host_config::EnvBootstrap::default(),
    )
    .await
    .unwrap();
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: DriverMode::Fake,
            fake_fixture: None,
            subagent_models: crate::subagent_models::SubagentModelState::load(config_store),
        },
        storage.clone(),
        broadcaster,
        broadcast_log.clone(),
        ProcessStore::default(),
        pushes,
        views,
    );
    let anchors = Arc::new(Mutex::new(TurnAnchorState {
        active: Some(TurnAnchors {
            owner_message_id: owner.id,
            task_action_event_id: None,
        }),
        ..TurnAnchorState::default()
    }));
    let process_registry = test_process_registry(dir.path()).await;
    (
        HirselToolExecutor {
            tools,
            anchors,
            process_registry,
        },
        storage,
        broadcast_log,
        dir,
    )
}

#[tokio::test]
async fn recompose_tool_is_bound_to_the_active_generated_task_action() {
    let (executor, storage, broadcast_log, _dir) = test_event_executor().await;
    let owner = storage
        .append_chat(ChatAuthor::Agent, "Adaptive Task", None)
        .await
        .unwrap();
    let event = storage
        .create_event(
            hirsel_proto::EventKind::Judgment,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            "adaptive",
            "Advance the Task",
            json!({
                "type": "card",
                "children": [{ "type": "submit", "action": "advance", "label": "Continue", "settles": false }]
            }),
            owner.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let args = json!({
        "event_id": event.id,
        "ui": {
            "type": "card",
            "children": [{ "type": "heading", "text": "Next stage", "level": 2 }]
        }
    });

    let inactive = executor.events_recompose(&args).await.unwrap_err();
    assert!(inactive.contains("active generated Task action turn"));
    executor
        .anchors
        .lock()
        .await
        .active
        .as_mut()
        .unwrap()
        .task_action_event_id = Some(event.id);
    let wrong = executor
        .events_recompose(&json!({
            "event_id": event.id + 1,
            "ui": { "type": "text", "text": "wrong" }
        }))
        .await
        .unwrap_err();
    assert!(wrong.contains(&format!("only update Task {}", event.id)));
    let invalid = executor
        .events_recompose(&json!({
            "event_id": event.id,
            "ui": { "type": "arbitrary-client-widget", "html": "<script>" }
        }))
        .await
        .unwrap_err();
    assert!(invalid.contains("unknown Task UI component"));
    let result = executor.events_recompose(&args).await.unwrap();
    assert_eq!(result, json!({ "event_id": event.id, "status": "open" }));
    let updated = storage.ping(event.id).await.unwrap().unwrap();
    assert_eq!(updated.id, event.id);
    assert_eq!(updated.anchor, event.anchor);
    assert_eq!(updated.ui["children"][0]["text"], "Next stage");
    assert!(broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event: update } if update.id == event.id
    )));
}

#[tokio::test]
async fn event_tools_emit_typed_events_and_deprecated_alias_still_works() {
    let (executor, storage, _broadcast_log, _dir) = test_event_executor().await;
    let judgment_args = json!({
        "question": "Which release path?",
        "context": "Stable limits the blast radius; edge reaches testers sooner.",
        "options": [
            { "label": "Stable", "detail": "Lower rollout risk." },
            { "label": "Edge", "detail": "Faster feedback." },
            { "label": "Hold", "detail": "More validation time." }
        ]
    });
    let judgment = executor.events_judgment(&judgment_args).await.unwrap();
    let info = executor
        .events_notify(&json!({
            "name": "tests-green",
            "description": "The release suite passed"
        }))
        .await
        .unwrap();
    let summary = executor
        .events_summary(&json!({
            "name": "daily-digest",
            "description": "Fleet digest ready",
            "content_md": "Three branches landed; one judgment remains."
        }))
        .await
        .unwrap();
    let alias = executor
        .pings_send(&json!({
            "name": "alias-choice",
            "description": "Which alias path?",
            "content_md": "One path preserves compatibility; the other removes old callers.",
            "requires_response": true,
            "options": [
                { "label": "Preserve", "detail": "Keeps old callers working." },
                { "label": "Remove", "detail": "Shrinks the surface." }
            ]
        }))
        .await
        .unwrap();

    assert_eq!(judgment["kind"], "judgment");
    assert_eq!(info["kind"], "info");
    assert_eq!(summary["kind"], "summary");
    for (result, kind) in [
        (&info, hirsel_proto::EventKind::Info),
        (&summary, hirsel_proto::EventKind::Summary),
    ] {
        assert_eq!(
            storage
                .ping(result["event_id"].as_u64().unwrap())
                .await
                .unwrap()
                .unwrap()
                .kind,
            kind
        );
    }
    let event = storage
        .ping(judgment["event_id"].as_u64().unwrap())
        .await
        .unwrap()
        .unwrap();
    let options = event.ui["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["type"] == "optionList")
        .unwrap()["options"]
        .as_array()
        .unwrap();
    assert_eq!(options[0]["key"], "A");
    assert_eq!(options[1]["key"], "B");
    assert_eq!(options[2]["key"], "C");
    assert_eq!(options[0]["recommended"], true);
    assert_eq!(options[1]["recommended"], false);

    let alias_event = storage
        .ping(alias["ping_id"].as_u64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alias_event.kind, hirsel_proto::EventKind::Judgment);

    let judgment_id = judgment["event_id"].as_u64().unwrap();
    let archived = executor
        .events_archive(&json!({ "event_id": judgment_id }))
        .await
        .unwrap();
    assert_eq!(archived["event_id"], judgment_id);
    assert_eq!(archived["status"], "done");
    assert_eq!(archived["archived"], true);

    let info_id = info["event_id"].as_u64().unwrap();
    storage.mark_ping_read(info_id).await.unwrap();
    assert_eq!(executor.events_clear().await.unwrap()["count"], 1);
    assert_eq!(executor.events_clear().await.unwrap()["count"], 0);
    let cleared = storage.ping(info_id).await.unwrap().unwrap();
    assert!(cleared.archived);
    assert_eq!(cleared.status, PingStatus::Done);

    for count in [1, 5] {
        let mut invalid = judgment_args.clone();
        invalid["options"] = Value::Array(
            (0..count)
                .map(|index| {
                    json!({
                        "label": format!("Option {index}"),
                        "detail": format!("Tradeoff {index}")
                    })
                })
                .collect(),
        );
        assert_eq!(
            executor.events_judgment(&invalid).await.unwrap_err(),
            "judgment events require 2–4 options"
        );
    }
}

/// lash refuses to register an RLM catalog member whose model-facing prose
/// names any registered dialect — one authored string is served to sessions
/// of every dialect, so naming even the active one is wrong. That refusal
/// happens when a session opens; catching it here means a description
/// written with a dialect word fails the build instead of the host's boot.
#[test]
fn tool_prose_never_names_a_dialect() {
    fn collect_prose(schema: &Value, out: &mut Vec<String>) {
        match schema {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "description"
                        && let Some(text) = value.as_str()
                    {
                        out.push(text.to_string());
                    }
                    collect_prose(value, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_prose(item, out);
                }
            }
            _ => {}
        }
    }

    let markers = RlmDialect::ALL
        .iter()
        .flat_map(|dialect| {
            let id = dialect.language_id();
            [id.to_string(), format!("<{id}>"), format!("</{id}>")]
        })
        .collect::<Vec<_>>();

    for definition in hirsel_tool_definitions(&crate::subagent_models::registry_catalog()) {
        let mut prose = vec![definition.description().to_string()];
        collect_prose(definition.contract.input_schema.canonical(), &mut prose);
        collect_prose(definition.contract.output_schema.canonical(), &mut prose);
        for text in prose {
            let lowered = text.to_lowercase();
            for marker in &markers {
                assert!(
                    !lowered.contains(marker),
                    "tool `{}` names the `{marker}` dialect in model-facing prose: {text}",
                    definition.name()
                );
            }
        }
    }
}

#[test]
fn subagent_spawn_schema_rejects_model_aliases() {
    let definitions = hirsel_tool_definitions(&crate::subagent_models::registry_catalog());
    let spawn = definitions
        .iter()
        .find(|definition| definition.name() == "subagents_spawn")
        .unwrap();
    let validator =
        jsonschema::JSONSchema::compile(spawn.contract.input_schema.canonical()).unwrap();

    assert!(
        validator
            .validate(&json!({
                "agent": "claude",
                "model": "opus",
                "effort": "high",
                "prompt": "Research Linear triage."
            }))
            .is_err(),
        "the model-facing contract must reject aliases the Host cannot execute"
    );
    assert!(
        validator
            .validate(&json!({
                "agent": "claude",
                "model": "claude-opus-5",
                "effort": "high",
                "prompt": "Research Linear triage."
            }))
            .is_ok()
    );
    assert!(
        validator
            .validate(&json!({
                "agent": "codex",
                "model": "gpt-5.6-luna",
                "variant": "max",
                "prompt": "Audit this repository."
            }))
            .is_ok(),
        "Codex-only variants must be represented by the generated contract"
    );
    assert!(
        validator
            .validate(&json!({
                "agent": "codex",
                "model": "gpt-5.6-luna",
                "variant": "high",
                "prompt": "Audit this repository."
            }))
            .is_err(),
        "each lane carries exactly one effort; there is no per-task tuning"
    );
    assert!(
        validator
            .validate(&json!({
                "agent": "claude",
                "model": "gpt-5.6-sol",
                "variant": "high",
                "prompt": "Audit this repository."
            }))
            .is_err(),
        "the generated contract must reject models from another provider"
    );
}

#[tokio::test]
async fn subagent_tool_provider_resolves_the_current_settings_schema() {
    let (executor, _storage, _broadcast_log, dir) = test_event_executor().await;
    let provider = HirselToolProvider { executor };
    let opus_spawn = json!({
        "agent": "claude",
        "model": "claude-opus-5",
        "effort": "high",
        "prompt": "Research Linear triage."
    });
    let before = provider.resolve_contract("subagents_spawn").unwrap();
    let before = jsonschema::JSONSchema::compile(before.input_schema.canonical()).unwrap();
    assert!(before.validate(&opus_spawn).is_ok());

    let store = crate::host_config::ConfigStore::load(
        dir.path().join("hirsel.toml"),
        dir.path(),
        std::path::Path::new("/docs/hirsel-config.md"),
        &crate::host_config::EnvBootstrap::default(),
    )
    .await
    .unwrap();
    store
        .set_subagent_model("claude", "claude-opus-5", false, &["high".to_string()])
        .await
        .unwrap();

    let after = provider.resolve_contract("subagents_spawn").unwrap();
    let after = jsonschema::JSONSchema::compile(after.input_schema.canonical()).unwrap();
    assert!(
        after.validate(&opus_spawn).is_err(),
        "a fresh contract resolution must reflect Settings without rebuilding the provider"
    );
}

#[tokio::test]
async fn pings_send_uses_active_turn_anchor_when_later_owner_message_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let owner_a = storage
        .append_chat(ChatAuthor::Owner, "owner A", None)
        .await
        .unwrap();
    let owner_b = storage
        .append_chat(ChatAuthor::Owner, "owner B", None)
        .await
        .unwrap();
    let (broadcaster, _) = broadcast::channel(16);
    let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views =
        crate::templates::ViewManager::new(templates, broadcaster.clone(), broadcast_log.clone());
    let config_store = crate::host_config::ConfigStore::load(
        dir.path().join("hirsel.toml"),
        dir.path(),
        std::path::Path::new("/docs/hirsel-config.md"),
        &crate::host_config::EnvBootstrap::default(),
    )
    .await
    .unwrap();
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: DriverMode::Fake,
            fake_fixture: None,
            subagent_models: crate::subagent_models::SubagentModelState::load(config_store),
        },
        storage,
        broadcaster,
        broadcast_log,
        ProcessStore::default(),
        pushes,
        views,
    );
    let anchors = Arc::new(Mutex::new(TurnAnchorState::default()));
    {
        let mut anchors = anchors.lock().await;
        anchors.pending_by_source_key.insert(
            owner_turn_source_key("client-a"),
            TurnAnchors {
                owner_message_id: owner_a.id,
                task_action_event_id: None,
            },
        );
        anchors.active = Some(TurnAnchors {
            owner_message_id: owner_a.id,
            task_action_event_id: None,
        });
        anchors.pending_by_source_key.insert(
            owner_turn_source_key("client-b"),
            TurnAnchors {
                owner_message_id: owner_b.id,
                task_action_event_id: None,
            },
        );
    }
    let executor = HirselToolExecutor {
        tools,
        anchors,
        process_registry: test_process_registry(dir.path()).await,
    };

    let result = executor
        .pings_send(&serde_json::json!({
            "name": "active-turn-result",
            "description": "Result for the active turn",
            "content": "The owner needs this result before deployment can continue.",
            "requires_response": true,
            "options": [
                {
                    "key": "A",
                    "label": "Accept",
                    "detail": "Accept the result",
                    "recommended": true
                },
                {
                    "key": "B",
                    "label": "Revise",
                    "detail": "Revise the result"
                }
            ]
        }))
        .await
        .unwrap();

    assert_eq!(result["anchor"], owner_a.id);
    assert_eq!(result["ping_id"], 1);
}

#[test]
fn every_executor_result_matches_its_declared_output_schema() {
    let now = Utc::now();
    let ping = Ping {
        id: 7,
        kind: hirsel_proto::EventKind::Judgment,
        source: hirsel_proto::EventSource {
            kind: hirsel_proto::EventSourceKind::Agent,
            r#ref: None,
        },
        name: "choose-release".to_string(),
        description: "Choose a release channel".to_string(),
        ui: json!({
            "type": "card",
            "children": [{ "type": "text", "text": "Choose a release" }]
        }),
        anchor: 3,
        requires_response: true,
        quick_replies: vec![QuickReply {
            value: "stable".to_string(),
            label: "Stable".to_string(),
        }],
        status: PingStatus::Done,
        read: true,
        archived: true,
        snoozed_until: None,
        archived_at: Some(now),
        fork_sc: None,
        ts: now,
    };
    let events = vec![
        SubagentEvent::Started {
            external_id: "driver-session-1".to_string(),
        },
        SubagentEvent::Progress {
            summary: "running tests".to_string(),
        },
        SubagentEvent::Terminal {
            outcome: TerminalOutcome::Done {
                summary: "tests passed".to_string(),
            },
        },
    ];
    let process = ProcessRecord::restored(
        "proc-1".to_string(),
        AgentKind::Codex,
        Some("gpt-test".to_string()),
        SessionHandle {
            id: "driver-session-1".to_string(),
            agent: AgentKind::Codex,
        },
        "Run the tests".to_string(),
        "/tmp/repo".to_string(),
        Some("external-1".to_string()),
        ProcessStatus::Done,
        events.clone(),
        now,
        now,
    );
    let monitor = MonitorRecord {
        id: "monitor-1".to_string(),
        cmd: "test -f done".to_string(),
        every_secs: 30,
        wake_on: MonitorWakeOn::Regex,
        pattern: Some("ready".to_string()),
        label: "build ready".to_string(),
        created_ts: now,
        last_event_ts: now,
        last_run_ts: Some(now),
        last_output: Some("ready".to_string()),
        summary: Some("matched".to_string()),
        cancelled_ts: Some(now),
    };
    let wait_outcomes = [
        ProcessAwaitOutput::Success {
            value: json!({ "summary": "done" }),
            control: None,
        },
        ProcessAwaitOutput::Failure {
            class: lash_core::ToolFailureClass::Execution,
            code: "subagent_failed".to_string(),
            message: "failed".to_string(),
            raw: Some(json!({ "reason": "failed" })),
            control: None,
        },
        ProcessAwaitOutput::Cancelled {
            message: "interrupted".to_string(),
            raw: None,
            control: None,
        },
        ProcessAwaitOutput::Abandoned {
            evidence: Box::new(lash_core::AbandonEvidence {
                writer: lash_core::AbandonWriter::ReconciledRequest,
                owner: None,
                epoch_ms: 42,
            }),
            control: None,
        },
    ];

    let mut results = BTreeMap::<&str, Vec<Value>>::new();
    let mut info = ping.clone();
    info.kind = hirsel_proto::EventKind::Info;
    info.requires_response = false;
    let mut summary = ping.clone();
    summary.kind = hirsel_proto::EventKind::Summary;
    summary.requires_response = false;
    results.insert("events_judgment", vec![event_send_result(&ping)]);
    results.insert("events_notify", vec![event_send_result(&info)]);
    results.insert("events_summary", vec![event_send_result(&summary)]);
    results.insert(
        "events_recompose",
        vec![json!({ "event_id": ping.id, "status": "open" })],
    );
    results.insert("events_archive", vec![event_archive_result(&ping)]);
    results.insert("events_clear", vec![events_clear_result(3)]);
    results.insert("pings_send", vec![pings_send_result(&ping)]);
    results.insert(
        "pings_resolve",
        vec![
            pings_resolve_result(Some(&ping)).unwrap(),
            pings_resolve_result(None).unwrap(),
        ],
    );
    let view = hirsel_proto::ViewInstance {
        instance_id: "view-1".to_string(),
        placement: "canvas".to_string(),
        spec: json!({ "type": "text", "text": "Ready" }),
    };
    results.insert("views_show", vec![view_instance_result(&view)]);
    results.insert("views_update", vec![view_instance_result(&view)]);
    results.insert(
        "views_clear",
        vec![json!({ "ok": true, "instance_id": "view-1" })],
    );
    results.insert(
        "views_list_templates",
        vec![json!([{ "id": "status", "title": "Status" }])],
    );
    results.insert("subagents_spawn", vec![subagent_spawn_result("proc-1")]);
    results.insert("subagents_prompt", vec![acknowledgement_result()]);
    results.insert("subagents_interrupt", vec![acknowledgement_result()]);
    results.insert(
        "subagents_list",
        vec![subagents_list_result(std::slice::from_ref(&process)).unwrap()],
    );
    results.insert(
        "subagents_progress",
        vec![
            subagents_progress_result(Some(&process), &events).unwrap(),
            subagents_progress_result(None, &[]).unwrap(),
        ],
    );
    results.insert(
        "subagents_wait",
        wait_outcomes
            .iter()
            .map(|outcome| subagents_wait_result("proc-1", outcome).unwrap())
            .collect(),
    );
    results.insert(
        "monitors_create",
        vec![monitors_create_result(&monitor).unwrap()],
    );
    results.insert(
        "monitors_list",
        vec![monitors_list_result(std::slice::from_ref(&monitor)).unwrap()],
    );
    results.insert("monitors_cancel", vec![monitors_cancel_result("monitor-1")]);
    results.insert(
        "shell_run",
        vec![
            shell_run_result(&ShellRunOutput {
                status: Some(0),
                stdout: "done\n".to_string(),
                stderr: String::new(),
                timed_out: false,
            })
            .unwrap(),
            shell_run_result(&ShellRunOutput {
                status: None,
                stdout: String::new(),
                stderr: "timed out".to_string(),
                timed_out: true,
            })
            .unwrap(),
        ],
    );

    let definitions = hirsel_tool_definitions(&crate::subagent_models::registry_catalog());
    assert_eq!(results.len(), definitions.len());
    for definition in definitions {
        let examples = results
            .get(definition.name())
            .unwrap_or_else(|| panic!("missing result examples for {}", definition.name()));
        let schema = definition.contract.output_schema.canonical();
        let validator = jsonschema::JSONSchema::compile(schema)
            .unwrap_or_else(|error| panic!("invalid schema for {}: {error}", definition.name()));
        for example in examples {
            if let Err(errors) = validator.validate(example) {
                let errors = errors.map(|error| error.to_string()).collect::<Vec<_>>();
                panic!(
                    "result for {} did not match its schema: {errors:?}\nresult: {example}",
                    definition.name()
                );
            }
        }
    }
}

fn remote_turn_activity(event: RemoteTurnEvent) -> RemoteSessionObservationEventPayload {
    RemoteSessionObservationEventPayload::TurnActivity {
        activity: Box::new(lash::remote::usage::RemoteTurnActivity {
            protocol_version: lash::remote::REMOTE_PROTOCOL_VERSION,
            sequence: 1,
            id: "activity-1".to_string(),
            correlation_id: "turn-1".to_string(),
            event,
        }),
    }
}

fn turn_events(broadcast_log: &BroadcastLog) -> Vec<(u64, TurnEventKind)> {
    broadcast_log
        .recent()
        .into_iter()
        .filter_map(|event| match event {
            HostToClient::TurnEvent { seq, event, .. } => Some((seq, event)),
            _ => None,
        })
        .collect()
}

#[test]
fn timer_in_secs_becomes_one_shot_due_from_registration_time() {
    let record = timer_registration(
        serde_json::json!({
            "label": "ping",
            "in_secs": 5
        }),
        1_000,
    );
    let schedule = TimerSchedule::from_registration(&record).unwrap();

    assert!(schedule.due_occurrence(&record, 5_999).is_none());
    let occurrence = schedule.due_occurrence(&record, 6_000).unwrap();
    assert!(occurrence.one_shot);
    assert_eq!(occurrence.label, "ping");
    assert_eq!(occurrence.scheduled_at_ms, 6_000);
    assert_eq!(occurrence.idempotency_key, "timer:source-key:once:6000");
}

#[test]
fn timer_every_secs_uses_sixty_second_floor() {
    let record = timer_registration(
        serde_json::json!({
            "label": "heartbeat",
            "every_secs": 5
        }),
        1_000,
    );
    let schedule = TimerSchedule::from_registration(&record).unwrap();

    assert_eq!(schedule.every_secs, Some(TIMER_MIN_RECURRING_SECS));
    assert!(schedule.due_occurrence(&record, 60_999).is_none());
    let occurrence = schedule.due_occurrence(&record, 61_000).unwrap();
    assert!(!occurrence.one_shot);
    assert_eq!(occurrence.scheduled_at_ms, 61_000);
    assert_eq!(occurrence.idempotency_key, "timer:source-key:every:1");
}

#[test]
fn timer_schedule_requires_exactly_one_clock_field() {
    let record = timer_registration(
        serde_json::json!({
            "label": "bad",
            "in_secs": 5,
            "every_secs": 60
        }),
        1_000,
    );

    let error = TimerSchedule::from_registration(&record).unwrap_err();
    assert!(error.contains("exactly one"));
}

#[test]
fn digest_timer_labels_select_the_scheduled_event_producer() {
    assert_eq!(
        scheduled_digest_label("digest: Morning fleet"),
        Some("Morning fleet")
    );
    assert_eq!(scheduled_digest_label("digest:   "), None);
    assert_eq!(scheduled_digest_label("ordinary timer"), None);
}

fn stored_blob(id: &str, name: &str, mime: &str, size: u64, path: PathBuf) -> StoredBlob {
    StoredBlob {
        blob: Blob {
            id: id.to_string(),
            name: name.to_string(),
            mime: mime.to_string(),
            size,
        },
        path,
        created_ts: Utc::now(),
    }
}

fn timer_registration(value: Value, created_at_ms: u64) -> TriggerSubscriptionRecord {
    TriggerSubscriptionRecord {
        subscription_id: "subscription-id".to_string(),
        owner_scope: lash::triggers::TriggerOwnerScope::session("agent"),
        subscription_key: "subscription-key".to_string(),
        incarnation: "incarnation-1".to_string(),
        revision: 1,
        definition_fingerprint: "fingerprint".to_string(),
        registrant: ProcessOriginator::session(SessionScope::new("agent")),
        env_ref: ProcessExecutionEnvRef::new("process-env:test"),
        wake_target: Some(SessionScope::new("agent")),
        name: None,
        source_type: TIMER_SOURCE_TYPE.to_string(),
        source_key: "source-key".to_string(),
        source: serde_json::json!({
            "$lash_host_descriptor_type": TIMER_SOURCE_TYPE,
            "$lash_host_descriptor_value": value,
        }),
        payload_schema: LashSchema::new(serde_json::json!({ "type": "object" })),
        target: ProcessInput::External {
            metadata: serde_json::json!({}),
        },
        target_identity: ProcessIdentity::new("timer-test"),
        event_types: Vec::new(),
        input_template: BTreeMap::<String, TriggerInputBinding>::new(),
        target_label: None,
        enabled: true,
        tombstoned: false,
        deleted_at_ms: None,
        created_at_ms,
        updated_at_ms: created_at_ms,
    }
}

/// A plugin whose only surface is one agent tool.
struct CatalogTestPlugin;

#[hirsel_plugin_api::async_trait]
impl hirsel_plugin_api::Plugin for CatalogTestPlugin {
    fn id(&self) -> &'static str {
        "catalog-test"
    }

    fn label(&self) -> &'static str {
        "Catalog test"
    }

    fn tools(&self) -> Vec<hirsel_plugin_api::PluginTool> {
        vec![hirsel_plugin_api::PluginTool::new(
            "ping",
            "Reply with pong.",
            serde_json::json!({ "type": "object", "properties": {} }),
            |_ctx, _args| async move { Ok(serde_json::json!({ "pong": true })) },
        )]
    }
}

/// Plugin tools are not a parallel catalog: they are ordinary definitions on
/// the same provider, so they resolve a contract, appear in the manifest list,
/// and feed the tool-surface fingerprint exactly like a built-in does.
#[tokio::test]
async fn plugin_tools_join_the_real_agent_tool_catalog() {
    let (executor, storage, _broadcast_log, _dir) = test_event_executor().await;
    let (broadcaster, _keepalive) = broadcast::channel(8);
    let host = crate::plugins::PluginHost::start(
        vec![hirsel_plugin_api::PluginRegistration::new(
            Box::new(CatalogTestPlugin),
            "1.0.0",
            "plugins/catalog-test",
        )],
        storage,
        executor.tools.clone(),
        broadcaster,
        BroadcastLog::default(),
        crate::plugins::SupervisorConfig::default(),
    )
    .await
    .unwrap();

    let tools = executor.tools.clone();
    let provider = HirselToolProvider { executor };
    assert!(
        provider
            .tool_manifests()
            .iter()
            .any(|manifest| manifest.name == "plugin__catalog_test__ping"),
        "an enabled plugin's tool must be advertised by the agent tool provider"
    );
    assert!(
        provider
            .resolve_contract("plugin__catalog_test__ping")
            .is_some(),
        "the plugin tool must resolve a contract through the normal path"
    );

    let with_plugin = agent_tool_surface(&provider.definitions()).unwrap();
    assert!(
        with_plugin
            .tool_names
            .contains(&"plugins.catalog_test.ping".to_string()),
        "the plugin tool binds into the lashlang surface as plugins.<id>.<tool>"
    );

    // Dispatch runs the plugin's handler and returns its JSON verbatim.
    let result = tools
        .plugin_tools()
        .call("plugin__catalog_test__ping", serde_json::json!({}))
        .await
        .expect("registered plugin tool")
        .unwrap();
    assert_eq!(result, serde_json::json!({ "pong": true }));

    // Disabling drops it back out of the same catalog, and the surface
    // fingerprint moves with it.
    host.set_enabled("catalog-test", false).await.unwrap();
    assert!(
        !provider
            .tool_manifests()
            .iter()
            .any(|manifest| manifest.name == "plugin__catalog_test__ping")
    );
    let without_plugin = agent_tool_surface(&provider.definitions()).unwrap();
    assert_ne!(
        without_plugin.fingerprint, with_plugin.fingerprint,
        "toggling a plugin rotates the tool-surface fingerprint"
    );
}

/// Lash refuses a process registration whose input is `Engine` (or `ToolCall`)
/// unless it names a captured execution env, so every hirsel start builder for
/// an engine row must declare one and it must survive into the registration.
#[test]
fn engine_start_requests_declare_a_captured_execution_env() {
    let now = Utc::now();
    let monitor = MonitorRecord {
        id: "monitor-1".to_string(),
        cmd: "test -f done".to_string(),
        every_secs: 30,
        wake_on: MonitorWakeOn::Regex,
        pattern: Some("ready".to_string()),
        label: "build ready".to_string(),
        created_ts: now,
        last_event_ts: now,
        last_run_ts: None,
        last_output: None,
        summary: None,
        cancelled_ts: None,
    };
    let policy = SessionPolicy::new(lash::TurnBudget::Unbounded);
    let requests = [
        subagent_start_request(
            "proc-1",
            "agent",
            json!({ "prompt": "go", "cwd": "/tmp" }),
            host_process_env_spec(policy.clone()),
        ),
        monitor_start_request(&monitor, "agent", host_process_env_spec(policy)),
    ];

    for request in requests {
        assert!(
            matches!(request.input, ProcessInput::Engine { .. }),
            "start builder no longer produces an engine row"
        );
        let env_spec = request
            .env_spec
            .clone()
            .expect("engine start declares an execution env");
        let env_ref = env_spec.stable_ref().expect("stable execution env ref");
        let registration = request.into_registration(Some(env_ref.clone()));
        assert_eq!(registration.env_ref, Some(env_ref));
    }
}
