use super::*;

#[derive(Clone)]
pub(super) struct HirselToolExecutor {
    pub(super) tools: ToolSuite,
    pub(super) anchors: Arc<Mutex<TurnAnchorState>>,
}

pub(super) struct HirselToolProvider {
    pub(super) executor: HirselToolExecutor,
    pub(super) coding: Arc<NativeCodingBinding>,
}

/// The coding operations' working directory for one Native lane.
///
/// The tools are built on first use and rebuilt when a Thread names a different
/// directory, so a lane that never touches a file never spawns a shell, and a
/// rebind reaps the previous root's owned commands rather than leaving them to
/// outlive the directory they were started in.
pub(super) struct NativeCodingBinding {
    default_cwd: PathBuf,
    state: Mutex<Option<(PathBuf, Arc<crate::native_coding_tools::NativeCodingTools>)>>,
}

impl NativeCodingBinding {
    pub(super) fn new(default_cwd: PathBuf) -> Self {
        Self {
            default_cwd,
            state: Mutex::new(None),
        }
    }

    pub(super) async fn bind(&self, cwd: &std::path::Path) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        if state.as_ref().is_some_and(|(bound, _)| bound == cwd) {
            return Ok(());
        }
        let tools = Arc::new(crate::native_coding_tools::NativeCodingTools::new(
            cwd.to_path_buf(),
        )?);
        if let Some((_, previous)) = state.replace((cwd.to_path_buf(), tools)) {
            previous.shutdown().await;
        }
        Ok(())
    }

    /// Reap every owned command and drop the root. A later call rebuilds the
    /// tools, so this is a quiesce, not a permanent teardown.
    pub(super) async fn shutdown(&self) {
        let previous = self.state.lock().await.take();
        if let Some((_, tools)) = previous {
            tools.shutdown().await;
        }
    }

    #[cfg(test)]
    pub(super) async fn tools_for_test(
        &self,
    ) -> Arc<crate::native_coding_tools::NativeCodingTools> {
        self.tools().await.expect("test working directory is valid")
    }

    async fn tools(&self) -> Result<Arc<crate::native_coding_tools::NativeCodingTools>, String> {
        let mut state = self.state.lock().await;
        if state.is_none() {
            let cwd = self.default_cwd.clone();
            let tools = Arc::new(
                crate::native_coding_tools::NativeCodingTools::new(cwd.clone())
                    .map_err(|error| error.to_string())?,
            );
            *state = Some((cwd, tools));
        }
        Ok(Arc::clone(&state.as_ref().expect("just populated above").1))
    }
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
        if crate::native_coding_tools::is_coding_tool(call.name) {
            return match self.coding.tools().await {
                Ok(tools) => tools.execute(call).await,
                Err(error) => ToolOutcome::err_fmt(error),
            };
        }
        StaticToolExecute::execute(&self.executor, call).await
    }

    async fn execute_attempt(&self, call: ToolCall<'_>) -> lash_core::ToolAttemptOutcome {
        if crate::native_coding_tools::is_coding_tool(call.name) {
            return match self.coding.tools().await {
                Ok(tools) => tools.execute_attempt(call).await,
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
            };
        }
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
    async fn caller(&self, call: &ToolCall<'_>) -> Result<crate::storage::ThreadCaller, String> {
        match call.context.runtime_process_id() {
            Some(process_id) => match call.context.process_execution_env_spec().policy.session_id {
                Some(owner_session_id) => {
                    self.tools
                        .storage()
                        .process_caller(
                            &owner_session_id,
                            process_id,
                            call.context.execution_scope_id(),
                        )
                        .await
                }
                None => Err(anyhow::anyhow!(
                    "process execution environment has no owning session"
                )),
            },
            None => {
                self.tools
                    .storage()
                    .execution_caller(call.context.session_id(), call.context.execution_scope_id())
                    .await
            }
        }
        .map_err(|error| error.to_string())
    }

    pub(super) async fn execute_inner(&self, call: ToolCall<'_>) -> Result<Value, String> {
        let caller = self.caller(&call).await?;
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

pub(crate) fn parse_agent_kind(value: &str) -> Result<hirsel_drivers::AgentKind, String> {
    match value {
        "claude" => Ok(hirsel_drivers::AgentKind::Claude),
        "codex" => Ok(hirsel_drivers::AgentKind::Codex),
        other => Err(format!("agent must be claude or codex, got `{other}`")),
    }
}
