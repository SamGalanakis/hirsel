use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use hirsel_drivers::{AgentKind, SessionHandle, SubagentEvent};
use hirsel_proto::{
    Blob, ChatAuthor, ChatMessage, Ping, PingStatus, ProcessInfo, ProcessKind, ProcessState,
    PushPlatform, QuickReply, ToolCallSummary,
};
use rusqlite::{Connection, OptionalExtension, params, types::Type};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::processes::{ProcessRecord, ProcessStatus};

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
    blobs_dir: Arc<PathBuf>,
    pairing_codes: Arc<Mutex<PairingCodes>>,
}

const MAX_PAIRING_CODES: usize = 1_024;
const MAX_PAIRING_REDEMPTIONS_PER_MINUTE: usize = 256;

#[derive(Default)]
struct PairingCodes {
    codes: HashMap<String, PairingCode>,
    recent_redemptions: VecDeque<Instant>,
}

struct PairingCode {
    expires_at: Instant,
    device_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlob {
    pub blob: Blob,
    pub path: PathBuf,
    pub created_ts: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloSnapshot {
    pub latest_msg_id: u64,
    pub messages: Vec<ChatMessage>,
    pub pings: Vec<Ping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushToken {
    pub token: String,
    pub platform: PushPlatform,
    pub created_ts: DateTime<Utc>,
    pub last_seen_ts: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub device_label: String,
    pub node_id: String,
    pub created_ts: DateTime<Utc>,
    pub last_seen_ts: DateTime<Utc>,
    pub revoked_ts: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorWakeOn {
    Changed,
    ExitZero,
    ExitNonzero,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonitorRecord {
    pub id: String,
    pub cmd: String,
    pub every_secs: u64,
    pub wake_on: MonitorWakeOn,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub label: String,
    pub created_ts: DateTime<Utc>,
    pub last_event_ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_ts: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_ts: Option<DateTime<Utc>>,
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
        Ok(storage)
    }

    pub async fn init(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS chat_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                author TEXT NOT NULL,
                body TEXT NOT NULL,
                ref INTEGER NULL,
                ts TEXT NOT NULL,
                tool_calls TEXT NOT NULL DEFAULT '[]'
            );
            CREATE TABLE IF NOT EXISTS side_chat_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sc TEXT NOT NULL,
                author TEXT NOT NULL,
                body TEXT NOT NULL,
                ts TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS side_chat_messages_sc_idx ON side_chat_messages(sc);
            CREATE TABLE IF NOT EXISTS client_messages (
                client_id TEXT PRIMARY KEY,
                msg_id INTEGER NOT NULL REFERENCES chat_messages(id)
            );
            CREATE TABLE IF NOT EXISTS blobs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                mime TEXT NOT NULL,
                size INTEGER NOT NULL,
                path TEXT NOT NULL,
                created_ts TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS client_blobs (
                client_id TEXT PRIMARY KEY,
                blob_id TEXT NOT NULL REFERENCES blobs(id)
            );
            CREATE TABLE IF NOT EXISTS message_attachments (
                message_id INTEGER NOT NULL REFERENCES chat_messages(id),
                blob_id TEXT NOT NULL REFERENCES blobs(id),
                position INTEGER NOT NULL,
                PRIMARY KEY (message_id, position)
            );
            CREATE TABLE IF NOT EXISTS monitors (
                id TEXT PRIMARY KEY,
                cmd TEXT NOT NULL,
                every_secs INTEGER NOT NULL,
                wake_on TEXT NOT NULL,
                pattern TEXT NULL,
                label TEXT NOT NULL,
                created_ts TEXT NOT NULL,
                last_event_ts TEXT NOT NULL,
                last_run_ts TEXT NULL,
                last_output TEXT NULL,
                summary TEXT NULL,
                cancelled_ts TEXT NULL
            );
            CREATE TABLE IF NOT EXISTS subagent_processes (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                model TEXT NULL,
                handle_id TEXT NOT NULL,
                handle_agent TEXT NOT NULL,
                prompt TEXT NOT NULL,
                cwd TEXT NOT NULL,
                external_id TEXT NULL,
                status TEXT NOT NULL,
                events TEXT NOT NULL,
                started_ts TEXT NOT NULL,
                last_event_ts TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS push_tokens (
                token TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                created_ts TEXT NOT NULL,
                last_seen_ts TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS device_tokens (
                token TEXT PRIMARY KEY,
                device_label TEXT NOT NULL,
                node_id TEXT NOT NULL,
                created_ts TEXT NOT NULL,
                last_seen_ts TEXT NOT NULL,
                revoked_ts TEXT NULL
            );
            ",
        )?;
        // Side-chat sessions are process-local and deliberately do not survive
        // a host restart, so any rows left by an unclean shutdown are orphaned.
        conn.execute("DELETE FROM side_chat_messages", [])?;
        migrate_pings_schema(&conn)?;
        ensure_chat_tool_calls_column(&conn)?;
        Ok(())
    }

    pub async fn append_chat(
        &self,
        author: ChatAuthor,
        body: impl Into<String>,
        anchor: Option<u64>,
    ) -> anyhow::Result<ChatMessage> {
        let body = body.into();
        let ts = Utc::now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO chat_messages (author, body, ref, ts) VALUES (?1, ?2, ?3, ?4)",
            params![author_to_str(author), body, anchor, ts.to_rfc3339()],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(ChatMessage {
            id,
            author,
            body,
            r#ref: anchor,
            ts,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
        })
    }

    pub async fn append_chat_with_tool_calls(
        &self,
        author: ChatAuthor,
        body: impl Into<String>,
        anchor: Option<u64>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<ChatMessage> {
        let body = body.into();
        let ts = Utc::now();
        let encoded_tool_calls = serde_json::to_string(&tool_calls)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO chat_messages (author, body, ref, ts, tool_calls)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                author_to_str(author),
                body,
                anchor,
                ts.to_rfc3339(),
                encoded_tool_calls
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(ChatMessage {
            id,
            author,
            body,
            r#ref: anchor,
            ts,
            attachments: Vec::new(),
            tool_calls,
        })
    }

    pub async fn append_side_chat_message(
        &self,
        sc: &str,
        author: ChatAuthor,
        body: &str,
    ) -> anyhow::Result<ChatMessage> {
        let ts = Utc::now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO side_chat_messages (sc, author, body, ts) VALUES (?1, ?2, ?3, ?4)",
            params![sc, author_to_str(author), body, ts.to_rfc3339()],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(ChatMessage {
            id,
            author,
            body: body.to_string(),
            r#ref: None,
            ts,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
        })
    }

    pub async fn side_chat_transcript(&self, sc: &str) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, author, body, ts
            FROM side_chat_messages
            WHERE sc = ?1
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map(params![sc], side_chat_message_from_row)?;
        collect_rows(rows)
    }

    pub async fn delete_side_chat_transcript(&self, sc: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM side_chat_messages WHERE sc = ?1", params![sc])?;
        Ok(())
    }

    pub async fn append_owner_message(
        &self,
        client_id: &str,
        body: impl Into<String>,
        anchor: Option<u64>,
        attachments: &[String],
    ) -> anyhow::Result<(ChatMessage, bool)> {
        let body = body.into();
        let ts = Utc::now();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let existing_id = tx
            .query_row(
                "SELECT msg_id FROM client_messages WHERE client_id = ?1",
                params![client_id],
                |row| row.get::<_, u64>(0),
            )
            .optional()?;
        if let Some(existing_id) = existing_id {
            let message = get_chat_message(&tx, existing_id)?;
            tx.commit()?;
            return Ok((message, false));
        }
        validate_blob_ids(&tx, attachments)?;
        tx.execute(
            "INSERT INTO chat_messages (author, body, ref, ts) VALUES ('owner', ?1, ?2, ?3)",
            params![body, anchor, ts.to_rfc3339()],
        )?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO client_messages (client_id, msg_id) VALUES (?1, ?2)",
            params![client_id, id],
        )?;
        for (position, blob_id) in attachments.iter().enumerate() {
            tx.execute(
                "
                INSERT INTO message_attachments (message_id, blob_id, position)
                VALUES (?1, ?2, ?3)
                ",
                params![id, blob_id, position as u64],
            )?;
        }
        let message = get_chat_message(&tx, id)?;
        tx.commit()?;
        Ok((message, true))
    }

    pub async fn latest_msg_id(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock().await;
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM chat_messages",
            [],
            |row| row.get(0),
        )?)
    }

    pub async fn message_id_for_client_id(&self, client_id: &str) -> anyhow::Result<Option<u64>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT msg_id FROM client_messages WHERE client_id = ?1",
            params![client_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub async fn delete_chat_message(&self, id: u64) -> anyhow::Result<bool> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM message_attachments WHERE message_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM client_messages WHERE msg_id = ?1", params![id])?;
        let changed = tx.execute("DELETE FROM chat_messages WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub async fn replay_messages(
        &self,
        last_seen_msg_id: Option<u64>,
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        replay_messages_from_conn(&conn, last_seen_msg_id)
    }

    pub async fn hello_snapshot(
        &self,
        last_seen_msg_id: Option<u64>,
    ) -> anyhow::Result<HelloSnapshot> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let db_max: u64 = tx.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM chat_messages",
            [],
            |row| row.get(0),
        )?;
        let effective_cursor = last_seen_msg_id.filter(|cursor| *cursor <= db_max);
        let messages = replay_messages_from_conn(&tx, effective_cursor)?;
        let pings = ping_snapshot_from_conn(&tx)?;
        tx.commit()?;
        Ok(HelloSnapshot {
            latest_msg_id: db_max,
            messages,
            pings,
        })
    }

    pub async fn all_chat(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, author, body, ref, ts, tool_calls
            FROM chat_messages
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map([], chat_message_from_row)?;
        let mut messages = collect_rows(rows)?;
        load_attachments_for_messages(&conn, &mut messages)?;
        Ok(messages)
    }

    pub async fn recent_chat(&self, limit: u64) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, author, body, ref, ts, tool_calls
            FROM (
                SELECT id, author, body, ref, ts, tool_calls
                FROM chat_messages
                ORDER BY id DESC
                LIMIT ?1
            )
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map(params![limit], chat_message_from_row)?;
        let mut messages = collect_rows(rows)?;
        load_attachments_for_messages(&conn, &mut messages)?;
        Ok(messages)
    }

    pub async fn chat_message(&self, id: u64) -> anyhow::Result<Option<ChatMessage>> {
        let conn = self.conn.lock().await;
        match get_chat_message(&conn, id) {
            Ok(message) => Ok(Some(message)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn create_ping(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<Ping> {
        let name = name.into();
        let description = description.into();
        let content = content.into();
        validate_ping_fields(&name, &description)?;
        let ts = Utc::now();
        let encoded_replies = serde_json::to_string(&quick_replies)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO pings (
                name,
                description,
                content,
                anchor,
                requires_response,
                quick_replies,
                status,
                read,
                ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', 0, ?7)
            ",
            params![
                name,
                description,
                content,
                anchor,
                requires_response,
                encoded_replies,
                ts.to_rfc3339()
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(Ping {
            id,
            name,
            description,
            content,
            anchor,
            requires_response,
            quick_replies,
            status: PingStatus::Open,
            read: false,
            ts,
        })
    }

    pub async fn resolve_ping(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET status = 'done' WHERE id = ?1",
            params![ping_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, ping_id)?))
    }

    pub async fn resolve_open_pings_for_anchor(&self, anchor: u64) -> anyhow::Result<Vec<Ping>> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let ping_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM pings WHERE anchor = ?1 AND status = 'open' ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![anchor], |row| row.get::<_, u64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if ping_ids.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }
        tx.execute(
            "UPDATE pings SET status = 'done' WHERE anchor = ?1 AND status = 'open'",
            params![anchor],
        )?;
        let pings = ping_ids
            .into_iter()
            .map(|ping_id| get_ping(&tx, ping_id))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(pings)
    }

    pub async fn mark_ping_read(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE pings SET read = 1 WHERE id = ?1 AND read = 0",
            params![ping_id],
        )?;
        get_ping_optional(&conn, ping_id).map_err(Into::into)
    }

    pub async fn ping_snapshot(&self) -> anyhow::Result<Vec<Ping>> {
        let conn = self.conn.lock().await;
        ping_snapshot_from_conn(&conn)
    }

    pub async fn ping(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let conn = self.conn.lock().await;
        get_ping_optional(&conn, ping_id).map_err(Into::into)
    }

    pub async fn mentioned_pings(&self, ping_ids: &[u64]) -> anyhow::Result<Vec<Ping>> {
        let conn = self.conn.lock().await;
        ping_ids
            .iter()
            .map(|ping_id| {
                get_ping_optional(&conn, *ping_id)?
                    .ok_or_else(|| anyhow::anyhow!("unknown mentioned ping: {ping_id}"))
            })
            .collect()
    }

    pub async fn all_pings(&self) -> anyhow::Result<Vec<Ping>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, name, description, content, anchor, requires_response,
                   quick_replies, status, read, ts
            FROM pings
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map([], ping_from_row)?;
        collect_rows(rows)
    }

    pub async fn register_push_token(
        &self,
        platform: PushPlatform,
        token: impl Into<String>,
    ) -> anyhow::Result<PushToken> {
        let token = token.into();
        if token.trim().is_empty() {
            anyhow::bail!("push token must not be empty");
        }
        let now = Utc::now();
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO push_tokens (token, platform, created_ts, last_seen_ts)
            VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(token) DO UPDATE SET
                platform = excluded.platform,
                last_seen_ts = excluded.last_seen_ts
            ",
            params![token, push_platform_to_str(platform), now.to_rfc3339()],
        )?;
        get_push_token(&conn, &token).map_err(Into::into)
    }

    pub async fn unregister_push_token(&self, token: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().await;
        Ok(conn.execute("DELETE FROM push_tokens WHERE token = ?1", params![token])? > 0)
    }

    pub async fn push_tokens(&self) -> anyhow::Result<Vec<PushToken>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT token, platform, created_ts, last_seen_ts
            FROM push_tokens
            ORDER BY created_ts ASC, token ASC
            ",
        )?;
        let rows = stmt.query_map([], push_token_from_row)?;
        collect_rows(rows)
    }

    pub async fn issue_device_token(
        &self,
        device_label: impl Into<String>,
        node_id: impl Into<String>,
    ) -> anyhow::Result<String> {
        let device_label = device_label.into();
        let node_id = node_id.into();
        validate_device_label(&device_label)?;
        if node_id.trim().is_empty() {
            anyhow::bail!("device NodeId must not be empty");
        }

        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        for _ in 0..4 {
            let token = random_secret();
            match conn.execute(
                "
                INSERT INTO device_tokens (
                    token, device_label, node_id, created_ts, last_seen_ts, revoked_ts
                )
                VALUES (?1, ?2, ?3, ?4, ?4, NULL)
                ",
                params![token, device_label, node_id, now],
            ) {
                Ok(_) => return Ok(token),
                Err(error) if is_unique_constraint(&error) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("failed to generate a unique device token")
    }

    pub async fn authenticate_device_token(
        &self,
        token: &str,
        node_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let record = conn
            .query_row(
                "SELECT node_id, revoked_ts FROM device_tokens WHERE token = ?1",
                params![token],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let Some((pinned_node_id, revoked_ts)) = record else {
            anyhow::bail!("unknown device token");
        };
        if revoked_ts.is_some() {
            anyhow::bail!("revoked device token");
        }
        if node_id.is_some_and(|node_id| node_id != pinned_node_id) {
            anyhow::bail!("device token NodeId mismatch");
        }
        conn.execute(
            "UPDATE device_tokens SET last_seen_ts = ?2 WHERE token = ?1",
            params![token, now],
        )?;
        Ok(())
    }

    pub async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT device_label, node_id, created_ts, last_seen_ts, revoked_ts
            FROM device_tokens
            ORDER BY created_ts ASC, device_label ASC
            ",
        )?;
        let rows = stmt.query_map([], device_from_row)?;
        collect_rows(rows)
    }

    pub async fn revoke_device(&self, token_or_label: &str) -> anyhow::Result<usize> {
        if token_or_label.trim().is_empty() {
            anyhow::bail!("device token or label must not be empty");
        }
        let conn = self.conn.lock().await;
        Ok(conn.execute(
            "
            UPDATE device_tokens
            SET revoked_ts = ?2
            WHERE revoked_ts IS NULL AND (token = ?1 OR device_label = ?1)
            ",
            params![token_or_label, Utc::now().to_rfc3339()],
        )?)
    }

    pub async fn mint_pairing_code(
        &self,
        device_label: impl Into<String>,
        ttl: Duration,
    ) -> anyhow::Result<String> {
        let device_label = device_label.into();
        validate_device_label(&device_label)?;
        let now = Instant::now();
        let expires_at = now
            .checked_add(ttl)
            .ok_or_else(|| anyhow::anyhow!("pairing-code TTL is too large"))?;
        let mut pairing_codes = self.pairing_codes.lock().await;
        pairing_codes
            .codes
            .retain(|_, entry| entry.expires_at > now);
        if pairing_codes.codes.len() >= MAX_PAIRING_CODES {
            anyhow::bail!("too many outstanding pairing codes");
        }
        for _ in 0..4 {
            let code = random_secret();
            if pairing_codes
                .codes
                .insert(
                    code.clone(),
                    PairingCode {
                        expires_at,
                        device_label: device_label.clone(),
                    },
                )
                .is_none()
            {
                return Ok(code);
            }
        }
        anyhow::bail!("failed to generate a unique pairing code")
    }

    pub async fn redeem_pairing_code(&self, code: &str) -> anyhow::Result<String> {
        let now = Instant::now();
        let mut pairing_codes = self.pairing_codes.lock().await;
        let window_start = now - Duration::from_secs(60);
        while pairing_codes
            .recent_redemptions
            .front()
            .is_some_and(|attempt| *attempt <= window_start)
        {
            pairing_codes.recent_redemptions.pop_front();
        }
        if pairing_codes.recent_redemptions.len() >= MAX_PAIRING_REDEMPTIONS_PER_MINUTE {
            anyhow::bail!("too many pairing-code redemption attempts");
        }
        pairing_codes.recent_redemptions.push_back(now);
        let entry = pairing_codes.codes.remove(code);
        drop(pairing_codes);
        let Some(entry) = entry else {
            anyhow::bail!("unknown pairing code");
        };
        if entry.expires_at <= Instant::now() {
            anyhow::bail!("expired pairing code");
        }
        Ok(entry.device_label)
    }

    pub async fn store_blob(
        &self,
        client_id: &str,
        name: impl Into<String>,
        mime: impl Into<String>,
        data: Vec<u8>,
    ) -> anyhow::Result<StoredBlob> {
        if let Some(blob) = self.blob_for_client_id(client_id).await? {
            return Ok(blob);
        }

        tokio::fs::create_dir_all(self.blobs_dir.as_ref()).await?;
        let id = Uuid::new_v4().to_string();
        let path = self.blobs_dir.join(&id);
        tokio::fs::write(&path, &data)
            .await
            .with_context(|| format!("write blob file {}", path.display()))?;

        let created_ts = Utc::now();
        let blob = Blob {
            id: id.clone(),
            name: name.into(),
            mime: mime.into(),
            size: data.len() as u64,
        };
        let record = StoredBlob {
            blob,
            path: path.clone(),
            created_ts,
        };

        let duplicate = {
            let mut conn = self.conn.lock().await;
            let tx = conn.transaction()?;
            let duplicate_id = tx
                .query_row(
                    "SELECT blob_id FROM client_blobs WHERE client_id = ?1",
                    params![client_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(duplicate_id) = duplicate_id {
                let duplicate = get_stored_blob(&tx, &duplicate_id)?;
                tx.commit()?;
                Some(duplicate)
            } else {
                tx.execute(
                    "
                    INSERT INTO blobs (id, name, mime, size, path, created_ts)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ",
                    params![
                        record.blob.id,
                        record.blob.name,
                        record.blob.mime,
                        record.blob.size,
                        record.path.to_string_lossy(),
                        record.created_ts.to_rfc3339()
                    ],
                )?;
                tx.execute(
                    "INSERT INTO client_blobs (client_id, blob_id) VALUES (?1, ?2)",
                    params![client_id, id],
                )?;
                tx.commit()?;
                None
            }
        };

        if let Some(duplicate) = duplicate {
            if let Err(error) = tokio::fs::remove_file(&path).await {
                tracing::debug!(%error, path = %path.display(), "failed to remove duplicate blob file");
            }
            return Ok(duplicate);
        }

        Ok(record)
    }

    pub async fn blob(&self, id: &str) -> anyhow::Result<Option<StoredBlob>> {
        let conn = self.conn.lock().await;
        get_stored_blob_optional(&conn, id).map_err(Into::into)
    }

    pub async fn create_monitor(
        &self,
        cmd: impl Into<String>,
        every_secs: u64,
        wake_on: MonitorWakeOn,
        pattern: Option<String>,
        label: impl Into<String>,
    ) -> anyhow::Result<MonitorRecord> {
        let now = Utc::now();
        let record = MonitorRecord {
            id: format!("mon-{}", Uuid::new_v4()),
            cmd: cmd.into(),
            every_secs: every_secs.max(30),
            wake_on,
            pattern,
            label: label.into(),
            created_ts: now,
            last_event_ts: now,
            last_run_ts: None,
            last_output: None,
            summary: None,
            cancelled_ts: None,
        };
        validate_monitor_record(&record)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO monitors (
                id,
                cmd,
                every_secs,
                wake_on,
                pattern,
                label,
                created_ts,
                last_event_ts,
                last_run_ts,
                last_output,
                summary,
                cancelled_ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, NULL)
            ",
            params![
                record.id,
                record.cmd,
                record.every_secs,
                monitor_wake_on_to_str(record.wake_on),
                record.pattern,
                record.label,
                record.created_ts.to_rfc3339(),
                record.last_event_ts.to_rfc3339(),
            ],
        )?;
        get_monitor(&conn, &record.id).map_err(Into::into)
    }

    pub async fn monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let conn = self.conn.lock().await;
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn active_monitors(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
                   last_run_ts, last_output, summary, cancelled_ts
            FROM monitors
            WHERE cancelled_ts IS NULL
            ORDER BY created_ts ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], monitor_from_row)?;
        collect_rows(rows)
    }

    pub async fn monitors_list(&self) -> anyhow::Result<Vec<MonitorRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
                   last_run_ts, last_output, summary, cancelled_ts
            FROM monitors
            ORDER BY created_ts ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], monitor_from_row)?;
        collect_rows(rows)
    }

    pub async fn cancel_monitor(&self, monitor_id: &str) -> anyhow::Result<Option<MonitorRecord>> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "
            UPDATE monitors
            SET cancelled_ts = COALESCE(cancelled_ts, ?2),
                last_event_ts = ?2,
                summary = 'cancelled'
            WHERE id = ?1
            ",
            params![monitor_id, now.to_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn record_monitor_tick(
        &self,
        monitor_id: &str,
        last_output: String,
        summary: String,
    ) -> anyhow::Result<Option<MonitorRecord>> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "
            UPDATE monitors
            SET last_run_ts = ?2,
                last_event_ts = ?2,
                last_output = ?3,
                summary = ?4
            WHERE id = ?1 AND cancelled_ts IS NULL
            ",
            params![monitor_id, now.to_rfc3339(), last_output, summary],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        get_monitor_optional(&conn, monitor_id).map_err(Into::into)
    }

    pub async fn monitor_snapshot(&self) -> anyhow::Result<Vec<ProcessInfo>> {
        let mut records = self.monitors_list().await?;
        let mut active = Vec::new();
        let mut terminal = Vec::new();
        for record in records.drain(..) {
            if record.cancelled_ts.is_some() {
                terminal.push(record);
            } else {
                active.push(record);
            }
        }
        terminal.sort_by(|left, right| {
            left.last_event_ts
                .cmp(&right.last_event_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        if terminal.len() > 10 {
            terminal.drain(..terminal.len() - 10);
        }
        Ok(active
            .iter()
            .chain(terminal.iter())
            .map(monitor_process_info)
            .collect())
    }

    pub async fn upsert_subagent_process(&self, record: &ProcessRecord) -> anyhow::Result<()> {
        let events = serde_json::to_string(&record.events)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO subagent_processes (
                id,
                agent,
                model,
                handle_id,
                handle_agent,
                prompt,
                cwd,
                external_id,
                status,
                events,
                started_ts,
                last_event_ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                agent = excluded.agent,
                model = excluded.model,
                handle_id = excluded.handle_id,
                handle_agent = excluded.handle_agent,
                prompt = excluded.prompt,
                cwd = excluded.cwd,
                external_id = excluded.external_id,
                status = excluded.status,
                events = excluded.events,
                started_ts = excluded.started_ts,
                last_event_ts = excluded.last_event_ts
            ",
            params![
                record.id,
                agent_kind_to_str(record.agent),
                record.model,
                record.handle.id,
                agent_kind_to_str(record.handle.agent),
                record.prompt,
                record.cwd,
                record.external_id,
                process_status_to_str(record.status),
                events,
                record.started_ts.to_rfc3339(),
                record.last_event_ts.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub async fn restore_subagent_processes_after_restart(
        &self,
    ) -> anyhow::Result<SubagentRestore> {
        let now = Utc::now();
        let conn = self.conn.lock().await;
        let mut records = {
            let mut stmt = conn.prepare(
                "
                SELECT id, agent, model, handle_id, handle_agent, prompt, cwd, external_id,
                       status, events, started_ts, last_event_ts
                FROM subagent_processes
                ORDER BY started_ts ASC, id ASC
                ",
            )?;
            let rows = stmt.query_map([], subagent_process_from_row)?;
            collect_rows(rows)?
        };
        let mut abandoned = Vec::new();
        for record in &mut records {
            if record.status != ProcessStatus::Running {
                continue;
            }
            record.status = ProcessStatus::Abandoned;
            record.last_event_ts = now;
            abandoned.push(record.id.clone());
            conn.execute(
                "
                UPDATE subagent_processes
                SET status = 'abandoned',
                    last_event_ts = ?2
                WHERE id = ?1
                ",
                params![record.id, now.to_rfc3339()],
            )?;
        }
        Ok(SubagentRestore { records, abandoned })
    }

    pub async fn blobs_for_message(&self, message_id: u64) -> anyhow::Result<Vec<StoredBlob>> {
        let conn = self.conn.lock().await;
        message_attachments(&conn, message_id).map_err(Into::into)
    }

    async fn blob_for_client_id(&self, client_id: &str) -> anyhow::Result<Option<StoredBlob>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "
            SELECT b.id, b.name, b.mime, b.size, b.path, b.created_ts
            FROM blobs b
            JOIN client_blobs cb ON cb.blob_id = b.id
            WHERE cb.client_id = ?1
            ",
            params![client_id],
            stored_blob_from_row,
        )
        .optional()
        .map_err(Into::into)
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

#[derive(Debug, Clone)]
pub struct SubagentRestore {
    pub records: Vec<ProcessRecord>,
    pub abandoned: Vec<String>,
}

fn collect_rows<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> anyhow::Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn replay_messages_from_conn(
    conn: &Connection,
    last_seen_msg_id: Option<u64>,
) -> anyhow::Result<Vec<ChatMessage>> {
    let mut messages = if let Some(last_seen_msg_id) = last_seen_msg_id {
        let mut stmt = conn.prepare(
            "
            SELECT id, author, body, ref, ts, tool_calls
            FROM chat_messages
            WHERE id > ?1
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map(params![last_seen_msg_id], chat_message_from_row)?;
        collect_rows(rows)?
    } else {
        let mut stmt = conn.prepare(
            "
            SELECT id, author, body, ref, ts, tool_calls
            FROM (
                SELECT id, author, body, ref, ts, tool_calls
                FROM chat_messages
                ORDER BY id DESC
                LIMIT 200
            )
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map([], chat_message_from_row)?;
        collect_rows(rows)?
    };
    load_attachments_for_messages(conn, &mut messages)?;
    Ok(messages)
}

fn ping_snapshot_from_conn(conn: &Connection) -> anyhow::Result<Vec<Ping>> {
    let mut pings = Vec::new();
    {
        let mut stmt = conn.prepare(
            "
            SELECT id, name, description, content, anchor, requires_response,
                   quick_replies, status, read, ts
            FROM pings
            WHERE status = 'open'
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map([], ping_from_row)?;
        pings.extend(collect_rows(rows)?);
    }
    {
        let mut stmt = conn.prepare(
            "
            SELECT id, name, description, content, anchor, requires_response,
                   quick_replies, status, read, ts
            FROM pings
            WHERE status = 'done'
            ORDER BY id DESC
            LIMIT 20
            ",
        )?;
        let rows = stmt.query_map([], ping_from_row)?;
        let mut done = collect_rows(rows)?;
        done.reverse();
        pings.extend(done);
    }
    Ok(pings)
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn validate_blob_ids(conn: &Connection, blob_ids: &[String]) -> anyhow::Result<()> {
    for blob_id in blob_ids {
        if get_stored_blob_optional(conn, blob_id)?.is_none() {
            anyhow::bail!("unknown blob id: {blob_id}");
        }
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table],
        |row| row.get(0),
    )
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for candidate in columns {
        if candidate? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_pings_schema(conn: &Connection) -> anyhow::Result<()> {
    if table_exists(conn, "inbox_items")? && !table_exists(conn, "pings")? {
        conn.execute("ALTER TABLE inbox_items RENAME TO pings", [])?;
    }
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            description TEXT NOT NULL,
            content TEXT NOT NULL,
            anchor INTEGER NOT NULL REFERENCES chat_messages(id),
            requires_response INTEGER NOT NULL,
            quick_replies TEXT NOT NULL,
            status TEXT NOT NULL,
            read INTEGER NOT NULL DEFAULT 0,
            ts TEXT NOT NULL
        );
        ",
    )?;
    if !table_has_column(conn, "pings", "read")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN read INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !table_has_column(conn, "pings", "name")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !table_has_column(conn, "pings", "description")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN description TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    conn.execute(
        "UPDATE pings SET status = 'done' WHERE status = 'archived'",
        [],
    )?;

    let legacy = {
        let mut stmt =
            conn.prepare("SELECT id, content FROM pings WHERE name = '' OR description = ''")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, content) in legacy {
        let name = derive_ping_name(&content);
        let description = derive_ping_description(&content);
        conn.execute(
            "
            UPDATE pings
            SET name = CASE WHEN name = '' THEN ?2 ELSE name END,
                description = CASE WHEN description = '' THEN ?3 ELSE description END
            WHERE id = ?1
            ",
            params![id, name, description],
        )?;
    }
    Ok(())
}

fn derive_ping_name(content: &str) -> String {
    let name = content
        .split_whitespace()
        .take(4)
        .flat_map(|word| word.chars().chain(std::iter::once('-')))
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let compact = name
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let fallback = if compact.is_empty() {
        "legacy-ping"
    } else {
        &compact
    };
    fallback.chars().take(32).collect()
}

fn derive_ping_description(content: &str) -> String {
    content
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or("Legacy ping")
        .to_string()
}

fn validate_ping_fields(name: &str, description: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("ping name is required");
    }
    if name.chars().count() > 32 {
        anyhow::bail!("ping name must be at most 32 characters");
    }
    if description.trim().is_empty() {
        anyhow::bail!("ping description is required");
    }
    if description.lines().count() != 1 {
        anyhow::bail!("ping description must be one line");
    }
    Ok(())
}

fn ensure_chat_tool_calls_column(conn: &Connection) -> rusqlite::Result<()> {
    if chat_messages_has_tool_calls_column(conn)? {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE chat_messages ADD COLUMN tool_calls TEXT NOT NULL DEFAULT '[]'",
        [],
    )?;
    Ok(())
}

fn chat_messages_has_tool_calls_column(conn: &Connection) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(chat_messages)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "tool_calls" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn get_chat_message(conn: &Connection, id: u64) -> rusqlite::Result<ChatMessage> {
    let mut message = conn.query_row(
        "
        SELECT id, author, body, ref, ts, tool_calls
        FROM chat_messages
        WHERE id = ?1
        ",
        params![id],
        chat_message_from_row,
    )?;
    message.attachments = message_attachments(conn, id)?
        .into_iter()
        .map(|stored| stored.blob)
        .collect();
    Ok(message)
}

fn get_ping(conn: &Connection, id: u64) -> rusqlite::Result<Ping> {
    get_ping_optional(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

fn get_ping_optional(conn: &Connection, id: u64) -> rusqlite::Result<Option<Ping>> {
    conn.query_row(
        "
        SELECT id, name, description, content, anchor, requires_response,
               quick_replies, status, read, ts
        FROM pings
        WHERE id = ?1
        ",
        params![id],
        ping_from_row,
    )
    .optional()
}

fn get_push_token(conn: &Connection, token: &str) -> rusqlite::Result<PushToken> {
    conn.query_row(
        "
        SELECT token, platform, created_ts, last_seen_ts
        FROM push_tokens
        WHERE token = ?1
        ",
        params![token],
        push_token_from_row,
    )
}

fn load_attachments_for_messages(
    conn: &Connection,
    messages: &mut [ChatMessage],
) -> rusqlite::Result<()> {
    for message in messages {
        message.attachments = message_attachments(conn, message.id)?
            .into_iter()
            .map(|stored| stored.blob)
            .collect();
    }
    Ok(())
}

fn message_attachments(conn: &Connection, message_id: u64) -> rusqlite::Result<Vec<StoredBlob>> {
    let mut stmt = conn.prepare(
        "
        SELECT b.id, b.name, b.mime, b.size, b.path, b.created_ts
        FROM message_attachments ma
        JOIN blobs b ON b.id = ma.blob_id
        WHERE ma.message_id = ?1
        ORDER BY ma.position ASC
        ",
    )?;
    let rows = stmt.query_map(params![message_id], stored_blob_from_row)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
}

fn get_stored_blob_optional(conn: &Connection, id: &str) -> rusqlite::Result<Option<StoredBlob>> {
    conn.query_row(
        "
        SELECT id, name, mime, size, path, created_ts
        FROM blobs
        WHERE id = ?1
        ",
        params![id],
        stored_blob_from_row,
    )
    .optional()
}

fn get_stored_blob(conn: &Connection, id: &str) -> rusqlite::Result<StoredBlob> {
    conn.query_row(
        "
        SELECT id, name, mime, size, path, created_ts
        FROM blobs
        WHERE id = ?1
        ",
        params![id],
        stored_blob_from_row,
    )
}

fn stored_blob_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredBlob> {
    let created_ts: String = row.get(5)?;
    Ok(StoredBlob {
        blob: Blob {
            id: row.get(0)?,
            name: row.get(1)?,
            mime: row.get(2)?,
            size: blob_size_from_row(row, 3)?,
        },
        path: PathBuf::from(row.get::<_, String>(4)?),
        created_ts: parse_ts(&created_ts)?,
    })
}

fn blob_size_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let size: i64 = row.get(index)?;
    u64::try_from(size).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
    })
}

fn chat_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let author: String = row.get(1)?;
    let ts: String = row.get(4)?;
    let tool_calls: String = row.get(5)?;
    Ok(ChatMessage {
        id: row.get(0)?,
        author: author_from_str(&author)?,
        body: row.get(2)?,
        r#ref: row.get(3)?,
        ts: parse_ts(&ts)?,
        attachments: Vec::new(),
        tool_calls: serde_json::from_str(&tool_calls).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(error))
        })?,
    })
}

fn side_chat_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let author: String = row.get(1)?;
    let ts: String = row.get(3)?;
    Ok(ChatMessage {
        id: row.get(0)?,
        author: author_from_str(&author)?,
        body: row.get(2)?,
        r#ref: None,
        ts: parse_ts(&ts)?,
        attachments: Vec::new(),
        tool_calls: Vec::new(),
    })
}

fn ping_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Ping> {
    let replies: String = row.get(6)?;
    let status: String = row.get(7)?;
    let ts: String = row.get(9)?;
    Ok(Ping {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        content: row.get(3)?,
        anchor: row.get(4)?,
        requires_response: row.get::<_, i64>(5)? != 0,
        quick_replies: serde_json::from_str(&replies).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
        })?,
        status: status_from_str(&status)?,
        read: row.get::<_, i64>(8)? != 0,
        ts: parse_ts(&ts)?,
    })
}

fn push_token_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PushToken> {
    let platform: String = row.get(1)?;
    let created_ts: String = row.get(2)?;
    let last_seen_ts: String = row.get(3)?;
    Ok(PushToken {
        token: row.get(0)?,
        platform: push_platform_from_str(&platform)?,
        created_ts: parse_ts(&created_ts)?,
        last_seen_ts: parse_ts(&last_seen_ts)?,
    })
}

fn device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    let created_ts: String = row.get(2)?;
    let last_seen_ts: String = row.get(3)?;
    let revoked_ts: Option<String> = row.get(4)?;
    Ok(Device {
        device_label: row.get(0)?,
        node_id: row.get(1)?,
        created_ts: parse_ts(&created_ts)?,
        last_seen_ts: parse_ts(&last_seen_ts)?,
        revoked_ts: revoked_ts.as_deref().map(parse_ts).transpose()?,
    })
}

fn validate_device_label(device_label: &str) -> anyhow::Result<()> {
    if device_label.trim().is_empty() {
        anyhow::bail!("device label must not be empty");
    }
    if device_label.chars().count() > 128 {
        anyhow::bail!("device label must be at most 128 characters");
    }
    Ok(())
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn is_unique_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn parse_ts(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|ts| ts.with_timezone(&Utc))
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error)))
}

fn author_to_str(author: ChatAuthor) -> &'static str {
    match author {
        ChatAuthor::Owner => "owner",
        ChatAuthor::Agent => "agent",
    }
}

fn author_from_str(value: &str) -> rusqlite::Result<ChatAuthor> {
    match value {
        "owner" => Ok(ChatAuthor::Owner),
        "agent" => Ok(ChatAuthor::Agent),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn status_from_str(value: &str) -> rusqlite::Result<PingStatus> {
    match value {
        "open" => Ok(PingStatus::Open),
        "done" => Ok(PingStatus::Done),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn push_platform_to_str(platform: PushPlatform) -> &'static str {
    match platform {
        PushPlatform::Android => "android",
        PushPlatform::Web => "web",
        PushPlatform::Ios => "ios",
    }
}

fn push_platform_from_str(value: &str) -> rusqlite::Result<PushPlatform> {
    match value {
        "android" => Ok(PushPlatform::Android),
        "web" => Ok(PushPlatform::Web),
        "ios" => Ok(PushPlatform::Ios),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            1,
            Type::Text,
            format!("unknown push platform: {other}").into(),
        )),
    }
}

pub fn monitor_process_info(record: &MonitorRecord) -> ProcessInfo {
    ProcessInfo {
        id: record.id.clone(),
        kind: ProcessKind::Monitor,
        label: short_monitor_label(&record.label),
        agent: None,
        model: None,
        state: if record.cancelled_ts.is_some() {
            ProcessState::Cancelled
        } else {
            ProcessState::Running
        },
        started_ts: record.created_ts,
        last_event_ts: record.last_event_ts,
        // The client's monitor rows have no dedicated cmd/interval fields;
        // the summary carries them (see app/PROTOCOL.md v1.4 notes).
        summary: Some(match &record.summary {
            Some(summary) => format!("{} · every {}s — {summary}", record.cmd, record.every_secs),
            None => format!("{} · every {}s", record.cmd, record.every_secs),
        }),
    }
}

fn validate_monitor_record(record: &MonitorRecord) -> anyhow::Result<()> {
    if record.cmd.trim().is_empty() {
        anyhow::bail!("monitor cmd is required");
    }
    if record.label.trim().is_empty() {
        anyhow::bail!("monitor label is required");
    }
    if matches!(record.wake_on, MonitorWakeOn::Regex)
        && record.pattern.as_deref().is_none_or(str::is_empty)
    {
        anyhow::bail!("monitor pattern is required for regex wake_on");
    }
    Ok(())
}

fn get_monitor(conn: &Connection, monitor_id: &str) -> rusqlite::Result<MonitorRecord> {
    get_monitor_optional(conn, monitor_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

fn get_monitor_optional(
    conn: &Connection,
    monitor_id: &str,
) -> rusqlite::Result<Option<MonitorRecord>> {
    conn.query_row(
        "
        SELECT id, cmd, every_secs, wake_on, pattern, label, created_ts, last_event_ts,
               last_run_ts, last_output, summary, cancelled_ts
        FROM monitors
        WHERE id = ?1
        ",
        params![monitor_id],
        monitor_from_row,
    )
    .optional()
}

fn monitor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MonitorRecord> {
    let wake_on: String = row.get(3)?;
    let created_ts: String = row.get(6)?;
    let last_event_ts: String = row.get(7)?;
    let last_run_ts: Option<String> = row.get(8)?;
    let cancelled_ts: Option<String> = row.get(11)?;
    Ok(MonitorRecord {
        id: row.get(0)?,
        cmd: row.get(1)?,
        every_secs: u64_from_row(row, 2)?,
        wake_on: monitor_wake_on_from_str(&wake_on)?,
        pattern: row.get(4)?,
        label: row.get(5)?,
        created_ts: parse_ts(&created_ts)?,
        last_event_ts: parse_ts(&last_event_ts)?,
        last_run_ts: last_run_ts.as_deref().map(parse_ts).transpose()?,
        last_output: row.get(9)?,
        summary: row.get(10)?,
        cancelled_ts: cancelled_ts.as_deref().map(parse_ts).transpose()?,
    })
}

fn u64_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
    })
}

fn monitor_wake_on_to_str(wake_on: MonitorWakeOn) -> &'static str {
    match wake_on {
        MonitorWakeOn::Changed => "changed",
        MonitorWakeOn::ExitZero => "exit_zero",
        MonitorWakeOn::ExitNonzero => "exit_nonzero",
        MonitorWakeOn::Regex => "regex",
    }
}

fn monitor_wake_on_from_str(value: &str) -> rusqlite::Result<MonitorWakeOn> {
    match value {
        "changed" => Ok(MonitorWakeOn::Changed),
        "exit_zero" => Ok(MonitorWakeOn::ExitZero),
        "exit_nonzero" => Ok(MonitorWakeOn::ExitNonzero),
        "regex" => Ok(MonitorWakeOn::Regex),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn subagent_process_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessRecord> {
    let agent: String = row.get(1)?;
    let handle_agent: String = row.get(4)?;
    let status: String = row.get(8)?;
    let events: String = row.get(9)?;
    let started_ts: String = row.get(10)?;
    let last_event_ts: String = row.get(11)?;
    Ok(ProcessRecord::restored(
        row.get(0)?,
        agent_kind_from_str(&agent)?,
        row.get(2)?,
        SessionHandle {
            id: row.get(3)?,
            agent: agent_kind_from_str(&handle_agent)?,
        },
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        process_status_from_str(&status)?,
        serde_json::from_str::<Vec<SubagentEvent>>(&events).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
        })?,
        parse_ts(&started_ts)?,
        parse_ts(&last_event_ts)?,
    ))
}

fn agent_kind_to_str(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
    }
}

fn agent_kind_from_str(value: &str) -> rusqlite::Result<AgentKind> {
    match value {
        "claude" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn process_status_to_str(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Running => "running",
        ProcessStatus::Done => "done",
        ProcessStatus::Failed => "failed",
        ProcessStatus::Interrupted => "interrupted",
        ProcessStatus::Abandoned => "abandoned",
    }
}

fn process_status_from_str(value: &str) -> rusqlite::Result<ProcessStatus> {
    match value {
        "running" => Ok(ProcessStatus::Running),
        "done" => Ok(ProcessStatus::Done),
        "failed" => Ok(ProcessStatus::Failed),
        "interrupted" => Ok(ProcessStatus::Interrupted),
        "abandoned" => Ok(ProcessStatus::Abandoned),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn short_monitor_label(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 80;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owner_messages_are_idempotent_by_client_id() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let (first, inserted) = storage
            .append_owner_message("client-1", "hello", None, &[])
            .await
            .unwrap();
        let (second, duplicate_inserted) = storage
            .append_owner_message("client-1", "hello again", None, &[])
            .await
            .unwrap();

        assert!(inserted);
        assert!(!duplicate_inserted);
        assert_eq!(first, second);
        assert_eq!(storage.all_chat().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn side_chat_transcripts_use_a_separate_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let owner = storage
            .append_side_chat_message("side:test", ChatAuthor::Owner, "question")
            .await
            .unwrap();
        let agent = storage
            .append_side_chat_message("side:test", ChatAuthor::Agent, "answer")
            .await
            .unwrap();
        assert_eq!(owner.id + 1, agent.id);
        assert!(storage.all_chat().await.unwrap().is_empty());

        let transcript = storage.side_chat_transcript("side:test").await.unwrap();
        assert_eq!(transcript, vec![owner, agent]);

        storage
            .delete_side_chat_transcript("side:test")
            .await
            .unwrap();
        assert!(
            storage
                .side_chat_transcript("side:test")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn resolving_a_ping_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap();
        let ping = storage
            .create_ping(
                "needs-reply",
                "Needs reply",
                "Needs reply",
                anchor.id,
                true,
                Vec::new(),
            )
            .await
            .unwrap();

        let first = storage.resolve_ping(ping.id).await.unwrap().unwrap();
        let second = storage.resolve_ping(ping.id).await.unwrap().unwrap();
        assert_eq!(first.status, PingStatus::Done);
        assert_eq!(second.status, PingStatus::Done);
    }

    #[tokio::test]
    async fn owner_reply_resolves_every_open_ping_for_its_anchor_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap();
        let first = storage
            .create_ping("first", "First", "First", anchor.id, true, Vec::new())
            .await
            .unwrap();
        let second = storage
            .create_ping("second", "Second", "Second", anchor.id, false, Vec::new())
            .await
            .unwrap();
        let other_anchor = storage
            .append_chat(ChatAuthor::Agent, "other anchor", None)
            .await
            .unwrap();
        let other = storage
            .create_ping("other", "Other", "Other", other_anchor.id, true, Vec::new())
            .await
            .unwrap();

        let resolved = storage
            .resolve_open_pings_for_anchor(anchor.id)
            .await
            .unwrap();
        assert_eq!(
            resolved.iter().map(|ping| ping.id).collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
        assert!(resolved.iter().all(|ping| ping.status == PingStatus::Done));
        assert!(
            storage
                .resolve_open_pings_for_anchor(anchor.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            storage.ping(other.id).await.unwrap().unwrap().status,
            PingStatus::Open
        );
    }

    #[tokio::test]
    async fn chat_tool_summaries_are_persisted_with_chat_messages() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let tool_calls = vec![
            ToolCallSummary {
                name: "shell_run".to_string(),
                ok: true,
            },
            ToolCallSummary {
                name: "subagents_spawn".to_string(),
                ok: false,
            },
        ];

        let message = storage
            .append_chat_with_tool_calls(ChatAuthor::Agent, "used tools", None, tool_calls.clone())
            .await
            .unwrap();
        let replay = storage.replay_messages(None).await.unwrap();

        assert_eq!(message.tool_calls, tool_calls);
        assert_eq!(replay[0].tool_calls, tool_calls);
    }

    #[tokio::test]
    async fn hello_snapshot_derives_latest_id_from_replayed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage
            .append_chat(ChatAuthor::Agent, "one", None)
            .await
            .unwrap();
        storage
            .append_chat(ChatAuthor::Agent, "two", None)
            .await
            .unwrap();

        let snapshot = storage.hello_snapshot(Some(1)).await.unwrap();
        assert_eq!(snapshot.latest_msg_id, 2);
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.messages[0].body, "two");

        let empty = storage.hello_snapshot(Some(2)).await.unwrap();
        assert_eq!(empty.latest_msg_id, 2);
        assert!(empty.messages.is_empty());

        let stale = storage.hello_snapshot(Some(99_999)).await.unwrap();
        assert_eq!(stale.latest_msg_id, 2);
        assert_eq!(
            stale
                .messages
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
    }

    #[tokio::test]
    async fn monitors_are_persisted_and_project_to_process_info() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let monitor = storage
            .create_monitor(
                "printf ready",
                5,
                MonitorWakeOn::Changed,
                None,
                "watch ready",
            )
            .await
            .unwrap();
        assert_eq!(monitor.every_secs, 30);
        assert_eq!(storage.active_monitors().await.unwrap().len(), 1);

        let updated = storage
            .record_monitor_tick(
                &monitor.id,
                "ready".to_string(),
                "exit 0: ready".to_string(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.summary.as_deref(), Some("exit 0: ready"));

        let snapshot = storage.monitor_snapshot().await.unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].kind, ProcessKind::Monitor);
        assert_eq!(snapshot[0].state, ProcessState::Running);
        assert_eq!(
            snapshot[0].summary.as_deref(),
            Some("printf ready · every 30s — exit 0: ready")
        );

        let cancelled = storage.cancel_monitor(&monitor.id).await.unwrap().unwrap();
        assert!(cancelled.cancelled_ts.is_some());
        let snapshot = storage.monitor_snapshot().await.unwrap();
        assert_eq!(snapshot[0].state, ProcessState::Cancelled);
    }

    #[tokio::test]
    async fn running_subagent_processes_restore_as_abandoned_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let now = Utc::now();
        let record = ProcessRecord::restored(
            "proc-running".to_string(),
            AgentKind::Codex,
            Some("gpt-test".to_string()),
            SessionHandle {
                id: "handle-1".to_string(),
                agent: AgentKind::Codex,
            },
            "fix it".to_string(),
            "/tmp".to_string(),
            Some("external-1".to_string()),
            ProcessStatus::Running,
            vec![SubagentEvent::Started {
                external_id: "external-1".to_string(),
            }],
            now,
            now,
        );
        storage.upsert_subagent_process(&record).await.unwrap();

        let restored = storage
            .restore_subagent_processes_after_restart()
            .await
            .unwrap();
        assert_eq!(restored.abandoned, vec!["proc-running".to_string()]);
        assert_eq!(restored.records.len(), 1);
        assert_eq!(restored.records[0].status, ProcessStatus::Abandoned);

        drop(storage);
        let reopened = Storage::open(dir.path()).await.unwrap();
        let restored_again = reopened
            .restore_subagent_processes_after_restart()
            .await
            .unwrap();
        assert!(restored_again.abandoned.is_empty());
        assert_eq!(restored_again.records[0].status, ProcessStatus::Abandoned);
    }

    #[tokio::test]
    async fn ping_snapshot_includes_done_pings() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap()
            .id;
        let ping = storage
            .create_ping(
                "release-decision",
                "Choose whether to release",
                "question",
                anchor,
                true,
                vec![QuickReply {
                    value: "yes".to_string(),
                    label: "Yes".to_string(),
                }],
            )
            .await
            .unwrap();

        assert!(!ping.read);
        let done = storage.resolve_ping(ping.id).await.unwrap().unwrap();
        let snapshot = storage.ping_snapshot().await.unwrap();

        assert_eq!(done.status, PingStatus::Done);
        assert!(!done.read);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].status, PingStatus::Done);
    }

    #[tokio::test]
    async fn mark_ping_read_is_idempotent_and_preserved_when_done() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap()
            .id;
        let ping = storage
            .create_ping("question", "Question", "question", anchor, true, Vec::new())
            .await
            .unwrap();

        assert!(!ping.read);
        let read = storage.mark_ping_read(ping.id).await.unwrap().unwrap();
        let read_again = storage.mark_ping_read(ping.id).await.unwrap().unwrap();
        let done = storage.resolve_ping(ping.id).await.unwrap().unwrap();

        assert!(read.read);
        assert!(read_again.read);
        assert!(done.read);
        assert_eq!(done.status, PingStatus::Done);
        assert!(storage.mark_ping_read(99_999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn push_token_registration_upserts_and_unregisters() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let created = storage
            .register_push_token(PushPlatform::Android, "token-1")
            .await
            .unwrap();
        let refreshed = storage
            .register_push_token(PushPlatform::Web, "token-1")
            .await
            .unwrap();

        assert_eq!(refreshed.token, "token-1");
        assert_eq!(refreshed.platform, PushPlatform::Web);
        assert_eq!(refreshed.created_ts, created.created_ts);
        assert!(refreshed.last_seen_ts >= created.last_seen_ts);
        assert_eq!(storage.push_tokens().await.unwrap(), vec![refreshed]);
        assert!(storage.unregister_push_token("token-1").await.unwrap());
        assert!(!storage.unregister_push_token("token-1").await.unwrap());
        assert!(storage.push_tokens().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn push_token_table_is_additive_for_an_existing_database() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE chat_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    author TEXT NOT NULL,
                    body TEXT NOT NULL,
                    ref INTEGER NULL,
                    ts TEXT NOT NULL,
                    tool_calls TEXT NOT NULL DEFAULT '[]'
                );
                INSERT INTO chat_messages (author, body, ref, ts)
                VALUES ('agent', 'existing row', NULL, '2026-07-10T12:00:00Z');
                ",
            )
            .unwrap();
        }

        let storage = Storage::open(dir.path()).await.unwrap();
        assert_eq!(storage.all_chat().await.unwrap().len(), 1);
        storage
            .register_push_token(PushPlatform::Ios, "token-live")
            .await
            .unwrap();
        drop(storage);

        let reopened = Storage::open(dir.path()).await.unwrap();
        assert_eq!(reopened.all_chat().await.unwrap().len(), 1);
        assert_eq!(reopened.push_tokens().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn device_tokens_are_pinned_revocable_and_additive() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE chat_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    author TEXT NOT NULL,
                    body TEXT NOT NULL,
                    ref INTEGER NULL,
                    ts TEXT NOT NULL,
                    tool_calls TEXT NOT NULL DEFAULT '[]'
                );
                INSERT INTO chat_messages (author, body, ref, ts)
                VALUES ('agent', 'existing row', NULL, '2026-07-10T12:00:00Z');
                ",
            )
            .unwrap();
        }

        let storage = Storage::open(dir.path()).await.unwrap();
        let token = storage
            .issue_device_token("Owner phone", "node-a")
            .await
            .unwrap();
        assert_eq!(token.len(), 64);
        storage
            .authenticate_device_token(&token, Some("node-a"))
            .await
            .unwrap();
        assert!(
            storage
                .authenticate_device_token(&token, Some("node-b"))
                .await
                .is_err()
        );
        assert_eq!(storage.list_devices().await.unwrap().len(), 1);
        assert_eq!(storage.revoke_device("Owner phone").await.unwrap(), 1);
        assert!(
            storage
                .authenticate_device_token(&token, Some("node-a"))
                .await
                .is_err()
        );
        drop(storage);

        let reopened = Storage::open(dir.path()).await.unwrap();
        assert_eq!(reopened.all_chat().await.unwrap().len(), 1);
        let devices = reopened.list_devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_label, "Owner phone");
        assert!(devices[0].revoked_ts.is_some());
    }

    #[tokio::test]
    async fn pairing_codes_are_long_single_use_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let code = storage
            .mint_pairing_code("Owner phone", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(code.len(), 64);
        assert_eq!(
            storage.redeem_pairing_code(&code).await.unwrap(),
            "Owner phone"
        );
        assert!(storage.redeem_pairing_code(&code).await.is_err());

        let expired = storage
            .mint_pairing_code("Old phone", Duration::ZERO)
            .await
            .unwrap();
        assert!(storage.redeem_pairing_code(&expired).await.is_err());
    }

    #[tokio::test]
    async fn pairing_code_redemption_attempts_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let code = storage
            .mint_pairing_code("Rate limited phone", Duration::from_secs(60))
            .await
            .unwrap();

        for attempt in 0..MAX_PAIRING_REDEMPTIONS_PER_MINUTE {
            assert!(
                storage
                    .redeem_pairing_code(&format!("unknown-{attempt}"))
                    .await
                    .is_err()
            );
        }
        let error = storage.redeem_pairing_code(&code).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "too many pairing-code redemption attempts"
        );
    }

    #[tokio::test]
    async fn open_migrates_legacy_ping_rows() {
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("hirsel.sqlite")).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE chat_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    author TEXT NOT NULL,
                    body TEXT NOT NULL,
                    ref INTEGER NULL,
                    ts TEXT NOT NULL
                );
                CREATE TABLE inbox_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    content TEXT NOT NULL,
                    anchor INTEGER NOT NULL REFERENCES chat_messages(id),
                    requires_response INTEGER NOT NULL,
                    quick_replies TEXT NOT NULL,
                    status TEXT NOT NULL,
                    ts TEXT NOT NULL
                );
                INSERT INTO chat_messages (author, body, ref, ts)
                VALUES ('agent', 'anchor', NULL, '2026-07-08T12:00:00Z');
                INSERT INTO inbox_items (
                    content,
                    anchor,
                    requires_response,
                    quick_replies,
                    status,
                    ts
                )
                VALUES ('legacy question', 1, 1, '[]', 'open', '2026-07-08T12:00:00Z');
                ",
            )
            .unwrap();
        }

        let storage = Storage::open(dir.path()).await.unwrap();
        let legacy_chat = storage.all_chat().await.unwrap();
        assert_eq!(legacy_chat.len(), 1);
        assert!(legacy_chat[0].tool_calls.is_empty());

        let legacy = storage.all_pings().await.unwrap();
        assert_eq!(legacy.len(), 1);
        assert!(!legacy[0].read);
        assert_eq!(legacy[0].name, "legacy-question");
        assert_eq!(legacy[0].description, "legacy question");

        let read = storage.mark_ping_read(legacy[0].id).await.unwrap().unwrap();
        assert!(read.read);
        drop(storage);

        let reopened = Storage::open(dir.path()).await.unwrap();
        let persisted = reopened.all_pings().await.unwrap();
        assert_eq!(persisted.len(), 1);
        assert!(persisted[0].read);
    }

    #[tokio::test]
    async fn blobs_are_stored_as_raw_files_and_idempotent_by_client_id() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let first = storage
            .store_blob(
                "upload-1",
                "note.txt",
                "text/plain",
                b"first bytes".to_vec(),
            )
            .await
            .unwrap();
        let duplicate = storage
            .store_blob(
                "upload-1",
                "other.txt",
                "text/plain",
                b"other bytes".to_vec(),
            )
            .await
            .unwrap();

        assert_eq!(first, duplicate);
        assert_eq!(tokio::fs::read(&first.path).await.unwrap(), b"first bytes");
        assert_eq!(
            first.path.file_name().and_then(|name| name.to_str()),
            Some(first.blob.id.as_str())
        );
        assert!(first.path.is_absolute());
    }

    #[tokio::test]
    async fn owner_message_attachments_are_joined_and_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let text = storage
            .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
            .await
            .unwrap();
        let image = storage
            .store_blob(
                "image-upload",
                "tiny.png",
                "image/png",
                vec![137, 80, 78, 71],
            )
            .await
            .unwrap();
        let attachment_ids = vec![text.blob.id.clone(), image.blob.id.clone()];

        let (message, inserted) = storage
            .append_owner_message("client-1", "see attached", None, &attachment_ids)
            .await
            .unwrap();
        let replay = storage.replay_messages(None).await.unwrap();
        let stored_blobs = storage.blobs_for_message(message.id).await.unwrap();

        assert!(inserted);
        assert_eq!(
            message.attachments,
            vec![text.blob.clone(), image.blob.clone()]
        );
        assert_eq!(replay[0].attachments, message.attachments);
        assert_eq!(
            stored_blobs
                .iter()
                .map(|stored| stored.path.as_path())
                .collect::<Vec<_>>(),
            vec![text.path.as_path(), image.path.as_path()]
        );
    }

    #[tokio::test]
    async fn delete_chat_message_removes_client_id_and_attachment_joins() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let blob = storage
            .store_blob("text-upload", "note.txt", "text/plain", b"hello".to_vec())
            .await
            .unwrap();
        let attachment_ids = vec![blob.blob.id.clone()];
        let (message, inserted) = storage
            .append_owner_message("client-1", "queued", None, &attachment_ids)
            .await
            .unwrap();

        assert!(inserted);
        assert_eq!(
            storage.message_id_for_client_id("client-1").await.unwrap(),
            Some(message.id)
        );
        assert!(
            !storage
                .blobs_for_message(message.id)
                .await
                .unwrap()
                .is_empty()
        );

        assert!(storage.delete_chat_message(message.id).await.unwrap());
        assert_eq!(storage.all_chat().await.unwrap(), Vec::<ChatMessage>::new());
        assert_eq!(
            storage.message_id_for_client_id("client-1").await.unwrap(),
            None
        );
        assert!(
            storage
                .blobs_for_message(message.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(storage.blob(&blob.blob.id).await.unwrap().is_some());
        assert!(!storage.delete_chat_message(message.id).await.unwrap());
    }

    #[tokio::test]
    async fn owner_message_rejects_unknown_attachment_ids() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let error = storage
            .append_owner_message(
                "client-1",
                "missing attachment",
                None,
                &[String::from("missing-blob")],
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("unknown blob id: missing-blob"));
        assert!(storage.all_chat().await.unwrap().is_empty());
    }
}
