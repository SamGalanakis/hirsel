use hirsel_proto::ChatAuthor;

use super::{AgentSessionBootstrap, ToolSuite};

impl ToolSuite {
    pub(crate) async fn prepare_native_worker_session(
        &self,
        thread_id: u64,
        before_message_id: Option<u64>,
        profile_fingerprint: &str,
        tool_names: &[String],
    ) -> anyhow::Result<AgentSessionBootstrap> {
        let state = self
            .storage
            .reconcile_native_worker_profile(thread_id, profile_fingerprint, tool_names)
            .await?;
        if !state.rotated {
            return Ok(AgentSessionBootstrap {
                session_id: state.session_id,
                handoff_seed: None,
            });
        }
        let messages = self
            .storage
            .thread_detail(thread_id, before_message_id, 30)
            .await?
            .messages;
        let mut handoff = String::from(
            "The host rotated this native worker session because its accepted execution profile changed. Continue from this Task's visible conversation; do not assume unfinished side effects from the previous session were applied.\n\n## Recent chat\n",
        );
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
        Ok(AgentSessionBootstrap {
            session_id: state.session_id,
            handoff_seed: Some(handoff),
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
