//! Accepted execution settings contain no credentials and are immutable per turn.
use super::Storage;
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
    let selected = if let Some(execution) = explicit {
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
    };
    if let Some(execution) = selected {
        c.execute(
            "INSERT INTO thread_turn_execution(turn_id,config) VALUES(?1,?2)",
            params![turn_id, serde_json::to_string(&execution)?],
        )?;
    }
    Ok(())
}
impl Storage {
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
