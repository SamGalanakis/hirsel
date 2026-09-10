use chrono::Utc;
use hirsel_proto::{
    AgentActivityState, Blob, ChatAuthor, ChatMessage, ProcessInfo, Thread, ThreadActivity,
    ThreadRelatedItem, ThreadTurn, ThreadTurnState, ToolCallSummary, TurnEventKind,
};
use std::collections::{HashMap, HashSet};

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
    pub artifact_ids: Vec<u64>,
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
            artifact_ids: message.artifact_ids,
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
    pub mentions: Vec<u64>,
    pub artifact_ids: Vec<u64>,
    pub timestamp: String,
}

impl PendingSend {
    pub(crate) fn new(
        thread_id: u64,
        attachments: Vec<String>,
        client_id: String,
        body: String,
        mentions: Vec<u64>,
        artifact_ids: Vec<u64>,
    ) -> Self {
        Self {
            error: None,
            thread_id,
            attachments,
            client_id,
            body,
            mentions,
            artifact_ids,
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
    pub briefs: Vec<ThreadBrief>,
    pub related_items: Vec<ThreadRelatedItem>,
    pub streams: Vec<ThreadStream>,
    pub opened_threads: Vec<u64>,
    pub created_threads: Vec<CreatedThread>,
    pub history_has_more: Vec<u64>,
    pub processes: Vec<ProcessInfo>,
    pub history_id: Option<String>,
    pub recovered_drafts: Vec<String>,
    /// Host build identity from the last `hello_ok`; `None` until a host that
    /// reports it connects (Settings → About shows "Not reported" then).
    pub host_version: Option<String>,
}

pub(crate) struct LocalStore {
    removed_message_ids: HashSet<u64>,
    related_revisions: HashMap<u64, u64>,
    pub connection: ConnectionState,
    pub messages: Vec<ChatEntry>,
    pub threads: Vec<Thread>,
    pub turns: Vec<ThreadTurn>,
    pub activities: Vec<ThreadActivity>,
    pub briefs: Vec<ThreadBrief>,
    pub related_items: Vec<ThreadRelatedItem>,
    pub streams: Vec<ThreadStream>,
    pub opened_threads: Vec<u64>,
    pub created_threads: Vec<CreatedThread>,
    pub requests: Vec<(String, u64)>,
    pub history_has_more: Vec<u64>,
    pub pending_creates: Vec<(String, String, Option<u64>)>,
    pub processes: Vec<ProcessInfo>,
    pub history_id: Option<String>,
    pub recovered_drafts: Vec<String>,
    pub host_version: Option<String>,
}

impl Default for LocalStore {
    fn default() -> Self {
        Self {
            removed_message_ids: HashSet::new(),
            related_revisions: HashMap::new(),
            connection: ConnectionState::Offline,
            messages: Vec::new(),
            threads: Vec::new(),
            turns: Vec::new(),
            activities: Vec::new(),
            briefs: Vec::new(),
            related_items: Vec::new(),
            streams: Vec::new(),
            opened_threads: Vec::new(),
            created_threads: Vec::new(),
            requests: Vec::new(),
            history_has_more: Vec::new(),
            pending_creates: Vec::new(),
            processes: Vec::new(),

            host_version: None,
            history_id: None,
            recovered_drafts: Vec::new(),
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
            briefs: self.briefs.clone(),
            related_items: self.related_items.clone(),
            streams: self.streams.clone(),
            opened_threads: self.opened_threads.clone(),
            created_threads: self.created_threads.clone(),
            history_has_more: self.history_has_more.clone(),
            processes: self.processes.clone(),
            history_id: self.history_id.clone(),
            recovered_drafts: self.recovered_drafts.clone(),
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

    /// A different store invalidates every identity-bound operation. Plain text
    /// is retained separately and can only be submitted by an explicit new send.
    pub fn apply_hello_ok(
        &mut self,
        history_id: String,
        threads: Vec<Thread>,
        processes: Vec<ProcessInfo>,
        host_version: String,
    ) -> bool {
        let changed = self
            .history_id
            .as_ref()
            .is_some_and(|old| old != &history_id);
        if changed {
            let mut drafts = std::mem::take(&mut self.recovered_drafts);
            drafts.extend(self.messages.iter().filter_map(|entry| match entry {
                ChatEntry::Pending(send) => Some(send.body.clone()),
                _ => None,
            }));
            let connection = self.connection;
            *self = Self::default();
            self.connection = connection;
            self.recovered_drafts = drafts;
        }
        self.history_id = Some(history_id);
        self.host_version = Some(host_version);
        self.threads = threads;
        self.processes = processes;
        changed
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
        self.replace_related_items(thread_id, detail.thread.revision, detail.related_items);
        self.briefs.retain(|b| b.thread_id != thread_id);
        self.briefs.push(ThreadBrief {
            thread_id,
            text: detail.brief.text,
            artifact_ids: detail.brief.artifact_ids,
        });
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

    /// Related snapshots order only against prior Related snapshots. Metadata
    /// may already be newer and must neither block these items nor roll back.
    fn replace_related_items(
        &mut self,
        thread_id: u64,
        revision: u64,
        items: Vec<ThreadRelatedItem>,
    ) -> bool {
        if self
            .related_revisions
            .get(&thread_id)
            .is_some_and(|old| *old > revision)
            || items.iter().any(|link| link.thread_id != thread_id)
        {
            return false;
        }
        self.related_revisions.insert(thread_id, revision);
        self.related_items
            .retain(|link| link.thread_id != thread_id);
        self.related_items.extend(items);
        true
    }

    pub fn apply_thread_related(
        &mut self,
        history_id: &str,
        thread_id: u64,
        revision: u64,
        items: Vec<ThreadRelatedItem>,
    ) -> bool {
        if self.history_id.as_deref() != Some(history_id) {
            return false;
        }
        self.replace_related_items(thread_id, revision, items)
    }

    pub fn refresh_open_thread(&mut self, thread_id: u64) -> Option<hirsel_proto::ClientToHost> {
        if !self.opened_threads.contains(&thread_id) {
            return None;
        }
        let client_id = uuid::Uuid::new_v4().to_string();
        self.requests.push((client_id.clone(), thread_id));
        Some(hirsel_proto::ClientToHost::OpenThread {
            client_id,
            thread_id,
            before_id: None,
        })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadBrief {
    pub thread_id: u64,
    pub text: String,
    pub artifact_ids: Vec<u64>,
}
