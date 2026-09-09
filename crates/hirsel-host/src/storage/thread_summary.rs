//! Bounded inventory projection from durable facts, without loading histories.
use super::{common::parse_ts, thread_activity};
use hirsel_proto::Thread;
use rusqlite::Connection;

pub(super) fn register_timestamp_function(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "hirsel_utc_timestamp",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let value = context.get::<String>(0)?;
            let ts = parse_ts(&value)?;
            Ok(ts.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        },
    )
}

pub(super) fn populate(conn: &Connection, thread: &mut Thread) -> anyhow::Result<()> {
    let (running, queued, finished, activity): (Option<u64>, u64, Option<u64>, String) = conn
        .query_row(
            "SELECT
                (SELECT id FROM thread_turns WHERE thread_id=?1 AND state='running'
                 ORDER BY id LIMIT 1),
                (SELECT COUNT(*) FROM thread_turns WHERE thread_id=?1 AND state='queued'),
                (SELECT id FROM thread_turns WHERE thread_id=?1
                 AND state IN ('completed','failed','cancelled','interrupted')
                 AND finished_at IS NOT NULL
                 ORDER BY hirsel_utc_timestamp(finished_at) DESC, id DESC LIMIT 1),
                (SELECT MAX(hirsel_utc_timestamp(ts)) FROM (
                    SELECT created_at AS ts FROM threads WHERE id=?1
                    UNION ALL SELECT ts FROM chat_messages WHERE thread_id=?1
                    UNION ALL SELECT started_at FROM thread_turns WHERE thread_id=?1
                    UNION ALL SELECT finished_at FROM thread_turns
                        WHERE thread_id=?1 AND finished_at IS NOT NULL
                    UNION ALL SELECT ts FROM thread_activities WHERE thread_id=?1
                ))",
            [thread.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    thread.running_turn = running
        .map(|id| thread_activity::get(conn, id))
        .transpose()?;
    thread.queued_turn_count = queued;
    thread.last_finished_turn = finished
        .map(|id| thread_activity::get(conn, id))
        .transpose()?;
    thread.last_activity_at = parse_ts(&activity)?;
    Ok(())
}

#[cfg(test)]
#[path = "thread_summary_tests.rs"]
mod tests;
