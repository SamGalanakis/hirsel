use super::*;

pub(super) struct ScriptedAgentRuntime {
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
    pub(super) async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        #[cfg(test)]
        if turn.body == "__hirsel_test_enqueue_error__" {
            anyhow::bail!("scripted enqueue failed for test");
        }
        self.tools
            .storage()
            .save_thread_request(&turn.client_id, &serde_json::to_value(&turn)?)
            .await?;
        let queued = self
            .tools
            .storage()
            .queue_thread_turn(turn.thread_id, Some(turn.message_id))
            .await?;
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
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: None,
                thread_id: None,
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
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
            let record = self
                .tools
                .storage()
                .queue_thread_turn(turn.thread_id, Some(turn.message_id))
                .await?;
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
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: None,
                thread_id: None,
                state: AgentActivityState::Thinking,
                text: Some("monitor wake".to_string()),
                sc: None,
            },
        );
        self.tools.chat_send(text, None).await?;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: None,
                thread_id: None,
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    pub(super) async fn spawn_active_standalone_monitors(self: Arc<Self>) {
        match self.tools.active_monitors().await {
            Ok(monitors) => {
                for monitor in monitors {
                    self.spawn_standalone_monitor(monitor.id);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to resume scripted standalone monitors");
            }
        }
    }

    pub(super) fn spawn_standalone_monitor(self: &Arc<Self>, monitor_id: String) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
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

    pub(super) async fn run(self: Arc<Self>) {
        tracing::info!(
            model = %self.config.model,
            data_dir = %self.config.data_dir.display(),
            "Scripted Agent test double opened session agent"
        );
        match self.tools.storage().pending_thread_requests().await {
            Ok(requests) => {
                for (client_id, payload) in requests {
                    let Ok(turn) = serde_json::from_value::<OwnerTurn>(payload) else {
                        continue;
                    };
                    let Ok(record) = self
                        .tools
                        .storage()
                        .queue_thread_turn(turn.thread_id, Some(turn.message_id))
                        .await
                    else {
                        continue;
                    };
                    if record.finished_at.is_some() {
                        let _ = self.tools.storage().remove_thread_request(&client_id).await;
                        continue;
                    }
                    let mut state = self.state.lock().await;
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
            turn_id: None,
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
        let record = self
            .tools
            .storage()
            .queue_thread_turn(turn.thread_id, Some(turn.message_id))
            .await?;
        let record = self.tools.storage().run_thread_turn(record.id).await?;
        self.tools.publish_thread_turn(record.clone()).await;
        if let Some(active) = self.state.lock().await.active.as_mut() {
            active.turn_id = Some(record.id);
        }
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: Some(record.id),
                thread_id: Some(turn.thread_id),
                state: AgentActivityState::Thinking,
                text: Some("processing owner message".into()),
                sc: None,
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
        let detail = self
            .tools
            .storage()
            .thread_detail(turn.thread_id, None, 100)
            .await?;
        let reply_id = detail
            .messages
            .iter()
            .rev()
            .find(|m| {
                m.author == hirsel_proto::ChatAuthor::Agent && m.r#ref == Some(turn.message_id)
            })
            .map(|m| m.id);
        let record = self
            .tools
            .storage()
            .finish_thread_turn(record.id, state, reply_id)
            .await?;
        self.tools.publish_thread_turn(record.clone()).await;
        self.tools
            .storage()
            .remove_thread_request(&turn.client_id)
            .await?;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                turn_id: Some(record.id),
                thread_id: Some(turn.thread_id),
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        result
    }

    pub(super) async fn handle_turn_inner(
        &self,
        turn: &OwnerTurn,
        cancel: &lash::CancellationToken,
    ) -> anyhow::Result<()> {
        if let Some(duration) = slow_turn_duration(&turn.body)?
            && !sleep_until_done_or_cancelled(duration, cancel).await
        {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Ok(());
        }
        self.emit_scripted_timeline(turn.thread_id).await;
        let turn_text = owner_turn_text(turn);
        let lower = turn_text.to_lowercase();
        if self.config.driver_mode == DriverMode::Fake && lower.contains("delegate") {
            return self.handle_fake_delegation(turn).await;
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
            self.tools.publish_thread(thread);
            return Ok(());
        }
        if let Some(context) = &turn.task_action {
            let label = context
                .data
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(&context.action);
            self.tools
                .events_recompose(
                    context.event.id,
                    Some(format!("Advanced after {label}")),
                    json!({
                        "type": "card",
                        "children": [
                            {
                                "type": "eyebrow",
                                "text": "Deterministic Host fixture",
                                "tone": "accent"
                            },
                            {
                                "type": "heading",
                                "text": format!("{} advanced", context.event.name),
                                "level": 2
                            },
                            {
                                "type": "status",
                                "state": "success",
                                "label": format!("Received action: {}", context.action),
                                "tone": "success"
                            },
                            {
                                "type": "text",
                                "text": "The same Task and Anchor now expose the next meaningful stage.",
                                "tone": "muted"
                            },
                            {
                                "type": "optionList",
                                "action": "choose",
                                "options": [
                                    {
                                        "key": "A",
                                        "label": "Complete task",
                                        "detail": "Settle this recomposed Task.",
                                        "recommended": true
                                    },
                                    {
                                        "key": "B",
                                        "label": "Keep open",
                                        "detail": "Leave the Task at this stage."
                                    }
                                ]
                            }
                        ]
                    }),
                )
                .await?;
            return Ok(());
        }
        if turn.anchor.is_some() {
            self.tools
                .chat_send(
                    "Acknowledged. I will continue in this Thread.",
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        if lower.contains("pong") {
            self.tools.chat_send("pong", Some(turn.message_id)).await?;
            return Ok(());
        }
        if !turn.attachments.is_empty() {
            self.tools
                .chat_send(
                    format!("Scripted turn input:\n\n{turn_text}"),
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        self.tools
            .chat_send(
                "I received the Owner message. This scripted Agent mode is a deterministic test double; set HIRSEL_AGENT=lash for the real RLM runtime.",
                Some(turn.message_id),
            )
            .await?;
        Ok(())
    }

    pub(super) async fn emit_scripted_timeline(&self, thread_id: u64) {
        let turn_id = self
            .state
            .lock()
            .await
            .active
            .as_ref()
            .and_then(|a| a.turn_id);
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id: Some(thread_id),
                seq: 1,
                event: TurnEventKind::Prose {
                    text: "I am checking the scripted path before replying.".to_string(),
                },
                sc: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id: Some(thread_id),
                seq: 2,
                event: TurnEventKind::ToolStart {
                    id: "scripted-tool-1".to_string(),
                    name: "scripted_double".to_string(),
                    summary: Some("deterministic branch".to_string()),
                },
                sc: None,
            },
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id: Some(thread_id),
                seq: 3,
                event: TurnEventKind::ToolDone {
                    id: "scripted-tool-1".to_string(),
                    name: "scripted_double".to_string(),
                    ok: true,
                    summary: Some("ok fixture selected".to_string()),
                },
                sc: None,
            },
        );
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::TurnEvent {
                turn_id,
                thread_id: Some(thread_id),
                seq: 4,
                event: TurnEventKind::Prose {
                    text: "The scripted response is ready.".to_string(),
                },
                sc: None,
            },
        );
    }

    pub(super) async fn handle_fake_delegation(&self, turn: &OwnerTurn) -> anyhow::Result<()> {
        let anchor = self
            .tools
            .chat_send(
                "I delegated the repo fix to a Sub-agent; its result will stay in this Thread.",
                Some(turn.message_id),
            )
            .await?;
        let mut terminal_events = self.tools.terminal_events();
        let cwd = std::env::current_dir()?;
        let process = self
            .tools
            .subagents_spawn(
                AgentKind::Claude,
                None,
                None,
                "Make the trivial repo fix and report back.",
                cwd,
            )
            .await?;
        let tools = self.tools.clone();
        tokio::spawn(async move {
            loop {
                match terminal_events.recv().await {
                    Ok(event) if event.process_id == process.process_id => {
                        let content = terminal_content(&event.outcome);
                        let activity = tools
                            .storage()
                            .append_thread_activity(
                                anchor.thread_id,
                                None,
                                "process_completed",
                                &json!({"process_id":process.process_id,"summary":content}),
                            )
                            .await;
                        match activity {
                            Ok(activity) => tools.publish_thread_activity(activity).await,
                            Err(error) => {
                                tracing::warn!(%error,"failed to record Sub-agent completion")
                            }
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(())
    }
}
