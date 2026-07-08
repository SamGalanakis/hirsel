use std::{path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use hirsel_proto::{ChatAuthor, ChatMessage, InboxItem, InboxStatus, QuickReply};
use rusqlite::{Connection, OptionalExtension, params, types::Type};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    pub async fn open(data_dir: &Path) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(data_dir).await?;
        let db_path = data_dir.join("hirsel.sqlite");
        let conn = Connection::open(db_path)?;
        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
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
        })
    }

    pub async fn append_owner_message(
        &self,
        client_id: &str,
        body: impl Into<String>,
        anchor: Option<u64>,
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
        tx.execute(
            "INSERT INTO chat_messages (author, body, ref, ts) VALUES ('owner', ?1, ?2, ?3)",
            params![body, anchor, ts.to_rfc3339()],
        )?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO client_messages (client_id, msg_id) VALUES (?1, ?2)",
            params![client_id, id],
        )?;
        tx.commit()?;
        Ok((
            ChatMessage {
                id,
                author: ChatAuthor::Owner,
                body,
                r#ref: anchor,
                ts,
            },
            true,
        ))
    }

    pub async fn latest_msg_id(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock().await;
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM chat_messages",
            [],
            |row| row.get(0),
        )?)
    }

    pub async fn replay_messages(
        &self,
        last_seen_msg_id: Option<u64>,
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        if let Some(last_seen_msg_id) = last_seen_msg_id {
            let mut stmt = conn.prepare(
                "SELECT id, author, body, ref, ts FROM chat_messages WHERE id > ?1 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![last_seen_msg_id], chat_message_from_row)?;
            collect_rows(rows)
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
            collect_rows(rows)
        }
    }

    pub async fn all_chat(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT id, author, body, ref, ts FROM chat_messages ORDER BY id ASC")?;
        let rows = stmt.query_map([], chat_message_from_row)?;
        collect_rows(rows)
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

    pub async fn reset(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "
            DELETE FROM client_messages;
            DELETE FROM inbox_items;
            DELETE FROM chat_messages;
            DELETE FROM sqlite_sequence WHERE name IN ('chat_messages', 'inbox_items');
            ",
        )?;
        Ok(())
    }
}

fn collect_rows<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> anyhow::Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn get_chat_message(conn: &Connection, id: u64) -> rusqlite::Result<ChatMessage> {
    conn.query_row(
        "SELECT id, author, body, ref, ts FROM chat_messages WHERE id = ?1",
        params![id],
        chat_message_from_row,
    )
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

fn chat_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let author: String = row.get(1)?;
    let ts: String = row.get(4)?;
    Ok(ChatMessage {
        id: row.get(0)?,
        author: author_from_str(&author)?,
        body: row.get(2)?,
        r#ref: row.get(3)?,
        ts: parse_ts(&ts)?,
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
            .append_owner_message("client-1", "hello", None)
            .await
            .unwrap();
        let (second, duplicate_inserted) = storage
            .append_owner_message("client-1", "hello again", None)
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
}
