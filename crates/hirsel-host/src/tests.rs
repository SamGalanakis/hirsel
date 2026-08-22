use std::time::Duration;

use hirsel_proto::{ChatAuthor, PingStatus, SendMode};
use serde_json::json;

use super::*;
use crate::config::{AgentMode, Config, DriverMode, ProviderMode};

#[tokio::test]
async fn scripted_next_turn_waits_and_cancel_queued_removes_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let mut broadcasts = state.broadcaster.subscribe();

    state
        .submit_owner_message(
            "active".to_string(),
            "slow:0.4".to_string(),
            None,
            Vec::new(),
            Vec::new(),
            SendMode::Send,
        )
        .await
        .unwrap();
    read_until_agent_activity(&mut broadcasts, AgentActivityState::Thinking).await;

    let queued = state
        .submit_owner_message(
            "queued".to_string(),
            "pong".to_string(),
            None,
            Vec::new(),
            Vec::new(),
            SendMode::NextTurn,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        state
            .storage
            .all_chat()
            .await
            .unwrap()
            .iter()
            .all(|message| message.author == ChatAuthor::Owner),
        "queued next-turn input should not be answered while slow turn is active"
    );

    let removed_id = state.cancel_queued_message("queued").await.unwrap();
    assert_eq!(removed_id, queued.message.id);
    read_until_msg_removed(&mut broadcasts, removed_id).await;
    assert!(
        state
            .storage
            .all_chat()
            .await
            .unwrap()
            .iter()
            .all(|message| message.id != removed_id)
    );

    read_until_agent_activity(&mut broadcasts, AgentActivityState::Idle).await;
    let messages = state.storage.all_chat().await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.author == ChatAuthor::Agent)
            .count(),
        1,
        "only the uncancelled slow turn should receive a scripted reply"
    );
}

#[tokio::test]
async fn scripted_cancel_turn_interrupts_slow_turn_without_reply() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let mut broadcasts = state.broadcaster.subscribe();

    state
        .submit_owner_message(
            "active".to_string(),
            "slow:5".to_string(),
            None,
            Vec::new(),
            Vec::new(),
            SendMode::Send,
        )
        .await
        .unwrap();
    read_until_agent_activity(&mut broadcasts, AgentActivityState::Thinking).await;

    state.cancel_turn().await.unwrap();
    read_until_agent_activity(&mut broadcasts, AgentActivityState::Idle).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        state
            .storage
            .all_chat()
            .await
            .unwrap()
            .iter()
            .all(|message| message.author == ChatAuthor::Owner),
        "cancelled slow turn should not produce an Agent reply"
    );
}

#[tokio::test]
async fn owner_message_enqueue_failure_deletes_message_without_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let mut broadcasts = state.broadcaster.subscribe();

    let error = state
        .submit_owner_message(
            "enqueue-fails".to_string(),
            "__hirsel_test_enqueue_error__".to_string(),
            None,
            Vec::new(),
            Vec::new(),
            SendMode::Send,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("scripted enqueue failed"));
    assert!(state.storage.all_chat().await.unwrap().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), broadcasts.recv())
            .await
            .is_err(),
        "failed enqueue must not publish a sent message"
    );
}

#[tokio::test]
async fn owner_reply_preserves_task_until_an_explicit_event_action() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Choose", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "choose-release",
            "Choose whether to release",
            "Choose",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    state
        .submit_owner_message(
            "reply-1".to_string(),
            "Ship it".to_string(),
            Some(anchor.id),
            Vec::new(),
            Vec::new(),
            SendMode::Send,
        )
        .await
        .unwrap();

    assert_eq!(
        state.storage.ping(ping.id).await.unwrap().unwrap().status,
        PingStatus::Open
    );
    assert!(!state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::EventUpsert { event: update }
            if update.id == ping.id
    )));
}

#[tokio::test]
async fn mentioning_a_ping_never_resolves_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Status", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "status-check",
            "Check the current status",
            "Status",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    state
        .submit_owner_message(
            "mention-1".to_string(),
            "What is happening?".to_string(),
            None,
            Vec::new(),
            vec![ping.id],
            SendMode::Send,
        )
        .await
        .unwrap();

    assert_eq!(
        state.storage.ping(ping.id).await.unwrap().unwrap().status,
        PingStatus::Open
    );
    assert!(state.broadcast_log.recent().iter().all(|event| !matches!(
        event,
        HostToClient::EventUpsert { event: update } if update.id == ping.id
    )));
}

#[tokio::test]
async fn set_model_changes_the_next_turn_model_spec() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    let selected = state.set_model("gpt-5.6-sol", "high").await.unwrap();
    let spec = state
        .agent
        .next_turn_model_spec()
        .expect("Codex runtime has a selectable model");

    assert_eq!(selected.id, "gpt-5.6-sol");
    assert_eq!(selected.variant, "high");
    assert_eq!(spec.id, "gpt-5.6-sol");
    assert_eq!(spec.variant.effort(), Some("high"));
    assert!(state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::ModelChanged { model }
            if model.current.id == "gpt-5.6-sol" && model.current.variant == "high"
    )));
}

#[tokio::test]
async fn set_model_rejects_unknown_models_and_variants() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    // Luna belongs only to the fork registry. Neither it nor retired models or
    // unknown variants may dislodge the resident Agent's Sol default.
    assert!(state.set_model("gpt-5.6-luna", "max").await.is_err());
    assert!(state.set_model("gpt-5.5", "high").await.is_err());
    assert!(state.set_model("gpt-5.6-sol", "impossible").await.is_err());
    let snapshot = state.model_snapshot().unwrap();
    assert_eq!(
        snapshot.current,
        ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "medium".to_string(),
        }
    );
    assert_eq!(
        snapshot
            .available
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-5.6-sol"]
    );
}

#[tokio::test]
async fn accepted_no_change_prompt_op_still_broadcasts_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let before = state.prompt_snapshot();
    state.broadcast_log.clear();

    let after = state.set_agent_prompt(" \n\t ").await.unwrap();

    assert_eq!(after, before);
    assert!(state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::PromptsChanged { prompts } if prompts == &before
    )));
}

#[tokio::test]
async fn set_subagent_model_persists_and_broadcasts_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let catalog = state
        .set_subagent_model("claude", "claude-opus-5", false, &["high".to_string()])
        .await
        .unwrap();
    let opus = catalog.providers[1]
        .models
        .iter()
        .find(|model| model.id == "claude-opus-5")
        .unwrap();
    assert!(!opus.enabled);
    assert_eq!(opus.enabled_variants, ["high"]);
    assert!(state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::SubagentModelsChanged { catalog }
            if !catalog.providers[1]
                .models
                .iter()
                .find(|model| model.id == "claude-opus-5")
                .unwrap()
                .enabled
    )));
    let persisted = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
    assert!(persisted.contains("[subagent_models.claude.claude-opus-5]"));
    assert!(persisted.contains("enabled_variants = [\"high\"]"));
}

#[tokio::test]
async fn canvas_view_event_enters_main_chat_as_owner_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    state
        .views
        .show(
            None,
            Some(json!({ "type": "action", "label": "Retry", "action": "retry" })),
            None,
            Some("view-canvas".to_string()),
            "canvas".to_string(),
        )
        .await
        .unwrap();

    let submission = state
        .handle_view_event(
            "view-canvas".to_string(),
            "retry".to_string(),
            json!({ "attempt": 2 }),
        )
        .await
        .unwrap();

    assert_eq!(submission.message.author, ChatAuthor::Owner);
    assert_eq!(submission.message.r#ref, None);
    assert!(submission.message.body.contains("`retry`"));
    assert!(submission.message.body.contains(r#"{"attempt":2}"#));
}

#[tokio::test]
async fn task_view_event_replies_to_anchor_without_settling_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Choose", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "release-window",
            "Choose a release window",
            "Choose",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    state
        .views
        .show(
            None,
            Some(json!({
                "type": "optionSet",
                "action": "window_selected",
                "choices": [{ "label": "Tonight", "value": "tonight" }]
            })),
            None,
            Some("view-ping".to_string()),
            format!("ping:{}", ping.id),
        )
        .await
        .unwrap();

    let submission = state
        .handle_view_event(
            "view-ping".to_string(),
            "window_selected".to_string(),
            json!({ "value": "tonight" }),
        )
        .await
        .unwrap();

    assert_eq!(submission.message.r#ref, Some(anchor.id));
    assert_eq!(
        state.storage.ping(ping.id).await.unwrap().unwrap().status,
        PingStatus::Open
    );
    assert!(!state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::EventUpsert { event: update }
            if update.id == ping.id
    )));
}

#[tokio::test]
async fn generated_task_action_recomposes_same_open_task_then_settles_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Adaptive Task", None)
        .await
        .unwrap();
    let original = state
        .storage
        .create_event(
            hirsel_proto::EventKind::Judgment,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: Some("generic-test".to_string()),
            },
            "generic-adaptive-task",
            "Advance the generic Task",
            json!({
                "type": "card",
                "children": [
                    { "type": "heading", "text": "Initial stage", "level": 2 },
                    { "type": "field", "name": "confirmation", "kind": "text", "label": "Confirmation", "required": true },
                    { "type": "submit", "action": "advance", "label": "Continue", "settles": false }
                ]
            }),
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    let accepted = state
        .handle_event_action(
            original.id,
            "advance".to_string(),
            json!({ "confirmation": "ready" }),
        )
        .await
        .unwrap();
    assert_eq!(accepted.id, original.id);
    assert_eq!(accepted.anchor, original.anchor);
    assert_eq!(accepted.status, PingStatus::Open);

    let recomposed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = state.storage.ping(original.id).await.unwrap().unwrap();
            if current.ui != original.ui {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scripted global Agent should recompose the Task");
    assert_eq!(recomposed.id, original.id);
    assert_eq!(recomposed.anchor, original.anchor);
    assert_eq!(recomposed.status, PingStatus::Open);
    assert_eq!(
        recomposed.ui["children"][1]["text"],
        "generic-adaptive-task advanced"
    );

    let settled = state
        .handle_event_action(original.id, "choose".to_string(), json!({ "choice": "A" }))
        .await
        .unwrap();
    assert_eq!(settled.status, PingStatus::Done);
    let reopened = state
        .handle_event_action(original.id, "reopen".to_string(), json!({}))
        .await
        .unwrap();
    assert_eq!(reopened.status, PingStatus::Open);
    assert_eq!(reopened.ui, recomposed.ui);
    assert_eq!(reopened.id, original.id);
    assert_eq!(reopened.anchor, original.anchor);
}

#[tokio::test]
async fn generated_task_action_rejects_unknown_client_actions() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Adaptive Task", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_event(
            hirsel_proto::EventKind::Judgment,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            "closed-contract",
            "Only declared actions pass",
            json!({
                "type": "card",
                "children": [{ "type": "submit", "action": "advance", "label": "Continue", "settles": false }]
            }),
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let error = state
        .handle_event_action(event.id, "invented".to_string(), json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not declared"));
    assert_eq!(
        state.storage.ping(event.id).await.unwrap().unwrap().ui,
        event.ui
    );

    state.storage.resolve_ping(event.id).await.unwrap();
    let error = state
        .handle_event_action(event.id, "advance".to_string(), json!({}))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("only an open Task can accept generated actions")
    );
    assert_eq!(
        state.storage.ping(event.id).await.unwrap().unwrap().status,
        PingStatus::Done
    );
}

#[tokio::test]
async fn hostile_generated_action_data_never_launches_an_agent_turn() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Adaptive Task", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_event(
            hirsel_proto::EventKind::Judgment,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            "hostile-data",
            "Validate before enqueue",
            json!({
                "type": "card",
                "children": [
                    { "type": "field", "name": "confirmation", "kind": "text", "label": "Confirmation", "required": true },
                    { "type": "submit", "action": "advance", "label": "Continue", "settles": false }
                ]
            }),
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let chat_before = state.storage.all_chat().await.unwrap();

    let error = state
        .handle_event_action(
            event.id,
            "advance".to_string(),
            json!({ "confirmation": 42, "injected": true }),
        )
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("unknown Task action data field")
            || error.to_string().contains("must be a string")
    );
    assert_eq!(state.storage.all_chat().await.unwrap(), chat_before);
    assert_eq!(state.storage.ping(event.id).await.unwrap().unwrap(), event);
    assert!(!state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            ..
        }
    )));
}

#[tokio::test]
async fn undeclared_terminal_submit_cannot_settle_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "No submit instrument", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_event(
            hirsel_proto::EventKind::Summary,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            "no-submit",
            "Read-only generated Task",
            json!({ "type": "heading", "text": "Nothing to submit" }),
            anchor.id,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    let chat_before = state.storage.all_chat().await.unwrap();

    let error = state
        .handle_event_action(event.id, "submit".to_string(), json!({}))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("not declared"));
    assert_eq!(state.storage.ping(event.id).await.unwrap(), Some(event));
    assert_eq!(state.storage.all_chat().await.unwrap(), chat_before);
    assert!(!state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            ..
        }
    )));
}

#[tokio::test]
async fn terminal_form_payload_is_validated_before_settlement() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Confirm release", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_event(
            hirsel_proto::EventKind::Judgment,
            hirsel_proto::EventSource {
                kind: hirsel_proto::EventSourceKind::Agent,
                r#ref: None,
            },
            "confirm-release",
            "Confirmation is required",
            json!({
                "type": "card",
                "children": [
                    { "type": "field", "name": "confirmation", "kind": "text", "required": true },
                    { "type": "submit", "action": "submit", "label": "Confirm" }
                ]
            }),
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let chat_before = state.storage.all_chat().await.unwrap();
    let oversized = "x".repeat(task_ui::MAX_ACTION_DATA_BYTES + 1);

    for hostile in [
        json!({}),
        json!({ "confirmation": 42 }),
        json!({ "confirmation": "ready", "unknown": "injected" }),
        json!({ "confirmation": oversized }),
    ] {
        state
            .handle_event_action(event.id, "submit".to_string(), hostile)
            .await
            .unwrap_err();
        assert_eq!(
            state.storage.ping(event.id).await.unwrap(),
            Some(event.clone())
        );
        assert_eq!(state.storage.all_chat().await.unwrap(), chat_before);
        assert!(!state.broadcast_log.recent().iter().any(|frame| matches!(
            frame,
            HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                ..
            }
        )));
    }

    let settled = state
        .handle_event_action(
            event.id,
            "submit".to_string(),
            json!({ "confirmation": "ready" }),
        )
        .await
        .unwrap();
    assert_eq!(settled.status, PingStatus::Done);
}

#[tokio::test]
async fn declared_terminal_choose_resolves_and_records_exact_owner_reply() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Choose the release", None)
        .await
        .unwrap();
    let event = state
        .tools
        .pings_send_with_view(
            "release-channel",
            "Which release channel should we use?",
            "Stable is slower; `edge` reaches testers now.",
            anchor.id,
            true,
            vec![
                hirsel_proto::QuickReply {
                    value: "Use stable for lower risk".to_string(),
                    label: "Stable".to_string(),
                },
                hirsel_proto::QuickReply {
                    value: "Use edge for faster feedback".to_string(),
                    label: "Edge".to_string(),
                },
            ],
            Some(json!({ "type": "text", "text": "release diff" })),
            Some(2),
        )
        .await
        .unwrap();

    assert_eq!(event.kind, hirsel_proto::EventKind::Judgment);
    assert_eq!(event.ui["type"], "card");
    assert_eq!(event.ui["children"][0]["type"], "eyebrow");
    assert_eq!(event.ui["children"][3]["type"], "optionList");
    assert_eq!(event.ui["children"][3]["options"][0]["key"], "A");
    assert_eq!(event.ui["children"][4]["type"], "viewSlot");
    let serialized_ui = event.ui.to_string();
    assert!(!serialized_ui.contains("wait"));
    assert!(!serialized_ui.contains("cost"));
    assert!(!serialized_ui.contains("turns"));

    let resolved = state
        .handle_event_action(
            event.id,
            "choose".to_string(),
            json!({ "choice": "A", "label": "Stable" }),
        )
        .await
        .unwrap();
    assert_eq!(resolved.status, PingStatus::Done);
    let owner_reply = state
        .storage
        .all_chat()
        .await
        .unwrap()
        .into_iter()
        .find(|message| message.author == ChatAuthor::Owner && message.r#ref == Some(event.anchor))
        .expect("choose should inject an anchor-refed Owner reply");
    assert_eq!(owner_reply.body, "Stable");
    let taste = state.storage.taste_decisions().await.unwrap();
    assert!(taste.is_empty());
}

#[tokio::test]
async fn terminal_choose_rejects_undeclared_note_without_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Choose the storage design", None)
        .await
        .unwrap();
    let event = state
        .tools
        .pings_send(
            "storage-design",
            "Where should views be stored?",
            "Choose the durable representation.",
            anchor.id,
            true,
            vec![
                hirsel_proto::QuickReply {
                    value: "Store views in their own table".to_string(),
                    label: "sqlite views table".to_string(),
                },
                hirsel_proto::QuickReply {
                    value: "Store views alongside events".to_string(),
                    label: "serialized event field".to_string(),
                },
            ],
        )
        .await
        .unwrap();

    let chat_before = state.storage.all_chat().await.unwrap();
    for hostile in [
        json!({ "choice": "A", "label": "The wrong label" }),
        json!({
            "choice": "A",
            "note": "Keep the schema queryable for debugging."
        }),
    ] {
        state
            .handle_event_action(event.id, "choose".to_string(), hostile)
            .await
            .unwrap_err();
        assert_eq!(
            state.storage.ping(event.id).await.unwrap(),
            Some(event.clone())
        );
        assert_eq!(state.storage.all_chat().await.unwrap(), chat_before);
    }
}

#[tokio::test]
async fn event_action_archive_and_unarchive_broadcast_full_upserts() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Review the release", None)
        .await
        .unwrap();
    let event = state
        .tools
        .pings_send(
            "release-review",
            "Review the release",
            "Choose whether to release.",
            anchor.id,
            true,
            vec![
                hirsel_proto::QuickReply {
                    value: "release".to_string(),
                    label: "Release".to_string(),
                },
                hirsel_proto::QuickReply {
                    value: "hold".to_string(),
                    label: "Hold".to_string(),
                },
            ],
        )
        .await
        .unwrap();
    let event_id = event.id;

    let archived = state
        .handle_event_action(event_id, "archive".to_string(), json!({}))
        .await
        .unwrap();
    assert!(archived.archived);
    assert_eq!(archived.status, PingStatus::Done);
    assert!(archived.archived_at.is_some());

    let unarchived = state
        .handle_event_action(event_id, "unarchive".to_string(), json!({}))
        .await
        .unwrap();
    assert!(!unarchived.archived);
    assert_eq!(unarchived.status, PingStatus::Done);
    assert_eq!(unarchived.archived_at, None);

    let updates = state
        .broadcast_log
        .recent()
        .into_iter()
        .filter_map(|frame| match frame {
            HostToClient::EventUpsert { event } if event.id == event_id => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(updates.len(), 3);
    assert!(updates[1].archived);
    assert!(!updates[2].archived);
}

#[tokio::test]
async fn event_action_snooze_validates_persists_and_excludes_push() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    state
        .storage
        .register_push_token(hirsel_proto::PushPlatform::Android, "device-token")
        .await
        .unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Choose", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_ping(
            "release-choice",
            "Choose the release channel",
            "Stable or beta?",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();

    for data in [
        json!({}),
        json!({ "until": "tomorrow" }),
        json!({ "until": (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339() }),
    ] {
        let error = state
            .handle_event_action(event.id, "snooze".to_string(), data)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("snooze preset"));
    }

    let until = Utc::now() + chrono::Duration::minutes(30);
    let snoozed = state
        .handle_event_action(
            event.id,
            "snooze".to_string(),
            json!({ "until": until.to_rfc3339() }),
        )
        .await
        .unwrap();
    assert_eq!(snoozed.snoozed_until, Some(until));
    assert_eq!(
        state
            .storage
            .ping(event.id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until,
        Some(until)
    );

    state.pushes.reenqueue_event(&snoozed).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(state.pushes.recorded_pushes().is_empty());

    let unsnoozed = state
        .handle_event_action(event.id, "unsnooze".to_string(), json!({}))
        .await
        .unwrap();
    assert_eq!(unsnoozed.snoozed_until, None);
}

#[tokio::test]
async fn snooze_return_survives_restart_and_repushed_open_judgment() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).await.unwrap();
    storage
        .register_push_token(hirsel_proto::PushPlatform::Android, "device-token")
        .await
        .unwrap();
    let anchor = storage
        .append_chat(ChatAuthor::Agent, "Choose", None)
        .await
        .unwrap();
    let event = storage
        .create_ping(
            "release-choice",
            "Choose the release channel",
            "Stable or beta?",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    storage
        .snooze_event(event.id, Utc::now() + chrono::Duration::milliseconds(500))
        .await
        .unwrap();
    drop(storage);

    let state = build_state(test_config(dir.path())).await.unwrap();
    assert!(
        state
            .storage
            .ping(event.id)
            .await
            .unwrap()
            .unwrap()
            .snoozed_until
            .is_some()
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let returned = state
                .storage
                .ping(event.id)
                .await
                .unwrap()
                .unwrap()
                .snoozed_until
                .is_none();
            if returned && !state.pushes.recorded_pushes().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("snoozed event returned and pushed");

    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event: update }
            if update.id == event.id && update.snoozed_until.is_none()
    )));
    assert_eq!(
        state.pushes.recorded_pushes()[0].payload.data.event_id,
        event.id
    );
}

#[tokio::test]
async fn scheduled_digest_emits_summary_event_without_push() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let event = state
        .tools
        .emit_scheduled_digest(
            "morning-digest",
            "Overnight work completed cleanly.",
            "3 repositories checked",
        )
        .await
        .unwrap();

    assert_eq!(event.kind, hirsel_proto::EventKind::Summary);
    assert_eq!(event.source.kind, hirsel_proto::EventSourceKind::Scheduled);
    assert_eq!(event.source.r#ref.as_deref(), Some("morning-digest"));
    assert_eq!(event.ui["children"][0]["type"], "text");
    assert_eq!(event.ui["children"][1]["type"], "status");
    assert_eq!(event.ui["children"][2]["type"], "keyValue");
    assert!(state.pushes.recorded_pushes().is_empty());
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event: update } if update.id == event.id
    )));
}

async fn read_until_agent_activity(
    broadcasts: &mut tokio::sync::broadcast::Receiver<HostToClient>,
    state: AgentActivityState,
) {
    loop {
        match broadcasts.recv().await.unwrap() {
            HostToClient::AgentActivity {
                state: observed, ..
            } if observed == state => return,
            _ => {}
        }
    }
}

async fn read_until_msg_removed(
    broadcasts: &mut tokio::sync::broadcast::Receiver<HostToClient>,
    id: u64,
) {
    loop {
        match broadcasts.recv().await.unwrap() {
            HostToClient::MsgRemoved { id: observed } if observed == id => return,
            _ => {}
        }
    }
}

pub(crate) fn test_config(data_dir: &std::path::Path) -> Config {
    Config {
        token: "test-token".to_string(),
        agent: AgentMode::Scripted,
        provider: ProviderMode::Anthropic,
        anthropic_api_key: None,
        openrouter_api_key: None,
        model: "claude-opus-4-7".to_string(),
        data_dir: data_dir.to_path_buf(),
        config_path: data_dir.join("hirsel.toml"),
        docs_path: crate::templates::bundled_docs_path(),
        templates_dir: crate::templates::bundled_templates_dir(),
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: true,
        compat_side_session_ttl_secs: None,
    }
}

/// `test_config` with debug off, so the owner token is checked exactly.
pub(crate) fn test_config_production_auth(data_dir: &std::path::Path) -> Config {
    Config {
        debug: false,
        ..test_config(data_dir)
    }
}

/// `test_config` with the legacy side-session compatibility window enabled.
pub(crate) fn test_config_with_compat_side_sessions(
    data_dir: &std::path::Path,
    ttl_secs: u64,
) -> Config {
    Config {
        compat_side_session_ttl_secs: Some(ttl_secs),
        ..test_config(data_dir)
    }
}

#[tokio::test]
async fn current_host_starts_without_legacy_side_session_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();

    assert!(!state.side_chats.reaper_started());
    assert!(state.side_chats.summaries().await.is_empty());
    let error = state
        .side_chats
        .open(1)
        .await
        .err()
        .expect("default Host must reject legacy side sessions");
    assert!(error.to_string().contains("HIRSEL_COMPAT_SIDE_SESSIONS=1"));
    assert!(!state.side_chats.reaper_started());
}

/// Removing the instance an agent points at is the same reshape as moving the
/// agent off it: the stored choice falls back to the booted provider, so both
/// agent surfaces have to go out with the roster.
#[tokio::test]
async fn removing_the_provider_an_agent_points_at_republishes_its_surface() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();
    state
        .add_provider(
            "router",
            "Router",
            "https://example.invalid/v1",
            "sk-fake-router-key",
            "some/model",
        )
        .await
        .unwrap();
    for agent in [AgentSlot::Main, AgentSlot::Fork] {
        state.set_agent_provider(agent, "router").await.unwrap();
    }
    assert!(state.model_snapshot().unwrap().free_text_model);
    state.broadcast_log.clear();

    state.remove_provider("router").await.unwrap();

    // Both agents are back on the booted provider's curated registry...
    let snapshot = state.model_snapshot().unwrap();
    assert_eq!(snapshot.provider_id.as_deref(), Some("codex"));
    assert!(!snapshot.free_text_model);
    assert_eq!(snapshot.current.id, "gpt-5.6-sol");
    let fork = state.prompt_snapshot().fork.unwrap();
    assert_eq!(fork.provider_id.as_deref(), Some("codex"));
    assert_eq!(fork.current.id, "gpt-5.6-luna");

    // ...and every client was told, not just about the roster.
    let broadcasts = state.broadcast_log.recent();
    assert!(broadcasts.iter().any(|event| matches!(
        event,
        HostToClient::ModelChanged { model } if model == &snapshot
    )));
    assert!(broadcasts.iter().any(|event| matches!(
        event,
        HostToClient::PromptsChanged { prompts } if prompts.fork.as_ref() == Some(&fork)
    )));
    assert!(
        broadcasts
            .iter()
            .any(|event| matches!(event, HostToClient::ProvidersChanged { .. }))
    );
    let encoded = serde_json::to_string(&broadcasts).unwrap();
    assert!(!encoded.contains("sk-fake-router-key"), "{encoded}");
}

/// Editing the pointed-at instance's `default_model` moves what an agent with
/// no stored selection of its own falls back to, so the same republish applies.
#[tokio::test]
async fn changing_the_pointed_at_default_model_republishes_the_model_surface() {
    let dir = tempfile::tempdir().unwrap();
    // A `[model]` naming the instance but no id/variant — a hand-edited config,
    // or one written before the Owner ever picked a model — so the served
    // selection IS the instance's `default_model`.
    tokio::fs::write(
        dir.path().join("hirsel.toml"),
        "[providers.router]\nkind = \"openai_compatible\"\nlabel = \"Router\"\n\
         base_url = \"https://example.invalid/v1\"\napi_key = \"sk-fake-router-key\"\n\
         default_model = \"some/model\"\n\n[model]\nprovider = \"router\"\n",
    )
    .await
    .unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();
    assert_eq!(state.model_snapshot().unwrap().current.id, "some/model");
    state.broadcast_log.clear();

    state
        .update_provider("router", None, None, None, Some("vendor/next-model"))
        .await
        .unwrap();

    let snapshot = state.model_snapshot().unwrap();
    assert_eq!(snapshot.current.id, "vendor/next-model");
    assert!(state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::ModelChanged { model } if model.current.id == "vendor/next-model"
    )));

    // A label-only edit moves nothing an agent renders, so nothing is claimed.
    state.broadcast_log.clear();
    state
        .update_provider("router", Some("Router II"), None, None, None)
        .await
        .unwrap();
    assert!(
        !state
            .broadcast_log
            .recent()
            .iter()
            .any(|event| matches!(event, HostToClient::ModelChanged { .. }))
    );
}

/// The field-observed defect: a host booted on OpenRouter, the Owner moves the
/// main Agent to Codex, and the Model row must become the curated Codex model
/// plus its reasoning ladder immediately — not stay the booted provider's
/// free-of-effort shape until a restart.
#[tokio::test]
async fn moving_the_main_agent_to_codex_reshapes_the_model_surface_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::OpenRouter;
    config.openrouter_api_key = Some("sk-fake-openrouter-key".to_string());
    config.model = "google/gemini-3.7-flash".to_string();
    let state = build_state(config).await.unwrap();
    // Booted shape: OpenRouter is an OpenAI-compatible endpoint, so the Model
    // row is one free-text id with no reasoning ladder at all.
    let booted = state.model_snapshot().unwrap();
    assert_eq!(booted.provider_id.as_deref(), Some("openrouter"));
    assert!(booted.free_text_model);
    assert!(booted.available.is_empty());
    state.broadcast_log.clear();

    state
        .set_agent_provider(AgentSlot::Main, "codex")
        .await
        .unwrap();

    let snapshot = state.model_snapshot().unwrap();
    assert_eq!(snapshot.provider_id.as_deref(), Some("codex"));
    assert!(!snapshot.free_text_model);
    assert_eq!(snapshot.current.id, "gpt-5.6-sol");
    assert_eq!(
        snapshot.available[0].variants,
        ["low", "medium", "high", "xhigh", "max"]
    );
    // The whole reshaped snapshot goes out, so a connected client renders the
    // reasoning select without waiting for a reconnect.
    let broadcast = state
        .broadcast_log
        .recent()
        .into_iter()
        .find_map(|event| match event {
            HostToClient::ModelChanged { model } => Some(model),
            _ => None,
        })
        .expect("a provider move must broadcast the whole model snapshot");
    assert_eq!(broadcast, snapshot);

    // An effort chosen now persists and is reported, while the session the host
    // actually booted keeps running OpenRouter's own spec until a restart.
    let selected = state.set_model("gpt-5.6-sol", "xhigh").await.unwrap();
    assert_eq!(selected.variant, "xhigh");
    assert_eq!(state.model_snapshot().unwrap().current.variant, "xhigh");
    let spec = state
        .agent
        .next_turn_model_spec()
        .expect("OpenRouter runtime has a selectable model");
    assert_eq!(spec.id, "google/gemini-3.7-flash");

    // ...and a fresh hello serves the reshaped snapshot too.
    assert_eq!(
        state.model_snapshot().unwrap().provider_id.as_deref(),
        Some("codex")
    );
}

#[tokio::test]
async fn set_agent_provider_seeds_the_model_and_broadcasts_both_surfaces() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();
    state
        .add_provider(
            "router",
            "Router",
            "https://example.invalid/v1",
            "sk-fake-router-key",
            "some/model",
        )
        .await
        .unwrap();
    state.broadcast_log.clear();

    let roster = state
        .set_agent_provider(AgentSlot::Main, "router")
        .await
        .unwrap();

    // The choice is stored with the provider's default model seeded...
    assert_eq!(roster.booted_provider_id.as_deref(), Some("codex"));
    let snapshot = state.model_snapshot().unwrap();
    assert_eq!(snapshot.provider_id.as_deref(), Some("router"));
    assert_eq!(snapshot.current.id, "some/model");
    assert!(snapshot.free_text_model);
    assert!(snapshot.available.is_empty());
    // ...and both the roster and the model surface are told — the WHOLE
    // snapshot, so the client learns the control's new shape and not just a
    // selection it cannot render.
    assert!(state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::ProvidersChanged { roster }
            if roster.instances.iter().any(|instance| instance.id == "router")
    )));
    assert!(state.broadcast_log.recent().iter().any(|event| matches!(
        event,
        HostToClient::ModelChanged { model }
            if model.current.id == "some/model"
                && model.free_text_model
                && model.available.is_empty()
                && model.provider_id.as_deref() == Some("router")
    )));
    // The key never rides along on any broadcast.
    let broadcasts = serde_json::to_string(&state.broadcast_log.recent()).unwrap();
    assert!(!broadcasts.contains("sk-fake-router-key"), "{broadcasts}");

    // The fork is a separate slot with its own seed.
    state
        .set_agent_provider(AgentSlot::Fork, "codex")
        .await
        .unwrap();
    let fork = state.prompt_snapshot().fork.unwrap();
    assert_eq!(fork.provider_id.as_deref(), Some("codex"));
    assert_eq!(fork.current.id, "gpt-5.6-luna");
    assert_eq!(fork.current.variant, "max");
}

/// A `hirsel.toml` naming a stored instance as the main Agent's provider, with
/// or without a key. Written before the host boots, exactly as an Owner's
/// previous session (or hand edit) would have left it.
async fn seed_stored_choice(data_dir: &std::path::Path, api_key: Option<&str>) {
    let key_line = api_key
        .map(|key| format!("api_key = \"{key}\"\n"))
        .unwrap_or_default();
    tokio::fs::write(
        data_dir.join("hirsel.toml"),
        format!(
            "[providers.acme]\nkind = \"openai_compatible\"\nlabel = \"Acme\"\n\
             base_url = \"https://acme.invalid/v1\"\n{key_line}default_model = \"acme/model\"\n\n\
             [model]\nprovider = \"acme\"\nid = \"acme/model\"\nvariant = \"default\"\n"
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn the_stored_main_agent_provider_is_what_the_host_boots_on() {
    let dir = tempfile::tempdir().unwrap();
    seed_stored_choice(dir.path(), Some("sk-acme-boot-key")).await;
    let mut config = test_config(dir.path());
    // The environment says Codex; the stored roster choice says Acme, and the
    // stored choice is the one that can actually boot.
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    let roster = state.provider_roster().await;
    assert_eq!(roster.booted_provider_id.as_deref(), Some("acme"));
    assert_eq!(roster.boot_notice, None);
    // The model picker follows the same choice, in free text.
    let snapshot = state.model_snapshot().unwrap();
    assert_eq!(snapshot.provider_id.as_deref(), Some("acme"));
    assert!(snapshot.free_text_model);
    let encoded = serde_json::to_string(&roster).unwrap();
    assert!(!encoded.contains("sk-acme-boot-key"), "{encoded}");
}

#[tokio::test]
async fn a_stored_provider_with_no_key_falls_back_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    seed_stored_choice(dir.path(), None).await;
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    let roster = state.provider_roster().await;
    assert_eq!(roster.booted_provider_id.as_deref(), Some("codex"));
    let notice = roster
        .boot_notice
        .expect("a discarded choice must be reported");
    assert!(notice.contains("acme"), "{notice}");
    assert!(notice.contains("no API key is stored"), "{notice}");
    assert!(notice.ends_with("running on Codex"), "{notice}");
}

#[tokio::test]
async fn a_hand_edited_claude_choice_falls_back_instead_of_bricking_the_boot() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(
        dir.path().join("hirsel.toml"),
        "[providers]\n\n[model]\nprovider = \"claude\"\nid = \"gpt-5.6-sol\"\nvariant = \"high\"\n",
    )
    .await
    .unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    let roster = state.provider_roster().await;
    assert_eq!(roster.booted_provider_id.as_deref(), Some("codex"));
    let notice = roster
        .boot_notice
        .expect("a discarded choice must be reported");
    assert!(notice.contains("claude"), "{notice}");
    assert!(notice.contains("ADR-0015"), "{notice}");
}

#[tokio::test]
async fn claude_is_rejected_for_both_resident_agents_and_broadcasts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();
    state.broadcast_log.clear();

    for agent in [AgentSlot::Main, AgentSlot::Fork] {
        let error = state
            .set_agent_provider(agent, "claude")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Sub-agents only"), "{error}");
        assert!(error.contains("ADR-0015"), "{error}");
    }
    // A rejected command settles on the error frame alone.
    assert!(state.broadcast_log.recent().is_empty());
    assert!(state.model_snapshot().unwrap().provider_id.as_deref() == Some("codex"));
}

#[tokio::test]
async fn anthropic_boot_mode_keeps_its_legacy_surface() {
    let dir = tempfile::tempdir().unwrap();
    // `test_config` boots on the legacy Anthropic path.
    let state = build_state(test_config(dir.path())).await.unwrap();

    assert!(state.model_snapshot().is_none());
    assert!(state.prompt_snapshot().fork.is_none());
    let roster = state.provider_roster().await;
    assert_eq!(roster.booted_provider_id, None);
    assert!(
        roster
            .instances
            .iter()
            .any(|instance| instance.id == "codex")
    );

    // Ops that need a resident agent's provider fail cleanly, and the
    // built-ins stay built in.
    assert!(
        state
            .set_agent_provider(AgentSlot::Main, "codex")
            .await
            .is_err()
    );
    assert!(state.remove_provider("codex").await.is_err());
    assert!(state.redetect_provider("nope").await.is_err());
    assert!(state.redetect_provider("codex").await.is_ok());
}

#[tokio::test]
async fn provider_edits_persist_and_never_broadcast_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    state
        .add_provider(
            "router",
            "Router",
            "https://example.invalid/v1",
            "sk-fake-router-key",
            "some/model",
        )
        .await
        .unwrap();

    let roster = state
        .update_provider("router", Some("Renamed"), None, None, Some("other/model"))
        .await
        .unwrap();
    let router = roster
        .instances
        .iter()
        .find(|instance| instance.id == "router")
        .unwrap();
    assert_eq!(router.label, "Renamed");
    assert_eq!(router.default_model, "other/model");
    assert!(router.api_key.present);
    assert_eq!(router.api_key.tail, "-key");
    assert!(router.removable);

    // The full key is in the file and nowhere else.
    let persisted = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
    assert!(persisted.contains("sk-fake-router-key"), "{persisted}");
    let broadcasts = serde_json::to_string(&state.broadcast_log.recent()).unwrap();
    assert!(!broadcasts.contains("sk-fake-router-key"), "{broadcasts}");

    let roster = state.remove_provider("router").await.unwrap();
    assert!(
        !roster
            .instances
            .iter()
            .any(|instance| instance.id == "router")
    );
    assert!(
        state
            .add_provider("codex", "X", "https://a.invalid/v1", "k", "m")
            .await
            .is_err()
    );
}
