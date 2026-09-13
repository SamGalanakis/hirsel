//! Durable reach. A Thread's default reach is itself and its descendants; a
//! grant names one other Thread whose subtree it may address as well. Grants
//! are ordinary rows: visible to the Owner, inspectable by the Thread that
//! holds them, and removable without a release.
use super::{Storage, ThreadCaller, thread_scope, threads};
use hirsel_proto::{Thread, ThreadGrant, ThreadGrantSource};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// A complete reach snapshot for one Thread, read at a known revision.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ThreadGrants {
    pub history_id: String,
    pub thread_id: u64,
    pub thread: Thread,
    pub revision: u64,
    pub grants: Vec<ThreadGrant>,
}

/// The most grants one Thread may hold. Reach stays readable in one line.
const MAX_GRANTS: u64 = 100;

pub(super) fn list(c: &Connection, thread_id: u64) -> anyhow::Result<Vec<ThreadGrant>> {
    Ok(c.prepare(
        "SELECT g.thread_id,g.target_thread_id,t.title,g.granted_by,g.granted_by_thread_id,g.granted_at,g.note,t.kind
         FROM thread_grants g JOIN threads t ON t.id=g.target_thread_id
         WHERE g.thread_id=?1 ORDER BY g.target_thread_id",
    )?
    .query_map([thread_id], |r| {
        let by: String = r.get(3)?;
        let by_thread: Option<u64> = r.get(4)?;
        let kind = r.get::<_, String>(7)?;
        Ok(ThreadGrant {
            thread_id: r.get(0)?,
            target_thread_id: r.get(1)?,
            title: r.get(2)?,
            kind: threads::parse_kind(&kind).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Text,
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid Thread kind `{kind}`"),
                    )
                    .into(),
                )
            })?,
            granted_by: match by_thread {
                Some(thread_id) if by == "thread" => ThreadGrantSource::Thread { thread_id },
                _ => ThreadGrantSource::Owner,
            },
            granted_at: super::common::parse_ts(&r.get::<_, String>(5)?)?,
            note: r.get(6)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

/// The one line the Owner reads above the composer and the Agent reads in its
/// own context: what this Thread can address, in the order it was granted.
pub(super) fn reach_summary(c: &Connection, thread_id: u64) -> anyhow::Result<String> {
    let mut summary = String::from("self + subtree");
    for grant in list(c, thread_id)? {
        summary.push_str(&format!(
            " · +{}",
            crate::thread_identity::ThreadIdentityRef {
                id: grant.target_thread_id,
                kind: grant.kind,
                title: grant.title,
            }
            .label()
        ));
    }
    Ok(summary)
}

pub(super) fn snapshot(c: &Connection, thread_id: u64) -> anyhow::Result<ThreadGrants> {
    let thread = threads::get(c, thread_id)?;
    Ok(ThreadGrants {
        history_id: c.query_row("SELECT value FROM meta WHERE key='history_id'", [], |r| {
            r.get(0)
        })?,
        thread_id,
        revision: thread.revision,
        thread,
        grants: list(c, thread_id)?,
    })
}

fn advance(c: &Connection, thread_id: u64) -> anyhow::Result<()> {
    c.execute(
        "UPDATE threads SET revision=revision+1,updated_at=?2 WHERE id=?1",
        params![thread_id, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn normalized_note(note: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(note) = note else { return Ok(None) };
    anyhow::ensure!(
        note.chars().count() <= 200 && !note.chars().any(char::is_control),
        "grant note must be at most 200 characters without control characters"
    );
    Ok((!note.trim().is_empty()).then(|| note.trim().to_string()))
}

/// A grant only ever widens. Default reach already covers the Thread itself and
/// everything below it, so naming one of those is a mistake, not a no-op row.
fn validate_target(c: &Connection, thread_id: u64, target: u64) -> anyhow::Result<()> {
    threads::get(c, thread_id)?;
    threads::get(c, target)?;
    anyhow::ensure!(thread_id != target, "a Thread always reaches itself");
    anyhow::ensure!(
        !thread_scope::is_ancestor(c, thread_id, target)?,
        "Thread #{target} is already inside this Thread's own subtree"
    );
    Ok(())
}

pub(super) fn grant(
    c: &Connection,
    thread_id: u64,
    target: u64,
    source: &ThreadGrantSource,
    note: Option<&str>,
) -> anyhow::Result<ThreadGrants> {
    validate_target(c, thread_id, target)?;
    let note = normalized_note(note)?;
    let (by, by_thread) = match source {
        ThreadGrantSource::Owner => ("owner", None),
        ThreadGrantSource::Thread { thread_id } => ("thread", Some(*thread_id)),
    };
    let count: u64 = c.query_row(
        "SELECT count(*) FROM thread_grants WHERE thread_id=?1",
        [thread_id],
        |r| r.get(0),
    )?;
    let existing: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread_grants WHERE thread_id=?1 AND target_thread_id=?2)",
        params![thread_id, target],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        existing || count < MAX_GRANTS,
        "Thread already holds {MAX_GRANTS} grants"
    );
    let changed = c.execute(
        "INSERT INTO thread_grants(thread_id,target_thread_id,granted_by,granted_by_thread_id,granted_at,note)
         VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(thread_id,target_thread_id) DO NOTHING",
        params![
            thread_id,
            target,
            by,
            by_thread,
            chrono::Utc::now().to_rfc3339(),
            note
        ],
    )?;
    if changed > 0 {
        advance(c, thread_id)?;
    }
    snapshot(c, thread_id)
}

pub(super) fn revoke(c: &Connection, thread_id: u64, target: u64) -> anyhow::Result<ThreadGrants> {
    threads::get(c, thread_id)?;
    if c.execute(
        "DELETE FROM thread_grants WHERE thread_id=?1 AND target_thread_id=?2",
        params![thread_id, target],
    )? > 0
    {
        advance(c, thread_id)?;
    }
    snapshot(c, thread_id)
}

/// Only a strict ancestor may widen or narrow a descendant's reach, and only
/// within what it can reach itself: authority flows down the tree, never sideways.
pub(super) fn authorize_widening(
    c: &Connection,
    caller: u64,
    thread_id: u64,
    target: Option<u64>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        caller != thread_id,
        "a Thread cannot change its own reach; ask the Thread that delegated to you"
    );
    anyhow::ensure!(
        thread_scope::is_ancestor(c, caller, thread_id)?,
        "only an ancestor Thread can change a Thread's reach"
    );
    if let Some(target) = target {
        thread_scope::authorize(c, caller, target)?;
    }
    Ok(())
}

fn replay(c: &Connection, client_id: &str, payload: &str) -> anyhow::Result<bool> {
    anyhow::ensure!(
        !client_id.trim().is_empty() && client_id.len() <= 200,
        "grant client_id must be 1..200 bytes"
    );
    let old: Option<String> = c
        .query_row(
            "SELECT payload FROM thread_grant_receipts WHERE client_id=?1",
            [client_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        anyhow::ensure!(old == payload, "grant client_id payload changed");
        Ok(true)
    } else {
        Ok(false)
    }
}

impl Storage {
    pub(crate) async fn grants_publication_snapshot(
        &self,
        history_id: &str,
        thread_id: u64,
    ) -> anyhow::Result<(tokio::sync::MutexGuard<'_, Connection>, ThreadGrants)> {
        let guard = self.conn.lock().await;
        thread_scope::validate_history(&guard, history_id)?;
        let current = snapshot(&guard, thread_id)?;
        Ok((guard, current))
    }

    /// The Owner widens or narrows any Thread's reach; the tree is theirs.
    pub(crate) async fn set_thread_reach(
        &self,
        client_id: &str,
        history_id: &str,
        thread_id: u64,
        target_thread_id: u64,
        note: Option<&str>,
        granted: bool,
    ) -> anyhow::Result<ThreadGrants> {
        let mut c = self.conn.lock().await;
        let tx = c.transaction()?;
        thread_scope::validate_history(&tx, history_id)?;
        threads::get(&tx, thread_id)?;
        let note = normalized_note(note)?;
        let payload = serde_json::to_string(&serde_json::json!({
            "operation": if granted { "grant" } else { "revoke" },
            "history_id": history_id,
            "thread_id": thread_id,
            "target_thread_id": target_thread_id,
            "note": note,
        }))?;
        let result = if replay(&tx, client_id, &payload)? {
            snapshot(&tx, thread_id)?
        } else {
            let result = if granted {
                grant(
                    &tx,
                    thread_id,
                    target_thread_id,
                    &ThreadGrantSource::Owner,
                    note.as_deref(),
                )?
            } else {
                revoke(&tx, thread_id, target_thread_id)?
            };
            tx.execute(
                "INSERT INTO thread_grant_receipts(client_id,payload) VALUES(?1,?2)",
                params![client_id, payload],
            )?;
            result
        };
        tx.commit()?;
        Ok(result)
    }

    /// One refusal, one durable row, in the Thread that tried it. Written
    /// outside the refused call's own transaction, which never started.
    pub(crate) async fn record_refusal(
        &self,
        caller: &ThreadCaller,
        detail: &serde_json::Value,
    ) -> anyhow::Result<hirsel_proto::ThreadActivity> {
        let c = self.conn.lock().await;
        thread_scope::validate_caller(&c, caller)?;
        c.execute(
            "INSERT INTO thread_activities(thread_id,turn_id,kind,data,ts) VALUES(?1,?2,'refusal',?3,?4)",
            params![
                caller.thread_id,
                caller.turn_id,
                serde_json::to_string(detail)?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        super::thread_activity::activity(&c, c.last_insert_rowid() as u64)
    }

    /// What an agent sees of its own reach, without a mutation.
    pub(crate) async fn thread_reach(&self, caller: &ThreadCaller) -> anyhow::Result<String> {
        let c = self.conn.lock().await;
        thread_scope::validate_caller(&c, caller)?;
        reach_summary(&c, caller.thread_id)
    }
}

#[cfg(test)]
#[path = "thread_grants_tests.rs"]
mod tests;
