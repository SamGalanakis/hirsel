use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
};

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use hirsel_proto::{HostToClient, IROH_OWNER_ALPN};
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::presets};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{
    AppState,
    attachments::MAX_BLOB_BASE64_BYTES,
    protocol::{IncomingFrame, ProtocolChannel, decode_json, run_protocol},
};

pub const SECRET_KEY_FILE: &str = "iroh-secret-key";

const IROH_FRAME_ENVELOPE_BYTES: usize = 64 * 1024;

pub struct IrohServer {
    endpoint: Endpoint,
    ticket: String,
    state: AppState,
    accept_task: Option<JoinHandle<()>>,
    relay_task: Option<JoinHandle<()>>,
}

impl IrohServer {
    pub async fn start(state: AppState, data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let key_path = data_dir.as_ref().join(SECRET_KEY_FILE);
        let secret_key = load_or_create_secret_key(&key_path)?;
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .alpns(vec![IROH_OWNER_ALPN.to_vec()])
            .bind()
            .await
            .context("bind host iroh endpoint")?;

        let endpoint_id = endpoint.id();
        let ticket = EndpointTicket::new(endpoint.addr()).to_string();
        state.set_iroh_ticket(Some(ticket.clone()));
        let accept_task = tokio::spawn(accept_loop(endpoint.clone(), state.clone()));
        tracing::info!(
            node_id = %endpoint_id,
            %ticket,
            key_path = %key_path.display(),
            "Hirsel iroh endpoint bound"
        );

        let relay_task = tokio::spawn(log_relay_online(endpoint.clone(), key_path, state.clone()));
        Ok(Self {
            endpoint,
            ticket,
            state,
            accept_task: Some(accept_task),
            relay_task: Some(relay_task),
        })
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    pub async fn shutdown(mut self) {
        self.state.set_iroh_ticket(None);
        self.endpoint.close().await;
        if let Some(task) = self.accept_task.take() {
            let _ = task.await;
        }
        if let Some(task) = self.relay_task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for IrohServer {
    fn drop(&mut self) {
        self.state.set_iroh_ticket(None);
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        if let Some(task) = self.relay_task.take() {
            task.abort();
        }
    }
}

async fn log_relay_online(endpoint: Endpoint, key_path: impl AsRef<Path>, state: AppState) {
    let closed = endpoint.closed();
    if closed.run_until(endpoint.online()).await.is_none() {
        return;
    }

    let endpoint_id = endpoint.id();
    let ticket = EndpointTicket::new(endpoint.addr()).to_string();
    state.set_iroh_ticket(Some(ticket.clone()));
    tracing::info!(
        node_id = %endpoint_id,
        %ticket,
        key_path = %key_path.as_ref().display(),
        "Hirsel iroh endpoint relay-online"
    );
}

async fn accept_loop(endpoint: Endpoint, state: AppState) {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(incoming, state).await {
                tracing::debug!(%error, "iroh owner connection ended");
            }
        });
    }
}

async fn handle_connection(
    incoming: ::iroh::endpoint::Incoming,
    state: AppState,
) -> anyhow::Result<()> {
    let connection = incoming
        .accept()
        .context("accept incoming iroh connection")?
        .await
        .context("complete incoming iroh handshake")?;
    let remote_id = connection.remote_id();
    let (send, recv) = connection
        .accept_bi()
        .await
        .context("accept iroh owner bidirectional stream")?;
    tracing::debug!(%remote_id, "iroh owner connection established");

    let mut channel = IrohChannel::new(send, recv);
    run_protocol(&mut channel, state, Some(remote_id.to_string())).await;
    let _ = channel.close().await;
    connection.close(0u32.into(), b"owner protocol complete");
    Ok(())
}

struct IrohChannel {
    inbound: FramedRead<::iroh::endpoint::RecvStream, LengthDelimitedCodec>,
    outbound: FramedWrite<::iroh::endpoint::SendStream, LengthDelimitedCodec>,
}

impl IrohChannel {
    fn new(send: ::iroh::endpoint::SendStream, recv: ::iroh::endpoint::RecvStream) -> Self {
        Self {
            inbound: FramedRead::new(recv, frame_codec()),
            outbound: FramedWrite::new(send, frame_codec()),
        }
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.outbound.close().await?;
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.outbound.get_ref().stopped(),
        )
        .await;
        Ok(())
    }
}

#[async_trait]
impl ProtocolChannel for IrohChannel {
    async fn receive(&mut self) -> anyhow::Result<Option<IncomingFrame>> {
        match self.inbound.next().await {
            Some(Ok(bytes)) => Ok(Some(decode_json(&bytes))),
            Some(Err(error)) => Err(error.into()),
            None => Ok(None),
        }
    }

    async fn send(&mut self, frame: &HostToClient) -> anyhow::Result<()> {
        self.outbound
            .send(serde_json::to_vec(frame)?.into())
            .await?;
        Ok(())
    }
}

fn frame_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_BLOB_BASE64_BYTES + IROH_FRAME_ENVELOPE_BYTES)
        .new_codec()
}

fn load_or_create_secret_key(path: &Path) -> anyhow::Result<SecretKey> {
    match fs::read(path) {
        Ok(bytes) => {
            set_secret_permissions(path)?;
            secret_key_from_bytes(path, bytes)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => create_secret_key(path),
        Err(error) => {
            Err(error).with_context(|| format!("read iroh secret key from {}", path.display()))
        }
    }
}

fn create_secret_key(path: &Path) -> anyhow::Result<SecretKey> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create iroh key directory {}", parent.display()))?;
    }
    let key = SecretKey::generate();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(&key.to_bytes())
                .with_context(|| format!("write iroh secret key to {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("sync iroh secret key at {}", path.display()))?;
            set_secret_permissions(path)?;
            Ok(key)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => load_or_create_secret_key(path),
        Err(error) => {
            Err(error).with_context(|| format!("create iroh secret key at {}", path.display()))
        }
    }
}

fn secret_key_from_bytes(path: &Path, bytes: Vec<u8>) -> anyhow::Result<SecretKey> {
    let bytes: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!(
            "iroh secret key at {} must be exactly 32 bytes, got {}",
            path.display(),
            bytes.len()
        )
    })?;
    Ok(SecretKey::from_bytes(&bytes))
}

#[cfg(unix)]
fn set_secret_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set iroh secret key permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_secret_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn persisted_secret_key_is_stable_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRET_KEY_FILE);

        let first = load_or_create_secret_key(&path).unwrap();
        let second = load_or_create_secret_key(&path).unwrap();

        assert_eq!(first.to_bytes(), second.to_bytes());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
