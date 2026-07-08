use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures_util::StreamExt;
use hirsel_drivers::{AgentKind, TerminalOutcome};
use hirsel_proto::{AgentActivityState, HostToClient, QuickReply};
use lash::{
    PromptLayerSink, TurnInput,
    observe::RemoteSessionObservationStreamItem,
    plugins::{PluginError, PluginFactory, PluginRegistrar, PluginSessionContext, SessionPlugin},
    process::{
        ProcessAwaitOutput, ProcessAwaiter, ProcessEventAppendRequest, ProcessEventType,
        ProcessIdentity, ProcessInput, ProcessStartRequest, ProcessTerminalState,
        ProcessWakeDedupeKey, ProcessWakeDelivery, ProcessWakeSpec, RecoveryDisposition,
        SessionScope,
    },
    provider::{ProviderHandle, ProviderOptions},
    remote::{
        observations::{RemoteSessionCursor, RemoteSessionObservationEventPayload},
        usage::RemoteTurnEvent,
    },
    tools::{
        LashlangToolBinding, StaticToolExecute, StaticToolProvider, ToolCall, ToolDefinition,
        ToolDefinitionLashlangExt, ToolResult, ToolScheduling,
    },
    triggers::LashSchema,
};
use lash_core::{
    DurabilityTier, ProcessEngine, ProcessEngineRunContext, ProcessEngineValidationContext,
    ProcessEventSemanticsSpec, ProcessOriginator, ProcessTerminalSpec, ProcessValueSelector,
    SessionPolicy, plugin::ProcessEngineContributionContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast, mpsc};

use crate::{
    config::{AgentMode, DriverMode, ProviderMode},
    tools::ToolSuite,
};

const AGENT_SESSION_ID: &str = "agent";
const HIRSEL_SUBAGENT_ENGINE: &str = "hirsel_subagent";
const SUBAGENT_COMPLETED: &str = "subagent.completed";
const SUBAGENT_FAILED: &str = "subagent.failed";
const SUBAGENT_CANCELLED: &str = "subagent.cancelled";
const AGENT_PROMPT: &str = include_str!("../../../prompts/agent.md");

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub agent_mode: AgentMode,
    pub provider_mode: ProviderMode,
    pub anthropic_api_key: Option<String>,
    pub model: String,
    pub data_dir: PathBuf,
    pub driver_mode: DriverMode,
}

#[derive(Clone)]
pub struct AgentRuntime {
    backend: Arc<AgentBackend>,
}

#[derive(Debug)]
pub struct OwnerTurn {
    pub message_id: u64,
    pub client_id: String,
    pub body: String,
    pub anchor: Option<u64>,
}

enum AgentBackend {
    Scripted(mpsc::Sender<OwnerTurn>),
    Lash(Arc<LashAgentRuntime>),
    Degraded(Arc<DegradedAgentRuntime>),
}

impl AgentRuntime {
    pub async fn start(
        config: RuntimeConfig,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
    ) -> anyhow::Result<Self> {
        match config.agent_mode {
            AgentMode::Scripted => Ok(Self {
                backend: Arc::new(AgentBackend::Scripted(start_scripted_runtime(
                    config,
                    tools,
                    broadcaster,
                ))),
            }),
            AgentMode::Lash => match LashAgentRuntime::start(config, tools, broadcaster).await? {
                LashStartup::Ready(runtime) => Ok(Self {
                    backend: Arc::new(AgentBackend::Lash(runtime)),
                }),
                LashStartup::Unavailable(runtime) => Ok(Self {
                    backend: Arc::new(AgentBackend::Degraded(runtime)),
                }),
            },
        }
    }

    pub async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        match self.backend.as_ref() {
            AgentBackend::Scripted(tx) => tx
                .send(turn)
                .await
                .map_err(|_| anyhow::anyhow!("Agent runtime queue is closed")),
            AgentBackend::Lash(runtime) => {
                let runtime = Arc::clone(runtime);
                tokio::spawn(async move {
                    runtime.enqueue_or_report(turn).await;
                });
                Ok(())
            }
            AgentBackend::Degraded(runtime) => runtime.enqueue(turn).await,
        }
    }
}

fn start_scripted_runtime(
    config: RuntimeConfig,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
) -> mpsc::Sender<OwnerTurn> {
    let (tx, rx) = mpsc::channel(64);
    let worker = ScriptedAgentWorker {
        config,
        tools,
        broadcaster,
        rx,
    };
    tokio::spawn(worker.run());
    tx
}

enum LashStartup {
    Ready(Arc<LashAgentRuntime>),
    Unavailable(Arc<DegradedAgentRuntime>),
}

struct LashAgentRuntime {
    session: lash::LashSession,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    notify: Arc<Notify>,
    pump_lock: Mutex<()>,
    anchors: Arc<Mutex<Option<TurnAnchors>>>,
}

#[derive(Debug, Clone)]
struct TurnAnchors {
    owner_message_id: u64,
    latest_agent_message_id: Option<u64>,
}

impl LashAgentRuntime {
    async fn start(
        config: RuntimeConfig,
        tools: ToolSuite,
        broadcaster: broadcast::Sender<HostToClient>,
    ) -> anyhow::Result<LashStartup> {
        let provider = match build_provider(&config).await {
            Ok(provider) => provider,
            Err(ProviderUnavailable { message }) => {
                tracing::warn!(%message, "Lash Agent provider unavailable; using degraded runtime");
                return Ok(LashStartup::Unavailable(Arc::new(DegradedAgentRuntime {
                    reason: message,
                    tools,
                    broadcaster,
                })));
            }
        };

        tokio::fs::create_dir_all(&config.data_dir).await?;
        let lash_dir = config.data_dir.join("lash");
        tokio::fs::create_dir_all(&lash_dir).await?;
        let store_factory = Arc::new(lash_sqlite_store::SqliteSessionStoreFactory::new(
            lash_dir.join("sessions"),
        ));
        let artifact_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("artifacts.db")).await?)
                as Arc<dyn lash::persistence::LashlangArtifactStore>;
        let process_env_store =
            Arc::new(lash_sqlite_store::Store::open(&lash_dir.join("process-env.db")).await?);
        let trigger_store = Arc::new(
            lash_sqlite_store::SqliteTriggerStore::open(&lash_dir.join("triggers.db")).await?,
        );
        let process_registry = Arc::new(
            lash_sqlite_store::SqliteProcessRegistry::open(&lash_dir.join("processes.db")).await?,
        ) as Arc<dyn lash::process::ProcessRegistry>;
        let model_spec =
            lash::ModelSpec::from_token_limits(config.model.clone(), None, 200_000, None)
                .map_err(|error| anyhow::anyhow!("invalid HIRSEL_MODEL metadata: {error}"))?;
        let rlm_config = lash_protocol_rlm::RlmProtocolPluginConfig::default()
            .with_lashlang_abilities(
                lash_protocol_rlm::LashlangAbilities::default().with_triggers(),
            );
        let rlm_factory =
            lash_protocol_rlm::RlmProtocolPluginFactory::new(rlm_config, artifact_store);
        let tool_provider = Arc::new(StaticToolProvider::new(
            hirsel_tool_definitions(),
            HirselToolExecutor {
                tools: tools.clone(),
                anchors: Arc::new(Mutex::new(None)),
            },
        ));
        let anchors = tool_provider.executor().anchors.clone();
        let core = lash::LashCore::rlm_builder(rlm_factory)
            .provider(provider)
            .model(model_spec)
            .store_factory(store_factory.clone())
            .attachment_store(Arc::new(lash::persistence::FileAttachmentStore::new(
                lash_dir.join("attachments"),
            )))
            .process_env_store(process_env_store)
            .effect_host(Arc::new(lash::durability::InlineEffectHost::default()))
            .process_registry(process_registry)
            .trigger_store(trigger_store)
            .tools(tool_provider)
            .plugin(Arc::new(HirselProcessPluginFactory {
                tools: tools.clone(),
            }))
            .disable_queued_work_driver()
            .build()?;
        let process_registry = core
            .process_registry()
            .ok_or_else(|| anyhow::anyhow!("Lash process registry was not configured"))?;
        let session = core
            .session(AGENT_SESSION_ID)
            .prompt_contribution(lash::prompt::PromptContribution::guidance(
                "Hirsel Agent",
                AGENT_PROMPT,
            ))
            .open()
            .await?;

        let runtime = Arc::new(Self {
            session,
            tools: tools.clone(),
            broadcaster: broadcaster.clone(),
            notify: Arc::new(Notify::new()),
            pump_lock: Mutex::new(()),
            anchors,
        });
        runtime.spawn_observation_bridge();
        runtime.spawn_turn_pump();
        runtime.spawn_process_terminal_bridge(process_registry, store_factory);
        runtime.notify_if_work_pending().await;
        tracing::info!(
            model = %config.model,
            provider = ?config.provider_mode,
            data_dir = %config.data_dir.display(),
            "Lash Agent runtime opened session agent"
        );
        Ok(LashStartup::Ready(runtime))
    }

    async fn enqueue_or_report(self: Arc<Self>, turn: OwnerTurn) {
        let message_id = turn.message_id;
        if let Err(error) = self.enqueue_inner(turn).await {
            tracing::warn!(%error, "failed to enqueue Owner turn into Lash session");
            match self
                .tools
                .chat_send(format!("Agent turn failed: {error}"), Some(message_id))
                .await
            {
                Ok(message) => {
                    let mut anchors = self.anchors.lock().await;
                    if let Some(anchors) = anchors.as_mut() {
                        anchors.latest_agent_message_id = Some(message.id);
                    }
                }
                Err(chat_error) => {
                    tracing::warn!(%chat_error, "failed to write Agent enqueue error to Chat");
                }
            }
        }
    }

    async fn enqueue_inner(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        {
            let mut anchors = self.anchors.lock().await;
            *anchors = Some(TurnAnchors {
                owner_message_id: turn.message_id,
                latest_agent_message_id: None,
            });
        }
        let body = match turn.anchor {
            Some(anchor) => format!("Owner replied to Inbox anchor {anchor}.\n\n{}", turn.body),
            None => turn.body,
        };
        self.session
            .enqueue(TurnInput::text(body))
            .id(turn.client_id)
            .send()
            .await?;
        self.notify.notify_one();
        Ok(())
    }

    async fn notify_if_work_pending(&self) {
        let pending_inputs = match self.session.pending_turn_inputs().await {
            Ok(inputs) => !inputs.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending Lash turn inputs at boot");
                false
            }
        };
        let queued_work = match self.session.queued_work().await {
            Ok(work) => !work.is_empty(),
            Err(error) => {
                tracing::warn!(%error, "failed to inspect pending Lash queued work at boot");
                false
            }
        };
        if pending_inputs || queued_work {
            self.notify.notify_one();
        }
    }

    fn spawn_turn_pump(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                runtime.notify.notified().await;
                let _guard = runtime.pump_lock.lock().await;
                loop {
                    let sends_before = runtime.tools.visible_sends();
                    match runtime.session.queued_turn().run().await {
                        Ok(Some(output)) => {
                            // Safety net: a turn that never called chat.send /
                            // inbox.file left Sam with nothing visible. Deliver
                            // its terminal prose to Chat rather than swallowing
                            // the reply (the prompt still teaches chat.send as
                            // the proper path).
                            if runtime.tools.visible_sends() == sends_before {
                                let text = output
                                    .assistant_message()
                                    .map(str::to_owned)
                                    .or_else(|| output.final_value().map(render_final_value));
                                if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                                    runtime.deliver_fallback_chat(text).await;
                                }
                            }
                            continue;
                        }
                        Ok(None) => break,
                        Err(error) => {
                            runtime.handle_turn_error(error).await;
                            break;
                        }
                    }
                }
            }
        });
    }

    async fn deliver_fallback_chat(&self, text: String) {
        match self.tools.chat_send(text, None).await {
            Ok(message) => {
                let mut anchors = self.anchors.lock().await;
                if let Some(anchors) = anchors.as_mut() {
                    anchors.latest_agent_message_id = Some(message.id);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to deliver fallback Agent chat message");
            }
        }
    }

    async fn handle_turn_error(&self, error: lash::EmbedError) {
        tracing::warn!(%error, "Lash queued turn failed");
        let anchor = self.current_anchor().await;
        match self
            .tools
            .chat_send(format!("Agent turn failed: {error}"), anchor)
            .await
        {
            Ok(message) => {
                let mut anchors = self.anchors.lock().await;
                if let Some(anchors) = anchors.as_mut() {
                    anchors.latest_agent_message_id = Some(message.id);
                }
            }
            Err(chat_error) => {
                tracing::warn!(%chat_error, "failed to write Agent turn error to Chat");
            }
        }
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
        });
    }

    async fn current_anchor(&self) -> Option<u64> {
        self.anchors.lock().await.as_ref().map(|anchors| {
            anchors
                .latest_agent_message_id
                .unwrap_or(anchors.owner_message_id)
        })
    }

    fn spawn_observation_bridge(self: &Arc<Self>) {
        let session = self.session.clone();
        let broadcaster = self.broadcaster.clone();
        tokio::spawn(async move {
            let observable = session.observe();
            let current = observable.current_remote_observation();
            let cursor = RemoteSessionCursor::new(current.cursor);
            let mut stream = match observable.subscribe_and_recover_remote(cursor) {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(%error, "failed to subscribe to Lash observation stream");
                    return;
                }
            };
            while let Some(item) = stream.next().await {
                match item {
                    Ok(RemoteSessionObservationStreamItem::Event(event)) => {
                        if let Some(activity) = activity_from_observation(&event.event) {
                            let _ = broadcaster.send(activity);
                        }
                    }
                    Ok(RemoteSessionObservationStreamItem::Gap { .. }) => {
                        let _ = broadcaster.send(HostToClient::AgentActivity {
                            state: AgentActivityState::Idle,
                            text: None,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Lash observation stream failed");
                        break;
                    }
                }
            }
        });
    }

    fn spawn_process_terminal_bridge(
        self: &Arc<Self>,
        process_registry: Arc<dyn lash::process::ProcessRegistry>,
        store_factory: Arc<dyn lash::persistence::SessionStoreFactory>,
    ) {
        let mut events = self.tools.terminal_events();
        let notify = Arc::clone(&self.notify);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let (event_type, payload) = terminal_event_payload(&event.outcome);
                        let request =
                            ProcessEventAppendRequest::new(event_type, payload).with_replay_key(
                                format!("hirsel-subagent:{}:{event_type}", event.process_id),
                            );
                        match process_registry
                            .append_event(&event.process_id, request)
                            .await
                        {
                            Ok(result) => {
                                if let Err(error) = enqueue_process_wake(
                                    store_factory.as_ref(),
                                    result.wake_delivery,
                                )
                                .await
                                {
                                    tracing::warn!(%error, "failed to enqueue Lash process wake");
                                } else {
                                    notify.notify_one();
                                }
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    process_id = %event.process_id,
                                    "failed to append Lash process terminal event"
                                );
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

async fn enqueue_process_wake(
    store_factory: &dyn lash::persistence::SessionStoreFactory,
    wake_delivery: Option<ProcessWakeDelivery>,
) -> anyhow::Result<()> {
    let Some(wake) = wake_delivery else {
        return Ok(());
    };
    let request = lash::persistence::SessionStoreCreateRequest {
        session_id: wake.target_session_id.clone(),
        relation: lash::persistence::SessionRelation::default(),
        policy: SessionPolicy::default(),
    };
    let Some(store) = store_factory
        .open_existing_store(&request)
        .await
        .map_err(anyhow::Error::msg)?
    else {
        return Ok(());
    };
    let draft = lash::persistence::QueuedWorkBatchDraft::new(
        wake.target_session_id.clone(),
        lash::persistence::DeliveryPolicy::EarliestSafeBoundary,
        lash::persistence::SlotPolicy::Exclusive,
        vec![lash::persistence::QueuedWorkPayload::process_wake(
            wake.clone(),
        )],
    )
    .with_source_key(format!(
        "process:{}:event:{}:wake",
        wake.process_id, wake.sequence
    ));
    store.enqueue_queued_work(draft).await?;
    Ok(())
}

fn render_final_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(other).unwrap_or_default()
        ),
    }
}

fn activity_from_observation(event: &RemoteSessionObservationEventPayload) -> Option<HostToClient> {
    match event {
        RemoteSessionObservationEventPayload::TurnActivity { activity } => match &activity.event {
            RemoteTurnEvent::ModelRequestStarted { .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some("thinking".to_string()),
            )),
            RemoteTurnEvent::AssistantProseDelta { text }
            | RemoteTurnEvent::ReasoningDelta { text } => Some(agent_activity(
                AgentActivityState::Thinking,
                latest_line(text),
            )),
            RemoteTurnEvent::ToolCallStarted { name, .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some(format!("tool {name}")),
            )),
            RemoteTurnEvent::ToolCallCompleted { name, .. } => Some(agent_activity(
                AgentActivityState::Thinking,
                Some(format!("tool {name} completed")),
            )),
            RemoteTurnEvent::Error { message } => Some(agent_activity(
                AgentActivityState::Thinking,
                latest_line(message),
            )),
            _ => None,
        },
        RemoteSessionObservationEventPayload::Committed => {
            Some(agent_activity(AgentActivityState::Idle, None))
        }
        _ => None,
    }
}

fn agent_activity(state: AgentActivityState, text: Option<String>) -> HostToClient {
    HostToClient::AgentActivity { state, text }
}

fn latest_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            const MAX_CHARS: usize = 180;
            if line.chars().count() <= MAX_CHARS {
                line.to_string()
            } else {
                let mut truncated = line.chars().take(MAX_CHARS - 3).collect::<String>();
                truncated.push_str("...");
                truncated
            }
        })
}

#[derive(Clone)]
struct DegradedAgentRuntime {
    reason: String,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
}

impl DegradedAgentRuntime {
    async fn enqueue(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("provider unavailable".to_string()),
        });
        self.tools
            .chat_send(
                format!("Agent turn failed: {}", self.reason),
                Some(turn.message_id),
            )
            .await?;
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
        });
        Ok(())
    }
}

struct ProviderUnavailable {
    message: String,
}

async fn build_provider(config: &RuntimeConfig) -> Result<ProviderHandle, ProviderUnavailable> {
    match config.provider_mode {
        ProviderMode::Anthropic => {
            let Some(api_key) = config.anthropic_api_key.clone() else {
                return Err(ProviderUnavailable {
                    message: "ANTHROPIC_API_KEY is not set for HIRSEL_PROVIDER=anthropic"
                        .to_string(),
                });
            };
            Ok(ProviderHandle::new(
                lash_provider_anthropic::AnthropicProvider::new(api_key)
                    .with_options(ProviderOptions {
                        expose_thinking: true,
                        ..ProviderOptions::default()
                    })
                    .into_components(),
            ))
        }
        ProviderMode::Codex => {
            let tokens = load_codex_tokens()
                .await
                .map_err(|message| ProviderUnavailable { message })?;
            Ok(ProviderHandle::new(
                lash_provider_openai::CodexProvider::new(
                    tokens.access_token,
                    tokens.refresh_token,
                    tokens.expires_at,
                )
                .with_account_id(tokens.account_id)
                .with_options(ProviderOptions {
                    expose_thinking: true,
                    ..ProviderOptions::default()
                })
                .into_components(),
            ))
        }
    }
}

#[derive(Debug)]
struct CodexTokens {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    tokens: CodexAuthTokens,
}

#[derive(Debug, Deserialize)]
struct CodexAuthTokens {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
}

async fn load_codex_tokens() -> Result<CodexTokens, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; cannot locate ~/.codex/auth.json".to_string())?;
    let auth_path = home.join(".codex").join("auth.json");
    let text = tokio::fs::read_to_string(&auth_path)
        .await
        .map_err(|error| format!("failed to read {}: {error}", auth_path.display()))?;
    let auth: CodexAuthFile =
        serde_json::from_str(&text).map_err(|error| format!("invalid Codex auth JSON: {error}"))?;
    if auth.tokens.access_token.is_empty() || auth.tokens.refresh_token.is_empty() {
        return Err("Codex OAuth tokens are missing access_token or refresh_token".to_string());
    }
    Ok(CodexTokens {
        access_token: auth.tokens.access_token,
        refresh_token: auth.tokens.refresh_token,
        expires_at: auth.tokens.expires_at.unwrap_or(0),
        account_id: auth.tokens.account_id,
    })
}

#[derive(Clone)]
struct HirselToolExecutor {
    tools: ToolSuite,
    anchors: Arc<Mutex<Option<TurnAnchors>>>,
}

#[async_trait]
impl StaticToolExecute for HirselToolExecutor {
    async fn execute(&self, call: ToolCall<'_>) -> ToolResult {
        match self.execute_inner(call).await {
            Ok(value) => ToolResult::ok(value),
            Err(error) => ToolResult::err_fmt(error),
        }
    }
}

impl HirselToolExecutor {
    async fn execute_inner(&self, call: ToolCall<'_>) -> Result<Value, String> {
        match call.name {
            "chat_send" => self.chat_send(call.args).await,
            "inbox_file" => self.inbox_file(call.args).await,
            "inbox_archive" => self.inbox_archive(call.args).await,
            "subagents_spawn" => self.subagents_spawn(call.args, call.context).await,
            "subagents_prompt" => self.subagents_prompt(call.args).await,
            "subagents_interrupt" => self.subagents_interrupt(call.args).await,
            "subagents_list" => self.subagents_list().await,
            "subagents_progress" => self.subagents_progress(call.args).await,
            "shell_run" => self.shell_run(call.args).await,
            other => Err(format!("Unknown tool: {other}")),
        }
    }

    async fn chat_send(&self, args: &Value) -> Result<Value, String> {
        let body = required_string_any(args, &["body_md", "body", "message"])?;
        // Chat messages quote-render their ref in the client, so only an
        // explicit ref from the Agent belongs here — the implicit turn anchor
        // is for Inbox Items, not ordinary replies.
        let explicit_ref = args.get("ref").and_then(Value::as_u64);
        let message = self
            .tools
            .chat_send(body, explicit_ref)
            .await
            .map_err(|error| error.to_string())?;
        {
            let mut anchors = self.anchors.lock().await;
            if let Some(anchors) = anchors.as_mut() {
                anchors.latest_agent_message_id = Some(message.id);
            }
        }
        serde_json::to_value(message).map_err(|error| error.to_string())
    }

    async fn inbox_file(&self, args: &Value) -> Result<Value, String> {
        let content = required_string_any(args, &["content_md", "content", "body"])?;
        let requires_response = args
            .get("requires_response")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let quick_replies = args
            .get("quick_replies")
            .cloned()
            .map(serde_json::from_value::<Vec<QuickReply>>)
            .transpose()
            .map_err(|error| format!("invalid quick_replies: {error}"))?
            .unwrap_or_default();
        let anchor = self
            .current_anchor()
            .await
            .ok_or_else(|| "inbox.file requires an active Owner turn anchor".to_string())?;
        let item = self
            .tools
            .inbox_file(content, anchor, requires_response, quick_replies)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(item).map_err(|error| error.to_string())
    }

    async fn inbox_archive(&self, args: &Value) -> Result<Value, String> {
        let item_id = required_u64_any(args, &["item_id", "id"])?;
        let item = self
            .tools
            .inbox_archive(item_id)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(json!({ "item": item })).map_err(|error| error.to_string())
    }

    async fn subagents_spawn(
        &self,
        args: &Value,
        context: &lash::tools::ToolContext<'_>,
    ) -> Result<Value, String> {
        let agent = parse_agent_kind(
            args.get("agent")
                .and_then(Value::as_str)
                .unwrap_or("claude"),
        )?;
        let prompt = required_string_any(args, &["prompt", "task"])?;
        let cwd = optional_path(args, "cwd")?
            .map(Ok)
            .unwrap_or_else(std::env::current_dir)
            .map_err(|error| format!("failed to resolve cwd: {error}"))?;
        let process_id = format!("proc-{}", uuid::Uuid::new_v4());
        let request = ProcessStartRequest::new(
            process_id.clone(),
            ProcessInput::Engine {
                kind: HIRSEL_SUBAGENT_ENGINE.to_string(),
                payload: json!({
                    "agent": agent,
                    "prompt": prompt,
                    "cwd": cwd,
                }),
            },
            RecoveryDisposition::OwnerBound,
            ProcessOriginator::session(SessionScope::new(context.session_id())),
        )
        .with_wake_target(Some(SessionScope::new(AGENT_SESSION_ID)))
        .with_event_types(subagent_event_types());
        let handle = context
            .processes()
            .start(request)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(json!({
            "process_id": handle.process_id,
            "handle": handle,
        }))
        .map_err(|error| error.to_string())
    }

    async fn subagents_prompt(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let text = required_string_any(args, &["text", "prompt", "message"])?;
        self.tools
            .subagents_prompt_process(&process_id, text)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true }))
    }

    async fn subagents_interrupt(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        self.tools
            .subagents_interrupt_process(&process_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true }))
    }

    async fn subagents_list(&self) -> Result<Value, String> {
        let processes = self
            .tools
            .subagents_list()
            .map_err(|error| error.to_string())?;
        serde_json::to_value(json!({ "processes": processes })).map_err(|error| error.to_string())
    }

    async fn subagents_progress(&self, args: &Value) -> Result<Value, String> {
        let process_id = required_string(args, "process_id")?;
        let events = self
            .tools
            .subagents_progress(&process_id)
            .map_err(|error| error.to_string())?;
        serde_json::to_value(json!({ "events": events })).map_err(|error| error.to_string())
    }

    async fn shell_run(&self, args: &Value) -> Result<Value, String> {
        let cmd = required_string(args, "cmd")?;
        let cwd = optional_path(args, "cwd")?;
        let timeout_secs = args.get("timeout_secs").and_then(Value::as_u64);
        let output = self
            .tools
            .shell_run(cmd, cwd, timeout_secs)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(output).map_err(|error| error.to_string())
    }

    async fn current_anchor(&self) -> Option<u64> {
        self.anchors.lock().await.as_ref().map(|anchors| {
            anchors
                .latest_agent_message_id
                .unwrap_or(anchors.owner_message_id)
        })
    }
}

#[derive(Clone)]
struct HirselProcessPluginFactory {
    tools: ToolSuite,
}

impl PluginFactory for HirselProcessPluginFactory {
    fn id(&self) -> &'static str {
        "hirsel_processes"
    }

    fn process_engine_contributions(
        &self,
        _ctx: &ProcessEngineContributionContext<'_>,
    ) -> Result<Vec<Arc<dyn ProcessEngine>>, PluginError> {
        Ok(vec![Arc::new(HirselSubagentEngine {
            tools: self.tools.clone(),
        })])
    }

    fn build(&self, _ctx: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        Ok(Arc::new(EmptyHirselSessionPlugin))
    }
}

struct EmptyHirselSessionPlugin;

impl SessionPlugin for EmptyHirselSessionPlugin {
    fn id(&self) -> &'static str {
        "hirsel_processes"
    }

    fn register(&self, _reg: &mut PluginRegistrar) -> Result<(), PluginError> {
        Ok(())
    }
}

#[derive(Clone)]
struct HirselSubagentEngine {
    tools: ToolSuite,
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
    ) -> ProcessAwaitOutput {
        let process_id = context.registration().id.to_string();
        let payload = match SubagentProcessPayload::from_value(&payload) {
            Ok(payload) => payload,
            Err(error) => return cancelled_await_output(error.to_string()),
        };
        if let Err(error) = self
            .tools
            .subagents_spawn_with_process_id(
                payload.agent,
                payload.prompt,
                payload.cwd,
                process_id.clone(),
            )
            .await
        {
            return cancelled_await_output(format!("failed to start Sub-agent Driver: {error}"));
        }
        match ProcessAwaiter::polling(context.registry())
            .await_terminal(&process_id)
            .await
        {
            Ok(output) => output,
            Err(error) => cancelled_await_output(format!("failed to await Sub-agent: {error}")),
        }
    }

    fn identity(&self, payload: &Value) -> ProcessIdentity {
        let label = SubagentProcessPayload::from_value(payload)
            .ok()
            .map(|payload| format!("{:?}: {}", payload.agent, short_label(&payload.prompt)));
        ProcessIdentity::new(HIRSEL_SUBAGENT_ENGINE).with_label(label)
    }

    fn durability_tier(&self) -> DurabilityTier {
        DurabilityTier::Durable
    }
}

struct SubagentProcessPayload {
    agent: AgentKind,
    prompt: String,
    cwd: PathBuf,
}

impl SubagentProcessPayload {
    fn from_value(value: &Value) -> Result<Self, PluginError> {
        let agent = parse_agent_kind(value.get("agent").and_then(Value::as_str).unwrap_or(""))
            .map_err(PluginError::Session)?;
        let prompt = required_string(value, "prompt").map_err(PluginError::Session)?;
        let cwd = optional_path(value, "cwd")
            .map_err(PluginError::Session)?
            .ok_or_else(|| PluginError::Session("cwd is required".to_string()))?;
        Ok(Self { agent, prompt, cwd })
    }
}

fn cancelled_await_output(message: String) -> ProcessAwaitOutput {
    ProcessAwaitOutput::Cancelled {
        message,
        raw: None,
        control: None,
    }
}

fn subagent_event_types() -> Vec<ProcessEventType> {
    vec![
        terminal_event_type(SUBAGENT_COMPLETED, ProcessTerminalState::Completed),
        terminal_event_type(SUBAGENT_FAILED, ProcessTerminalState::Failed),
        terminal_event_type(SUBAGENT_CANCELLED, ProcessTerminalState::Cancelled),
    ]
}

fn terminal_event_type(name: &str, state: ProcessTerminalState) -> ProcessEventType {
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
                state,
                await_output: Some(ProcessValueSelector::Pointer("/await_output".to_string())),
            }),
            wake: Some(ProcessWakeSpec {
                when: None,
                input: ProcessValueSelector::Pointer("/text".to_string()),
                dedupe_key: ProcessWakeDedupeKey::EventIdentity,
            }),
        },
    }
}

fn terminal_event_payload(outcome: &TerminalOutcome) -> (&'static str, Value) {
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

fn hirsel_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_definition(
            "hirsel.chat_send",
            "chat_send",
            "Append an Agent-authored Chat message for the Owner. Optional ref quotes an OLDER chat message by id; never ref the message you are directly answering.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["body_md"],
                "properties": {
                    "body_md": { "type": "string" },
                    "ref": { "type": "integer", "minimum": 1 }
                }
            }),
            json!({ "type": "object" }),
            ["chat"],
            "send",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.inbox_file",
            "inbox_file",
            "File an Inbox Item anchored to the current Agent turn.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["content_md"],
                "properties": {
                    "content_md": { "type": "string" },
                    "requires_response": { "type": "boolean", "default": true },
                    "quick_replies": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["value", "label"],
                            "properties": {
                                "value": { "type": "string" },
                                "label": { "type": "string" }
                            }
                        }
                    }
                }
            }),
            json!({ "type": "object" }),
            ["inbox"],
            "file",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.inbox_archive",
            "inbox_archive",
            "Archive an Inbox Item.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["item_id"],
                "properties": {
                    "item_id": { "type": "integer", "minimum": 1 }
                }
            }),
            json!({ "type": "object" }),
            ["inbox"],
            "archive",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_spawn",
            "subagents_spawn",
            "Start a Claude or Codex Sub-agent as a Lash Runtime Process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["agent", "prompt"],
                "properties": {
                    "agent": { "type": "string", "enum": ["claude", "codex"] },
                    "prompt": { "type": "string" },
                    "cwd": { "type": "string" }
                }
            }),
            json!({ "type": "object" }),
            ["subagents"],
            "spawn",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_prompt",
            "subagents_prompt",
            "Send follow-up input to a running Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id", "text"],
                "properties": {
                    "process_id": { "type": "string" },
                    "text": { "type": "string" }
                }
            }),
            json!({ "type": "object" }),
            ["subagents"],
            "prompt",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_interrupt",
            "subagents_interrupt",
            "Request interruption of a running Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            json!({ "type": "object" }),
            ["subagents"],
            "interrupt",
            ToolScheduling::Serial,
        ),
        tool_definition(
            "hirsel.subagents_list",
            "subagents_list",
            "List known Sub-agent processes.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            json!({ "type": "object" }),
            ["subagents"],
            "list",
            ToolScheduling::Parallel,
        ),
        tool_definition(
            "hirsel.subagents_progress",
            "subagents_progress",
            "Read recent progress events for a Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            json!({ "type": "object" }),
            ["subagents"],
            "progress",
            ToolScheduling::Parallel,
        ),
        tool_definition(
            "hirsel.shell_run",
            "shell_run",
            "Run a bounded shell command and return stdout, stderr, status, and timeout state.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["cmd"],
                "properties": {
                    "cmd": { "type": "string" },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 600 }
                }
            }),
            json!({ "type": "object" }),
            ["shell"],
            "run",
            ToolScheduling::Serial,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn tool_definition(
    id: &str,
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
    module_path: impl IntoIterator<Item = &'static str>,
    operation: &str,
    scheduling: ToolScheduling,
) -> ToolDefinition {
    ToolDefinition::raw(id, name, description, input_schema, output_schema)
        .with_lashlang_binding(LashlangToolBinding::new(module_path, operation))
        .with_scheduling(scheduling)
}

fn required_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{key}`"))
}

fn required_string_any(args: &Value, keys: &[&str]) -> Result<String, String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field `{}`", keys.join("` or `")))
}

fn required_u64_any(args: &Value, keys: &[&str]) -> Result<u64, String> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_u64))
        .ok_or_else(|| format!("missing required integer field `{}`", keys.join("` or `")))
}

fn optional_path(args: &Value, key: &str) -> Result<Option<PathBuf>, String> {
    args.get(key)
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| format!("field `{key}` must be a string path"))
        })
        .transpose()
}

fn parse_agent_kind(value: &str) -> Result<AgentKind, String> {
    match value {
        "claude" => Ok(AgentKind::Claude),
        "codex" => Ok(AgentKind::Codex),
        other => Err(format!("agent must be claude or codex, got `{other}`")),
    }
}

fn short_label(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 80;
    if compact.chars().count() <= MAX_CHARS {
        compact
    } else {
        let mut truncated = compact.chars().take(MAX_CHARS - 3).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

struct ScriptedAgentWorker {
    config: RuntimeConfig,
    tools: ToolSuite,
    broadcaster: broadcast::Sender<HostToClient>,
    rx: mpsc::Receiver<OwnerTurn>,
}

impl ScriptedAgentWorker {
    async fn run(mut self) {
        tracing::info!(
            model = %self.config.model,
            data_dir = %self.config.data_dir.display(),
            "Scripted Agent test double opened session agent"
        );
        while let Some(turn) = self.rx.recv().await {
            if let Err(error) = self.handle_turn(turn).await {
                tracing::error!(%error, "scripted Agent turn failed");
            }
        }
    }

    async fn handle_turn(&self, turn: OwnerTurn) -> anyhow::Result<()> {
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Thinking,
            text: Some("processing owner message".to_string()),
        });
        let result = self.handle_turn_inner(&turn).await;
        let _ = self.broadcaster.send(HostToClient::AgentActivity {
            state: AgentActivityState::Idle,
            text: None,
        });
        result
    }

    async fn handle_turn_inner(&self, turn: &OwnerTurn) -> anyhow::Result<()> {
        let lower = turn.body.to_lowercase();
        if self.config.driver_mode == DriverMode::Fake && lower.contains("delegate") {
            return self.handle_fake_delegation(turn).await;
        }
        if turn.anchor.is_some() {
            self.tools
                .chat_send(
                    "Acknowledged. I will continue from that Inbox reply.",
                    Some(turn.message_id),
                )
                .await?;
            return Ok(());
        }
        if lower.contains("pong") {
            self.tools.chat_send("pong", Some(turn.message_id)).await?;
            return Ok(());
        }
        self.tools
            .chat_send(
                "I received the Owner message. This scripted Agent mode is a deterministic test double; set HIRSEL_AGENT=lash for the real RLM runtime.",
                Some(turn.message_id),
            )
            .await?;
        Ok(())
    }

    async fn handle_fake_delegation(&self, turn: &OwnerTurn) -> anyhow::Result<()> {
        let anchor = self
            .tools
            .chat_send(
                "I delegated the repo fix to a Sub-agent and will file the result in the Inbox.",
                Some(turn.message_id),
            )
            .await?;
        let mut terminal_events = self.tools.terminal_events();
        let cwd = std::env::current_dir()?;
        let process = self
            .tools
            .subagents_spawn(
                AgentKind::Claude,
                "Make the trivial repo fix and report back.",
                cwd,
            )
            .await?;
        let tools = self.tools.clone();
        tokio::spawn(async move {
            loop {
                match terminal_events.recv().await {
                    Ok(event) if event.process_id == process.process_id => {
                        let content = terminal_content(&event.outcome);
                        if let Err(error) = tools
                            .inbox_file(
                                content,
                                anchor.id,
                                true,
                                vec![QuickReply {
                                    value: "ship it".to_string(),
                                    label: "Ship it".to_string(),
                                }],
                            )
                            .await
                        {
                            tracing::warn!(%error, "failed to file Sub-agent terminal Inbox Item");
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Ok(())
    }
}

fn terminal_content(outcome: &TerminalOutcome) -> String {
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
