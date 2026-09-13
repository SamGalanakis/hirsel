use std::collections::BTreeMap;

use crate::{
    storage::Storage,
    tools::{ShellRunOutput, ToolsConfig},
};
use chrono::Utc;
use hirsel_proto::{ChatAuthor, ThreadTurnState};
use lash::triggers::LashSchema;
use lash_core::{
    ProcessExecutionEnvRef, ProcessIdentity, ProcessInput, ProcessOriginator, SessionPolicy,
    SessionScope, TriggerInputBinding, TriggerSubscriptionRecord,
};

use super::process_bridge::{terminal_process_delivery, trigger_display};
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
    let mut timeline = TurnTimelineBridge {
        thread_id: Some(0),
        turn_id: Some(1),
        ..Default::default()
    };

    timeline.observe(&remote_turn_activity(
        RemoteTurnEvent::ModelRequestStarted {
            protocol_iteration: 0,
        },
    ));
    timeline.observe(&remote_turn_activity(
        RemoteTurnEvent::AssistantProseDelta {
            text: "I will ".to_string(),
        },
    ));
    timeline.observe(&remote_turn_activity(
        RemoteTurnEvent::AssistantProseDelta {
            text: "check now.".to_string(),
        },
    ));
    assert!(timeline.take_ready().is_empty());

    timeline.observe(&remote_turn_activity(RemoteTurnEvent::ToolCallStarted {
        call_id: Some("call-1".to_string()),
        name: "shell_run".to_string(),
        args: serde_json::json!({ "cmd": "true" }),
        graph_key: None,
        parent_call_id: None,
    }));
    timeline.observe(&remote_turn_activity(RemoteTurnEvent::ToolCallCompleted {
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
    }));

    let events = timeline.take_ready();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[0],
        TurnEventKind::Prose {
            text: "I will check now.".to_string()
        }
    );
    assert_eq!(
        events[1],
        TurnEventKind::ToolStart {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            summary: Some("cmd: true".to_string()),
            input: Some(bounded_turn_payload(&serde_json::json!({ "cmd": "true" })))
        }
    );
    assert_eq!(
        events[2],
        TurnEventKind::ToolDone {
            id: "call-1".to_string(),
            name: "shell_run".to_string(),
            ok: true,
            summary: Some("ok status 0".to_string()),
            result: Some(bounded_turn_payload(&serde_json::json!({
                "outcome": {
                    "status": "success",
                    "payload": {
                        "status": 0,
                        "stdout": "",
                        "stderr": "",
                        "timed_out": false
                    }
                }
            })))
        }
    );
}

#[test]
fn code_blocks_stream_full_source_and_pair_with_their_completion() {
    let mut timeline = TurnTimelineBridge {
        thread_id: Some(0),
        turn_id: Some(1),
        ..Default::default()
    };
    let source = "const x = await shell.run({ cmd: \"true\" });\nfinish(x);";

    timeline.observe(&remote_turn_activity(RemoteTurnEvent::CodeBlockStarted {
        language: "typescript".to_string(),
        code: source.to_string(),
        graph_key: None,
    }));
    timeline.observe(&remote_turn_activity(RemoteTurnEvent::CodeBlockCompleted {
        language: "typescript".to_string(),
        output: "ok".to_string(),
        error: None,
        success: true,
        duration_ms: 42,
        tool_call_ids: vec!["call-1".to_string()],
        graph_key: None,
    }));

    let events = timeline.take_ready();
    assert_eq!(events.len(), 2);
    // The full program is carried verbatim — never through the 120-char
    // summary path that tool rows use.
    assert_eq!(
        events[0],
        TurnEventKind::CodeStart {
            id: "code:1".to_string(),
            language: "typescript".to_string(),
            code: source.to_string(),
            truncated: false,
        }
    );
    assert_eq!(
        events[1],
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
    let options = input
        .protocol_turn_options
        .as_ref()
        .expect("resident Agent turns require an explicit finish");
    assert_eq!(
        options.decode::<RlmTurnOptions>().unwrap(),
        RlmTurnOptions {
            termination: Some(RlmTermination::FinishRequired { schema: None }),
            final_answer_format: None,
        }
    );
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

#[tokio::test]
async fn resident_agent_retries_bare_prose_and_projects_finished_chat_text() {
    use lash_core::{LlmOutputPart, llm::types::LlmResponse};

    let responses = Arc::new(std::sync::Mutex::new(VecDeque::from([
        "I cannot create that artifact.".to_string(),
        "<typescript>\nfinish(\"Ordinary chat answer.\");\n</typescript>".to_string(),
    ])));
    let request_count = Arc::new(AtomicU64::new(0));
    let provider = lash_core::testing::TestProvider::builder()
        .kind("hirsel-resident-finish-test")
        .complete({
            let responses = Arc::clone(&responses);
            let request_count = Arc::clone(&request_count);
            move |_request| {
                let response = responses
                    .lock()
                    .expect("response queue")
                    .pop_front()
                    .expect("queued response");
                request_count.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(LlmResponse {
                        parts: vec![LlmOutputPart::Text {
                            text: response,
                            response_meta: None,
                        }],
                        ..LlmResponse::default()
                    })
                }
            }
        })
        .build()
        .into_handle();
    let protocol = lash_protocol_rlm::RlmProtocolPluginFactory::new(
        lash_protocol_rlm::RlmProtocolPluginConfig::builder()
            .instruction_limit(lash_protocol_rlm::InstructionBound::instructions(1_000_000))
            .wall_clock(lash_protocol_rlm::WallClockBound::secs(30))
            .memory_limit(lash_protocol_rlm::MemoryBound::mebibytes(64))
            .build(),
        Arc::new(lash::persistence::InMemoryLashlangArtifactStore::new()),
    );
    let core = lash::LashCore::rlm_builder(lash::TurnBudget::Unbounded, protocol)
        .with_native_queued_work()
        .provider(provider)
        .model(provider_rebind_test_model("hirsel-resident-finish-model"))
        .store_factory(Arc::new(
            lash_core::facade_support::InMemorySessionStoreFactory::new(),
        ))
        .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
        .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
        .process_env_store(Arc::new(
            lash::persistence::InMemoryProcessExecutionEnvStore::new(),
        ))
        .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
        .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
        .without_queued_work()
        .build(lash_core::testing::runtime_lease_owner())
        .unwrap();
    let session = core
        .session("hirsel-resident-finish")
        .plugin_option(
            RLM_PROTOCOL_PLUGIN_ID,
            RlmCreateExtras {
                dialect: Some(AGENT_RLM_DIALECT),
                ..RlmCreateExtras::default()
            },
        )
        .unwrap()
        .open()
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let turn = OwnerTurn {
        history_id: "fixture-history".into(),
        turn_id: None,
        thread_id: 0,
        thread_action: None,
        message_id: Some(1),
        report_triggered: false,
        client_id: "resident-finish".into(),
        body: "Say hello".into(),
        anchor: None,
        attachments: Vec::new(),
        mode: SendMode::Send,
    };

    session
        .enqueue(owner_turn_input(&turn, &storage).await.unwrap())
        .id("resident-finish")
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    let output = session
        .queued_turn()
        .turn_id("resident-finish-drain")
        .run()
        .await
        .unwrap()
        .expect("resident input should drain");

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        output.final_value(),
        Some(&serde_json::json!("Ordinary chat answer."))
    );
    assert_eq!(
        turn_chat_payload(&output).map(|payload| payload.0),
        Some("Ordinary chat answer.".to_string())
    );
    assert!(responses.lock().unwrap().is_empty());
}

#[tokio::test]
async fn timer_triggered_typescript_process_calls_hirsel_tool_and_delivers_message() {
    use lash_core::{LlmOutputPart, llm::types::LlmResponse};

    const DRAIN: &str = "process-e2e-drain";
    let source = r#"<typescript>
const timer_shell_check = defineProcess({
  name: "timer_shell_check",
  signals: {},
  run: async (_event: unknown) => {
    const output = await shell.run({ cmd: "printf primitive-ok" });
    return output.stdout;
  }
});
const source = timer.Schedule({ label: "e2e", in_secs: 1 });
await registerTrigger({
  source,
  target: timer_shell_check,
  inputs: { _event: trigger.event },
  name: "timer shell check"
});
finish("registered");
</typescript>"#;
    let provider = lash_core::testing::TestProvider::builder()
        .kind("hirsel-process-e2e")
        .complete(move |_request| async move {
            Ok(LlmResponse {
                parts: vec![LlmOutputPart::Text {
                    text: source.to_string(),
                    response_meta: None,
                }],
                ..LlmResponse::default()
            })
        })
        .build()
        .into_handle();
    let (executor, storage, _log, _dir) = test_event_executor().await;
    let route = executor.anchors.lock().await.active.clone().unwrap();
    let session_id = storage
        .reconcile_agent_tool_surface(
            route.thread_id,
            "process-e2e-surface",
            &["shell_run".to_string()],
        )
        .await
        .unwrap()
        .session_id;
    storage
        .bind_thread_execution(
            &storage.history_id().await.unwrap(),
            &session_id,
            DRAIN,
            route.thread_turn_id.unwrap(),
        )
        .await
        .unwrap();
    let trigger_store = Arc::new(lash_core::facade_support::InMemoryTriggerStore::default());
    let process_registry = Arc::new(lash_core::TestLocalProcessRegistry::default());
    let protocol = lash_protocol_rlm::RlmProtocolPluginFactory::new(
        coordinator_rlm_config(),
        Arc::new(lash::persistence::InMemoryLashlangArtifactStore::new()),
    );
    let core = lash::LashCore::rlm_builder(lash::TurnBudget::Unbounded, protocol)
        .with_native_queued_work()
        .provider(provider)
        .model(provider_rebind_test_model("hirsel-process-e2e-model"))
        .store_factory(Arc::new(
            lash_core::facade_support::InMemorySessionStoreFactory::new(),
        ))
        .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
        .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
        .process_env_store(Arc::new(
            lash::persistence::InMemoryProcessExecutionEnvStore::new(),
        ))
        .process_registry(process_registry)
        .trigger_store(trigger_store.clone())
        .tools(Arc::new(HirselToolProvider {
            executor: executor.clone(),
        }))
        .plugin(Arc::new(HirselPluginFactory))
        .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
        .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
        .build(lash_core::testing::runtime_lease_owner())
        .unwrap();
    let session = core
        .session(&session_id)
        .plugin_option(
            RLM_PROTOCOL_PLUGIN_ID,
            RlmCreateExtras {
                dialect: Some(AGENT_RLM_DIALECT),
                ..RlmCreateExtras::default()
            },
        )
        .unwrap()
        .open()
        .await
        .unwrap();
    session
        .enqueue(lash::TurnInput::text("register the process"))
        .id("process-e2e-input")
        .ingress(TurnInputIngress::next_turn())
        .send()
        .await
        .unwrap();
    let registration = session
        .queued_turn()
        .turn_id(DRAIN)
        .run()
        .await
        .unwrap()
        .expect("registration turn");
    assert_eq!(
        registration.final_value(),
        Some(&json!("registered")),
        "registration output: {registration:#?}"
    );

    let subscriptions = trigger_store
        .list_subscriptions(TriggerSubscriptionFilter::for_session(&session_id))
        .await
        .unwrap();
    let subscription = subscriptions.first().expect("registered timer trigger");
    let report = core
        .triggers()
        .emit(
            lash::triggers::TriggerOccurrenceRequest::new(
                TIMER_SOURCE_TYPE,
                subscription.source_key.clone(),
                json!({
                    "label": "e2e",
                    "fired_at": Utc::now().to_rfc3339(),
                    "scheduled_at": Utc::now().to_rfc3339(),
                    "source_key": subscription.source_key,
                    "subscription_key": subscription.subscription_key,
                }),
                "process-e2e-timer",
            )
            .with_source(subscription.source.clone()),
            inline_trigger_scope("process-e2e-timer"),
        )
        .await
        .unwrap();
    let process_id = report
        .started_process_ids()
        .first()
        .cloned()
        .expect("process start");
    let item = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = core
                .processes()
                .session_snapshot(&session_id)
                .await
                .unwrap();
            if let Some(item) = snapshot.items.into_iter().find(|item| {
                item.process.process_id == process_id
                    && !matches!(
                        item.process.lifecycle,
                        lash_core::ProcessStatus::Running | lash_core::ProcessStatus::Waiting
                    )
            }) {
                break item;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("process completes");
    assert_eq!(
        item.process.lifecycle,
        lash_core::ProcessStatus::Completed,
        "terminal process: {item:#?}"
    );
    let (_, mut delivery) = terminal_process_delivery(route.thread_id, &item).unwrap();
    delivery.trigger = trigger_display(subscription);
    storage.stage_process_delivery(&delivery).await.unwrap();
    let delivered = storage
        .deliver_process_message(&delivery.key)
        .await
        .unwrap();

    assert!(delivered.newly_appended);
    assert!(delivered.message.body.contains("timer_shell_check"));
    assert!(delivered.message.body.contains("in 1s"));
    assert!(delivered.message.body.contains("primitive-ok"));
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
    test_event_executor_with_skills(crate::skills::Skills::default()).await
}

pub(super) async fn test_event_executor_with_skills(
    skills: crate::skills::Skills,
) -> (HirselToolExecutor, Storage, BroadcastLog, tempfile::TempDir) {
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
    let providers = crate::providers::ProviderRosterState::new(
        config_store.clone(),
        &crate::boot_provider::BootProvider::env_default(crate::config::ProviderMode::Codex),
        None,
    );
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: DriverMode::Fake,
            fake_fixture: None,
            subagent_models: crate::subagent_models::SubagentModelState::load(config_store),
            providers,
            skills,
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

#[test]
fn coordinator_posture_is_typescript_rlm_with_processes_and_triggers() {
    let config = coordinator_rlm_config();
    assert_eq!(AGENT_RLM_DIALECT, RlmDialect::Typescript);
    assert!(config.lashlang_abilities.processes);
    assert!(config.lashlang_abilities.triggers);

    // ADR 0019 keeps the native worker on the standard protocol with exactly
    // its narrow coding tools; process orchestration belongs to the coordinator.
    assert!(
        native_worker::ensure_native_tool_surface(
            &["read", "edit", "write", "exec_command"].map(str::to_string)
        )
        .is_ok()
    );
}

#[test]
fn hirsel_surface_exports_typed_thread_trigger_vocabulary() {
    let rendered = format!("{:?}", hirsel_lashlang_surface());
    for (source, event) in [
        (THREAD_REPORTED_SOURCE_TYPE, THREAD_REPORTED_EVENT_TYPE),
        (THREAD_COMPLETED_SOURCE_TYPE, THREAD_COMPLETED_EVENT_TYPE),
        (THREAD_MESSAGE_SOURCE_TYPE, THREAD_MESSAGE_EVENT_TYPE),
        (THREAD_TURN_SOURCE_TYPE, THREAD_TURN_EVENT_TYPE),
    ] {
        assert!(rendered.contains(source), "missing trigger source {source}");
        assert!(rendered.contains(event), "missing event type {event}");
    }
    for field in ["thread_id", "title", "payload"] {
        assert!(
            rendered.contains(field),
            "missing Thread event field {field}"
        );
    }
}

#[tokio::test]
async fn every_executor_result_matches_its_declared_output_schema() {
    let now = Utc::now();
    let mut results = BTreeMap::<&str, Vec<Value>>::new();
    for name in ["artifacts_create", "artifacts_edit", "artifacts_show"] {
        results.insert(name, vec![json!({"id":1,"content":"result"})]);
    }
    results.insert("artifacts_list", vec![json!({"artifacts":[]})]);
    let thread = json!({"id":1,"kind":"task","title":"Buy groceries","description":"","instrument":null,"attention":"quiet","settled_at":null,"archived_at":null,"snoozed_until":null,"read":false,"created_at":now,"updated_at":now,"revision":1});
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
fn registered_trigger_projects_before_its_first_process_run() {
    let registration = timer_registration(
        serde_json::json!({
            "label": "first run",
            "every_secs": 60
        }),
        1_000,
    );
    let process = super::process_bridge::subscription_process_info(7, &registration);

    assert_eq!(process.thread_id, 7);
    assert_eq!(process.name, "timer-test");
    assert_eq!(process.trigger.as_deref(), Some("every 60s"));
    assert_eq!(process.state, hirsel_proto::ProcessState::Waiting);
    assert!(!process.cancellable);
    assert_eq!(process.last_fired_ts, None);
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
        target_label: Some("timer-test".to_string()),
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
            hirsel_proto::ThreadKind::Task,
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

#[tokio::test]
async fn native_session_seeds_first_and_intervening_same_task_conversation_only() {
    let (executor, storage, _log, _dir) = test_event_executor().await;
    let thread = storage.test_running_caller().await.thread_id;
    let (other, _) = storage
        .create_thread(
            "other-history",
            "Other",
            "",
            &Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    storage
        .append_thread_chat(
            other.id,
            ChatAuthor::Agent,
            "UNRELATED TASK MESSAGE",
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    storage
        .append_thread_chat(
            thread,
            ChatAuthor::Owner,
            "host owner message",
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    storage
        .append_thread_chat(thread, ChatAuthor::Agent, "host answer", None, Vec::new())
        .await
        .unwrap();
    let first_turn = storage.queue_thread_turn(thread, None).await.unwrap();

    let first = executor
        .tools
        .prepare_native_worker_session(thread, first_turn.id, "profile", &["read".into()])
        .await
        .unwrap();
    let seed = first.handoff_seed.expect("first native use needs history");
    assert!(seed.contains("host owner message"), "{seed}");
    assert!(seed.contains("host answer"), "{seed}");
    assert!(!seed.contains("UNRELATED TASK MESSAGE"), "{seed}");

    let native_answer = storage
        .append_thread_chat(
            thread,
            ChatAuthor::Agent,
            "native answer already in its session",
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    storage
        .finish_thread_turn(
            first_turn.id,
            hirsel_proto::ThreadTurnState::Completed,
            Some(native_answer.id),
        )
        .await
        .unwrap();
    storage
        .mark_native_worker_conversation_seen(
            thread,
            first_turn.id,
            first.unowned_message_watermark,
        )
        .await
        .unwrap();
    let resumed_turn = storage.queue_thread_turn(thread, None).await.unwrap();
    assert!(
        executor
            .tools
            .prepare_native_worker_session(thread, resumed_turn.id, "profile", &["read".into()])
            .await
            .unwrap()
            .handoff_seed
            .is_none(),
        "an unchanged reusable native session must not receive duplicate history"
    );

    storage
        .append_thread_chat(
            thread,
            ChatAuthor::Owner,
            "cli owner message",
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    storage
        .append_thread_chat(thread, ChatAuthor::Agent, "cli answer", None, Vec::new())
        .await
        .unwrap();
    let resumed = executor
        .tools
        .prepare_native_worker_session(thread, resumed_turn.id, "profile", &["read".into()])
        .await
        .unwrap();
    assert_eq!(resumed.session_id, first.session_id);
    let seed = resumed
        .handoff_seed
        .expect("intervening backend conversation needs a handoff");
    assert!(seed.contains("cli owner message"), "{seed}");
    assert!(seed.contains("cli answer"), "{seed}");
    assert!(!seed.contains("host owner message"), "{seed}");
    assert!(!seed.contains("native answer already"), "{seed}");
    assert!(!seed.contains("UNRELATED TASK MESSAGE"), "{seed}");
}

#[tokio::test]
async fn native_session_handoff_uses_terminal_turn_order_across_queued_owner_messages() {
    let (executor, storage, _log, _dir) = test_event_executor().await;
    let caller = storage.test_running_caller().await;
    let cli = crate::storage::Delegation {
        title: "Queued-order Task".into(),
        brief: "CLI assignment".into(),
        artifact_ids: Vec::new(),
        child_thread_id: None,
        execution: Some(crate::storage::ThreadExecution::Cli {
            agent: hirsel_drivers::AgentKind::Claude,
            model: "fake-cli".into(),
            variant: "default".into(),
            cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
        }),
    };
    let cli_turn = storage
        .delegate_thread(&caller, "queued-order-cli", &cli, &json!(cli))
        .await
        .unwrap();
    storage.run_thread_turn(cli_turn.turn_id).await.unwrap();
    let native = crate::storage::Delegation {
        title: "Native preference".into(),
        brief: "Native assignment that will be cancelled".into(),
        artifact_ids: Vec::new(),
        child_thread_id: Some(cli_turn.thread_id),
        execution: Some(crate::storage::ThreadExecution::LashWorker {
            provider: crate::providers::NativeWorkerProviderSnapshot {
                id: "openrouter".into(),
                base_url: lash_provider_openai::OPENROUTER_BASE_URL.into(),
                revision: "queued-order-route".into(),
            },
            model: crate::providers::NATIVE_WORKER_DEFAULT_MODEL.into(),
            variant: "default".into(),
            cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
            tool_profile: crate::storage::NATIVE_CODING_TOOL_PROFILE.into(),
        }),
    };
    let cancelled_native = storage
        .delegate_thread(&caller, "queued-order-native", &native, &json!(native))
        .await
        .unwrap();
    let history = storage.history_id().await.unwrap();
    let request = json!({"mode":"send","thread_action":null,"body":"CURRENT NATIVE OWNER"});
    storage
        .append_thread_owner_request(
            &history,
            cli_turn.thread_id,
            "queued-order-current",
            "CURRENT NATIVE OWNER".into(),
            &[],
            &[],
            &[],
            &request,
        )
        .await
        .unwrap();
    let current = storage
        .thread_request("queued-order-current")
        .await
        .unwrap()
        .unwrap()["turn_id"]
        .as_u64()
        .unwrap();
    storage
        .append_thread_owner_request(
            &history,
            cli_turn.thread_id,
            "queued-order-future",
            "FUTURE QUEUED OWNER".into(),
            &[],
            &[],
            &[],
            &json!({"mode":"send","thread_action":null,"body":"FUTURE QUEUED OWNER"}),
        )
        .await
        .unwrap();
    let future = storage
        .thread_request("queued-order-future")
        .await
        .unwrap()
        .unwrap()["turn_id"]
        .as_u64()
        .unwrap();
    storage
        .finish_thread_turn(
            cancelled_native.turn_id,
            hirsel_proto::ThreadTurnState::Cancelled,
            None,
        )
        .await
        .unwrap();
    storage
        .complete_thread_turn(
            &history,
            cli_turn.turn_id,
            hirsel_proto::ThreadTurnState::Completed,
            Some(("CLI FINAL AFTER CURRENT ACCEPTANCE".into(), Vec::new())),
        )
        .await
        .unwrap();
    let (other, _) = storage
        .create_thread(
            "queued-order-other",
            "Other",
            "",
            &Value::Null,
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap();
    storage
        .append_thread_chat(
            other.id,
            ChatAuthor::Agent,
            "UNRELATED QUEUED ORDER",
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    let first = executor
        .tools
        .prepare_native_worker_session(
            cli_turn.thread_id,
            current,
            "queued-order-profile",
            &["read".into()],
        )
        .await
        .unwrap();
    let seed = first.handoff_seed.as_deref().unwrap();
    assert!(
        seed.contains("CLI FINAL AFTER CURRENT ACCEPTANCE"),
        "{seed}"
    );
    assert!(!seed.contains("CURRENT NATIVE OWNER"), "{seed}");
    assert!(!seed.contains("FUTURE QUEUED OWNER"), "{seed}");
    assert!(!seed.contains("UNRELATED QUEUED ORDER"), "{seed}");

    storage
        .complete_thread_turn(
            &history,
            current,
            hirsel_proto::ThreadTurnState::Completed,
            Some(("CURRENT NATIVE FINAL".into(), Vec::new())),
        )
        .await
        .unwrap();
    storage
        .mark_native_worker_conversation_seen(
            cli_turn.thread_id,
            current,
            first.unowned_message_watermark,
        )
        .await
        .unwrap();
    storage
        .complete_thread_turn(
            &history,
            future,
            hirsel_proto::ThreadTurnState::Cancelled,
            None,
        )
        .await
        .unwrap();
    storage
        .append_thread_owner_request(
            &history,
            cli_turn.thread_id,
            "queued-order-latest",
            "LATEST NATIVE OWNER".into(),
            &[],
            &[],
            &[],
            &json!({"mode":"send","thread_action":null,"body":"LATEST NATIVE OWNER"}),
        )
        .await
        .unwrap();
    let latest = storage
        .thread_request("queued-order-latest")
        .await
        .unwrap()
        .unwrap()["turn_id"]
        .as_u64()
        .unwrap();
    let resumed = executor
        .tools
        .prepare_native_worker_session(
            cli_turn.thread_id,
            latest,
            "queued-order-profile",
            &["read".into()],
        )
        .await
        .unwrap();
    let seed = resumed.handoff_seed.as_deref().unwrap();
    assert!(seed.contains("FUTURE QUEUED OWNER"), "{seed}");
    assert!(
        !seed.contains("CLI FINAL AFTER CURRENT ACCEPTANCE"),
        "{seed}"
    );
    assert!(!seed.contains("CURRENT NATIVE OWNER"), "{seed}");
    assert!(!seed.contains("CURRENT NATIVE FINAL"), "{seed}");
    assert!(!seed.contains("LATEST NATIVE OWNER"), "{seed}");
}

#[test]
fn view_tool_contract_is_canvas_only_without_a_placement_dimension() {
    let definitions = hirsel_tool_definitions(&crate::subagent_models::registry_catalog());
    let show = definitions
        .iter()
        .find(|d| d.name() == "views_show")
        .unwrap();
    let schema = jsonschema::JSONSchema::compile(show.contract.input_schema.canonical()).unwrap();
    assert!(schema.is_valid(&json!({"spec":{"type":"text","text":"hello"}})));
    assert!(!schema.is_valid(&json!({"placement":"chat","spec":{"type":"text","text":"hello"}})));
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
