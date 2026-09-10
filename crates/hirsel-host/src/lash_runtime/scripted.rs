use super::*;

pub(super) struct ScriptedAgentRuntime {
    pub(super) tasks: RuntimeTasks,
    pub(super) thread_id: u64,
    pub(super) capacity: Arc<tokio::sync::Semaphore>,
    pub(super) config: RuntimeConfig,
    pub(super) tools: ToolSuite,
    pub(super) broadcaster: broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
    pub(super) state: Arc<Mutex<ScriptedQueueState>>,
    pub(super) notify: Arc<Notify>,
}

#[derive(Default)]
pub(super) struct ScriptedQueueState {
    pub(super) queue: VecDeque<OwnerTurn>,
    pub(super) active: Option<ScriptedActiveTurn>,
}

pub(super) struct ScriptedActiveTurn {
    pub(super) turn_id: Option<u64>,
    pub(super) thread_id: u64,
    pub(super) cancel: lash::CancellationToken,
}

impl ScriptedAgentRuntime {
    pub(super) async fn enqueue(&self, mut turn: OwnerTurn) -> anyhow::Result<()> {
        #[cfg(test)]
        if turn.body == "__hirsel_test_enqueue_error__" {
            anyhow::bail!("scripted enqueue failed for test");
        }
        self.tools
            .storage()
            .save_thread_request(&turn.client_id, &serde_json::to_value(&turn)?)
            .await?;
        let queued = turn.stored_turn(&self.tools.storage()).await?;
        turn.turn_id = Some(queued.id);
        self.tools.publish_thread_turn(queued).await;
        self.state.lock().await.queue.push_back(turn);
        self.notify.notify_one();
        Ok(())
    }

    pub(super) async fn cancel_turn(&self) -> anyhow::Result<()> {
        if let Some(cancel) = self
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.cancel.clone())
        {
            cancel.cancel();
        }

        Ok(())
    }

    pub(super) async fn cancel_queued(
        &self,
        client_id: &str,
    ) -> anyhow::Result<CancelQueuedResult> {
        let mut state = self.state.lock().await;
        if let Some(position) = state
            .queue
            .iter()
            .position(|turn| turn.client_id == client_id)
        {
            let turn = state.queue.remove(position).expect("position exists");
            drop(state);
            let record = turn.stored_turn(&self.tools.storage()).await?;
            let record = self
                .tools
                .storage()
                .finish_thread_turn(record.id, hirsel_proto::ThreadTurnState::Cancelled, None)
                .await?;
            self.tools.publish_thread_turn(record).await;
            self.tools
                .storage()
                .remove_thread_request(&turn.client_id)
                .await?;
            return Ok(CancelQueuedResult::Cancelled);
        }
        Ok(CancelQueuedResult::AlreadyClaimed)
    }

    pub(super) async fn deliver_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        self.tools
            .thread_chat_send(self.thread_id, text, None, Vec::new())
            .await?;

        Ok(())
    }

    pub(super) fn spawn_standalone_monitor(self: &Arc<Self>, monitor_id: String) {
        let runtime = Arc::clone(self);
        self.tasks.spawn(async move {
            loop {
                let record = match runtime.tools.monitor(&monitor_id).await {
                    Ok(Some(record)) if record.cancelled_ts.is_none() => record,
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor lookup failed");
                        break;
                    }
                };
                tokio::time::sleep(Duration::from_secs(record.every_secs)).await;
                let record = match runtime.tools.monitor(&monitor_id).await {
                    Ok(Some(record)) if record.cancelled_ts.is_none() => record,
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor lookup failed");
                        break;
                    }
                };
                let tick = run_monitor_tick(&record).await;
                match runtime
                    .tools
                    .record_monitor_tick(&monitor_id, tick.probe.output.clone(), tick.summary)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor tick persist failed");
                        continue;
                    }
                }
                if tick.wake
                    && let Some(text) = tick.wake_text
                    && let Err(error) = runtime.deliver_monitor_wake(text).await
                {
                    tracing::warn!(%error, monitor_id = %monitor_id, "scripted standalone monitor wake delivery failed");
                }
            }
        });
    }

    pub(super) async fn recover_pending(&self) -> anyhow::Result<()> {
        match self.tools.storage().pending_thread_requests().await {
            Ok(requests) => {
                for (client_id, payload) in requests
                    .into_iter()
                    .filter(|(_, p)| p["thread_id"].as_u64() == Some(self.thread_id))
                {
                    let Ok(turn) = serde_json::from_value::<OwnerTurn>(payload) else {
                        continue;
                    };
                    let Ok(record) = turn.stored_turn(&self.tools.storage()).await else {
                        continue;
                    };
                    if !matches!(
                        self.tools.storage().turn_execution(record.id).await?,
                        crate::storage::ThreadExecution::Host { .. }
                    ) {
                        continue;
                    }
                    if record.finished_at.is_some() {
                        let _ = self.tools.storage().remove_thread_request(&client_id).await;
                        continue;
                    }
                    let mut state = self.state.lock().await;
                    if state
                        .active
                        .as_ref()
                        .is_some_and(|a| a.turn_id == Some(record.id))
                    {
                        continue;
                    }
                    if !state
                        .queue
                        .iter()
                        .any(|existing| existing.client_id == client_id)
                    {
                        state.queue.push_back(turn);
                    }
                }
            }
            Err(error) => tracing::warn!(%error,"failed to restore queued Thread turns"),
        }
        Ok(())
    }
    pub(super) async fn run(self: Arc<Self>) {
        tracing::info!(
            model = %self.config.model,
            data_dir = %self.config.data_dir.display(),
            "Scripted Agent test double opened session agent"
        );
        if let Err(error) = self.recover_pending().await {
            tracing::warn!(%error,"Thread queue recovery failed");
        }
        let mut snooze_tick = tokio::time::interval(SNOOZE_TICK_INTERVAL);
        snooze_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let (turn, cancel) = loop {
                if let Some(next) = self.claim_next_turn().await {
                    break next;
                }
                tokio::select! {
                    _ = self.notify.notified() => {}
                    _ = snooze_tick.tick() => {
                        if let Err(error) = self.tools.return_expired_snoozes().await {
                            tracing::warn!(%error, "scripted snoozed event return poll failed");
                        }
                    }
                }
            };
            let Ok(_capacity) = self.capacity.acquire().await else {
                return;
            };
            if let Err(error) = self.handle_turn(turn, cancel.clone()).await {
                tracing::error!(%error, "scripted Agent turn failed");
            }
            self.clear_active_turn(&cancel).await;
        }
    }

    pub(super) async fn claim_next_turn(&self) -> Option<(OwnerTurn, lash::CancellationToken)> {
        let mut state = self.state.lock().await;
        let turn = state.queue.pop_front()?;
        let cancel = lash::CancellationToken::new();
        state.active = Some(ScriptedActiveTurn {
            turn_id: turn.turn_id,
            thread_id: turn.thread_id,
            cancel: cancel.clone(),
        });
        Some((turn, cancel))
    }

    pub(super) async fn clear_active_turn(&self, _cancel: &lash::CancellationToken) {
        self.state.lock().await.active = None;
    }

    pub(super) async fn handle_turn(
        &self,
        turn: OwnerTurn,
        cancel: lash::CancellationToken,
    ) -> anyhow::Result<()> {
        let record = turn.stored_turn(&self.tools.storage()).await?;
        let record = self.tools.storage().run_thread_turn(record.id).await?;
        self.tools.publish_thread_turn(record.clone()).await;
        if let Some(active) = self.state.lock().await.active.as_mut() {
            active.turn_id = Some(record.id);
        }
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: record.id,
                thread_id: turn.thread_id,
                state: AgentActivityState::Thinking,
                text: Some("processing owner message".into()),
            },
        );
        let result = self.handle_turn_inner(&turn, &cancel).await;
        let state = if cancel.is_cancelled() {
            hirsel_proto::ThreadTurnState::Cancelled
        } else if result.is_ok() {
            hirsel_proto::ThreadTurnState::Completed
        } else {
            hirsel_proto::ThreadTurnState::Failed
        };
        let (record, message) = self
            .tools
            .storage()
            .complete_thread_turn(
                &turn.history_id,
                record.id,
                state,
                result
                    .as_ref()
                    .ok()
                    .and_then(|text| text.clone())
                    .map(|text| (text, vec![])),
            )
            .await?;
        if let Some(message) = message {
            self.tools.publish_thread_message(message).await;
        }
        self.tools.publish_thread_turn(record.clone()).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: record.id,
                thread_id: turn.thread_id,
                state: AgentActivityState::Idle,
                text: None,
            },
        );
        result.map(|_| ())
    }

    pub(super) async fn handle_turn_inner(
        &self,
        turn: &OwnerTurn,
        cancel: &lash::CancellationToken,
    ) -> anyhow::Result<Option<String>> {
        if let Some(duration) = slow_turn_duration(&turn.body)?
            && !sleep_until_done_or_cancelled(duration, cancel).await
        {
            return Ok(None);
        }
        if cancel.is_cancelled() {
            return Ok(None);
        }
        self.emit_scripted_timeline(turn.thread_id).await;
        let turn_text = owner_turn_text(turn, &self.tools.storage());
        let lower = turn_text.to_lowercase();
        if self.config.driver_mode == DriverMode::Fake && lower.contains("delegate") {
            let turn_id = turn
                .turn_id
                .ok_or_else(|| anyhow::anyhow!("accepted turn missing"))?;
            let launch = uuid::Uuid::new_v4().to_string();
            let caller = self
                .tools
                .storage()
                .bind_thread_execution(&turn.history_id, &launch, &launch, turn_id)
                .await?;
            let facade = ScopedThreadTools {
                tools: self.tools.clone(),
                caller,
                operation_id: format!("scripted:{turn_id}:delegate"),
            };
            facade.execute("threads_delegate",&json!({"title":"Repository fix","brief":"Make the trivial repo fix and report back.","artifact_ids":[],"agent":"claude","cwd":std::env::current_dir()?})).await.map_err(anyhow::Error::msg)?;
        }
        if let Some(context) = &turn.thread_action {
            let label = context
                .data
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(&context.action);
            let instrument = json!({"type":"card","children":[{"type":"heading","level":2,"text":format!("{} advanced", context.thread.title)},{"type":"text","text":format!("Received action: {label}")}]});
            let thread = self
                .tools
                .storage()
                .update_thread(
                    context.thread.id,
                    None,
                    Some(&format!("Advanced after {label}")),
                    Some(&instrument),
                    Some(hirsel_proto::ThreadAttention::Quiet),
                )
                .await?;
            self.tools.publish_thread(&turn.history_id, thread).await;
            return Ok(None);
        }

        if turn.anchor.is_some() {
            return Ok(Some("Acknowledged. I will continue in this Thread.".into()));
        }
        if lower.contains("pong") {
            return Ok(Some("pong".into()));
        }
        if !turn.attachments.is_empty() {
            return Ok(Some(format!("Scripted turn input:\n\n{turn_text}")));
        }
        Ok(Some("I received the Owner message. This scripted Agent mode is a deterministic test double; set HIRSEL_AGENT=lash for the real RLM runtime.".into()))
    }

    pub(super) async fn emit_scripted_timeline(&self, thread_id: u64) {
        let turn_id = self
            .state
            .lock()
            .await
            .active
            .as_ref()
            .and_then(|a| a.turn_id);
        let Some(turn_id) = turn_id else {
            return;
        };
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id,
                seq: 1,
                event: TurnEventKind::Prose {
                    text: "I am checking the scripted path before replying.".to_string(),
                },
            },
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id,
                seq: 2,
                event: TurnEventKind::ToolStart {
                    id: "scripted-tool-1".to_string(),
                    name: "scripted_double".to_string(),
                    summary: Some("deterministic branch".to_string()),
                },
            },
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id,
                seq: 3,
                event: TurnEventKind::ToolDone {
                    id: "scripted-tool-1".to_string(),
                    name: "scripted_double".to_string(),
                    ok: true,
                    summary: Some("ok fixture selected".to_string()),
                },
            },
        );
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id,
                seq: 4,
                event: TurnEventKind::Prose {
                    text: "The scripted response is ready.".to_string(),
                },
            },
        );
    }
}
