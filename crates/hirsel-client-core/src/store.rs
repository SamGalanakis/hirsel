use chrono::Utc;
use hirsel_proto::{
    AgentActivityState, Blob, ChatAuthor, ChatMessage, ProcessInfo, Thread, ThreadActivity,
    ThreadTurn, ThreadTurnState, ToolCallSummary, TurnEventKind,
};
use std::collections::HashSet;

/// Connection state exposed to client UIs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Online,
    #[default]
    Offline,
}

/// A chat row whose state determines which fields can exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    Confirmed(ConfirmedMessage),
    Pending(PendingSend),
}

impl ChatEntry {
    pub fn id(&self) -> Option<u64> {
        match self {
            Self::Confirmed(message) => Some(message.id),
            Self::Pending(_) => None,
        }
    }

    pub fn client_id(&self) -> Option<&str> {
        match self {
            Self::Confirmed(message) => message.client_id.as_deref(),
            Self::Pending(send) => Some(&send.client_id),
        }
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedMessage {
    pub thread_id: u64,
    pub client_id: Option<String>,
    pub mentions: Vec<u64>,
    pub id: u64,
    pub author: ChatAuthor,
    pub body: String,
    pub reply_to: Option<u64>,
    pub timestamp: String,
    pub attachments: Vec<Blob>,
    pub tool_calls: Vec<ToolCallSummary>,
}

impl From<ChatMessage> for ConfirmedMessage {
    fn from(message: ChatMessage) -> Self {
        Self {
            id: message.id,
            thread_id: message.thread_id,
            client_id: message.client_id,
            mentions: message.mentions,
            author: message.author,
            body: message.body,
            reply_to: message.r#ref,
            timestamp: message.ts.to_rfc3339(),
            attachments: message.attachments,
            tool_calls: message.tool_calls,
        }
    }
}

impl From<ChatMessage> for ChatEntry {
    fn from(message: ChatMessage) -> Self {
        Self::Confirmed(message.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSend {
    pub error: Option<String>,
    pub thread_id: u64,
    pub attachments: Vec<String>,
    pub client_id: String,
    pub body: String,
    pub reply_to: Option<u64>,
    pub mentions: Vec<u64>,
    pub timestamp: String,
}

impl PendingSend {
    pub(crate) fn new(
        thread_id: u64,
        attachments: Vec<String>,
        client_id: String,
        body: String,
        reply_to: Option<u64>,
        mentions: Vec<u64>,
    ) -> Self {
        Self {
            error: None,
            thread_id,
            attachments,
            client_id,
            body,
            reply_to,
            mentions,
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

/// Ephemeral activity associated with an addressed durable turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentActivity {
    pub state: AgentActivityState,
    pub text: Option<String>,
}

impl Default for AgentActivity {
    fn default() -> Self {
        Self {
            state: AgentActivityState::Idle,
            text: None,
        }
    }
}

/// Complete state view delivered to observers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSnapshot {
    pub connection: ConnectionState,
    pub messages: Vec<ChatEntry>,
    pub threads: Vec<Thread>,
    pub turns: Vec<ThreadTurn>,
    pub activities: Vec<ThreadActivity>,
    pub streams: Vec<ThreadStream>,
    pub opened_threads: Vec<u64>,
    pub created_threads: Vec<CreatedThread>,
    pub history_has_more: Vec<u64>,
    pub processes: Vec<ProcessInfo>,
    pub last_seen_msg_id: Option<u64>,
    /// Host build identity from the last `hello_ok`; `None` until a host that
    /// reports it connects (Settings → About shows "Not reported" then).
    pub host_version: Option<String>,
}

pub(crate) struct LocalStore {
    removed_message_ids: HashSet<u64>,
    pub connection: ConnectionState,
    pub messages: Vec<ChatEntry>,
    pub threads: Vec<Thread>,
    pub turns: Vec<ThreadTurn>,
    pub activities: Vec<ThreadActivity>,
    pub streams: Vec<ThreadStream>,
    pub opened_threads: Vec<u64>,
    pub created_threads: Vec<CreatedThread>,
    pub requests: Vec<(String, u64)>,
    pub history_has_more: Vec<u64>,
    pub pending_creates: Vec<(String, String)>,
    pub processes: Vec<ProcessInfo>,
    pub last_seen_msg_id: Option<u64>,
    pub host_version: Option<String>,
}

impl Default for LocalStore {
    fn default() -> Self {
        Self {
            removed_message_ids: HashSet::new(),
            connection: ConnectionState::Offline,
            messages: Vec::new(),
            threads: Vec::new(),
            turns: Vec::new(),
            activities: Vec::new(),
            streams: Vec::new(),
            opened_threads: Vec::new(),
            created_threads: Vec::new(),
            requests: Vec::new(),
            history_has_more: Vec::new(),
            pending_creates: Vec::new(),
            processes: Vec::new(),
            last_seen_msg_id: None,
            host_version: None,
        }
    }
}

impl LocalStore {
    pub fn snapshot(&self) -> ClientSnapshot {
        ClientSnapshot {
            connection: self.connection,
            messages: self.messages.clone(),
            threads: self.threads.clone(),
            turns: self.turns.clone(),
            activities: self.activities.clone(),
            streams: self.streams.clone(),
            opened_threads: self.opened_threads.clone(),
            created_threads: self.created_threads.clone(),
            history_has_more: self.history_has_more.clone(),
            processes: self.processes.clone(),
            last_seen_msg_id: self.last_seen_msg_id,
            host_version: self.host_version.clone(),
        }
    }

    pub fn add_optimistic_send(&mut self, pending: PendingSend) {
        self.messages.push(ChatEntry::Pending(pending));
    }

    pub fn pending_sends(&self) -> impl Iterator<Item = &PendingSend> {
        self.messages.iter().filter_map(|entry| match entry {
            ChatEntry::Confirmed(_) => None,
            ChatEntry::Pending(send) if send.error.is_none() => Some(send),
            ChatEntry::Pending(_) => None,
        })
    }

    pub fn apply_hello_ok(
        &mut self,
        latest_msg_id: u64,
        messages: Vec<ChatMessage>,
        threads: Vec<Thread>,
        processes: Vec<ProcessInfo>,
        host_version: String,
    ) {
        // An older host that doesn't report its version sends "" — keep it None
        // so the UI can show "Not reported" rather than a blank line.
        if !host_version.is_empty() {
            self.host_version = Some(host_version);
        }
        for message in messages {
            self.apply_message(message);
        }
        self.messages
            .sort_by_key(|entry| entry.id().unwrap_or(u64::MAX));
        self.threads = threads;
        self.processes = processes;
        self.bump_last_seen(latest_msg_id);
    }

    pub fn apply_message(&mut self, message: ChatMessage) {
        if let Some(client_id) = &message.client_id {
            self.messages.retain(|entry| !matches!(entry, ChatEntry::Pending(send) if send.client_id == *client_id && send.thread_id == message.thread_id));
        }
        if self.removed_message_ids.contains(&message.id) {
            return;
        }
        if self
            .messages
            .iter()
            .any(|entry| entry.id() == Some(message.id))
        {
            return;
        }

        self.bump_last_seen(message.id);
        self.messages.push(message.into());
    }

    pub fn remove_message(&mut self, id: u64) {
        self.removed_message_ids.insert(id);
        self.messages.retain(|message| message.id() != Some(id));
    }

    pub fn upsert_thread(&mut self, thread: Thread) {
        if let Some(existing) = self.threads.iter_mut().find(|item| item.id == thread.id) {
            // Execution and activity projections can change without an instrument revision.
            if thread.revision >= existing.revision {
                *existing = thread;
            }
        } else {
            self.threads.push(thread);
        }
    }

    pub fn apply_detail(&mut self, client_id: &str, detail: hirsel_proto::ThreadDetail) {
        let Some(index) = self
            .requests
            .iter()
            .position(|(id, thread)| id == client_id && *thread == detail.thread.id)
        else {
            return;
        };
        self.requests.remove(index);
        let thread_id = detail.thread.id;
        if !self.opened_threads.contains(&thread_id) {
            self.opened_threads.push(thread_id);
        }
        self.history_has_more.retain(|id| *id != thread_id);
        if detail.has_more {
            self.history_has_more.push(thread_id);
        }
        self.upsert_thread(detail.thread);
        for message in detail
            .messages
            .into_iter()
            .filter(|m| m.thread_id == thread_id)
        {
            self.apply_message(message);
        }
        for turn in detail
            .turns
            .into_iter()
            .filter(|t| t.thread_id == thread_id)
        {
            self.upsert_turn(turn);
        }
        for activity in detail
            .activities
            .into_iter()
            .filter(|a| a.thread_id == thread_id)
        {
            self.upsert_activity(activity);
        }
        self.messages
            .sort_by_key(|entry| entry.id().unwrap_or(u64::MAX));
    }

    pub fn upsert_activity(&mut self, activity: ThreadActivity) {
        if !self.activities.iter().any(|a| a.id == activity.id) {
            self.activities.push(activity);
        }
    }

    pub fn upsert_turn(&mut self, turn: ThreadTurn) {
        if let Some(old) = self.turns.iter_mut().find(|t| t.id == turn.id) {
            if old.finished_at.is_some() {
                return;
            }
            *old = turn.clone();
        } else {
            self.turns.push(turn.clone());
        }
        if let Some(stream) = self
            .streams
            .iter_mut()
            .find(|s| s.thread_id == turn.thread_id && s.turn_id == turn.id)
        {
            stream.finished = !matches!(
                turn.state,
                ThreadTurnState::Queued | ThreadTurnState::Running
            );
        }
    }

    pub fn stream(&mut self, thread_id: u64, turn_id: u64) -> Option<&mut ThreadStream> {
        if self
            .turns
            .iter()
            .any(|t| t.id == turn_id && t.finished_at.is_some())
        {
            return None;
        }
        if let Some(index) = self.streams.iter().position(|s| s.thread_id == thread_id) {
            if self.streams[index].turn_id > turn_id {
                return None;
            }
            if self.streams[index].turn_id < turn_id {
                self.streams[index] = ThreadStream::new(thread_id, turn_id);
            }
            if self.streams[index].finished {
                return None;
            }
            return Some(&mut self.streams[index]);
        }
        self.streams.push(ThreadStream::new(thread_id, turn_id));
        self.streams.last_mut()
    }

    pub fn apply_delta(&mut self, thread_id: u64, turn_id: u64, seq: u64, event: TurnEventKind) {
        if let Some(stream) = self.stream(thread_id, turn_id) {
            if stream.last_seq.is_some_and(|last| seq <= last) {
                return;
            }
            stream.last_seq = Some(seq);
            stream.events.push(event);
        }
    }

    pub fn upsert_process(&mut self, process: ProcessInfo) {
        if let Some(existing) = self.processes.iter_mut().find(|item| item.id == process.id) {
            *existing = process;
        } else {
            self.processes.push(process);
        }
    }

    fn bump_last_seen(&mut self, id: u64) {
        self.last_seen_msg_id = Some(self.last_seen_msg_id.map_or(id, |seen| seen.max(id)));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedThread {
    pub client_id: String,
    pub thread_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadStream {
    pub thread_id: u64,
    pub turn_id: u64,
    pub last_seq: Option<u64>,
    pub events: Vec<TurnEventKind>,
    pub activity: AgentActivity,
    pub finished: bool,
}
impl ThreadStream {
    fn new(thread_id: u64, turn_id: u64) -> Self {
        Self {
            thread_id,
            turn_id,
            last_seq: None,
            events: Vec::new(),
            activity: AgentActivity::default(),
            finished: false,
        }
    }
}
