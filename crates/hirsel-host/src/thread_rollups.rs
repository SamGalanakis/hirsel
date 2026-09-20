//! Deterministic parent headlines derived only from Host facts.
use rusqlite::{Connection, params};

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
    const fn label(self) -> &'static str {
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

fn headline(c: &Connection, parent_id: u64) -> anyhow::Result<String> {
    let mut statement = c.prepare(
        "WITH RECURSIVE descendants(root_id,id) AS (
            SELECT id,id FROM threads WHERE parent_thread_id=?1 AND archived_at IS NULL
            UNION ALL SELECT d.root_id,t.id FROM threads t JOIN descendants d ON t.parent_thread_id=d.id WHERE t.archived_at IS NULL
         )
         SELECT d.root_id,t.id,t.attention,t.settled_at,
            EXISTS(SELECT 1 FROM thread_turns r WHERE r.thread_id=t.id AND r.state='running'),
            EXISTS(SELECT 1 FROM thread_turns q WHERE q.thread_id=t.id AND q.state='queued'),
            (SELECT state FROM thread_turns z WHERE z.thread_id=t.id AND z.finished_at IS NOT NULL ORDER BY hirsel_utc_timestamp(z.finished_at) DESC,z.id DESC LIMIT 1)
         FROM descendants d JOIN threads t ON t.id=d.id ORDER BY d.root_id,t.id",
    )?;
    let rows = statement
        .query_map([parent_id], |row| {
            let terminal: Option<String> = row.get(6)?;
            let status = if row.get::<_, String>(2)? == "needs_owner" {
                ChildStatus::NeedsOwner
            } else if row.get::<_, bool>(4)? {
                ChildStatus::Running
            } else if row.get::<_, bool>(5)? {
                ChildStatus::Queued
            } else if terminal.as_deref() == Some("failed") {
                ChildStatus::Failed
            } else if matches!(terminal.as_deref(), Some("cancelled" | "interrupted")) {
                ChildStatus::Blocked
            } else if row.get::<_, Option<String>>(3)?.is_some() {
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
        return Ok(c.query_row(
            "SELECT own_headline FROM threads WHERE id=?1",
            [parent_id],
            |row| row.get(0),
        )?);
    };
    Ok(format!(
        "{} {} · #{child_id} {}",
        children.len(),
        if children.len() == 1 {
            "child"
        } else {
            "children"
        },
        status.label()
    ))
}

fn replace(c: &Connection, id: u64, next: &str) -> anyhow::Result<bool> {
    let current: String = c.query_row("SELECT headline FROM threads WHERE id=?1", [id], |r| {
        r.get(0)
    })?;
    if current == next {
        return Ok(false);
    }
    c.execute("UPDATE threads SET previous_headline=headline,headline=?2,headline_revision=headline_revision+1,updated_at=?3,revision=revision+1 WHERE id=?1", params![id, next, chrono::Utc::now().to_rfc3339()])?;
    Ok(true)
}

pub(crate) fn refresh_from(c: &Connection, changed_id: u64) -> anyhow::Result<Vec<u64>> {
    let mut current = changed_id;
    let mut changed = Vec::new();
    while let Some(parent) = c.query_row(
        "SELECT parent_thread_id FROM threads WHERE id=?1",
        [current],
        |r| r.get::<_, Option<u64>>(0),
    )? {
        if replace(c, parent, &headline(c, parent)?)? {
            changed.push(parent);
        }
        current = parent;
    }
    Ok(changed)
}

pub(crate) fn refresh_self_and_ancestors(c: &Connection, id: u64) -> anyhow::Result<Vec<u64>> {
    let own = headline(c, id)?;
    let mut changed = Vec::new();
    if replace(c, id, &own)? {
        changed.push(id);
    }
    changed.extend(refresh_from(c, id)?);
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
