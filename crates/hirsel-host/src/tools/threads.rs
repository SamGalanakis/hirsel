use super::ToolSuite;
use hirsel_proto::{
    ChatAuthor, ChatMessage, HostToClient, Thread, ThreadActivity, ThreadTurn, ToolCallSummary,
};

impl ToolSuite {
    pub(crate) fn publish_artifact(&self, artifact: hirsel_proto::ArtifactSummary) {
        self.broadcast(HostToClient::ArtifactUpsert { artifact });
    }
    pub(crate) async fn publish_thread_summary(&self, thread_id: u64) {
        match self.storage.thread(thread_id).await {
            Ok(Some(thread)) => self.publish_thread(thread),
            Ok(None) => tracing::warn!(thread_id, "cannot publish missing Thread summary"),
            Err(error) => tracing::warn!(thread_id, %error, "cannot refresh Thread summary"),
        }
    }
    pub(crate) async fn publish_thread_message(&self, message: ChatMessage) {
        let thread_id = message.thread_id;
        self.broadcast(HostToClient::Msg { message, sc: None });
        self.publish_thread_summary(thread_id).await;
    }
    pub(crate) fn publish_thread(&self, thread: Thread) {
        self.broadcast(HostToClient::ThreadUpsert { thread });
    }
    pub(crate) async fn publish_thread_activity(&self, activity: ThreadActivity) {
        let thread_id = activity.thread_id;
        self.broadcast(HostToClient::ThreadActivity { activity });
        self.publish_thread_summary(thread_id).await;
    }
    pub(crate) async fn publish_thread_turn(&self, turn: ThreadTurn) {
        let thread_id = turn.thread_id;
        self.broadcast(HostToClient::ThreadTurn { turn });
        self.publish_thread_summary(thread_id).await;
    }

    pub(crate) async fn thread_chat_send(
        &self,
        thread_id: u64,
        body: String,
        anchor: Option<u64>,
        tool_calls: Vec<ToolCallSummary>,
    ) -> anyhow::Result<ChatMessage> {
        let message = self
            .storage
            .append_thread_chat(thread_id, ChatAuthor::Agent, body, anchor, tool_calls)
            .await?;
        self.publish_thread_message(message.clone()).await;
        Ok(message)
    }
}

#[cfg(test)]
#[path = "thread_summary_tests.rs"]
mod thread_summary_tests;
