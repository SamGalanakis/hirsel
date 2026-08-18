use hirsel_proto::{ChatAuthor, QuickReply};
use tokio::time::Duration;

use super::events::{
    blessed_judgment_ui_from_options, judgment_event_name, normalize_judgment_options,
    validate_judgment_context,
};
use super::*;
use crate::processes::ProcessStatus;
use crate::text::option_key;

fn judgment_options(count: usize) -> Vec<JudgmentOptionInput> {
    (0..count)
        .map(|index| JudgmentOptionInput {
            key: Some(option_key(index)),
            label: format!("Option {}", index + 1),
            detail: format!("Choose option {}", index + 1),
            recommended: index == 0,
        })
        .collect()
}

#[test]
fn judgment_context_rejects_exact_and_prefix_echoes() {
    let error = validate_judgment_context(
        "Which release channel should we use?",
        "  WHICH RELEASE CHANNEL SHOULD WE USE. ",
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
    );

    for (heading, context) in [
        (
            "Which release channel should we use?",
            "Which release channel",
        ),
        (
            "Choose stable",
            "Choose stable because it has the smaller blast radius.",
        ),
    ] {
        assert_eq!(
            validate_judgment_context(heading, context)
                .unwrap_err()
                .to_string(),
            "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
        );
    }
}

#[test]
fn judgment_context_rejects_paraphrases_but_allows_additive_context() {
    assert_eq!(
        validate_judgment_context(
            "Where should canvas view state persist?",
            "Choose where canvas view state should persist.",
        )
        .unwrap_err()
        .to_string(),
        "judgment context must add information beyond the question — state the stakes or constraint, or omit it"
    );

    validate_judgment_context(
        "Where should canvas view state persist?",
        "resolve_ping is terminal on the wire; the reopen affordance needs a real op",
    )
    .unwrap();
}

#[test]
fn empty_judgment_context_is_allowed_and_omitted_from_ui() {
    validate_judgment_context("Which release channel?", "  ").unwrap();

    let ui = blessed_judgment_ui_from_options(
        "Which release channel?",
        "  ",
        &normalize_judgment_options(judgment_options(2)).unwrap(),
        None,
        Some(3),
    );
    assert_eq!(ui["children"].as_array().unwrap().len(), 3);
    assert_eq!(ui["children"][0]["type"], "eyebrow");
    assert_eq!(ui["children"][0]["text"], "Deciding unblocks 3 agents");
    assert_eq!(ui["children"][0]["boundary"], true);
    assert_eq!(ui["children"][1]["type"], "heading");
    assert_eq!(ui["children"][2]["type"], "optionList");
}

#[test]
fn judgment_without_unblocks_omits_the_eyebrow_and_leads_with_the_heading() {
    let ui = blessed_judgment_ui_from_options(
        "Which release channel?",
        "  ",
        &normalize_judgment_options(judgment_options(2)).unwrap(),
        None,
        None,
    );
    let children = ui["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["type"], "heading");
    assert_eq!(children[1]["type"], "optionList");
}

#[test]
fn judgment_options_require_two_to_four_choices() {
    for count in [1, 5] {
        assert_eq!(
            normalize_judgment_options(judgment_options(count))
                .unwrap_err()
                .to_string(),
            "judgment events require 2–4 options"
        );
    }
    normalize_judgment_options(judgment_options(2)).unwrap();
    normalize_judgment_options(judgment_options(4)).unwrap();
}

#[test]
fn keyless_options_get_ordered_keys_and_first_recommendation() {
    let options = (0..3)
        .map(|index| JudgmentOptionInput {
            key: None,
            label: format!("Option {}", index + 1),
            detail: format!("Tradeoff {}", index + 1),
            recommended: false,
        })
        .collect();

    let normalized = normalize_judgment_options(options).unwrap();
    assert_eq!(
        normalized
            .iter()
            .map(|option| option.key.as_str())
            .collect::<Vec<_>>(),
        ["A", "B", "C"]
    );
    assert!(normalized[0].recommended);
    assert!(!normalized[1].recommended);
    assert!(!normalized[2].recommended);
}

#[test]
fn supplied_keys_and_recommendations_keep_existing_validation() {
    let invalid_key = vec![
        JudgmentOptionInput {
            key: Some("a".to_string()),
            label: "Alpha".to_string(),
            detail: "First tradeoff".to_string(),
            recommended: true,
        },
        JudgmentOptionInput {
            key: Some("B".to_string()),
            label: "Beta".to_string(),
            detail: "Second tradeoff".to_string(),
            recommended: false,
        },
    ];
    assert_eq!(
        normalize_judgment_options(invalid_key)
            .unwrap_err()
            .to_string(),
        "judgment option keys must be one uppercase ASCII letter"
    );

    let mut duplicate_key = judgment_options(2);
    duplicate_key[1].key = Some("A".to_string());
    assert_eq!(
        normalize_judgment_options(duplicate_key)
            .unwrap_err()
            .to_string(),
        "judgment option keys must be unique"
    );

    let mut duplicate_recommendation = judgment_options(2);
    duplicate_recommendation[1].recommended = true;
    assert_eq!(
        normalize_judgment_options(duplicate_recommendation)
            .unwrap_err()
            .to_string(),
        "judgment events require exactly one recommended option"
    );
}

#[test]
fn populated_judgment_keeps_the_blessed_layout_and_optional_unblocks() {
    let ui = blessed_judgment_ui_from_options(
        "Which release channel?",
        "Stable reduces rollout risk; edge gets feedback sooner.",
        &normalize_judgment_options(judgment_options(2)).unwrap(),
        Some(serde_json::json!({ "type": "text", "text": "release diff" })),
        Some(2),
    );
    let children = ui["children"].as_array().unwrap();
    assert_eq!(children[0]["type"], "eyebrow");
    assert_eq!(children[0]["text"], "Deciding unblocks 2 agents");
    assert_eq!(children[1]["type"], "heading");
    assert_eq!(children[2]["type"], "text");
    assert_eq!(children[3]["type"], "optionList");
    assert_eq!(children[4]["type"], "viewSlot");
    assert_eq!(children.len(), 5);
}

#[test]
fn judgment_event_name_truncates_on_a_word_boundary() {
    // The live bug sliced mid-word at 32 chars ("…-digestincl"); the name must
    // instead drop the overflowing trailing word whole.
    let name = judgment_event_name("Should the morning digest include overnight fleet output?");
    assert_eq!(name, "should-the-morning-digest");
    assert!(name.chars().count() <= 32);
    assert!(!name.ends_with('-'));

    // Punctuation collapses to single hyphens and never leaves a trailing one.
    assert_eq!(
        judgment_event_name("Reopen a resolved Ping — how?"),
        "reopen-a-resolved-ping-how"
    );

    // A single word longer than the cap is hard-clipped, still bounded.
    let long = judgment_event_name("supercalifragilisticexpialidocioussummary");
    assert_eq!(long.chars().count(), 32);

    // No alphanumerics at all falls back to a stable default.
    assert_eq!(judgment_event_name("—!?"), "judgment");
}

#[tokio::test]
async fn terminal_events_are_retained_for_late_subscribers() {
    let bus = TerminalEventBus::new(2);
    bus.publish(ProcessTerminal {
        process_id: "proc-finished".to_string(),
        handle: SessionHandle {
            id: "session-finished".to_string(),
            agent: AgentKind::Codex,
        },
        outcome: TerminalOutcome::Done {
            summary: "complete".to_string(),
        },
    });

    let mut late = bus.subscribe();
    let event = late.recv().await.unwrap();
    assert_eq!(event.process_id, "proc-finished");
    assert!(matches!(event.outcome, TerminalOutcome::Done { .. }));
}

#[tokio::test]
async fn requires_response_ping_records_one_push_and_unregister_stops_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    storage
        .register_push_token(hirsel_proto::PushPlatform::Android, "token-1")
        .await
        .unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Owner, "Need a decision", None)
        .await
        .unwrap();
    let (broadcaster, _) = broadcast::channel(16);
    let (pushes, recording) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views =
        crate::templates::ViewManager::new(templates, broadcaster.clone(), broadcast_log.clone());
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: DriverMode::Fake,
            fake_fixture: None,
            subagent_models: test_subagent_models(dir.path()).await,
        },
        storage.clone(),
        broadcaster,
        broadcast_log,
        ProcessStore::default(),
        pushes,
        views,
    );

    let requiring_response = tools
        .pings_send(
            "release-choice",
            "Choose the release channel",
            "Stable or beta?",
            anchor.id,
            true,
            vec![
                QuickReply {
                    value: "stable".to_string(),
                    label: "Stable".to_string(),
                },
                QuickReply {
                    value: "beta".to_string(),
                    label: "Beta".to_string(),
                },
            ],
        )
        .await
        .unwrap();
    wait_for_recorded_pushes(&recording, 1).await;
    let recorded = recording.pushes();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].tokens, vec!["token-1"]);
    assert_eq!(recorded[0].payload.title, "Hirsel");
    assert_eq!(recorded[0].payload.body, "Choose the release channel");
    assert_eq!(recorded[0].payload.data.event_id, requiring_response.id);
    assert_eq!(recorded[0].payload.data.name, "release-choice");

    tools.pushes.enqueue_ping(&requiring_response).await;
    tools
        .pings_send(
            "informational",
            "No answer needed",
            "FYI",
            anchor.id,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(recording.pushes().len(), 1);

    assert!(storage.unregister_push_token("token-1").await.unwrap());
    tools
        .pings_send(
            "second-choice",
            "Choose again",
            "A or B?",
            anchor.id,
            true,
            vec![
                QuickReply {
                    value: "a".to_string(),
                    label: "A".to_string(),
                },
                QuickReply {
                    value: "b".to_string(),
                    label: "B".to_string(),
                },
            ],
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(recording.pushes().len(), 1);
}

async fn wait_for_recorded_pushes(sender: &crate::push::RecordingPushSender, count: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while sender.pushes().len() < count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("recorded push count reached");
}

async fn test_subagent_models(path: &std::path::Path) -> SubagentModelState {
    let store = crate::host_config::ConfigStore::load(
        path.join("hirsel.toml"),
        path,
        std::path::Path::new("/docs/hirsel-config.md"),
    )
    .await
    .unwrap();
    SubagentModelState::load(store)
}

#[tokio::test]
async fn subagent_abandon_retires_driver_session_and_stays_abandoned() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        fixture.path(),
        serde_json::to_string(&serde_json::json!({
            "external_id": "fake-running",
            "progress": ["still running"],
            "delay_ms": 5_000,
            "terminal": { "status": "done", "summary": "should not happen" }
        }))
        .unwrap(),
    )
    .unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    let (broadcaster, _) = broadcast::channel(128);
    let (pushes, _) = crate::push::PushGateway::recording(storage.clone());
    let broadcast_log = BroadcastLog::default();
    let templates =
        crate::templates::TemplateStore::load(crate::templates::bundled_templates_dir())
            .await
            .unwrap();
    let views =
        crate::templates::ViewManager::new(templates, broadcaster.clone(), broadcast_log.clone());
    let tools = ToolSuite::new(
        ToolsConfig {
            driver_mode: DriverMode::Fake,
            fake_fixture: Some(fixture.path().to_path_buf()),
            subagent_models: test_subagent_models(dir.path()).await,
        },
        storage.clone(),
        broadcaster,
        broadcast_log,
        ProcessStore::default(),
        pushes,
        views,
    );
    let spawned = tools
        .subagents_spawn(
            AgentKind::Codex,
            None,
            None,
            "keep running",
            dir.path().to_path_buf(),
        )
        .await
        .unwrap();
    assert_eq!(spawned.model.as_deref(), Some("gpt-5.6-sol"));
    let specs = tools.fake.spawned_specs().unwrap();
    assert_eq!(specs[0].model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(specs[0].variant.as_deref(), Some("high"));

    tools
        .subagents_abandon_process(&spawned.process_id)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let record = tools
        .subagents_process(&spawned.process_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.status, ProcessStatus::Abandoned);
    assert!(
        tools
            .subagents_prompt_process(&spawned.process_id, "after abandon".to_string())
            .await
            .is_err(),
        "abandoned Sub-agent driver session should be retired"
    );

    let reopened = Storage::open(dir.path()).await.unwrap();
    let restored = reopened
        .restore_subagent_processes_after_restart()
        .await
        .unwrap();
    assert_eq!(restored.records.len(), 1);
    assert_eq!(restored.records[0].status, ProcessStatus::Abandoned);

    tools
        .subagent_models
        .set("codex", "gpt-5.6-luna", false, &["max".to_string()])
        .await
        .unwrap();
    let disabled = tools
        .subagents_spawn(
            AgentKind::Codex,
            Some("gpt-5.6-luna".to_string()),
            None,
            "must be rejected",
            dir.path().to_path_buf(),
        )
        .await
        .unwrap_err();
    assert!(disabled.to_string().contains("gpt-5.6-sol"));
    let unknown = tools
        .subagents_spawn(
            AgentKind::Claude,
            Some("unknown".to_string()),
            None,
            "must be rejected",
            dir.path().to_path_buf(),
        )
        .await
        .unwrap_err();
    assert!(unknown.to_string().contains("unknown or disabled"));
}
