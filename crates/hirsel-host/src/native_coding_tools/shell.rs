use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use lash_core::{ToolFailure, ToolFailureClass, ToolOutcome, ToolValue};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::{JoinError, JoinSet},
    time::{Instant, sleep_until, timeout_at},
};
use tokio_util::sync::CancellationToken;

const SHELL_PATH: &str = "/bin/sh";
const DEFAULT_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const MAX_TIMEOUT_MS: u64 = 10 * 60 * 1_000;
const MAX_OUTPUT_BYTES: usize = 512_000;
const SPILL_OUTPUT_THRESHOLD: usize = 50 * 1_024;
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const GROUP_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const GROUP_TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Clone)]
pub(super) struct ShellExecutor {
    cwd: PathBuf,
    #[cfg(test)]
    reader_failure_barrier: Option<Arc<tokio::sync::Barrier>>,
    #[cfg(test)]
    fail_pidfd_preflight: bool,
}

impl ShellExecutor {
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            #[cfg(test)]
            reader_failure_barrier: None,
            #[cfg(test)]
            fail_pidfd_preflight: false,
        }
    }

    #[cfg(test)]
    pub(super) fn with_reader_failure_barrier(
        mut self,
        barrier: Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.reader_failure_barrier = Some(barrier);
        self
    }

    #[cfg(test)]
    pub(super) fn with_pidfd_preflight_failure(mut self) -> Self {
        self.fail_pidfd_preflight = true;
        self
    }

    pub(super) async fn execute(
        &self,
        args: ShellArgs,
        attempt_cancellation: CancellationToken,
        shutdown_cancellation: CancellationToken,
    ) -> ToolOutcome {
        #[cfg(target_os = "linux")]
        {
            execute_linux(
                &self.cwd,
                args,
                attempt_cancellation,
                shutdown_cancellation,
                #[cfg(test)]
                self.reader_failure_barrier.clone(),
                #[cfg(test)]
                self.fail_pidfd_preflight,
            )
            .await
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (args, attempt_cancellation, shutdown_cancellation);
            execution_failure(
                "native_shell_unsupported",
                "native worker command execution requires Linux pidfd support",
            )
        }
    }
}

pub(super) struct ShellArgs {
    command: String,
    timeout_ms: u64,
    max_output_tokens: Option<usize>,
}

impl ShellArgs {
    pub(super) fn parse(args: &Value) -> Result<Self, ToolOutcome> {
        let Some(args) = args.as_object() else {
            return Err(invalid_request(
                "invalid_exec_command_arguments",
                "exec_command arguments must be an object",
            ));
        };
        let Some(command) = args.get("cmd").and_then(Value::as_str) else {
            return Err(invalid_request(
                "missing_exec_command",
                "missing required string field `cmd`",
            ));
        };
        if command.is_empty() {
            return Err(invalid_request(
                "empty_exec_command",
                "field `cmd` must not be empty",
            ));
        }

        let timeout_ms = optional_bounded_u64(args, "timeout_ms", DEFAULT_TIMEOUT_MS, 1)
            .and_then(|value| {
                if value <= MAX_TIMEOUT_MS {
                    Ok(value)
                } else {
                    Err(format!(
                        "field `timeout_ms` must be an integer from 1 through {MAX_TIMEOUT_MS}"
                    ))
                }
            })
            .map_err(|message| invalid_request("invalid_exec_command_timeout", message))?;
        let max_output_tokens = match args.get("max_output_tokens") {
            None => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|value| *value >= 1)
                    .ok_or_else(|| {
                        invalid_request(
                            "invalid_exec_command_output_limit",
                            "field `max_output_tokens` must be a positive integer",
                        )
                    })?,
            ),
        };

        Ok(Self {
            command: command.to_string(),
            timeout_ms,
            max_output_tokens,
        })
    }
}

fn optional_bounded_u64(
    args: &Map<String, Value>,
    key: &str,
    default: u64,
    minimum: u64,
) -> Result<u64, String> {
    let Some(value) = args.get(key) else {
        return Ok(default);
    };
    value
        .as_u64()
        .filter(|value| *value >= minimum)
        .ok_or_else(|| format!("field `{key}` must be an integer of at least {minimum}"))
}

#[cfg(target_os = "linux")]
async fn execute_linux(
    cwd: &Path,
    args: ShellArgs,
    attempt_cancellation: CancellationToken,
    shutdown_cancellation: CancellationToken,
    #[cfg(test)] reader_failure_barrier: Option<Arc<tokio::sync::Barrier>>,
    #[cfg(test)] fail_pidfd_preflight: bool,
) -> ToolOutcome {
    if attempt_cancellation.is_cancelled() || shutdown_cancellation.is_cancelled() {
        return ToolOutcome::cancelled("tool call cancelled before command start");
    }

    #[cfg(test)]
    if fail_pidfd_preflight {
        return unsupported_linux_host("injected pidfd preflight failure");
    }
    if let Err(error) = preflight_linux_process_control() {
        return unsupported_linux_host(format!(
            "native worker command execution requires pidfd and readable procfs process-group support: {error}"
        ));
    }
    if attempt_cancellation.is_cancelled() || shutdown_cancellation.is_cancelled() {
        return ToolOutcome::cancelled("tool call cancelled before command start");
    }

    let started = Instant::now();
    let mut command = Command::new(SHELL_PATH);
    command
        .arg("-c")
        .arg(&args.command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: `setsid` is async-signal-safe and touches no Rust-owned state in
    // the post-fork child. It makes the direct shell the session and process-
    // group leader before exec, so its PID is the owned PGID.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return io_failure(
                "spawn_shell_command_failed",
                format!(
                    "failed to spawn command with shell `{SHELL_PATH}` in `{}`: {error}",
                    cwd.display()
                ),
            );
        }
    };
    let Some(pid) = child.id() else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return io_failure(
            "native_shell_pid_missing",
            "spawned native shell did not expose a process id",
        );
    };
    let pidfd = match open_pidfd(pid) {
        Ok(pidfd) => pidfd,
        Err(error) => {
            let cleanup_error = terminate_and_reap_spawn_failure(&mut child, pid).await;
            let cleanup_suffix = cleanup_error
                .map(|cleanup| format!("; cleanup also failed: {cleanup}"))
                .unwrap_or_default();
            return io_failure(
                "native_shell_pidfd_open_failed",
                format!("failed to open pidfd for native shell process: {error}{cleanup_suffix}"),
            );
        }
    };

    let Some(stdout) = child.stdout.take() else {
        let cleanup_error = terminate_and_reap_spawn_failure(&mut child, pid).await;
        return io_failure(
            "native_shell_stdout_missing",
            append_cleanup_error("spawned native shell did not expose stdout", cleanup_error),
        );
    };
    let Some(stderr) = child.stderr.take() else {
        let cleanup_error = terminate_and_reap_spawn_failure(&mut child, pid).await;
        return io_failure(
            "native_shell_stderr_missing",
            append_cleanup_error("spawned native shell did not expose stderr", cleanup_error),
        );
    };

    let buffer = Arc::new(StdMutex::new(OutputBuffer::default()));
    let mut readers = JoinSet::new();
    readers.spawn(read_output(stdout, Arc::clone(&buffer), "stdout"));
    readers.spawn(read_output(stderr, Arc::clone(&buffer), "stderr"));
    #[cfg(test)]
    if let Some(barrier) = reader_failure_barrier {
        readers.spawn(async move {
            barrier.wait().await;
            Err("injected native shell output reader failure".to_string())
        });
    }

    let mut process = OwnedShellProcess::new(child, pid);
    let mut exit_ready = tokio::task::spawn_blocking(move || wait_for_pidfd_exit(pidfd));
    let deadline = started + Duration::from_millis(args.timeout_ms);
    let finish = loop {
        if attempt_cancellation.is_cancelled() || shutdown_cancellation.is_cancelled() {
            break Finish::Cancelled;
        }
        if exit_ready.is_finished() {
            break Finish::Exited((&mut exit_ready).await);
        }
        if Instant::now() >= deadline {
            break Finish::TimedOut;
        }

        tokio::select! {
            biased;
            () = attempt_cancellation.cancelled() => break Finish::Cancelled,
            () = shutdown_cancellation.cancelled() => break Finish::Cancelled,
            result = readers.join_next(), if !readers.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => break Finish::ReaderFailed(error),
                    Some(Err(error)) => break Finish::ReaderFailed(reader_join_error(error)),
                    None => {}
                }
            }
            observed = &mut exit_ready => break Finish::Exited(observed),
            () = sleep_until(deadline) => break Finish::TimedOut,
        }
    };

    let termination_error = process.terminate_group();
    let group_termination_error = if termination_error.is_none() {
        process.wait_for_group_termination().await.err()
    } else {
        None
    };
    let exit_observation = match &finish {
        Finish::Exited(_) => None,
        _ => Some(exit_ready.await),
    };
    let status = process.reap().await;
    let drain_error = drain_readers(&mut readers).await;

    if let Some(error) = termination_error {
        return io_failure("native_shell_group_termination_failed", error);
    }
    if let Some(error) = group_termination_error {
        return io_failure("native_shell_group_termination_unconfirmed", error);
    }
    if let Err(error) = status.as_ref() {
        return io_failure(
            "native_shell_reap_failed",
            format!("failed to reap native shell process: {error}"),
        );
    }
    let (terminal, observed) = match finish {
        Finish::Cancelled => (
            Terminal::Cancelled,
            exit_observation.expect("cancellation retains pidfd observer"),
        ),
        Finish::ReaderFailed(error) => (
            Terminal::ReaderFailed(error),
            exit_observation.expect("reader failure retains pidfd observer"),
        ),
        Finish::Exited(observed) => (Terminal::Exited, observed),
        Finish::TimedOut => (
            Terminal::TimedOut,
            exit_observation.expect("timeout retains pidfd observer"),
        ),
    };
    match observed {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return io_failure(
                "native_shell_pidfd_wait_failed",
                format!("failed to wait for native shell pidfd: {error}"),
            );
        }
        Err(error) => {
            return execution_failure(
                "native_shell_wait_task_failed",
                format!("native shell pidfd wait task failed: {error}"),
            );
        }
    }
    if let Some(error) = drain_error {
        return reader_failure(error);
    }

    match terminal {
        Terminal::Cancelled => ToolOutcome::cancelled("tool call cancelled"),
        Terminal::ReaderFailed(error) => reader_failure(error),
        Terminal::Exited => match render_output(&buffer, args.max_output_tokens) {
            Ok(rendered) => completed_result(
                rendered,
                exit_status_code(status.expect("reap result checked above")),
                started.elapsed(),
            ),
            Err(error) => io_failure("native_shell_output_finalize_failed", error),
        },
        Terminal::TimedOut => match render_output(&buffer, args.max_output_tokens) {
            Ok(rendered) => timeout_result(rendered, started.elapsed(), args.timeout_ms),
            Err(error) => io_failure("native_shell_output_finalize_failed", error),
        },
    }
}

#[cfg(target_os = "linux")]
enum Finish {
    Cancelled,
    ReaderFailed(String),
    Exited(Result<io::Result<()>, JoinError>),
    TimedOut,
}

#[cfg(target_os = "linux")]
enum Terminal {
    Cancelled,
    ReaderFailed(String),
    Exited,
    TimedOut,
}

#[cfg(target_os = "linux")]
struct OwnedShellProcess {
    child: Option<Child>,
    pgid: libc::pid_t,
}

#[cfg(target_os = "linux")]
impl OwnedShellProcess {
    fn new(child: Child, pid: u32) -> Self {
        Self {
            child: Some(child),
            pgid: libc::pid_t::try_from(pid).unwrap_or_default(),
        }
    }

    fn terminate_group(&self) -> Option<String> {
        if self.pgid <= 0 {
            return Some("native shell process id exceeds pid_t".to_string());
        }
        // SAFETY: the unreaped direct shell remains the owner of this PGID;
        // sending signal 0 is not used as identity evidence. A negative PID
        // addresses exactly that still-owned process group.
        let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        if result == 0 {
            return None;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            None
        } else {
            Some(format!(
                "failed to terminate native shell process group {}: {error}",
                self.pgid
            ))
        }
    }

    async fn wait_for_group_termination(&self) -> Result<(), String> {
        let pgid = self.pgid;
        tokio::task::spawn_blocking(move || wait_for_process_group_termination(pgid))
            .await
            .map_err(|error| format!("native shell process-group wait task failed: {error}"))?
            .map_err(|error| {
                format!("native shell process group {pgid} did not become terminal: {error}")
            })
    }

    async fn reap(&mut self) -> io::Result<ExitStatus> {
        let result = self
            .child
            .as_mut()
            .expect("owned shell child exists until reap")
            .wait()
            .await;
        if result.is_ok() {
            self.pgid = 0;
            self.child = None;
        }
        result
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedShellProcess {
    fn drop(&mut self) {
        if self.pgid > 0 {
            // SAFETY: while `child` is retained and unreaped, `pgid` still
            // identifies this owner's process group. Drop is only a bounded
            // emergency signal; normal paths explicitly terminate and reap.
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn open_pidfd(pid: u32) -> io::Result<OwnedFd> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid exceeds pid_t"))?;
    // SAFETY: `pidfd_open` reads only these integer arguments. On success it
    // returns a new close-on-exec descriptor owned by this call.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    let fd = libc::c_int::try_from(fd)
        .map_err(|_| io::Error::other("pidfd_open returned a descriptor outside c_int range"))?;
    // SAFETY: the successful syscall returned one newly owned descriptor and
    // no other Rust owner has been constructed for it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn preflight_linux_process_control() -> io::Result<()> {
    let pid = std::process::id();
    let _pidfd = open_pidfd(pid)?;
    let pgid = unsafe { libc::getpgrp() };
    let members = process_group_members(pgid)?;
    if members.iter().any(|member| member.pid == pid) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "procfs did not report the current process in its process group",
        ))
    }
}

#[cfg(target_os = "linux")]
fn wait_for_process_group_termination(pgid: libc::pid_t) -> io::Result<()> {
    let deadline = std::time::Instant::now() + GROUP_TERMINATION_TIMEOUT;
    loop {
        let live = process_group_members(pgid)?
            .into_iter()
            .filter(|member| !matches!(member.state, 'Z' | 'X' | 'x'))
            .collect::<Vec<_>>();
        if live.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            let states = live
                .iter()
                .map(|member| format!("{}:{}", member.pid, member.state))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("live members remained after SIGKILL: {states}"),
            ));
        }

        // The direct leader remains unreaped for this entire loop, so its PID
        // still anchors this numeric PGID. Reasserting SIGKILL here is safe and
        // closes the scheduling window for members that had not reached their
        // terminal state after the initial group signal.
        let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        std::thread::sleep(GROUP_TERMINATION_POLL_INTERVAL);
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ProcessGroupMember {
    pid: u32,
    state: char,
}

#[cfg(target_os = "linux")]
fn process_group_members(pgid: libc::pid_t) -> io::Result<Vec<ProcessGroupMember>> {
    let mut members = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let Some(state) = fields.next().and_then(|field| field.chars().next()) else {
            continue;
        };
        let _parent_pid = fields.next();
        let Some(member_pgid) = fields
            .next()
            .and_then(|field| field.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if member_pgid == pgid {
            members.push(ProcessGroupMember { pid, state });
        }
    }
    Ok(members)
}

#[cfg(target_os = "linux")]
fn wait_for_pidfd_exit(pidfd: OwnedFd) -> io::Result<()> {
    let mut pollfd = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `pollfd` points to one initialized record that remains valid
        // for the call, and `pidfd` keeps its descriptor open for this loop.
        let result = unsafe { libc::poll(&raw mut pollfd, 1, -1) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn terminate_group(pid: u32) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: this is used only before the direct child is reaped, while
        // its PID remains the PGID created by `setsid`.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(target_os = "linux")]
async fn terminate_and_reap_spawn_failure(child: &mut Child, pid: u32) -> Option<String> {
    let pgid = match libc::pid_t::try_from(pid) {
        Ok(pgid) => pgid,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Some("native shell process id exceeds pid_t".to_string());
        }
    };
    terminate_group(pid);
    let group_error = tokio::task::spawn_blocking(move || wait_for_process_group_termination(pgid))
        .await
        .map_err(|error| format!("process-group wait task failed: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .err();
    let reap_error = child.wait().await.err().map(|error| error.to_string());
    match (group_error, reap_error) {
        (None, None) => None,
        (Some(group), None) => Some(group),
        (None, Some(reap)) => Some(format!("failed to reap shell: {reap}")),
        (Some(group), Some(reap)) => Some(format!("{group}; failed to reap shell: {reap}")),
    }
}

fn append_cleanup_error(message: &str, cleanup_error: Option<String>) -> String {
    cleanup_error
        .map(|cleanup| format!("{message}; cleanup also failed: {cleanup}"))
        .unwrap_or_else(|| message.to_string())
}

#[cfg(target_os = "linux")]
fn unsupported_linux_host(message: impl Into<String>) -> ToolOutcome {
    execution_failure("native_shell_unsupported", message)
}

async fn read_output<R>(
    mut reader: R,
    buffer: Arc<StdMutex<OutputBuffer>>,
    stream: &'static str,
) -> Result<(), String>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut chunk = [0_u8; 4_096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => return Ok(()),
            Ok(read) => buffer
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .append(&chunk[..read])
                .map_err(|error| format!("failed to capture native shell {stream}: {error}"))?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("failed to read native shell {stream}: {error}")),
        }
    }
}

async fn drain_readers(readers: &mut JoinSet<Result<(), String>>) -> Option<String> {
    let deadline = Instant::now() + READER_DRAIN_TIMEOUT;
    let mut first_error = None;
    while !readers.is_empty() {
        match timeout_at(deadline, readers.join_next()).await {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => {
                first_error.get_or_insert(error);
            }
            Ok(Some(Err(error))) => {
                first_error.get_or_insert_with(|| reader_join_error(error));
            }
            Ok(None) => break,
            Err(_) => {
                readers.abort_all();
                while readers.join_next().await.is_some() {}
                first_error.get_or_insert_with(|| {
                    "native shell output readers did not drain after process termination"
                        .to_string()
                });
                break;
            }
        };
    }
    first_error
}

fn reader_join_error(error: JoinError) -> String {
    format!("native shell output reader task failed: {error}")
}

#[derive(Default)]
struct OutputBuffer {
    bytes: Vec<u8>,
    start_offset: usize,
    spill: Option<Spill>,
}

impl OutputBuffer {
    fn append(&mut self, chunk: &[u8]) -> io::Result<()> {
        if self.spill.is_none()
            && self.bytes.len().saturating_add(chunk.len()) > SPILL_OUTPUT_THRESHOLD
        {
            self.spill = Some(Spill::create(&self.bytes)?);
        }
        if let Some(spill) = &mut self.spill {
            spill.file.write_all(chunk)?;
        }

        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > MAX_OUTPUT_BYTES {
            let discarded = self.bytes.len() - MAX_OUTPUT_BYTES;
            self.bytes.drain(..discarded);
            self.start_offset = self.start_offset.saturating_add(discarded);
        }
        Ok(())
    }

    fn render(&mut self, max_output_tokens: Option<usize>) -> Result<RenderedOutput, String> {
        let mut output = String::from_utf8_lossy(&self.bytes).to_string();
        if self.start_offset > 0 {
            append_truncation_marker(&mut output);
        }
        output = clean_terminal_output(&output);
        let original_token_count = max_output_tokens.map(|_| estimate_token_count(&output));
        let mut token_truncated = false;
        if let Some(limit) = max_output_tokens {
            let maximum_chars = limit.saturating_mul(4);
            if output.chars().count() > maximum_chars {
                output = output.chars().take(maximum_chars).collect();
                append_truncation_marker(&mut output);
                token_truncated = true;
            }
        }

        if token_truncated && self.spill.is_none() {
            self.spill = Some(Spill::create(&self.bytes).map_err(|error| error.to_string())?);
        }
        let full_output_path = if let Some(spill) = &mut self.spill {
            spill.file.flush().map_err(|error| error.to_string())?;
            spill.keep = true;
            Some(spill.path.clone())
        } else {
            None
        };
        Ok(RenderedOutput {
            output,
            original_token_count,
            full_output_path,
        })
    }
}

struct Spill {
    file: File,
    path: PathBuf,
    keep: bool,
}

impl Spill {
    fn create(existing: &[u8]) -> io::Result<Self> {
        let directory = std::env::temp_dir().join("hirsel-native-tool-output");
        fs::create_dir_all(&directory)?;
        let temporary = tempfile::Builder::new()
            .prefix("exec_command-")
            .suffix(".log")
            .tempfile_in(directory)?;
        let (mut file, path) = temporary.keep().map_err(|error| error.error)?;
        if let Err(error) = file.write_all(existing) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self {
            file,
            path,
            keep: false,
        })
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct RenderedOutput {
    output: String,
    original_token_count: Option<usize>,
    full_output_path: Option<PathBuf>,
}

fn render_output(
    buffer: &Arc<StdMutex<OutputBuffer>>,
    max_output_tokens: Option<usize>,
) -> Result<RenderedOutput, String> {
    buffer
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .render(max_output_tokens)
}

fn append_truncation_marker(output: &mut String) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str("[truncated]");
}

fn clean_terminal_output(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut previous_was_escape = false;
                    for next in chars.by_ref() {
                        if next == '\x07' || (previous_was_escape && next == '\\') {
                            break;
                        }
                        previous_was_escape = next == '\x1b';
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        match character {
            '\r' => {
                if !matches!(chars.peek(), Some('\n')) {
                    output.push('\n');
                }
            }
            '\x08' => {
                output.pop();
            }
            character if character.is_control() && character != '\n' && character != '\t' => {}
            character => output.push(character),
        }
    }
    output
}

fn estimate_token_count(output: &str) -> usize {
    output.chars().count().div_ceil(4)
}

fn completed_result(output: RenderedOutput, exit_code: i32, wall_time: Duration) -> ToolOutcome {
    ToolOutcome::ok(output_record(
        output,
        "completed",
        Some(exit_code),
        wall_time,
    ))
}

fn timeout_result(output: RenderedOutput, wall_time: Duration, timeout_ms: u64) -> ToolOutcome {
    let message = format!("Command timed out after {timeout_ms} ms");
    let mut record = output_record(output, "timed_out", None, wall_time);
    if let Some(record) = record.as_object_mut() {
        record.insert("timed_out".to_string(), Value::Bool(true));
        record.insert("error".to_string(), Value::String(message.clone()));
    }
    failure_with_raw("shell_timeout", message, record)
}

fn output_record(
    output: RenderedOutput,
    status: &str,
    exit_code: Option<i32>,
    wall_time: Duration,
) -> Value {
    let mut record = Map::new();
    record.insert("output".to_string(), Value::String(output.output));
    record.insert("status".to_string(), Value::String(status.to_string()));
    record.insert("done".to_string(), Value::Bool(true));
    record.insert("running".to_string(), Value::Bool(false));
    record.insert(
        "wall_time_seconds".to_string(),
        json!(wall_time.as_secs_f64()),
    );
    if let Some(exit_code) = exit_code {
        record.insert("exit_code".to_string(), json!(exit_code));
    }
    if let Some(token_count) = output.original_token_count {
        record.insert("original_token_count".to_string(), json!(token_count));
    }
    if let Some(path) = output.full_output_path {
        record.insert(
            "full_output_path".to_string(),
            Value::String(path.to_string_lossy().into_owned()),
        );
    }
    Value::Object(record)
}

fn exit_status_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

fn invalid_request(code: &'static str, message: impl Into<String>) -> ToolOutcome {
    ToolOutcome::failure(ToolFailure::invalid_request(code, message))
}

fn io_failure(code: &'static str, message: impl Into<String>) -> ToolOutcome {
    ToolOutcome::failure(ToolFailure::io(code, message))
}

fn execution_failure(code: &'static str, message: impl Into<String>) -> ToolOutcome {
    ToolOutcome::failure(ToolFailure::tool(
        ToolFailureClass::Execution,
        code,
        message,
    ))
}

fn failure_with_raw(code: &'static str, message: impl Into<String>, raw: Value) -> ToolOutcome {
    let mut failure = ToolFailure::tool(ToolFailureClass::Execution, code, message);
    failure.raw = Some(ToolValue::untrusted_json(raw));
    ToolOutcome::failure(failure)
}

fn reader_failure(message: impl Into<String>) -> ToolOutcome {
    failure_with_raw("shell_reader_died", message, json!({ "reader_died": true }))
}
