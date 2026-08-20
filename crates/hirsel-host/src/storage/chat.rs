//! Main chat transcript: messages, owner submissions, replay.

use super::Storage;
use super::blobs::{message_attachments, validate_blob_ids};
use super::common::{collect_rows, parse_ts};
use super::events::ping_snapshot_from_conn;
use chrono::Utc;
use hirsel_proto::ChatAuthor;
use hirsel_proto::ChatMessage;
use hirsel_proto::Event;
use hirsel_proto::ToolCallSummary;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Type;

impl Storage {
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
        replay_messages_from_conn(&conn, last_seen_msg_id)
    }

    pub async fn fetch_messages(&self, before_id: u64, limit: u64) -> anyhow::Result<MessagePage> {
        let conn = self.conn.lock().await;
        fetch_messages_from_conn(&conn, before_id, limit)
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
        let events = ping_snapshot_from_conn(&tx)?;
        tx.commit()?;
        Ok(HelloSnapshot {
            latest_msg_id: db_max,
            messages,
            events,
        })
    }

    #[cfg(test)]
    pub(crate) async fn force_hello_snapshot_error(&self) {
        self.conn
            .lock()
            .await
            .execute(
                "ALTER TABLE chat_messages RENAME TO broken_chat_messages",
                [],
            )
            .expect("break hello snapshot schema for test");
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloSnapshot {
    pub latest_msg_id: u64,
    pub messages: Vec<ChatMessage>,
    pub events: Vec<Event>,
}

/// The newest-N conversation window every `hello_ok` carries, whatever cursor
/// the client presents.
pub(crate) const HELLO_REPLAY_WINDOW: u64 = 200;

/// Maximum number of rows returned by one just-in-time history request.
pub(crate) const FETCH_MESSAGES_LIMIT: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePage {
    pub messages: Vec<ChatMessage>,
    pub has_more: bool,
}

fn fetch_messages_from_conn(
    conn: &Connection,
    before_id: u64,
    requested_limit: u64,
) -> anyhow::Result<MessagePage> {
    let limit = requested_limit.clamp(1, FETCH_MESSAGES_LIMIT);
    // SQLite row ids are signed 64-bit. A protocol u64 above that range simply
    // means "beyond the newest row", not a binding error.
    let before_id = before_id.min(i64::MAX as u64);
    let mut stmt = conn.prepare(
        "
        SELECT id, author, body, ref, ts, tool_calls
        FROM (
            SELECT id, author, body, ref, ts, tool_calls
            FROM chat_messages
            WHERE id < ?1
            ORDER BY id DESC
            LIMIT ?2
        )
        ORDER BY id ASC
        ",
    )?;
    let rows = stmt.query_map(params![before_id, limit], chat_message_from_row)?;
    let mut messages = collect_rows(rows)?;
    load_attachments_for_messages(conn, &mut messages)?;
    let has_more = match messages.first() {
        Some(first) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE id < ?1)",
            params![first.id],
            |row| row.get(0),
        )?,
        None => false,
    };
    Ok(MessagePage { messages, has_more })
}

/// Replay for a `hello`.
///
/// `last_seen_msg_id` is an ATTENTION cursor (what this client had already
/// seen), never a history gate: a reload must not empty the conversation just
/// because the client had seen everything. So the replay is always at least the
/// newest [`HELLO_REPLAY_WINDOW`] rows, and grows beyond that only for a client
/// that is further behind than the window. A null cursor keeps its historical
/// meaning — exactly the window — because that is the same floor.
///
/// The client merge is range-authoritative (the snapshot owns everything from
/// its lowest id up, local history below it is preserved), so re-sending rows
/// the client already holds is an idempotent replace, not a duplicate.
fn replay_messages_from_conn(
    conn: &Connection,
    last_seen_msg_id: Option<u64>,
) -> anyhow::Result<Vec<ChatMessage>> {
    // Lowest id inside the newest-N window (0 when the table is empty).
    let window_floor: u64 = conn.query_row(
        "
        SELECT COALESCE(MIN(id), 0)
        FROM (SELECT id FROM chat_messages ORDER BY id DESC LIMIT ?1)
        ",
        params![HELLO_REPLAY_WINDOW],
        |row| row.get(0),
    )?;
    // Exclusive cursor: `> window_cursor` is exactly the window.
    let window_cursor = window_floor.saturating_sub(1);
    let cursor = match last_seen_msg_id {
        Some(seen) if seen < window_cursor => seen,
        _ => window_cursor,
    };

    let mut stmt = conn.prepare(
        "
        SELECT id, author, body, ref, ts, tool_calls
        FROM chat_messages
        WHERE id > ?1
        ORDER BY id ASC
        ",
    )?;
    let rows = stmt.query_map(params![cursor], chat_message_from_row)?;
    let mut messages = collect_rows(rows)?;
    load_attachments_for_messages(conn, &mut messages)?;
    Ok(messages)
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

pub(super) fn author_to_str(author: ChatAuthor) -> &'static str {
    match author {
        ChatAuthor::Owner => "owner",
        ChatAuthor::Agent => "agent",
    }
}

pub(super) fn author_from_str(value: &str) -> rusqlite::Result<ChatAuthor> {
    match value {
        "owner" => Ok(ChatAuthor::Owner),
        "agent" => Ok(ChatAuthor::Agent),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

#[cfg(test)]
mod tests;
