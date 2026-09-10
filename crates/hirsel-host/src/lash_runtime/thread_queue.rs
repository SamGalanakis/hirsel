//! One admitted input per Thread lane. Lash may coalesce pending
//! inputs, so addressed requests wait durably in Hirsel until the prior turn ends.
use super::*;
use hirsel_proto::ThreadTurnState;

impl LashAgentRuntime {
    pub(super) async fn enqueue_thread_request(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let stored = turn.stored_turn(&self.tools.storage()).await?;
        self.tools
            .storage()
            .save_thread_request(&turn.client_id, &serde_json::to_value(&turn)?)
            .await?;
        self.tools.publish_thread_turn(stored).await;
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn publish_background_acceptance(
        &self,
        client_id: &str,
        turn: hirsel_proto::ThreadTurn,
    ) -> anyhow::Result<()> {
        let activity = self
            .tools
            .storage()
            .append_thread_activity_once(
                &format!("background-request:{client_id}"),
                turn.thread_id,
                Some(turn.id),
                "background_queued",
                &json!({}),
            )
            .await?;
        self.tools.publish_thread_turn(turn).await;
        self.tools.publish_thread_activity(activity).await;
        Ok(())
    }

    pub(super) async fn admit_next_thread_request(&self) -> anyhow::Result<Option<String>> {
        let _request_guard = self.request_lock.lock().await;
        // A dead process's leased inputs may become visible after boot, once
        // its lease expires. Reconcile before each claim, not just at startup.
        self.reconcile_unowned_inputs().await?;
        // An unclaimed background drain retains its own identity while the Lash
        // execution lane is busy. Newly accepted Owner messages wait behind it.
        if self
            .anchors
            .lock()
            .await
            .active
            .as_ref()
            .is_some_and(|a| a.request_id.is_none())
        {
            return Ok(None);
        }
        let Some((client_id, payload)) = self
            .tools
            .storage()
            .pending_thread_requests()
            .await?
            .into_iter()
            .find(|(_, p)| p["thread_id"].as_u64() == Some(self.thread_id))
        else {
            return Ok(None);
        };
        let mut turn: OwnerTurn = serde_json::from_value(payload.clone())?;
        let accepted = turn.stored_turn(&self.tools.storage()).await?;
        if !matches!(
            self.tools.storage().turn_execution(accepted.id).await?,
            crate::storage::ThreadExecution::Host { .. }
        ) {
            return Ok(None);
        }
        let mut detail = self
            .tools
            .storage()
            .thread_detail(turn.thread_id, turn.message_id, 30)
            .await?;
        let queued = if let Some(id) = payload.get("turn_id").and_then(Value::as_u64) {
            detail
                .turns
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing background ThreadTurn {id}"))?
        } else {
            turn.stored_turn(&self.tools.storage()).await?
        };
        if !detail.turns.iter().any(|t| t.id == queued.id) {
            detail.turns.push(queued.clone());
        }
        // Interrupted work is never replayed automatically after a restart.
        if detail
            .turns
            .iter()
            .any(|t| t.id == queued.id && t.finished_at.is_some())
        {
            anyhow::ensure!(
                self.cancel_lash_request(&client_id).await?,
                "interrupted Thread input still holds a live Lash claim"
            );
            self.tools
                .storage()
                .remove_thread_request(&client_id)
                .await?;
            self.anchors.lock().await.active = None;
            self.set_active_turn_id(None).await;
            drop(_request_guard);
            return Box::pin(self.admit_next_thread_request()).await;
        }
        let history = detail
            .messages
            .into_iter()
            .filter(|m| turn.message_id.is_none_or(|id|m.id<id))
            .map(|m| json!({"id":m.id,"author":m.author,"body":m.body,"artifact_ids":m.artifact_ids}))
            .collect::<Vec<_>>();
        let current_artifacts = self
            .tools
            .storage()
            .accepted_message_references(&turn.history_id, queued.id)
            .await?;
        let source_label = if turn.message_id.is_none() {
            "Background wake"
        } else {
            "Owner message"
        };
        turn.body = format!(
            "[Thread #{}: {}]\n[durable conversation context]\n{}\n\n[{source_label}]\n{}",
            turn.thread_id,
            detail.thread.title,
            serde_json::to_string(&history)?,
            turn.body
        );
        turn.body.push_str(&format!(
            "\n[Current accepted message artifact references]\n{}",
            serde_json::to_string(&current_artifacts)?
        ));
        if let Some(message_id) = turn.message_id
            && let Some(message) = self.tools.storage().chat_message(message_id).await?
            && !message.mentions.is_empty()
        {
            turn.body.push_str(&format!("\n[Explicitly referenced Threads: {}. References do not change the owning Thread; use threads.read for their context.]",message.mentions.iter().map(|id|format!("#{id}")).collect::<Vec<_>>().join(", ")));
        }
        let execution = self.tools.storage().turn_execution(queued.id).await?;
        let crate::storage::ThreadExecution::Host { provider_id, model } = execution else {
            anyhow::bail!("CLI turn must run on the CLI lane");
        };
        anyhow::ensure!(
            provider_id == self.provider_id,
            "accepted provider is unavailable on this host"
        );
        if self.session.policy_snapshot().model != model {
            self.session
                .admin()
                .config()
                .update(lash::SessionConfigPatch {
                    model: Some(model),
                    ..Default::default()
                })
                .await?;
        }
        let input = owner_turn_input(&turn).await?;
        let source_key = owner_turn_source_key(&client_id);
        let anchors = TurnAnchors {
            request_id: Some(client_id.clone()),
            thread_id: turn.thread_id,
            thread_turn_id: Some(queued.id),
        };
        // A crash can leave a queued Thread request already accepted by Lash.
        // Reuse that exact input: the Thread title/history may have changed since
        // acceptance, and Lash correctly rejects a changed idempotent payload.
        let accepted = self
            .session
            .pending_turn_inputs()
            .await?
            .iter()
            .any(|input| input.source_key.as_deref() == Some(source_key.as_str()));
        if !accepted {
            self.session
                .enqueue(input)
                .id(client_id.clone())
                .ingress(TurnInputIngress::next_turn())
                .send()
                .await?;
        }
        let stored = self.tools.storage().run_thread_turn(queued.id).await?;
        self.tools.publish_thread_turn(stored).await;
        let drain_id = self.next_drain_id(&anchors);
        self.tools
            .storage()
            .bind_thread_execution(&self.history_id, &self.session_id, &drain_id, queued.id)
            .await?;
        self.anchors.lock().await.active = Some(anchors);
        self.set_active_turn_id(Some(drain_id)).await;
        Ok(Some(client_id))
    }

    pub(super) async fn run_admitted_drain(
        &self,
        drain_id: &str,
    ) -> Result<QueuedTurnDrain<lash::TurnOutput>, lash::EmbedError> {
        for attempt in 0..3 {
            let result = self
                .session
                .queued_turn()
                .turn_id(drain_id.to_owned())
                .run()
                .await;
            if matches!(&result, Err(lash::EmbedError::Runtime(error)) if error.code == lash_core::RuntimeErrorCode::StoreCommitContended)
                && attempt < 2
            {
                // Lash explicitly permits an unchanged retry of this failure.
                // Keep the exact execution ID and owning Thread throughout it.
                tokio::time::sleep(Duration::from_millis(50 * (attempt + 1))).await;
                continue;
            }
            return result;
        }
        unreachable!("the final attempt always returns")
    }

    pub(super) async fn finish_thread_request(
        &self,
        client_id: &str,
        output: Option<&lash::TurnOutput>,
    ) -> anyhow::Result<()> {
        self.finish_active_thread(output).await?;
        // Cancellation or a failed drain may defer an admitted input in Lash.
        // Remove it before dropping Hirsel's durable ownership receipt.
        anyhow::ensure!(
            self.cancel_lash_request(client_id).await?,
            "cannot remove ownership of a still-claimed Lash input"
        );
        self.tools
            .storage()
            .remove_thread_request(client_id)
            .await?;
        Ok(())
    }

    pub(super) async fn finish_active_thread(
        &self,
        output: Option<&lash::TurnOutput>,
    ) -> anyhow::Result<()> {
        let active = self.anchors.lock().await.active.clone();
        if let Some(active) = active {
            let state = match output.map(|o| &o.result.outcome) {
                Some(lash::TurnOutcome::Finished(_)) => ThreadTurnState::Completed,
                Some(lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. })) => {
                    ThreadTurnState::Cancelled
                }
                _ => ThreadTurnState::Failed,
            };
            if let Some(turn_id) = active.thread_turn_id {
                if let Some(output) = output {
                    for tool in tool_call_summaries(output) {
                        let activity = self
                            .tools
                            .storage()
                            .append_thread_activity_once(
                                &format!("turn:{turn_id}:tool:{}", tool.id),
                                active.thread_id,
                                Some(turn_id),
                                "tool_completed",
                                &serde_json::to_value(tool)?,
                            )
                            .await?;
                        self.tools.publish_thread_activity(activity).await;
                    }
                }
                let (stored, message) = self
                    .tools
                    .storage()
                    .complete_thread_turn(
                        &self.history_id,
                        turn_id,
                        state,
                        output.and_then(turn_chat_payload),
                    )
                    .await?;
                if let Some(message) = message {
                    self.tools.publish_thread_message(message).await;
                }
                self.tools.publish_thread_turn(stored).await;
            }
        }
        Ok(())
    }

    async fn cancel_lash_request(&self, client_id: &str) -> anyhow::Result<bool> {
        let receipts = self
            .session
            .cancel_pending_turn_inputs([lash::PendingTurnInputCancelTarget::source_key(
                owner_turn_source_key(client_id),
            )])
            .await?;
        Ok(!receipts.iter().any(|r| {
            matches!(
                &r.outcome,
                lash::PendingTurnInputCancelOutcome::AlreadyClaimed { input, .. }
                    if input.state != lash::persistence::TurnInputState::Accepted
            )
        }))
    }

    /// Old or interrupted requests must never drain as unaddressed coordinator input.
    pub(super) async fn reconcile_unowned_inputs(&self) -> anyhow::Result<()> {
        let sources = self
            .tools
            .storage()
            .pending_thread_requests()
            .await?
            .into_iter()
            .map(|(id, _)| owner_turn_source_key(&id))
            .collect::<HashSet<_>>();
        let targets = self
            .session
            .pending_turn_inputs()
            .await?
            .into_iter()
            .filter(|input| {
                input
                    .source_key
                    .as_ref()
                    .is_none_or(|key| !sources.contains(key))
            })
            .map(|input| lash::PendingTurnInputCancelTarget::input_id(input.input_id));
        self.session.cancel_pending_turn_inputs(targets).await?;
        Ok(())
    }

    pub(super) async fn cancel_owned_turn(&self, thread_id: Option<u64>) -> anyhow::Result<()> {
        // Admission and Stop share this gate so the ownership check and exact
        // cancellation cannot straddle a switch to a different Thread.
        let _request_guard = self.request_lock.lock().await;
        if let Some(thread_id) = thread_id {
            anyhow::ensure!(
                self.anchors
                    .lock()
                    .await
                    .active
                    .as_ref()
                    .is_some_and(|a| a.thread_id == thread_id),
                "Thread #{thread_id} has no running turn"
            );
        }
        let active = self.active_turn_id.lock().await;
        if let Some(id) = active.as_ref() {
            // Exact cancellation also reaches a drain before it enters Lash's
            // process-local registry. Drop prevents undelivered input replay.
            self.session
                .request_turn_cancel_with_disposition(
                    id,
                    format!("owner-stop:{id}"),
                    Some("owner".into()),
                    None,
                    lash_core::facade_support::TurnCancelDisposition::Drop,
                )
                .await?;
        } else if let Some(thread_id) = thread_id {
            anyhow::bail!("Thread #{thread_id} has no running turn");
        }
        Ok(())
    }

    pub(super) async fn cancel_thread_request(
        &self,
        client_id: &str,
    ) -> anyhow::Result<CancelQueuedResult> {
        let _request_guard = self.request_lock.lock().await;
        let active = self.anchors.lock().await.active.clone();
        if let Some(payload) = self.tools.storage().thread_request(client_id).await? {
            {
                let request: OwnerTurn = serde_json::from_value(payload.clone())?;
                let queued_id = if let Some(id) = payload.get("turn_id").and_then(Value::as_u64) {
                    id
                } else {
                    self.tools
                        .storage()
                        .queue_thread_turn(request.thread_id, request.message_id)
                        .await?
                        .id
                };
                if active
                    .as_ref()
                    .is_some_and(|a| a.thread_turn_id == Some(queued_id))
                {
                    return Ok(CancelQueuedResult::AlreadyClaimed);
                }
                return self.remove_cancelled_request(client_id, queued_id).await;
            }
        }
        Ok(CancelQueuedResult::AlreadyClaimed)
    }

    async fn remove_cancelled_request(
        &self,
        client_id: &str,
        turn_id: u64,
    ) -> anyhow::Result<CancelQueuedResult> {
        // Persist the cancellation intent first. If the host dies before Lash
        // acknowledges it, startup sees this terminal turn and cancels its
        // pending input instead of admitting the cancelled request again.
        let turn = self
            .tools
            .storage()
            .finish_thread_turn(turn_id, ThreadTurnState::Cancelled, None)
            .await?;
        self.tools.publish_thread_turn(turn).await;
        anyhow::ensure!(
            self.cancel_lash_request(client_id).await?,
            "cannot remove ownership of a still-claimed Lash input"
        );
        self.tools
            .storage()
            .remove_thread_request(client_id)
            .await?;
        Ok(CancelQueuedResult::Cancelled)
    }
}
