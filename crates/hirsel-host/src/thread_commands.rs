//! Addressed commands: owning conversation is never inferred from citations.
use crate::{
    AppState, OwnerSubmission,
    lash_runtime::{OwnerTurn, ThreadActionContext},
    validate_empty_lifecycle_data, validate_snooze_lifecycle_data,
};
use hirsel_proto::{HostToClient, SendMode, Thread};
impl AppState {
    /// Deduplicate before reading local skills: a retry must still acknowledge
    /// its accepted message after the invoked skill has been edited or removed.
    pub(crate) async fn owner_input_body(
        &self,
        client_id: &str,
        body: &str,
        generated_action: bool,
    ) -> anyhow::Result<String> {
        if generated_action
            || self
                .storage
                .message_id_for_client_id(client_id)
                .await?
                .is_some()
        {
            return Ok(body.to_owned());
        }
        self.prompts.expand_skill(body)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn submit_thread_message(
        &self,
        client_id: String,
        thread_id: u64,
        body: String,
        attachments: Vec<String>,
        mentions: Vec<u64>,
        mode: SendMode,
        artifact_ids: Vec<u64>,
    ) -> anyhow::Result<OwnerSubmission> {
        let expected_history = self.storage.history_id().await?;
        self.submit_addressed_turn(
            &expected_history,
            client_id,
            thread_id,
            body,
            attachments,
            mentions,
            mode,
            None,
            artifact_ids,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn submit_addressed_turn(
        &self,
        expected_history: &str,
        client_id: String,
        thread_id: u64,
        body: String,
        attachments: Vec<String>,
        mentions: Vec<u64>,
        mode: SendMode,
        thread_action: Option<ThreadActionContext>,
        artifact_ids: Vec<u64>,
    ) -> anyhow::Result<OwnerSubmission> {
        self.agent.readiness()?;
        let agent_body = self
            .owner_input_body(&client_id, &body, thread_action.is_some())
            .await?;
        let request =
            serde_json::json!({"mode":mode,"thread_action":thread_action,"body":agent_body});
        let (message, inserted) = self
            .storage
            .append_thread_owner_request(
                expected_history,
                thread_id,
                &client_id,
                body,
                &attachments,
                &mentions,
                &artifact_ids,
                &request,
            )
            .await?;
        if inserted {
            if thread_action.is_some() {
                let thread = self
                    .storage
                    .thread(thread_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("unknown thread: {thread_id}"))?;
                self.broadcast(HostToClient::ThreadUpsert { thread });
            }
            let payload = self
                .storage
                .thread_request(&client_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("accepted request is no longer available"))?;
            let turn: OwnerTurn = serde_json::from_value(payload)?;
            if let Err(error) = self.agent.enqueue(turn).await {
                // The durable request and message remain together for recovery.
                tracing::warn!(%error, message_id=message.id,"thread request persisted; admission will recover on restart");
                self.tools.publish_thread_message(message.clone()).await;
                return Err(error);
            }
            self.tools.publish_thread_message(message.clone()).await;
        }
        Ok(OwnerSubmission {
            client_id,
            message,
            inserted,
        })
    }
    pub async fn handle_thread_action(
        &self,
        id: u64,
        action: String,
        data: serde_json::Value,
        expected_revision: Option<u64>,
    ) -> anyhow::Result<Thread> {
        let expected_history = self.storage.history_id().await?;
        let current = self
            .storage
            .thread(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("unknown thread: {id}"))?;
        let thread = match action.as_str() {
            "set_icon" => {
                let object = data
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("set_icon data must be an object"))?;
                anyhow::ensure!(
                    object.keys().all(|key| key == "icon"),
                    "set_icon accepts only icon"
                );
                let icon = crate::storage::parse_icon(&data)?;
                let revision = expected_revision
                    .ok_or_else(|| anyhow::anyhow!("expected_revision is required for set_icon"))?;
                self.storage
                    .update_thread_icon(id, icon.as_ref().map(|value| value.as_deref()), revision)
                    .await?
            }

            "set_showcase" => {
                let object = data
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("set_showcase data must be an object"))?;
                anyhow::ensure!(
                    object.len() == 2
                        && object.contains_key("artifact_id")
                        && object.contains_key("history_id"),
                    "set_showcase requires artifact_id and history_id"
                );
                let history = data["history_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("history_id must be a string"))?;
                let artifact_id =
                    crate::storage::parse_showcase(&data, "artifact_id")?.expect("required field");
                let revision = expected_revision.ok_or_else(|| {
                    anyhow::anyhow!("expected_revision is required for set_showcase")
                })?;
                self.storage
                    .update_thread_showcase(history, id, artifact_id, revision)
                    .await?
            }

            "pin" | "unpin" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.pin_thread(id, action == "pin").await?
            }
            "settle" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.settle_thread(id, true).await?
            }
            "reopen" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.settle_thread(id, false).await?
            }
            "read" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.mark_thread_read(id).await?
            }
            "archive" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.archive_thread(id, true).await?
            }
            "unarchive" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.archive_thread(id, false).await?
            }
            "snooze" => {
                let until = validate_snooze_lifecycle_data(&data)?;
                self.storage.snooze_thread(id, Some(until)).await?
            }
            "unsnooze" => {
                validate_empty_lifecycle_data(&action, &data)?;
                self.storage.snooze_thread(id, None).await?
            }
            generated => {
                anyhow::ensure!(
                    expected_revision == Some(current.revision),
                    "instrument changed; reload the thread before submitting this action"
                );
                anyhow::ensure!(
                    current.settled_at.is_none()
                        && current.archived_at.is_none()
                        && current
                            .snoozed_until
                            .is_none_or(|until| until <= chrono::Utc::now()),
                    "only an active thread accepts instrument actions"
                );
                let validated = crate::thread_instrument::validate_action(
                    &current.instrument,
                    generated,
                    &data,
                )?;
                let body = validated
                    .choice_label
                    .as_deref()
                    .or_else(|| data.get("label").and_then(serde_json::Value::as_str))
                    .unwrap_or(generated)
                    .to_string();
                self.submit_addressed_turn(
                    &expected_history,
                    format!("thread-action-{id}-{}-{generated}", current.revision),
                    id,
                    body,
                    Vec::new(),
                    Vec::new(),
                    SendMode::Send,
                    Some(ThreadActionContext {
                        thread: current.clone().into(),
                        action: action.clone(),
                        data,
                    }),
                    Vec::new(),
                )
                .await?;
                if validated.settles {
                    self.storage.settle_thread(id, true).await?
                } else {
                    self.storage
                        .thread(id)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("unknown thread: {id}"))?
                }
            }
        };
        if action == "set_showcase" {
            let ids = current
                .showcased_artifact_id
                .into_iter()
                .chain(thread.showcased_artifact_id)
                .collect::<Vec<_>>();
            self.tools
                .publish_showcase_artifacts(&expected_history, &ids)
                .await?;
        }
        self.tools.publish_thread(thread.clone()).await;
        Ok(thread)
    }
}

#[cfg(test)]
mod tests {
    use hirsel_proto::ThreadAttention;
    use serde_json::json;
    #[tokio::test]
    async fn generated_continue_preserves_identity_and_rejects_stale_instrument() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::build_state(crate::tests::test_config(dir.path()))
            .await
            .unwrap();
        let instrument = json!({"type":"card","children":[{"type":"submit","action":"advance","label":"Continue","settles":false}]});
        let (thread, _) = state
            .storage
            .create_thread(
                "create",
                "Groceries",
                "",
                &instrument,
                ThreadAttention::NeedsOwner,
                None,
            )
            .await
            .unwrap();
        assert!(
            state
                .handle_thread_action(thread.id, "advance".into(), json!({}), None)
                .await
                .is_err()
        );
        let updated = state
            .storage
            .update_thread(thread.id, None, Some("Updated"), None, None)
            .await
            .unwrap();
        assert!(
            state
                .handle_thread_action(
                    thread.id,
                    "advance".into(),
                    json!({}),
                    Some(thread.revision)
                )
                .await
                .is_err()
        );
        assert!(
            state
                .storage
                .thread_detail(thread.id, None, 100)
                .await
                .unwrap()
                .messages
                .is_empty()
        );
        let continued = state
            .handle_thread_action(
                thread.id,
                "advance".into(),
                json!({}),
                Some(updated.revision),
            )
            .await
            .unwrap();
        assert_eq!(continued.id, thread.id);
        assert!(continued.settled_at.is_none());
        let detail = state
            .storage
            .thread_detail(thread.id, None, 100)
            .await
            .unwrap();
        assert!(
            detail
                .messages
                .iter()
                .any(|m| m.author == hirsel_proto::ChatAuthor::Owner && m.thread_id == thread.id)
        );
        assert!(state.storage.thread(0).await.unwrap().is_none());
    }
}
