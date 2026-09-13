use super::*;

impl LashAgentRuntime {
    pub(super) fn spawn_observation_bridge(self: &Arc<Self>) {
        let observable = self.session.observe();
        let current = observable.current_remote_observation();
        let initial_cursor = RemoteSessionCursor::new(current.cursor);
        let broadcaster = self.broadcaster.clone();
        let broadcast_log = self.broadcast_log.clone();
        let tools = self.tools.clone();
        let timeline_commits = self.timeline_commits.clone();
        let active_turn_id = self.active_turn_id.clone();
        self.tasks.spawn(async move {
            let mut cursor = initial_cursor;
            let mut ingest = TurnIngest::default();
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
                    let keep_stream = if ingest.has_pending() {
                        let flush_delay = ingest.flush_delay();
                        tokio::select! {
                            item = stream.next() => process_observation_stream_item(
                                item, &broadcast_log, &broadcaster, &tools, &mut ingest,
                                &timeline_commits, &active_turn_id,
                            ).await,
                            () = tokio::time::sleep(flush_delay) => {
                                if let Err(error) = ingest.flush(&tools).await {
                                    tracing::warn!(%error, "failed to flush observed turn timeline");
                                }
                                true
                            }
                        }
                    } else {
                        process_observation_stream_item(
                            stream.next().await,
                            &broadcast_log,
                            &broadcaster,
                            &tools,
                            &mut ingest,
                            &timeline_commits,
                            &active_turn_id,
                        )
                        .await
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

pub(super) async fn process_observation_stream_item<E>(
    item: Option<Result<RemoteSessionObservationStreamItem, E>>,
    _broadcast_log: &BroadcastLog,
    _broadcaster: &broadcast::Sender<HostToClient>,
    tools: &ToolSuite,
    ingest: &mut TurnIngest,
    timeline_commits: &TimelineCommitBarrier,
    active_turn_id: &Mutex<Option<String>>,
) -> bool
where
    E: std::fmt::Display,
{
    let committed = committed_turn_id(&item);
    if let Err(error) = route_observation(&item, ingest, tools).await {
        record_ingest_failure(&item, tools, ingest, timeline_commits, error).await;
    }
    let keep =
        handle_observation_stream_item(item, tools, ingest, timeline_commits, active_turn_id).await;
    if let Some(turn_id) = committed {
        timeline_commits.record(turn_id).await;
    }
    keep
}

fn committed_turn_id<E>(
    item: &Option<Result<RemoteSessionObservationStreamItem, E>>,
) -> Option<String> {
    let Some(Ok(RemoteSessionObservationStreamItem::Event(event))) = item else {
        return None;
    };
    matches!(event.event, RemoteSessionObservationEventPayload::Committed)
        .then(|| event.turn_id.clone())
        .flatten()
}

async fn handle_observation_stream_item<E>(
    item: Option<Result<RemoteSessionObservationStreamItem, E>>,
    tools: &ToolSuite,
    ingest: &mut TurnIngest,
    timeline_commits: &TimelineCommitBarrier,
    active_turn_id: &Mutex<Option<String>>,
) -> bool
where
    E: std::fmt::Display,
{
    match item {
        Some(Ok(RemoteSessionObservationStreamItem::Event(event))) => {
            let result = if matches!(event.event, RemoteSessionObservationEventPayload::Committed) {
                ingest.flush(tools).await
            } else if let Some(event) = host_executor_event(&event.event) {
                ingest.accept(tools, event).await
            } else {
                Ok(())
            };
            if let Err(error) = result {
                let wrapped = Some(Ok::<_, E>(RemoteSessionObservationStreamItem::Event(event)));
                record_ingest_failure(&wrapped, tools, ingest, timeline_commits, error).await;
            }
            true
        }
        Some(Ok(RemoteSessionObservationStreamItem::Gap { gap, .. })) => {
            let _ = ingest.flush(tools).await;
            if let Some(drain_id) = active_turn_id.lock().await.clone()
                && let Some((_thread_id, turn_id)) = observation_thread_route(&drain_id)
            {
                let reason = format!(
                    "Turn timeline is incomplete because the Lash observation replay window reported a {:?} gap",
                    gap.reason
                );
                timeline_commits.fail(drain_id, reason.clone()).await;
                tools.fail_turn_timeline_integrity(turn_id, &reason).await;
            }
            true
        }
        Some(Err(error)) => {
            let _ = ingest.flush(tools).await;
            tracing::warn!(%error, "Lash observation stream failed");
            false
        }
        None => {
            let _ = ingest.flush(tools).await;
            false
        }
    }
}

async fn record_ingest_failure<E>(
    item: &Option<Result<RemoteSessionObservationStreamItem, E>>,
    tools: &ToolSuite,
    ingest: &TurnIngest,
    timeline_commits: &TimelineCommitBarrier,
    error: anyhow::Error,
) {
    let physical_id = match item {
        Some(Ok(RemoteSessionObservationStreamItem::Event(event))) => event.turn_id.clone(),
        _ => None,
    };
    let reason = format!("turn event ingest failed: {error}");
    if let Some(physical_id) = physical_id {
        timeline_commits.fail(physical_id, reason.clone()).await;
    }
    if let Some(turn_id) = ingest.turn_id {
        tools.fail_turn_timeline_integrity(turn_id, &reason).await;
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

async fn route_observation<E>(
    item: &Option<Result<RemoteSessionObservationStreamItem, E>>,
    ingest: &mut TurnIngest,
    tools: &ToolSuite,
) -> anyhow::Result<()> {
    let Some(Ok(RemoteSessionObservationStreamItem::Event(event))) = item else {
        return Ok(());
    };
    let Some(turn_id) = event.turn_id.as_ref() else {
        return Ok(());
    };
    let (thread_id, owning_turn_id) = observation_thread_route(turn_id)
        .map(|(thread, turn)| (Some(thread), Some(turn)))
        .unwrap_or((None, None));
    ingest.reroute(tools, thread_id, owning_turn_id).await
}

/// Identity is carried by the execution key rather than a growing routing map.
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
