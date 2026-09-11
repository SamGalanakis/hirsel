use hirsel_proto::ChatAuthor;

use super::{AgentSessionBootstrap, ToolSuite};

impl ToolSuite {
    pub(crate) async fn prepare_native_worker_session(
        &self,
        thread_id: u64,
        current_turn_id: u64,
        profile_fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionBootstrap> {
        let state = self
            .storage
            .reconcile_native_worker_profile(thread_id, profile_fingerprint, tool_names)
            .await?;
        let turn_watermark = self
            .storage
            .native_worker_conversation_turn_watermark(thread_id)
            .await?;
        let unowned_message_watermark = self
            .storage
            .native_worker_unowned_message_watermark(thread_id)
            .await?;
        let conversation = self
            .storage
            .native_worker_conversation(
                thread_id,
                current_turn_id,
                if state.rotated { None } else { turn_watermark },
                if state.rotated {
                    None
                } else {
                    unowned_message_watermark
                },
                30,
            )
            .await?;
        let messages = conversation.messages;
        if !state.rotated && messages.is_empty() {
            return Ok(AgentSessionBootstrap {
                session_id: state.session_id,
                handoff_seed: None,
                unowned_message_watermark: None,
            });
        }
        let mut handoff = if state.rotated {
            String::from(
                "The host rotated this native worker session because its accepted execution profile changed. Continue from this Task's visible conversation; do not assume unfinished side effects from the previous session were applied.\n\n## Recent chat\n",
            )
        } else if turn_watermark.is_none() && unowned_message_watermark.is_none() {
            String::from(
                "This native worker is joining an existing Task. Continue from this Task's visible recent conversation.\n\n## Recent chat\n",
            )
        } else {
            String::from(
                "This Task's conversation advanced outside this native worker session. Continue from the unseen recent messages below.\n\n## Recent chat\n",
            )
        };
        if messages.is_empty() {
            handoff.push_str("(none)\n");
        }
        for message in messages {
            let author = match message.author {
                hirsel_proto::ChatAuthor::Owner => "owner",
                hirsel_proto::ChatAuthor::Agent => "worker",
            };
            handoff.push_str(&format!(
                "- {author}: {}\n",
                indent_continuation_lines(&message.body)
            ));
        }
        if state.rotated {
            let activity = self
                .storage
                .append_thread_activity(
                    thread_id,
                    None,
                    "worker_session_rotated",
                    &serde_json::json!({"session_id":state.session_id}),
                )
                .await?;
            self.publish_thread_activity(activity).await;
        }
        Ok(AgentSessionBootstrap {
            session_id: state.session_id,
            handoff_seed: Some(handoff),
            unowned_message_watermark: conversation.unowned_message_watermark,
        })
    }

    pub(crate) async fn prepare_agent_session(
        &self,
        thread_id: u64,
        tool_surface_fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionBootstrap> {
        let state = self
            .storage
            .reconcile_agent_tool_surface(thread_id, tool_surface_fingerprint, tool_names)
            .await?;
        if !state.rotated {
            return Ok(AgentSessionBootstrap {
                session_id: state.session_id,
                handoff_seed: None,
                unowned_message_watermark: None,
            });
        }

        let handoff_seed = self
            .session_handoff_seed(thread_id, &state.added_tools)
            .await?;
        self.emit_session_rotated(thread_id, &state.session_id, &state.added_tools)
            .await?;
        Ok(AgentSessionBootstrap {
            session_id: state.session_id,
            handoff_seed: Some(handoff_seed),
            unowned_message_watermark: None,
        })
    }

    async fn session_handoff_seed(
        &self,
        thread_id: u64,
        added_tools: &[String],
    ) -> anyhow::Result<String> {
        let messages = self
            .storage
            .thread_detail(thread_id, None, 30)
            .await?
            .messages;
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
        seed.push_str("\nThis is this Thread's own conversation only. Use threads.context for its accepted assignment and scoped identity.\n");
        Ok(seed)
    }

    async fn emit_session_rotated(
        &self,
        thread_id: u64,
        session_id: &str,
        added_tools: &[String],
    ) -> anyhow::Result<()> {
        let activity = self
            .storage
            .append_thread_activity(
                thread_id,
                None,
                "session_rotated",
                &serde_json::json!({"session_id":session_id,"added_tools":added_tools}),
            )
            .await?;
        self.publish_thread_activity(activity).await;
        Ok(())
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
