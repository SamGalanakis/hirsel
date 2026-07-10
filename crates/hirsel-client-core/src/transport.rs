use std::collections::HashSet;
use std::sync::Weak;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use hirsel_proto::{ClientToHost, HostToClient};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::ConnectionState;
use crate::client::{ClientInner, Command, pending_to_wire, upgrade};
use crate::observer::LifecycleEvent;

enum SessionEnd {
    Stop,
    Disconnected { reason: String, became_online: bool },
}

pub(crate) async fn run(inner: Weak<ClientInner>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let Some(client) = upgrade(&inner) else {
        return;
    };
    let url = client.config.websocket_url();
    let reconnect = client.config.reconnect.clone();
    drop(client);

    let mut attempt = 0_u32;
    loop {
        let Some(client) = upgrade(&inner) else {
            return;
        };
        client.set_connection(ConnectionState::Connecting);
        client.notify_lifecycle(LifecycleEvent::Connecting { attempt });
        drop(client);

        let connect = connect_async(&url);
        tokio::pin!(connect);
        let socket = loop {
            tokio::select! {
                result = &mut connect => break result,
                command = commands.recv() => match command {
                    Some(Command::Stop) | None => {
                        set_offline(&inner, None);
                        return;
                    }
                    Some(Command::SendPending) => {}
                }
            }
        };

        let end = match socket {
            Ok((socket, _response)) => run_session(&inner, &mut commands, socket).await,
            Err(error) => SessionEnd::Disconnected {
                reason: error.to_string(),
                became_online: false,
            },
        };

        match end {
            SessionEnd::Stop => {
                set_offline(&inner, None);
                return;
            }
            SessionEnd::Disconnected {
                reason,
                became_online,
            } => {
                set_offline(&inner, Some(reason));
                if became_online {
                    attempt = 0;
                }
            }
        }

        let delay = reconnect.delay_ms(attempt);
        attempt = attempt.saturating_add(1);
        let sleep = tokio::time::sleep(Duration::from_millis(delay));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                command = commands.recv() => match command {
                    Some(Command::Stop) | None => return,
                    Some(Command::SendPending) => {}
                }
            }
        }
    }
}

async fn run_session<S>(
    inner: &Weak<ClientInner>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    mut socket: tokio_tungstenite::WebSocketStream<S>,
) -> SessionEnd
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(client) = upgrade(inner) else {
        return SessionEnd::Stop;
    };
    let hello = ClientToHost::Hello {
        token: client.config.token.clone(),
        last_seen_msg_id: client.read_store().last_seen_msg_id,
    };
    drop(client);
    if let Err(error) = send_json(&mut socket, &hello).await {
        return SessionEnd::Disconnected {
            reason: error,
            became_online: false,
        };
    }

    let mut online = false;
    let mut sent_this_connection = HashSet::new();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Stop) | None => {
                    let _ = socket.close(None).await;
                    return SessionEnd::Stop;
                }
                Some(Command::SendPending) if online => {
                    if let Err(error) = flush_pending(inner, &mut socket, &mut sent_this_connection).await {
                        return SessionEnd::Disconnected {
                            reason: error,
                            became_online: online,
                        };
                    }
                }
                Some(Command::SendPending) => {}
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let message = match serde_json::from_str::<HostToClient>(&text) {
                        Ok(message) => message,
                        Err(error) => {
                            notify_protocol_error(inner, format!("invalid server frame: {error}"));
                            continue;
                        }
                    };
                    let hello_ok = matches!(message, HostToClient::HelloOk { .. });
                    handle_server_message(inner, message);
                    if hello_ok {
                        online = true;
                        sent_this_connection.clear();
                        if let Some(client) = upgrade(inner) {
                            client.set_connection(ConnectionState::Online);
                            client.notify_lifecycle(LifecycleEvent::Online);
                        }
                        if let Err(error) = flush_pending(inner, &mut socket, &mut sent_this_connection).await {
                            return SessionEnd::Disconnected {
                                reason: error,
                                became_online: true,
                            };
                        }
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if let Err(error) = socket.send(Message::Pong(payload)).await {
                        return SessionEnd::Disconnected {
                            reason: error.to_string(),
                            became_online: online,
                        };
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    return SessionEnd::Disconnected {
                        reason: frame.map_or_else(
                            || "websocket closed".into(),
                            |frame| frame.to_string(),
                        ),
                        became_online: online,
                    };
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    return SessionEnd::Disconnected {
                        reason: error.to_string(),
                        became_online: online,
                    };
                }
                None => {
                    return SessionEnd::Disconnected {
                        reason: "websocket stream ended".into(),
                        became_online: online,
                    };
                }
            }
        }
    }
}

async fn flush_pending<S>(
    inner: &Weak<ClientInner>,
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    sent_this_connection: &mut HashSet<String>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(client) = upgrade(inner) else {
        return Err("client dropped".into());
    };
    let pending: Vec<_> = client.read_store().pending_sends.iter().cloned().collect();
    drop(client);
    for send in pending {
        if sent_this_connection.insert(send.client_id.clone()) {
            send_json(socket, &pending_to_wire(&send)).await?;
        }
    }
    Ok(())
}

fn handle_server_message(inner: &Weak<ClientInner>, message: HostToClient) {
    let Some(client) = upgrade(inner) else {
        return;
    };
    let changed = {
        let mut store = client.write_store();
        match message {
            HostToClient::HelloOk {
                latest_msg_id,
                messages,
                pings,
                processes,
                ..
            } => {
                store.apply_hello_ok(latest_msg_id, messages, pings, processes);
                true
            }
            HostToClient::Msg { message, sc: None } => {
                store.apply_message(message);
                true
            }
            HostToClient::AgentActivity {
                state,
                text,
                sc: None,
            } => {
                store.agent_activity.state = state;
                store.agent_activity.text = text;
                true
            }
            HostToClient::PingUpsert { ping } => {
                store.upsert_ping(ping);
                true
            }
            HostToClient::ProcessUpsert { process } => {
                store.upsert_process(process);
                true
            }
            HostToClient::Error { detail, .. } => {
                drop(store);
                client.notify_lifecycle(LifecycleEvent::ProtocolError { detail });
                false
            }
            _ => false,
        }
    };
    if changed {
        client.notify_snapshot();
    }
}

fn notify_protocol_error(inner: &Weak<ClientInner>, detail: String) {
    if let Some(client) = upgrade(inner) {
        client.notify_lifecycle(LifecycleEvent::ProtocolError { detail });
    }
}

fn set_offline(inner: &Weak<ClientInner>, reason: Option<String>) {
    if let Some(client) = upgrade(inner) {
        client.set_connection(ConnectionState::Offline);
        client.notify_lifecycle(LifecycleEvent::Offline { reason });
    }
}

async fn send_json<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    message: &ClientToHost,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(message).map_err(|error| error.to_string())?;
    socket
        .send(Message::Text(text))
        .await
        .map_err(|error| error.to_string())
}
