use super::*;

const PROCESS_RESULT_CHARS: usize = 8 * 1024;

impl LashAgentRuntime {
    pub(super) fn spawn_process_bridge(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        self.tasks.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = runtime.process_notify.notified() => {}
                }
                if let Err(error) = runtime.reconcile_processes().await {
                    tracing::warn!(%error, thread_id = runtime.thread_id, "process projection failed");
                }
                // Only after process wakes have been durably staged and
                // removed from Lash's resident queue may ordinary queued work
                // reach the main Agent pump.
                if runtime.work_pending().await {
                    runtime.notify.notify_one();
                }
            }
        });
    }

    pub(super) async fn process_snapshot(&self) -> anyhow::Result<Vec<hirsel_proto::ProcessInfo>> {
        let snapshot = self
            .core
            .processes()
            .session_snapshot(&self.session_id)
            .await?;
        let subscriptions = self
            .trigger_store
            .list_subscriptions(TriggerSubscriptionFilter::for_session(&self.session_id))
            .await?;
        let represented_subscriptions = snapshot
            .items
            .iter()
            .filter_map(|item| match item.process.caused_by.as_ref() {
                Some(lash_core::CausalRef::TriggerOccurrence {
                    subscription_id: Some(id),
                    ..
                }) => Some(id.as_str()),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut processes = snapshot
            .items
            .iter()
            .map(|item| process_info(self.thread_id, item, &subscriptions))
            .collect::<Vec<_>>();
        processes.extend(
            subscriptions
                .iter()
                .filter(|record| {
                    !record.tombstoned
                        && !represented_subscriptions.contains(record.subscription_id.as_str())
                })
                .map(|record| subscription_process_info(self.thread_id, record)),
        );
        processes.sort_by(|left, right| {
            left.started_ts
                .cmp(&right.started_ts)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(processes)
    }

    async fn reconcile_processes(&self) -> anyhow::Result<()> {
        self.capture_process_wakes().await?;
        self.capture_terminal_processes().await?;
        for key in self
            .tools
            .storage()
            .pending_process_deliveries(self.thread_id)
            .await?
        {
            self.deliver_process_event(&key).await?;
        }
        let current = self.process_snapshot().await?;
        let mut previous = self.last_processes.lock().await;
        for process in &current {
            if previous.get(&process.id) != Some(process) {
                self.tools.broadcast(HostToClient::ProcessUpsert {
                    process: process.clone(),
                });
            }
        }
        previous.clear();
        previous.extend(
            current
                .into_iter()
                .map(|process| (process.id.clone(), process)),
        );
        Ok(())
    }

    async fn capture_process_wakes(&self) -> anyhow::Result<()> {
        for batch in self.session.queued_work().await? {
            let mut captured = false;
            for item in &batch.items {
                let payload = serde_json::to_value(&item.payload)?;
                if payload.get("type").and_then(Value::as_str) != Some("process_wake") {
                    continue;
                }
                let wake: lash_core::ProcessWakeDelivery = serde_json::from_value(
                    payload
                        .get("wake")
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("process wake payload is missing wake"))?,
                )?;
                let process = self.core.processes().get(&wake.process_id).await?;
                let name = process
                    .as_ref()
                    .map(|process| process.label.clone())
                    .unwrap_or_else(|| wake.process_id.clone());
                let trigger = self.trigger_label(wake.process_caused_by.as_ref()).await?;
                self.tools
                    .storage()
                    .stage_process_delivery(&crate::storage::ProcessDelivery {
                        key: format!("process-wake:{}", wake.wake_id),
                        thread_id: self.thread_id,
                        process_id: wake.process_id.clone(),
                        process_name: name,
                        trigger,
                        outcome: "woke".to_string(),
                        result: bound_text(&wake.input),
                    })
                    .await?;
                captured = true;
            }
            if captured {
                self.session
                    .cancel_queued_work_batch(&batch.batch_id)
                    .await?;
            }
        }
        Ok(())
    }

    async fn capture_terminal_processes(&self) -> anyhow::Result<()> {
        let snapshot = self
            .core
            .processes()
            .session_snapshot(&self.session_id)
            .await?;
        for item in snapshot.items {
            let Some((event, mut delivery)) = terminal_process_delivery(self.thread_id, &item)
            else {
                continue;
            };
            delivery.trigger = self.trigger_label(item.process.caused_by.as_ref()).await?;
            debug_assert!(delivery.key.ends_with(&event.sequence.to_string()));
            self.tools
                .storage()
                .stage_process_delivery(&delivery)
                .await?;
        }
        Ok(())
    }

    async fn deliver_process_event(&self, key: &str) -> anyhow::Result<()> {
        let delivered = self.tools.storage().deliver_process_message(key).await?;
        let text = delivered.message.body.clone();
        if delivered.newly_appended {
            self.tools.publish_thread_message(delivered.message).await;
        }
        anyhow::ensure!(
            self.fork_wake.dispatch(crate::fork_wake::WakeMessage::new(
                self.thread_id,
                crate::fork_wake::WakeSource::Process {
                    process_id: delivered.delivery.process_id,
                    name: delivered.delivery.process_name,
                    trigger: delivered.delivery.trigger,
                },
                text,
                key.to_string(),
            )),
            "process event requires its Thread triage dispatcher"
        );
        self.tools
            .storage()
            .mark_process_triage_dispatched(key)
            .await?;
        Ok(())
    }

    async fn trigger_label(&self, cause: Option<&lash_core::CausalRef>) -> anyhow::Result<String> {
        let Some(lash_core::CausalRef::TriggerOccurrence {
            subscription_id: Some(subscription_id),
            ..
        }) = cause
        else {
            return Ok("direct start".to_string());
        };
        let records = self
            .trigger_store
            .list_subscriptions(TriggerSubscriptionFilter::for_session(&self.session_id))
            .await?;
        Ok(records
            .iter()
            .find(|record| &record.subscription_id == subscription_id)
            .map(trigger_display)
            .unwrap_or_else(|| format!("trigger {subscription_id}")))
    }

    pub(super) async fn cancel_process(&self, process_id: &str) -> anyhow::Result<()> {
        self.core
            .processes()
            .cancel(
                process_id,
                inline_trigger_scope(format!("ui:cancel-process:{process_id}")),
            )
            .await?;
        Ok(())
    }

    pub(super) async fn disable_trigger(
        &self,
        subscription_key: &str,
        expected_revision: u64,
    ) -> anyhow::Result<()> {
        let records = self
            .trigger_store
            .list_subscriptions(TriggerSubscriptionFilter::for_session(&self.session_id))
            .await?;
        let record = records
            .into_iter()
            .find(|record| record.subscription_key == subscription_key)
            .ok_or_else(|| anyhow::anyhow!("trigger subscription is unavailable"))?;
        anyhow::ensure!(
            record.revision == expected_revision,
            "trigger subscription changed"
        );
        self.trigger_store
            .execute_command(
                &format!("ui:disable-trigger:{subscription_key}:{expected_revision}"),
                lash::triggers::TriggerCommand::Disable {
                    owner_scope: record.owner_scope,
                    actor: record.registrant,
                    subscription_key: subscription_key.to_string(),
                    expected_revision,
                },
            )
            .await??;
        Ok(())
    }
}

pub(super) fn terminal_process_delivery(
    thread_id: u64,
    item: &lash_core::facade_support::ObservedWorkItem,
) -> Option<(
    &lash_core::facade_support::ObservedProcessEvent,
    crate::storage::ProcessDelivery,
)> {
    if !matches!(
        item.process.lifecycle,
        lash_core::ProcessStatus::Completed
            | lash_core::ProcessStatus::Failed
            | lash_core::ProcessStatus::Cancelled
    ) {
        return None;
    }
    let event = item.events.iter().rev().find(|event| {
        matches!(
            event.event_type.as_str(),
            "process.completed" | "process.failed" | "process.cancelled"
        )
    })?;
    Some((
        event,
        crate::storage::ProcessDelivery {
            key: format!(
                "process-terminal:{}:{:?}:{}",
                item.process.process_id, item.process.incarnation, event.sequence
            ),
            thread_id,
            process_id: item.process.process_id.clone(),
            process_name: item.label.clone(),
            trigger: "direct start".to_string(),
            outcome: item.process.lifecycle.label().to_string(),
            result: terminal_result(&event.payload),
        },
    ))
}

pub(super) fn process_info(
    thread_id: u64,
    item: &lash_core::facade_support::ObservedWorkItem,
    subscriptions: &[lash_core::TriggerSubscriptionRecord],
) -> hirsel_proto::ProcessInfo {
    let subscription = match item.process.caused_by.as_ref() {
        Some(lash_core::CausalRef::TriggerOccurrence {
            subscription_id: Some(id),
            ..
        }) => subscriptions
            .iter()
            .find(|record| &record.subscription_id == id),
        _ => None,
    };
    let terminal = item.events.iter().rev().find(|event| {
        matches!(
            event.event_type.as_str(),
            "process.completed" | "process.failed" | "process.cancelled" | "process.abandoned"
        )
    });
    hirsel_proto::ProcessInfo {
        thread_id,
        id: item.process.process_id.clone(),
        name: item.label.clone(),
        trigger: subscription.map(trigger_display),
        trigger_subscription_key: subscription.map(|record| record.subscription_key.clone()),
        trigger_revision: subscription.map(|record| record.revision),
        trigger_enabled: subscription.map(|record| record.enabled),
        cancellable: true,
        state: match item.process.lifecycle {
            lash_core::ProcessStatus::Running => hirsel_proto::ProcessState::Running,
            lash_core::ProcessStatus::Waiting => hirsel_proto::ProcessState::Waiting,
            lash_core::ProcessStatus::Completed => hirsel_proto::ProcessState::Done,
            lash_core::ProcessStatus::Failed => hirsel_proto::ProcessState::Failed,
            lash_core::ProcessStatus::Cancelled => hirsel_proto::ProcessState::Cancelled,
            lash_core::ProcessStatus::Abandoned => hirsel_proto::ProcessState::Abandoned,
            lash_core::ProcessStatus::CallerDeparted => hirsel_proto::ProcessState::CallerDeparted,
        },
        started_ts: timestamp(item.process.created_at_ms),
        last_event_ts: timestamp(item.process.updated_at_ms),
        last_fired_ts: subscription.map(|_| timestamp(item.process.created_at_ms)),
        last_outcome: terminal.map(|event| bound_json(&event.payload)),
    }
}

pub(super) fn subscription_process_info(
    thread_id: u64,
    subscription: &lash_core::TriggerSubscriptionRecord,
) -> hirsel_proto::ProcessInfo {
    hirsel_proto::ProcessInfo {
        thread_id,
        id: format!("subscription:{}", subscription.subscription_id),
        name: subscription
            .target_label
            .clone()
            .or_else(|| subscription.target_identity.label.clone())
            .or_else(|| subscription.name.clone())
            .unwrap_or_else(|| subscription.subscription_key.clone()),
        trigger: Some(trigger_display(subscription)),
        trigger_subscription_key: Some(subscription.subscription_key.clone()),
        trigger_revision: Some(subscription.revision),
        trigger_enabled: Some(subscription.enabled),
        cancellable: false,
        state: hirsel_proto::ProcessState::Waiting,
        started_ts: timestamp(subscription.created_at_ms),
        last_event_ts: timestamp(subscription.updated_at_ms),
        last_fired_ts: None,
        last_outcome: None,
    }
}

pub(super) fn trigger_display(record: &lash_core::TriggerSubscriptionRecord) -> String {
    let value = record.source.get("$lash_host_descriptor_value");
    match record.source_type.as_str() {
        "cron.Schedule" => value
            .and_then(|value| value.get("expr"))
            .and_then(Value::as_str)
            .map(|expr| format!("cron {expr}")),
        TIMER_SOURCE_TYPE => value.and_then(|value| {
            value
                .get("every_secs")
                .and_then(Value::as_u64)
                .map(|seconds| format!("every {seconds}s"))
                .or_else(|| {
                    value
                        .get("at")
                        .and_then(Value::as_str)
                        .map(|at| format!("at {at}"))
                })
                .or_else(|| {
                    value
                        .get("in_secs")
                        .and_then(Value::as_u64)
                        .map(|seconds| format!("in {seconds}s"))
                })
        }),
        source if source.starts_with("thread.") => value
            .and_then(|value| value.get("thread_id"))
            .and_then(Value::as_u64)
            .map(|id| format!("{source} #{id}")),
        _ => None,
    }
    .or_else(|| record.name.clone())
    .unwrap_or_else(|| record.source_type.clone())
}

fn timestamp(milliseconds: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(milliseconds.min(i64::MAX as u64) as i64)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn bound_json(value: &Value) -> String {
    bound_text(&serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()))
}

fn terminal_result(payload: &Value) -> String {
    payload
        .get("await_output")
        .cloned()
        .and_then(|value| serde_json::from_value::<ProcessAwaitOutput>(value).ok())
        .map(ProcessAwaitOutput::into_tool_output)
        .map(|output| bound_json(&output.into_value_for_projection()))
        .unwrap_or_else(|| bound_json(payload))
}

fn bound_text(value: &str) -> String {
    let mut text = value.chars().take(PROCESS_RESULT_CHARS).collect::<String>();
    if text.len() < value.len() {
        text.push('…');
    }
    text
}
