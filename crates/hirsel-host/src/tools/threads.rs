use super::ToolSuite;
#[cfg(test)]
use hirsel_proto::{ChatAuthor, ToolCallSummary};
use hirsel_proto::{ChatMessage, HostToClient, Thread, ThreadActivity, ThreadTurn};

impl ToolSuite {
    pub(crate) async fn publish_thread_related(
        &self,
        client_id: Option<String>,
        result: crate::storage::ThreadRelated,
    ) -> anyhow::Result<()> {
        // Agent responses may hide inaccessible targets. The human broadcast is
        // a fresh complete snapshot, read and sent while history cannot reset.
        let (_guard, current) = self
            .storage
            .related_publication_snapshot(&result.history_id, result.thread_id)
            .await?;
        self.broadcast(HostToClient::ThreadUpsert {
            thread: current.thread,
        });
        self.broadcast(HostToClient::ThreadRelatedChanged {
            client_id,
            history_id: current.history_id,
            thread_id: current.thread_id,
            revision: current.revision,
            items: current.related_items,
        });
        Ok(())
    }
    pub(crate) async fn publish_thread_grants(
        &self,
        client_id: Option<String>,
        result: crate::storage::ThreadGrants,
    ) -> anyhow::Result<()> {
        // Reach is Owner-visible state: the broadcast is a fresh complete
        // snapshot read while history cannot reset, never the caller's view.
        let (_guard, current) = self
            .storage
            .grants_publication_snapshot(&result.history_id, result.thread_id)
            .await?;
        self.broadcast(HostToClient::ThreadUpsert {
            thread: current.thread,
        });
        self.broadcast(HostToClient::ThreadGrantsChanged {
            client_id,
            history_id: current.history_id,
            thread_id: current.thread_id,
            revision: current.revision,
            grants: current.grants,
        });
        Ok(())
    }
    pub(crate) async fn publish_showcase_artifacts(
        &self,
        history: &str,
        ids: &[u64],
    ) -> anyhow::Result<()> {
        let (_guard, summaries) = self
            .storage
            .showcase_publication_snapshot(history, ids)
            .await?;
        for artifact in summaries {
            self.publish_artifact(artifact);
        }
        Ok(())
    }
    pub(crate) fn publish_artifact(&self, artifact: hirsel_proto::ArtifactSummary) {
        self.broadcast(HostToClient::ArtifactUpsert { artifact });
    }
    pub(crate) async fn publish_thread_summary(&self, thread_id: u64) {
        match self.storage.current_thread_publication(thread_id).await {
            Ok((guard, publication)) => {
                for ancestor in publication.ancestors().iter().rev() {
                    self.broadcast(HostToClient::ThreadUpsert {
                        thread: ancestor.clone(),
                    });
                }
                self.broadcast(HostToClient::ThreadUpsert {
                    thread: publication.thread().clone(),
                });
                drop(guard);
                self.pushes.enqueue_thread(&publication).await;
            }
            Err(error) => tracing::warn!(thread_id, %error, "cannot refresh Thread summary"),
        }
    }
    pub(crate) async fn publish_thread_message(&self, message: ChatMessage) {
        let thread_id = message.thread_id;
        self.broadcast(HostToClient::Msg { message });
        self.publish_thread_summary(thread_id).await;
    }
    pub(crate) async fn publish_thread(&self, expected_history: &str, thread: Thread) {
        let thread_id = thread.id;
        match self
            .storage
            .checked_thread_publication(expected_history, &thread)
            .await
        {
            Ok((guard, publication)) => {
                for ancestor in publication.ancestors().iter().rev() {
                    self.broadcast(HostToClient::ThreadUpsert {
                        thread: ancestor.clone(),
                    });
                }
                self.broadcast(HostToClient::ThreadUpsert {
                    thread: publication.thread().clone(),
                });
                drop(guard);
                self.pushes.enqueue_thread(&publication).await;
            }
            Err(error) => tracing::warn!(thread_id, %error, "cannot publish stale Thread summary"),
        }
    }
    pub(crate) async fn publish_thread_activity(&self, activity: ThreadActivity) {
        let thread_id = activity.thread_id;
        self.broadcast(HostToClient::ThreadActivity { activity });
        self.publish_thread_summary(thread_id).await;
    }
    pub(crate) async fn publish_thread_turn(&self, turn: ThreadTurn) {
        let thread_id = turn.thread_id;
        let parent = turn.requester_thread_id;
        let turn_id = turn.id;
        let terminal = turn.state.is_terminal();
        self.broadcast(HostToClient::ThreadTurn { turn });
        self.publish_thread_summary(thread_id).await;
        if terminal {
            self.emit_thread_trigger(
                crate::lash_runtime::THREAD_TURN_SOURCE_TYPE,
                crate::lash_runtime::THREAD_TURN_EVENT_TYPE,
                thread_id,
                format!("turn #{turn_id} completed"),
                format!("thread-turn:{turn_id}"),
            )
            .await;
            if parent.is_some() {
                self.emit_thread_trigger(
                    crate::lash_runtime::THREAD_COMPLETED_SOURCE_TYPE,
                    crate::lash_runtime::THREAD_COMPLETED_EVENT_TYPE,
                    thread_id,
                    format!("child turn #{turn_id} completed"),
                    format!("thread-complete:{turn_id}"),
                )
                .await;
            }
        }
        if terminal && let Some(parent) = parent {
            match self.storage.thread_detail(parent, None, 1).await {
                Ok(detail) => {
                    for activity in detail.activities.into_iter().filter(|a| {
                        a.kind == "child_report"
                            && a.data["child_turn_id"].as_u64() == Some(turn_id)
                    }) {
                        self.publish_thread_activity(activity).await;
                    }
                    self.publish_thread_summary(parent).await;
                }
                Err(error) => tracing::warn!(%error,"failed to publish committed child report"),
            }
        }
    }

    #[cfg(test)]
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
