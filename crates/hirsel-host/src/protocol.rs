use async_trait::async_trait;
use hirsel_proto::{ClientToHost, HelloAuth, HostToClient};
use tokio::sync::broadcast;

use crate::{
    AppState,
    attachments::{decode_blob_data_b64, normalize_mime, sanitize_blob_name},
    auth::{AuthPeer, owner_token_matches},
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

pub(crate) async fn run_protocol<C>(channel: &mut C, state: AppState, peer: AuthPeer)
where
    C: ProtocolChannel,
{
    let first_frame = tokio::time::timeout(
        AUTH_HANDSHAKE_TIMEOUT,
        channel.receive(PRE_AUTH_MAX_FRAME_BYTES),
    )
    .await;
    let auth = match first_frame {
        Err(_) => {
            tracing::warn!(peer = %peer, "auth handshake timed out");
            return;
        }
        Ok(frame) => match frame {
            Ok(Some(IncomingFrame::Message {
                frame: ClientToHost::Hello { auth },
                ..
            })) => auth,
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
            tokio::time::sleep(state.auth_throttle.record_failure(&peer)).await;
            let _ = channel
                .send(&HostToClient::Error {
                    detail,
                    client_id: None,
                })
                .await;
            return;
        }
    };
    state.auth_throttle.record_success(&peer);
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

    let (hello, mut dedupe) = match build_snapshot(&state).await {
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
                        dedupe.before_request(&frame);
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
                        // A queued upsert may predate hello or a detail response.
                        // Execution changes share the instrument revision, so
                        // deliver the current projection before deduplicating.
                        let event = match refresh_thread_upsert(&state, event).await {
                            Ok(event) => event,
                            Err(error) => {
                                tracing::warn!(%error, "failed to refresh Thread broadcast");
                                break;
                            }
                        };
                        if !dedupe.should_send(&event) {
                            continue;
                        }
                        if channel.send(&event).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "client broadcast receiver lagged; sending full resync");
                        match build_snapshot(&state).await {
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

async fn refresh_thread_upsert(
    state: &AppState,
    event: HostToClient,
) -> anyhow::Result<HostToClient> {
    if let HostToClient::ThreadUpsert { thread } = event {
        let thread = state
            .storage
            .thread(thread.id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing Thread {} for broadcast", thread.id))?;
        Ok(HostToClient::ThreadUpsert { thread })
    } else {
        Ok(event)
    }
}

/// Host build identity reported to clients in `hello_ok` (Settings → About).
/// Combines the crate version with the git sha embedded at build time.
pub fn host_version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("HIRSEL_GIT_SHA"))
}

async fn build_snapshot(state: &AppState) -> anyhow::Result<(HostToClient, HelloBroadcastDedupe)> {
    let snapshot = state.storage.hello_snapshot().await?;
    let views = state.views.snapshot().await;
    let mut dedupe = HelloBroadcastDedupe::new(views.clone());
    dedupe.include_threads(&snapshot.threads);
    let hello = HostToClient::HelloOk {
        history_id: snapshot.history_id,
        threads: snapshot.threads,
        processes: state.process_snapshot().await?,
        host_version: host_version(),
        model: state.model_snapshot(),
        subagent_models: Some(state.subagent_model_snapshot()),
        prompts: Some(state.prompt_snapshot()),
        providers: Some(state.provider_roster().await),
        views,
    };
    Ok((hello, dedupe))
}

async fn authenticate(
    state: &AppState,
    auth: HelloAuth,
    peer: &AuthPeer,
) -> Result<Option<String>, String> {
    match (auth, peer) {
        (HelloAuth::StaticToken(token), _) => {
            if owner_token_matches(&state.token, &token, state.debug_enabled) {
                Ok(None)
            } else {
                Err("invalid token".to_string())
            }
        }
        (HelloAuth::DeviceToken(token), AuthPeer::Iroh(node_id)) => {
            state
                .storage
                .authenticate_device_token(&token, Some(node_id))
                .await
                .map_err(|_| "invalid device token".to_string())?;
            Ok(None)
        }
        (HelloAuth::PairingCode { code, device_label }, AuthPeer::Iroh(node_id)) => {
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
        (HelloAuth::DeviceToken(_), AuthPeer::WebSocket(_)) => {
            Err("device-token auth requires iroh".to_string())
        }
        (HelloAuth::PairingCode { .. }, AuthPeer::WebSocket(_)) => {
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
        ClientToHost::ListArtifacts {
            client_id,
            thread_id,
        } => {
            let artifacts = state.storage.artifacts(thread_id).await?;
            channel
                .send(&HostToClient::ArtifactsListed {
                    client_id,
                    artifacts,
                })
                .await?;
        }
        ClientToHost::OpenArtifact {
            client_id,
            artifact_id,
        } => {
            let artifact = state.storage.artifact(artifact_id).await?;
            channel
                .send(&HostToClient::ArtifactOpened {
                    client_id,
                    artifact,
                })
                .await?;
        }
        ClientToHost::CreateThread {
            client_id,
            history_id,
            kind,
            title,
            parent_thread_id,
        } => {
            let (thread, inserted) = state
                .storage
                .create_addressed_thread(
                    &history_id,
                    &client_id,
                    &title,
                    "",
                    &serde_json::json!({}),
                    hirsel_proto::ThreadAttention::Quiet,
                    kind,
                    parent_thread_id,
                )
                .await?;
            if inserted {
                state.broadcast(HostToClient::ThreadUpsert {
                    thread: thread.clone(),
                });
            }
            channel
                .send(&HostToClient::ThreadCreated { client_id, thread })
                .await?;
        }
        ClientToHost::OpenThread {
            client_id,
            thread_id,
            before_id,
        } => {
            let detail = state
                .storage
                .thread_detail(thread_id, before_id, 100)
                .await?;
            channel
                .send(&HostToClient::ThreadOpened { client_id, detail })
                .await?;
        }
        ClientToHost::AddThreadRelated {
            client_id,
            history_id,
            thread_id,
            target,
            title,
        } => {
            let result = state
                .storage
                .add_thread_related(
                    &client_id,
                    &history_id,
                    thread_id,
                    &target,
                    title.as_deref(),
                )
                .await?;
            state
                .tools
                .publish_thread_related(Some(client_id), result)
                .await?;
        }
        ClientToHost::RemoveThreadRelated {
            client_id,
            history_id,
            thread_id,
            item_id,
        } => {
            let result = state
                .storage
                .remove_thread_related(&client_id, &history_id, thread_id, item_id)
                .await?;
            state
                .tools
                .publish_thread_related(Some(client_id), result)
                .await?;
        }
        ClientToHost::SendThreadMessage {
            client_id,
            history_id,
            thread_id,
            body,
            attachments,
            mentions,
            mode,
            artifact_ids,
        } => {
            let submission = state
                .submit_addressed_thread_message(
                    &history_id,
                    client_id,
                    thread_id,
                    body,
                    attachments,
                    mentions,
                    mode,
                    artifact_ids,
                )
                .await?;
            if !submission.inserted {
                channel
                    .send(&HostToClient::Msg {
                        message: submission.message,
                    })
                    .await?;
            }
        }
        ClientToHost::ThreadAction {
            client_id,
            history_id,
            thread_id,
            action,
            data,
            expected_revision,
        } => {
            state
                .handle_addressed_thread_action(
                    &history_id,
                    thread_id,
                    action,
                    data,
                    expected_revision,
                )
                .await?;
            channel
                .send(&HostToClient::ThreadActionApplied {
                    client_id,
                    history_id,
                    thread_id,
                })
                .await?;
        }

        ClientToHost::CancelTurn {
            history_id,
            thread_id,
        } => {
            state
                .agent
                .cancel_thread_turn(&history_id, thread_id)
                .await?;
        }
        ClientToHost::CancelQueued { client_id } => {
            state.cancel_queued_message(&client_id).await?;
        }
        ClientToHost::SetModel {
            provider_id,
            model_id,
            variant,
        } => {
            state
                .set_agent_model(&provider_id, &model_id, &variant)
                .await?;
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
        ClientToHost::SetNativeWorker { enabled, model } => {
            state.set_native_worker(enabled, model.as_deref()).await?;
        }
        ClientToHost::SetAgentPrompt { text } => {
            state.set_agent_prompt(&text).await?;
        }
        ClientToHost::SetForkPrompt { text } => {
            state.set_fork_prompt(&text).await?;
        }
        ClientToHost::SetForkModel {
            provider_id,
            model_id,
            variant,
        } => {
            state
                .set_fork_model(&provider_id, &model_id, &variant)
                .await?;
        }
        ClientToHost::SetAgentProvider { agent, provider_id } => {
            state.set_agent_provider(agent, &provider_id).await?;
        }
        ClientToHost::AddProvider {
            id,
            label,
            base_url,
            api_key,
            default_model,
        } => {
            state
                .add_provider(&id, &label, &base_url, &api_key, &default_model)
                .await?;
        }
        ClientToHost::UpdateProvider {
            id,
            label,
            base_url,
            api_key,
            default_model,
        } => {
            state
                .update_provider(
                    &id,
                    label.as_deref(),
                    base_url.as_deref(),
                    api_key.as_deref(),
                    default_model.as_deref(),
                )
                .await?;
        }
        ClientToHost::RemoveProvider { id } => {
            state.remove_provider(&id).await?;
        }
        ClientToHost::RedetectProvider { id } => {
            state.redetect_provider(&id).await?;
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

        ClientToHost::RegisterPushToken { platform, token } => {
            state.storage.register_push_token(platform, token).await?;
        }
        ClientToHost::UnregisterPushToken { token } => {
            state.storage.unregister_push_token(&token).await?;
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
            .append_thread_chat(
                state
                    .storage
                    .create_thread(
                        "fixture-Conversation",
                        "Conversation",
                        "",
                        &serde_json::Value::Null,
                        hirsel_proto::ThreadAttention::Quiet,
                        hirsel_proto::ThreadKind::Task,
                        None,
                    )
                    .await
                    .unwrap()
                    .0
                    .id,
                ChatAuthor::Agent,
                hook.body,
                None,
                vec![],
            )
            .await
            .expect("hello test hook appends chat");
        state.broadcast(HostToClient::Msg { message });
    }
}

#[cfg(test)]
mod tests;
