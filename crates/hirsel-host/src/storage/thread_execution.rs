//! Accepted execution settings contain no credentials and are immutable per turn.
use super::{Storage, thread_scope};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ThreadExecution {
    /// Hirsel's own session: the full Thread tool set plus the four coding
    /// operations, on one roster provider and one model. `cwd` is the working
    /// directory those coding operations are rooted at — execution context,
    /// never a filesystem sandbox — so it is captured, not public.
    Native {
        provider_id: String,
        model: lash::ModelSpec,
        cwd: PathBuf,
    },
    Cli {
        agent: hirsel_drivers::AgentKind,
        model: String,
        variant: String,
        cwd: PathBuf,
    },
}
/// The stored preference as the public, key-free identity of a backend.
/// Absent means the Thread inherits the configured default Native execution.
pub(super) fn preference(
    c: &Connection,
    thread_id: u64,
) -> anyhow::Result<Option<hirsel_proto::ThreadExecutionTarget>> {
    let stored: Option<String> = c
        .query_row(
            "SELECT config FROM thread_execution_preferences WHERE thread_id=?1",
            [thread_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(stored
        .map(|s| serde_json::from_str::<ThreadExecution>(&s))
        .transpose()?
        .map(public_target))
}

pub(crate) fn public_target(execution: ThreadExecution) -> hirsel_proto::ThreadExecutionTarget {
    match execution {
        ThreadExecution::Native {
            provider_id, model, ..
        } => hirsel_proto::ThreadExecutionTarget::Native {
            provider_id,
            model: model.id,
        },
        ThreadExecution::Cli {
            agent,
            model,
            variant,
            ..
        } => hirsel_proto::ThreadExecutionTarget::Cli {
            agent: match agent {
                hirsel_drivers::AgentKind::Claude => "claude".to_string(),
                hirsel_drivers::AgentKind::Codex => "codex".to_string(),
            },
            model,
            variant,
        },
    }
}

impl Storage {
    /// Check the revision and write the Owner's backend choice. It applies to
    /// the NEXT turn: a running turn already captured what it runs on.
    pub(crate) async fn set_addressed_thread_execution(
        &self,
        expected_history: &str,
        id: u64,
        execution: Option<&ThreadExecution>,
        expected_revision: u64,
    ) -> anyhow::Result<hirsel_proto::Thread> {
        let c = self.conn.lock().await;
        thread_scope::validate_history(&c, expected_history)?;
        let current = super::threads::get(&c, id)?;
        anyhow::ensure!(
            current.revision == expected_revision,
            "thread changed; reload before updating where it runs"
        );
        match execution {
            Some(execution) => {
                c.execute("INSERT INTO thread_execution_preferences(thread_id,config) VALUES(?1,?2) ON CONFLICT(thread_id) DO UPDATE SET config=excluded.config",params![id,serde_json::to_string(execution)?])?;
            }
            None => {
                c.execute(
                    "DELETE FROM thread_execution_preferences WHERE thread_id=?1",
                    [id],
                )?;
            }
        }
        c.execute(
            "UPDATE threads SET updated_at=?2,revision=revision+1 WHERE id=?1",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        super::threads::get(&c, id)
    }
}

pub(super) fn capture(
    c: &Connection,
    thread_id: u64,
    turn_id: u64,
    explicit: Option<&ThreadExecution>,
) -> anyhow::Result<()> {
    let selected = select(c, thread_id, explicit)?;
    capture_selected(c, turn_id, selected.as_ref())
}

pub(super) fn capture_selected(
    c: &Connection,
    turn_id: u64,
    selected: Option<&ThreadExecution>,
) -> anyhow::Result<()> {
    if let Some(execution) = selected {
        c.execute(
            "INSERT INTO thread_turn_execution(turn_id,config) VALUES(?1,?2)",
            params![turn_id, serde_json::to_string(&execution)?],
        )?;
    }
    Ok(())
}

pub(super) fn select(
    c: &Connection,
    thread_id: u64,
    explicit: Option<&ThreadExecution>,
) -> anyhow::Result<Option<ThreadExecution>> {
    Ok(if let Some(execution) = explicit {
        Some(execution.clone())
    } else {
        let preferred: Option<String> = c
            .query_row(
                "SELECT config FROM thread_execution_preferences WHERE thread_id=?1",
                [thread_id],
                |r| r.get(0),
            )
            .optional()?;
        let preferred = preferred
            .map(|s| serde_json::from_str::<ThreadExecution>(&s))
            .transpose()?;
        match preferred {
            // Every stored backend is the Thread's own, Native included: a
            // Native preference names the provider and model this Thread's own
            // session runs on, not the Settings default.
            Some(preferred) => Some(preferred),
            None => c
                .query_row(
                    "SELECT value FROM meta WHERE key='native_execution_default'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
        }
    })
}
impl Storage {
    pub(crate) async fn set_native_execution_default(
        &self,
        execution: &ThreadExecution,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(execution, ThreadExecution::Native { .. }),
            "the default execution must name a Native provider"
        );
        self.conn.lock().await.execute("INSERT INTO meta(key,value) VALUES('native_execution_default',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(execution)?])?;
        Ok(())
    }
    pub(crate) async fn turn_execution(&self, turn_id: u64) -> anyhow::Result<ThreadExecution> {
        let value: String = self.conn.lock().await.query_row(
            "SELECT config FROM thread_turn_execution WHERE turn_id=?1",
            [turn_id],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&value)?)
    }

    pub(crate) async fn native_execution_default(&self) -> anyhow::Result<ThreadExecution> {
        let value: String = self.conn.lock().await.query_row(
            "SELECT value FROM meta WHERE key='native_execution_default'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&value)?)
    }
}
