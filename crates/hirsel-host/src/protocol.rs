use async_trait::async_trait;
use hirsel_proto::{ClientToHost, HelloAuth, HostToClient, Ping};
use tokio::sync::broadcast;

use crate::{
    AppState,
    attachments::{decode_blob_data_b64, normalize_mime, sanitize_blob_name},
};

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
    async fn receive(&mut self) -> anyhow::Result<Option<IncomingFrame>>;
    async fn send(&mut self, frame: &HostToClient) -> anyhow::Result<()>;
}

pub(crate) async fn run_protocol<C>(channel: &mut C, state: AppState, peer_node_id: Option<String>)
where
    C: ProtocolChannel,
{
    let (auth, last_seen_msg_id) = match channel.receive().await {
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
    };

    let paired_token = match authenticate(&state, auth, peer_node_id.as_deref()).await {
        Ok(token) => token,
        Err(detail) => {
            let _ = channel
                .send(&HostToClient::Error {
                    detail,
                    client_id: None,
                })
                .await;
            return;
        }
    };
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

    let snapshot = match state.storage.hello_snapshot(last_seen_msg_id).await {
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
    let dedupe = HelloBroadcastDedupe::new(snapshot.latest_msg_id, snapshot.pings.clone());
    if channel
        .send(&HostToClient::HelloOk {
            latest_msg_id: snapshot.latest_msg_id,
            messages: snapshot.messages,
            pings: snapshot.pings,
            processes: state.process_snapshot().await.unwrap_or_default(),
            side_chats: state.side_chats.summaries().await,
        })
        .await
        .is_err()
    {
        return;
    }
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::HelloOkSent, &state).await;

    loop {
        tokio::select! {
            frame = channel.receive() => {
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
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn authenticate(
    state: &AppState,
    auth: HelloAuth,
    peer_node_id: Option<&str>,
) -> Result<Option<String>, String> {
    match auth {
        HelloAuth::StaticToken(token) => {
            if token == state.token.as_ref() {
                Ok(None)
            } else {
                Err("invalid token".to_string())
            }
        }
        HelloAuth::DeviceToken(token) => {
            let Some(node_id) = peer_node_id else {
                return Err("device-token auth requires iroh".to_string());
            };
            state
                .storage
                .authenticate_device_token(&token, Some(node_id))
                .await
                .map_err(|_| "invalid device token".to_string())?;
            Ok(None)
        }
        HelloAuth::PairingCode { code, device_label } => {
            let Some(node_id) = peer_node_id else {
                return Err("pairing-code auth requires iroh".to_string());
            };
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
    }
}

struct HelloBroadcastDedupe {
    latest_msg_id: u64,
    pings: Vec<Ping>,
}

impl HelloBroadcastDedupe {
    fn new(latest_msg_id: u64, pings: Vec<Ping>) -> Self {
        Self {
            latest_msg_id,
            pings,
        }
    }

    fn should_send(&self, event: &HostToClient) -> bool {
        match event {
            HostToClient::Msg { message, sc } => sc.is_some() || message.id > self.latest_msg_id,
            HostToClient::PingUpsert { ping } => {
                !self.pings.iter().any(|snapshot| snapshot == ping)
            }
            _ => true,
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
        ClientToHost::ResolvePing { ping_id } => {
            if let Some(ping) = state.storage.resolve_ping(ping_id).await? {
                state.broadcast(HostToClient::PingUpsert { ping });
            }
        }
        ClientToHost::ReadPing { ping_id } => {
            let ping = state
                .storage
                .mark_ping_read(ping_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown ping: {ping_id}"))?;
            state.broadcast(HostToClient::PingUpsert { ping });
        }
        ClientToHost::RegisterPushToken { platform, token } => {
            state.storage.register_push_token(platform, token).await?;
        }
        ClientToHost::UnregisterPushToken { token } => {
            state.storage.unregister_push_token(&token).await?;
        }
        ClientToHost::OpenSideChat {
            client_id: _,
            ping_id,
        } => {
            let (sc, messages, _) = state.side_chats.open(ping_id).await?;
            state.broadcast(HostToClient::SideChatOpen {
                sc,
                ping_id,
                messages,
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
    use std::time::Duration;

    use hirsel_proto::HelloAuth;

    use super::authenticate;
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
            model: "test-model".to_string(),
            data_dir: dir.path().to_path_buf(),
            driver: DriverMode::Fake,
            fake_fixture: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            debug: true,
            sidechat_ttl_secs: 86_400,
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
            Some("node-a"),
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
}
