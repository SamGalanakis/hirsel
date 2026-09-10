//! Main chat transcript: messages, owner submissions, replay.

use super::Storage;
use super::blobs::message_attachments;
use super::common::{collect_rows, parse_ts};
use hirsel_proto::ChatAuthor;
use hirsel_proto::ChatMessage;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Type;

impl Storage {
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
            "DELETE FROM thread_requests WHERE client_id IN (SELECT client_id FROM client_messages WHERE msg_id = ?1)",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM message_attachments WHERE message_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM client_messages WHERE msg_id = ?1", params![id])?;
        let changed = tx.execute("DELETE FROM chat_messages WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub async fn hello_snapshot(&self) -> anyhow::Result<HelloSnapshot> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let history_id =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
                r.get(0)
            })?;
        let threads = super::threads::snapshot(&tx)?;
        tx.commit()?;
        Ok(HelloSnapshot {
            history_id,
            threads,
        })
    }

    #[cfg(test)]
    pub(crate) async fn force_hello_snapshot_error(&self) {
        self.conn
            .lock()
            .await
            .execute(
                "ALTER TABLE threads RENAME COLUMN title TO broken_title",
                [],
            )
            .expect("break hello snapshot schema for test");
    }

    pub async fn all_chat(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "
            SELECT id, author, body, ref, ts, tool_calls, thread_id, mentions
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
            SELECT id, author, body, ref, ts, tool_calls, thread_id, mentions
            FROM (
                SELECT id, author, body, ref, ts, tool_calls, thread_id, mentions
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
    pub history_id: String,
    pub threads: Vec<hirsel_proto::Thread>,
}

pub(super) fn get_chat_message(conn: &Connection, id: u64) -> rusqlite::Result<ChatMessage> {
    let mut message = conn.query_row(
        "
        SELECT id, author, body, ref, ts, tool_calls, thread_id, mentions
        FROM chat_messages
        WHERE id = ?1
        ",
        params![id],
        chat_message_from_row,
    )?;
    message.client_id = conn
        .query_row(
            "SELECT client_id FROM client_messages WHERE msg_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    message.artifact_ids = super::artifacts::message_artifacts(conn, id)?;
    message.attachments = message_attachments(conn, id)?;
    Ok(message)
}

pub(super) fn load_attachments_for_messages(
    conn: &Connection,
    messages: &mut [ChatMessage],
) -> rusqlite::Result<()> {
    for message in messages {
        message.client_id = conn
            .query_row(
                "SELECT client_id FROM client_messages WHERE msg_id=?1",
                [message.id],
                |r| r.get(0),
            )
            .optional()?;
        message.artifact_ids = super::artifacts::message_artifacts(conn, message.id)?;
        message.attachments = message_attachments(conn, message.id)?;
    }
    Ok(())
}

pub(super) fn chat_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    let author: String = row.get(1)?;
    let ts: String = row.get(4)?;
    let tool_calls: String = row.get(5)?;
    Ok(ChatMessage {
        artifact_ids: Vec::new(),
        client_id: None,
        thread_id: row.get(6)?,
        mentions: serde_json::from_str(&row.get::<_, String>(7)?)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(e)))?,
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
