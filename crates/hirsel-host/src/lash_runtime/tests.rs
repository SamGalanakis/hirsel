use std::collections::BTreeMap;

use crate::{
    storage::Storage,
    tools::{ShellRunOutput, ToolsConfig},
};
use chrono::Utc;
use hirsel_proto::{ChatAuthor, ThreadTurnState};
use lash_core::{
    ProcessExecutionEnvRef, ProcessIdentity, ProcessInput, ProcessOriginator, SessionScope,
    TriggerInputBinding, TriggerSubscriptionRecord,
};

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
    let mut timeline = TurnTimelineBridge {
        thread_id: Some(0),
        turn_id: Some(1),
        ..Default::default()
    };

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
    let mut timeline = TurnTimelineBridge {
        thread_id: Some(0),
        turn_id: Some(1),
        ..Default::default()
    };
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
            id: "completed".to_string(),
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

    assert!(complete_fixture_turn(&executor, &output).await.unwrap());

    let messages = storage.all_chat().await.unwrap();
    let persisted = messages.last().expect("tool-only Agent Chat row");
    assert_eq!(persisted.author, ChatAuthor::Agent);
    assert!(persisted.body.is_empty());
    assert_eq!(
        persisted.tool_calls,
        vec![ToolCallSummary {
            id: "completed".to_string(),
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
    let storage = Storage::open(dir.path()).await.unwrap();
    let text = storage
        .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
        .await
        .unwrap();
    let image = storage
        .store_blob(
            "image-upload",
            "tiny.png",
            "image/png",
            vec![137, 80, 78, 71],
        )
        .await
        .unwrap();
    let turn = OwnerTurn {
        history_id: "fixture-history".into(),
        turn_id: None,
        thread_id: 0,
        thread_action: None,
        message_id: Some(1),
        report_triggered: false,
        client_id: "client-1".to_string(),
        body: "see attached".to_string(),
        anchor: None,
        attachments: vec![text.blob.clone(), image.blob.clone()],

        mode: SendMode::Send,
    };

    let rendered = owner_turn_text(&turn, &storage);
    assert!(rendered.contains(&format!(
        "[attachment stored at {}: note.txt (text/plain, 5 bytes)]",
        text.path.display()
    )));
    assert!(rendered.contains(&format!(
        "[attachment stored at {}: tiny.png (image/png, 4 bytes)]",
        image.path.display()
    )));

    let input = owner_turn_input(&turn, &storage).await.unwrap();
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
fn agent_host_section_references_runtime_config_and_docs_paths() {
    let dir = tempfile::tempdir().unwrap();
    let config = crate::tests::test_config(dir.path());
    let section = agent_host_section(&config);
    assert!(section.contains(config.config_path.to_str().unwrap()));
    assert!(section.contains(config.docs_path.to_str().unwrap()));
    assert!(section.contains("## Host configuration"));
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

    assert!(complete_fixture_turn(&executor, &output).await.unwrap());

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
            id: "completed".to_string(),
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
        HostToClient::Msg { message } if message == persisted
    )));
}

pub(super) async fn test_event_executor()
-> (HirselToolExecutor, Storage, BroadcastLog, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let storage = Storage::open(&path).await.unwrap();
    let caller = storage.test_running_caller().await;
    let _owner = storage
        .append_thread_chat(
            caller.thread_id,
            ChatAuthor::Owner,
            "owner turn",
            None,
            vec![],
        )
        .await
        .unwrap();
    let (broadcaster, _) = broadcast::channel(16);
    let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views = crate::templates::ViewManager::new(
        storage.history_id().await.unwrap(),
        templates,
        broadcaster.clone(),
        broadcast_log.clone(),
    );
    let config_store = crate::host_config::ConfigStore::load(
        path.join("hirsel.toml"),
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
        pushes,
        views,
    );
    let anchors = Arc::new(Mutex::new(TurnAnchorState {
        active: Some(TurnAnchors {
            request_id: None,
            thread_id: caller.thread_id,
            thread_turn_id: Some(caller.turn_id),
        }),
    }));
    (
        HirselToolExecutor { tools, anchors },
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

#[tokio::test]
async fn every_executor_result_matches_its_declared_output_schema() {
    let now = Utc::now();
    let monitor = MonitorRecord {
        thread_id: 1,
        id: "monitor-1".to_string(),
        cmd: "test -f done".to_string(),
        every_secs: 30,
        condition: MonitorCondition::parse("regex", Some("ready".to_string())).unwrap(),
        label: "build ready".to_string(),
        created_ts: now,
        last_event_ts: now,
        last_run_ts: Some(now),
        last_output: Some("ready".to_string()),
        summary: Some("matched".to_string()),
        cancelled_ts: Some(now),
    };
    let mut changed_monitor = monitor.clone();
    changed_monitor.id = "monitor-2".to_string();
    changed_monitor.condition = MonitorCondition::Changed;
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
        thread_id: 1,
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
    results.insert(
        "monitors_create",
        vec![
            monitors_create_result(&monitor).unwrap(),
            monitors_create_result(&changed_monitor).unwrap(),
        ],
    );
    results.insert(
        "monitors_list",
        vec![monitors_list_result(&[monitor.clone(), changed_monitor]).unwrap()],
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

    results.insert(
        "threads_context",
        vec![json!({"thread":thread,"ancestors":[],"brief":{"text":"","artifact_ids":[]}})],
    );
    results.insert("threads_delegate", vec![json!({"thread_id":2,"turn_id":3})]);
    results.insert("threads_send", vec![json!({"thread_id":2,"turn_id":4})]);
    results.insert("threads_report", vec![json!({"activity_id":5})]);
    results.insert(
        "threads_cancel",
        vec![json!({"turn_id":4,"cancel_requested":true})],
    );
    let (executor, storage, _log, _dir) = test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let mut tools = ScopedThreadTools {
        tools: executor.tools.clone(),
        caller: caller.clone(),
        operation_id: String::new(),
    };
    let mut added_examples = Vec::new();
    let mut removed_examples = Vec::new();
    for (index, input) in [
        json!({"target":{"kind":"url","url":"https://example.com/reference#section"},"title":"Reference"}),
        json!({"target":{"kind":"thread","thread":"."}}),
    ].into_iter().enumerate() {
        tools.operation_id = format!("schema-add-{index}");
        let added = tools.execute("threads_add_related", &input).await.unwrap();
        assert_eq!(added["history_id"], caller.history_id);
        assert_eq!(added["thread_id"], caller.thread_id);
        assert_eq!(added["related_items"][0]["target"]["kind"], input["target"]["kind"]);
        let item_id = added["related_items"][0]["id"].as_u64().unwrap();
        tools.operation_id = format!("schema-remove-{index}");
        let removed = tools.execute("threads_remove_related", &json!({"item_id":item_id})).await.unwrap();
        assert_eq!(removed["related_items"], json!([]));
        added_examples.push(added);
        removed_examples.push(removed);
    }
    results.insert("threads_add_related", added_examples);
    results.insert("threads_remove_related", removed_examples);
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

#[test]
fn monitor_create_schema_and_parser_share_the_condition_contract() {
    let definition = hirsel_tool_definitions(&crate::subagent_models::registry_catalog())
        .into_iter()
        .find(|definition| definition.name() == "monitors_create")
        .unwrap();
    let schema = jsonschema::JSONSchema::compile(definition.contract.input_schema.canonical())
        .expect("monitor input schema compiles");
    let base = json!({"cmd":"printf ready","label":"ready","every_secs":30});

    for condition in [
        json!({"wake_on":"changed"}),
        json!({"wake_on":"exit_zero"}),
        json!({"wake_on":"exit_nonzero"}),
        json!({"wake_on":"regex","pattern":"ready"}),
        json!({"wake_on":"regex","pattern":" "}),
        json!({"wake_on":"regex","pattern":"\u{0}"}),
    ] {
        let mut input = base.clone();
        input.as_object_mut().unwrap().extend(
            condition
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        assert!(schema.is_valid(&input), "schema rejected {input}");
        parse_monitor_condition(&input).unwrap();
    }

    for condition in [
        json!({"wake_on":"regex"}),
        json!({"wake_on":"regex","pattern":""}),
        json!({"wake_on":"changed","pattern":"ignored"}),
        json!({"wake_on":"unknown"}),
    ] {
        let mut input = base.clone();
        input.as_object_mut().unwrap().extend(
            condition
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        assert!(!schema.is_valid(&input), "schema accepted {input}");
        assert!(parse_monitor_condition(&input).is_err());
    }

    let mut malformed = base;
    malformed["wake_on"] = json!("regex");
    malformed["pattern"] = json!("[");
    assert!(schema.is_valid(&malformed));
    assert!(parse_monitor_condition(&malformed).is_err());
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
        .call(
            "plugin__catalog_test__ping",
            serde_json::json!({}),
            tools.clone(),
            tools.storage().test_running_caller().await,
            "plugin-call".into(),
        )
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
        thread_id: 1,
        id: "monitor-1".to_string(),
        cmd: "test -f done".to_string(),
        every_secs: 30,
        condition: MonitorCondition::parse("regex", Some("ready".to_string())).unwrap(),
        label: "build ready".to_string(),
        created_ts: now,
        last_event_ts: now,
        last_run_ts: None,
        last_output: None,
        summary: None,
        cancelled_ts: None,
    };
    let policy = SessionPolicy::new(lash::TurnBudget::Unbounded);
    let requests = [monitor_start_request(
        &monitor,
        "agent",
        host_process_env_spec(policy),
    )];

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
    let (thread, _) = state
        .storage
        .create_thread(
            "release",
            "Release",
            "Choose stable or beta",
            &Value::Null,
            hirsel_proto::ThreadAttention::NeedsOwner,
            None,
        )
        .await
        .unwrap();
    let first = state
        .tools
        .prepare_agent_session(thread.id, "v1", &["threads.create".into()])
        .await
        .unwrap();
    assert!(first.session_id.starts_with(&format!(
        "thread-{}-{}-g",
        state.storage.history_id().await.unwrap(),
        thread.id
    )));
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
        .prepare_agent_session(
            thread.id,
            "v2",
            &["threads.create".into(), "threads.read".into()],
        )
        .await
        .unwrap();
    assert!(rotated.session_id.ends_with("-g1"));
    let seed = rotated.handoff_seed.unwrap();
    assert!(seed.contains(&format!("Thread #{} owner: Release request", thread.id)));
    assert!(seed.contains("This is this Thread"));
    let detail = state
        .storage
        .thread_detail(thread.id, None, 30)
        .await
        .unwrap();
    assert!(
        detail
            .activities
            .iter()
            .any(|a| a.kind == "session_rotated")
    );

    assert!(
        state
            .tools
            .prepare_agent_session(
                thread.id,
                "v2",
                &["threads.create".into(), "threads.read".into()]
            )
            .await
            .unwrap()
            .handoff_seed
            .is_none()
    );
}

#[test]
fn view_tool_contract_accepts_current_placements_only() {
    let definitions = hirsel_tool_definitions(&crate::subagent_models::registry_catalog());
    let show = definitions
        .iter()
        .find(|d| d.name() == "views_show")
        .unwrap();
    let schema = jsonschema::JSONSchema::compile(show.contract.input_schema.canonical()).unwrap();
    for placement in ["canvas", "chat"] {
        assert!(
            schema.is_valid(&json!({"placement":placement,"spec":{"type":"text","text":"hello"}}))
        );
    }
    assert!(!schema.is_valid(&json!({"placement":"ping:7","spec":{"type":"text","text":"hello"}})));
}

async fn complete_fixture_turn(
    executor: &HirselToolExecutor,
    output: &lash::TurnOutput,
) -> anyhow::Result<bool> {
    let turn_id = executor
        .anchors
        .lock()
        .await
        .active
        .as_ref()
        .unwrap()
        .thread_turn_id
        .unwrap();
    let history = executor.tools.storage().history_id().await?;
    let reply = turn_chat_payload(output);
    let terminal = match &output.result.outcome {
        lash::TurnOutcome::Finished(_) => ThreadTurnState::Completed,
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. }) => ThreadTurnState::Cancelled,
        _ => ThreadTurnState::Failed,
    };
    let (_, message) = executor
        .tools
        .storage()
        .complete_thread_turn(&history, turn_id, terminal, reply)
        .await?;
    if let Some(message) = message {
        executor.tools.publish_thread_message(message).await;
        Ok(true)
    } else {
        Ok(false)
    }
}
