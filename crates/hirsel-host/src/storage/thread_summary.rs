//! Bounded inventory projection from durable facts, without loading histories.
use super::{common::parse_ts, thread_activity};
use hirsel_proto::{Thread, ThreadStatus, ThreadStatusKind};
use rusqlite::{Connection, OptionalExtension};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub(super) fn register_functions(
    conn: &Connection,
    hung_after_minutes: Arc<AtomicU64>,
) -> rusqlite::Result<()> {
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
    )?;
    conn.create_scalar_function(
        "hirsel_hung_minutes",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |_| Ok(hung_after_minutes.load(Ordering::Relaxed)),
    )
}

pub(super) fn populate(conn: &Connection, thread: &mut Thread) -> anyhow::Result<()> {
    let (running, queued, finished, activity): (Option<u64>, u64, Option<u64>, String) = conn
        .query_row(
            &format!(
                "SELECT
                (SELECT id FROM thread_turns WHERE thread_id=?1 AND state='running'
                 ORDER BY id LIMIT 1),
                (SELECT COUNT(*) FROM thread_turns WHERE thread_id=?1 AND state='queued'),
                (SELECT id FROM thread_turns WHERE thread_id=?1
                 AND state IN ({terminal})
                 ORDER BY hirsel_utc_timestamp(finished_at) DESC, id DESC LIMIT 1),
                (SELECT MAX(hirsel_utc_timestamp(ts)) FROM (
                    SELECT created_at AS ts FROM threads WHERE id=?1
                    UNION ALL SELECT ts FROM chat_messages WHERE thread_id=?1
                    UNION ALL SELECT accepted_at FROM thread_turns WHERE thread_id=?1
                    UNION ALL SELECT started_at FROM thread_turns WHERE thread_id=?1 AND started_at IS NOT NULL
                    UNION ALL SELECT finished_at FROM thread_turns
                        WHERE thread_id=?1 AND finished_at IS NOT NULL
                    UNION ALL SELECT ts FROM thread_activities WHERE thread_id=?1
                ))",
                terminal = super::schema::state_list(Some(true))
            ),
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
    thread.execution = super::thread_execution::preference(conn, thread.id)?;
    let running_last_event: Option<String> = conn
        .query_row(
            "SELECT last_event_at FROM thread_turns WHERE thread_id=?1 AND state='running' ORDER BY id LIMIT 1",
            [thread.id],
            |row| row.get(0),
        )
        .optional()?;
    let now = chrono::Utc::now();
    let hung_after_minutes: i64 = conn
        .query_row("SELECT hirsel_hung_minutes()", [], |row| row.get(0))
        .unwrap_or(10);
    thread.status = if thread.snoozed_until.is_some_and(|until| until > now) {
        ThreadStatus {
            kind: ThreadStatusKind::Sleeping,
            reason: format!(
                "Snoozed until {}",
                thread.snoozed_until.expect("checked above")
            ),
        }
    } else if thread.attention == hirsel_proto::ThreadAttention::NeedsOwner {
        ThreadStatus {
            kind: ThreadStatusKind::NeedsYou,
            reason: "Waiting for your input".into(),
        }
    } else if thread.running_turn.is_some()
        && running_last_event
            .as_deref()
            .map(parse_ts)
            .transpose()?
            .is_some_and(|last| now.signed_duration_since(last).num_minutes() >= hung_after_minutes)
    {
        ThreadStatus {
            kind: ThreadStatusKind::Hung,
            reason: format!(
                "No durable turn event for {hung_after_minutes} minute{}",
                if hung_after_minutes == 1 { "" } else { "s" },
            ),
        }
    } else if thread.running_turn.is_some() {
        ThreadStatus {
            kind: ThreadStatusKind::Running,
            reason: "A turn is running".into(),
        }
    } else if thread.queued_turn_count > 0 {
        ThreadStatus {
            kind: ThreadStatusKind::Queued,
            reason: format!(
                "{} turn{} queued",
                thread.queued_turn_count,
                if thread.queued_turn_count == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        }
    } else {
        ThreadStatus {
            kind: ThreadStatusKind::Idle,
            reason: "No work is active".into(),
        }
    };
    Ok(())
}

#[cfg(test)]
#[path = "thread_summary_tests.rs"]
mod tests;
