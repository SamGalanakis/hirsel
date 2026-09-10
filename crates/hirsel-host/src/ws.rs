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
    auth::AuthPeer,
    protocol::{IncomingFrame, ProtocolChannel, decode_json, run_protocol},
};

const WS_UPLOAD_ENVELOPE_BYTES: usize = 64 * 1024;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_BLOB_BASE64_BYTES + WS_UPLOAD_ENVELOPE_BYTES)
        .max_frame_size(MAX_BLOB_BASE64_BYTES + WS_UPLOAD_ENVELOPE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, peer.ip()))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, peer_ip: std::net::IpAddr) {
    run_protocol(
        &mut WebSocketChannel(&mut socket),
        state,
        AuthPeer::WebSocket(peer_ip),
    )
    .await;
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
    use std::net::SocketAddr;

    use axum::Router;
    use futures_util::{SinkExt, StreamExt};
    use hirsel_proto::{ChatAuthor, HostToClient};
    use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    use tokio::net::{TcpListener, TcpSocket, TcpStream};
    use tokio_tungstenite::{WebSocketStream, client_async, connect_async, tungstenite::Message};

    use crate::{
        build_state,
        config::{AgentMode, Config, DriverMode, ProviderMode},
        router_from_state,
    };

    #[tokio::test]
    async fn websocket_hello_supplies_current_inventory_and_store_identity() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            token: "test-token".to_string(),
            agent: AgentMode::Scripted,
            provider: ProviderMode::Anthropic,
            anthropic_api_key: None,
            openrouter_api_key: None,
            model: "claude-opus-4-7".to_string(),
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("hirsel.toml"),
            docs_path: crate::templates::bundled_docs_path(),
            templates_dir: crate::templates::bundled_templates_dir(),
            driver: DriverMode::Fake,
            fake_fixture: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            debug: true,
        };
        let state = build_state(config.clone()).await.unwrap();
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
                "prior",
                None,
                vec![],
            )
            .await
            .unwrap();
        let app = router_from_state(state);
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        ws.send(Message::Text(
            serde_json::json!({
                "type": "hello",
                "auth": {"static_token":"test-token"}
            })
            .to_string(),
        ))
        .await
        .unwrap();
        let frame = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let response: HostToClient = serde_json::from_str(&frame).unwrap();

        match response {
            HostToClient::HelloOk {
                threads,
                history_id,
                processes,
                host_version,
                model,
                subagent_models,
                prompts,
                providers,
                views,
            } => {
                assert_eq!(threads.len(), 1);
                assert_eq!(threads[0].id, 1);
                assert!(!history_id.is_empty());
                assert!(processes.is_empty());
                assert!(views.is_empty());
                assert!(!host_version.is_empty());
                assert!(model.is_none());
                assert!(subagent_models.is_some());
                // The roster is always reported: the two built-ins exist in
                // every boot mode, detected or not.
                let roster = providers.expect("hello_ok carries the provider roster");
                assert!(
                    roster
                        .instances
                        .iter()
                        .any(|instance| instance.id == "codex")
                );
                // The prompt surface is always reported: the Agent prompt is
                // editable in every provider mode, including this one.
                let prompts = prompts.expect("prompt snapshot");
                assert!(prompts.agent.is_default);
                assert!(prompts.agent.text.contains("hirsel"));
            }
            other => panic!("unexpected hello response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn websocket_auth_throttle_uses_stable_peer_ip_across_source_ports() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path());
        config.debug = false;
        let state = build_state(config).await.unwrap();
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;

        let (mut first, first_addr) = connect_from_new_source(addr).await;
        send_local_hello(&mut first, "wrong-token").await;
        assert!(matches!(
            read_local_frame(&mut first).await,
            HostToClient::Error { detail, .. } if detail == "invalid token"
        ));

        let (mut second, second_addr) = connect_from_new_source(addr).await;
        send_local_hello(&mut second, "still-wrong").await;
        assert!(matches!(
            read_local_frame(&mut second).await,
            HostToClient::Error { detail, .. } if detail == "invalid token"
        ));

        assert_ne!(first_addr.port(), second_addr.port());
        assert_eq!(first_addr.ip(), second_addr.ip());
        let peer = crate::auth::AuthPeer::WebSocket(first_addr.ip());
        assert_eq!(state.auth_throttle.failure_attempts(&peer), Some(2));

        let (mut valid, _) = connect_from_new_source(addr).await;
        send_local_hello(&mut valid, "test-token").await;
        assert!(matches!(
            read_local_frame(&mut valid).await,
            HostToClient::HelloOk { .. }
        ));
        assert_eq!(state.auth_throttle.failure_attempts(&peer), None);
    }

    #[tokio::test]
    async fn debug_canvas_view_surface_rejects_legacy_placement_and_posts_events() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let thread = state
            .storage
            .create_thread(
                "destination",
                "Destination",
                "",
                &serde_json::json!({}),
                hirsel_proto::ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;
        let client = owner_http_client();

        let legacy = client
            .post(format!("http://{addr}/debug/show-view"))
            .json(&serde_json::json!({
                "thread_id":thread.id,
                "spec": { "type": "text", "text": "invisible" },
                "placement": "chat"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(legacy.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

        let shown: serde_json::Value = client
            .post(format!("http://{addr}/debug/show-view"))
            .json(&serde_json::json!({
                "thread_id":thread.id,
                "spec": {
                    "type": "action",
                    "label": "Continue",
                    "action": "continue"
                }
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let instance_id = shown["instance_id"].as_str().unwrap();
        assert!(shown.get("placement").is_none());

        let active: serde_json::Value = client
            .get(format!("http://{addr}/debug/views"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(active["views"].as_array().unwrap().len(), 1);
        assert_eq!(active["views"][0]["instance_id"], instance_id);

        let event: serde_json::Value = client
            .post(format!("http://{addr}/debug/view-event"))
            .json(&serde_json::json!({
                "instance_id": instance_id,
                "action": "continue",
                "data": { "confirmed": true }
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(event["message"]["author"], "owner");
        assert!(state.broadcast_log.recent().iter().any(|frame| matches!(
            frame,
            HostToClient::ViewUpsert { instance_id: id, .. } if id == instance_id
        )));
    }

    #[tokio::test]
    async fn websocket_hello_subscribe_first_covers_reconnect_races() {
        let cases = [
            (crate::protocol::HelloTestHookPoint::Subscribed, true),
            (crate::protocol::HelloTestHookPoint::Snapshotted, false),
            (crate::protocol::HelloTestHookPoint::HelloOkSent, false),
        ];
        for (index, (point, _should_be_in_snapshot)) in cases.into_iter().enumerate() {
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
            assert!(matches!(
                read_hello_ok(&mut ws).await,
                HostToClient::HelloOk { .. }
            ));
            match read_agent_msg(&mut ws).await {
                HostToClient::Msg { message } => assert_eq!(message.body, body),
                other => panic!("unexpected message response: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn websocket_upload_blob_is_idempotent_and_send_message_replays_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let thread = state
            .storage
            .create_thread(
                "destination",
                "Destination",
                "",
                &serde_json::json!({}),
                hirsel_proto::ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let history_id = state.storage.history_id().await.unwrap();
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
                "type": "send_thread_message",
                "history_id": history_id,
                "artifact_ids": [],
                "thread_id": thread.id,
                "client_id": "message-1",
                "body": "see attached",

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
    async fn websocket_enqueue_failure_reports_error_and_keeps_durable_request() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let thread = state
            .storage
            .create_thread(
                "destination",
                "Destination",
                "",
                &serde_json::json!({}),
                hirsel_proto::ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let history_id = state.storage.history_id().await.unwrap();
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        send_hello(&mut ws).await;
        let _ = read_hello_ok(&mut ws).await;

        ws.send(Message::Text(
            serde_json::json!({
                "type": "send_thread_message",
                "history_id": history_id,
                "artifact_ids": [],
                "thread_id": thread.id,
                "client_id": "enqueue-fails",
                "body": "__hirsel_test_enqueue_error__",

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
        assert_eq!(state.storage.all_chat().await.unwrap().len(), 1);
        assert_eq!(
            state.storage.pending_thread_requests().await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn websocket_rejects_unknown_mention_with_correlated_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = build_state(test_config(dir.path())).await.unwrap();
        let thread = state
            .storage
            .create_thread(
                "destination",
                "Destination",
                "",
                &serde_json::json!({}),
                hirsel_proto::ThreadAttention::Quiet,
                None,
            )
            .await
            .unwrap()
            .0;
        let history_id = state.storage.history_id().await.unwrap();
        let app = router_from_state(state.clone());
        let addr = spawn_app(app).await;

        let (mut ws, _) = connect_async(format!("ws://{addr}/ws")).await.unwrap();
        send_hello(&mut ws).await;
        let _ = read_hello_ok(&mut ws).await;
        ws.send(Message::Text(
            serde_json::json!({
                "type": "send_thread_message",
                "history_id": history_id,
                "artifact_ids": [],
                "thread_id": thread.id,
                "client_id": "bad-mention",
                "body": "What about this?",

                "mentions": [99_999]
            })
            .to_string(),
        ))
        .await
        .unwrap();

        match read_error(&mut ws).await {
            HostToClient::Error { detail, client_id } => {
                assert!(detail.contains("99999"), "{detail}");
                assert_eq!(client_id.as_deref(), Some("bad-mention"));
            }
            other => panic!("unexpected response: {other:?}"),
        }
        assert!(state.storage.all_chat().await.unwrap().is_empty());
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
        let svg = state
            .storage
            .store_blob(
                "svg-upload",
                "active.svg",
                "image/svg+xml",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".to_vec(),
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
            image_response
                .headers()
                .get("x-content-type-options")
                .unwrap(),
            "nosniff"
        );
        assert_eq!(
            image_response
                .headers()
                .get("content-security-policy")
                .unwrap(),
            "sandbox; default-src 'none'"
        );
        assert_eq!(
            image_response.bytes().await.unwrap().as_ref(),
            &[137, 80, 78, 71]
        );

        let svg_response = client
            .get(format!("http://{addr}/blob/{}", svg.blob.id))
            .bearer_auth("test-token")
            .send()
            .await
            .unwrap();
        assert_eq!(svg_response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            svg_response.headers().get(CONTENT_TYPE).unwrap(),
            "image/svg+xml"
        );
        assert_eq!(
            svg_response.headers().get(CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"active.svg\""
        );
        assert_eq!(
            svg_response
                .headers()
                .get("x-content-type-options")
                .unwrap(),
            "nosniff"
        );
        assert_eq!(
            svg_response
                .headers()
                .get("content-security-policy")
                .unwrap(),
            "sandbox; default-src 'none'"
        );
    }

    async fn spawn_app(app: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        addr
    }

    async fn connect_from_new_source(addr: SocketAddr) -> (WebSocketStream<TcpStream>, SocketAddr) {
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let stream = socket.connect(addr).await.unwrap();
        let source = stream.local_addr().unwrap();
        let (websocket, _) = client_async(format!("ws://{addr}/ws"), stream)
            .await
            .unwrap();
        (websocket, source)
    }

    async fn send_local_hello(websocket: &mut WebSocketStream<TcpStream>, token: &str) {
        websocket
            .send(Message::Text(
                serde_json::json!({
                    "type": "hello",
                    "auth": {"static_token": token}
                })
                .to_string(),
            ))
            .await
            .unwrap();
    }

    async fn read_local_frame(websocket: &mut WebSocketStream<TcpStream>) -> HostToClient {
        let message = websocket.next().await.unwrap().unwrap();
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }

    fn test_config(data_dir: &std::path::Path) -> Config {
        crate::tests::test_config(data_dir)
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
                "auth": {"static_token":token}
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
