//! Side-chat transcripts (process-local, cleared on restart).

use super::Storage;
use super::chat::{author_from_str, author_to_str};
use super::common::{collect_rows, parse_ts};
use chrono::Utc;
use hirsel_proto::ChatAuthor;
use hirsel_proto::ChatMessage;
use rusqlite::params;

impl Storage {
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

#[cfg(test)]
mod tests;
