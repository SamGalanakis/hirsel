//! SQLite-backed persistence for the host.
//!
//! The `Storage` handle is defined here; its methods live in per-domain
//! submodules that each carry their own row mapping and tests.

mod blobs;
mod chat;
mod common;
mod devices;
mod events;
mod meta;
mod monitors;
mod plugins;
mod push_tokens;
mod schema;
mod side_chat;
mod subagents;
mod taste;

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use rusqlite::Connection;
use tokio::sync::Mutex;

use common::absolute_path;
use devices::PairingCodes;

pub use blobs::StoredBlob;
pub use chat::HelloSnapshot;
pub use devices::Device;
pub use monitors::{MonitorRecord, MonitorWakeOn, monitor_process_info};
pub use push_tokens::PushToken;
pub use subagents::SubagentRestore;
pub use taste::TasteDecision;

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
    blobs_dir: Arc<PathBuf>,
    pairing_codes: Arc<Mutex<PairingCodes>>,
}

pub(crate) const TOOL_SURFACE_FINGERPRINT_META_KEY: &str = "agent_tool_surface_fingerprint";

pub(crate) const TOOL_SURFACE_NAMES_META_KEY: &str = "agent_tool_surface_names";

pub(crate) const AGENT_SESSION_GENERATION_META_KEY: &str = "agent_session_generation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSessionState {
    pub session_id: String,
    pub rotated: bool,
    pub added_tools: Vec<String>,
}

impl Storage {
    pub async fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let data_dir = absolute_path(data_dir)?;
        let blobs_dir = data_dir.join("blobs");
        tokio::fs::create_dir_all(&data_dir).await?;
        tokio::fs::create_dir_all(&blobs_dir).await?;
        let db_path = data_dir.join("hirsel.sqlite");
        let conn = Connection::open(db_path)?;
        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir: Arc::new(blobs_dir),
            pairing_codes: Arc::new(Mutex::new(PairingCodes::default())),
        };
        storage.init().await?;
        storage.log_orphaned_blobs().await?;
        Ok(storage)
    }

    pub async fn reset(&self) -> anyhow::Result<()> {
        {
            let conn = self.conn.lock().await;
            conn.execute_batch(
                "
                DELETE FROM message_attachments;
                DELETE FROM client_blobs;
                DELETE FROM blobs;
                DELETE FROM client_messages;
                DELETE FROM pings;
                DELETE FROM side_chat_messages;
                DELETE FROM monitors;
                DELETE FROM subagent_processes;
                DELETE FROM push_tokens;
                DELETE FROM taste_decisions;
                DELETE FROM chat_messages;
                DELETE FROM sqlite_sequence
                WHERE name IN ('chat_messages', 'pings', 'side_chat_messages');
                ",
            )?;
        }
        match tokio::fs::remove_dir_all(self.blobs_dir.as_ref()).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove blob files during reset"),
        }
        tokio::fs::create_dir_all(self.blobs_dir.as_ref()).await?;
        Ok(())
    }
}
