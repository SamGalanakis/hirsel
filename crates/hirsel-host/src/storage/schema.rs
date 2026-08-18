//! Schema creation and in-place migrations.

use super::Storage;
use super::events::event_kind_from_str;
use crate::text::option_key;
use hirsel_proto::EventKind;
use hirsel_proto::QuickReply;
use rusqlite::Connection;
use rusqlite::params;

impl Storage {
    pub async fn init(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
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
            CREATE TABLE IF NOT EXISTS taste_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id INTEGER NOT NULL,
                choice TEXT NULL,
                rule TEXT NOT NULL,
                ts TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )?;
        // Side-chat sessions are process-local and deliberately do not survive
        // a host restart, so any rows left by an unclean shutdown are orphaned.
        conn.execute("DELETE FROM side_chat_messages", [])?;
        migrate_pings_schema(&conn)?;
        ensure_chat_tool_calls_column(&conn)?;
        let integrity = conn
            .prepare("PRAGMA integrity_check")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if integrity.as_slice() != ["ok"] {
            tracing::error!(?integrity, "SQLite integrity check failed");
        }
        Ok(())
    }
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
            kind TEXT NOT NULL DEFAULT 'info',
            source_kind TEXT NOT NULL DEFAULT 'agent',
            source_ref TEXT NULL,
            name TEXT NOT NULL,
            description TEXT NOT NULL,
            content TEXT NOT NULL,
            ui TEXT NOT NULL DEFAULT '{}',
            anchor INTEGER NOT NULL REFERENCES chat_messages(id),
            requires_response INTEGER NOT NULL,
            quick_replies TEXT NOT NULL,
            status TEXT NOT NULL,
            read INTEGER NOT NULL DEFAULT 0,
            archived INTEGER NOT NULL DEFAULT 0,
            snoozed_until TEXT NULL,
            archived_at TEXT NULL,
            fork_sc TEXT NULL,
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
    if !table_has_column(conn, "pings", "archived")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !table_has_column(conn, "pings", "snoozed_until")? {
        conn.execute("ALTER TABLE pings ADD COLUMN snoozed_until TEXT NULL", [])?;
    }
    if !table_has_column(conn, "pings", "archived_at")? {
        conn.execute("ALTER TABLE pings ADD COLUMN archived_at TEXT NULL", [])?;
    }
    if !table_has_column(conn, "pings", "fork_sc")? {
        conn.execute("ALTER TABLE pings ADD COLUMN fork_sc TEXT NULL", [])?;
    }
    // Fork sessions are process-local and never survive a host restart.
    conn.execute("UPDATE pings SET fork_sc = NULL", [])?;
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
    if !table_has_column(conn, "pings", "kind")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN kind TEXT NOT NULL DEFAULT 'info'",
            [],
        )?;
        conn.execute(
            "UPDATE pings SET kind = CASE WHEN requires_response != 0 THEN 'judgment' ELSE 'info' END",
            [],
        )?;
    }
    if !table_has_column(conn, "pings", "source_kind")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'agent'",
            [],
        )?;
    }
    if !table_has_column(conn, "pings", "source_ref")? {
        conn.execute("ALTER TABLE pings ADD COLUMN source_ref TEXT NULL", [])?;
    }
    if !table_has_column(conn, "pings", "ui")? {
        conn.execute(
            "ALTER TABLE pings ADD COLUMN ui TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    conn.execute(
        "UPDATE pings SET status = 'done', archived = 1, archived_at = COALESCE(archived_at, ts) WHERE status = 'archived'",
        [],
    )?;
    conn.execute(
        "UPDATE pings SET archived_at = COALESCE(archived_at, ts) WHERE archived != 0",
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
    let missing_ui = {
        let mut stmt = conn.prepare(
            "SELECT id, kind, description, content, quick_replies FROM pings WHERE ui = '{}'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, kind, description, content, replies) in missing_ui {
        let kind = event_kind_from_str(&kind)?;
        let replies = serde_json::from_str::<Vec<QuickReply>>(&replies)?;
        let ui = legacy_event_ui(kind, &description, &content, &replies);
        conn.execute(
            "UPDATE pings SET ui = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&ui)?],
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

pub(super) fn legacy_event_ui(
    kind: EventKind,
    description: &str,
    content: &str,
    quick_replies: &[QuickReply],
) -> serde_json::Value {
    if matches!(kind, EventKind::Judgment) {
        let options = quick_replies
            .iter()
            .enumerate()
            .map(|(index, reply)| {
                serde_json::json!({
                    "key": option_key(index),
                    "label": reply.label,
                    "detail": reply.value,
                    "recommended": index == 0,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "type": "card",
            "children": [
                {
                    "type": "eyebrow",
                    "text": "Taste boundary — fleet stopped",
                    "boundary": true
                },
                { "type": "heading", "text": description },
                { "type": "text", "text": content },
                { "type": "optionList", "options": options }
            ]
        })
    } else {
        serde_json::json!({
            "type": "card",
            "children": [{ "type": "text", "text": content }]
        })
    }
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

#[cfg(test)]
mod tests;
