use super::*;

impl LashAgentRuntime {
    pub(super) fn spawn_observation_bridge(self: &Arc<Self>) {
        let session = self.session.clone();
        let broadcaster = self.broadcaster.clone();
        let broadcast_log = self.broadcast_log.clone();
        self.tasks.spawn(async move {
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
                                route_observation(&item, &mut timeline, &broadcast_log, &broadcaster).await;
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
                        let item = stream.next().await;
                        route_observation(&item, &mut timeline, &broadcast_log, &broadcaster).await;
                        handle_observation_stream_item(
                            item,
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
                if let (Some((state, text)), Some(thread_id), Some(turn_id)) = (
                    activity_from_observation(&event.event),
                    timeline.thread_id,
                    timeline.turn_id,
                ) {
                    publish(
                        broadcast_log,
                        broadcaster,
                        HostToClient::AgentActivity {
                            thread_id,
                            turn_id,
                            state,
                            text,
                        },
                    );
                }
            } else {
                if let (Some((state, text)), Some(thread_id), Some(turn_id)) = (
                    activity_from_observation(&event.event),
                    timeline.thread_id,
                    timeline.turn_id,
                ) {
                    publish(
                        broadcast_log,
                        broadcaster,
                        HostToClient::AgentActivity {
                            thread_id,
                            turn_id,
                            state,
                            text,
                        },
                    );
                }
                timeline.observe(&event.event, broadcast_log, broadcaster);
            }
            true
        }
        Some(Ok(RemoteSessionObservationStreamItem::Gap { .. })) => {
            timeline.finish_turn(broadcast_log, broadcaster);
            if let (Some(thread_id), Some(turn_id)) = (timeline.thread_id, timeline.turn_id) {
                publish(
                    broadcast_log,
                    broadcaster,
                    HostToClient::AgentActivity {
                        thread_id,
                        turn_id,
                        state: AgentActivityState::Idle,
                        text: None,
                    },
                );
            }
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
) -> Option<(AgentActivityState, Option<String>)> {
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

async fn route_observation<E>(
    item: &Option<Result<RemoteSessionObservationStreamItem, E>>,
    timeline: &mut TurnTimelineBridge,
    log: &BroadcastLog,
    broadcaster: &broadcast::Sender<HostToClient>,
) {
    let Some(Ok(RemoteSessionObservationStreamItem::Event(event))) = item else {
        return;
    };
    let Some(turn_id) = event.turn_id.as_ref() else {
        return;
    };
    let (thread_id, owning_turn_id) = observation_thread_route(turn_id)
        .map(|(thread, turn)| (Some(thread), Some(turn)))
        .unwrap_or((None, None));
    if timeline.thread_id != thread_id || timeline.turn_id != owning_turn_id {
        timeline.finish_turn(log, broadcaster);
        timeline.thread_id = thread_id;
        timeline.turn_id = owning_turn_id;
    }
}

/// Identity is carried by the execution key rather than a growing routing map.
/// Delayed observations therefore keep their original owner after later turns.
pub(super) fn observation_thread_route(id: &str) -> Option<(u64, u64)> {
    let mut parts = id.strip_prefix("host-queue-drain:")?.split(':');
    parts.next()?.parse::<u64>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    if parts.next()? != "thread" {
        return None;
    }
    let thread = parts.next()?.parse().ok()?;
    if parts.next()? != "turn" {
        return None;
    }
    let turn = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((thread, turn))
}
