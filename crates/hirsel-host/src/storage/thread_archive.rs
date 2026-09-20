//! The one archive path, shared by the Owner action and the agent tool.
//!
//! Archiving is Hirsel's only removal: a Thread and its subtree leave the
//! active tree, their open work is cancelled, and their conversation,
//! activity and artifacts stay exactly where they were. Both callers land in
//! [`apply`], so an Owner archive and an agent archive can never diverge.
use super::{thread_activity, threads};
use hirsel_proto::{Thread, ThreadActivity, ThreadKind};
use rusqlite::{Transaction, params};
use serde_json::json;

/// Who performed the archive, and where its single activity row belongs.
pub(crate) struct ArchiveActor {
    /// Thread that records the activity — the caller's own Thread for an
    /// agent, the archived Thread itself for the Owner.
    pub thread_id: u64,
    /// Turn the activity belongs to, so it renders on that turn's run card.
    pub turn_id: Option<u64>,
    /// `owner` or `agent`; the clients render the sentence from this.
    pub actor: &'static str,
    /// A turn that must survive the archive: the agent turn issuing the call
    /// finishes cleanly, and the admission guard stops the next wake instead.
    pub keep_turn_id: Option<u64>,
}

pub(crate) struct ArchiveOutcome {
    /// Every Thread clients must refresh, root first.
    pub threads: Vec<Thread>,
    pub cancelled_turn_ids: Vec<u64>,
    pub activity: ThreadActivity,
}

/// Request cancellation of a Thread's open turns. This is the cancel path
/// `threads.cancel` uses (`first_only`), reused whole for archiving.
pub(crate) fn request_cancel(
    tx: &Transaction<'_>,
    thread_id: u64,
    first_only: bool,
    keep_turn_id: Option<u64>,
) -> rusqlite::Result<Vec<u64>> {
    let mut statement = tx.prepare(&format!(
        "SELECT id FROM thread_turns WHERE thread_id=?1 AND state IN ({}) AND (?2 IS NULL OR id<>?2) ORDER BY CASE state WHEN 'running' THEN 0 ELSE 1 END,id{}",
        super::schema::state_list(Some(false)),
        if first_only { " LIMIT 1" } else { "" }
    ))?;
    let ids = statement
        .query_map(params![thread_id, keep_turn_id], |r| r.get::<_, u64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        tx.execute(
            "UPDATE thread_turns SET cancel_requested_at=COALESCE(cancel_requested_at,strftime('%Y-%m-%dT%H:%M:%fZ','now')) WHERE id=?1",
            [id],
        )?;
    }
    Ok(ids)
}

/// Archived Threads accept no new work. Wakes and delegated sends check this
/// before they create a turn, so archiving a Thread the agent is running in
/// ends that turn and nothing follows it.
pub(super) fn ensure_accepts_work(c: &rusqlite::Connection, thread_id: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        threads::get(c, thread_id)?.archived_at.is_none(),
        "Thread is archived and accepts no new work; unarchive it first"
    );
    Ok(())
}

fn subtree(tx: &Transaction<'_>, root: u64) -> rusqlite::Result<Vec<u64>> {
    tx.prepare(
        "WITH RECURSIVE scope(id) AS (SELECT id FROM threads WHERE id=?1 UNION ALL SELECT t.id FROM threads t JOIN scope s ON t.parent_thread_id=s.id) SELECT id FROM scope ORDER BY id",
    )?
    .query_map([root], |r| r.get::<_, u64>(0))?
    .collect()
}

pub(crate) fn apply(
    tx: &Transaction<'_>,
    root: u64,
    archived: bool,
    actor: &ArchiveActor,
) -> anyhow::Result<ArchiveOutcome> {
    let root_thread = threads::get(tx, root)?;
    let now = chrono::Utc::now().to_rfc3339();
    let ids = subtree(tx, root)?;
    let mut cancelled_turn_ids = Vec::new();
    if archived {
        for id in &ids {
            cancelled_turn_ids.extend(request_cancel(tx, *id, false, actor.keep_turn_id)?);
        }
    }
    // Only the Threads whose state actually moves are rewritten: re-archiving
    // an already archived subtree bumps no revisions and republishes nothing.
    let mut changed = Vec::new();
    for id in &ids {
        let updated = if archived {
            tx.execute(
                "UPDATE threads SET archived_at=?2,attention='quiet',updated_at=?2,revision=revision+1 WHERE id=?1 AND archived_at IS NULL",
                params![id, now],
            )?
        } else {
            tx.execute(
                "UPDATE threads SET archived_at=NULL,updated_at=?2,revision=revision+1 WHERE id=?1 AND archived_at IS NOT NULL",
                params![id, now],
            )?
        };
        if updated > 0 {
            super::thread_state::touch(
                tx,
                *id,
                match actor.actor {
                    "owner" => super::thread_state::StateActor::owner(),
                    _ => super::thread_state::StateActor {
                        kind: "thread",
                        thread_id: Some(actor.thread_id),
                        turn_id: actor.turn_id,
                    },
                },
                if archived { "archived" } else { "unarchived" },
                false,
            )?;
            changed.push(threads::get(tx, *id)?);
        }
    }
    let mut publications = Vec::new();
    if !changed.iter().any(|thread| thread.id == root) {
        publications.push(root_thread.clone());
    }
    publications.extend(changed);
    publications.sort_by_key(|thread| (thread.id != root, thread.id));
    let data = json!({
        "archived": archived,
        "actor": actor.actor,
        "thread_id": root,
        "thread_kind": match root_thread.kind { ThreadKind::Space => "space", ThreadKind::Task => "task" },
        "title": root_thread.title,
        "thread_count": publications.len(),
        "cancelled_turns": cancelled_turn_ids.len(),
    });
    tx.execute(
        "INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,'archived',?3,?4)",
        params![
            actor.thread_id,
            actor.turn_id,
            serde_json::to_string(&data)?,
            now
        ],
    )?;
    let activity = thread_activity::activity(tx, tx.last_insert_rowid() as u64)?;
    if archived && threads::home_project_id(tx)?.is_some_and(|home| ids.contains(&home)) {
        let history_id: String =
            tx.query_row("SELECT value FROM meta WHERE key='history_id'", [], |row| {
                row.get(0)
            })?;
        let (replacement, inserted) = threads::reconcile_home_project(tx, &history_id)?;
        if inserted {
            publications.push(replacement);
        }
    }
    Ok(ArchiveOutcome {
        threads: publications,
        cancelled_turn_ids,
        activity,
    })
}

#[cfg(test)]
#[path = "thread_archive_tests.rs"]
mod tests;
