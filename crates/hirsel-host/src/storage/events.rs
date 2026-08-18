//! Events and pings: creation, lifecycle, snooze/archive, forks.

use super::Storage;
use super::common::{collect_rows, parse_ts};
use super::schema::legacy_event_ui;
use chrono::DateTime;
use chrono::Utc;
use hirsel_proto::Event;
use hirsel_proto::EventKind;
use hirsel_proto::EventSource;
use hirsel_proto::EventSourceKind;
use hirsel_proto::EventStatus;
use hirsel_proto::Ping;
use hirsel_proto::PingStatus;
use hirsel_proto::QuickReply;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::types::Type;

impl Storage {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_event(
        &self,
        kind: EventKind,
        source: EventSource,
        name: impl Into<String>,
        description: impl Into<String>,
        ui: serde_json::Value,
        anchor: u64,
        requires_response: bool,
        quick_replies: Vec<QuickReply>,
    ) -> anyhow::Result<Event> {
        let name = name.into();
        let description = description.into();
        validate_event_fields(kind, &name, &description, requires_response)?;
        let ts = Utc::now();
        let encoded_replies = serde_json::to_string(&quick_replies)?;
        let encoded_ui = serde_json::to_string(&ui)?;
        let conn = self.conn.lock().await;
        conn.execute(
            "
            INSERT INTO pings (
                kind,
                source_kind,
                source_ref,
                name,
                description,
                content,
                ui,
                anchor,
                requires_response,
                quick_replies,
                status,
                read,
                archived,
                ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7, ?8, ?9, 'open', 0, 0, ?10)
            ",
            params![
                event_kind_to_str(kind),
                event_source_kind_to_str(source.kind),
                source.r#ref,
                name,
                description,
                encoded_ui,
                anchor,
                requires_response,
                encoded_replies,
                ts.to_rfc3339()
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(Event {
            id,
            kind,
            source,
            name,
            description,
            ui,
            anchor,
            requires_response,
            quick_replies,
            status: EventStatus::Open,
            read: false,
            archived: false,
            snoozed_until: None,
            archived_at: None,
            fork_sc: None,
            ts,
        })
    }

    /// Compatibility entry point used by older host tests and the `pings.send` tool alias.
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
        let kind = if requires_response {
            EventKind::Judgment
        } else {
            EventKind::Info
        };
        let ui = legacy_event_ui(kind, &description, &content, &quick_replies);
        self.create_event(
            kind,
            EventSource {
                kind: EventSourceKind::Agent,
                r#ref: None,
            },
            name,
            description,
            ui,
            anchor,
            requires_response,
            quick_replies,
        )
        .await
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

    pub async fn reopen_ping(&self, ping_id: u64) -> anyhow::Result<Option<Ping>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET status = 'open' WHERE id = ?1",
            params![ping_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, ping_id)?))
    }

    /// Replace only the producer-authored presentation of an open Task. Stable
    /// identity, source, Anchor, lifecycle flags, and timestamps are immutable
    /// at this boundary; the Host validates the constrained instrument before
    /// one atomic same-row update.
    pub async fn recompose_event(
        &self,
        event_id: u64,
        description: Option<String>,
        ui: serde_json::Value,
    ) -> anyhow::Result<Option<Event>> {
        crate::task_ui::validate(&ui)?;
        let conn = self.conn.lock().await;
        let current = match get_ping(&conn, event_id) {
            Ok(event) => event,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if current.status != EventStatus::Open {
            anyhow::bail!("only an open Task can be recomposed");
        }
        let description = description.unwrap_or_else(|| current.description.clone());
        validate_event_fields(
            current.kind,
            &current.name,
            &description,
            current.requires_response,
        )?;
        conn.execute(
            "UPDATE pings SET description = ?2, ui = ?3 WHERE id = ?1 AND status = 'open'",
            params![event_id, description, serde_json::to_string(&ui)?],
        )?;
        Ok(Some(get_ping(&conn, event_id)?))
    }

    pub async fn archive_event(&self, event_id: u64) -> anyhow::Result<Option<Event>> {
        let conn = self.conn.lock().await;
        let archived_at = Utc::now().to_rfc3339();
        let changed = conn.execute(
            "UPDATE pings SET archived = 1, status = 'done', archived_at = COALESCE(archived_at, ?2) WHERE id = ?1",
            params![event_id, archived_at],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, event_id)?))
    }

    pub async fn unarchive_event(&self, event_id: u64) -> anyhow::Result<Option<Event>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET archived = 0, archived_at = NULL WHERE id = ?1",
            params![event_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, event_id)?))
    }

    pub async fn archive_finished_events(&self) -> anyhow::Result<usize> {
        let conn = self.conn.lock().await;
        let archived_at = Utc::now().to_rfc3339();
        Ok(conn.execute(
            "
            UPDATE pings
            SET archived = 1, status = 'done', archived_at = ?1
            WHERE archived = 0
              AND (status = 'done' OR (read != 0 AND requires_response = 0))
            ",
            params![archived_at],
        )?)
    }

    pub async fn snooze_event(
        &self,
        event_id: u64,
        until: DateTime<Utc>,
    ) -> anyhow::Result<Option<Event>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET snoozed_until = ?2 WHERE id = ?1",
            params![event_id, until.to_rfc3339()],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, event_id)?))
    }

    pub async fn unsnooze_event(&self, event_id: u64) -> anyhow::Result<Option<Event>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET snoozed_until = NULL WHERE id = ?1",
            params![event_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, event_id)?))
    }

    pub async fn clear_expired_snoozes(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<Event>> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let event_ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM pings WHERE snoozed_until IS NOT NULL AND snoozed_until <= ?1 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![now.to_rfc3339()], |row| row.get::<_, u64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if event_ids.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }
        tx.execute(
            "UPDATE pings SET snoozed_until = NULL WHERE snoozed_until IS NOT NULL AND snoozed_until <= ?1",
            params![now.to_rfc3339()],
        )?;
        let events = event_ids
            .into_iter()
            .map(|event_id| get_ping(&tx, event_id))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(events)
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

    pub async fn set_event_fork(
        &self,
        event_id: u64,
        fork_sc: Option<&str>,
    ) -> anyhow::Result<Option<Event>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET fork_sc = ?2 WHERE id = ?1",
            params![event_id, fork_sc],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, event_id)?))
    }

    pub async fn resolve_event_fork(&self, event_id: u64) -> anyhow::Result<Option<Event>> {
        let conn = self.conn.lock().await;
        let changed = conn.execute(
            "UPDATE pings SET status = 'done', fork_sc = NULL WHERE id = ?1",
            params![event_id],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(get_ping(&conn, event_id)?))
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
            SELECT id, kind, source_kind, source_ref, name, description, ui, anchor,
                   requires_response, quick_replies, status, read, archived, snoozed_until,
                   archived_at, fork_sc, ts
            FROM pings
            ORDER BY id ASC
            ",
        )?;
        let rows = stmt.query_map([], ping_from_row)?;
        collect_rows(rows)
    }
}

pub(super) fn ping_snapshot_from_conn(conn: &Connection) -> anyhow::Result<Vec<Ping>> {
    let mut pings = Vec::new();
    {
        let mut stmt = conn.prepare(
            "
            SELECT id, kind, source_kind, source_ref, name, description, ui, anchor,
                   requires_response, quick_replies, status, read, archived, snoozed_until,
                   archived_at, fork_sc, ts
            FROM pings
            WHERE status = 'open'
            ORDER BY CASE kind
                WHEN 'judgment' THEN 0
                WHEN 'summary' THEN 1
                ELSE 2
            END, id ASC
            ",
        )?;
        let rows = stmt.query_map([], ping_from_row)?;
        pings.extend(collect_rows(rows)?);
    }
    {
        let mut stmt = conn.prepare(
            "
            SELECT id, kind, source_kind, source_ref, name, description, ui, anchor,
                   requires_response, quick_replies, status, read, archived, snoozed_until,
                   archived_at, fork_sc, ts
            FROM pings
            WHERE status = 'done'
            ORDER BY id DESC
            ",
        )?;
        let rows = stmt.query_map([], ping_from_row)?;
        let mut done = collect_rows(rows)?;
        done.reverse();
        pings.extend(done);
    }
    Ok(pings)
}

fn validate_event_fields(
    kind: EventKind,
    name: &str,
    description: &str,
    requires_response: bool,
) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("event name is required");
    }
    if name.chars().count() > 32 {
        anyhow::bail!("event name must be at most 32 characters");
    }
    if description.trim().is_empty() {
        anyhow::bail!("event description is required");
    }
    if description.lines().count() != 1 {
        anyhow::bail!("event description must be one line");
    }
    if matches!(kind, EventKind::Judgment) != requires_response {
        anyhow::bail!("only judgment events may require a response");
    }
    Ok(())
}

fn get_ping(conn: &Connection, id: u64) -> rusqlite::Result<Ping> {
    get_ping_optional(conn, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub(super) fn get_ping_optional(conn: &Connection, id: u64) -> rusqlite::Result<Option<Ping>> {
    conn.query_row(
        "
        SELECT id, kind, source_kind, source_ref, name, description, ui, anchor,
               requires_response, quick_replies, status, read, archived, snoozed_until,
               archived_at, fork_sc, ts
        FROM pings
        WHERE id = ?1
        ",
        params![id],
        ping_from_row,
    )
    .optional()
}

fn ping_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Ping> {
    let kind: String = row.get(1)?;
    let source_kind: String = row.get(2)?;
    let ui: String = row.get(6)?;
    let replies: String = row.get(9)?;
    let status: String = row.get(10)?;
    let snoozed_until = row.get::<_, Option<String>>(13)?;
    let archived_at = row.get::<_, Option<String>>(14)?;
    let ts: String = row.get(16)?;
    Ok(Ping {
        id: row.get(0)?,
        kind: event_kind_from_str(&kind)?,
        source: EventSource {
            kind: event_source_kind_from_str(&source_kind)?,
            r#ref: row.get(3)?,
        },
        name: row.get(4)?,
        description: row.get(5)?,
        ui: serde_json::from_str(&ui).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(error))
        })?,
        anchor: row.get(7)?,
        requires_response: row.get::<_, i64>(8)? != 0,
        quick_replies: serde_json::from_str(&replies).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
        })?,
        status: status_from_str(&status)?,
        read: row.get::<_, i64>(11)? != 0,
        archived: row.get::<_, i64>(12)? != 0,
        snoozed_until: snoozed_until.as_deref().map(parse_ts).transpose()?,
        archived_at: archived_at.as_deref().map(parse_ts).transpose()?,
        fork_sc: row.get(15)?,
        ts: parse_ts(&ts)?,
    })
}

fn status_from_str(value: &str) -> rusqlite::Result<PingStatus> {
    match value {
        "open" => Ok(PingStatus::Open),
        "done" => Ok(PingStatus::Done),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn event_kind_to_str(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Judgment => "judgment",
        EventKind::Summary => "summary",
        EventKind::Info => "info",
    }
}

pub(super) fn event_kind_from_str(value: &str) -> rusqlite::Result<EventKind> {
    match value {
        "judgment" => Ok(EventKind::Judgment),
        "summary" => Ok(EventKind::Summary),
        "info" => Ok(EventKind::Info),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            1,
            Type::Text,
            format!("unknown event kind: {other}").into(),
        )),
    }
}

fn event_source_kind_to_str(kind: EventSourceKind) -> &'static str {
    match kind {
        EventSourceKind::Agent => "agent",
        EventSourceKind::Subagent => "subagent",
        EventSourceKind::Scheduled => "scheduled",
        EventSourceKind::Monitor => "monitor",
    }
}

fn event_source_kind_from_str(value: &str) -> rusqlite::Result<EventSourceKind> {
    match value {
        "agent" => Ok(EventSourceKind::Agent),
        "subagent" => Ok(EventSourceKind::Subagent),
        "scheduled" => Ok(EventSourceKind::Scheduled),
        "monitor" => Ok(EventSourceKind::Monitor),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            2,
            Type::Text,
            format!("unknown event source kind: {other}").into(),
        )),
    }
}

#[cfg(test)]
mod tests;
