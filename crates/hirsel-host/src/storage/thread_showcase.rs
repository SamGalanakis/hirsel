//! A showcase is one explicit, current artifact reference, independent of cards.
use super::{Storage, threads};
use hirsel_proto::{ArtifactSummary, Thread};
use rusqlite::Connection;
use rusqlite::params;
use serde_json::Value;

/// Omission preserves the pointer; JSON null explicitly removes it.
pub(crate) fn parse_showcase(args: &Value, key: &str) -> anyhow::Result<Option<Option<u64>>> {
    args.get(key)
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let id = value
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or_else(|| anyhow::anyhow!("{key} must be a positive artifact ID or null"))?;
            Ok(Some(id))
        })
        .transpose()
}

/// Reference changes are artifact metadata changes, so cached inventories can
/// replace thread_ids. Strictly advance the affected artifact timestamps.
pub(super) fn touch_artifacts(
    c: &Connection,
    old: Option<u64>,
    new: Option<u64>,
) -> anyhow::Result<()> {
    for id in old
        .into_iter()
        .chain(new)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let updated_at = super::artifacts::next_updated_at(c, id)?;
        c.execute(
            "UPDATE artifacts SET updated_at=?2 WHERE id=?1",
            params![id, updated_at],
        )?;
    }
    Ok(())
}

impl Storage {
    /// Owner authority spans the history. Existence/revision/write share a lock.
    pub(crate) async fn update_thread_showcase(
        &self,
        expected_history: &str,
        id: u64,
        artifact_id: Option<u64>,
        expected_revision: u64,
    ) -> anyhow::Result<Thread> {
        let mut guard = self.conn.lock().await;
        let c = guard.transaction()?;
        super::thread_scope::validate_history(&c, expected_history)?;
        let current = threads::get(&c, id)?;
        anyhow::ensure!(
            current.revision == expected_revision,
            "thread changed; reload before updating its showcase"
        );
        if let Some(artifact_id) = artifact_id {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM artifacts WHERE id=?1)",
                [artifact_id],
                |r| r.get(0),
            )?;
            anyhow::ensure!(exists, "Artifact is unavailable");
        }
        c.execute("UPDATE threads SET showcased_artifact_id=?2,updated_at=?3,revision=revision+1 WHERE id=?1", params![id, artifact_id, chrono::Utc::now().to_rfc3339()])?;
        touch_artifacts(&c, current.showcased_artifact_id, artifact_id)?;
        let thread = threads::get(&c, id)?;
        c.commit()?;
        Ok(thread)
    }

    pub(crate) async fn showcase_publication_snapshot(
        &self,
        history: &str,
        ids: &[u64],
    ) -> anyhow::Result<(
        tokio::sync::MutexGuard<'_, Connection>,
        Vec<ArtifactSummary>,
    )> {
        let guard = self.conn.lock().await;
        super::thread_scope::validate_history(&guard, history)?;
        let summaries = ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|id| super::artifacts::summary(&guard, id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((guard, summaries))
    }
}

#[cfg(test)]
#[path = "thread_showcase_tests.rs"]
mod tests;
