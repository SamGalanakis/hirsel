//! Plumbing shared by the CLI-backed drivers: the event fan-out hub, summary
//! formatting, JSON line writing, and process-group lifecycle helpers.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use async_stream::stream;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, Command},
    sync::broadcast,
};

use crate::types::{
    DriverError, DriverResult, EventStream, SessionHandle, SubagentEvent, TerminalOutcome,
};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> DriverResult<MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| DriverError::StatePoisoned)
}

pub(crate) struct SessionRegistry<S> {
    sessions: Mutex<HashMap<String, Arc<S>>>,
}

impl<S> Default for SessionRegistry<S> {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl<S> SessionRegistry<S> {
    pub(crate) fn insert(&self, id: String, session: Arc<S>) -> DriverResult<()> {
        lock(&self.sessions)?.insert(id, session);
        Ok(())
    }

    pub(crate) fn get(&self, handle: &SessionHandle) -> DriverResult<Arc<S>> {
        lock(&self.sessions)?
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| DriverError::SessionNotFound(handle.id.clone()))
    }

    pub(crate) fn remove(&self, handle: &SessionHandle) -> DriverResult<Option<Arc<S>>> {
        Ok(lock(&self.sessions)?.remove(&handle.id))
    }
}

pub(crate) struct EventHub {
    tx: broadcast::Sender<()>,
    state: Mutex<EventState>,
}

#[derive(Default)]
struct EventState {
    events: Vec<SubagentEvent>,
    has_assistant_output: bool,
}

impl EventState {
    fn is_terminal(&self) -> bool {
        matches!(self.events.last(), Some(SubagentEvent::Terminal { .. }))
    }

    fn push(&mut self, event: SubagentEvent) {
        if self.is_terminal() {
            return;
        }
        if let SubagentEvent::AssistantOutput { text } = &event {
            if self.has_assistant_output || text.is_empty() {
                return;
            }
            self.has_assistant_output = true;
        }
        self.events.push(event);
    }
}

impl EventHub {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            tx,
            state: Mutex::new(EventState::default()),
        })
    }

    pub(crate) fn emit(&self, event: SubagentEvent) -> DriverResult<()> {
        lock(&self.state)?.push(event);
        let _ = self.tx.send(());
        Ok(())
    }

    /// Output and its terminal event win or lose a cancellation race together.
    pub(crate) fn complete(
        &self,
        outcome: TerminalOutcome,
        assistant_output: Option<String>,
    ) -> DriverResult<()> {
        let mut state = lock(&self.state)?;
        if let Some(text) = assistant_output {
            state.push(SubagentEvent::AssistantOutput { text });
        }
        state.push(SubagentEvent::Terminal { outcome });
        let _ = self.tx.send(());
        Ok(())
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.is_terminal())
    }

    pub(crate) fn stream(self: &Arc<Self>) -> DriverResult<EventStream> {
        let (hub, mut rx) = {
            let _state = lock(&self.state)?;
            let rx = self.tx.subscribe();
            (Arc::clone(self), rx)
        };
        Ok(Box::pin(stream! {
            let mut cursor = 0;
            loop {
                // Notifications are only wakeups. The retained log is authoritative,
                // so a slow subscriber cannot lose final output to broadcast lag.
                let pending = match lock(&hub.state) {
                    Ok(state) => state.events[cursor..].to_vec(),
                    Err(_) => return,
                };
                for event in pending {
                    cursor += 1;
                    let terminal = matches!(event, SubagentEvent::Terminal { .. });
                    yield event;
                    if terminal { return; }
                }
                match rx.recv().await {
                    Ok(()) => {},
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }))
    }

    pub(crate) async fn wait_terminal(self: &Arc<Self>) -> DriverResult<()> {
        use futures_util::StreamExt;
        let mut events = self.stream()?;
        while let Some(event) = events.next().await {
            if matches!(event, SubagentEvent::Terminal { .. }) {
                return Ok(());
            }
        }
        Err(DriverError::SessionClosed)
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

/// Keep the CLI's existing provider authentication, but never expose the host's
/// owner credentials or execution configuration through inherited Hirsel vars.
pub(crate) fn sanitize_hirsel_environment(command: &mut Command) {
    let names = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.as_std().get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| name.as_encoded_bytes().starts_with(b"HIRSEL_"))
        .collect::<Vec<_>>();
    for name in names {
        command.env_remove(name);
    }
}

pub(crate) fn start_in_process_group(command: &mut Command) {
    sanitize_hirsel_environment(command);
    command.kill_on_drop(true);
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

pub(crate) struct ProcessGroup(i32);

impl ProcessGroup {
    pub(crate) fn new(pgid: i32) -> Self {
        Self(pgid)
    }

    pub(crate) fn kill_group(&self) {
        if self.0 <= 0 {
            return;
        }
        // Best-effort cleanup for externally spawned CLIs.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill_group();
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
