use super::*;

#[derive(Clone)]
pub(super) struct HirselToolExecutor {
    pub(super) tools: ToolSuite,
    pub(super) anchors: Arc<Mutex<TurnAnchorState>>,
}

pub(super) struct HirselToolProvider {
    pub(super) executor: HirselToolExecutor,
}

impl HirselToolProvider {
    pub(super) fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = hirsel_tool_definitions(
            &self.executor.tools.subagent_model_snapshot(),
            &self.executor.tools.native_worker_provider_ids(),
        );
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
            Ok(outcome) => ToolOutcome::ok(outcome),
            Err(error) => ToolOutcome::err_fmt(error),
        }
    }

    async fn execute_attempt(&self, call: ToolCall<'_>) -> lash_core::ToolAttemptOutcome {
        match self.execute_inner(call).await {
            Ok(outcome) => lash_core::ToolAttemptOutcome::done_without_intents(
                lash_core::ToolOutcomeDone::ok(outcome),
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

impl HirselToolExecutor {
    pub(super) async fn execute_inner(&self, call: ToolCall<'_>) -> Result<Value, String> {
        let caller = self
            .tools
            .storage()
            .execution_caller(call.context.session_id(), call.context.execution_scope_id())
            .await
            .map_err(|e| e.to_string())?;
        let key = call
            .context
            .replay_key()
            .or_else(|| call.context.tool_call_id())
            .ok_or("tool requires execution invocation identity")?;
        let outcome = ScopedThreadTools {
            tools: self.tools.clone(),
            caller,
            operation_id: format!("{}:{key}", call.name),
        }
        .execute(call.name, call.args)
        .await?;
        Ok(outcome)
    }
}

pub(super) fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{key}`"))
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

pub(super) fn parse_monitor_condition(args: &Value) -> Result<MonitorCondition, String> {
    let wake_on = required_string(args, "wake_on")?;
    let pattern = optional_string_any_allow_empty(args, &["pattern"])?;
    MonitorCondition::parse(&wake_on, pattern).map_err(|error| error.to_string())
}
