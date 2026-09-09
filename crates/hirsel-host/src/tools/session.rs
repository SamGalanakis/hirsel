use hirsel_proto::{ChatAuthor, ChatMessage, ToolCallSummary};

use super::{AgentSessionBootstrap, ToolSuite};

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
        let threads = self.storage.thread_snapshot().await?;
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
                    "- Thread #{} {author}: {}\n",
                    message.thread_id,
                    indent_continuation_lines(&message.body)
                ));
            }
        }
        seed.push_str("\n## Threads\n");
        for thread in threads {
            seed.push_str(&format!(
                "- #{} {} [{}; attention={:?}]: {}\n",
                thread.id,
                thread.title,
                if thread.settled_at.is_some() {
                    "settled"
                } else {
                    "open"
                },
                thread.attention,
                indent_continuation_lines(&thread.description)
            ));
        }
        seed.push_str("\nEach Thread owns its messages. Use threads.read for exact history. This cross-thread summary is coordination context, not a merged conversation.\n");
        Ok(seed)
    }

    async fn emit_session_rotated(
        &self,
        session_id: &str,
        added_tools: &[String],
    ) -> anyhow::Result<()> {
        let activity = self
            .storage
            .append_thread_activity(
                0,
                None,
                "session_rotated",
                &serde_json::json!({"session_id":session_id,"added_tools":added_tools}),
            )
            .await?;
        self.publish_thread_activity(activity).await;
        Ok(())
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
        self.publish_thread_message(message.clone()).await;
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
