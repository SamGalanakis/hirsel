use std::collections::VecDeque;

use chrono::Utc;
use hirsel_proto::{
    AgentActivityState, Blob, ChatAuthor, ChatMessage, Ping, ProcessInfo, ToolCallSummary,
};

/// Connection state exposed to client UIs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Online,
    #[default]
    Offline,
}

/// A confirmed or optimistic chat row. All fields are owned for FFI conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    pub id: Option<u64>,
    pub author: ChatAuthor,
    pub body: String,
    pub reply_to: Option<u64>,
    pub timestamp: String,
    pub attachments: Vec<Blob>,
    pub tool_calls: Vec<ToolCallSummary>,
    pub client_id: Option<String>,
    pub pending: bool,
}

impl From<ChatMessage> for ChatEntry {
    fn from(message: ChatMessage) -> Self {
        Self {
            id: Some(message.id),
            author: message.author,
            body: message.body,
            reply_to: message.r#ref,
            timestamp: message.ts.to_rfc3339(),
            attachments: message.attachments,
            tool_calls: message.tool_calls,
            client_id: None,
            pending: false,
        }
    }
}

/// Ephemeral main-session activity.
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
    pub pings: Vec<Ping>,
    pub processes: Vec<ProcessInfo>,
    pub agent_activity: AgentActivity,
    pub last_seen_msg_id: Option<u64>,
    /// Host build identity from the last `hello_ok`; `None` until a host that
    /// reports it connects (Settings → About shows "Not reported" then).
    pub host_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingSend {
    pub client_id: String,
    pub body: String,
    pub reply_to: Option<u64>,
    pub mentions: Vec<u64>,
}

pub(crate) struct LocalStore {
    pub connection: ConnectionState,
    pub messages: Vec<ChatEntry>,
    pub pings: Vec<Ping>,
    pub processes: Vec<ProcessInfo>,
    pub agent_activity: AgentActivity,
    pub last_seen_msg_id: Option<u64>,
    pub host_version: Option<String>,
    pub pending_sends: VecDeque<PendingSend>,
}

impl Default for LocalStore {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Offline,
            messages: Vec::new(),
            pings: Vec::new(),
            processes: Vec::new(),
            agent_activity: AgentActivity::default(),
            last_seen_msg_id: None,
            host_version: None,
            pending_sends: VecDeque::new(),
        }
    }
}

impl LocalStore {
    pub fn snapshot(&self) -> ClientSnapshot {
        ClientSnapshot {
            connection: self.connection,
            messages: self.messages.clone(),
            pings: self.pings.clone(),
            processes: self.processes.clone(),
            agent_activity: self.agent_activity.clone(),
            last_seen_msg_id: self.last_seen_msg_id,
            host_version: self.host_version.clone(),
        }
    }

    pub fn add_optimistic_send(&mut self, pending: PendingSend) {
        self.messages.push(ChatEntry {
            id: None,
            author: ChatAuthor::Owner,
            body: pending.body.clone(),
            reply_to: pending.reply_to,
            timestamp: Utc::now().to_rfc3339(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            client_id: Some(pending.client_id.clone()),
            pending: true,
        });
        self.pending_sends.push_back(pending);
    }

    pub fn apply_hello_ok(
        &mut self,
        latest_msg_id: u64,
        messages: Vec<ChatMessage>,
        pings: Vec<Ping>,
        processes: Vec<ProcessInfo>,
        host_version: String,
    ) {
        // An older host that doesn't report its version sends "" — keep it None
        // so the UI can show "Not reported" rather than a blank line.
        if !host_version.is_empty() {
            self.host_version = Some(host_version);
        }
        let known_ids: Vec<u64> = self.messages.iter().filter_map(|entry| entry.id).collect();
        let newly_replayed: Vec<ChatMessage> = messages
            .iter()
            .filter(|message| !known_ids.contains(&message.id))
            .cloned()
            .collect();

        let mut confirmed: Vec<ChatEntry> = self
            .messages
            .iter()
            .filter(|entry| !entry.pending)
            .cloned()
            .collect();
        for message in messages {
            confirmed.retain(|entry| entry.id != Some(message.id));
            confirmed.push(message.into());
        }
        confirmed.sort_by_key(|entry| entry.id);

        for message in newly_replayed {
            if message.author == ChatAuthor::Owner {
                self.reconcile_pending_body(&message.body);
            }
        }
        let pending = self.messages.iter().filter(|entry| entry.pending).cloned();
        confirmed.extend(pending);
        self.messages = confirmed;
        self.pings = pings;
        self.processes = processes;
        self.bump_last_seen(latest_msg_id);
    }

    pub fn apply_message(&mut self, message: ChatMessage) {
        if self
            .messages
            .iter()
            .any(|entry| !entry.pending && entry.id == Some(message.id))
        {
            return;
        }

        self.bump_last_seen(message.id);
        if message.author == ChatAuthor::Owner
            && let Some(index) = self
                .messages
                .iter()
                .position(|entry| entry.pending && entry.body == message.body)
        {
            let client_id = self.messages[index].client_id.clone();
            self.messages[index] = message.into();
            if let Some(client_id) = client_id {
                self.pending_sends
                    .retain(|pending| pending.client_id != client_id);
            }
            return;
        }

        self.messages.push(message.into());
    }

    pub fn upsert_ping(&mut self, ping: Ping) {
        if let Some(existing) = self.pings.iter_mut().find(|item| item.id == ping.id) {
            *existing = ping;
        } else {
            self.pings.push(ping);
        }
    }

    pub fn upsert_process(&mut self, process: ProcessInfo) {
        if let Some(existing) = self.processes.iter_mut().find(|item| item.id == process.id) {
            *existing = process;
        } else {
            self.processes.push(process);
        }
    }

    fn reconcile_pending_body(&mut self, body: &str) {
        let Some(index) = self
            .messages
            .iter()
            .position(|entry| entry.pending && entry.body == body)
        else {
            return;
        };
        if let Some(client_id) = self.messages[index].client_id.as_deref() {
            self.pending_sends
                .retain(|pending| pending.client_id != client_id);
        }
        self.messages.remove(index);
    }

    fn bump_last_seen(&mut self, id: u64) {
        self.last_seen_msg_id = Some(self.last_seen_msg_id.map_or(id, |seen| seen.max(id)));
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn message(id: u64, author: ChatAuthor, body: &str) -> ChatMessage {
        ChatMessage {
            id,
            author,
            body: body.into(),
            r#ref: None,
            ts: Utc.timestamp_opt(id as i64, 0).unwrap(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
        }
    }

    fn pending(client_id: &str, body: &str) -> PendingSend {
        PendingSend {
            client_id: client_id.into(),
            body: body.into(),
            reply_to: None,
            mentions: Vec::new(),
        }
    }

    #[test]
    fn replay_reconciles_only_new_owner_messages() {
        let mut store = LocalStore::default();
        store.apply_message(message(1, ChatAuthor::Owner, "same"));
        store.add_optimistic_send(pending("new", "same"));
        store.apply_hello_ok(
            1,
            vec![message(1, ChatAuthor::Owner, "same")],
            vec![],
            vec![],
            "0.1.0 (test)".to_string(),
        );
        assert_eq!(store.pending_sends.len(), 1);
        assert!(store.messages.last().unwrap().pending);
    }
}
