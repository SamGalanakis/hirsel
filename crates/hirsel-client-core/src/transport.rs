use std::collections::HashSet;
use std::sync::Weak;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use hirsel_proto::{ClientToHost, HostToClient, IROH_OWNER_ALPN};
use iroh::{Endpoint, endpoint::presets};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::ConnectionState;
use crate::client::{ClientInner, Command, pending_to_wire, upgrade};
use crate::config::TransportTarget;
use crate::observer::LifecycleEvent;

const MAX_IROH_FRAME_BYTES: usize = 16 * 1024 * 1024;

enum SessionEnd {
    Stop,
    Disconnected { reason: String, became_online: bool },
}

enum ServerFrame {
    Message(HostToClient),
    Invalid(String),
    Ignored,
}

#[async_trait]
trait ClientChannel: Send {
    async fn receive(&mut self) -> Result<ServerFrame, String>;
    async fn send(&mut self, frame: &ClientToHost) -> Result<(), String>;
    async fn close(&mut self) -> Result<(), String>;
}

pub(crate) async fn run(inner: Weak<ClientInner>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let Some(client) = upgrade(&inner) else {
        return;
    };
    let target = client.config.transport_target();
    let reconnect = client.config.reconnect.clone();
    let iroh_secret_key = client.iroh_secret_key.clone();
    drop(client);

    let mut attempt = 0_u32;
    loop {
        let Some(client) = upgrade(&inner) else {
            return;
        };
        client.set_connection(ConnectionState::Connecting);
        client.notify_lifecycle(LifecycleEvent::Connecting { attempt });
        drop(client);

        let connect = connect_transport(&target, iroh_secret_key.clone());
        tokio::pin!(connect);
        let channel = loop {
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

        let end = match channel {
            Ok(mut channel) => run_session(&inner, &mut commands, channel.as_mut()).await,
            Err(error) => SessionEnd::Disconnected {
                reason: error,
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

async fn connect_transport(
    target: &TransportTarget,
    iroh_secret_key: Option<iroh::SecretKey>,
) -> Result<Box<dyn ClientChannel>, String> {
    match target {
        TransportTarget::WebSocket(url) => {
            let (socket, _response) = connect_async(url)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Box::new(WebSocketChannel { socket }))
        }
        TransportTarget::Iroh(ticket) => {
            let ticket = ticket
                .parse::<EndpointTicket>()
                .map_err(|error| format!("invalid iroh ticket: {error}"))?;
            let address: iroh::EndpointAddr = ticket.into();
            let remote_id = address.id;
            let secret_key = iroh_secret_key
                .ok_or_else(|| "iroh transport is missing a client secret key".to_string())?;
            let endpoint = Endpoint::builder(presets::N0)
                .secret_key(secret_key)
                .bind()
                .await
                .map_err(|error| format!("bind client iroh endpoint: {error}"))?;
            let connection = endpoint
                .connect(address, IROH_OWNER_ALPN)
                .await
                .map_err(|error| format!("connect to host over iroh: {error}"))?;
            let (send, recv) = connection
                .open_bi()
                .await
                .map_err(|error| format!("open iroh owner stream: {error}"))?;
            tracing::info!(host_node_id = %remote_id, "connection established over iroh");
            Ok(Box::new(IrohChannel::new(endpoint, connection, send, recv)))
        }
    }
}

async fn run_session(
    inner: &Weak<ClientInner>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    channel: &mut dyn ClientChannel,
) -> SessionEnd {
    let Some(client) = upgrade(inner) else {
        return SessionEnd::Stop;
    };
    let auth = client.current_auth();
    let mut awaiting_paired = matches!(auth, hirsel_proto::HelloAuth::PairingCode { .. });
    let hello = ClientToHost::Hello {
        auth,
        last_seen_msg_id: client.read_store().last_seen_msg_id,
    };
    drop(client);
    if let Err(error) = channel.send(&hello).await {
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
                    let _ = channel.close().await;
                    return SessionEnd::Stop;
                }
                Some(Command::SendPending) if online => {
                    if let Err(error) = flush_pending(inner, channel, &mut sent_this_connection).await {
                        return SessionEnd::Disconnected {
                            reason: error,
                            became_online: online,
                        };
                    }
                }
                Some(Command::SendPending) => {}
            },
            frame = channel.receive() => match frame {
                Ok(ServerFrame::Message(message)) => {
                    if let HostToClient::Paired { device_token } = message {
                        if !awaiting_paired {
                            notify_protocol_error(inner, "unexpected paired frame".to_string());
                            continue;
                        }
                        if let Some(client) = upgrade(inner) {
                            client.capture_paired_device_token(device_token);
                        }
                        awaiting_paired = false;
                        continue;
                    }
                    let hello_ok = matches!(message, HostToClient::HelloOk { .. });
                    if hello_ok && awaiting_paired {
                        notify_protocol_error(inner, "hello_ok arrived before paired".to_string());
                        return SessionEnd::Disconnected {
                            reason: "pairing handshake did not issue a device token".to_string(),
                            became_online: false,
                        };
                    }
                    handle_server_message(inner, message);
                    if hello_ok {
                        online = true;
                        sent_this_connection.clear();
                        if let Some(client) = upgrade(inner) {
                            client.set_connection(ConnectionState::Online);
                            client.notify_lifecycle(LifecycleEvent::Online);
                        }
                        if let Err(error) = flush_pending(inner, channel, &mut sent_this_connection).await {
                            return SessionEnd::Disconnected {
                                reason: error,
                                became_online: true,
                            };
                        }
                    }
                }
                Ok(ServerFrame::Invalid(error)) => {
                    notify_protocol_error(inner, format!("invalid server frame: {error}"));
                }
                Ok(ServerFrame::Ignored) => {}
                Err(error) => {
                    return SessionEnd::Disconnected {
                        reason: error,
                        became_online: online,
                    };
                }
            }
        }
    }
}

async fn flush_pending(
    inner: &Weak<ClientInner>,
    channel: &mut dyn ClientChannel,
    sent_this_connection: &mut HashSet<String>,
) -> Result<(), String> {
    let Some(client) = upgrade(inner) else {
        return Err("client dropped".into());
    };
    let frames = {
        let mut pending = client
            .pending_frames
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *pending)
    };
    let pending: Vec<_> = client.read_store().pending_sends.iter().cloned().collect();
    drop(client);

    let mut frames = frames.into_iter();
    while let Some(frame) = frames.next() {
        if let Err(error) = channel.send(&frame).await {
            if let Some(client) = upgrade(inner) {
                let mut unsent = std::collections::VecDeque::from([frame]);
                unsent.extend(frames);
                let mut pending = client
                    .pending_frames
                    .lock()
                    .unwrap_or_else(|lock_error| lock_error.into_inner());
                unsent.append(&mut pending);
                *pending = unsent;
            }
            return Err(error);
        }
    }
    for send in pending {
        if sent_this_connection.insert(send.client_id.clone()) {
            channel.send(&pending_to_wire(&send)).await?;
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
                host_version,
                ..
            } => {
                store.apply_hello_ok(latest_msg_id, messages, pings, processes, host_version);
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

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct WebSocketChannel {
    socket: ClientWebSocket,
}

#[async_trait]
impl ClientChannel for WebSocketChannel {
    async fn receive(&mut self) -> Result<ServerFrame, String> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                Ok(message) => Ok(ServerFrame::Message(message)),
                Err(error) => Ok(ServerFrame::Invalid(error.to_string())),
            },
            Some(Ok(Message::Ping(payload))) => {
                self.socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(ServerFrame::Ignored)
            }
            Some(Ok(Message::Close(frame))) => {
                Err(frame.map_or_else(|| "websocket closed".into(), |frame| frame.to_string()))
            }
            Some(Ok(_)) => Ok(ServerFrame::Ignored),
            Some(Err(error)) => Err(error.to_string()),
            None => Err("websocket stream ended".into()),
        }
    }

    async fn send(&mut self, frame: &ClientToHost) -> Result<(), String> {
        let text = serde_json::to_string(frame).map_err(|error| error.to_string())?;
        self.socket
            .send(Message::Text(text))
            .await
            .map_err(|error| error.to_string())
    }

    async fn close(&mut self) -> Result<(), String> {
        self.socket
            .close(None)
            .await
            .map_err(|error| error.to_string())
    }
}

struct IrohChannel {
    endpoint: Endpoint,
    connection: iroh::endpoint::Connection,
    inbound: FramedRead<iroh::endpoint::RecvStream, LengthDelimitedCodec>,
    outbound: FramedWrite<iroh::endpoint::SendStream, LengthDelimitedCodec>,
}

impl IrohChannel {
    fn new(
        endpoint: Endpoint,
        connection: iroh::endpoint::Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
    ) -> Self {
        Self {
            endpoint,
            connection,
            inbound: FramedRead::new(recv, iroh_frame_codec()),
            outbound: FramedWrite::new(send, iroh_frame_codec()),
        }
    }
}

#[async_trait]
impl ClientChannel for IrohChannel {
    async fn receive(&mut self) -> Result<ServerFrame, String> {
        match self.inbound.next().await {
            Some(Ok(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(message) => Ok(ServerFrame::Message(message)),
                Err(error) => Ok(ServerFrame::Invalid(error.to_string())),
            },
            Some(Err(error)) => Err(error.to_string()),
            None => Err("iroh stream ended".into()),
        }
    }

    async fn send(&mut self, frame: &ClientToHost) -> Result<(), String> {
        let bytes = serde_json::to_vec(frame).map_err(|error| error.to_string())?;
        self.outbound
            .send(bytes.into())
            .await
            .map_err(|error| error.to_string())
    }

    async fn close(&mut self) -> Result<(), String> {
        let result = self
            .outbound
            .close()
            .await
            .map_err(|error| error.to_string());
        self.connection.close(0u32.into(), b"client disconnecting");
        self.endpoint.close().await;
        result
    }
}

fn iroh_frame_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_IROH_FRAME_BYTES)
        .new_codec()
}
