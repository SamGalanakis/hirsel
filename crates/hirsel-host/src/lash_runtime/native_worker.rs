//! Dedicated in-process Lash standard-tool worker for one accepted Thread turn.
use super::bridges::{activity_from_observation, publish_ready_timeline};
use super::*;
use crate::{
    native_coding_tools::NativeCodingTools,
    providers::{NATIVE_WORKER_DEFAULT_MODEL, NativeWorkerProviderSnapshot},
};
use hirsel_proto::ThreadTurnState;
use lash::{TurnActivity, TurnActivitySink};

const NATIVE_WORKER_TURN_BUDGET: usize = 32;
const NATIVE_WORKER_INSTRUCTION_BYTES: usize = 256 * 1024;
const NATIVE_WORKER_ERROR_BYTES: usize = 4 * 1024;
const NATIVE_WORKER_TOOL_NAMES: [&str; 4] = ["read", "edit", "write", "exec_command"];

pub(super) struct NativeWorkerTurn {
    pub(super) turn_id: u64,
    pub(super) cancel: lash::CancellationToken,
    shutdown: Mutex<()>,
    active_tools: Mutex<Option<Arc<NativeCodingTools>>>,
}

pub(super) struct NativeWorkerRunContext<'a> {
    pub(super) config: &'a RuntimeConfig,
    pub(super) tools: &'a ToolSuite,
    pub(super) capacity: Arc<tokio::sync::Semaphore>,
    pub(super) broadcaster: broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
}

struct NativeTerminalProjection {
    state: ThreadTurnState,
    output: Option<(String, Vec<hirsel_proto::ToolCallSummary>)>,
    reason: Option<String>,
}

struct NativeWorkerExecution {
    output: lash::TurnOutput,
    unowned_message_watermark: Option<u64>,
}

impl NativeWorkerTurn {
    pub(super) fn new(turn_id: u64) -> Arc<Self> {
        Arc::new(Self {
            turn_id,
            cancel: lash::CancellationToken::new(),
            shutdown: Mutex::new(()),
            active_tools: Mutex::new(None),
        })
    }

    /// Freeze tool admission and reap every owned one-shot command before the
    /// runtime task or its durable session is released.
    pub(super) async fn stop(&self) {
        self.cancel.cancel();
        self.cleanup().await;
    }

    /// Release per-turn tool resources without inventing cancellation intent.
    /// Normal completion and execution failure both need cleanup, while only
    /// an external stop request is allowed to classify the turn as cancelled.
    async fn cleanup(&self) {
        // `NativeCodingTools::shutdown` is idempotent across time, but one
        // final tool completion wakes one waiter. Serialize concurrent Host
        // cancellation and normal turn teardown so both observe full cleanup.
        let _shutdown = self.shutdown.lock().await;
        if let Some(tools) = self.active_tools.lock().await.take() {
            tools.shutdown().await;
        }
    }

    #[cfg(test)]
    pub(super) async fn install_active_tools_for_test(&self, tools: Arc<NativeCodingTools>) {
        *self.active_tools.lock().await = Some(tools);
    }

    pub(super) async fn run(
        &self,
        context: NativeWorkerRunContext<'_>,
        request: OwnerTurn,
        execution: crate::storage::ThreadExecution,
    ) -> anyhow::Result<()> {
        let tools = context.tools;
        let result = self.execute(&context, &request, execution).await;
        let unowned_message_watermark = result
            .as_ref()
            .ok()
            .and_then(|execution| execution.unowned_message_watermark);
        let cancelled = self.cancel.is_cancelled();
        self.cleanup().await;
        let abandon_session = result.is_err();
        let session_reusable = result.is_ok();

        let integrity_failure = tools.turn_timeline_integrity_failure(self.turn_id);
        let projection = match (integrity_failure.as_ref(), result) {
            (Some(reason), _) => NativeTerminalProjection {
                state: ThreadTurnState::Failed,
                output: None,
                reason: Some(reason.clone()),
            },
            (None, Ok(execution)) => native_terminal_projection(&execution.output),
            (None, Err(_error)) if cancelled => NativeTerminalProjection {
                state: ThreadTurnState::Cancelled,
                output: None,
                reason: None,
            },
            (None, Err(error)) => NativeTerminalProjection {
                state: ThreadTurnState::Failed,
                output: None,
                reason: Some(bounded_error(&error.to_string())),
            },
        };

        // Terminal delivery is an outbox operation. Once provider execution
        // stops, retry only storage projection; never rerun a model or tool.
        let mut delay = Duration::from_millis(50);
        loop {
            if abandon_session
                && let Err(error) = tools
                    .storage()
                    .abandon_native_worker_session(
                        &request.history_id,
                        request.thread_id,
                        self.turn_id,
                    )
                    .await
            {
                if let Ok(history) = tools.storage().history_id().await {
                    anyhow::ensure!(
                        history == request.history_id,
                        "native worker session abandonment belongs to a previous history"
                    );
                }
                tracing::warn!(
                    turn_id = self.turn_id,
                    %error,
                    "Retrying durable native worker session abandonment"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
                continue;
            }
            match tools
                .storage()
                .complete_thread_turn_with_failure(
                    &request.history_id,
                    self.turn_id,
                    projection.state,
                    projection.output.clone(),
                    projection.reason.as_deref(),
                )
                .await
            {
                Ok(completion) => {
                    if integrity_failure.is_some() {
                        anyhow::ensure!(
                            completion.turn.state == ThreadTurnState::Failed,
                            "timeline integrity failure lost to an earlier terminal projection"
                        );
                        tools.clear_turn_timeline_integrity_failure(self.turn_id);
                    }
                    if session_reusable
                        && let Err(error) = tools
                            .storage()
                            .mark_native_worker_conversation_seen(
                                request.thread_id,
                                self.turn_id,
                                unowned_message_watermark,
                            )
                            .await
                    {
                        tracing::warn!(
                            turn_id = self.turn_id,
                            %error,
                            "Retrying native worker conversation watermark"
                        );
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(2));
                        continue;
                    }
                    if let Some(activity) = completion.failure_activity {
                        tools.publish_thread_activity(activity).await;
                    }
                    if let Some(message) = completion.message {
                        tools.publish_thread_message(message).await;
                    }
                    tools.publish_thread_turn(completion.turn).await;
                    return Ok(());
                }
                Err(error) => {
                    if let Ok(history) = tools.storage().history_id().await {
                        anyhow::ensure!(
                            history == request.history_id,
                            "native worker completion belongs to a previous history"
                        );
                    }
                    tracing::warn!(
                        turn_id = self.turn_id,
                        %error,
                        "Retrying durable native worker terminal delivery"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(2));
                }
            }
        }
    }

    async fn execute(
        &self,
        context: &NativeWorkerRunContext<'_>,
        request: &OwnerTurn,
        execution: crate::storage::ThreadExecution,
    ) -> anyhow::Result<NativeWorkerExecution> {
        let tools = context.tools;
        let _permit = tokio::select! {
            () = self.cancel.cancelled() => anyhow::bail!("native worker turn cancelled before admission"),
            permit = context.capacity.acquire() => permit?,
        };
        let accepted = request.stored_turn(&tools.storage()).await?;
        anyhow::ensure!(
            accepted.state == ThreadTurnState::Queued,
            "native worker input is no longer queued"
        );

        let crate::storage::ThreadExecution::LashWorker {
            provider,
            model,
            variant,
            cwd,
            tool_profile,
        } = execution
        else {
            anyhow::bail!("native Lash worker execution settings required")
        };
        anyhow::ensure!(
            tool_profile == crate::storage::NATIVE_CODING_TOOL_PROFILE,
            "unsupported native worker tool profile `{tool_profile}`"
        );
        anyhow::ensure!(
            variant == "default",
            "unsupported native worker variant `{variant}`"
        );

        let stored = tools.storage().run_thread_turn(self.turn_id).await?;
        tools.publish_thread_turn(stored).await;

        // Resolution happens after capacity admission and immediately before
        // construction. A changed or removed private provider entry refuses
        // this immutable accepted turn instead of silently retargeting it.
        let resolved = tools.resolve_native_worker_provider(&provider)?;
        let provider_handle =
            openai_compatible_handle(resolved.api_key, resolved.snapshot.base_url.clone());
        let model_spec = native_worker_model_spec(&provider, &model)?;
        let coding_tools = Arc::new(NativeCodingTools::new(cwd.clone())?);
        let manifests = coding_tools.tool_manifests();
        let tool_names = manifests
            .iter()
            .map(|manifest| manifest.name.clone())
            .collect::<Vec<_>>();
        ensure_native_tool_surface(&tool_names)?;
        *self.active_tools.lock().await = Some(Arc::clone(&coding_tools));
        if self.cancel.is_cancelled() {
            self.stop().await;
            anyhow::bail!("native worker turn cancelled during construction");
        }

        let fingerprint = native_worker_profile_fingerprint(
            &provider,
            &model,
            &variant,
            &cwd,
            &tool_profile,
            &tool_names,
        )?;
        let bootstrap = tools
            .prepare_native_worker_session(
                request.thread_id,
                self.turn_id,
                &fingerprint,
                &tool_names,
            )
            .await?;

        let lash_dir = context
            .config
            .data_dir
            .join("thread-runtime")
            .join(&request.history_id)
            .join(request.thread_id.to_string())
            .join("native-worker");
        tokio::fs::create_dir_all(&lash_dir).await?;
        let store_factory = Arc::new(lash_sqlite_store::SqliteSessionStoreFactory::new(
            lash_dir.join("sessions"),
        ));
        let process_env_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("process-env.db")).await?);
        let core =
            lash::LashCore::standard_builder(lash::TurnBudget::bounded(NATIVE_WORKER_TURN_BUDGET))
                .provider(provider_handle.clone())
                .model(model_spec.clone())
                .store_factory(store_factory)
                .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
                    lash_dir.join("attachments"),
                )))
                .process_env_store(process_env_store)
                .effect_host(Arc::new(lash::durability::NativeEffectHost::default()))
                .tools(coding_tools.clone() as Arc<dyn ToolProvider>)
                .without_queued_work()
                .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
                .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
                .build(lash_core::LeaseOwnerIdentity::opaque(
                    format!("hirsel-host:native-worker:{}", local_host_id()),
                    Uuid::new_v4().to_string(),
                ))?;
        let guidance = native_worker_guidance(&cwd, bootstrap.handoff_seed.as_deref());
        let session = core
            .session(&bootstrap.session_id)
            .prompt_contribution(lash::prompt::PromptContribution::guidance(
                "Hirsel native coding worker",
                guidance,
            ))
            .open()
            .await?;
        reconcile_opened_session_provider(&session, &provider_handle, &model_spec).await?;
        let active_tool_names = session
            .observe()
            .active_tool_manifests()
            .into_iter()
            .map(|manifest| manifest.name)
            .collect::<Vec<_>>();
        ensure_native_tool_surface(&active_tool_names)?;

        let turn_id = native_physical_turn_id(request.thread_id, self.turn_id);
        tools
            .storage()
            .bind_thread_execution(
                &request.history_id,
                &bootstrap.session_id,
                &turn_id,
                self.turn_id,
            )
            .await?;
        let activity = tools
            .storage()
            .append_thread_activity(
                request.thread_id,
                Some(self.turn_id),
                "execution_started",
                &json!({
                    "agent":"lash",
                    "provider_id":provider.id,
                    "model":model,
                    "session_id":bootstrap.session_id,
                }),
            )
            .await?;
        tools.publish_thread_activity(activity).await;

        let instructions = applicable_repo_instructions(&cwd).await?;
        let input = native_worker_input(request, &cwd, &instructions);
        let sink = NativeTimelineSink::new(
            request.thread_id,
            self.turn_id,
            tools.clone(),
            context.broadcaster.clone(),
            context.broadcast_log.clone(),
        );
        let report = session
            .turn(input)
            .provider(provider_handle)
            .cancel_with_origin(
                self.cancel.clone(),
                Some(format!("hirsel-thread-turn:{}", self.turn_id)),
            )
            .turn_id(turn_id)
            .stream_to(&sink)
            .await;
        sink.finish().await;
        let output = report?;
        Ok(NativeWorkerExecution {
            output: lash::TurnOutput {
                result: output,
                activities: sink.activities().await,
            },
            unowned_message_watermark: bootstrap.unowned_message_watermark,
        })
    }
}

fn native_terminal_projection(output: &lash::TurnOutput) -> NativeTerminalProjection {
    match &output.result.outcome {
        lash::TurnOutcome::Finished(_) => NativeTerminalProjection {
            state: ThreadTurnState::Completed,
            output: turn_chat_payload(output),
            reason: None,
        },
        lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. }) => NativeTerminalProjection {
            state: ThreadTurnState::Cancelled,
            output: turn_chat_payload(output),
            reason: None,
        },
        lash::TurnOutcome::Stopped(stop) => NativeTerminalProjection {
            state: ThreadTurnState::Failed,
            output: None,
            reason: Some(bounded_error(&format!(
                "native Lash worker stopped: {stop:?}"
            ))),
        },
        lash::TurnOutcome::AgentFrameSwitch { .. } => NativeTerminalProjection {
            state: ThreadTurnState::Failed,
            output: None,
            reason: Some(
                "native Lash standard worker attempted an unsupported agent-frame switch".into(),
            ),
        },
    }
}

fn bounded_error(message: &str) -> String {
    if message.len() <= NATIVE_WORKER_ERROR_BYTES {
        return message.to_string();
    }
    const ELLIPSIS: &str = "…";
    let mut end = NATIVE_WORKER_ERROR_BYTES - ELLIPSIS.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = message[..end].to_string();
    bounded.push_str(ELLIPSIS);
    bounded
}

fn ensure_native_tool_surface(names: &[String]) -> anyhow::Result<()> {
    let actual = names.iter().map(String::as_str).collect::<HashSet<_>>();
    let expected = NATIVE_WORKER_TOOL_NAMES.into_iter().collect::<HashSet<_>>();
    anyhow::ensure!(
        actual == expected && names.len() == expected.len(),
        "native worker tool provider exposed an unexpected tool surface: {}",
        names.join(", ")
    );
    Ok(())
}

fn native_worker_model_spec(
    provider: &NativeWorkerProviderSnapshot,
    model: &str,
) -> anyhow::Result<lash::ModelSpec> {
    // This metadata was verified for one public route/model pair. Provider ids
    // are Owner-chosen labels, so an `openrouter` lookalike must not inherit it.
    let verified_openrouter_default = provider.base_url
        == lash_provider_openai::OPENROUTER_BASE_URL
        && model == NATIVE_WORKER_DEFAULT_MODEL;
    let (context, output) = if verified_openrouter_default {
        (1_048_576, Some(384_000))
    } else {
        (200_000, None)
    };
    let mut capability = lash_core::provider::ModelCapability::default();
    if verified_openrouter_default {
        capability.attachment_acceptance =
            Arc::new(lash_core::provider::AttachmentCapabilitySnapshot {
                revision: "hirsel-native-worker-images-v1".into(),
                acceptors: vec![lash_core::provider::AttachmentAcceptor {
                    // `OpenAiCompatibleProvider` drives Chat Completions, whose
                    // adapter validates against this transport-dialect label.
                    provider: "OpenAI Chat Completions".into(),
                    rules: vec![lash_core::provider::AttachmentAcceptanceRule::Mime {
                        source: lash_core::provider::AttachmentMimeSource::Inline,
                        media_types: [
                            "image/png",
                            "image/jpeg",
                            "image/gif",
                            "image/webp",
                            "image/bmp",
                        ]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                        media_families: Vec::new(),
                    }],
                }],
            });
    }
    let mut builder = lash::ModelSpec::builder(model)
        .variant(ReasoningSelection::ProviderDefault)
        .context_window_tokens(context)
        .capability(capability);
    if let Some(output) = output {
        builder = builder.output_token_capacity(output);
    }
    builder
        .build()
        .map_err(|error| anyhow::anyhow!("invalid native worker model metadata: {error}"))
}

fn native_worker_profile_fingerprint(
    provider: &NativeWorkerProviderSnapshot,
    model: &str,
    variant: &str,
    cwd: &std::path::Path,
    tool_profile: &str,
    tool_names: &[String],
) -> anyhow::Result<String> {
    let value = serde_json::to_vec(&json!({
        "provider":provider,
        "model":model,
        "variant":variant,
        "cwd":cwd,
        "tool_profile":tool_profile,
        "tool_names":tool_names,
    }))?;
    Ok(format!("{:x}", Sha256::digest(value)))
}

fn native_physical_turn_id(thread_id: u64, turn_id: u64) -> String {
    let boot_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    format!("host-queue-drain:{boot_ms}:{turn_id}:thread:{thread_id}:turn:{turn_id}")
}

fn native_worker_guidance(cwd: &std::path::Path, handoff: Option<&str>) -> String {
    let mut guidance = format!(
        "You are a focused coding worker inside one Hirsel Task. Work only on the accepted assignment and return a concise summary of changed files and verification. Your accepted working directory is `{}`; it is a default base, not a filesystem sandbox. You have exactly four tools: `read`, `edit`, `write`, and `exec_command` (the model-facing binding for semantic `shell.exec`). You cannot delegate, manage Hirsel Threads, browse the web, publish artifacts, edit coordinator settings, or mark the Task Done. Use bounded reads and command output ranges when results are truncated. Do not assume a timed-out or interrupted command completed.",
        cwd.display()
    );
    if let Some(handoff) = handoff {
        guidance.push_str("\n\n## Session handoff\n\n");
        guidance.push_str(handoff);
    }
    guidance
}

fn native_worker_input(
    request: &OwnerTurn,
    cwd: &std::path::Path,
    instructions: &str,
) -> TurnInput {
    TurnInput::text(format!(
        "Accepted assignment for Hirsel Task #{}\nWorking directory: {}\n\n## Applicable repository instructions\n\n{}\n\n## Assignment\n\n{}",
        request.thread_id,
        cwd.display(),
        if instructions.is_empty() {
            "(none found)"
        } else {
            instructions
        },
        request.body,
    ))
}

async fn applicable_repo_instructions(cwd: &std::path::Path) -> anyhow::Result<String> {
    let mut dirs = cwd
        .ancestors()
        .map(std::path::Path::to_path_buf)
        .collect::<Vec<_>>();
    dirs.reverse();
    let mut output = String::new();
    for dir in dirs {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            let path = dir.join(name);
            let contents = match tokio::fs::read_to_string(&path).await {
                Ok(contents) => contents,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("read {}", path.display()));
                }
            };
            let section = format!("### {}\n\n{}\n\n", path.display(), contents);
            let remaining = NATIVE_WORKER_INSTRUCTION_BYTES.saturating_sub(output.len());
            anyhow::ensure!(
                remaining > 0,
                "applicable repository instructions exceed 256 KiB"
            );
            anyhow::ensure!(
                section.len() <= remaining,
                "applicable repository instructions exceed 256 KiB"
            );
            output.push_str(&section);
        }
    }
    Ok(output)
}

struct NativeTimelineSink {
    thread_id: u64,
    turn_id: u64,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
    timeline: Mutex<TurnTimelineBridge>,
    activities: Mutex<Vec<TurnActivity>>,
    sequence: AtomicU64,
}

impl NativeTimelineSink {
    fn new(
        thread_id: u64,
        turn_id: u64,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
    ) -> Self {
        Self {
            thread_id,
            turn_id,
            tools,
            broadcaster,
            broadcast_log,
            timeline: Mutex::new(TurnTimelineBridge {
                thread_id: Some(thread_id),
                turn_id: Some(turn_id),
                ..TurnTimelineBridge::default()
            }),
            activities: Mutex::new(Vec::new()),
            sequence: AtomicU64::new(0),
        }
    }

    async fn activities(&self) -> Vec<TurnActivity> {
        self.activities.lock().await.clone()
    }

    async fn finish(&self) {
        let mut timeline = self.timeline.lock().await;
        timeline.finish_turn();
        publish_ready_timeline(&self.tools, &mut timeline).await;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                thread_id: self.thread_id,
                turn_id: self.turn_id,
                state: AgentActivityState::Idle,
                text: None,
            },
        );
    }

    async fn route(&self, activity: TurnActivity) {
        self.activities.lock().await.push(activity.clone());
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let remote = match lash::remote::usage::RemoteTurnActivity::from_core(sequence, activity) {
            Ok(remote) => remote,
            Err(error) => {
                let reason = format!("native worker timeline conversion failed: {error}");
                self.tools
                    .fail_turn_timeline_integrity(self.turn_id, &reason)
                    .await;
                return;
            }
        };
        let payload = RemoteSessionObservationEventPayload::TurnActivity {
            activity: Box::new(remote),
        };
        if let Some((state, text)) = activity_from_observation(&payload) {
            publish(
                &self.broadcast_log,
                &self.broadcaster,
                HostToClient::AgentActivity {
                    thread_id: self.thread_id,
                    turn_id: self.turn_id,
                    state,
                    text,
                },
            );
        }
        let mut timeline = self.timeline.lock().await;
        timeline.observe(&payload);
        publish_ready_timeline(&self.tools, &mut timeline).await;
    }
}

#[async_trait]
impl TurnActivitySink for NativeTimelineSink {
    async fn emit(&self, activity: TurnActivity) {
        self.route(activity).await;
    }

    async fn emit_for_turn(&self, _turn_id: &str, activity: TurnActivity) {
        self.route(activity).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID;

    #[tokio::test]
    async fn queued_provider_route_refusal_fails_and_reports_to_parent() {
        let (executor, storage, _log, dir) = super::super::tests::test_event_executor().await;
        let config_store = crate::host_config::ConfigStore::load(
            dir.path().join("hirsel.toml"),
            std::path::Path::new("/docs/hirsel-config.md"),
            &crate::host_config::EnvBootstrap::default(),
        )
        .await
        .unwrap();
        config_store
            .upsert_provider(&crate::host_config::StoredProvider {
                id: NATIVE_WORKER_DEFAULT_PROVIDER_ID.into(),
                label: "OpenRouter".into(),
                base_url: lash_provider_openai::OPENROUTER_BASE_URL.into(),
                api_key: Some("test-key-no-provider-call".into()),
                default_model: NATIVE_WORKER_DEFAULT_MODEL.into(),
            })
            .await
            .unwrap();
        let caller = storage.test_running_caller().await;
        let assignment = crate::storage::Delegation {
            title: "Native route refusal".into(),
            brief: "Do not reach a provider".into(),
            artifact_ids: Vec::new(),
            child_thread_id: None,
            execution: Some(crate::storage::ThreadExecution::LashWorker {
                provider: executor.tools.capture_native_worker_provider(None).unwrap(),
                model: NATIVE_WORKER_DEFAULT_MODEL.into(),
                variant: "default".into(),
                cwd: std::env::current_dir().unwrap().canonicalize().unwrap(),
                tool_profile: crate::storage::NATIVE_CODING_TOOL_PROFILE.into(),
            }),
        };
        let delegated = storage
            .delegate_thread(
                &caller,
                "native-route-refusal",
                &assignment,
                &serde_json::to_value(&assignment).unwrap(),
            )
            .await
            .unwrap();
        let (_, request) = storage
            .pending_thread_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|(_, request)| request["turn_id"].as_u64() == Some(delegated.turn_id))
            .unwrap();
        let request: OwnerTurn = serde_json::from_value(request).unwrap();
        let execution = storage.turn_execution(delegated.turn_id).await.unwrap();

        config_store
            .remove_provider(NATIVE_WORKER_DEFAULT_PROVIDER_ID)
            .await
            .unwrap();
        let boot = crate::boot_provider::BootProvider::env_default(ProviderMode::Codex);
        let providers = crate::providers::ProviderRosterState::new(
            config_store.clone(),
            &boot,
            Some(dir.path().to_owned()),
        );
        let prompts = crate::prompt_config::PromptConfig::new(
            ProviderMode::Codex,
            config_store.clone(),
            providers.clone(),
            String::new(),
        );
        let runtime_config = RuntimeConfig {
            agent_mode: AgentMode::Scripted,
            provider_mode: ProviderMode::Codex,
            boot_plan: boot.plan,
            anthropic_api_key: None,
            openrouter_api_key: None,
            model: "test-model".into(),
            data_dir: dir.path().to_owned(),
            driver_mode: DriverMode::Fake,
            config_store,
            providers,
            prompts,
        };
        let (broadcaster, _) = broadcast::channel(16);
        let turn = NativeWorkerTurn::new(delegated.turn_id);
        turn.run(
            NativeWorkerRunContext {
                config: &runtime_config,
                tools: &executor.tools,
                capacity: Arc::new(tokio::sync::Semaphore::new(1)),
                broadcaster,
                broadcast_log: BroadcastLog::default(),
            },
            request,
            execution,
        )
        .await
        .unwrap();

        assert!(
            !turn.cancel.is_cancelled(),
            "cleanup invented cancellation intent"
        );
        let child = storage
            .thread_detail(delegated.thread_id, None, 100)
            .await
            .unwrap();
        assert_eq!(child.turns[0].state, ThreadTurnState::Failed);
        let failure = child
            .activities
            .iter()
            .find(|activity| activity.kind == "execution_failed")
            .expect("child failure activity");
        assert!(
            failure.data["reason"]
                .as_str()
                .unwrap()
                .contains("is no longer configured"),
            "{}",
            failure.data
        );
        let parent = storage
            .thread_detail(caller.thread_id, None, 100)
            .await
            .unwrap();
        let report = parent
            .activities
            .iter()
            .find(|activity| activity.kind == "child_report")
            .expect("terminal report to parent");
        assert_eq!(report.data["status"], "failed");
        assert!(
            report.data["summary"]
                .as_str()
                .unwrap()
                .contains("is no longer configured"),
            "{}",
            report.data
        );
    }

    #[test]
    fn native_worker_surface_is_exact() {
        let names = NATIVE_WORKER_TOOL_NAMES.map(str::to_string);
        ensure_native_tool_surface(&names).unwrap();
        assert!(ensure_native_tool_surface(&["read".into(), "delegate".into()]).is_err());
    }

    #[test]
    fn deepseek_default_uses_verified_limits() {
        let official = NativeWorkerProviderSnapshot {
            id: NATIVE_WORKER_DEFAULT_PROVIDER_ID.into(),
            base_url: lash_provider_openai::OPENROUTER_BASE_URL.into(),
            revision: "official-route".into(),
        };
        let spec = native_worker_model_spec(&official, NATIVE_WORKER_DEFAULT_MODEL).unwrap();
        assert_eq!(spec.context_window_tokens(), 1_048_576);
        assert_eq!(
            spec.limits.output_token_capacity.map(|value| value.get()),
            Some(384_000)
        );
        assert_eq!(
            spec.capability.attachment_acceptance.acceptors[0].provider,
            "OpenAI Chat Completions"
        );

        let lookalike = NativeWorkerProviderSnapshot {
            id: NATIVE_WORKER_DEFAULT_PROVIDER_ID.into(),
            base_url: "https://openrouter.example.invalid/api/v1".into(),
            revision: "lookalike-route".into(),
        };
        let unknown = native_worker_model_spec(&lookalike, NATIVE_WORKER_DEFAULT_MODEL).unwrap();
        assert_eq!(unknown.context_window_tokens(), 200_000);
        assert!(unknown.limits.output_token_capacity.is_none());
        assert!(unknown.capability.attachment_acceptance.is_empty());

        let renamed_official = NativeWorkerProviderSnapshot {
            id: "my-router".into(),
            ..official.clone()
        };
        assert_eq!(
            native_worker_model_spec(&renamed_official, NATIVE_WORKER_DEFAULT_MODEL)
                .unwrap()
                .context_window_tokens(),
            1_048_576
        );
        assert_eq!(
            native_worker_model_spec(&official, "deepseek/another-model")
                .unwrap()
                .context_window_tokens(),
            200_000
        );
    }

    #[test]
    fn bounded_error_preserves_utf8() {
        let message = "x".repeat(NATIVE_WORKER_ERROR_BYTES - 1) + "💚";
        let bounded = bounded_error(&message);
        assert!(bounded.ends_with('…'));
        assert!(bounded.is_char_boundary(bounded.len()));
        assert!(bounded.len() <= NATIVE_WORKER_ERROR_BYTES);
    }
}
