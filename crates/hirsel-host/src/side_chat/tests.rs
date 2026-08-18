use std::{sync::Arc, time::Duration};

use hirsel_proto::{
    AgentActivityState, ChatAuthor, ChatMessage, EventKind, EventSource, EventSourceKind,
    EventStatus, HostToClient, PingStatus, SideChatSummary,
};
use serde_json::json;

use super::{manager::SideChatManager, projection::render_context_block};
use crate::{build_state, config::Config};

#[tokio::test]
async fn event_fork_seeds_blessed_ui_and_marks_event_open() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    assert!(
        !state.side_chats.reaper_started(),
        "opt-in compatibility must remain lazy until the first side session"
    );
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Review the release shape.", None)
        .await
        .unwrap();
    let ui = json!({
        "type": "card",
        "children": [
            { "type": "heading", "text": "Choose the release shape" },
            { "type": "status", "state": "warning", "label": "two blockers" }
        ]
    });
    let event = state
        .storage
        .create_event(
            EventKind::Summary,
            EventSource {
                kind: EventSourceKind::Scheduled,
                r#ref: Some("release-digest".to_string()),
            },
            "release-digest",
            "Release readiness digest",
            ui.clone(),
            anchor.id,
            false,
            Vec::new(),
        )
        .await
        .unwrap();

    let seed = render_context_block(&event, &anchor, &[]);
    assert!(seed.contains(r#""event_id": "#));
    assert!(seed.contains(&event.id.to_string()));
    assert!(seed.contains(r#""kind": "summary""#));
    assert!(seed.contains(r#""name": "release-digest""#));
    assert!(seed.contains(r#""description": "Release readiness digest""#));
    assert!(seed.contains(r#""state": "warning""#));

    let opened = state.side_chats.open(event.id).await.unwrap();
    assert!(state.side_chats.reaper_started());
    assert_eq!(opened.event.ui, ui);
    assert_eq!(opened.event.fork_sc.as_deref(), Some(opened.sc.as_str()));
    assert_eq!(
        state.storage.ping(event.id).await.unwrap().unwrap().fork_sc,
        Some(opened.sc.clone())
    );
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::EventUpsert { event: update }
            if update.id == event.id && update.fork_sc.is_some()
    )));
    assert!(state.side_chats.conclude(&opened.sc).await.is_err());
}

#[tokio::test]
async fn choosing_in_event_fork_posts_one_quiet_anchor_and_closes() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Pick a release channel.", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_event(
            EventKind::Judgment,
            EventSource {
                kind: EventSourceKind::Agent,
                r#ref: None,
            },
            "release-channel",
            "Which release channel should we use?",
            json!({
                "type": "card",
                "children": [{
                    "type": "optionList",
                    "options": [
                        { "key": "A", "label": "Stable", "detail": "Ship broadly" },
                        { "key": "B", "label": "Canary", "detail": "Limit exposure" }
                    ]
                }]
            }),
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let opened = state.side_chats.open(event.id).await.unwrap();
    let chat_before = state.storage.all_chat().await.unwrap().len();

    let decided = state
        .handle_event_action(event.id, "choose".to_string(), json!({ "choice": "B" }))
        .await
        .unwrap();

    assert_eq!(decided.status, EventStatus::Done);
    assert_eq!(decided.fork_sc, None);
    assert!(state.side_chats.summaries().await.is_empty());
    let chat = state.storage.all_chat().await.unwrap();
    assert_eq!(chat.len(), chat_before + 1);
    let conclusion = chat.last().unwrap();
    assert_eq!(conclusion.author, ChatAuthor::Owner);
    assert_eq!(conclusion.body, "Discussed @release-channel → Canary");
    assert_eq!(conclusion.r#ref, Some(anchor.id));
    assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
        frame,
        HostToClient::SideChatClosed { sc } if sc == &opened.sc
    )));
}

#[tokio::test]
async fn closing_non_judgment_fork_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Digest ready.", None)
        .await
        .unwrap();
    let event = state
        .storage
        .create_event(
            EventKind::Info,
            EventSource {
                kind: EventSourceKind::Agent,
                r#ref: None,
            },
            "digest-ready",
            "Digest is ready",
            json!({ "type": "card", "children": [] }),
            anchor.id,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    let opened = state.side_chats.open(event.id).await.unwrap();
    let chat_before = state.storage.all_chat().await.unwrap();

    state.side_chats.discard(&opened.sc).await.unwrap();

    assert_eq!(state.storage.all_chat().await.unwrap(), chat_before);
    let stored = state.storage.ping(event.id).await.unwrap().unwrap();
    assert_eq!(stored.status, EventStatus::Open);
    assert_eq!(stored.fork_sc, None);
}

#[tokio::test]
async fn scripted_side_chat_runs_resumes_concludes_and_confirms() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Please decide whether to ship.", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "release-decision",
            "Choose whether to release",
            "Release decision",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let opened = state.side_chats.open_legacy_ping(ping.id).await.unwrap();
    let sc = opened.sc;
    assert!(opened.messages.is_empty());
    assert!(!opened.resumed);
    assert_eq!(
        state.side_chats.summaries().await,
        vec![SideChatSummary {
            sc: sc.clone(),
            ping_id: ping.id,
        }]
    );

    state
        .side_chats
        .send(&sc, "Ship after the final check.".to_string(), Vec::new())
        .await
        .unwrap();
    let transcript = wait_for_transcript_len(&state.side_chats, &sc, 2).await;
    assert_eq!(transcript[0].author, ChatAuthor::Owner);
    assert_eq!(transcript[1].author, ChatAuthor::Agent);
    assert_eq!(
        transcript[1].body,
        "(side chat) noted: Ship after the final check."
    );

    let resumed = state.side_chats.open_legacy_ping(ping.id).await.unwrap();
    assert_eq!(resumed.sc, sc);
    assert_eq!(resumed.messages, transcript);
    assert!(resumed.resumed);

    let draft = state.side_chats.conclude(&sc).await.unwrap();
    assert!(draft.contains("Release decision"));
    assert!(draft.contains("Ship after the final check."));
    assert_eq!(
        state.side_chats.transcript(&sc).await.unwrap().len(),
        2,
        "an unconfirmed draft is not part of the transcript"
    );

    state
        .side_chats
        .confirm(&sc, "Ship it.".to_string(), &state)
        .await
        .unwrap();
    assert!(state.side_chats.summaries().await.is_empty());
    assert!(
        state
            .storage
            .side_chat_transcript(&sc)
            .await
            .unwrap()
            .is_empty()
    );
    let stored_ping = state.storage.ping(ping.id).await.unwrap().unwrap();
    assert_eq!(stored_ping.status, PingStatus::Open);
    let main_chat = state.storage.all_chat().await.unwrap();
    let conclusion = main_chat
        .iter()
        .find(|message| message.author == ChatAuthor::Owner && message.body == "Ship it.")
        .unwrap();
    assert_eq!(conclusion.r#ref, Some(anchor.id));

    let events = state.broadcast_log.recent();
    assert!(events.iter().any(|event| matches!(
        event,
        HostToClient::TurnEvent { sc: Some(scope), .. } if scope == &sc
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        HostToClient::AgentActivity { sc: Some(scope), .. } if scope == &sc
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        HostToClient::SideChatClosed { sc: closed } if closed == &sc
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        HostToClient::EventUpsert { event: update }
            if update.id == ping.id
                && update.status == PingStatus::Open
                && update.fork_sc.is_none()
    )));
}

#[tokio::test]
async fn discard_deletes_transcript_without_resolving_ping() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Anchor", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "keep-open",
            "Keep this open",
            "Keep open",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let sc = state.side_chats.open(ping.id).await.unwrap().sc;
    state
        .side_chats
        .send(&sc, "temporary".to_string(), Vec::new())
        .await
        .unwrap();
    wait_for_transcript_len(&state.side_chats, &sc, 2).await;

    state.side_chats.discard(&sc).await.unwrap();

    assert_eq!(
        state.storage.ping(ping.id).await.unwrap().unwrap().status,
        PingStatus::Open
    );
    assert!(
        state
            .storage
            .side_chat_transcript(&sc)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn scoped_cancel_stops_only_the_active_side_turn() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Anchor", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "cancel-test",
            "Cancel test",
            "Cancel test",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let sc = state.side_chats.open(ping.id).await.unwrap().sc;
    let mut broadcasts = state.broadcaster.subscribe();
    state
        .side_chats
        .send(&sc, "do not answer".to_string(), Vec::new())
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), broadcasts.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(
            event,
            HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                sc: Some(ref scope),
                ..
            } if scope == &sc
        ) {
            break;
        }
    }
    assert!(state.side_chats.cancel(&sc).await.unwrap());
    tokio::time::sleep(Duration::from_millis(60)).await;
    let transcript = state.side_chats.transcript(&sc).await.unwrap();
    assert_eq!(transcript.len(), 1);
    assert_eq!(transcript[0].author, ChatAuthor::Owner);
}

#[tokio::test]
async fn debug_routes_cover_the_scripted_side_chat_loop() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(test_config(dir.path())).await.unwrap();
    let anchor = state
        .storage
        .append_chat(ChatAuthor::Agent, "Anchor", None)
        .await
        .unwrap();
    let ping = state
        .storage
        .create_ping(
            "debug-route",
            "Debug route Ping",
            "Debug route Ping",
            anchor.id,
            true,
            Vec::new(),
        )
        .await
        .unwrap();
    let summary = state
        .storage
        .create_event(
            EventKind::Summary,
            EventSource {
                kind: EventSourceKind::Scheduled,
                r#ref: Some("debug-digest".to_string()),
            },
            "debug-digest",
            "Debug digest",
            json!({
                "type": "card",
                "children": [{ "type": "status", "state": "success", "label": "ready" }]
            }),
            anchor.id,
            false,
            Vec::new(),
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, crate::router_from_state(state))
            .await
            .unwrap();
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer test-token".parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let base = format!("http://{addr}");

    let opened: serde_json::Value = client
        .post(format!("{base}/debug/open-side-chat"))
        .json(&serde_json::json!({ "ping_id": ping.id }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let sc = opened["sc"].as_str().unwrap().to_string();
    assert_eq!(opened["messages"], serde_json::json!([]));
    assert_eq!(opened["resumed"], serde_json::json!(false));

    client
        .post(format!("{base}/debug/side-message"))
        .json(&serde_json::json!({ "sc": sc, "body": "Use this guidance" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    wait_for_debug_transcript(&client, &base, &sc, 2).await;

    let draft: serde_json::Value = client
        .post(format!("{base}/debug/conclude"))
        .json(&serde_json::json!({ "sc": sc }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(draft["text"].as_str().unwrap().contains("Debug route Ping"));

    client
        .post(format!("{base}/debug/confirm-conclusion"))
        .json(&serde_json::json!({ "sc": sc, "text": "Confirmed reply" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let listed: serde_json::Value = client
        .get(format!("{base}/debug/side-chats"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["side_chats"], serde_json::json!([]));

    let event_opened: serde_json::Value = client
        .post(format!("{base}/debug/open-side-chat"))
        .json(&json!({ "event_id": summary.id }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(event_opened["event_id"], summary.id);
    assert_eq!(event_opened["ping_id"], summary.id);
    assert_eq!(event_opened["event"]["id"], summary.id);
    assert_eq!(event_opened["event"]["ui"], summary.ui);
    assert_eq!(event_opened["event"]["fork_sc"], event_opened["sc"]);
}

async fn wait_for_transcript_len(
    manager: &Arc<SideChatManager>,
    sc: &str,
    expected: usize,
) -> Vec<ChatMessage> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(messages) = manager.transcript(sc).await
                && messages.len() >= expected
            {
                return messages;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("side-chat transcript reached expected length")
}

async fn wait_for_debug_transcript(
    client: &reqwest::Client,
    base: &str,
    sc: &str,
    expected: usize,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let value: serde_json::Value = client
                .get(format!("{base}/debug/side-chats"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            let reached = value["side_chats"]
                .as_array()
                .unwrap()
                .iter()
                .find(|chat| chat["sc"] == sc)
                .and_then(|chat| chat["messages"].as_array())
                .is_some_and(|messages| messages.len() >= expected);
            if reached {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("debug side-chat transcript reached expected length");
}

fn test_config(data_dir: &std::path::Path) -> Config {
    crate::tests::test_config_with_compat_side_sessions(data_dir, 86_400)
}
