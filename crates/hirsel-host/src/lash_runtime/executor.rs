use super::*;

#[derive(Clone)]
pub(super) struct HirselToolExecutor {
    pub(super) tools: ToolSuite,
    pub(super) anchors: Arc<Mutex<TurnAnchorState>>,
    /// Installed after boot to use Lash's process wait without retaining a
    /// core -> tool provider -> core ownership cycle.
    pub(super) runtime: Arc<std::sync::OnceLock<std::sync::Weak<LashAgentRuntime>>>,
}

pub(super) struct HirselToolProvider {
    pub(super) executor: HirselToolExecutor,
}

impl HirselToolProvider {
    pub(super) fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions =
            hirsel_tool_definitions(&self.executor.tools.subagent_model_snapshot());
        definitions.extend(self.executor.tools.plugin_tools().definitions());
        definitions
    }
}

#[async_trait]
impl ToolProvider for HirselToolProvider {
    fn tool_manifests(&self) -> Vec<ToolManifest> {
        self.definitions()
            .iter()
            .map(ToolDefinition::manifest)
            .collect()
    }

    fn resolve_contract(&self, name: &str) -> Option<Arc<ToolContract>> {
        self.definitions()
            .into_iter()
            .find(|definition| definition.name() == name)
            .map(|definition| Arc::new(definition.contract()))
    }

    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        StaticToolExecute::execute(&self.executor, call).await
    }

    async fn execute_attempt(&self, call: ToolCall<'_>) -> lash_core::ToolAttemptOutcome {
        StaticToolExecute::execute_attempt(&self.executor, call).await
    }
}

#[async_trait]
impl StaticToolExecute for HirselToolExecutor {
    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        match self.execute_inner(call).await {
            Ok(outcome) => ToolOutcome::ok(outcome.value),
            Err(error) => ToolOutcome::err_fmt(error),
        }
    }

    /// Durable follow-on work — starting, cancelling, or signalling a process —
    /// is declared, not performed: lash admits a declaration only after the
    /// attempt itself is recorded, so a tool that crashed mid-body can never
    /// leave a half-started process behind.
    async fn execute_attempt(&self, call: ToolCall<'_>) -> lash_core::ToolAttemptOutcome {
        match self.execute_inner(call).await {
            Ok(outcome) => lash_core::ToolAttemptOutcome::done(
                lash_core::ToolOutcomeDone::ok(outcome.value),
                lash_core::ToolIntents::v1(outcome.intents),
            ),
            Err(error) => lash_core::ToolAttemptOutcome::done_without_intents(
                lash_core::ToolOutcomeDone::failure(lash_core::ToolFailure {
                    class: lash_core::ToolFailureClass::Execution,
                    code: "tool_error".to_string(),
                    message: error,
                    source: lash_core::ToolFailureSource::Tool,
                    retry: lash_core::ToolRetryStatus::Never,
                    raw: None,
                }),
            ),
        }
    }
}

/// A completed hirsel tool body: the value the model sees, plus any durable
/// declarations the host wants admitted once the attempt is recorded.
pub(super) struct HirselToolOutcome {
    pub(super) value: Value,
    pub(super) intents: Vec<lash_core::ToolIntent>,
}

impl HirselToolOutcome {
    pub(super) fn value(value: Value) -> Self {
        Self {
            value,
            intents: Vec::new(),
        }
    }

    pub(super) fn with_intents(value: Value, intents: Vec<lash_core::ToolIntent>) -> Self {
        Self { value, intents }
    }
}

impl From<Value> for HirselToolOutcome {
    fn from(value: Value) -> Self {
        Self::value(value)
    }
}

impl HirselToolExecutor {
    pub(super) async fn execute_inner(
        &self,
        call: ToolCall<'_>,
    ) -> Result<HirselToolOutcome, String> {
        let outcome: HirselToolOutcome = match call.name {
            "artifacts_create" | "artifacts_edit" | "artifacts_show" => self
                .artifact_mutation(call.name, call.args, call.context)
                .await?
                .into(),
            "artifacts_list" => self.artifacts_list(call.args).await?.into(),
            "threads_create" => self.threads_create(call.args).await?.into(),
            "threads_update" => self.threads_update(call.args).await?.into(),
            "threads_list" => self.threads_list().await?.into(),
            "threads_read" => self.threads_read(call.args).await?.into(),
            "threads_activity" => self.threads_activity(call.args).await?.into(),
            "views_show" => self.views_show(call.args).await?.into(),
            "views_update" => self.views_update(call.args).await?.into(),
            "views_clear" => self.views_clear(call.args).await?.into(),
            "views_list_templates" => self.views_list_templates().await?.into(),
            "subagents_spawn" => self.subagents_spawn(call.args, call.context).await?,
            "subagents_prompt" => self.subagents_prompt(call.args).await?.into(),
            "subagents_interrupt" => self.subagents_interrupt(call.args).await?.into(),
            "subagents_list" => self.subagents_list().await?.into(),
            "subagents_progress" => self.subagents_progress(call.args).await?.into(),
            "subagents_wait" => self.subagents_wait(call.args).await?.into(),
            "monitors_create" => self.monitors_create(call.args, call.context).await?,
            "monitors_list" => self.monitors_list().await?.into(),
            "monitors_cancel" => self.monitors_cancel(call.args, call.context).await?,
            "shell_run" => self.shell_run(call.args).await?.into(),
            // Plugin tools share this dispatch path with the built-ins: same
            // provider, same recorded attempt, same failure shape. Only the
            // namespace (`plugin__<id>__<name>`) and the 120s handler timeout
            // are plugin-specific, and both live in the registry.
            other => match self
                .tools
                .plugin_tools()
                .call(other, call.args.clone())
                .await
            {
                Some(result) => result?.into(),
                None => return Err(format!("Unknown tool: {other}")),
            },
        };
        Ok(outcome)
    }

    pub(super) async fn views_show(&self, args: &Value) -> Result<Value, String> {
        let template_id = optional_string(args, "template_id")?;
        let spec = args.get("spec").cloned().filter(|value| !value.is_null());
        let params = args.get("params").cloned().filter(|value| !value.is_null());
        let instance_id = optional_string(args, "instance_id")?;
        let placement = required_string(args, "placement")?;
        let view = self
            .tools
            .views_show(template_id, spec, params, instance_id, placement)
            .await
            .map_err(|error| error.to_string())?;
        Ok(view_instance_result(&view))
    }

    pub(super) async fn views_update(&self, args: &Value) -> Result<Value, String> {
        let instance_id = required_string(args, "instance_id")?;
        let params = args.get("params").cloned().filter(|value| !value.is_null());
        let patch = args.get("patch").cloned().filter(|value| !value.is_null());
        let view = self
            .tools
            .views_update(&instance_id, params, patch)
            .await
            .map_err(|error| error.to_string())?;
        Ok(view_instance_result(&view))
    }

    pub(super) async fn views_clear(&self, args: &Value) -> Result<Value, String> {
        let instance_id = required_string(args, "instance_id")?;
        self.tools
            .views_clear(&instance_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true, "instance_id": instance_id }))
    }

    pub(super) async fn views_list_templates(&self) -> Result<Value, String> {
        let templates = self
            .tools
            .views_list_templates()
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(templates).map_err(|error| error.to_string())
    }

    pub(super) async fn subagents_spawn(
        &self,
        args: &Value,
        context: &lash::tools::AttemptContext<'_>,
    ) -> Result<HirselToolOutcome, String> {
        let agent = parse_agent_kind(
            args.get("agent")
                .and_then(Value::as_str)
                .unwrap_or("claude"),
        )?;
        let model = optional_string(args, "model")?;
        let variant = optional_string(args, "variant")?.or(optional_string(args, "effort")?);
        let prompt = required_string_any(args, &["prompt", "task"])?;
        let cwd = optional_path(args, "cwd")?
            .map(Ok)
            .unwrap_or_else(std::env::current_dir)
            .map_err(|error| format!("failed to resolve cwd: {error}"))?;
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        // The tool declares the Sub-agent; it never starts the Driver itself.
        // A declaration is admitted only once this attempt is recorded, so a
        // Driver started here would live in a window with no registry row —
        // long enough for a fast failure's terminal event to be dropped as an
        // unknown process id, and long enough for a crash to replay the body
        // and spawn a second Driver. `HirselSubagentEngine::run` starts it
        // after admission instead, against the id this result already names.
        let request = subagent_start_request(
            &process_id,
            context.session_id(),
            json!({
                "agent": agent,
                "model": model,
                "variant": variant,
                "prompt": prompt,
                "cwd": cwd,
            }),
            context.process_execution_env_spec(),
        );
        Ok(HirselToolOutcome::with_intents(
            subagent_spawn_result(&process_id),
            vec![lash_core::ToolIntent::StartProcess(Box::new(
                lash_core::StartProcessIntent {
                    session_id: context.session_id().to_string(),
                    request,
                    on_parent_end: lash_core::ProcessParentEndPolicy::Abandon,
                },
            ))],
        ))
    }

    pub(super) async fn subagents_prompt(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let text = required_string_any(args, &["text", "prompt", "message"])?;
        self.tools
            .subagents_prompt_process(&process_id, text)
            .await
            .map_err(|error| error.to_string())?;
        Ok(acknowledgement_result())
    }

    pub(super) async fn subagents_interrupt(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        self.tools
            .subagents_interrupt_process(&process_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(acknowledgement_result())
    }

    pub(super) async fn subagents_list(&self) -> Result<Value, String> {
        let processes = self
            .tools
            .subagents_list()
            .map_err(|error| error.to_string())?;
        subagents_list_result(&processes)
    }

    pub(super) async fn subagents_progress(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let process = self
            .tools
            .subagents_process(&process_id)
            .map_err(|error| error.to_string())?;
        let events = self
            .tools
            .subagents_progress(&process_id)
            .map_err(|error| error.to_string())?;
        subagents_progress_result(process.as_ref(), &events)
    }

    pub(super) async fn subagents_wait(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let runtime = self
            .runtime
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| "agent runtime is unavailable".to_string())?;
        let outcome = runtime
            .core
            .processes()
            .await_output(&process_id)
            .await
            .map_err(|error| error.to_string())?;
        subagents_wait_result(&process_id, &outcome)
    }

    pub(super) async fn shell_run(&self, args: &Value) -> Result<Value, String> {
        let cmd = required_string(args, "cmd")?;
        let cwd = optional_path(args, "cwd")?;
        let timeout_secs = args.get("timeout_secs").and_then(Value::as_u64);
        let output = self
            .tools
            .shell_run(cmd, cwd, timeout_secs)
            .await
            .map_err(|error| error.to_string())?;
        shell_run_result(&output)
    }

    pub(super) async fn monitors_create(
        &self,
        args: &Value,
        context: &lash::tools::AttemptContext<'_>,
    ) -> Result<HirselToolOutcome, String> {
        let cmd = required_string(args, "cmd")?;
        let every_secs = args
            .get("every_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .max(30);
        let wake_on = parse_monitor_wake_on(required_string(args, "wake_on")?.as_str())?;
        let pattern = optional_string(args, "pattern")?;
        if matches!(wake_on, MonitorWakeOn::Regex) && pattern.is_none() {
            return Err("pattern is required when wake_on is regex".to_string());
        }
        let label = required_string(args, "label")?;
        let record = self
            .tools
            .monitors_create(cmd, every_secs, wake_on, pattern, label)
            .await
            .map_err(|error| error.to_string())?;
        let request = monitor_start_request(
            &record,
            context.session_id(),
            context.process_execution_env_spec(),
        );
        Ok(HirselToolOutcome::with_intents(
            monitors_create_result(&record)?,
            vec![lash_core::ToolIntent::StartProcess(Box::new(
                lash_core::StartProcessIntent {
                    session_id: context.session_id().to_string(),
                    request,
                    on_parent_end: lash_core::ProcessParentEndPolicy::Cancel,
                },
            ))],
        ))
    }

    pub(super) async fn monitors_list(&self) -> Result<Value, String> {
        let monitors = self
            .tools
            .monitors_list()
            .await
            .map_err(|error| error.to_string())?;
        monitors_list_result(&monitors)
    }

    pub(super) async fn monitors_cancel(
        &self,
        args: &Value,
        context: &lash::tools::AttemptContext<'_>,
    ) -> Result<HirselToolOutcome, String> {
        let monitor_id = required_string_any(args, &["monitor_id", "process_id", "id"])?;
        let record = self
            .tools
            .monitors_cancel(&monitor_id)
            .await
            .map_err(|error| error.to_string())?;
        if record.is_none() {
            return Err(format!("monitor not found: {monitor_id}"));
        }
        Ok(HirselToolOutcome::with_intents(
            monitors_cancel_result(&monitor_id),
            vec![lash_core::ToolIntent::CancelProcess(
                lash_core::CancelProcessIntent {
                    session_id: context.session_id().to_string(),
                    process_id: monitor_id,
                    reason: Some("monitor cancelled by the agent".to_string()),
                },
            )],
        ))
    }
}

pub(super) fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{key}`"))
}

pub(super) fn required_string_any(args: &Value, keys: &[&str]) -> Result<String, String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{}`", keys.join("` or `")))
}

pub(super) fn optional_string_any_allow_empty(
    args: &Value,
    keys: &[&str],
) -> Result<Option<String>, String> {
    let Some((key, value)) = keys
        .iter()
        .find_map(|key| args.get(*key).map(|value| (*key, value)))
    else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::to_string)
        .map(Some)
        .ok_or_else(|| format!("field `{key}` must be a string"))
}

pub(super) fn required_u64_any(args: &Value, keys: &[&str]) -> Result<u64, String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_u64))
        .ok_or_else(|| format!("missing required integer field `{}`", keys.join("` or `")))
}

pub(super) fn optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
            .map(Some)
            .ok_or_else(|| format!("field `{key}` must be a non-empty string")),
    }
}

pub(super) fn optional_path(args: &Value, key: &str) -> Result<Option<PathBuf>, String> {
    args.get(key)
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| format!("field `{key}` must be a string path"))
        })
        .transpose()
}

pub(super) fn parse_agent_kind(value: &str) -> Result<AgentKind, String> {
    match value {
        "claude" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        other => Err(format!("agent must be claude or codex, got `{other}`")),
    }
}

pub(super) fn parse_monitor_wake_on(value: &str) -> Result<MonitorWakeOn, String> {
    match value {
        "changed" => Ok(MonitorWakeOn::Changed),
        "exit_zero" => Ok(MonitorWakeOn::ExitZero),
        "exit_nonzero" => Ok(MonitorWakeOn::ExitNonzero),
        "regex" => Ok(MonitorWakeOn::Regex),
        other => Err(format!(
            "wake_on must be changed, exit_zero, exit_nonzero, or regex, got `{other}`"
        )),
    }
}
