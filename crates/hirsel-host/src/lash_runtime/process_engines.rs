use super::*;

#[derive(Clone)]
pub(super) struct HirselSubagentEngine {
    pub(super) tools: ToolSuite,
}

#[async_trait]
impl ProcessEngine for HirselSubagentEngine {
    fn kind(&self) -> &'static str {
        HIRSEL_SUBAGENT_ENGINE
    }

    async fn validate_start(
        &self,
        _context: ProcessEngineValidationContext<'_>,
        payload: &Value,
        _env_spec: Option<&lash::process::ProcessExecutionEnvSpec>,
    ) -> Result<(), PluginError> {
        SubagentProcessPayload::from_value(payload).map(|_| ())
    }

    async fn run(
        &self,
        context: ProcessEngineRunContext<'_>,
        payload: Value,
    ) -> Result<ProcessRunOutcome, lash_core::ProcessInfraError> {
        let process_id = context.registration().id.to_string();
        let payload = match SubagentProcessPayload::from_value(&payload) {
            Ok(payload) => payload,
            Err(error) => return Ok(cancelled_await_output(error.to_string()).into()),
        };
        if let Err(error) = self
            .tools
            .subagents_spawn_with_process_id(
                payload.agent,
                payload.model,
                payload.variant,
                payload.prompt,
                payload.cwd,
                process_id.clone(),
            )
            .await
        {
            return Ok(cancelled_await_output(format!(
                "failed to start Sub-agent Driver: {error}"
            ))
            .into());
        }
        Ok(
            match context.processes().await_terminal(&process_id).await {
                Ok(output) => output.into(),
                Err(error) => {
                    cancelled_await_output(format!("failed to await Sub-agent: {error}")).into()
                }
            },
        )
    }

    fn identity(&self, payload: &Value) -> ProcessIdentity {
        let label = SubagentProcessPayload::from_value(payload)
            .ok()
            .map(|payload| format!("{:?}: {}", payload.agent, short_label(&payload.prompt)));
        ProcessIdentity::new(HIRSEL_SUBAGENT_ENGINE).with_label(label)
    }
}

#[derive(Clone)]
pub(super) struct HirselMonitorEngine {
    pub(super) tools: ToolSuite,
    pub(super) notify: Arc<Notify>,
}

impl HirselMonitorEngine {
    /// Load the monitor spec, mapping "gone" and "cancelled" states onto the
    /// terminal outcome the engine loop returns for them.
    pub(super) async fn active_monitor(
        &self,
        monitor_id: &str,
    ) -> Result<MonitorRecord, Box<ProcessRunOutcome>> {
        match self.tools.monitor(monitor_id).await {
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
        MonitorProcessPayload::from_value(payload).map(|_| ())
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
                .record_monitor_tick(
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
            if let Err(error) = append_monitor_wake(&processes, &updated, &tick, &self.notify).await
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

pub(super) struct SubagentProcessPayload {
    pub(super) agent: AgentKind,
    pub(super) model: Option<String>,
    pub(super) variant: Option<String>,
    pub(super) prompt: String,
    pub(super) cwd: PathBuf,
}

impl SubagentProcessPayload {
    pub(super) fn from_value(value: &Value) -> Result<Self, PluginError> {
        let agent = parse_agent_kind(value.get("agent").and_then(Value::as_str).unwrap_or(""))
            .map_err(PluginError::Session)?;
        let model = optional_string(value, "model").map_err(PluginError::Session)?;
        let variant = optional_string(value, "variant").map_err(PluginError::Session)?;
        let prompt = required_string(value, "prompt").map_err(PluginError::Session)?;
        let cwd = optional_path(value, "cwd")
            .map_err(PluginError::Session)?
            .ok_or_else(|| PluginError::Session("cwd is required".to_string()))?;
        Ok(Self {
            agent,
            model,
            variant,
            prompt,
            cwd,
        })
    }
}

pub(super) fn is_hirsel_subagent_process_record(record: &lash_core::ProcessRecord) -> bool {
    match record.input.as_ref() {
        ProcessInput::Engine { kind, .. } => kind == HIRSEL_SUBAGENT_ENGINE,
        ProcessInput::External { metadata } => metadata
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == HIRSEL_SUBAGENT_ENGINE),
        _ => false,
    }
}

pub(super) fn cancelled_await_output(message: String) -> ProcessAwaitOutput {
    ProcessAwaitOutput::Cancelled {
        message,
        raw: None,
        control: None,
    }
}

pub(super) fn subagent_event_types() -> Vec<ProcessEventType> {
    vec![
        terminal_event_type(SUBAGENT_COMPLETED, ProcessStatus::Completed),
        terminal_event_type(SUBAGENT_FAILED, ProcessStatus::Failed),
        terminal_event_type(SUBAGENT_CANCELLED, ProcessStatus::Cancelled),
        terminal_event_type(SUBAGENT_ABANDONED, ProcessStatus::Abandoned),
    ]
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

/// A Sub-agent row lash executes itself, through [`HirselSubagentEngine`].
///
/// `OwnerBound` rather than `Rerunnable`: starting a Driver is not idempotent,
/// so once an owner has begun executing this row no other owner may re-run it —
/// a host that died mid-Sub-agent leaves an abandonment for the Agent to judge,
/// never a duplicate Sub-agent.
pub(super) fn subagent_start_request(
    process_id: &str,
    session_id: &str,
    payload: Value,
    env_spec: ProcessExecutionEnvSpec,
) -> ProcessStartRequest {
    ProcessStartRequest::new(
        process_id.to_string(),
        ProcessInput::Engine {
            kind: HIRSEL_SUBAGENT_ENGINE.to_string(),
            payload,
        },
        RecoveryDisposition::OwnerBound,
        ProcessOriginator::session(SessionScope::new(session_id)),
    )
    .with_env_spec(env_spec)
    .with_wake_session_id(Some(session_id.to_string()))
    .with_observers([session_id.to_string()])
    .with_event_types(subagent_event_types())
}

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
        RecoveryDisposition::Rerunnable,
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
        payload_schema: LashSchema::new(json!({
            "type": "object",
            "additionalProperties": true,
            "required": ["text", "label", "output_tail"],
            "properties": {
                "text": { "type": "string" },
                "label": { "type": "string" },
                "output_tail": { "type": "string" }
            }
        })),
        semantics: ProcessEventSemanticsSpec {
            terminal: None,
            wake: Some(ProcessWakeSpec {
                when: None,
                input: ProcessValueSelector::Pointer("/text".to_string()),
            }),
        },
    }]
}

/// Append a monitor wake event through the running process's own context,
/// which carries the execution write authority and enqueues the wake delivery
/// itself — the raw-registry append this replaced had to redo both by hand.
pub(super) async fn append_monitor_wake(
    processes: &lash_core::facade_support::ProcessEngineProcessContext,
    record: &MonitorRecord,
    tick: &crate::monitors::MonitorTick,
    notify: &Notify,
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
    let request = ProcessEventAppendRequest::new(
        MONITOR_WAKE_EVENT,
        json!({
            "text": text,
            "label": record.label,
            "output_tail": output_tail(&tick.probe.output, 4 * 1024),
        }),
    )
    .with_replay_key(format!("hirsel-monitor:{}:{run_key}", record.id));
    processes.emit(request).await?;
    notify.notify_one();
    Ok(())
}

pub(super) fn terminal_event_type(name: &str, status: ProcessStatus) -> ProcessEventType {
    ProcessEventType {
        name: name.to_string(),
        payload_schema: LashSchema::new(json!({
            "type": "object",
            "additionalProperties": true,
            "required": ["text", "await_output"],
            "properties": {
                "text": { "type": "string" },
                "await_output": { "type": "object" }
            }
        })),
        semantics: ProcessEventSemanticsSpec {
            terminal: Some(ProcessTerminalSpec {
                status,
                await_output: Some(ProcessValueSelector::Pointer("/await_output".to_string())),
            }),
            wake: Some(ProcessWakeSpec {
                when: None,
                input: ProcessValueSelector::Pointer("/text".to_string()),
            }),
        },
    }
}

pub(super) fn terminal_event_payload(outcome: &TerminalOutcome) -> (&'static str, Value) {
    match outcome {
        TerminalOutcome::Done { summary } => (
            SUBAGENT_COMPLETED,
            json!({
                "text": format!("Sub-agent completed: {summary}"),
                "await_output": {
                    "type": "success",
                    "value": { "summary": summary },
                }
            }),
        ),
        TerminalOutcome::Failed { reason } => (
            SUBAGENT_FAILED,
            json!({
                "text": format!("Sub-agent failed: {reason}"),
                "await_output": {
                    "type": "failure",
                    "class": "execution",
                    "code": "subagent_failed",
                    "message": reason,
                    "raw": { "reason": reason },
                }
            }),
        ),
        TerminalOutcome::Interrupted => (
            SUBAGENT_CANCELLED,
            json!({
                "text": "Sub-agent was interrupted.",
                "await_output": {
                    "type": "cancelled",
                    "message": "Sub-agent was interrupted.",
                }
            }),
        ),
    }
}

pub(super) fn subagent_abandoned_payload() -> Value {
    json!({
        "text": "Sub-agent was abandoned after host restart.",
        "await_output": {
            "type": "abandoned",
            "evidence": {
                "writer": "reconciled_request",
                "owner": null,
                "epoch_ms": Utc::now().timestamp_millis().max(0) as u64,
            }
        }
    })
}

pub(super) fn subagent_abandoned_output() -> ProcessAwaitOutput {
    ProcessAwaitOutput::Abandoned {
        evidence: Box::new(lash_core::AbandonEvidence {
            writer: lash_core::AbandonWriter::OwnerDrain,
            owner: None,
            epoch_ms: Utc::now().timestamp_millis().max(0) as u64,
        }),
        control: None,
    }
}

pub(super) fn terminal_content(outcome: &TerminalOutcome) -> String {
    match outcome {
        TerminalOutcome::Done { summary } => {
            format!("Sub-agent completed: {summary}\n\nHow should I proceed?")
        }
        TerminalOutcome::Failed { reason } => {
            format!("Sub-agent failed: {reason}\n\nHow should I proceed?")
        }
        TerminalOutcome::Interrupted => {
            "Sub-agent was interrupted.\n\nHow should I proceed?".to_string()
        }
    }
}
