use super::*;
use hirsel_proto::{MessageOrigin, ProcessOutcome, TriggerLabel};

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
        let deliveries = self
            .trigger_store
            .list_deliveries()
            .await?
            .into_iter()
            .filter(|delivery| {
                delivery.subscription.registrant_session_id() == Some(self.session_id.as_str())
            })
            .collect::<Vec<_>>();
        Ok(super::process_projection::process_rows(
            self.thread_id,
            &snapshot.items,
            &subscriptions,
            &deliveries,
        ))
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
        for (id, old) in previous.iter() {
            if !current.iter().any(|row| &row.id == id) {
                self.tools.broadcast(HostToClient::ProcessRemoved {
                    thread_id: old.thread_id,
                    id: id.clone(),
                });
            }
        }
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
                let (trigger, subscription_key) = self
                    .trigger_label(&wake.process_id, wake.process_caused_by.as_ref())
                    .await?;
                self.tools
                    .storage()
                    .stage_process_delivery(&crate::storage::ProcessDelivery {
                        key: format!("process-wake:{}", wake.wake_id),
                        thread_id: self.thread_id,
                        origin: MessageOrigin::Process {
                            process_id: wake.process_id.clone(),
                            name,
                            trigger,
                            subscription_key,
                            outcome: ProcessOutcome::Woke,
                            result: Value::String(wake.input),
                            error: None,
                        },
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
            let (label, key) = self
                .trigger_label(&item.process.process_id, item.process.caused_by.as_ref())
                .await?;
            let MessageOrigin::Process {
                trigger,
                subscription_key,
                ..
            } = &mut delivery.origin;
            *trigger = label;
            *subscription_key = key;
            debug_assert!(delivery.key.ends_with(&event.sequence.to_string()));
            self.tools
                .storage()
                .stage_process_delivery(&delivery)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn deliver_process_event(&self, key: &str) -> anyhow::Result<()> {
        let delivered = self.tools.storage().deliver_process_message(key).await?;
        anyhow::ensure!(
            delivered.message.thread_id == self.thread_id,
            "process wake destination mismatch"
        );
        if delivered.newly_appended {
            self.tools
                .publish_thread_message(delivered.message.clone())
                .await;
        }
        let client_id = format!("process-delivery:{key}");
        let request = OwnerTurn {
            history_id: self.history_id.clone(),
            turn_id: None,
            thread_id: self.thread_id,
            thread_action: None,
            message_id: None,
            report_triggered: false,
            client_id: client_id.clone(),
            body: format!(
                "Solicited process delivery (message #{}):\n{}",
                delivered.message.id,
                serde_json::to_string(&delivered.message)?
            ),
            anchor: None,
            attachments: Vec::new(),
            mode: SendMode::NextTurn,
        };
        // queue_background_thread_request retains a durable acceptance key even
        // after the request is consumed. A crash at any boundary can replay this.
        let turn = self
            .tools
            .storage()
            .queue_background_thread_request(
                &client_id,
                self.thread_id,
                &serde_json::to_value(request)?,
            )
            .await?;
        self.publish_background_acceptance(&client_id, turn).await?;
        self.tools.storage().mark_process_enqueued(key).await?;
        self.notify.notify_one();
        Ok(())
    }

    async fn trigger_label(
        &self,
        process_id: &str,
        cause: Option<&lash_core::CausalRef>,
    ) -> anyhow::Result<(TriggerLabel, Option<String>)> {
        let Some(lash_core::CausalRef::TriggerOccurrence {
            subscription_id: Some(id),
            ..
        }) = cause
        else {
            return Ok((
                TriggerLabel::Other {
                    key: "direct start".into(),
                },
                None,
            ));
        };
        let Some(record) = trigger_registration(
            self.trigger_store.as_ref(),
            &self.session_id,
            process_id,
            id,
        )
        .await?
        else {
            return Ok((
                TriggerLabel::Other {
                    key: "unavailable trigger".into(),
                },
                None,
            ));
        };
        let mut label = structured_trigger(&record);
        if let TriggerLabel::Thread {
            thread_id, title, ..
        } = &mut label
        {
            *title = self
                .tools
                .storage()
                .thread(*thread_id)
                .await?
                .map(|thread| thread.title)
                .unwrap_or_else(|| "Unavailable Thread".into());
        }
        Ok((label, Some(record.subscription_key.clone())))
    }

    pub(super) async fn cancel_process(&self, process_id: &str) -> anyhow::Result<()> {
        let snapshot = self
            .core
            .processes()
            .session_snapshot(&self.session_id)
            .await?;
        anyhow::ensure!(
            snapshot
                .items
                .iter()
                .any(|item| item.process.process_id == process_id
                    && matches!(
                        item.process.lifecycle,
                        lash_core::ProcessStatus::Running | lash_core::ProcessStatus::Waiting
                    )),
            "process is not running in this Thread"
        );
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
        anyhow::ensure!(
            record.enabled && !record.tombstoned && super::process_projection::recurring(&record),
            "trigger is not live and recurring"
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
            origin: MessageOrigin::Process {
                process_id: item.process.process_id.clone(),
                name: item.label.clone(),
                trigger: TriggerLabel::Other {
                    key: "direct start".into(),
                },
                subscription_key: None,
                outcome: match item.process.lifecycle {
                    lash_core::ProcessStatus::Failed => ProcessOutcome::Failed,
                    lash_core::ProcessStatus::Cancelled => ProcessOutcome::Cancelled,
                    _ => ProcessOutcome::Completed,
                },
                result: terminal_result(&event.payload),
                error: (item.process.lifecycle == lash_core::ProcessStatus::Failed)
                    .then(|| terminal_error(&event.payload)),
            },
        },
    ))
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

pub(super) fn timestamp(milliseconds: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(milliseconds.min(i64::MAX as u64) as i64)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

pub(super) fn terminal_result(payload: &Value) -> Value {
    payload
        .get("await_output")
        .cloned()
        .and_then(|value| serde_json::from_value::<ProcessAwaitOutput>(value).ok())
        .map(ProcessAwaitOutput::into_tool_output)
        .map(|output| output.into_value_for_projection())
        .unwrap_or_else(|| payload.clone())
}

pub(super) fn bound_text(value: &str) -> String {
    let mut text = value.chars().take(PROCESS_RESULT_CHARS).collect::<String>();
    if text.len() < value.len() {
        text.push('…');
    }
    text
}

fn terminal_error(payload: &Value) -> String {
    let result = terminal_result(payload);
    result
        .pointer("/error/message")
        .or_else(|| result.get("message"))
        .or_else(|| result.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| crate::storage::process_deliveries::result_body(&result))
}

/// Delivery reservations retain the exact registration revision, including
/// one-shot subscriptions that were deleted before their process completed.
pub(super) async fn trigger_registration(
    store: &dyn lash_core::TriggerStore,
    session_id: &str,
    process_id: &str,
    subscription_id: &str,
) -> anyhow::Result<Option<lash_core::TriggerSubscriptionRecord>> {
    Ok(store
        .list_deliveries_by_process_id(process_id)
        .await?
        .into_iter()
        .map(|delivery| delivery.subscription)
        .find(|record| {
            record.subscription_id == subscription_id
                && record.registrant_session_id() == Some(session_id)
        }))
}

pub(super) fn structured_trigger(record: &lash_core::TriggerSubscriptionRecord) -> TriggerLabel {
    let value = record
        .source
        .get("$lash_host_descriptor_value")
        .unwrap_or(&Value::Null);
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    match record.source_type.as_str() {
        TIMER_SOURCE_TYPE => TriggerLabel::Timer {
            label: text("label")
                .or_else(|| record.name.clone())
                .unwrap_or_else(|| "timer".into()),
            in_secs: value.get("in_secs").and_then(Value::as_u64),
            every_secs: value.get("every_secs").and_then(Value::as_u64),
            at: text("at"),
        },
        "cron.Schedule" => TriggerLabel::Cron {
            expr: text("expr").unwrap_or_default(),
            tz: text("tz"),
        },
        source if source.starts_with("thread.") => {
            match value.get("thread_id").and_then(Value::as_u64) {
                Some(thread_id) => TriggerLabel::Thread {
                    event: match source {
                        "thread.Reported" => "thread.Report",
                        "thread.Completed" => "thread.Complete",
                        "thread.Messaged" => "thread.Message",
                        "thread.Turned" => "thread.Turn",
                        other => other,
                    }
                    .into(),
                    thread_id,
                    title: String::new(),
                },
                None => TriggerLabel::Other { key: source.into() },
            }
        }
        source => TriggerLabel::Other { key: source.into() },
    }
}

#[cfg(test)]
mod result_tests {
    use super::*;
    #[test]
    fn failed_await_output_becomes_plain_error_text() {
        let output = lash_core::ToolCallOutput::failure(lash_core::ToolFailure::io(
            "denied",
            "Permission denied",
        ));
        let payload = json!({"await_output": ProcessAwaitOutput::Settled { output }});
        assert_eq!(terminal_error(&payload), "Permission denied");
    }
}
