use std::{path::PathBuf, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

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
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    start_in_process_group(&mut command);

    let mut child = command.spawn()?;
    let pgid = child.id().map(|id| id as i32).unwrap_or_default();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing child stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing child stderr pipe"))?;
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let (status, timed_out) = match timeout(duration, child.wait()).await {
        Ok(Ok(status)) => (status.code(), false),
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            kill_process_group(pgid);
            let _ = timeout(Duration::from_secs(5), child.wait()).await;
            (None, true)
        }
    };

    Ok(BashCommandOutput {
        status,
        stdout: stdout_task.await??,
        stderr: stderr_task.await??,
        timed_out,
    })
}

async fn read_pipe<R>(mut reader: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    Ok(bytes)
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
        let cmd = format!("sleep 999 & echo $! > {}; wait", pid_file.display());

        // Generous timeout: the child must reach `echo $! > pidfile` before the
        // timeout fires. A tight 100ms races on a cold/loaded CI runner (bash
        // startup + fork), leaving the pidfile unwritten. 999s sleep still times out.
        let output = run_bash_command(cmd, None, Duration::from_secs(2))
            .await
            .unwrap();

        assert!(output.timed_out);
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
