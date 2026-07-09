use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use hirsel_drivers::{AgentKind, SessionHandle, SubagentEvent};
use hirsel_proto::{
    Blob, ChatAuthor, ChatMessage, InboxItem, InboxStatus, ProcessInfo, ProcessKind, ProcessState,
    QuickReply, ToolCallSummary,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlob {
    pub blob: Blob,
    pub path: PathBuf,
    pub created_ts: DateTime<Utc>,
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
            CREATE TABLE IF NOT EXISTS inbox_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                anchor INTEGER NOT NULL REFERENCES chat_messages(id),
                requires_response INTEGER NOT NULL,
                quick_replies TEXT NOT NULL,
                status TEXT NOT NULL,
                read INTEGER NOT NULL DEFAULT 0,
                ts TEXT NOT NULL
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
            ",
        )?;
        ensure_inbox_read_column(&conn)?;
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
        load_attachments_for_messages(&conn, &mut messages)?;
        Ok(messages)
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

    pub async fn create_inbox_item(
        &self,
        content: impl Into<String>,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<InboxItem> {
        let content = content.into();
        let ts = Utc::now();
        let encoded_replies = serde_json::to_string(&quick_replies)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO inbox_items (
                content,
                anchor,
                requires_response,
                quick_replies,
                status,
                read,
                ts
            )
            VALUES (?1, ?2, ?3, ?4, 'open', 0, ?5)
            ",
            params![
                content,
                anchor,
                requires_response,
                encoded_replies,
                ts.to_rfc3339()
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(InboxItem {
            id,
            content,
            anchor,
            requires_response,
            quick_replies,
            status: InboxStatus::Open,
            read: false,
            ts,
        })
    }

    pub async fn archive_inbox_item(&self, item_id: u64) -> anyhow::Result<Option<InboxItem>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE inbox_items SET status = 'archived' WHERE id = ?1",
            params![item_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_inbox_item(&conn, item_id)?))
    }

    pub async fn mark_read(&self, item_id: u64) -> anyhow::Result<Option<InboxItem>> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE inbox_items SET read = 1 WHERE id = ?1 AND read = 0",
            params![item_id],
        )?;
        get_inbox_item_optional(&conn, item_id).map_err(Into::into)
    }

    pub async fn inbox_snapshot(&self) -> anyhow::Result<Vec<InboxItem>> {
        let conn = self.conn.lock().await;
        let mut items = Vec::new();
        {
            let mut stmt = conn.prepare(
                "
                SELECT id, content, anchor, requires_response, quick_replies, status, read, ts
                FROM inbox_items
                WHERE status = 'open'
                ORDER BY id ASC
                ",
            )?;
            let rows = stmt.query_map([], inbox_item_from_row)?;
            items.extend(collect_rows(rows)?);
        }
        {
            let mut stmt = conn.prepare(
                "
                SELECT id, content, anchor, requires_response, quick_replies, status, read, ts
                FROM inbox_items
                WHERE status = 'archived'
                ORDER BY id DESC
                LIMIT 20
                ",
            )?;
            let rows = stmt.query_map([], inbox_item_from_row)?;
            let mut archived = collect_rows(rows)?;
            archived.reverse();
            items.extend(archived);
        }
        Ok(items)
    }

    pub async fn all_inbox(&self) -> anyhow::Result<Vec<InboxItem>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, content, anchor, requires_response, quick_replies, status, read, ts
            FROM inbox_items
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map([], inbox_item_from_row)?;
        collect_rows(rows)
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
                DELETE FROM inbox_items;
                DELETE FROM monitors;
                DELETE FROM subagent_processes;
                DELETE FROM chat_messages;
                DELETE FROM sqlite_sequence WHERE name IN ('chat_messages', 'inbox_items');
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

fn ensure_inbox_read_column(conn: &Connection) -> rusqlite::Result<()> {
    if inbox_items_has_read_column(conn)? {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE inbox_items ADD COLUMN read INTEGER NOT NULL DEFAULT 0",
        [],
    )?;
    Ok(())
}

fn inbox_items_has_read_column(conn: &Connection) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(inbox_items)")?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "read" {
            return Ok(true);
        }
    }
    Ok(false)
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

fn get_inbox_item(conn: &Connection, id: u64) -> rusqlite::Result<InboxItem> {
    get_inbox_item_optional(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

fn get_inbox_item_optional(conn: &Connection, id: u64) -> rusqlite::Result<Option<InboxItem>> {
    conn.query_row(
        "
        SELECT id, content, anchor, requires_response, quick_replies, status, read, ts
        FROM inbox_items
        WHERE id = ?1
        ",
        params![id],
        inbox_item_from_row,
    )
    .optional()
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

fn inbox_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItem> {
    let replies: String = row.get(4)?;
    let status: String = row.get(5)?;
    let ts: String = row.get(7)?;
    Ok(InboxItem {
        id: row.get(0)?,
        content: row.get(1)?,
        anchor: row.get(2)?,
        requires_response: row.get::<_, i64>(3)? != 0,
        quick_replies: serde_json::from_str(&replies).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error))
        })?,
        status: status_from_str(&status)?,
        read: row.get::<_, i64>(6)? != 0,
        ts: parse_ts(&ts)?,
    })
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

fn status_from_str(value: &str) -> rusqlite::Result<InboxStatus> {
    match value {
        "open" => Ok(InboxStatus::Open),
        "archived" => Ok(InboxStatus::Archived),
        _ => Err(rusqlite::Error::InvalidQuery),
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
    async fn inbox_snapshot_includes_archived_items() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap()
            .id;
        let item = storage
            .create_inbox_item(
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

        assert!(!item.read);
        let archived = storage.archive_inbox_item(item.id).await.unwrap().unwrap();
        let snapshot = storage.inbox_snapshot().await.unwrap();

        assert_eq!(archived.status, InboxStatus::Archived);
        assert!(!archived.read);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].status, InboxStatus::Archived);
    }

    #[tokio::test]
    async fn mark_read_is_idempotent_and_preserved_by_archive() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        let anchor = storage
            .append_chat(ChatAuthor::Agent, "anchor", None)
            .await
            .unwrap()
            .id;
        let item = storage
            .create_inbox_item("question", anchor, true, Vec::new())
            .await
            .unwrap();

        assert!(!item.read);
        let read = storage.mark_read(item.id).await.unwrap().unwrap();
        let read_again = storage.mark_read(item.id).await.unwrap().unwrap();
        let archived = storage.archive_inbox_item(item.id).await.unwrap().unwrap();

        assert!(read.read);
        assert!(read_again.read);
        assert!(archived.read);
        assert_eq!(archived.status, InboxStatus::Archived);
        assert!(storage.mark_read(99_999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_adds_inbox_read_column_to_existing_databases() {
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

        let legacy = storage.all_inbox().await.unwrap();
        assert_eq!(legacy.len(), 1);
        assert!(!legacy[0].read);

        let read = storage.mark_read(legacy[0].id).await.unwrap().unwrap();
        assert!(read.read);
        drop(storage);

        let reopened = Storage::open(dir.path()).await.unwrap();
        let persisted = reopened.all_inbox().await.unwrap();
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
