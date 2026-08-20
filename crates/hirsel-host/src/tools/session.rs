use chrono::Utc;
use hirsel_proto::{
    ChatAuthor, ChatMessage, Event, EventKind, EventSource, EventSourceKind, HostToClient,
    ToolCallSummary,
};

use super::{AgentSessionBootstrap, ToolSuite, info_ui};

impl ToolSuite {
    pub(crate) async fn prepare_agent_session(
        &self,
        tool_surface_fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionBootstrap> {
        let state = self
            .storage
            .reconcile_agent_tool_surface(tool_surface_fingerprint, tool_names)
            .await?;
        if !state.rotated {
            return Ok(AgentSessionBootstrap {
                session_id: state.session_id,
                handoff_seed: None,
            });
        }

        let handoff_seed = self.session_handoff_seed(&state.added_tools).await?;
        self.emit_session_rotated(&state.session_id, &state.added_tools)
            .await?;
        Ok(AgentSessionBootstrap {
            session_id: state.session_id,
            handoff_seed: Some(handoff_seed),
        })
    }

    async fn session_handoff_seed(&self, added_tools: &[String]) -> anyhow::Result<String> {
        let messages = self.storage.recent_chat(30).await?;
        let events = self
            .storage
            .all_pings()
            .await?
            .into_iter()
            .filter(|event| crate::storage::Storage::is_live(event, Utc::now()))
            .collect::<Vec<_>>();
        let added_tools = display_added_tools(added_tools);
        let mut seed = format!(
            "Session rotated by the host to pick up new tools: {added_tools}. Prior conversation summary follows.\n\n## Recent chat\n"
        );
        if messages.is_empty() {
            seed.push_str("(none)\n");
        } else {
            for message in messages {
                let author = match message.author {
                    ChatAuthor::Owner => "owner",
                    ChatAuthor::Agent => "agent",
                };
                seed.push_str(&format!(
                    "- {author}: {}\n",
                    indent_continuation_lines(&message.body)
                ));
            }
        }
        seed.push_str("\n## Open events\n");
        if events.is_empty() {
            seed.push_str("(none)\n");
        } else {
            for event in events {
                seed.push_str(&format!(
                    "- [{}] {}: {}\n",
                    event_kind_name(event.kind),
                    event.name,
                    indent_continuation_lines(&event.description)
                ));
            }
        }
        Ok(seed)
    }

    async fn emit_session_rotated(
        &self,
        session_id: &str,
        added_tools: &[String],
    ) -> anyhow::Result<Event> {
        let added_tools = display_added_tools(added_tools);
        let description = format!(
            "Opened {session_id} after the tool surface changed. New tools: {added_tools}."
        );
        let anchor = self
            .storage
            .append_chat(
                ChatAuthor::Agent,
                format!("Host rotated the Agent session to `{session_id}`."),
                None,
            )
            .await?
            .id;
        let event = self
            .storage
            .create_event(
                EventKind::Info,
                EventSource {
                    kind: EventSourceKind::Scheduled,
                    r#ref: Some(session_id.to_string()),
                },
                "session-rotated",
                &description,
                info_ui(&description),
                anchor,
                false,
                Vec::new(),
            )
            .await?;
        self.broadcast(HostToClient::EventUpsert {
            event: event.clone(),
        });
        Ok(event)
    }

    pub async fn restore_subagent_processes_after_restart(&self) -> anyhow::Result<Vec<String>> {
        let restored = self
            .storage
            .restore_subagent_processes_after_restart()
            .await?;
        for record in restored.records {
            self.processes.restore(record.clone())?;
            if matches!(record.status, crate::processes::ProcessStatus::Abandoned) {
                self.broadcast_process_upsert(crate::processes::process_info(&record));
            }
        }
        Ok(restored.abandoned)
    }

    pub async fn chat_send(
        &self,
        body_md: impl Into<String>,
        anchor: Option<u64>,
    ) -> anyhow::Result<ChatMessage> {
        self.chat_send_with_tool_calls(body_md, anchor, Vec::new())
            .await
    }

    pub async fn chat_send_with_tool_calls(
        &self,
        body_md: impl Into<String>,
        anchor: Option<u64>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<ChatMessage> {
        let message = self
            .storage
            .append_chat_with_tool_calls(ChatAuthor::Agent, body_md.into(), anchor, tool_calls)
            .await?;
        self.broadcast(HostToClient::Msg {
            message: message.clone(),
            sc: None,
        });
        Ok(message)
    }
}

fn display_added_tools(added_tools: &[String]) -> String {
    if added_tools.is_empty() {
        "none".to_string()
    } else {
        added_tools.join(", ")
    }
}

fn indent_continuation_lines(value: &str) -> String {
    value.replace('\n', "\n  ")
}

fn event_kind_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Judgment => "judgment",
        EventKind::Info => "info",
        EventKind::Summary => "summary",
    }
}
