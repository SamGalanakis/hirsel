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

use crate::{AppState, lash_runtime::OwnerTurn};

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
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
        } => {
            let (message, inserted) = state
                .storage
                .append_owner_message(&client_id, body, r#ref)
                .await?;
            if inserted {
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
                    })
                    .await?;
            } else {
                send_json_sink(sink, &HostToClient::Msg { message }).await?;
            }
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
    use tokio::net::TcpListener;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    use crate::{
        build_state,
        config::{Config, DriverMode},
        router_from_state,
    };

    #[tokio::test]
    async fn websocket_hello_replays_existing_chat() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            token: "test-token".to_string(),
            anthropic_api_key: None,
            model: "claude-opus-4-8".to_string(),
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

    async fn spawn_app(app: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }
}
