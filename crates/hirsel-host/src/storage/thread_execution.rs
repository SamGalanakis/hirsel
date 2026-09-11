//! Accepted execution settings contain no credentials and are immutable per turn.
use super::{Storage, ThreadCaller, thread_scope};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const NATIVE_CODING_TOOL_PROFILE: &str = "hirsel.native-coding.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ThreadExecution {
    Host {
        provider_id: String,
        model: lash::ModelSpec,
    },
    Cli {
        agent: hirsel_drivers::AgentKind,
        model: String,
        variant: String,
        cwd: PathBuf,
    },
    LashWorker {
        provider: crate::providers::NativeWorkerProviderSnapshot,
        model: String,
        variant: String,
        cwd: PathBuf,
        tool_profile: String,
    },
}
pub(super) fn capture(
    c: &Connection,
    thread_id: u64,
    turn_id: u64,
    explicit: Option<&ThreadExecution>,
) -> anyhow::Result<()> {
    let selected = select(c, thread_id, explicit)?;
    if let Some(execution) = selected {
        c.execute(
            "INSERT INTO thread_turn_execution(turn_id,config) VALUES(?1,?2)",
            params![turn_id, serde_json::to_string(&execution)?],
        )?;
    }
    Ok(())
}

fn select(
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
            Some(
                preferred @ (ThreadExecution::Cli { .. } | ThreadExecution::LashWorker { .. }),
            ) => Some(preferred),
            _ => c
                .query_row(
                    "SELECT value FROM meta WHERE key='host_execution_default'",
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
    /// Resolve a direct child's backend before backend-specific input policy is
    /// applied. The same preference is captured atomically when delegation is
    /// accepted; this read exists to reject unsupported native input first.
    pub(crate) async fn effective_child_execution(
        &self,
        caller: &ThreadCaller,
        child_thread_id: u64,
    ) -> anyhow::Result<ThreadExecution> {
        let c = self.conn.lock().await;
        thread_scope::validate_caller(&c, caller)?;
        let direct_child: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1 AND parent_thread_id=?2)",
            params![child_thread_id, caller.thread_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(direct_child, "dispatch requires a direct child Thread");
        select(&c, child_thread_id, None)?
            .ok_or_else(|| anyhow::anyhow!("child Thread has no executable backend"))
    }

    pub(crate) async fn set_host_execution_default(
        &self,
        execution: &ThreadExecution,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(execution, ThreadExecution::Host { .. }),
            "host default must name a host provider"
        );
        self.conn.lock().await.execute("INSERT INTO meta(key,value) VALUES('host_execution_default',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(execution)?])?;
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

    pub(crate) async fn host_execution_default(&self) -> anyhow::Result<ThreadExecution> {
        let value: String = self.conn.lock().await.query_row(
            "SELECT value FROM meta WHERE key='host_execution_default'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&value)?)
    }
}
