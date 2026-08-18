use super::*;

impl LashAgentRuntime {
    pub(super) fn spawn_timer_trigger_source(
        self: &Arc<Self>,
        trigger_store: Arc<dyn TriggerStore>,
    ) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SNOOZE_TICK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = runtime.tools.return_expired_snoozes().await {
                    tracing::warn!(%error, "snoozed event return poll failed");
                }
                if let Err(error) = runtime.fire_due_timers(Arc::clone(&trigger_store)).await {
                    tracing::warn!(%error, "timer trigger source poll failed");
                }
            }
        });
    }

    pub(super) async fn fire_due_timers(
        &self,
        trigger_store: Arc<dyn TriggerStore>,
    ) -> anyhow::Result<()> {
        let mut filter = TriggerSubscriptionFilter::for_session(&self.session_id);
        filter.source_type = Some(TIMER_SOURCE_TYPE.to_string());
        filter.enabled = Some(true);
        let records = trigger_store.list_subscriptions(filter).await?;
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        for record in records {
            let schedule = match TimerSchedule::from_registration(&record) {
                Ok(schedule) => schedule,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        subscription_key = %record.subscription_key,
                        source_key = %record.source_key,
                        "invalid timer trigger registration"
                    );
                    continue;
                }
            };
            let Some(occurrence) = schedule.due_occurrence(&record, now_ms) else {
                continue;
            };
            if let Err(error) = self
                .emit_timer_occurrence(Arc::clone(&trigger_store), &record, occurrence)
                .await
            {
                tracing::warn!(
                    %error,
                    subscription_key = %record.subscription_key,
                    source_key = %record.source_key,
                    "failed to emit timer trigger occurrence"
                );
            }
        }
        Ok(())
    }

    pub(super) async fn emit_timer_occurrence(
        &self,
        trigger_store: Arc<dyn TriggerStore>,
        record: &lash_core::TriggerSubscriptionRecord,
        occurrence: TimerOccurrence,
    ) -> anyhow::Result<()> {
        let fired_at = Utc::now();
        let digest_label = scheduled_digest_label(&occurrence.label).map(str::to_string);
        let payload = json!({
            "label": occurrence.label,
            "fired_at": fired_at.to_rfc3339(),
            "scheduled_at": timestamp_ms_rfc3339(occurrence.scheduled_at_ms),
            "source_key": record.source_key,
            "subscription_key": record.subscription_key,
        });
        let report = self
            .core
            .triggers()
            .emit(
                lash::triggers::TriggerOccurrenceRequest::new(
                    TIMER_SOURCE_TYPE,
                    record.source_key.clone(),
                    payload,
                    occurrence.idempotency_key,
                )
                .with_source(record.source.clone()),
                inline_trigger_scope(format!(
                    "timer:{}:{}",
                    record.source_key, occurrence.scheduled_at_ms
                )),
            )
            .await?;
        if !report.deliveries.is_empty() {
            if let Some(label) = digest_label {
                self.tools
                    .emit_scheduled_digest(
                        record.source_key.clone(),
                        format!(
                            "Scheduled digest `{label}` fired at {}.",
                            fired_at.to_rfc3339()
                        ),
                        "scheduled lash job completed",
                    )
                    .await?;
            }
            self.notify.notify_one();
        }
        if occurrence.one_shot {
            // Subscription mutation is a fenced, receipted command now: a
            // delete names the owner scope, the actor, and the revision it
            // expects, so a concurrent update cannot be silently clobbered.
            let _receipt = trigger_store
                .execute_command(
                    &format!("timer:one-shot-delete:{}", record.subscription_key),
                    lash::triggers::TriggerCommand::Delete {
                        owner_scope: record.owner_scope.clone(),
                        actor: record.registrant.clone(),
                        subscription_key: record.subscription_key.clone(),
                        expected_revision: record.revision,
                    },
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct TimerSchedule {
    pub(super) label: String,
    pub(super) at_ms: Option<u64>,
    pub(super) in_secs: Option<u64>,
    pub(super) every_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct TimerOccurrence {
    pub(super) label: String,
    pub(super) scheduled_at_ms: u64,
    pub(super) idempotency_key: String,
    pub(super) one_shot: bool,
}

impl TimerSchedule {
    pub(super) fn from_registration(
        record: &lash_core::TriggerSubscriptionRecord,
    ) -> Result<Self, String> {
        let descriptor_type = record
            .source
            .get("$lash_host_descriptor_type")
            .and_then(Value::as_str);
        if descriptor_type != Some(TIMER_SOURCE_TYPE) {
            return Err(format!(
                "expected descriptor type `{TIMER_SOURCE_TYPE}`, got `{}`",
                descriptor_type.unwrap_or("<missing>")
            ));
        }
        let value = record
            .source
            .get("$lash_host_descriptor_value")
            .ok_or_else(|| "missing timer schedule descriptor value".to_string())?;
        let label = value
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|label| !label.trim().is_empty())
            .or_else(|| record.name.clone())
            .ok_or_else(|| "timer.Schedule requires non-empty `label`".to_string())?;
        let at_ms = value.get("at").map(parse_timer_at_ms).transpose()?;
        let in_secs = value.get("in_secs").map(parse_timer_secs).transpose()?;
        let every_secs = value
            .get("every_secs")
            .map(parse_timer_secs)
            .transpose()?
            .map(|secs| secs.max(TIMER_MIN_RECURRING_SECS));
        let configured = [at_ms.is_some(), in_secs.is_some(), every_secs.is_some()]
            .into_iter()
            .filter(|present| *present)
            .count();
        if configured != 1 {
            return Err(
                "timer.Schedule requires exactly one of `at`, `in_secs`, or `every_secs`"
                    .to_string(),
            );
        }
        Ok(Self {
            label,
            at_ms,
            in_secs,
            every_secs,
        })
    }

    pub(super) fn due_occurrence(
        &self,
        record: &lash_core::TriggerSubscriptionRecord,
        now_ms: u64,
    ) -> Option<TimerOccurrence> {
        if let Some(every_secs) = self.every_secs {
            let interval_ms = every_secs.saturating_mul(1_000);
            let first_due_ms = record.created_at_ms.saturating_add(interval_ms);
            if now_ms < first_due_ms {
                return None;
            }
            let period_index = now_ms
                .saturating_sub(record.created_at_ms)
                .checked_div(interval_ms)
                .unwrap_or(0);
            if period_index == 0 {
                return None;
            }
            let scheduled_at_ms = record
                .created_at_ms
                .saturating_add(period_index.saturating_mul(interval_ms));
            return Some(TimerOccurrence {
                label: self.label.clone(),
                scheduled_at_ms,
                idempotency_key: format!("timer:{}:every:{period_index}", record.source_key),
                one_shot: false,
            });
        }

        let scheduled_at_ms = self
            .at_ms
            .or_else(|| {
                self.in_secs.map(|secs| {
                    record
                        .created_at_ms
                        .saturating_add(secs.saturating_mul(1_000))
                })
            })
            .expect("one-shot schedule was validated");
        (now_ms >= scheduled_at_ms).then(|| TimerOccurrence {
            label: self.label.clone(),
            scheduled_at_ms,
            idempotency_key: format!("timer:{}:once:{scheduled_at_ms}", record.source_key),
            one_shot: true,
        })
    }
}

pub(super) fn parse_timer_secs(value: &Value) -> Result<u64, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .filter(|secs| *secs > 0)
            .ok_or_else(|| "timer seconds must be a positive integer".to_string()),
        other => Err(format!("timer seconds must be an integer, got {other}")),
    }
}

pub(super) fn parse_timer_at_ms(value: &Value) -> Result<u64, String> {
    let text = value
        .as_str()
        .ok_or_else(|| "`at` must be an RFC3339 timestamp string".to_string())?;
    let ts = DateTime::parse_from_rfc3339(text)
        .map_err(|error| format!("invalid `at` timestamp `{text}`: {error}"))?
        .with_timezone(&Utc)
        .timestamp_millis();
    Ok(ts.max(0) as u64)
}

pub(super) fn timestamp_ms_rfc3339(timestamp_ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms as i64)
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| timestamp_ms.to_string())
}
