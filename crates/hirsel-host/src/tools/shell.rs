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
        Ok(shell_output(output))
    }
}

pub(crate) fn shell_output(output: crate::process_run::BashCommandOutput) -> ShellRunOutput {
    ShellRunOutput {
        status: output.status,
        stdout: truncate_output(String::from_utf8_lossy(&output.stdout)),
        stderr: truncate_output(String::from_utf8_lossy(&output.stderr)),
        timed_out: output.timed_out,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_out_projection_preserves_stderr_and_serializes_timeout() {
        let projected = shell_output(crate::process_run::BashCommandOutput {
            status: None,
            stdout: b"partial-output".to_vec(),
            stderr: b"diagnostic: child still running".to_vec(),
            timed_out: true,
        });

        assert_eq!(projected.stderr, "diagnostic: child still running");

        let encoded = serde_json::to_value(&projected).unwrap();
        assert_eq!(encoded["stderr"], "diagnostic: child still running");
        assert_eq!(encoded["timed_out"], true);
        assert!(encoded["status"].is_null());
    }
}
