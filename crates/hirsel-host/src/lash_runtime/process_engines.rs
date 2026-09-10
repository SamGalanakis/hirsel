use super::*;

#[derive(Clone)]
pub(super) struct HirselMonitorEngine {
    pub(super) history_id: String,
    pub(super) thread_id: u64,
    pub(super) tools: ToolSuite,
    pub(super) fork_wake: crate::fork_wake::ForkWakeHandle,
}

impl HirselMonitorEngine {
    /// Load the monitor spec, mapping "gone" and "cancelled" states onto the
    /// terminal outcome the engine loop returns for them.
    pub(super) async fn active_monitor(
        &self,
        monitor_id: &str,
    ) -> Result<MonitorRecord, Box<ProcessRunOutcome>> {
        match self
            .tools
            .storage()
            .background_monitor(&self.history_id, self.thread_id, monitor_id)
            .await
        {
            Ok(Some(record)) if record.cancelled_ts.is_none() => Ok(record),
            Ok(Some(_)) => Err(Box::new(
                cancelled_await_output("monitor cancelled".to_string()).into(),
            )),
            Ok(None) => Err(Box::new(
                cancelled_await_output("monitor spec missing".to_string()).into(),
            )),
            Err(error) => Err(Box::new(
                cancelled_await_output(format!("monitor lookup failed: {error}")).into(),
            )),
        }
    }
}

#[async_trait]
impl ProcessEngine for HirselMonitorEngine {
    fn kind(&self) -> &'static str {
        HIRSEL_MONITOR_ENGINE
    }

    async fn validate_start(
        &self,
        _context: ProcessEngineValidationContext<'_>,
        payload: &Value,
        _env_spec: Option<&lash::process::ProcessExecutionEnvSpec>,
    ) -> Result<(), PluginError> {
        let payload = MonitorProcessPayload::from_value(payload)?;
        self.active_monitor(&payload.monitor_id)
            .await
            .map(|_| ())
            .map_err(|_| {
                PluginError::Session("monitor is unavailable in this Thread history".into())
            })
    }

    async fn run(
        &self,
        context: ProcessEngineRunContext<'_>,
        payload: Value,
    ) -> Result<ProcessRunOutcome, lash_core::ProcessInfraError> {
        let payload = match MonitorProcessPayload::from_value(&payload) {
            Ok(payload) => payload,
            Err(error) => return Ok(cancelled_await_output(error.to_string()).into()),
        };
        let cancellation = context.cancellation_token();
        let processes = context.processes();
        drop(context);
        loop {
            let record = match self.active_monitor(&payload.monitor_id).await {
                Ok(record) => record,
                Err(outcome) => return Ok(*outcome),
            };
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Ok(cancelled_await_output("monitor cancelled".to_string()).into());
                }
                () = tokio::time::sleep(Duration::from_secs(record.every_secs)) => {}
            }
            let record = match self.active_monitor(&payload.monitor_id).await {
                Ok(record) => record,
                Err(outcome) => return Ok(*outcome),
            };
            let tick = run_monitor_tick(&record).await;
            let updated = match self
                .tools
                .storage()
                .record_background_monitor_tick(
                    &self.history_id,
                    self.thread_id,
                    &payload.monitor_id,
                    tick.probe.output.clone(),
                    tick.summary.clone(),
                )
                .await
            {
                Ok(Some(updated)) => updated,
                Ok(None) => {
                    return Ok(cancelled_await_output("monitor cancelled".to_string()).into());
                }
                Err(error) => {
                    tracing::warn!(%error, monitor_id = %payload.monitor_id, "failed to persist monitor tick");
                    continue;
                }
            };
            if !tick.wake {
                continue;
            }
            if let Err(error) =
                append_monitor_wake(&processes, &updated, &tick, &self.fork_wake).await
            {
                tracing::warn!(%error, monitor_id = %payload.monitor_id, "failed to append monitor wake event");
            }
        }
    }

    fn identity(&self, payload: &Value) -> ProcessIdentity {
        let label = MonitorProcessPayload::from_value(payload)
            .ok()
            .map(|payload| payload.label);
        ProcessIdentity::new(HIRSEL_MONITOR_ENGINE).with_label(label)
    }
}

pub(super) struct MonitorProcessPayload {
    pub(super) monitor_id: String,
    pub(super) label: String,
}

impl MonitorProcessPayload {
    pub(super) fn from_value(value: &Value) -> Result<Self, PluginError> {
        let monitor_id = required_string(value, "monitor_id").map_err(PluginError::Session)?;
        let label = required_string(value, "label").map_err(PluginError::Session)?;
        Ok(Self { monitor_id, label })
    }
}

pub(super) fn cancelled_await_output(message: String) -> ProcessAwaitOutput {
    ProcessAwaitOutput::Settled {
        output: lash_core::ToolCallOutput::cancelled(lash_core::ToolCancellation::runtime(message)),
    }
}

/// The execution env a host-owned start declares.
///
/// Lash refuses an `Engine` (or `ToolCall`) process registration that names no
/// captured execution env: attempts must be able to rebuild the env they run
/// under, at recovery as well as at start. A start raised inside a recorded tool
/// attempt takes the attempt's own env
/// ([`lash::tools::AttemptContext::process_execution_env_spec`]); a start the
/// host raises directly has no agent frame, so it mirrors lash's own frameless
/// fallback — default plugin options over the session's current policy.
pub(super) fn host_process_env_spec(policy: SessionPolicy) -> ProcessExecutionEnvSpec {
    ProcessExecutionEnvSpec::new(PluginOptions::default(), policy)
}

/// Capture a monitor process with the owning Thread session.
pub(super) fn monitor_start_request(
    record: &MonitorRecord,
    session_id: &str,
    env_spec: ProcessExecutionEnvSpec,
) -> ProcessStartRequest {
    ProcessStartRequest::new(
        record.id.clone(),
        ProcessInput::Engine {
            kind: HIRSEL_MONITOR_ENGINE.to_string(),
            payload: json!({
                "monitor_id": record.id,
                "label": record.label,
            }),
        },
        RecoveryContract::Rerunnable,
        ProcessOriginator::session(SessionScope::new(session_id)),
    )
    .with_env_spec(env_spec)
    .with_wake_session_id(Some(session_id.to_string()))
    .with_observers([session_id.to_string()])
    .with_event_types(monitor_event_types())
}

pub(super) fn monitor_event_types() -> Vec<ProcessEventType> {
    vec![ProcessEventType {
        name: MONITOR_WAKE_EVENT.to_string(),
        payload_schema: monitor_wake_schema(),
        semantics: ProcessEventSemanticsSpec {
            terminal: None,
            wake: None,
        },
    }]
}

fn monitor_wake_schema() -> LashSchema {
    LashSchema::new(json!({
        "type": "object",
        "additionalProperties": true,
        "required": ["text", "label", "output_tail"],
        "properties": {
            "text": { "type": "string" },
            "label": { "type": "string" },
            "output_tail": { "type": "string" }
        }
    }))
}

/// Append a monitor wake event through the running process's own context,
/// which carries the execution write authority and enqueues the wake delivery
/// itself — the raw-registry append this replaced had to redo both by hand.
pub(super) async fn append_monitor_wake(
    processes: &lash_core::facade_support::ProcessEngineProcessContext,
    record: &MonitorRecord,
    tick: &crate::monitors::MonitorTick,
    fork_wake: &crate::fork_wake::ForkWakeHandle,
) -> anyhow::Result<()> {
    let text = tick.wake_text.clone().unwrap_or_else(|| {
        format!(
            "Monitor `{}` fired.\n\n{}",
            record.label,
            output_tail(&tick.probe.output, 4 * 1024)
        )
    });
    let run_key = record
        .last_run_ts
        .map(|ts| ts.timestamp_millis().to_string())
        .unwrap_or_else(|| Utc::now().timestamp_millis().to_string());
    anyhow::ensure!(
        fork_wake.is_installed(),
        "monitor requires its Thread triage dispatcher"
    );
    let request = ProcessEventAppendRequest::new(
        MONITOR_WAKE_EVENT,
        json!({
            "text": text.clone(),
            "label": record.label,
            "output_tail": output_tail(&tick.probe.output, 4 * 1024),
        }),
    )
    .with_replay_key(format!("hirsel-monitor:{}:{run_key}", record.id));
    processes.emit(request).await?;
    anyhow::ensure!(
        fork_wake.dispatch(monitor_wake_message(record, text)),
        "monitor triage dispatcher unavailable"
    );
    Ok(())
}

pub(super) fn monitor_wake_message(
    record: &MonitorRecord,
    text: String,
) -> crate::fork_wake::WakeMessage {
    crate::fork_wake::WakeMessage::new(
        record.thread_id,
        crate::fork_wake::WakeSource::Monitor {
            monitor_id: record.id.clone(),
            label: record.label.clone(),
        },
        text,
        format!("monitor:{}", record.id),
    )
}
