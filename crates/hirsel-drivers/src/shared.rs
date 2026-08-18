//! Plumbing shared by the CLI-backed drivers: the event fan-out hub, summary
//! formatting, JSON line writing, and process-group lifecycle helpers.

use std::sync::{Arc, Mutex, MutexGuard};

use async_stream::stream;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, Command},
    sync::broadcast,
};

use crate::types::{DriverError, DriverResult, EventStream, SubagentEvent};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> DriverResult<MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| DriverError::StatePoisoned)
}

pub(crate) struct EventHub {
    tx: broadcast::Sender<SubagentEvent>,
    events: Mutex<Vec<SubagentEvent>>,
}

impl EventHub {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            tx,
            events: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn emit(&self, event: SubagentEvent) -> DriverResult<()> {
        lock(&self.events)?.push(event.clone());
        let _ = self.tx.send(event);
        Ok(())
    }

    pub(crate) fn stream(self: &Arc<Self>) -> DriverResult<EventStream> {
        let (backlog, rx) = {
            let events = lock(&self.events)?;
            let rx = self.tx.subscribe();
            (events.clone(), rx)
        };
        Ok(Box::pin(stream! {
            for event in backlog {
                yield event;
            }
            let mut rx = rx;
            loop {
                match rx.recv().await {
                    Ok(event) => yield event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }))
    }
}

pub(crate) fn short_line(text: impl AsRef<str>) -> String {
    let compact = text
        .as_ref()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const MAX_CHARS: usize = 240;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

const TERMINAL_MESSAGE_MAX_CHARS: usize = 24_000;
const TERMINAL_MESSAGE_TRUNCATION_MARKER: &str = "…[truncated by hirsel at 24k chars]";

pub(crate) fn terminal_message(text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if text.chars().count() <= TERMINAL_MESSAGE_MAX_CHARS {
        return text.to_string();
    }
    let content_chars = TERMINAL_MESSAGE_MAX_CHARS
        .saturating_sub(TERMINAL_MESSAGE_TRUNCATION_MARKER.chars().count());
    let mut truncated = text.chars().take(content_chars).collect::<String>();
    truncated.push_str(TERMINAL_MESSAGE_TRUNCATION_MARKER);
    truncated
}

pub(crate) async fn write_json_line(stdin: &mut ChildStdin, value: &Value) -> DriverResult<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await?;
    Ok(())
}

pub(crate) fn start_in_process_group(command: &mut Command) {
    // A Sub-agent Driver owns the whole CLI process tree; setsid lets hard cleanup target the group.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub(crate) fn kill_process_group(pgid: i32) {
    if pgid > 0 {
        // Best-effort cleanup for externally spawned CLIs.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

pub(crate) async fn drain_stderr(mut stderr: ChildStderr) {
    let mut buf = [0_u8; 8192];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
}
