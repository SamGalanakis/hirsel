use std::time::Duration;

use hirsel_proto::{AgentActivityState, ChatAuthor, SendMode};
use serde_json::json;

use super::*;
use crate::config::{AgentMode, Config, DriverMode, ProviderMode};

#[tokio::test]
async fn timeline_persistence_failure_fails_the_turn_without_broadcasting_the_event() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let thread = state
        .storage
        .create_thread(
            "timeline-failure",
            "Timeline failure",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let turn = state
        .storage
        .start_thread_turn(thread.id, None)
        .await
        .unwrap();
    state.broadcast_log.clear();

    assert!(
        state
            .tools
            .publish_turn_event(
                thread.id + 1,
                turn.id,
                hirsel_proto::TurnEventKind::Prose {
                    text: "must not leak".into(),
                },
            )
            .await
            .is_err()
    );

    let detail = state
        .storage
        .thread_detail(thread.id, None, 100)
        .await
        .unwrap();
    assert_eq!(detail.turns[0].state, hirsel_proto::ThreadTurnState::Failed);
    assert_eq!(detail.turn_timelines.len(), 1);
    assert!(detail.turn_timelines[0].events.is_empty());
    assert!(detail.activities.iter().any(|activity| {
        activity.kind == "execution_failed"
            && activity.data["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("timeline persistence failed"))
    }));
    assert!(
        state
            .broadcast_log
            .recent()
            .iter()
            .all(|frame| !matches!(frame, hirsel_proto::HostToClient::TurnEvent { .. }))
    );
}

#[tokio::test]
async fn scripted_next_turn_waits_and_cancel_queued_removes_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let thread = state
        .storage
        .create_thread(
            "destination",
            "Destination",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let mut broadcasts = state.broadcaster.subscribe();

    state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "active".to_string(),
            thread.id,
            "slow:0.4".to_string(),
            Vec::new(),
            Vec::new(),
            SendMode::Send,
            Vec::new(),
        )
        .await
        .unwrap();
    read_until_agent_activity(&mut broadcasts, AgentActivityState::Thinking).await;

    let queued = state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "queued".to_string(),
            thread.id,
            "pong".to_string(),
            Vec::new(),
            Vec::new(),
            SendMode::NextTurn,
            Vec::new(),
        )
        .await
        .unwrap();
    let queued_turn = state
        .storage
        .thread_detail(thread.id, None, 100)
        .await
        .unwrap()
        .turns
        .into_iter()
        .find(|turn| turn.owner_message_id == Some(queued.message.id))
        .expect("accepted Owner message has a durable turn");
    assert_eq!(queued_turn.state, hirsel_proto::ThreadTurnState::Queued);
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::ThreadTurn { turn } if turn == &queued_turn
    )));
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
    let thread = state
        .storage
        .create_thread(
            "destination",
            "Destination",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let mut broadcasts = state.broadcaster.subscribe();

    state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "active".to_string(),
            thread.id,
            "slow:5".to_string(),
            Vec::new(),
            Vec::new(),
            SendMode::Send,
            Vec::new(),
        )
        .await
        .unwrap();
    read_until_agent_activity(&mut broadcasts, AgentActivityState::Thinking).await;

    state
        .agent
        .cancel_thread_turn(&state.storage.history_id().await.unwrap(), thread.id)
        .await
        .unwrap();
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
async fn enqueue_failure_retains_accepted_thread_message_and_request() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let thread = state
        .storage
        .create_thread(
            "destination",
            "Destination",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    let error = state
        .submit_addressed_thread_message(
            &state.storage.history_id().await.unwrap(),
            "enqueue-fails".into(),
            thread.id,
            "__hirsel_test_enqueue_error__".into(),
            vec![],
            vec![],
            SendMode::Send,
            Vec::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("scripted enqueue failed"));
    assert_eq!(state.storage.all_chat().await.unwrap().len(), 1);
    assert_eq!(
        state.storage.pending_thread_requests().await.unwrap().len(),
        1
    );
    assert!(state.broadcast_log.recent().iter().any(|f| matches!(f,HostToClient::Msg{message} if message.client_id.as_deref()==Some("enqueue-fails"))));
}

#[tokio::test]
async fn set_agent_model_changes_the_next_turn_model_spec() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    let selected = state
        .set_agent_model("codex", "gpt-5.6-sol", "high")
        .await
        .unwrap();
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
async fn set_agent_model_rejects_cross_provider_models_and_variants() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(dir.path());
    config.provider = ProviderMode::Codex;
    config.model = "gpt-5.6-sol".to_string();
    let state = build_state(config).await.unwrap();

    // Luna belongs only to the fork registry. Neither it nor retired models or
    // unknown variants may dislodge the resident Agent's Sol default.
    assert!(
        state
            .set_agent_model("codex", "gpt-5.6-luna", "max")
            .await
            .is_err()
    );
    assert!(
        state
            .set_agent_model("codex", "google/gemini-3.7-flash", "default")
            .await
            .is_err()
    );
    assert!(
        state
            .set_agent_model("codex", "gpt-5.5", "high")
            .await
            .is_err()
    );
    assert!(
        state
            .set_agent_model("codex", "gpt-5.6-sol", "impossible")
            .await
            .is_err()
    );
    let error = state
        .set_agent_model("openrouter", "gpt-5.6-sol", "high")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("configured for `codex`"), "{error}");
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
        ["gpt-5.6-sol", "gpt-6-astra"]
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
async fn canvas_view_event_enters_its_origin_thread_as_owner_message() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let thread = state
        .storage
        .create_thread(
            "destination",
            "Destination",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            hirsel_proto::ThreadKind::Task,
            None,
        )
        .await
        .unwrap()
        .0;
    state
        .views
        .show(
            &state.storage.history_id().await.unwrap(),
            thread.id,
            None,
            Some(json!({ "type": "action", "label": "Retry", "action": "retry" })),
            None,
            Some("view-canvas".to_string()),
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
    }
}

/// `test_config` with debug off, so the owner token is checked exactly.
pub(crate) fn test_config_production_auth(data_dir: &std::path::Path) -> Config {
    Config {
        debug: false,
        ..test_config(data_dir)
    }
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
    let selected = state
        .set_agent_model("codex", "gpt-5.6-sol", "xhigh")
        .await
        .unwrap();
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
    let error = state
        .set_fork_model("router", "some/model", "default")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("configured for `codex`"), "{error}");
    assert!(
        state
            .set_fork_model("codex", "google/gemini-3.7-flash", "default")
            .await
            .is_err()
    );
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
