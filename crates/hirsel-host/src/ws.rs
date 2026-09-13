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
mod tests;
