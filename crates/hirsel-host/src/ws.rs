use async_trait::async_trait;
use axum::{
    extract::{
        ConnectInfo, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use hirsel_proto::HostToClient;
use std::net::SocketAddr;

use crate::{
    AppState,
    attachments::MAX_BLOB_BASE64_BYTES,
    protocol::{IncomingFrame, ProtocolChannel, decode_json, run_protocol},
};

const WS_UPLOAD_ENVELOPE_BYTES: usize = 64 * 1024;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_BLOB_BASE64_BYTES + WS_UPLOAD_ENVELOPE_BYTES)
        .max_frame_size(MAX_BLOB_BASE64_BYTES + WS_UPLOAD_ENVELOPE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, peer.map(|peer| peer.0.to_string())))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, peer: Option<String>) {
    run_protocol(&mut WebSocketChannel(&mut socket), state, None, peer).await;
}

struct WebSocketChannel<'a>(&'a mut WebSocket);

#[async_trait]
impl ProtocolChannel for WebSocketChannel<'_> {
    async fn receive(&mut self, max_bytes: usize) -> anyhow::Result<Option<IncomingFrame>> {
        match self.0.recv().await {
            Some(Ok(Message::Text(text))) if text.len() <= max_bytes => {
                Ok(Some(decode_json(text.as_bytes())))
            }
            Some(Ok(Message::Text(_))) => anyhow::bail!("protocol frame exceeds size limit"),
            Some(Ok(Message::Close(_))) | None => Ok(None),
            Some(Ok(_)) => Ok(Some(IncomingFrame::Ignored)),
            Some(Err(error)) => Err(error.into()),
        }
    }

    async fn send(&mut self, frame: &HostToClient) -> anyhow::Result<()> {
        let text = serde_json::to_string(frame)?;
        self.0.send(Message::Text(text)).await?;
        Ok(())
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
    async fn debug_push_surface_registers_and_inspects_recorded_pushes() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;
        let client = owner_http_client();

        let unauthorized = reqwest::Client::new()
            .get(format!("http://{addr}/debug/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let registered: serde_json::Value = client
            .post(format!("http://{addr}/debug/register-push-token"))
            .json(&serde_json::json!({
                "platform": "android",
                "token": "debug-token"
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(registered["token"], "debug-token");
        assert_eq!(registered["platform"], "android");

        let anchor = state
            .storage
            .append_chat(ChatAuthor::Owner, "Choose", None)
            .await
            .unwrap();
        let ping = state
            .storage
            .create_ping(
                "debug-choice",
                "Choose from debug",
                "A or B?",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
        state.pushes.enqueue_ping(&ping).await;

        let pushes = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let body: serde_json::Value = client
                    .get(format!("http://{addr}/debug/pushes"))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if !body["pushes"].as_array().unwrap().is_empty() {
                    break body;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("debug push was recorded");
        assert_eq!(pushes["pushes"][0]["payload"]["body"], "Choose from debug");
        assert_eq!(pushes["pushes"][0]["payload"]["data"]["ping_id"], ping.id);

        let removed: serde_json::Value = client
            .post(format!("http://{addr}/debug/unregister-push-token"))
            .json(&serde_json::json!({ "token": "debug-token" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(removed["removed"], true);
        assert!(state.storage.push_tokens().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn websocket_hello_subscribe_first_covers_reconnect_races() {
        let cases = [
            (crate::protocol::HelloTestHookPoint::Subscribed, true),
            (crate::protocol::HelloTestHookPoint::Snapshotted, false),
            (crate::protocol::HelloTestHookPoint::HelloOkSent, false),
        ];
        for (index, (point, should_be_in_snapshot)) in cases.into_iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            let token = format!("race-token-{index}");
            let body = format!("race-message-{index}");
            let mut config = test_config(dir.path());
            config.token = token.clone();
            let state = build_state(config).await.unwrap();
            crate::protocol::queue_hello_test_hook(token.clone(), point, body.clone());
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
                "type": "get_blob_url",
                "client_id": "blob-url-1",
                "blob_id": first_blob.id.clone()
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let frame = ws.next().await.unwrap().unwrap().into_text().unwrap();
        match serde_json::from_str::<HostToClient>(&frame).unwrap() {
            HostToClient::BlobUrl {
                client_id,
                blob_id,
                url,
                expires_at,
            } => {
                assert_eq!(client_id, "blob-url-1");
                assert_eq!(blob_id, first_blob.id);
                assert!(url.starts_with(&format!("/blob/{}?exp=", first_blob.id)));
                assert!(url.contains("&sig="));
                assert!(expires_at > 0);
            }
            other => panic!("unexpected blob URL response: {other:?}"),
        }

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
    async fn websocket_rejects_unknown_mention_with_correlated_error() {
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
                "client_id": "bad-mention",
                "body": "What about this?",
                "ref": null,
                "mentions": [99_999]
            })
            .to_string(),
        ))
        .await
        .unwrap();

        match read_error(&mut ws).await {
            HostToClient::Error { detail, client_id } => {
                assert!(detail.contains("unknown mentioned ping: 99999"));
                assert_eq!(client_id.as_deref(), Some("bad-mention"));
            }
            other => panic!("unexpected response: {other:?}"),
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
                "ping_id": 99_999,
                "client_id": "raw-correlation"
            })
            .to_string(),
        ))
        .await
        .unwrap();
        match read_error(&mut ws).await {
            HostToClient::Error { detail, client_id } => {
                assert!(detail.contains("unknown ping"));
                assert_eq!(client_id.as_deref(), Some("raw-correlation"));
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
        let signed_text = state.blob_signer.mint(&text.blob.id).unwrap();
        let app = router_from_state(state);
        let addr = spawn_app(app).await;
        let client = reqwest::Client::new();

        let unauthorized = client
            .get(format!("http://{addr}/blob/{}", text.blob.id))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let raw_query_token = client
            .get(format!("http://{addr}/blob/missing?token=test-token"))
            .send()
            .await
            .unwrap();
        assert_eq!(raw_query_token.status(), reqwest::StatusCode::UNAUTHORIZED);

        let missing = client
            .get(format!("http://{addr}/blob/missing"))
            .bearer_auth("test-token")
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

        let text_response = client
            .get(format!("http://{addr}{}", signed_text.url))
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

    fn owner_http_client() -> reqwest::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            "Bearer test-token".parse().unwrap(),
        );
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap()
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
