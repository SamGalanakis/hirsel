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
        HostToClient::ModelChanged { current }
            if current.id == "gpt-5.6-sol" && current.variant == "high"
    )));
}

#[tokio::test]
async fn set_model_rejects_unknown_models_and_variants() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    // gpt-5.5 is no longer offered for the main agent — reject it, and any
    // unknown variant, while leaving the configured selection untouched.
    assert!(state.set_model("gpt-5.5", "high").await.is_err());
    assert!(state.set_model("gpt-5.6-sol", "impossible").await.is_err());
    assert_eq!(
        state.model_snapshot().unwrap().current,
        ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "medium".to_string(),
        }
    );
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
