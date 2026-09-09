use std::collections::BTreeMap;

use crate::{
    processes::{ProcessRecord, ProcessStatus, ProcessStore},
    storage::Storage,
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

pub(super) fn test_turn_output(
    outcome: lash::TurnOutcome,
    safe_text: &str,
    tool_calls: Vec<lash_core::ToolCallRecord>,
) -> lash::TurnOutput {
    lash::TurnOutput {
        result: lash::TurnReport {
            state: lash_core::SessionSnapshot::new(SessionPolicy::new(lash::TurnBudget::Unbounded)),
            outcome,
            acceptance: None,
            cancel_input_outcome: Default::default(),
            failure_evidence: Vec::new(),
            omitted: Default::default(),
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
        thread_id: 0,
        thread_action: None,
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
        thread_id: 0,
        thread_action: None,
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
        "[Owning Thread #0; answer only within this Thread. Use threads.read to inspect other conversations.]\nWhat changed?\n[mentioned ping @release-choice (ping_id 7, done, requires_response=true, anchor 3): Choose the release channel]"
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

fn provider_rebind_test_core(
    provider: ProviderHandle,
    model: lash::ModelSpec,
    store_factory: Arc<lash_core::facade_support::InMemorySessionStoreFactory>,
    incarnation: &str,
) -> lash::LashCore {
    lash::LashCore::standard_builder(lash::TurnBudget::Unbounded)
        .provider(provider)
        .model(model)
        .store_factory(store_factory)
        .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
        .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
        .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
        .without_queued_work()
        .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
        .process_env_store(Arc::new(
            lash::persistence::InMemoryProcessExecutionEnvStore::new(),
        ))
        .build(lash_core::LeaseOwnerIdentity::opaque(
            "hirsel-provider-rebind-test",
            incarnation,
        ))
        .unwrap()
}

fn provider_rebind_test_model(id: &str) -> lash::ModelSpec {
    lash::ModelSpec::builder(id)
        .variant(ReasoningSelection::ProviderDefault)
        .context_window_tokens(200_000)
        .build()
        .unwrap()
}

#[tokio::test]
async fn reopened_agent_session_rebinds_provider_and_model_at_open() {
    let store_factory = Arc::new(lash_core::facade_support::InMemorySessionStoreFactory::new());
    let old_provider = ProviderHandle::new(
        lash_provider_anthropic::AnthropicProvider::new("old-test-key").into_components(),
    );
    let old_provider_id = old_provider.kind().to_string();
    let old_model = provider_rebind_test_model("old-provider-model");
    let first_core = provider_rebind_test_core(
        old_provider,
        old_model.clone(),
        Arc::clone(&store_factory),
        "first-boot",
    );
    let first_session = first_core.session("agent-g2").open().await.unwrap();
    assert_eq!(
        first_session.policy_snapshot().recorded_provider_id(),
        old_provider_id
    );
    first_session.close().await.unwrap();
    drop(first_core);

    let booted_provider = ProviderHandle::new(
        lash_provider_openai::OpenAiCompatibleProvider::new(
            "new-test-key",
            "https://example.invalid/v1",
        )
        .into_components(),
    );
    let booted_provider_id = booted_provider.kind().to_string();
    let selected_model = provider_rebind_test_model("new-provider-model");
    let reopened_core = provider_rebind_test_core(
        booted_provider.clone(),
        old_model,
        store_factory,
        "second-boot",
    );
    let reopened = reopened_core.session("agent-g2").open().await.unwrap();

    reconcile_opened_session_provider(&reopened, &booted_provider, &selected_model)
        .await
        .unwrap();

    let policy = reopened.policy_snapshot();
    assert_eq!(policy.recorded_provider_id(), booted_provider_id);
    assert_eq!(policy.model, selected_model);
}

#[tokio::test]
async fn session_surface_bootstrap_stores_rotates_emits_and_seeds() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let first = state
        .tools
        .prepare_agent_session("v1", &["threads.create".into()])
        .await
        .unwrap();
    assert_eq!(first.session_id, "agent");
    let (thread, _) = state
        .storage
        .create_thread(
            "release",
            "Release",
            "Choose stable or beta",
            &Value::Null,
            hirsel_proto::ThreadAttention::NeedsOwner,
        )
        .await
        .unwrap();
    state
        .storage
        .append_thread_chat(
            thread.id,
            ChatAuthor::Owner,
            "Release request",
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let rotated = state
        .tools
        .prepare_agent_session("v2", &["threads.create".into(), "threads.read".into()])
        .await
        .unwrap();
    assert_eq!(rotated.session_id, "agent-g1");
    let seed = rotated.handoff_seed.unwrap();
    assert!(seed.contains(&format!("Thread #{} owner: Release request", thread.id)));
    assert!(seed.contains("Choose stable or beta"));
    let detail = state.storage.thread_detail(0, None, 30).await.unwrap();
    assert!(
        detail
            .activities
            .iter()
            .any(|a| a.kind == "session_rotated")
    );
    assert!(state.storage.all_pings().await.unwrap().is_empty());
    assert!(
        state
            .tools
            .prepare_agent_session("v2", &["threads.create".into(), "threads.read".into()])
            .await
            .unwrap()
            .handoff_seed
            .is_none()
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

pub(super) async fn test_event_executor()
-> (HirselToolExecutor, Storage, BroadcastLog, tempfile::TempDir) {
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
            request_id: None,
            thread_id: 0,
            thread_turn_id: None,
            owner_message_id: owner.id,
        }),
    }));
    (
        HirselToolExecutor {
            tools,
            anchors,
            runtime: Arc::new(std::sync::OnceLock::new()),
        },
        storage,
        broadcast_log,
        dir,
    )
}

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

#[test]
fn every_executor_result_matches_its_declared_output_schema() {
    let now = Utc::now();
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
        ProcessAwaitOutput::Settled {
            output: lash_core::ToolCallOutput::success(json!({ "summary": "done" })),
        },
        ProcessAwaitOutput::Settled {
            output: lash_core::ToolCallOutput::failure(lash_core::ToolFailure {
                class: lash_core::ToolFailureClass::Execution,
                code: "subagent_failed".to_string(),
                message: "failed".to_string(),
                raw: Some(lash_core::ToolValue::untrusted_json(
                    json!({ "reason": "failed" }),
                )),
                source: lash_core::ToolFailureSource::Tool,
                retry: lash_core::ToolRetryStatus::Never,
            }),
        },
        cancelled_await_output("interrupted".to_string()),
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
    for name in ["artifacts_create", "artifacts_edit", "artifacts_show"] {
        results.insert(name, vec![json!({"id":1,"content":"result"})]);
    }
    results.insert("artifacts_list", vec![json!({"artifacts":[]})]);
    let thread = json!({"id":1,"title":"Buy groceries","description":"","instrument":null,"attention":"quiet","settled_at":null,"archived_at":null,"snoozed_until":null,"read":false,"created_at":now,"updated_at":now,"revision":1});
    results.insert(
        "threads_create",
        vec![json!({"thread_id":1,"thread":thread})],
    );
    results.insert(
        "threads_update",
        vec![json!({"thread_id":1,"thread":thread})],
    );
    results.insert("threads_list", vec![json!({"threads":[thread]})]);
    results.insert(
        "threads_read",
        vec![json!({"thread":thread,"messages":[],"turns":[],"activities":[],"has_more":false})],
    );
    results.insert("threads_activity",vec![json!({"activity":{"id":1,"thread_id":1,"turn_id":null,"kind":"progress","data":{},"ts":now}})]);
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
