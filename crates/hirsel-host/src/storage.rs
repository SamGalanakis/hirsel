use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use hirsel_proto::{Blob, ChatAuthor, ChatMessage, InboxItem, InboxStatus, QuickReply};
use rusqlite::{Connection, OptionalExtension, params, types::Type};
use tokio::sync::Mutex;
use uuid::Uuid;

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
                ts TEXT NOT NULL
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
                ts TEXT NOT NULL
            );
            ",
        )?;
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
                "SELECT id, author, body, ref, ts FROM chat_messages WHERE id > ?1 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![last_seen_msg_id], chat_message_from_row)?;
            collect_rows(rows)?
        } else {
            let mut stmt = conn.prepare(
                "
                SELECT id, author, body, ref, ts
                FROM (
                    SELECT id, author, body, ref, ts FROM chat_messages ORDER BY id DESC LIMIT 200
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
        let mut stmt =
            conn.prepare("SELECT id, author, body, ref, ts FROM chat_messages ORDER BY id ASC")?;
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
            INSERT INTO inbox_items (content, anchor, requires_response, quick_replies, status, ts)
            VALUES (?1, ?2, ?3, ?4, 'open', ?5)
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

    pub async fn inbox_snapshot(&self) -> anyhow::Result<Vec<InboxItem>> {
        let conn = self.conn.lock().await;
        let mut items = Vec::new();
        {
            let mut stmt = conn.prepare(
                "
                SELECT id, content, anchor, requires_response, quick_replies, status, ts
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
                SELECT id, content, anchor, requires_response, quick_replies, status, ts
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
            SELECT id, content, anchor, requires_response, quick_replies, status, ts
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

fn get_chat_message(conn: &Connection, id: u64) -> rusqlite::Result<ChatMessage> {
    let mut message = conn.query_row(
        "SELECT id, author, body, ref, ts FROM chat_messages WHERE id = ?1",
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
    conn.query_row(
        "
        SELECT id, content, anchor, requires_response, quick_replies, status, ts
        FROM inbox_items
        WHERE id = ?1
        ",
        params![id],
        inbox_item_from_row,
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
    Ok(ChatMessage {
        id: row.get(0)?,
        author: author_from_str(&author)?,
        body: row.get(2)?,
        r#ref: row.get(3)?,
        ts: parse_ts(&ts)?,
        attachments: Vec::new(),
    })
}

fn inbox_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItem> {
    let replies: String = row.get(4)?;
    let status: String = row.get(5)?;
    let ts: String = row.get(6)?;
    Ok(InboxItem {
        id: row.get(0)?,
        content: row.get(1)?,
        anchor: row.get(2)?,
        requires_response: row.get::<_, i64>(3)? != 0,
        quick_replies: serde_json::from_str(&replies).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error))
        })?,
        status: status_from_str(&status)?,
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

        let archived = storage.archive_inbox_item(item.id).await.unwrap().unwrap();
        let snapshot = storage.inbox_snapshot().await.unwrap();

        assert_eq!(archived.status, InboxStatus::Archived);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].status, InboxStatus::Archived);
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
