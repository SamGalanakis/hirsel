use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use hirsel_proto::{ClientToHost, HostToClient};
use tokio::sync::broadcast;

use crate::{
    AppState,
    attachments::{
        MAX_BLOB_BASE64_BYTES, decode_blob_data_b64, normalize_mime, sanitize_blob_name,
    },
    lash_runtime::OwnerTurn,
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
                    },
                )
                .await;
                return;
            }
        },
        _ => return,
    };

    let messages = match state.storage.replay_messages(hello).await {
        Ok(messages) => messages,
        Err(error) => {
            send_json(
                &mut socket,
                &HostToClient::Error {
                    detail: format!("replay failed: {error}"),
                },
            )
            .await;
            return;
        }
    };
    let inbox = match state.storage.inbox_snapshot().await {
        Ok(inbox) => inbox,
        Err(error) => {
            send_json(
                &mut socket,
                &HostToClient::Error {
                    detail: format!("inbox replay failed: {error}"),
                },
            )
            .await;
            return;
        }
    };
    let latest_msg_id = match state.storage.latest_msg_id().await {
        Ok(id) => id,
        Err(error) => {
            send_json(
                &mut socket,
                &HostToClient::Error {
                    detail: format!("latest message lookup failed: {error}"),
                },
            )
            .await;
            return;
        }
    };
    send_json(
        &mut socket,
        &HostToClient::HelloOk {
            latest_msg_id,
            messages,
            inbox,
        },
    )
    .await;

    let (mut sink, mut stream) = socket.split();
    let mut broadcasts = state.broadcaster.subscribe();
    loop {
        tokio::select! {
            frame = stream.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(error) = handle_client_frame(&state, &mut sink, &text).await {
                            let response = HostToClient::Error { detail: error.to_string() };
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
                },
            )
            .await?;
        }
        ClientToHost::SendMessage {
            client_id,
            body,
            r#ref,
            attachments,
        } => {
            let (message, inserted) = state
                .storage
                .append_owner_message(&client_id, body, r#ref, &attachments)
                .await?;
            if inserted {
                let stored_attachments = state.storage.blobs_for_message(message.id).await?;
                let _ = state.broadcaster.send(HostToClient::Msg {
                    message: message.clone(),
                });
                state
                    .agent
                    .enqueue(OwnerTurn {
                        message_id: message.id,
                        client_id,
                        body: message.body.clone(),
                        anchor: message.r#ref,
                        attachments: stored_attachments,
                    })
                    .await?;
            } else {
                send_json_sink(sink, &HostToClient::Msg { message }).await?;
            }
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
        ClientToHost::ArchiveItem { item_id } => {
            if let Some(item) = state.storage.archive_inbox_item(item_id).await? {
                let _ = state.broadcaster.send(HostToClient::InboxUpsert { item });
            }
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
mod tests {
    use std::net::SocketAddr;

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
                inbox,
            } => {
                assert_eq!(latest_msg_id, 1);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].author, ChatAuthor::Agent);
                assert!(inbox.is_empty());
            }
            other => panic!("unexpected hello response: {other:?}"),
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
            HostToClient::Msg { message } => {
                assert_eq!(message.body, "see attached");
                assert_eq!(message.attachments, vec![first_blob]);
            }
            other => panic!("unexpected message response: {other:?}"),
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
            HostToClient::Error { detail } => assert!(detail.contains("15 MB")),
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
        }
    }

    async fn send_hello(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) {
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
            HostToClient::Msg { message } => message.author == ChatAuthor::Owner,
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
