use async_trait::async_trait;
use hirsel_proto::{ClientToHost, HelloAuth, HostToClient};
use tokio::sync::broadcast;

use crate::{
    AppState,
    attachments::{decode_blob_data_b64, normalize_mime, sanitize_blob_name},
    auth::owner_token_matches,
};

mod hello_dedupe;

use hello_dedupe::HelloBroadcastDedupe;

const AUTH_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub(crate) const PRE_AUTH_MAX_FRAME_BYTES: usize = 8 * 1024;
pub(crate) const POST_AUTH_MAX_FRAME_BYTES: usize =
    crate::attachments::MAX_BLOB_BASE64_BYTES + 64 * 1024;

pub(crate) enum IncomingFrame {
    Message {
        frame: ClientToHost,
        client_id: Option<String>,
    },
    InvalidJson {
        detail: String,
        client_id: Option<String>,
    },
    Ignored,
}

pub(crate) fn decode_json(bytes: &[u8]) -> IncomingFrame {
    let client_id = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| value.get("client_id")?.as_str().map(String::from));
    match serde_json::from_slice(bytes) {
        Ok(frame) => IncomingFrame::Message { frame, client_id },
        Err(error) => IncomingFrame::InvalidJson {
            detail: error.to_string(),
            client_id,
        },
    }
}

#[async_trait]
pub(crate) trait ProtocolChannel: Send {
    async fn receive(&mut self, max_bytes: usize) -> anyhow::Result<Option<IncomingFrame>>;
    async fn send(&mut self, frame: &HostToClient) -> anyhow::Result<()>;
}

pub(crate) enum Peer {
    WebSocket { addr: Option<String> },
    Iroh { node_id: String },
}

pub(crate) async fn run_protocol<C>(channel: &mut C, state: AppState, peer: Peer)
where
    C: ProtocolChannel,
{
    let peer_key = match &peer {
        Peer::WebSocket { addr } => addr.as_deref(),
        Peer::Iroh { node_id } => Some(node_id.as_str()),
    };
    let first_frame = tokio::time::timeout(
        AUTH_HANDSHAKE_TIMEOUT,
        channel.receive(PRE_AUTH_MAX_FRAME_BYTES),
    )
    .await;
    let (auth, last_seen_msg_id) = match first_frame {
        Err(_) => {
            tracing::warn!(
                peer = peer_key.unwrap_or("unknown"),
                "auth handshake timed out"
            );
            return;
        }
        Ok(frame) => match frame {
            Ok(Some(IncomingFrame::Message {
                frame:
                    ClientToHost::Hello {
                        auth,
                        last_seen_msg_id,
                    },
                ..
            })) => (auth, last_seen_msg_id),
            Ok(Some(IncomingFrame::Message { .. })) => {
                let _ = channel
                    .send(&HostToClient::Error {
                        detail: "hello must be the first frame".to_string(),
                        client_id: None,
                    })
                    .await;
                return;
            }
            Ok(Some(IncomingFrame::InvalidJson { detail, .. })) => {
                let _ = channel
                    .send(&HostToClient::Error {
                        detail: format!("invalid hello: {detail}"),
                        client_id: None,
                    })
                    .await;
                return;
            }
            Ok(Some(IncomingFrame::Ignored)) | Ok(None) | Err(_) => return,
        },
    };

    let paired_token = match authenticate(&state, auth, &peer).await {
        Ok(token) => token,
        Err(detail) => {
            if let Some(peer) = peer_key {
                tokio::time::sleep(state.auth_throttle.record_failure(peer)).await;
            }
            let _ = channel
                .send(&HostToClient::Error {
                    detail,
                    client_id: None,
                })
                .await;
            return;
        }
    };
    if let Some(peer) = peer_key {
        state.auth_throttle.record_success(peer);
    }
    if let Some(device_token) = paired_token
        && channel
            .send(&HostToClient::Paired { device_token })
            .await
            .is_err()
    {
        return;
    }

    let mut broadcasts = state.broadcaster.subscribe();
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::Subscribed, &state).await;

    let (hello, mut dedupe) = match build_snapshot(&state, last_seen_msg_id).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = channel
                .send(&HostToClient::Error {
                    detail: format!("hello snapshot failed: {error}"),
                    client_id: None,
                })
                .await;
            return;
        }
    };
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::Snapshotted, &state).await;
    if channel.send(&hello).await.is_err() {
        return;
    }
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::HelloOkSent, &state).await;

    loop {
        tokio::select! {
            frame = channel.receive(POST_AUTH_MAX_FRAME_BYTES) => {
                match frame {
                    Ok(Some(IncomingFrame::Message { frame, client_id })) => {
                        if let Err(error) = handle_client_frame(&state, channel, frame).await {
                            let response = HostToClient::Error {
                                detail: error.to_string(),
                                client_id,
                            };
                            if channel.send(&response).await.is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Some(IncomingFrame::InvalidJson { detail, client_id })) => {
                        let response = HostToClient::Error { detail, client_id };
                        if channel.send(&response).await.is_err() {
                            break;
                        }
                    }
                    Ok(Some(IncomingFrame::Ignored)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        tracing::debug!(%error, "protocol transport receive failed");
                        break;
                    }
                }
            }
            event = broadcasts.recv() => {
                match event {
                    Ok(event) => {
                        if !dedupe.should_send(&event) {
                            continue;
                        }
                        if channel.send(&event).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "client broadcast receiver lagged; sending full resync");
                        match build_snapshot(&state, None).await {
                            Ok((hello, resynced)) if channel.send(&hello).await.is_ok() => {
                                dedupe = resynced;
                            }
                            Ok(_) => break,
                            Err(error) => {
                                tracing::warn!(%error, "failed to resync lagged client");
                                let _ = channel.send(&HostToClient::Error {
                                    detail: format!("resync failed: {error}"),
                                    client_id: None,
                                }).await;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Host build identity reported to clients in `hello_ok` (Settings → About).
/// Combines the crate version with the git sha embedded at build time.
pub fn host_version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("HIRSEL_GIT_SHA"))
}

async fn build_snapshot(
    state: &AppState,
    last_seen_msg_id: Option<u64>,
) -> anyhow::Result<(HostToClient, HelloBroadcastDedupe)> {
    let snapshot = state.storage.hello_snapshot(last_seen_msg_id).await?;
    let views = state.views.snapshot().await;
    let dedupe = HelloBroadcastDedupe::new(
        snapshot.latest_msg_id,
        snapshot.events.clone(),
        views.clone(),
    );
    let hello = HostToClient::HelloOk {
        latest_msg_id: snapshot.latest_msg_id,
        messages: snapshot.messages,
        events: snapshot.events,
        processes: state.process_snapshot().await?,
        side_chats: state.side_chats.summaries().await,
        host_version: host_version(),
        model: state.model_snapshot(),
        subagent_models: Some(state.subagent_model_snapshot()),
        views,
    };
    Ok((hello, dedupe))
}

async fn authenticate(
    state: &AppState,
    auth: HelloAuth,
    peer: &Peer,
) -> Result<Option<String>, String> {
    match (auth, peer) {
        (HelloAuth::StaticToken(token), _) => {
            if owner_token_matches(&state.token, &token, state.debug_enabled) {
                Ok(None)
            } else {
                Err("invalid token".to_string())
            }
        }
        (HelloAuth::DeviceToken(token), Peer::Iroh { node_id }) => {
            state
                .storage
                .authenticate_device_token(&token, Some(node_id))
                .await
                .map_err(|_| "invalid device token".to_string())?;
            Ok(None)
        }
        (HelloAuth::PairingCode { code, device_label }, Peer::Iroh { node_id }) => {
            let _ = state
                .storage
                .redeem_pairing_code(&code)
                .await
                .map_err(|_| "invalid pairing code".to_string())?;
            state
                .storage
                .issue_device_token(device_label, node_id)
                .await
                .map(Some)
                .map_err(|_| "failed to issue device token".to_string())
        }
        (HelloAuth::DeviceToken(_), Peer::WebSocket { .. }) => {
            Err("device-token auth requires iroh".to_string())
        }
        (HelloAuth::PairingCode { .. }, Peer::WebSocket { .. }) => {
            Err("pairing-code auth requires iroh".to_string())
        }
    }
}

async fn handle_client_frame<C>(
    state: &AppState,
    channel: &mut C,
    frame: ClientToHost,
) -> anyhow::Result<()>
where
    C: ProtocolChannel,
{
    match frame {
        ClientToHost::Hello { .. } => {
            channel
                .send(&HostToClient::Error {
                    detail: "hello already completed".to_string(),
                    client_id: None,
                })
                .await?;
        }
        ClientToHost::SendMessage {
            client_id,
            body,
            r#ref,
            attachments,
            mode,
            sc,
            mentions,
        } => {
            if let Some(sc) = sc {
                state.side_chats.send(&sc, body, mentions).await?;
            } else {
                let submission = state
                    .submit_owner_message(client_id, body, r#ref, attachments, mentions, mode)
                    .await?;
                if !submission.inserted {
                    channel
                        .send(&HostToClient::Msg {
                            message: submission.message,
                            sc: None,
                        })
                        .await?;
                }
            }
        }
        ClientToHost::FetchMessages {
            client_id,
            before_id,
            limit,
        } => {
            let page = state.storage.fetch_messages(before_id, limit).await?;
            channel
                .send(&HostToClient::Messages {
                    client_id,
                    before_id,
                    messages: page.messages,
                    has_more: page.has_more,
                })
                .await?;
        }
        ClientToHost::CancelTurn { sc } => {
            if let Some(sc) = sc {
                state.side_chats.cancel(&sc).await?;
            } else {
                state.cancel_turn().await?;
            }
        }
        ClientToHost::CancelQueued { client_id } => {
            state.cancel_queued_message(&client_id).await?;
        }
        ClientToHost::SetModel { model_id, variant } => {
            state.set_model(&model_id, &variant).await?;
        }
        ClientToHost::SetSubagentModel {
            provider,
            model_id,
            enabled,
            enabled_variants,
        } => {
            state
                .set_subagent_model(&provider, &model_id, enabled, &enabled_variants)
                .await?;
        }
        ClientToHost::UploadBlob {
            client_id,
            name,
            mime,
            data_b64,
        } => {
            let data = decode_blob_data_b64(&data_b64)?;
            let stored = state
                .storage
                .store_blob(
                    &client_id,
                    sanitize_blob_name(&name),
                    normalize_mime(&mime),
                    data,
                )
                .await?;
            channel
                .send(&HostToClient::BlobOk {
                    client_id,
                    blob: stored.blob,
                })
                .await?;
        }
        ClientToHost::GetBlobUrl { client_id, blob_id } => {
            if state.storage.blob(&blob_id).await?.is_none() {
                anyhow::bail!("unknown blob id: {blob_id}");
            }
            let signed = state.blob_signer.mint(&blob_id)?;
            channel
                .send(&HostToClient::BlobUrl {
                    client_id,
                    blob_id,
                    url: signed.url,
                    expires_at: signed.expires_at,
                })
                .await?;
        }
        ClientToHost::ResolvePing { ping_id } => {
            if let Some(event) = state.storage.resolve_ping(ping_id).await? {
                state.broadcast(HostToClient::EventUpsert { event });
            }
        }
        ClientToHost::ReopenPing { ping_id } => {
            if let Some(event) = state.storage.reopen_ping(ping_id).await? {
                state.broadcast(HostToClient::EventUpsert { event });
            }
        }
        ClientToHost::ReadPing { ping_id } => {
            let event = state
                .storage
                .mark_ping_read(ping_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown ping: {ping_id}"))?;
            state.broadcast(HostToClient::EventUpsert { event });
        }
        ClientToHost::EventAction {
            event_id,
            action,
            data,
        } => {
            state.handle_event_action(event_id, action, data).await?;
        }
        ClientToHost::ClearFinishedEvents {} => {
            state.tools.events_clear().await?;
        }
        ClientToHost::RegisterPushToken { platform, token } => {
            state.storage.register_push_token(platform, token).await?;
        }
        ClientToHost::UnregisterPushToken { token } => {
            state.storage.unregister_push_token(&token).await?;
        }
        ClientToHost::OpenSideChat {
            client_id: _,
            event_id,
            ping_id,
        } => {
            let (event_id, legacy_ping) = match (event_id, ping_id) {
                (Some(event_id), None) => (event_id, false),
                (None, Some(ping_id)) => (ping_id, true),
                (Some(event_id), Some(ping_id)) if event_id == ping_id => (event_id, false),
                (Some(_), Some(_)) => anyhow::bail!("event_id and ping_id must match"),
                (None, None) => anyhow::bail!("event_id or ping_id is required"),
            };
            let opened = if legacy_ping {
                state.side_chats.open_legacy_ping(event_id).await?
            } else {
                state.side_chats.open(event_id).await?
            };
            state.broadcast(HostToClient::SideChatOpen {
                sc: opened.sc,
                event_id,
                ping_id: event_id,
                event: opened.event,
                messages: opened.messages,
            });
        }
        ClientToHost::ConcludeSideChat { sc } => {
            state.side_chats.conclude(&sc).await?;
        }
        ClientToHost::ConfirmConclusion { sc, text } => {
            state.side_chats.confirm(&sc, text, state).await?;
        }
        ClientToHost::DiscardSideChat { sc } => {
            state.side_chats.discard(&sc).await?;
        }
        ClientToHost::ViewEvent {
            instance_id,
            action,
            data,
        } => {
            state.handle_view_event(instance_id, action, data).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelloTestHookPoint {
    Subscribed,
    Snapshotted,
    HelloOkSent,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct HelloTestHook {
    token: String,
    point: HelloTestHookPoint,
    body: String,
}

#[cfg(test)]
fn hello_test_hooks() -> &'static std::sync::Mutex<std::collections::VecDeque<HelloTestHook>> {
    static HOOKS: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<HelloTestHook>>> =
        std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
}

#[cfg(test)]
pub(crate) fn queue_hello_test_hook(token: String, point: HelloTestHookPoint, body: String) {
    hello_test_hooks()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push_back(HelloTestHook { token, point, body });
}

#[cfg(test)]
async fn run_hello_test_hook(point: HelloTestHookPoint, state: &AppState) {
    use hirsel_proto::ChatAuthor;

    let hook = {
        let mut hooks = hello_test_hooks()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(position) = hooks
            .iter()
            .position(|hook| hook.point == point && hook.token == state.token.as_ref())
        else {
            return;
        };
        hooks.remove(position)
    };
    if let Some(hook) = hook {
        let message = state
            .storage
            .append_chat(ChatAuthor::Agent, hook.body, None)
            .await
            .expect("hello test hook appends chat");
        state.broadcast(HostToClient::Msg { message, sc: None });
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, time::Duration};

    use async_trait::async_trait;
    use hirsel_proto::{ChatAuthor, ClientToHost, EventStatus, HelloAuth, HostToClient};
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
            compat_side_session_ttl_secs: Some(86_400),
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
            compat_side_session_ttl_secs: Some(86_400),
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
            compat_side_session_ttl_secs: Some(86_400),
        })
        .await
        .unwrap();
        state
            .storage
            .append_chat(ChatAuthor::Agent, "missed", None)
            .await
            .unwrap();
        state
            .views
            .show(
                None,
                Some(json!({ "type": "text", "text": "Still active" })),
                None,
                Some("view-reconnect".to_string()),
                "chat".to_string(),
            )
            .await
            .unwrap();

        let (frame, _) = build_snapshot(&state, None).await.unwrap();
        match frame {
            HostToClient::HelloOk {
                latest_msg_id,
                messages,
                views,
                ..
            } => {
                assert_eq!(latest_msg_id, 1);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].body, "missed");
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
    async fn fetch_messages_frame_returns_a_correlated_bounded_page() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(crate::tests::test_config(dir.path()))
            .await
            .unwrap();
        for id in 1..=3 {
            state
                .storage
                .append_chat(ChatAuthor::Agent, format!("m{id}"), None)
                .await
                .unwrap();
        }
        let mut channel = TestChannel {
            incoming: VecDeque::new(),
            sent: Vec::new(),
        };

        handle_client_frame(
            &state,
            &mut channel,
            ClientToHost::FetchMessages {
                client_id: "history-1".to_string(),
                before_id: 3,
                limit: 1,
            },
        )
        .await
        .unwrap();

        assert!(matches!(
            channel.sent.as_slice(),
            [HostToClient::Messages {
                client_id,
                before_id: 3,
                messages,
                has_more: true,
            }] if client_id == "history-1" && messages.len() == 1 && messages[0].id == 2
        ));
    }

    #[tokio::test]
    async fn clear_finished_events_archives_with_timestamp_and_broadcasts_upsert() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(crate::tests::test_config(dir.path()))
            .await
            .unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "Finished", None)
            .await
            .unwrap();
        let finished = state
            .storage
            .create_ping(
                "finished",
                "Finished",
                "Finished",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
        state.storage.resolve_ping(finished.id).await.unwrap();
        let open = state
            .storage
            .create_ping("open", "Open", "Open", anchor.id, true, Vec::new())
            .await
            .unwrap();
        let mut channel = TestChannel {
            incoming: VecDeque::new(),
            sent: Vec::new(),
        };

        handle_client_frame(&state, &mut channel, ClientToHost::ClearFinishedEvents {})
            .await
            .unwrap();

        let finished = state.storage.ping(finished.id).await.unwrap().unwrap();
        assert_eq!(finished.status, EventStatus::Done);
        assert!(finished.archived);
        assert!(finished.archived_at.is_some());
        assert!(!state.storage.ping(open.id).await.unwrap().unwrap().archived);
        assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
            frame,
            HostToClient::EventUpsert { event }
                if event.id == finished.id && event.archived_at.is_some()
        )));
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
            compat_side_session_ttl_secs: Some(86_400),
        })
        .await
        .unwrap();
        state.storage.force_hello_snapshot_error().await;
        let mut channel = TestChannel {
            incoming: VecDeque::from([IncomingFrame::Message {
                frame: ClientToHost::Hello {
                    auth: HelloAuth::StaticToken("test-token".to_string()),
                    last_seen_msg_id: None,
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
}
