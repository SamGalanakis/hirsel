//! SQLite-backed persistence for the host.
//!
//! The `Storage` handle is defined here; its methods live in per-domain
//! submodules that each carry their own row mapping and tests.

mod artifacts;
mod blobs;
mod chat;
mod common;
mod devices;
mod meta;
mod monitors;
mod plugins;
mod push_tokens;
mod schema;

mod thread_related;
pub(crate) use thread_related::ThreadRelated;
mod thread_activity;
mod thread_events;
mod thread_execution;
mod thread_icons;
mod thread_showcase;
pub(crate) use thread_showcase::parse_showcase;
mod thread_messages;
mod thread_requests;
mod thread_scope;
mod thread_summary;
mod threads;
pub(crate) use thread_execution::ThreadExecution;
pub(crate) use thread_icons::parse_icon;
pub(crate) use threads::ThreadPublication;
mod thread_mutations;
mod thread_read;
pub(crate) use thread_mutations::{RelatedTargetInput, ThreadMutation};
mod thread_delegation;
pub(crate) use thread_delegation::Delegation;
pub(crate) use thread_scope::{ThreadCaller, ThreadRef};

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use rusqlite::Connection;
use tokio::sync::Mutex;

use common::absolute_path;
use devices::PairingCodes;

pub(crate) use artifacts::ArtifactDraft;
pub use blobs::StoredBlob;
pub use chat::HelloSnapshot;
pub use devices::Device;
pub(crate) use monitors::monitor_process_info;
pub use monitors::{MonitorCondition, MonitorRecord};
pub use push_tokens::PushToken;

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
    blobs_dir: Arc<PathBuf>,
    pairing_codes: Arc<Mutex<PairingCodes>>,
}

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
        let mut conn = Connection::open(db_path)?;
        schema::initialize(&mut conn)?;
        thread_summary::register_timestamp_function(&conn)?;
        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir: Arc::new(blobs_dir),
            pairing_codes: Arc::new(Mutex::new(PairingCodes::default())),
        };
        storage.log_orphaned_blobs().await?;
        Ok(storage)
    }

    pub async fn reset(&self) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().await;
        {
            let tx = conn.transaction()?;
            tx.execute_batch(
                "
                DELETE FROM meta WHERE key LIKE 'thread:%';
                DELETE FROM thread_cancellations;
                DELETE FROM turn_output_artifacts;
                DELETE FROM artifact_operations;
                DELETE FROM message_artifacts;
                DELETE FROM activity_artifacts;
                DELETE FROM artifacts;
                DELETE FROM thread_related_receipts;
                DELETE FROM thread_action_receipts;
                DELETE FROM thread_requests;
                DELETE FROM thread_activity_keys;
                DELETE FROM plugin_thread_kv;
                DELETE FROM thread_execution_preferences;
                DELETE FROM thread_turn_execution;
                DELETE FROM thread_mutation_receipts;
                DELETE FROM thread_execution_bindings;
                DELETE FROM thread_reports;
                DELETE FROM thread_delegations;
                DELETE FROM thread_activities;
                DELETE FROM thread_turn_events;
                DELETE FROM thread_turns;
                DELETE FROM message_attachments;
                DELETE FROM client_blobs;
                DELETE FROM blobs;
                DELETE FROM client_messages;
                DELETE FROM monitors;
                DELETE FROM chat_messages;
                DELETE FROM threads;
                DELETE FROM sqlite_sequence
                WHERE name IN ('chat_messages', 'threads', 'thread_turns', 'thread_activities', 'thread_related_items');
                ",
            )?;
            tx.execute(
                "UPDATE meta SET value=?1 WHERE key='history_id'",
                [uuid::Uuid::new_v4().to_string()],
            )?;
            tx.commit()?;
        }
        // Keep new-history uploads excluded until the shared filesystem is ready.
        #[cfg(test)]
        blobs::tests::pause(self.blobs_dir.as_ref(), "reset").await;
        match tokio::fs::remove_dir_all(self.blobs_dir.as_ref()).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove blob files during reset"),
        }
        tokio::fs::create_dir_all(self.blobs_dir.as_ref()).await?;
        drop(conn);
        Ok(())
    }
}

#[cfg(test)]
mod threads_tests;

#[cfg(test)]
mod thread_scope_tests;

#[cfg(test)]
impl Storage {
    pub(crate) async fn test_running_caller(&self) -> ThreadCaller {
        let key = uuid::Uuid::new_v4().to_string();
        let thread = self
            .create_thread(
                &key,
                "Test conversation",
                "",
                &serde_json::json!({}),
                hirsel_proto::ThreadAttention::Quiet,
                hirsel_proto::ThreadKind::Space,
                None,
            )
            .await
            .unwrap()
            .0;
        let turn = self.start_thread_turn(thread.id, None).await.unwrap();
        self.bind_thread_execution(&self.history_id().await.unwrap(), &key, &key, turn.id)
            .await
            .unwrap()
    }
}

mod thread_completion;
