//! Deterministic parent headlines derived only from Host facts.
use crate::storage::thread_state::StateActor;
use rusqlite::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ChildStatus {
    NeedsOwner,
    Failed,
    Running,
    Queued,
    Blocked,
    Done,
    Idle,
}

impl ChildStatus {
    fn label(self) -> &'static str {
        match self {
            Self::NeedsOwner => "needs you",
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Queued => "queued",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Idle => "idle",
        }
    }
}

pub(crate) fn headline(c: &Connection, parent_id: u64) -> anyhow::Result<String> {
    let mut statement = c.prepare(
        "WITH RECURSIVE descendants(root_id,id) AS (
            SELECT id,id FROM threads WHERE parent_thread_id=?1 AND archived_at IS NULL
            UNION ALL
            SELECT d.root_id,t.id FROM threads t JOIN descendants d ON t.parent_thread_id=d.id
            WHERE t.archived_at IS NULL
         )
         SELECT d.root_id,t.id,t.attention,t.settled_at,
            EXISTS(SELECT 1 FROM thread_turns r WHERE r.thread_id=t.id AND r.state='running'),
            EXISTS(SELECT 1 FROM thread_turns q WHERE q.thread_id=t.id AND q.state='queued'),
            (SELECT state FROM thread_turns z WHERE z.thread_id=t.id AND z.finished_at IS NOT NULL ORDER BY hirsel_utc_timestamp(z.finished_at) DESC,z.id DESC LIMIT 1)
         FROM descendants d JOIN threads t ON t.id=d.id ORDER BY d.root_id,t.id",
    )?;
    let rows = statement
        .query_map([parent_id], |row| {
            let attention: String = row.get(2)?;
            let settled: Option<String> = row.get(3)?;
            let running: bool = row.get(4)?;
            let queued: bool = row.get(5)?;
            let terminal: Option<String> = row.get(6)?;
            let status = if attention == "needs_owner" {
                ChildStatus::NeedsOwner
            } else if running {
                ChildStatus::Running
            } else if queued {
                ChildStatus::Queued
            } else if terminal.as_deref() == Some("failed") {
                ChildStatus::Failed
            } else if matches!(terminal.as_deref(), Some("cancelled" | "interrupted")) {
                ChildStatus::Blocked
            } else if settled.is_some() {
                ChildStatus::Done
            } else {
                ChildStatus::Idle
            };
            Ok((row.get::<_, u64>(0)?, status))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut children = Vec::<(u64, ChildStatus)>::new();
    for (root_id, status) in rows {
        if let Some((_, aggregate)) = children.last_mut().filter(|(id, _)| *id == root_id) {
            *aggregate = (*aggregate).min(status);
        } else {
            children.push((root_id, status));
        }
    }
    let Some((child_id, status)) = children.iter().min_by_key(|(id, status)| (*status, *id)) else {
        return Ok(crate::storage::thread_state::get(c, parent_id)?.own_headline);
    };
    let noun = if children.len() == 1 {
        "child"
    } else {
        "children"
    };
    Ok(format!(
        "{} {noun} · #{child_id} {}",
        children.len(),
        status.label()
    ))
}

pub(crate) fn refresh_ancestors(
    c: &Connection,
    changed_thread_id: u64,
    actor: StateActor,
) -> anyhow::Result<Vec<u64>> {
    let mut current = changed_thread_id;
    let mut changed = Vec::new();
    while let Some(parent) = c.query_row(
        "SELECT parent_thread_id FROM threads WHERE id=?1",
        [current],
        |row| row.get::<_, Option<u64>>(0),
    )? {
        let next = headline(c, parent)?;
        if crate::storage::thread_state::replace_headline(c, parent, &next, actor, "child_rollup")?
        {
            changed.push(parent);
        }
        current = parent;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_fixed() {
        assert!(ChildStatus::NeedsOwner < ChildStatus::Failed);
        assert!(ChildStatus::Failed < ChildStatus::Running);
        assert!(ChildStatus::Running < ChildStatus::Queued);
    }
}
