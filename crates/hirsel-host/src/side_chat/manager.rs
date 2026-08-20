use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use hirsel_proto::{
    AgentActivityState, ChatAuthor, ChatMessage, Event, EventKind, HostToClient, SendMode,
    SideChatSummary, TurnEventKind,
};
use lash::{PromptLayerSink, TurnInput};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use super::{
    backend::{SideChatBackend, SideSessionCompatibility},
    fork_tool::fork_decide_provider,
    projection::{event_context_text, render_context_block, turn_result_text},
    session::SideChatSession,
    sink::ScopedTurnSink,
};
use crate::{AppState, BroadcastLog, storage::Storage};

const SIDE_CHAT_CONTEXT_MESSAGES: u64 = 20;
const CONCLUSION_INSTRUCTION: &str = "Draft the Owner's reply to this Ping now, based on this conversation so far. Reply with ONLY the reply text the Owner should send — no preamble, no meta-commentary, just the words that should be posted as the Owner's message.";

pub struct SideChatManager {
    compatibility: Option<SideSessionCompatibility>,
    reaper_started: AtomicBool,
    sessions: Mutex<HashMap<String, Arc<SideChatSession>>>,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    storage: Storage,
}

#[derive(Debug, Clone, Serialize)]
pub struct SideChatView {
    pub sc: String,
    pub event_id: u64,
    /// Compatibility mirror for the old debug surface.
    pub ping_id: u64,
    pub event: Event,
    pub messages: Vec<ChatMessage>,
}

pub struct SideChatOpenResult {
    pub sc: String,
    pub event: Event,
    pub messages: Vec<ChatMessage>,
    pub resumed: bool,
}

impl SideChatManager {
    pub(crate) fn new(
        compatibility: Option<SideSessionCompatibility>,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
        storage: Storage,
    ) -> Self {
        Self {
            compatibility,
            reaper_started: AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
            broadcaster,
            broadcast_log,
            storage,
        }
    }

    pub async fn open(self: &Arc<Self>, event_id: u64) -> anyhow::Result<SideChatOpenResult> {
        self.open_inner(event_id, false).await
    }

    pub async fn open_legacy_ping(
        self: &Arc<Self>,
        ping_id: u64,
    ) -> anyhow::Result<SideChatOpenResult> {
        self.open_inner(ping_id, true).await
    }

    async fn open_inner(
        self: &Arc<Self>,
        event_id: u64,
        legacy_ping: bool,
    ) -> anyhow::Result<SideChatOpenResult> {
        let compatibility = self.compatibility.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "legacy side sessions are disabled; set HIRSEL_COMPAT_SIDE_SESSIONS=1 to opt in"
            )
        })?;
        self.spawn_reaper_once(compatibility.ttl);
        if let SideChatBackend::Degraded(reason) = &compatibility.backend {
            anyhow::bail!("Agent provider unavailable; cannot open side chat: {reason}");
        }

        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions
            .values()
            .find(|session| session.event_id == event_id && !session.closed.load(Ordering::Acquire))
            .cloned()
        {
            session.touch();
            let transcript = self.storage.side_chat_transcript(&session.sc).await?;
            let event = self
                .storage
                .ping(event_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
            return Ok(SideChatOpenResult {
                sc: session.sc.clone(),
                event,
                messages: transcript,
                resumed: true,
            });
        }

        let event = self
            .storage
            .ping(event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        let anchor = self
            .storage
            .chat_message(event.anchor)
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing anchor message: {}", event.anchor))?;
        let recent = self.storage.recent_chat(SIDE_CHAT_CONTEXT_MESSAGES).await?;
        let context = render_context_block(&event, &anchor, &recent);
        let sc = format!("side:{}", Uuid::new_v4());
        let mut lash_session = match &compatibility.backend {
            SideChatBackend::Lash { core, prompts } => Some(
                core.session(sc.clone())
                    .store(Arc::new(lash::persistence::InMemorySessionStore::new()))
                    .prompt_contribution(lash::prompt::PromptContribution::guidance(
                        "Hirsel Agent",
                        prompts.agent_guidance(),
                    ))
                    .prompt_contribution(lash::prompt::PromptContribution::guidance(
                        "Side Chat Context",
                        context,
                    ))
                    .open()
                    .await
                    .context("open fresh Lash side-chat session")?,
            ),
            SideChatBackend::Scripted => None,
            SideChatBackend::Degraded(_) => unreachable!("degraded backend returned above"),
        };
        if let Some(session) = &mut lash_session
            && matches!(event.kind, EventKind::Judgment)
        {
            session
                .tools()
                .add_provider(Arc::new(fork_decide_provider(
                    Arc::downgrade(self),
                    sc.clone(),
                    event_id,
                )))
                .await
                .context("add fork.decide to side-chat session")?;
        }
        let session = Arc::new(SideChatSession {
            sc: sc.clone(),
            event_id,
            legacy_ping,
            ping_content: event_context_text(&event),
            anchor: event.anchor,
            lash_session: Mutex::new(lash_session),
            turn_lock: Mutex::new(()),
            seq: AtomicU64::new(0),
            active_cancel: StdMutex::new(None),
            last_activity: StdMutex::new(Instant::now()),
            closed: AtomicBool::new(false),
        });
        sessions.insert(sc.clone(), session);
        let event = self
            .storage
            .set_event_fork(event_id, Some(&sc))
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        self.publish(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(SideChatOpenResult {
            sc,
            event,
            messages: Vec::new(),
            resumed: false,
        })
    }

    pub async fn send(
        self: &Arc<Self>,
        sc: &str,
        body: String,
        mentions: Vec<u64>,
    ) -> anyhow::Result<()> {
        let session = self.session(sc).await?;
        if session.closed.load(Ordering::Acquire) {
            anyhow::bail!("side chat is closed: {sc}");
        }
        let mentioned_pings = self.storage.mentioned_pings(&mentions).await?;
        let owner = self
            .storage
            .append_side_chat_message(sc, ChatAuthor::Owner, &body)
            .await?;
        if session.closed.load(Ordering::Acquire) {
            self.storage.delete_side_chat_transcript(sc).await?;
            anyhow::bail!("side chat is closed: {sc}");
        }
        session.touch();
        self.publish(HostToClient::Msg {
            message: owner,
            sc: Some(sc.to_string()),
        });
        let mut agent_input = body.clone();
        crate::lash_runtime::append_mentioned_ping_context(&mut agent_input, &mentioned_pings);

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let backend = &manager
                .compatibility
                .as_ref()
                .expect("a live side session requires opted-in compatibility")
                .backend;
            let result = match backend {
                SideChatBackend::Lash { .. } => {
                    manager
                        .run_lash_reply(Arc::clone(&session), agent_input)
                        .await
                }
                SideChatBackend::Scripted => {
                    manager.run_scripted_reply(Arc::clone(&session), body).await
                }
                SideChatBackend::Degraded(_) => Ok(()),
            };
            if let Err(error) = result {
                tracing::warn!(%error, sc = %session.sc, "side-chat turn failed");
                manager.publish_turn_failure(&session, &error).await;
            }
        });
        Ok(())
    }

    pub async fn cancel(&self, sc: &str) -> anyhow::Result<bool> {
        let session = self.session(sc).await?;
        let cancelled = session.cancel_active_turn();
        session.touch();
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: Some(sc.to_string()),
        });
        Ok(cancelled)
    }

    pub async fn conclude(&self, sc: &str) -> anyhow::Result<String> {
        let session = self.session(sc).await?;
        if !session.legacy_ping {
            anyhow::bail!(
                "event forks conclude automatically when a judgment is chosen; discard this fork to close it without a decision"
            );
        }
        let _turn_guard = session.turn_lock.lock().await;
        if session.closed.load(Ordering::Acquire) {
            anyhow::bail!("side chat is closed: {sc}");
        }
        let backend = &self
            .compatibility
            .as_ref()
            .expect("a live side session requires opted-in compatibility")
            .backend;
        let text = match backend {
            SideChatBackend::Lash { .. } => self
                .execute_lash_turn(&session, CONCLUSION_INSTRUCTION.to_string())
                .await?
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("side-chat conclusion produced no reply text"))?,
            SideChatBackend::Scripted => self.scripted_conclusion(&session).await?,
            SideChatBackend::Degraded(reason) => {
                anyhow::bail!("Agent provider unavailable; cannot conclude side chat: {reason}")
            }
        };
        session.touch();
        self.publish(HostToClient::ConclusionDraft {
            sc: sc.to_string(),
            text: text.clone(),
        });
        Ok(text)
    }

    pub async fn confirm(
        &self,
        sc: &str,
        text: String,
        app_state: &AppState,
    ) -> anyhow::Result<()> {
        let session = self.session(sc).await?;
        if !session.legacy_ping {
            anyhow::bail!("confirm_conclusion is only available for legacy ping side chats");
        }
        app_state
            .submit_owner_message(
                format!("side-conclude:{sc}"),
                text,
                Some(session.anchor),
                Vec::new(),
                Vec::new(),
                SendMode::Send,
            )
            .await?;
        self.close_session(sc).await?;
        self.publish(HostToClient::SideChatClosed { sc: sc.to_string() });
        Ok(())
    }

    /// Resolve a judgment through its live fork. Both client `event_action`
    /// and the side-scoped `fork.decide` tool enter through this path.
    pub async fn decide_event(
        self: &Arc<Self>,
        event_id: u64,
        data: &Value,
    ) -> anyhow::Result<Option<Event>> {
        if self.compatibility.is_none() {
            return Ok(None);
        }
        let session = self
            .sessions
            .lock()
            .await
            .values()
            .find(|session| session.event_id == event_id && !session.closed.load(Ordering::Acquire))
            .cloned();
        let Some(session) = session else {
            return Ok(None);
        };
        let current = self
            .storage
            .ping(event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        if !matches!(current.kind, EventKind::Judgment) {
            anyhow::bail!("choose is only valid for judgment events");
        }
        let choice = data
            .get("choice")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("event choose requires data.choice"))?;
        let choice_label = crate::event_choice_label(&current.ui, choice)
            .ok_or_else(|| anyhow::anyhow!("unknown event choice: {choice}"))?;
        if let Some(note) = data.get("note") {
            note.as_str()
                .ok_or_else(|| anyhow::anyhow!("event note must be a string"))?;
        }
        let rule = data
            .get("record_rule")
            .map(|rule| {
                rule.as_str()
                    .ok_or_else(|| anyhow::anyhow!("event record_rule must be a string"))
            })
            .transpose()?;
        if session.closed.swap(true, Ordering::AcqRel) {
            return Ok(None);
        }
        self.sessions.lock().await.remove(&session.sc);
        if let Some(rule) = rule {
            self.storage
                .record_taste_rule(event_id, Some(choice), rule)
                .await?;
        }

        let anchor = self
            .storage
            .append_chat(
                ChatAuthor::Owner,
                format!("Discussed @{} → {choice_label}", current.name),
                Some(current.anchor),
            )
            .await?;
        let event = self
            .storage
            .resolve_event_fork(event_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown event: {event_id}"))?;
        self.publish(HostToClient::Msg {
            message: anchor,
            sc: None,
        });
        self.publish(HostToClient::EventUpsert {
            event: event.clone(),
        });
        self.detach_decided_session(session).await?;
        Ok(Some(event))
    }

    pub async fn discard(&self, sc: &str) -> anyhow::Result<()> {
        self.close_session(sc).await?;
        self.publish(HostToClient::SideChatClosed { sc: sc.to_string() });
        Ok(())
    }

    pub async fn discard_all(&self) {
        let ids = self
            .sessions
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for sc in ids {
            if let Err(error) = self.discard(&sc).await {
                tracing::warn!(%error, %sc, "failed to discard side chat during reset");
            }
        }
    }

    pub async fn summaries(&self) -> Vec<SideChatSummary> {
        if self.compatibility.is_none() {
            return Vec::new();
        }
        let mut summaries = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|session| !session.closed.load(Ordering::Acquire))
            .map(|session| SideChatSummary {
                sc: session.sc.clone(),
                ping_id: session.event_id,
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.sc.cmp(&right.sc));
        summaries
    }

    pub async fn transcript(&self, sc: &str) -> Option<Vec<ChatMessage>> {
        let session = self.sessions.lock().await.get(sc).cloned()?;
        if session.closed.load(Ordering::Acquire) {
            return None;
        }
        self.storage.side_chat_transcript(sc).await.ok()
    }

    pub async fn views(&self) -> anyhow::Result<Vec<SideChatView>> {
        let mut sessions = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|session| !session.closed.load(Ordering::Acquire))
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.sc.cmp(&right.sc));
        let mut views = Vec::with_capacity(sessions.len());
        for session in sessions {
            let event = self
                .storage
                .ping(session.event_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown event: {}", session.event_id))?;
            views.push(SideChatView {
                sc: session.sc.clone(),
                event_id: session.event_id,
                ping_id: session.event_id,
                event,
                messages: self.storage.side_chat_transcript(&session.sc).await?,
            });
        }
        Ok(views)
    }

    fn spawn_reaper_once(self: &Arc<Self>, ttl: Duration) {
        if self.reaper_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let tick = Duration::from_secs(ttl.as_secs().clamp(1, 60));
            let mut interval = tokio::time::interval(tick);
            interval.tick().await;
            loop {
                interval.tick().await;
                let expired = manager
                    .sessions
                    .lock()
                    .await
                    .values()
                    .filter(|session| session.idle_for() >= ttl)
                    .map(|session| session.sc.clone())
                    .collect::<Vec<_>>();
                for sc in expired {
                    if let Err(error) = manager.discard(&sc).await {
                        tracing::warn!(%error, %sc, "failed to reap expired side chat");
                    }
                }
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn reaper_started(&self) -> bool {
        self.reaper_started.load(Ordering::Acquire)
    }

    async fn session(&self, sc: &str) -> anyhow::Result<Arc<SideChatSession>> {
        self.sessions
            .lock()
            .await
            .get(sc)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown side chat: {sc}"))
    }

    async fn run_lash_reply(
        &self,
        session: Arc<SideChatSession>,
        body: String,
    ) -> anyhow::Result<()> {
        let _turn_guard = session.turn_lock.lock().await;
        if session.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(text) = self.execute_lash_turn(&session, body).await?
            && !session.closed.load(Ordering::Acquire)
            && !text.trim().is_empty()
        {
            self.append_agent_reply(&session, text).await?;
        }
        Ok(())
    }

    async fn execute_lash_turn(
        &self,
        session: &Arc<SideChatSession>,
        input: String,
    ) -> anyhow::Result<Option<String>> {
        session.seq.store(0, Ordering::Release);
        let cancel = lash::CancellationToken::new();
        session.set_active_cancel(Some(cancel.clone()));
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("thinking".to_string()),
            sc: Some(session.sc.clone()),
        });
        let sink = ScopedTurnSink {
            session: Arc::clone(session),
            broadcaster: self.broadcaster.clone(),
            broadcast_log: self.broadcast_log.clone(),
        };
        let result = {
            let lash_session = session.lash_session.lock().await;
            let lash_session = lash_session
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("side-chat Lash session is closed"))?;
            lash_session
                .turn(TurnInput::text(input))
                .cancel(cancel.clone())
                .stream_to(&sink)
                .await
        };
        session.set_active_cancel(None);
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: Some(session.sc.clone()),
        });
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let result = result?;
        Ok(turn_result_text(&result))
    }

    async fn run_scripted_reply(
        &self,
        session: Arc<SideChatSession>,
        body: String,
    ) -> anyhow::Result<()> {
        let _turn_guard = session.turn_lock.lock().await;
        if session.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        session.seq.store(0, Ordering::Release);
        let cancel = lash::CancellationToken::new();
        session.set_active_cancel(Some(cancel.clone()));
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("processing side-chat message".to_string()),
            sc: Some(session.sc.clone()),
        });
        self.publish(HostToClient::TurnEvent {
            seq: 1,
            event: TurnEventKind::Prose {
                text: "I am checking the side-chat context.".to_string(),
            },
            sc: Some(session.sc.clone()),
        });
        let completed = tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(30)) => true,
            () = cancel.cancelled() => false,
        };
        session.set_active_cancel(None);
        if completed && !session.closed.load(Ordering::Acquire) {
            self.append_agent_reply(&session, format!("(side chat) noted: {body}"))
                .await?;
        }
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: Some(session.sc.clone()),
        });
        Ok(())
    }

    async fn scripted_conclusion(&self, session: &Arc<SideChatSession>) -> anyhow::Result<String> {
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("drafting conclusion".to_string()),
            sc: Some(session.sc.clone()),
        });
        let transcript = self.storage.side_chat_transcript(&session.sc).await?;
        let last_owner = transcript
            .iter()
            .rev()
            .find(|message| message.author == ChatAuthor::Owner)
            .map(|message| message.body.as_str())
            .unwrap_or("No additional owner guidance.");
        let draft = format!(
            "Draft reply regarding \"{}\": {last_owner}",
            session.ping_content
        );
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: Some(session.sc.clone()),
        });
        Ok(draft)
    }

    async fn append_agent_reply(
        &self,
        session: &SideChatSession,
        text: String,
    ) -> anyhow::Result<()> {
        let message = self
            .storage
            .append_side_chat_message(&session.sc, ChatAuthor::Agent, &text)
            .await?;
        session.touch();
        self.publish(HostToClient::Msg {
            message,
            sc: Some(session.sc.clone()),
        });
        Ok(())
    }

    async fn publish_turn_failure(&self, session: &SideChatSession, error: &anyhow::Error) {
        if session.closed.load(Ordering::Acquire) {
            return;
        }
        let text = format!("Side chat turn failed: {error}");
        match self
            .storage
            .append_side_chat_message(&session.sc, ChatAuthor::Agent, &text)
            .await
        {
            Ok(message) => self.publish(HostToClient::Msg {
                message,
                sc: Some(session.sc.clone()),
            }),
            Err(storage_error) => {
                tracing::warn!(%storage_error, sc = %session.sc, "failed to persist side-chat turn error");
            }
        }
        self.publish(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
            sc: Some(session.sc.clone()),
        });
    }

    async fn close_session(&self, sc: &str) -> anyhow::Result<()> {
        let session = self
            .sessions
            .lock()
            .await
            .remove(sc)
            .ok_or_else(|| anyhow::anyhow!("unknown side chat: {sc}"))?;
        session.closed.store(true, Ordering::Release);
        session.cancel_active_turn();
        let _turn_guard = session.turn_lock.lock().await;
        let delete_result = self.storage.delete_side_chat_transcript(sc).await;
        if let Some(lash_session) = session.lash_session.lock().await.take()
            && let Err(error) = lash_session.close().await
        {
            tracing::warn!(%error, %sc, "failed to close Lash side-chat session");
        }
        delete_result?;
        if let Some(event) = self.storage.set_event_fork(session.event_id, None).await? {
            self.publish(HostToClient::EventUpsert { event });
        }
        Ok(())
    }

    async fn detach_decided_session(
        self: &Arc<Self>,
        session: Arc<SideChatSession>,
    ) -> anyhow::Result<()> {
        self.storage
            .delete_side_chat_transcript(&session.sc)
            .await?;
        self.publish(HostToClient::SideChatClosed {
            sc: session.sc.clone(),
        });
        tokio::spawn(async move {
            let _turn_guard = session.turn_lock.lock().await;
            if let Some(lash_session) = session.lash_session.lock().await.take()
                && let Err(error) = lash_session.close().await
            {
                tracing::warn!(%error, sc = %session.sc, "failed to close decided fork session");
            }
        });
        Ok(())
    }

    fn publish(&self, event: HostToClient) {
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }
}
