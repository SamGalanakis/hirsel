use std::{collections::VecDeque, time::Duration};

use async_trait::async_trait;
use hirsel_proto::{ChatAuthor, ClientToHost, HelloAuth, HostToClient};
use serde_json::json;

use super::{
    IncomingFrame, POST_AUTH_MAX_FRAME_BYTES, PRE_AUTH_MAX_FRAME_BYTES, Peer, ProtocolChannel,
    authenticate, build_snapshot, handle_client_frame, run_protocol,
};
use crate::{
    build_state,
    config::{AgentMode, Config, DriverMode, ProviderMode},
};

#[tokio::test]
async fn pairing_uses_the_apps_device_label() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(Config {
        token: "test-token".to_string(),
        agent: AgentMode::Scripted,
        provider: ProviderMode::Anthropic,
        anthropic_api_key: None,
        openrouter_api_key: None,
        model: "test-model".to_string(),
        data_dir: dir.path().to_path_buf(),
        config_path: dir.path().join("hirsel.toml"),
        docs_path: crate::templates::bundled_docs_path(),
        templates_dir: crate::templates::bundled_templates_dir(),
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: true,
    })
    .await
    .unwrap();
    let code = state
        .storage
        .mint_pairing_code("Mint-time label", Duration::from_secs(60))
        .await
        .unwrap();

    let device_token = authenticate(
        &state,
        HelloAuth::PairingCode {
            code,
            device_label: "App-chosen label".to_string(),
        },
        &Peer::Iroh {
            node_id: "node-a".to_string(),
        },
    )
    .await
    .unwrap()
    .expect("pairing should issue a device token");

    state
        .storage
        .authenticate_device_token(&device_token, Some("node-a"))
        .await
        .unwrap();
    let devices = state.storage.list_devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_label, "App-chosen label");
}

#[tokio::test]
async fn static_owner_auth_rejects_empty_and_accepts_real_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(Config {
        token: "real-token".to_string(),
        agent: AgentMode::Scripted,
        provider: ProviderMode::Anthropic,
        anthropic_api_key: None,
        openrouter_api_key: None,
        model: "test-model".to_string(),
        data_dir: dir.path().to_path_buf(),
        config_path: dir.path().join("hirsel.toml"),
        docs_path: crate::templates::bundled_docs_path(),
        templates_dir: crate::templates::bundled_templates_dir(),
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: false,
    })
    .await
    .unwrap();

    assert!(
        authenticate(
            &state,
            HelloAuth::StaticToken(String::new()),
            &Peer::WebSocket { addr: None }
        )
        .await
        .is_err()
    );
    assert!(
        authenticate(
            &state,
            HelloAuth::StaticToken("real-token".to_string()),
            &Peer::WebSocket { addr: None }
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn websocket_rejects_iroh_only_auth() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let peer = Peer::WebSocket {
        addr: Some("127.0.0.1:1234".to_string()),
    };

    assert_eq!(
        authenticate(
            &state,
            HelloAuth::DeviceToken("device-token".to_string()),
            &peer,
        )
        .await
        .unwrap_err(),
        "device-token auth requires iroh"
    );
    assert_eq!(
        authenticate(
            &state,
            HelloAuth::PairingCode {
                code: "pairing-code".to_string(),
                device_label: "Browser".to_string(),
            },
            &peer,
        )
        .await
        .unwrap_err(),
        "pairing-code auth requires iroh"
    );
}

#[tokio::test]
async fn full_resync_snapshot_replays_all_chat() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(Config {
        token: "test-token".to_string(),
        agent: AgentMode::Scripted,
        provider: ProviderMode::Anthropic,
        anthropic_api_key: None,
        openrouter_api_key: None,
        model: "test-model".to_string(),
        data_dir: dir.path().to_path_buf(),
        config_path: dir.path().join("hirsel.toml"),
        docs_path: crate::templates::bundled_docs_path(),
        templates_dir: crate::templates::bundled_templates_dir(),
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: false,
    })
    .await
    .unwrap();
    state
        .storage
        .append_thread_chat(
            state
                .storage
                .create_thread(
                    "fixture-Conversation",
                    "Conversation",
                    "",
                    &serde_json::Value::Null,
                    hirsel_proto::ThreadAttention::Quiet,
                    None,
                )
                .await
                .unwrap()
                .0
                .id,
            ChatAuthor::Agent,
            "missed",
            None,
            vec![],
        )
        .await
        .unwrap();
    state
        .views
        .show(
            &state.storage.history_id().await.unwrap(),
            1,
            None,
            Some(json!({ "type": "text", "text": "Still active" })),
            None,
            Some("view-reconnect".to_string()),
        )
        .await
        .unwrap();

    let (frame, _) = build_snapshot(&state).await.unwrap();
    match frame {
        HostToClient::HelloOk {
            history_id, views, ..
        } => {
            assert!(!history_id.is_empty());
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].instance_id, "view-reconnect");
        }
        other => panic!("unexpected resync frame: {other:?}"),
    }
}

#[test]
fn pre_auth_frames_have_a_stricter_limit() {
    assert_eq!(PRE_AUTH_MAX_FRAME_BYTES, 8 * 1024);
    const { assert!(PRE_AUTH_MAX_FRAME_BYTES < POST_AUTH_MAX_FRAME_BYTES) };
}

struct TestChannel {
    incoming: VecDeque<IncomingFrame>,
    sent: Vec<HostToClient>,
}

#[async_trait]
impl ProtocolChannel for TestChannel {
    async fn receive(&mut self, _max_bytes: usize) -> anyhow::Result<Option<IncomingFrame>> {
        Ok(self.incoming.pop_front())
    }

    async fn send(&mut self, frame: &HostToClient) -> anyhow::Result<()> {
        self.sent.push(frame.clone());
        Ok(())
    }
}

#[tokio::test]
async fn thread_create_is_visible_live_and_snapshot_and_reconnect_dedupes() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let mut channel = TestChannel {
        incoming: VecDeque::new(),
        sent: Vec::new(),
    };
    let history_id = state.storage.history_id().await.unwrap();
    let frame = ClientToHost::CreateThread {
        history_id,
        parent_thread_id: None,
        client_id: "groceries-create".into(),
        title: "Buy groceries".into(),
    };
    handle_client_frame(&state, &mut channel, frame.clone())
        .await
        .unwrap();
    let HostToClient::ThreadCreated { thread, .. } = &channel.sent[0] else {
        panic!("missing creation acknowledgement")
    };
    let thread = thread.clone();
    assert_eq!(thread.attention, hirsel_proto::ThreadAttention::Quiet);
    assert!(
        state
            .broadcast_log
            .recent()
            .contains(&HostToClient::ThreadUpsert {
                thread: thread.clone()
            })
    );
    handle_client_frame(&state, &mut channel, frame)
        .await
        .unwrap();
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 1);
    let (snapshot, mut dedupe) = build_snapshot(&state).await.unwrap();
    let HostToClient::HelloOk { threads, .. } = snapshot else {
        panic!("missing snapshot")
    };
    assert!(threads.contains(&thread));
    assert!(!dedupe.should_send(&HostToClient::ThreadUpsert {
        thread: thread.clone()
    }));
    let newer = state.storage.mark_thread_read(thread.id).await.unwrap();
    assert!(dedupe.should_send(&HostToClient::ThreadUpsert { thread: newer }));
    assert!(!dedupe.should_send(&HostToClient::ThreadUpsert {
        thread: thread.clone()
    }));
    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::OpenThread {
            client_id: "open".into(),
            thread_id: thread.id,
            before_id: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(channel.sent.last(),Some(HostToClient::ThreadOpened{client_id,detail}) if client_id=="open" && detail.thread.id==thread.id && detail.messages.is_empty())
    );
}

#[tokio::test]
async fn already_sent_old_history_mutations_cannot_touch_reused_thread_ids() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let old_history = state.storage.history_id().await.unwrap();
    let old = state
        .storage
        .create_thread(
            "old-thread",
            "Old",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    state.agent.reset_history().await.unwrap();
    let new_history = state.storage.history_id().await.unwrap();
    let fresh = state
        .storage
        .create_thread(
            "fresh-thread",
            "Fresh",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    assert_eq!(old.id, fresh.id, "reset must exercise numeric ID reuse");

    let mut channel = TestChannel {
        incoming: VecDeque::new(),
        sent: Vec::new(),
    };
    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::SendThreadMessage {
            history_id: new_history.clone(),
            client_id: "current-send".into(),
            thread_id: fresh.id,
            body: "slow:5".into(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            mode: hirsel_proto::SendMode::Send,
            artifact_ids: Vec::new(),
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state
                .storage
                .thread(fresh.id)
                .await
                .unwrap()
                .is_some_and(|thread| thread.running_turn.is_some())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for stale in [
        ClientToHost::CreateThread {
            history_id: old_history.clone(),
            parent_thread_id: Some(fresh.id),
            client_id: "stale-child".into(),
            title: "Wrong child".into(),
        },
        ClientToHost::ThreadAction {
            history_id: old_history.clone(),
            thread_id: fresh.id,
            action: "archive".into(),
            data: json!({}),
            expected_revision: None,
        },
        ClientToHost::SendThreadMessage {
            history_id: old_history.clone(),
            client_id: "stale-send".into(),
            thread_id: fresh.id,
            body: "Wrong history".into(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            mode: hirsel_proto::SendMode::Send,
            artifact_ids: Vec::new(),
        },
        ClientToHost::CancelTurn {
            history_id: old_history,
            thread_id: fresh.id,
        },
    ] {
        let error = handle_client_frame(&state, &mut channel, stale)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("history"),
            "stale frame failed for the wrong reason: {error}"
        );
    }
    let unchanged = state.storage.thread(fresh.id).await.unwrap().unwrap();
    assert_eq!(unchanged.title, "Fresh");
    assert!(unchanged.archived_at.is_none());
    assert!(unchanged.running_turn.is_some());
    assert_eq!(state.storage.thread_snapshot().await.unwrap().len(), 1);
    assert_eq!(state.storage.all_chat().await.unwrap().len(), 1);

    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::CreateThread {
            history_id: new_history.clone(),
            parent_thread_id: Some(fresh.id),
            client_id: "current-child".into(),
            title: "Current child".into(),
        },
    )
    .await
    .unwrap();
    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::ThreadAction {
            history_id: new_history.clone(),
            thread_id: fresh.id,
            action: "archive".into(),
            data: json!({}),
            expected_revision: None,
        },
    )
    .await
    .unwrap();
    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::CancelTurn {
            history_id: new_history,
            thread_id: fresh.id,
        },
    )
    .await
    .unwrap();

    let threads = state.storage.thread_snapshot().await.unwrap();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[1].parent_thread_id, Some(fresh.id));
    assert!(threads[0].archived_at.is_some());
    assert!(
        state
            .storage
            .all_chat()
            .await
            .unwrap()
            .iter()
            .any(|message| message.client_id.as_deref() == Some("current-send"))
    );
}

#[tokio::test]
async fn snapshot_failure_sends_error_instead_of_empty_hello() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(Config {
        token: "test-token".to_string(),
        agent: AgentMode::Scripted,
        provider: ProviderMode::Anthropic,
        anthropic_api_key: None,
        openrouter_api_key: None,
        model: "test-model".to_string(),
        data_dir: dir.path().to_path_buf(),
        config_path: dir.path().join("hirsel.toml"),
        docs_path: crate::templates::bundled_docs_path(),
        templates_dir: crate::templates::bundled_templates_dir(),
        driver: DriverMode::Fake,
        fake_fixture: None,
        listen: "127.0.0.1:0".parse().unwrap(),
        debug: false,
    })
    .await
    .unwrap();
    state.storage.force_hello_snapshot_error().await;
    let mut channel = TestChannel {
        incoming: VecDeque::from([IncomingFrame::Message {
            frame: ClientToHost::Hello {
                auth: HelloAuth::StaticToken("test-token".to_string()),
            },
            client_id: None,
        }]),
        sent: Vec::new(),
    };

    run_protocol(&mut channel, state, Peer::WebSocket { addr: None }).await;

    assert_eq!(channel.sent.len(), 1);
    assert!(matches!(
        &channel.sent[0],
        HostToClient::Error { detail, .. } if detail.starts_with("hello snapshot failed:")
    ));
}

#[tokio::test]
async fn artifacts_are_fetched_by_identity_and_references_survive_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let thread = state
        .storage
        .create_thread(
            "artifact-thread",
            "Artifact",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let (artifact, card) = state
        .storage
        .publish_artifact_human(
            "protocol-artifact",
            &json!({"create":"result"}),
            thread.id,
            None,
            Some(crate::storage::ArtifactDraft {
                title: "Result".into(),
                kind: hirsel_proto::ArtifactKind::File,
                mime: "text/plain".into(),
                filename: Some("result.txt".into()),
                content: "Hello".into(),
                expected_content: None,
            }),
        )
        .await
        .unwrap();
    let mut channel = TestChannel {
        incoming: VecDeque::new(),
        sent: Vec::new(),
    };
    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::ListArtifacts {
            client_id: "list".into(),
            thread_id: Some(thread.id),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(channel.sent.last(),Some(HostToClient::ArtifactsListed{client_id,artifacts}) if client_id=="list" && artifacts[0].id==artifact.summary.id)
    );
    handle_client_frame(
        &state,
        &mut channel,
        ClientToHost::OpenArtifact {
            client_id: "open".into(),
            artifact_id: artifact.summary.id,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(channel.sent.last(),Some(HostToClient::ArtifactOpened{client_id,artifact:a}) if client_id=="open" && a.content=="Hello")
    );
    let detail = state
        .storage
        .thread_detail(thread.id, None, 100)
        .await
        .unwrap();
    assert!(
        detail
            .messages
            .iter()
            .any(|m| m.id == card.as_ref().unwrap().id
                && m.artifact_ids == vec![artifact.summary.id])
    );
}

#[tokio::test]
async fn thread_summary_updates_survive_same_revision_and_reconnect_without_stale_running_replay() {
    use hirsel_proto::{ThreadAttention, ThreadTurnState};
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let (thread, _) = state
        .storage
        .create_thread(
            "summary",
            "Summary",
            "",
            &json!({}),
            ThreadAttention::NeedsOwner,
            None,
        )
        .await
        .unwrap();
    let (hello, mut dedupe) = build_snapshot(&state).await.unwrap();
    let HostToClient::HelloOk { threads, .. } = hello else {
        panic!("hello");
    };
    assert!(threads.contains(&thread));
    let queued = state
        .storage
        .queue_thread_turn(thread.id, None)
        .await
        .unwrap();
    let stale_queued = HostToClient::ThreadUpsert {
        thread: state.storage.thread(thread.id).await.unwrap().unwrap(),
    };
    let running = state.storage.run_thread_turn(queued.id).await.unwrap();
    let stale_running = HostToClient::ThreadUpsert {
        thread: state.storage.thread(thread.id).await.unwrap().unwrap(),
    };
    let current_running = super::refresh_thread_upsert(&state, stale_queued)
        .await
        .unwrap();
    assert!(
        dedupe.should_send(&current_running),
        "same revision execution update must pass hello dedupe"
    );
    assert!(
        !dedupe.should_send(&current_running),
        "identical projection is a duplicate"
    );
    let finished = state
        .storage
        .finish_thread_turn(running.id, ThreadTurnState::Completed, None)
        .await
        .unwrap();
    let completed = state.storage.thread(thread.id).await.unwrap().unwrap();
    assert_eq!(completed.last_finished_turn, Some(finished));
    assert_eq!(completed.revision, thread.revision);
    assert!(completed.settled_at.is_none());
    let (_, mut reconnect_dedupe) = build_snapshot(&state).await.unwrap();
    let refreshed = super::refresh_thread_upsert(&state, stale_running)
        .await
        .unwrap();
    assert_eq!(refreshed, HostToClient::ThreadUpsert { thread: completed });
    assert!(
        !reconnect_dedupe.should_send(&refreshed),
        "pre-snapshot running event cannot regress completed hello"
    );
    assert!(
        dedupe.should_send(&refreshed),
        "already-connected client receives terminal summary"
    );
}

#[tokio::test]
async fn same_revision_message_removal_refresh_can_reduce_activity_after_hello() {
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let original = state
        .storage
        .create_thread(
            "fixture-Conversation",
            "Conversation",
            "",
            &json!({}),
            hirsel_proto::ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap()
        .0;
    let message = state
        .storage
        .append_thread_chat(
            state
                .storage
                .create_thread(
                    "fixture-Conversation",
                    "Conversation",
                    "",
                    &serde_json::Value::Null,
                    hirsel_proto::ThreadAttention::Quiet,
                    None,
                )
                .await
                .unwrap()
                .0
                .id,
            ChatAuthor::Agent,
            "Removed",
            None,
            vec![],
        )
        .await
        .unwrap();
    let stale = state.storage.thread(original.id).await.unwrap().unwrap();
    let (_, mut dedupe) = build_snapshot(&state).await.unwrap();
    state.storage.delete_chat_message(message.id).await.unwrap();
    let updated =
        super::refresh_thread_upsert(&state, HostToClient::ThreadUpsert { thread: stale })
            .await
            .unwrap();
    assert_eq!(updated, HostToClient::ThreadUpsert { thread: original });
    assert!(
        dedupe.should_send(&updated),
        "activity timestamps are not monotonic revisions"
    );
}

#[tokio::test]
async fn direct_thread_reply_cannot_suppress_rollback_to_previous_hello_summary() {
    use hirsel_proto::ThreadAttention;
    let dir = tempfile::tempdir().unwrap();
    let state = build_state(crate::tests::test_config(dir.path()))
        .await
        .unwrap();
    let (original, _) = state
        .storage
        .create_thread(
            "direct-summary",
            "Direct",
            "",
            &json!({}),
            ThreadAttention::Quiet,
            None,
        )
        .await
        .unwrap();
    for request in [
        ClientToHost::OpenThread {
            client_id: "open".into(),
            thread_id: original.id,
            before_id: None,
        },
        ClientToHost::CreateThread {
            history_id: state.storage.history_id().await.unwrap(),
            parent_thread_id: None,
            client_id: "direct-summary".into(),
            title: "Retry".into(),
        },
    ] {
        let (_, mut dedupe) = build_snapshot(&state).await.unwrap();
        let message = state
            .storage
            .append_thread_chat(
                original.id,
                ChatAuthor::Owner,
                "Pending admission",
                None,
                vec![],
            )
            .await
            .unwrap();
        let mut channel = TestChannel {
            incoming: VecDeque::new(),
            sent: Vec::new(),
        };
        dedupe.before_request(&request);
        handle_client_frame(&state, &mut channel, request)
            .await
            .unwrap();
        let direct = match channel.sent.last().unwrap() {
            HostToClient::ThreadOpened { detail, .. } => &detail.thread,
            HostToClient::ThreadCreated { thread, .. } => thread,
            other => panic!("expected direct Thread reply, got {other:?}"),
        };
        assert_eq!(direct.last_activity_at, message.ts);
        assert_eq!(direct.revision, original.revision);
        state.storage.delete_chat_message(message.id).await.unwrap();
        let rollback = super::refresh_thread_upsert(
            &state,
            HostToClient::ThreadUpsert {
                thread: direct.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            rollback,
            HostToClient::ThreadUpsert {
                thread: original.clone()
            }
        );
        assert!(
            dedupe.should_send(&rollback),
            "direct response moved the client beyond its earlier hello snapshot"
        );
    }
}
