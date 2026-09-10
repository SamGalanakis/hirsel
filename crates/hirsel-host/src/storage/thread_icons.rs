//! One icon contract for owner edits and execution-scoped agent tools.
use super::{Storage, threads};
use hirsel_proto::Thread;
use rusqlite::params;
use serde_json::Value;

pub(super) fn validate_icon(icon: Option<&str>) -> anyhow::Result<()> {
    if let Some(icon) = icon {
        anyhow::ensure!(
            !icon.trim().is_empty()
                && icon.len() <= 64
                && icon.chars().count() <= 16
                && !icon
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')),
            "icon must be a nonblank emoji or symbol of at most 16 Unicode code points / 64 UTF-8 bytes without controls or line separators; use null to reset"
        );
    }
    Ok(())
}

/// Outer None means omitted; Some(None) explicitly restores the generated avatar.
pub(crate) fn parse_icon(args: &Value) -> anyhow::Result<Option<Option<String>>> {
    args.get("icon")
        .map(|value| {
            let icon = if value.is_null() {
                None
            } else {
                Some(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("icon must be a string or null"))?,
                )
            };
            validate_icon(icon)?;
            Ok(icon.map(str::to_owned))
        })
        .transpose()
}

impl Storage {
    /// Check the revision and write under the same lock as agent mutations.
    pub(crate) async fn update_thread_icon(
        &self,
        id: u64,
        icon: Option<Option<&str>>,
        expected_revision: u64,
    ) -> anyhow::Result<Thread> {
        if let Some(icon) = icon {
            validate_icon(icon)?;
        }
        let c = self.conn.lock().await;
        let current = threads::get(&c, id)?;
        anyhow::ensure!(
            current.revision == expected_revision,
            "thread changed; reload before updating its icon"
        );
        if let Some(icon) = icon {
            c.execute(
                "UPDATE threads SET icon=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
                params![id, icon, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        threads::get(&c, id)
    }
}

#[cfg(test)]
#[path = "thread_icons_tests.rs"]
mod tests;
