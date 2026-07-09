use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use hirsel_proto::{ClientToHost, HostToClient, Ping};
use tokio::sync::broadcast;

use crate::{
    AppState,
    attachments::{
        MAX_BLOB_BASE64_BYTES, decode_blob_data_b64, normalize_mime, sanitize_blob_name,
    },
};

const WS_UPLOAD_ENVELOPE_BYTES: usize = 64 * 1024;

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.max_message_size(MAX_BLOB_BASE64_BYTES + WS_UPLOAD_ENVELOPE_BYTES)
        .max_frame_size(MAX_BLOB_BASE64_BYTES + WS_UPLOAD_ENVELOPE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let hello = match socket.recv().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientToHost>(&text) {
            Ok(ClientToHost::Hello {
                token,
                last_seen_msg_id,
            }) => {
                if token != state.token.as_ref() {
                    send_json(
                        &mut socket,
                        &HostToClient::Error {
                            detail: "invalid token".to_string(),
                            client_id: None,
                        },
                    )
                    .await;
                    return;
                }
                last_seen_msg_id
            }
            Ok(_) => {
                send_json(
                    &mut socket,
                    &HostToClient::Error {
                        detail: "hello must be the first frame".to_string(),
                        client_id: None,
                    },
                )
                .await;
                return;
            }
            Err(error) => {
                send_json(
                    &mut socket,
                    &HostToClient::Error {
                        detail: format!("invalid hello: {error}"),
                        client_id: None,
                    },
                )
                .await;
                return;
            }
        },
        _ => return,
    };

    let mut broadcasts = state.broadcaster.subscribe();
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::Subscribed, &state).await;

    let snapshot = match state.storage.hello_snapshot(hello).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            send_json(
                &mut socket,
                &HostToClient::Error {
                    detail: format!("hello snapshot failed: {error}"),
                    client_id: None,
                },
            )
            .await;
            return;
        }
    };
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::Snapshotted, &state).await;
    let dedupe = HelloBroadcastDedupe::new(snapshot.latest_msg_id, snapshot.pings.clone());
    send_json(
        &mut socket,
        &HostToClient::HelloOk {
            latest_msg_id: snapshot.latest_msg_id,
            messages: snapshot.messages,
            pings: snapshot.pings,
            processes: state.process_snapshot().await.unwrap_or_default(),
            side_chats: state.side_chats.summaries().await,
        },
    )
    .await;
    #[cfg(test)]
    run_hello_test_hook(HelloTestHookPoint::HelloOkSent, &state).await;

    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            frame = stream.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(error) = handle_client_frame(&state, &mut sink, &text).await {
                            let client_id = serde_json::from_str::<serde_json::Value>(&text)
                                .ok()
                                .and_then(|v| v.get("client_id")?.as_str().map(String::from));
                            let response = HostToClient::Error { detail: error.to_string(), client_id };
                            if send_json_sink(&mut sink, &response).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        tracing::debug!(%error, "websocket receive failed");
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
                        if send_json_sink(&mut sink, &event).await.is_err() {
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

async fn handle_client_frame(
    state: &AppState,
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    text: &str,
) -> anyhow::Result<()> {
    match serde_json::from_str::<ClientToHost>(text)? {
        ClientToHost::Hello { .. } => {
            send_json_sink(
                sink,
                &HostToClient::Error {
                    detail: "hello already completed".to_string(),
                    client_id: None,
                },
            )
            .await?;
        }
        ClientToHost::SendMessage {
            client_id,
            body,
            r#ref,
            attachments,
            mode,
            sc,
            mentions: _,
        } => {
            if let Some(sc) = sc {
                state.side_chats.send(&sc, body).await?;
            } else {
                let submission = state
                    .submit_owner_message(client_id, body, r#ref, attachments, mode)
                    .await?;
                if !submission.inserted {
                    send_json_sink(
                        sink,
                        &HostToClient::Msg {
                            message: submission.message,
                            sc: None,
                        },
                    )
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
            send_json_sink(
                sink,
                &HostToClient::BlobOk {
                    client_id,
                    blob: stored.blob,
                },
            )
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

async fn send_json(socket: &mut WebSocket, value: &HostToClient) {
    match serde_json::to_string(value) {
        Ok(text) => {
            let _ = socket.send(Message::Text(text)).await;
        }
        Err(error) => {
            tracing::warn!(%error, "failed to encode websocket response");
        }
    }
}

async fn send_json_sink(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: &HostToClient,
) -> anyhow::Result<()> {
    let text = serde_json::to_string(value)?;
    sink.send(Message::Text(text)).await?;
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelloTestHookPoint {
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
fn queue_hello_test_hook(token: String, point: HelloTestHookPoint, body: String) {
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
    use std::{net::SocketAddr, time::Duration};

    use axum::Router;
    use futures_util::{SinkExt, StreamExt};
    use hirsel_proto::{ChatAuthor, HostToClient};
    use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    use crate::{
        build_state,
        config::{AgentMode, Config, DriverMode, ProviderMode},
        router_from_state,
    };

    #[tokio::test]
    async fn websocket_hello_replays_existing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            token: "test-token".to_string(),
            agent: AgentMode::Scripted,
            provider: ProviderMode::Anthropic,
            anthropic_api_key: None,
            model: "claude-opus-4-7".to_string(),
            data_dir: dir.path().to_path_buf(),
            driver: DriverMode::Fake,
            fake_fixture: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            debug: true,
            sidechat_ttl_secs: 86_400,
        };
        let state = build_state(config.clone()).await.unwrap();
        state
            .storage
            .append_chat(ChatAuthor::Agent, "prior", None)
            .await
            .unwrap();
        let app = router_from_state(state);
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        ws.send(Message::Text(
            serde_json::json!({
                "type": "hello",
                "token": "test-token",
                "last_seen_msg_id": null
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let frame = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let response: HostToClient = serde_json::from_str(&frame).unwrap();

        match response {
            HostToClient::HelloOk {
                latest_msg_id,
                messages,
                pings,
                processes,
                side_chats,
            } => {
                assert_eq!(latest_msg_id, 1);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].author, ChatAuthor::Agent);
                assert!(pings.is_empty());
                assert!(processes.is_empty());
                assert!(side_chats.is_empty());
            }
            other => panic!("unexpected hello response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn websocket_hello_subscribe_first_covers_reconnect_races() {
        let cases = [
            (super::HelloTestHookPoint::Subscribed, true),
            (super::HelloTestHookPoint::Snapshotted, false),
            (super::HelloTestHookPoint::HelloOkSent, false),
        ];
        for (index, (point, should_be_in_snapshot)) in cases.into_iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            let token = format!("race-token-{index}");
            let body = format!("race-message-{index}");
            let mut config = test_config(dir.path());
            config.token = token.clone();
            let state = build_state(config).await.unwrap();
            super::queue_hello_test_hook(token.clone(), point, body.clone());
            let app = router_from_state(state);
            let addr = spawn_app(app).await;

            let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
            send_hello_token(&mut ws, &token).await;
            match read_hello_ok(&mut ws).await {
                HostToClient::HelloOk {
                    latest_msg_id,
                    messages,
                    ..
                } if should_be_in_snapshot => {
                    assert_eq!(latest_msg_id, 1);
                    assert_eq!(messages.len(), 1);
                    assert_eq!(messages[0].body, body);
                    assert!(
                        tokio::time::timeout(Duration::from_millis(100), ws.next())
                            .await
                            .is_err(),
                        "snapshot message should not be delivered again from the buffered broadcast"
                    );
                }
                HostToClient::HelloOk {
                    latest_msg_id,
                    messages,
                    ..
                } => {
                    assert_eq!(latest_msg_id, 0);
                    assert!(messages.is_empty());
                    match read_agent_msg(&mut ws).await {
                        HostToClient::Msg { message, .. } => assert_eq!(message.body, body),
                        other => panic!("unexpected message response: {other:?}"),
                    }
                }
                other => panic!("unexpected hello response: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn websocket_upload_blob_is_idempotent_and_send_message_replays_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let app = router_from_state(state);
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        send_hello(&mut ws).await;
        let _ = read_hello_ok(&mut ws).await;

        ws.send(Message::Text(
            serde_json::json!({
                "type": "upload_blob",
                "client_id": "upload-1",
                "name": "../note.txt",
                "mime": "text/plain",
                "data_b64": "aGVsbG8="
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let first = read_blob_ok(&mut ws).await;
        let first_blob = match first {
            HostToClient::BlobOk { blob, .. } => blob,
            other => panic!("unexpected blob response: {other:?}"),
        };

        ws.send(Message::Text(
            serde_json::json!({
                "type": "upload_blob",
                "client_id": "upload-1",
                "name": "different.txt",
                "mime": "text/plain",
                "data_b64": "b3RoZXI="
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let duplicate = read_blob_ok(&mut ws).await;
        let duplicate_blob = match duplicate {
            HostToClient::BlobOk { blob, .. } => blob,
            other => panic!("unexpected duplicate blob response: {other:?}"),
        };
        assert_eq!(duplicate_blob, first_blob);
        assert_eq!(first_blob.name, "note.txt");
        assert_eq!(first_blob.size, 5);

        ws.send(Message::Text(
            serde_json::json!({
                "type": "send_message",
                "client_id": "message-1",
                "body": "see attached",
                "ref": null,
                "attachments": [first_blob.id]
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let msg = read_owner_msg(&mut ws).await;
        match msg {
            HostToClient::Msg { message, .. } => {
                assert_eq!(message.body, "see attached");
                assert_eq!(message.attachments, vec![first_blob]);
            }
            other => panic!("unexpected message response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn websocket_send_message_enqueue_failure_returns_error_without_msg() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        send_hello(&mut ws).await;
        let _ = read_hello_ok(&mut ws).await;

        ws.send(Message::Text(
            serde_json::json!({
                "type": "send_message",
                "client_id": "enqueue-fails",
                "body": "__hirsel_test_enqueue_error__",
                "ref": null
            })
            .to_string(),
        ))
        .await
        .unwrap();

        let frame = ws.next().await.unwrap().unwrap().into_text().unwrap();
        match serde_json::from_str::<HostToClient>(&frame).unwrap() {
            HostToClient::Error { detail, client_id } => {
                assert!(detail.contains("scripted enqueue failed"));
                assert_eq!(client_id.as_deref(), Some("enqueue-fails"));
            }
            other => panic!("unexpected response before error: {other:?}"),
        }
        assert!(state.storage.all_chat().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn websocket_read_ping_marks_read_and_errors_on_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let anchor = state
            .storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap()
            .id;
        let ping = state
            .storage
            .create_ping("question", "Question", "question", anchor, true, Vec::new())
            .await
            .unwrap();
        assert!(!ping.read);
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        send_hello(&mut ws).await;
        match read_hello_ok(&mut ws).await {
            HostToClient::HelloOk { pings, .. } => {
                assert_eq!(pings.len(), 1);
                assert_eq!(pings[0].id, ping.id);
                assert!(!pings[0].read);
            }
            other => panic!("unexpected hello response: {other:?}"),
        }

        ws.send(Message::Text(
            serde_json::json!({
                "type": "read_ping",
                "ping_id": ping.id
            })
            .to_string(),
        ))
        .await
        .unwrap();
        match read_ping_upsert(&mut ws).await {
            HostToClient::PingUpsert { ping: read_ping } => {
                assert_eq!(read_ping.id, ping.id);
                assert!(read_ping.read);
            }
            other => panic!("unexpected read response: {other:?}"),
        }
        assert!(state.storage.all_pings().await.unwrap()[0].read);

        ws.send(Message::Text(
            serde_json::json!({
                "type": "read_ping",
                "ping_id": 99_999
            })
            .to_string(),
        ))
        .await
        .unwrap();
        match read_error(&mut ws).await {
            HostToClient::Error { detail, client_id } => {
                assert!(detail.contains("unknown ping"));
                assert_eq!(client_id, None);
            }
            other => panic!("unexpected error response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn websocket_upload_rejects_over_size_payload() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let app = router_from_state(state);
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        send_hello(&mut ws).await;
        let _ = read_hello_ok(&mut ws).await;
        let too_large_b64 = "A".repeat((15_usize * 1024 * 1024).div_ceil(3) * 4 + 4);

        ws.send(Message::Text(
            serde_json::json!({
                "type": "upload_blob",
                "client_id": "upload-too-large",
                "name": "too-large.bin",
                "mime": "application/octet-stream",
                "data_b64": too_large_b64
            })
            .to_string(),
        ))
        .await
        .unwrap();

        match read_error(&mut ws).await {
            HostToClient::Error { detail, .. } => assert!(detail.contains("15 MB")),
            other => panic!("unexpected error response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn blob_route_requires_token_and_serves_content_headers() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let text = state
            .storage
            .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
            .await
            .unwrap();
        let image = state
            .storage
            .store_blob(
                "image-upload",
                "tiny.png",
                "image/png",
                vec![137, 80, 78, 71],
            )
            .await
            .unwrap();
        let app = router_from_state(state);
        let addr = spawn_app(app).await;
        let client = reqwest::Client::new();

        let unauthorized = client
            .get(format!("http://{addr}/blob/{}", text.blob.id))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let missing = client
            .get(format!("http://{addr}/blob/missing?token=test-token"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

        let text_response = client
            .get(format!(
                "http://{addr}/blob/{}?token=test-token",
                text.blob.id
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(text_response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            text_response.headers().get(CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(
            text_response.headers().get(CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"note.txt\""
        );
        assert_eq!(text_response.bytes().await.unwrap().as_ref(), b"hello");

        let image_response = client
            .get(format!("http://{addr}/blob/{}", image.blob.id))
            .bearer_auth("test-token")
            .send()
            .await
            .unwrap();
        assert_eq!(image_response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            image_response.headers().get(CONTENT_TYPE).unwrap(),
            "image/png"
        );
        assert_eq!(
            image_response.headers().get(CONTENT_DISPOSITION).unwrap(),
            "inline; filename=\"tiny.png\""
        );
        assert_eq!(
            image_response.bytes().await.unwrap().as_ref(),
            &[137, 80, 78, 71]
        );
    }

    async fn spawn_app(app: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    fn test_config(data_dir: &std::path::Path) -> Config {
        Config {
            token: "test-token".to_string(),
            agent: AgentMode::Scripted,
            provider: ProviderMode::Anthropic,
            anthropic_api_key: None,
            model: "claude-opus-4-7".to_string(),
            data_dir: data_dir.to_path_buf(),
            driver: DriverMode::Fake,
            fake_fixture: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            debug: true,
            sidechat_ttl_secs: 86_400,
        }
    }

    async fn send_hello(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) {
        send_hello_token(ws, "test-token").await;
    }

    async fn send_hello_token(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        token: &str,
    ) {
        ws.send(Message::Text(
            serde_json::json!({
                "type": "hello",
                "token": token,
                "last_seen_msg_id": null
            })
            .to_string(),
        ))
        .await
        .unwrap();
    }

    async fn read_hello_ok(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> HostToClient {
        read_until(ws, |response| {
            matches!(response, HostToClient::HelloOk { .. })
        })
        .await
    }

    async fn read_blob_ok(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> HostToClient {
        read_until(ws, |response| {
            matches!(response, HostToClient::BlobOk { .. })
        })
        .await
    }

    async fn read_ping_upsert(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> HostToClient {
        read_until(ws, |response| {
            matches!(response, HostToClient::PingUpsert { .. })
        })
        .await
    }

    async fn read_owner_msg(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> HostToClient {
        read_until(ws, |response| match response {
            HostToClient::Msg { message, .. } => message.author == ChatAuthor::Owner,
            _ => false,
        })
        .await
    }

    async fn read_agent_msg(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> HostToClient {
        read_until(ws, |response| match response {
            HostToClient::Msg { message, .. } => message.author == ChatAuthor::Agent,
            _ => false,
        })
        .await
    }

    async fn read_error(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> HostToClient {
        read_until(ws, |response| {
            matches!(response, HostToClient::Error { .. })
        })
        .await
    }

    async fn read_until(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        predicate: impl Fn(&HostToClient) -> bool,
    ) -> HostToClient {
        loop {
            let frame = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let response: HostToClient = serde_json::from_str(&frame).unwrap();
            if predicate(&response) {
                return response;
            }
        }
    }
}
