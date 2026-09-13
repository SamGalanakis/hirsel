use super::*;

#[derive(Clone)]
pub struct RuntimeConfig {
    pub agent_mode: AgentMode,
    /// The `HIRSEL_PROVIDER` boot mode. Still what the legacy anthropic paths
    /// key off; what the provider handle is built from is `boot_plan`.
    pub provider_mode: ProviderMode,
    /// The host's single boot resolution: the stored roster choice reconciled
    /// with the environment default, decided once in `build_state`.
    pub boot_plan: BootPlan,
    pub anthropic_api_key: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub model: String,
    pub data_dir: PathBuf,
    pub driver_mode: DriverMode,
    pub config_store: ConfigStore,
    pub providers: ProviderRosterState,
    pub prompts: PromptConfig,
}

impl RuntimeConfig {
    /// The provider a Thread runs on when it has not named one of its own, and
    /// the plan that builds it. Derived from live configuration, so a Settings
    /// or hand edit repoints it without a restart.
    pub(crate) fn native_default_route(&self) -> (String, BootPlan) {
        crate::boot_provider::native_default_route(
            &self.config_store,
            &self.providers,
            &self.boot_plan,
        )
    }
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

/// The Agent's full session guidance: where it is, then what it can do, then
/// any handoff seed. The identity block leads because an agent that does not
/// know which Thread it is in cannot read the rest correctly.
pub(super) fn agent_guidance_with_handoff(
    identity: &crate::thread_identity::ThreadIdentity,
    guidance: String,
    handoff_seed: Option<&str>,
) -> String {
    let mut guidance = format!("{}\n{guidance}", identity.block());
    if let Some(handoff_seed) = handoff_seed {
        guidance.push_str("\n\n## Session handoff\n\n");
        guidance.push_str(handoff_seed);
    }
    guidance
}

pub(super) struct ProviderUnavailable {
    pub(super) message: String,
}

/// Build the transport for one resolved plan. The default Native route and a
/// Thread that names its own provider both resolve a plan through
/// `boot_provider` and arrive here with it, so every route constructs its
/// handle from the same credentials.
pub(super) async fn build_provider_for_plan(
    config: &RuntimeConfig,
    plan: &BootPlan,
) -> Result<ProviderHandle, ProviderUnavailable> {
    match plan {
        BootPlan::Env(ProviderMode::Anthropic) => {
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
        BootPlan::Env(ProviderMode::OpenRouter) => {
            let Some(api_key) = config.openrouter_api_key.clone() else {
                return Err(ProviderUnavailable {
                    message: "OPENROUTER_API_KEY is not set for HIRSEL_PROVIDER=openrouter"
                        .to_string(),
                });
            };
            Ok(openai_compatible_handle(
                api_key,
                lash_provider_openai::OPENROUTER_BASE_URL.to_string(),
            ))
        }
        // A stored instance boots on its own base URL and its own key.
        // `OPENROUTER_API_KEY` is a first-boot seed and is never consulted
        // here: whatever Settings shows as stored is what the host runs on.
        BootPlan::OpenAiCompatible {
            base_url, api_key, ..
        } => Ok(openai_compatible_handle(api_key.clone(), base_url.clone())),
        BootPlan::Env(ProviderMode::Codex) | BootPlan::Codex => {
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

/// One OpenAI-compatible handle for both the env OpenRouter mode and a stored
/// instance. OpenRouter's compat quirks are applied by base URL, never by
/// assumption: an instance pointing elsewhere must not claim them.
pub(super) fn openai_compatible_handle(api_key: String, base_url: String) -> ProviderHandle {
    let openrouter = base_url == lash_provider_openai::OPENROUTER_BASE_URL;
    let mut provider = lash_provider_openai::OpenAiCompatibleProvider::new(api_key, base_url);
    if openrouter {
        provider = provider.with_compat(lash_provider_openai::OpenAiCompat::openrouter());
    }
    ProviderHandle::new(
        provider
            .with_options(ProviderOptions {
                expose_thinking: true,
                ..ProviderOptions::default()
            })
            .into_components(),
    )
}

#[derive(Debug)]
pub(super) struct CodexTokens {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) expires_at: u64,
    pub(super) account_id: Option<String>,
}

pub(super) async fn load_codex_tokens() -> Result<CodexTokens, String> {
    let home = crate::provider_detect::home_dir()
        .ok_or_else(|| "HOME is not set; cannot locate ~/.codex/auth.json".to_string())?;
    let tokens = crate::provider_detect::read_codex_tokens(&home).await?;
    Ok(CodexTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at.unwrap_or(0),
        account_id: tokens.account_id,
    })
}
