use super::*;

impl LashAgentRuntime {
    pub(super) fn spawn_observation_bridge(self: &Arc<Self>) {
        let session = self.session.clone();
        let broadcaster = self.broadcaster.clone();
        let broadcast_log = self.broadcast_log.clone();
        tokio::spawn(async move {
            let observable = session.observe();
            let current = observable.current_remote_observation();
            let mut cursor = RemoteSessionCursor::new(current.cursor);
            let mut timeline = TurnTimelineBridge::default();
            let mut retry = ObservationRetryBackoff::default();
            loop {
                let mut stream = match observable.subscribe_and_recover_remote(cursor.clone()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let delay = retry.next_delay();
                        tracing::warn!(%error, ?delay, "failed to subscribe to Lash observation stream; retrying");
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                };
                loop {
                    let keep_stream = if timeline.has_pending() {
                        let flush_delay = timeline.flush_delay();
                        tokio::select! {
                            item = stream.next() => {
                                handle_observation_stream_item(
                                    item,
                                    &broadcast_log,
                                    &broadcaster,
                                    &mut timeline,
                                )
                            }
                            () = tokio::time::sleep(flush_delay) => {
                                timeline.flush_pending(&broadcast_log, &broadcaster);
                                true
                            }
                        }
                    } else {
                        handle_observation_stream_item(
                            stream.next().await,
                            &broadcast_log,
                            &broadcaster,
                            &mut timeline,
                        )
                    };
                    cursor = stream.cursor();
                    if !keep_stream {
                        break;
                    }
                    retry.reset();
                }
                let delay = retry.next_delay();
                tracing::warn!(?delay, "Lash observation stream ended; resubscribing");
                tokio::time::sleep(delay).await;
            }
        });
    }

    pub(super) fn spawn_process_terminal_bridge(
        self: &Arc<Self>,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        let mut events = self.tools.terminal_events();
        let notify = Arc::clone(&self.notify);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let (event_type, payload) = terminal_event_payload(&event.outcome);
                        let request =
                            ProcessEventAppendRequest::new(event_type, payload).with_replay_key(
                                format!("hirsel-subagent:{}:{event_type}", event.process_id),
                            );
                        match process_registry
                            .append_event(&event.process_id, request)
                            .await
                        {
                            Ok(result) => {
                                if let Err(error) = enqueue_process_wake(
                                    store_factory.as_ref(),
                                    result.wake_delivery,
                                )
                                .await
                                {
                                    tracing::warn!(%error, "failed to enqueue Lash process wake");
                                } else {
                                    notify.notify_one();
                                }
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %event.process_id,
                                    "failed to append Lash process terminal event"
                                );
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    pub(super) fn spawn_subagent_control_bridge(
        self: &Arc<Self>,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
    ) {
        let tools = self.tools.clone();
        tokio::spawn(async move {
            let mut cancelled = HashSet::new();
            let mut abandoned = HashSet::new();
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let processes = match tools.subagents_list() {
                    Ok(processes) => processes,
                    Err(error) => {
                        tracing::warn!(%error, "failed to list Sub-agent processes for control bridge");
                        continue;
                    }
                };
                for process in processes {
                    if !matches!(process.status, crate::processes::ProcessStatus::Running) {
                        continue;
                    }
                    let process_id = process.id.clone();
                    let Ok(Some(record)) = process_registry.get_process(&process_id).await else {
                        continue;
                    };
                    if record.abandon_request.is_some() && !abandoned.contains(&process_id) {
                        match tools.subagents_abandon_process(&process_id).await {
                            Ok(()) => {
                                abandoned.insert(process_id.clone());
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %process_id,
                                    "failed to abandon Sub-agent after Lash abandon request"
                                );
                            }
                        }
                        continue;
                    }
                    if cancelled.contains(&process_id) {
                        continue;
                    }
                    let events = match process_registry.events_after(&process_id, 0).await {
                        Ok(events) => events,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                process_id = %process_id,
                                "failed to read Sub-agent process events for control bridge"
                            );
                            continue;
                        }
                    };
                    if events
                        .iter()
                        .any(|event| event.event_type == "process.cancel_requested")
                    {
                        match tools.subagents_interrupt_process(&process_id).await {
                            Ok(()) => {
                                cancelled.insert(process_id.clone());
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %process_id,
                                    "failed to interrupt Sub-agent after Lash cancel request"
                                );
                            }
                        }
                    }
                }
            }
        });
    }
}

pub(super) async fn enqueue_process_wake(
    store_factory: &dyn lash::persistence::SessionStoreFactory,
    wake_delivery: Option<ProcessWakeDelivery>,
) -> anyhow::Result<()> {
    let Some(wake) = wake_delivery else {
        return Ok(());
    };
    let request = lash::persistence::SessionStoreCreateRequest {
        session_id: wake.target_session_id.clone(),
        relation: lash::persistence::SessionRelation::default(),
        policy: SessionPolicy::new(lash::TurnBudget::Unbounded),
    };
    let Some(store) = store_factory
        .open_existing_store(&request)
        .await
        .map_err(anyhow::Error::msg)?
    else {
        return Ok(());
    };
    let draft = lash::persistence::QueuedWorkBatchDraft::new(
        wake.target_session_id.clone(),
        lash::persistence::DeliveryPolicy::EarliestSafeBoundary,
        vec![lash::persistence::QueuedWorkPayload::process_wake(
            wake.clone(),
        )],
    )
    .with_source_key(format!(
        "process:{}:event:{}:wake",
        wake.process_id, wake.sequence
    ));
    store.enqueue_queued_work(draft).await?;
    Ok(())
}

pub(super) fn handle_observation_stream_item<E>(
    item: Option<Result<RemoteSessionObservationStreamItem, E>>,
    broadcast_log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
    timeline: &mut TurnTimelineBridge,
) -> bool
where
    E: std::fmt::Display,
{
    match item {
        Some(Ok(RemoteSessionObservationStreamItem::Event(event))) => {
            if matches!(
                &event.event,
                RemoteSessionObservationEventPayload::Committed
            ) {
                timeline.observe(&event.event, broadcast_log, broadcaster);
                if let Some(activity) = activity_from_observation(&event.event) {
                    publish(broadcast_log, broadcaster, activity);
                }
            } else {
                if let Some(activity) = activity_from_observation(&event.event) {
                    publish(broadcast_log, broadcaster, activity);
                }
                timeline.observe(&event.event, broadcast_log, broadcaster);
            }
            true
        }
        Some(Ok(RemoteSessionObservationStreamItem::Gap { .. })) => {
            timeline.finish_turn(broadcast_log, broadcaster);
            publish(
                broadcast_log,
                broadcaster,
                HostToClient::AgentActivity {
                    state: AgentActivityState::Idle,
                    text: None,
                    sc: None,
                },
            );
            true
        }
        Some(Err(error)) => {
            timeline.finish_turn(broadcast_log, broadcaster);
            tracing::warn!(%error, "Lash observation stream failed");
            false
        }
        None => {
            timeline.finish_turn(broadcast_log, broadcaster);
            false
        }
    }
}

#[derive(Default)]
pub(super) struct ObservationRetryBackoff {
    pub(super) failures: u32,
}

impl ObservationRetryBackoff {
    pub(super) fn next_delay(&mut self) -> Duration {
        let exponent = self.failures.min(5);
        self.failures = self.failures.saturating_add(1);
        Duration::from_millis(100).saturating_mul(1 << exponent)
    }

    pub(super) fn reset(&mut self) {
        self.failures = 0;
    }
}

pub(super) fn activity_from_observation(
    event: &RemoteSessionObservationEventPayload,
) -> Option<HostToClient> {
    match event {
        RemoteSessionObservationEventPayload::TurnActivity { activity } => match &activity.event {
            RemoteTurnEvent::ModelRequestStarted { .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some("thinking".to_string()),
            )),
            RemoteTurnEvent::AssistantProseDelta { text }
            | RemoteTurnEvent::ReasoningDelta { text } => Some(agent_activity(
                AgentActivityState::Thinking,
                latest_line(text),
            )),
            RemoteTurnEvent::ToolCallStarted { name, .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some(format!("tool {name}")),
            )),
            RemoteTurnEvent::ToolCallCompleted { name, .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some(format!("tool {name} completed")),
            )),
            RemoteTurnEvent::Error { message } => Some(agent_activity(
                AgentActivityState::Thinking,
                latest_line(message),
            )),
            _ => None,
        },
        RemoteSessionObservationEventPayload::Committed => {
            Some(agent_activity(AgentActivityState::Idle, None))
        }
        _ => None,
    }
}
