use std::path::PathBuf;

use tokio::time::Duration;

use super::{ShellRunOutput, ToolSuite};
use crate::process_run::run_bash_command;

impl ToolSuite {
    pub async fn shell_run(
        &self,
        cmd: String,
        cwd: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<ShellRunOutput> {
        let duration = Duration::from_secs(timeout_secs.unwrap_or(30).min(600));
        let output = run_bash_command(cmd, cwd, duration).await?;
        Ok(ShellRunOutput {
            status: output.status,
            stdout: truncate_output(String::from_utf8_lossy(&output.stdout)),
            stderr: if output.timed_out {
                "command timed out".to_string()
            } else {
                truncate_output(String::from_utf8_lossy(&output.stderr))
            },
            timed_out: output.timed_out,
        })
    }
}

fn truncate_output(output: impl AsRef<str>) -> String {
    const MAX_BYTES: usize = 16 * 1024;
    let output = output.as_ref();
    if output.len() <= MAX_BYTES {
        return output.to_string();
    }
    let mut truncated = output
        .char_indices()
        .take_while(|(idx, _)| *idx < MAX_BYTES)
        .map(|(_, ch)| ch)
        .collect::<String>();
    truncated.push_str("\n[truncated]");
    truncated
}
