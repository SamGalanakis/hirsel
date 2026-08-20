use super::*;

#[derive(Clone)]
pub struct RuntimeConfig {
    pub agent_mode: AgentMode,
    pub provider_mode: ProviderMode,
    pub anthropic_api_key: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub model: String,
    pub data_dir: PathBuf,
    pub driver_mode: DriverMode,
    pub config_store: ConfigStore,
    pub prompts: PromptConfig,
}

/// The host-generated tail of the Agent's guidance: what the Owner's prompt
/// body is followed by, whatever that body says. Not editable — a prompt edit
/// must never be able to hide where the runtime configuration lives.
pub(crate) fn agent_host_section(config: &Config) -> String {
    format!(
        "\n\n## Host configuration\n\nYour runtime configuration lives at `{}` (hand-editable TOML, hot-reloaded). It is documented at `{}` — read that before changing models or other settings. Your own system prompt lives there too, under `[agent] prompt`.\n",
        config.config_path.display(),
        config.docs_path.display()
    )
}

pub(super) fn agent_guidance_with_handoff(
    mut guidance: String,
    handoff_seed: Option<&str>,
) -> String {
    if let Some(handoff_seed) = handoff_seed {
        guidance.push_str("\n\n## Session handoff\n\n");
        guidance.push_str(handoff_seed);
    }
    guidance
}

#[derive(Clone)]
pub(super) struct DegradedAgentRuntime {
    pub(super) reason: String,
    pub(super) tools: ToolSuite,
    pub(super) broadcaster: broadcast::Sender<HostToClient>,
    pub(super) broadcast_log: BroadcastLog,
}

impl DegradedAgentRuntime {
    pub(super) async fn enqueue(&self, _turn: OwnerTurn) -> anyhow::Result<()> {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Thinking,
                text: Some("provider unavailable".to_string()),
                sc: None,
            },
        );
        self.tools
            .chat_send(format!("Agent turn failed: {}", self.reason), None)
            .await?;
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    pub(super) async fn cancel_turn(&self) -> anyhow::Result<()> {
        publish(
            &self.broadcast_log,
            &self.broadcaster,
            HostToClient::AgentActivity {
                state: AgentActivityState::Idle,
                text: None,
                sc: None,
            },
        );
        Ok(())
    }

    pub(super) async fn deliver_monitor_wake(&self, text: String) -> anyhow::Result<()> {
        self.tools.chat_send(text, None).await?;
        Ok(())
    }

    pub(super) async fn cancel_queued(
        &self,
        _client_id: &str,
    ) -> anyhow::Result<CancelQueuedResult> {
        Ok(CancelQueuedResult::AlreadyClaimed)
    }
}

pub(super) struct ProviderUnavailable {
    pub(super) message: String,
}

pub(super) async fn build_provider(
    config: &RuntimeConfig,
) -> Result<ProviderHandle, ProviderUnavailable> {
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
        ProviderMode::OpenRouter => {
            let Some(api_key) = config.openrouter_api_key.clone() else {
                return Err(ProviderUnavailable {
                    message: "OPENROUTER_API_KEY is not set for HIRSEL_PROVIDER=openrouter"
                        .to_string(),
                });
            };
            Ok(ProviderHandle::new(
                lash_provider_openai::OpenAiCompatibleProvider::new(
                    api_key,
                    lash_provider_openai::OPENROUTER_BASE_URL,
                )
                .with_compat(lash_provider_openai::OpenAiCompat::openrouter())
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
pub(super) struct CodexTokens {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) expires_at: u64,
    pub(super) account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CodexAuthFile {
    pub(super) tokens: CodexAuthTokens,
}

#[derive(Debug, Deserialize)]
pub(super) struct CodexAuthTokens {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    #[serde(default)]
    pub(super) account_id: Option<String>,
    #[serde(default)]
    pub(super) expires_at: Option<u64>,
}

pub(super) async fn load_codex_tokens() -> Result<CodexTokens, String> {
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
