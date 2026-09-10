use std::{path::PathBuf, process::Stdio, time::Duration};

use tokio::{io::AsyncReadExt, process::Command, time::timeout};

#[derive(Debug)]
pub(crate) struct BashCommandOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

pub(crate) async fn run_bash_command(
    cmd: String,
    cwd: Option<PathBuf>,
    duration: Duration,
) -> anyhow::Result<BashCommandOutput> {
    start_bash_command(cmd, cwd)?.finish(duration).await
}

/// Owns only the process group created here. Dropping a cancelled tool future
/// also tears down its children; no host-wide process discovery is involved.
pub(crate) struct RunningBash {
    child: tokio::process::Child,
    pgid: i32,
}
impl Drop for RunningBash {
    fn drop(&mut self) {
        kill_process_group(self.pgid);
    }
}
pub(crate) fn start_bash_command(cmd: String, cwd: Option<PathBuf>) -> anyhow::Result<RunningBash> {
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    start_in_process_group(&mut command);
    let child = command.spawn()?;
    let pgid = child.id().map(|id| id as i32).unwrap_or_default();
    Ok(RunningBash { child, pgid })
}
impl RunningBash {
    pub(crate) async fn finish(mut self, duration: Duration) -> anyhow::Result<BashCommandOutput> {
        let mut stdout = self
            .child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing stdout"))?;
        let mut stderr = self
            .child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing stderr"))?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        // Pipe futures and their buffers belong to this scope. Cancellation
        // stops readers, while timeout retains bytes that were already read.
        let completed = timeout(duration, async {
            tokio::try_join!(
                self.child.wait(),
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err)
            )
        })
        .await;
        match completed {
            Ok(Ok((status, _, _))) => {
                self.pgid = 0;
                Ok(BashCommandOutput {
                    status: status.code(),
                    stdout: out,
                    stderr: err,
                    timed_out: false,
                })
            }
            Ok(Err(error)) => Err(error.into()),
            Err(_) => {
                kill_process_group(self.pgid);
                let _ = timeout(Duration::from_secs(5), self.child.wait()).await;
                self.pgid = 0;
                Ok(BashCommandOutput {
                    status: None,
                    stdout: out,
                    stderr: err,
                    timed_out: true,
                })
            }
        }
    }
}

fn start_in_process_group(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn kill_process_group(pgid: i32) {
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_kills_the_spawned_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("sleep.pid");
        let cmd = format!(
            "printf partial-output; printf partial-error >&2; sleep 999 & echo $! > {}; wait",
            pid_file.display()
        );

        // Generous timeout: the child must reach `echo $! > pidfile` before the
        // timeout fires. A tight 100ms races on a cold/loaded CI runner (bash
        // startup + fork), leaving the pidfile unwritten. 999s sleep still times out.
        let output = run_bash_command(cmd, None, Duration::from_secs(2))
            .await
            .unwrap();

        assert!(output.timed_out);
        assert_eq!(output.stdout, b"partial-output");
        assert_eq!(output.stderr, b"partial-error");
        let pid = tokio::fs::read_to_string(&pid_file)
            .await
            .unwrap()
            .trim()
            .to_string();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !std::process::Command::new("kill")
                .arg("-0")
                .arg(pid)
                .status()
                .unwrap()
                .success(),
            "timed-out shell child survived process-group kill"
        );
    }
}
