use anyhow::anyhow;
use hirsel_proto::{AgentSlot, AvailableModel, ModelSelection, ModelSnapshot};
use lash::provider::{ModelCapability, ReasoningCapability, ReasoningEncoding, ReasoningSelection};

use crate::{config::ProviderMode, host_config::ConfigStore, providers::ProviderRosterState};

/// How a registry entry's variants map onto the wire.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VariantKind {
    /// Named reasoning efforts the provider validates and honours.
    Effort,
    /// The model has no host-selectable reasoning control; its single variant
    /// is a placeholder that resolves to the provider's own default.
    ProviderDefault,
}

/// The single variant offered by a [`VariantKind::ProviderDefault`] model. The
/// protocol and the Settings picker always want a non-empty variant list, so
/// provider-default models expose exactly this one.
const PROVIDER_DEFAULT_VARIANT: &str = "default";

struct RegistryEntry {
    id: &'static str,
    label: &'static str,
    variants: &'static [&'static str],
    default_variant: &'static str,
    context_window_tokens: usize,
    variant_kind: VariantKind,
}

// Current ChatGPT-account model metadata confirms this ID and its effort tokens.
// The main agent is deliberately pinned to GPT-5.6 Sol; keep this curated until
// the host has a provider-backed catalog.
const CODEX_REGISTRY: &[RegistryEntry] = &[RegistryEntry {
    id: "gpt-5.6-sol",
    label: "GPT-5.6 Sol",
    variants: &["low", "medium", "high", "xhigh", "max"],
    default_variant: "medium",
    context_window_tokens: 200_000,
    variant_kind: VariantKind::Effort,
}];

// Forks have their own curated surface: Luna is the economy default, while Sol
// remains available as a deliberate escalation without becoming selectable by
// the resident Agent.
const CODEX_FORK_REGISTRY: &[RegistryEntry] = &[
    RegistryEntry {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        variants: &["low", "medium", "high", "xhigh", "max"],
        default_variant: "max",
        context_window_tokens: 200_000,
        variant_kind: VariantKind::Effort,
    },
    RegistryEntry {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        variants: &["low", "medium", "high", "xhigh", "max"],
        default_variant: "medium",
        context_window_tokens: 200_000,
        variant_kind: VariantKind::Effort,
    },
];

// OpenRouter routes to Gemini 3.7 Flash, which reasons on its own schedule:
// there is no host-selectable effort ladder to offer, so the entry carries the
// single provider-default variant.
const OPENROUTER_REGISTRY: &[RegistryEntry] = &[RegistryEntry {
    id: "google/gemini-3.7-flash",
    label: "Gemini 3.7 Flash",
    variants: &[PROVIDER_DEFAULT_VARIANT],
    default_variant: PROVIDER_DEFAULT_VARIANT,
    context_window_tokens: 1_000_000,
    variant_kind: VariantKind::ProviderDefault,
}];

const OPENROUTER_FORK_REGISTRY: &[RegistryEntry] = &[RegistryEntry {
    id: "google/gemini-3.7-flash",
    label: "Gemini 3.7 Flash",
    variants: &[PROVIDER_DEFAULT_VARIANT],
    default_variant: PROVIDER_DEFAULT_VARIANT,
    context_window_tokens: 1_000_000,
    variant_kind: VariantKind::ProviderDefault,
}];

/// The model a wake-triage fork runs as until the Owner picks another: the
/// cheapest entry the booted provider offers, because a fork exists to read a
/// wake without spending the main session's budget. Anthropic mode has no
/// runtime-selectable registry, so it has no fork model either.
fn default_fork_model_id(provider: ProviderMode) -> Option<&'static str> {
    match provider {
        ProviderMode::Codex => Some("gpt-5.6-luna"),
        ProviderMode::OpenRouter => Some("google/gemini-3.7-flash"),
        ProviderMode::Anthropic => None,
    }
}

/// The fork's fallback selection: its default model at that model's default
/// variant.
pub fn default_fork_selection(provider: ProviderMode) -> Option<ModelSelection> {
    let entry = fork_registry_entry(provider, default_fork_model_id(provider)?)?;
    Some(ModelSelection {
        id: entry.id.to_string(),
        variant: entry.default_variant.to_string(),
    })
}

/// The context window a free-text model is assumed to have when the host has
/// no metadata for it. Deliberately conservative: an over-claimed window is a
/// truncated turn at the endpoint, an under-claimed one only condenses sooner.
const UNKNOWN_CONTEXT_WINDOW_TOKENS: usize = 200_000;

/// How the provider a resident agent is pointed at shapes its model choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionMode {
    /// A curated registry: the host knows every id and every variant.
    Curated {
        provider: ProviderMode,
        provider_id: Option<String>,
    },
    /// An OpenAI-compatible endpoint: the model id is free text and the single
    /// variant defers to the provider.
    FreeText {
        provider_id: String,
        default_model: String,
    },
}

impl SelectionMode {
    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Curated { provider_id, .. } => provider_id.as_deref(),
            Self::FreeText { provider_id, .. } => Some(provider_id),
        }
    }

    pub fn is_free_text(&self) -> bool {
        matches!(self, Self::FreeText { .. })
    }
}

/// The mode a resident agent's stored provider puts it in. An absent, removed,
/// or Sub-agents-only provider leaves the agent on the booted provider's
/// curated registry — a stale value is a warning, never a boot error.
pub fn selection_mode(
    roster: &ProviderRosterState,
    booted: ProviderMode,
    agent: AgentSlot,
) -> SelectionMode {
    match roster.agent_provider(agent) {
        Some(choice) if choice.is_free_text() => SelectionMode::FreeText {
            provider_id: choice.id,
            default_model: choice.default_model,
        },
        Some(choice) => SelectionMode::Curated {
            provider: ProviderMode::Codex,
            provider_id: Some(choice.id),
        },
        None => SelectionMode::Curated {
            provider: booted,
            provider_id: roster.booted_provider_id().map(str::to_string),
        },
    }
}

/// Validate a free-text model id: the host has no registry to check it
/// against, so it checks the only two things it can — that the Owner typed
/// something, and that they did not type it with stray whitespace that an
/// endpoint would reject as an unknown model.
pub fn validate_free_text(model_id: &str) -> anyhow::Result<ModelSelection> {
    if model_id.is_empty() || model_id.trim().is_empty() {
        return Err(anyhow!("model id must not be empty"));
    }
    if model_id.trim() != model_id {
        return Err(anyhow!(
            "model id `{model_id}` has leading or trailing whitespace"
        ));
    }
    Ok(ModelSelection {
        id: model_id.to_string(),
        variant: PROVIDER_DEFAULT_VARIANT.to_string(),
    })
}

/// Validate a model id + variant under whichever mode the agent is in.
pub fn validate_in_mode(
    mode: &SelectionMode,
    fork: bool,
    model_id: &str,
    variant: &str,
) -> anyhow::Result<ModelSelection> {
    match mode {
        SelectionMode::FreeText { .. } => validate_free_text(model_id),
        SelectionMode::Curated { provider, .. } if fork => {
            validate_fork_selection(*provider, model_id, variant)
        }
        SelectionMode::Curated { provider, .. } => validate_selection(*provider, model_id, variant),
    }
}

/// What a picker may offer under this mode: the curated registry, or nothing
/// at all because the Owner types the id.
pub fn available_in_mode(mode: &SelectionMode, fork: bool) -> Vec<AvailableModel> {
    match mode {
        SelectionMode::FreeText { .. } => Vec::new(),
        SelectionMode::Curated { provider, .. } if fork => available_fork_models(*provider),
        SelectionMode::Curated { provider, .. } => available_models(*provider),
    }
}

/// The selection an agent lands on when nothing usable is stored: the
/// provider's own default model at its default variant.
pub fn default_in_mode(mode: &SelectionMode, fork: bool) -> Option<ModelSelection> {
    match mode {
        SelectionMode::FreeText { default_model, .. } => validate_free_text(default_model).ok(),
        SelectionMode::Curated { provider, .. } if fork => default_fork_selection(*provider),
        SelectionMode::Curated { provider, .. } => {
            let entry = registry(*provider).first()?;
            Some(ModelSelection {
                id: entry.id.to_string(),
                variant: entry.default_variant.to_string(),
            })
        }
    }
}

/// Validate a main-Agent model id + variant against the booted provider's
/// main-Agent registry.
pub fn validate(
    provider: ProviderMode,
    model_id: &str,
    variant: &str,
) -> anyhow::Result<ModelSelection> {
    validate_selection(provider, model_id, variant)
}

/// Validate a fork model id + variant against the booted provider's fork-only
/// registry.
pub fn validate_fork(
    provider: ProviderMode,
    model_id: &str,
    variant: &str,
) -> anyhow::Result<ModelSelection> {
    validate_fork_selection(provider, model_id, variant)
}

/// The curated main-agent models for a provider. Anthropic mode pins its model
/// through `HIRSEL_MODEL` and never builds a selection state, so it offers no
/// runtime-selectable entries.
fn registry(provider: ProviderMode) -> &'static [RegistryEntry] {
    match provider {
        ProviderMode::Codex => CODEX_REGISTRY,
        ProviderMode::OpenRouter => OPENROUTER_REGISTRY,
        ProviderMode::Anthropic => &[],
    }
}

fn fork_registry(provider: ProviderMode) -> &'static [RegistryEntry] {
    match provider {
        ProviderMode::Codex => CODEX_FORK_REGISTRY,
        ProviderMode::OpenRouter => OPENROUTER_FORK_REGISTRY,
        ProviderMode::Anthropic => &[],
    }
}

#[derive(Clone)]
pub struct ModelSelectionState {
    provider: ProviderMode,
    fallback: ModelSelection,
    config_store: ConfigStore,
    roster: ProviderRosterState,
}

impl ModelSelectionState {
    pub async fn load(
        provider: ProviderMode,
        config_store: ConfigStore,
        roster: ProviderRosterState,
        configured_model: &str,
    ) -> anyhow::Result<Self> {
        let fallback = selection_for_configured_model(provider, configured_model)?;
        Ok(Self {
            provider,
            fallback,
            config_store,
            roster,
        })
    }

    /// The mode the main Agent's selected provider puts its model choice in.
    pub fn mode(&self) -> SelectionMode {
        selection_mode(&self.roster, self.provider, AgentSlot::Main)
    }

    pub fn current(&self) -> ModelSelection {
        self.selection_in(&self.mode())
    }

    pub fn snapshot(&self) -> ModelSnapshot {
        let mode = self.mode();
        ModelSnapshot {
            current: self.selection_in(&mode),
            available: available_in_mode(&mode, false),
            provider_id: mode.provider_id().map(str::to_string),
            free_text_model: mode.is_free_text(),
        }
    }

    pub fn validate(&self, model_id: &str, variant: &str) -> anyhow::Result<ModelSelection> {
        validate_in_mode(&self.mode(), false, model_id, variant)
    }

    pub async fn persist_and_select(&self, selection: ModelSelection) -> anyhow::Result<()> {
        self.config_store
            .set_model_selection(&selection.id, &selection.variant)
            .await?;
        Ok(())
    }

    /// What the live Lash session runs. The `ProviderHandle` is built once at
    /// boot and baked into the session, so a main-agent provider pointed
    /// somewhere else is stored and reported but never applied: the running
    /// session keeps the booted provider's own selection until the host
    /// restarts.
    pub fn model_spec(&self) -> anyhow::Result<lash::ModelSpec> {
        let mode = self.mode();
        if mode.provider_id() == self.roster.booted_provider_id() {
            return model_spec_in(&mode, &self.selection_in(&mode));
        }
        let booted = SelectionMode::Curated {
            provider: self.provider,
            provider_id: self.roster.booted_provider_id().map(str::to_string),
        };
        model_spec_in(&booted, &self.fallback)
    }

    fn selection_in(&self, mode: &SelectionMode) -> ModelSelection {
        let fallback = || default_in_mode(mode, false).unwrap_or_else(|| self.fallback.clone());
        let Some((model_id, variant)) = self.config_store.model_selection() else {
            tracing::warn!(
                path = %self.config_store.path().display(),
                "host config [model] section is missing or malformed; falling back to the provider's default model"
            );
            return fallback();
        };
        match validate_in_mode(mode, false, &model_id, &variant) {
            Ok(selection) => selection,
            Err(error) => {
                tracing::warn!(
                    path = %self.config_store.path().display(),
                    %error,
                    "persisted model selection is not available on the selected provider; falling back to its default model"
                );
                fallback()
            }
        }
    }
}

pub fn available_models(provider: ProviderMode) -> Vec<AvailableModel> {
    available_from_registry(registry(provider))
}

pub fn available_fork_models(provider: ProviderMode) -> Vec<AvailableModel> {
    available_from_registry(fork_registry(provider))
}

fn available_from_registry(entries: &[RegistryEntry]) -> Vec<AvailableModel> {
    entries
        .iter()
        .map(|entry| AvailableModel {
            id: entry.id.to_string(),
            label: entry.label.to_string(),
            variants: entry
                .variants
                .iter()
                .map(|variant| (*variant).to_string())
                .collect(),
            default_variant: entry.default_variant.to_string(),
        })
        .collect()
}

fn model_spec_in(
    mode: &SelectionMode,
    selection: &ModelSelection,
) -> anyhow::Result<lash::ModelSpec> {
    match mode {
        SelectionMode::Curated { provider, .. } => model_spec(*provider, selection),
        SelectionMode::FreeText { .. } => free_text_model_spec(selection),
    }
}

/// A spec for a model the host has no registry entry for: reasoning is the
/// endpoint's business, and the context window is whatever the host happens to
/// know about that id, or a conservative default.
fn free_text_model_spec(selection: &ModelSelection) -> anyhow::Result<lash::ModelSpec> {
    let context_window_tokens = CODEX_REGISTRY
        .iter()
        .chain(CODEX_FORK_REGISTRY)
        .chain(OPENROUTER_REGISTRY)
        .find(|entry| entry.id == selection.id)
        .map_or(UNKNOWN_CONTEXT_WINDOW_TOKENS, |entry| {
            entry.context_window_tokens
        });
    lash::ModelSpec::builder(selection.id.clone())
        .variant(ReasoningSelection::ProviderDefault)
        .context_window_tokens(context_window_tokens)
        .build()
        .map_err(anyhow::Error::msg)
}

fn model_spec(
    provider: ProviderMode,
    selection: &ModelSelection,
) -> anyhow::Result<lash::ModelSpec> {
    let entry = registry_entry(provider, &selection.id)
        .ok_or_else(|| anyhow!("unknown model: {}", selection.id))?;
    // A provider-default model advertises no reasoning capability: the host has
    // nothing to encode on the wire, and claiming an effort ladder the endpoint
    // will not honour would make the picker lie.
    let capability = match entry.variant_kind {
        VariantKind::Effort => ModelCapability {
            reasoning: Some(ReasoningCapability {
                efforts: entry
                    .variants
                    .iter()
                    .map(|variant| (*variant).to_string())
                    .collect(),
                default_effort: Some(entry.default_variant.to_string()),
                aliases: Default::default(),
                encoding: ReasoningEncoding::Effort,
                disable: None,
                mandatory: true,
            }),
            ..Default::default()
        },
        VariantKind::ProviderDefault => ModelCapability::default(),
    };
    let variant = match entry.variant_kind {
        VariantKind::Effort => ReasoningSelection::Effort(selection.variant.clone()),
        VariantKind::ProviderDefault => ReasoningSelection::ProviderDefault,
    };
    lash::ModelSpec::builder(selection.id.clone())
        .variant(variant)
        .context_window_tokens(entry.context_window_tokens)
        .build()
        .map(|spec| spec.with_capability(capability))
        .map_err(anyhow::Error::msg)
}

fn selection_for_configured_model(
    provider: ProviderMode,
    configured_model: &str,
) -> anyhow::Result<ModelSelection> {
    let model_id = configured_model.trim();
    let entry = registry_entry(provider, model_id).ok_or_else(|| {
        anyhow!("HIRSEL_MODEL `{configured_model}` is not available for runtime selection")
    })?;
    Ok(ModelSelection {
        id: entry.id.to_string(),
        variant: entry.default_variant.to_string(),
    })
}

fn validate_selection(
    provider: ProviderMode,
    model_id: &str,
    variant: &str,
) -> anyhow::Result<ModelSelection> {
    let entry =
        registry_entry(provider, model_id).ok_or_else(|| anyhow!("unknown model: {model_id}"))?;
    if !entry.variants.contains(&variant) {
        return Err(anyhow!(
            "unknown variant `{variant}` for model `{model_id}`; available variants: {}",
            entry.variants.join(", ")
        ));
    }
    Ok(ModelSelection {
        id: entry.id.to_string(),
        variant: variant.to_string(),
    })
}

fn validate_fork_selection(
    provider: ProviderMode,
    model_id: &str,
    variant: &str,
) -> anyhow::Result<ModelSelection> {
    let entry = fork_registry_entry(provider, model_id)
        .ok_or_else(|| anyhow!("unknown fork model: {model_id}"))?;
    if !entry.variants.contains(&variant) {
        return Err(anyhow!(
            "unknown variant `{variant}` for fork model `{model_id}`; available variants: {}",
            entry.variants.join(", ")
        ));
    }
    Ok(ModelSelection {
        id: entry.id.to_string(),
        variant: variant.to_string(),
    })
}

fn registry_entry(provider: ProviderMode, model_id: &str) -> Option<&'static RegistryEntry> {
    registry(provider).iter().find(|entry| entry.id == model_id)
}

fn fork_registry_entry(provider: ProviderMode, model_id: &str) -> Option<&'static RegistryEntry> {
    fork_registry(provider)
        .iter()
        .find(|entry| entry.id == model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(
        dir: &tempfile::TempDir,
        store: &ConfigStore,
        provider: ProviderMode,
    ) -> ProviderRosterState {
        ProviderRosterState::new(store.clone(), provider, Some(dir.path().to_path_buf()))
    }

    async fn state(
        dir: &tempfile::TempDir,
        provider: ProviderMode,
        configured_model: &str,
    ) -> ModelSelectionState {
        let store = store(dir).await;
        let roster = roster(dir, &store, provider);
        ModelSelectionState::load(provider, store, roster, configured_model)
            .await
            .unwrap()
    }

    async fn store(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            std::path::Path::new("/docs/hirsel-config.md"),
            &crate::host_config::EnvBootstrap::default(),
        )
        .await
        .unwrap()
    }

    #[test]
    fn registry_validates_models_and_variants() {
        let selected = validate_selection(ProviderMode::Codex, "gpt-5.6-sol", "high").unwrap();
        assert_eq!(selected.id, "gpt-5.6-sol");
        assert_eq!(selected.variant, "high");
        assert!(validate_selection(ProviderMode::Codex, "gpt-5.6-luna", "max").is_err());
        assert!(validate_selection(ProviderMode::Codex, "gpt-5", "high").is_err());
        assert!(validate_selection(ProviderMode::Codex, "gpt-5.6-sol", "impossible").is_err());
    }

    #[test]
    fn codex_fork_registry_defaults_to_luna_max_and_can_escalate_to_sol() {
        let models = available_fork_models(ProviderMode::Codex);
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-5.6-luna", "gpt-5.6-sol"]
        );
        assert_eq!(
            models[0].variants,
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(models[0].default_variant, "max");
        assert_eq!(
            default_fork_selection(ProviderMode::Codex),
            Some(ModelSelection {
                id: "gpt-5.6-luna".to_string(),
                variant: "max".to_string(),
            })
        );
        assert!(validate_fork_selection(ProviderMode::Codex, "gpt-5.6-sol", "high").is_ok());
    }

    #[test]
    fn registries_are_scoped_to_their_provider() {
        assert_eq!(
            available_models(ProviderMode::OpenRouter)
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            vec!["google/gemini-3.7-flash".to_string()]
        );
        // A model from the other provider's registry is unknown here, and vice
        // versa; Anthropic mode offers nothing selectable at all.
        assert!(validate_selection(ProviderMode::OpenRouter, "gpt-5.6-sol", "high").is_err());
        assert!(
            validate_selection(ProviderMode::Codex, "google/gemini-3.7-flash", "default").is_err()
        );
        assert!(available_models(ProviderMode::Anthropic).is_empty());
    }

    #[test]
    fn openrouter_offers_a_single_provider_default_variant() {
        let models = available_models(ProviderMode::OpenRouter);
        let entry = models.first().expect("OpenRouter registry entry");
        assert_eq!(entry.label, "Gemini 3.7 Flash");
        assert_eq!(entry.variants, vec!["default".to_string()]);
        assert_eq!(entry.default_variant, "default");
        assert!(validate_selection(ProviderMode::OpenRouter, &entry.id, "high").is_err());
    }

    #[tokio::test]
    async fn persistence_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let selection_state = state(&dir, ProviderMode::Codex, "gpt-5.6-sol").await;
        selection_state
            .persist_and_select(ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "max".to_string(),
            })
            .await
            .unwrap();

        let reloaded = state(&dir, ProviderMode::Codex, "gpt-5.6-sol").await;
        assert_eq!(
            reloaded.current(),
            ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "max".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn stale_or_invalid_config_falls_back_without_bricking_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        store
            .set_model_selection("retired-model", "impossible")
            .await
            .unwrap();
        let roster = roster(&dir, &store, ProviderMode::Codex);
        let state = ModelSelectionState::load(ProviderMode::Codex, store, roster, "gpt-5.6-sol")
            .await
            .unwrap();
        assert_eq!(
            state.current(),
            ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "medium".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn a_selection_from_another_provider_falls_back_to_the_configured_model() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        // What `data/hirsel.toml` holds after running in Codex mode; booting in
        // OpenRouter mode must degrade to the configured default, not fail.
        store
            .set_model_selection("gpt-5.6-sol", "high")
            .await
            .unwrap();
        let roster = roster(&dir, &store, ProviderMode::OpenRouter);
        let state = ModelSelectionState::load(
            ProviderMode::OpenRouter,
            store,
            roster,
            "google/gemini-3.7-flash",
        )
        .await
        .unwrap();
        assert_eq!(
            state.current(),
            ModelSelection {
                id: "google/gemini-3.7-flash".to_string(),
                variant: "default".to_string(),
            }
        );
    }

    async fn router_state(dir: &tempfile::TempDir, booted: ProviderMode) -> ModelSelectionState {
        let store = store(dir).await;
        let roster = roster(dir, &store, booted);
        roster
            .add(
                "router",
                "Router",
                "https://example.invalid/v1",
                "sk-fake-key",
                "some/model",
            )
            .await
            .unwrap();
        let choice = roster.selection_for("router").unwrap();
        roster
            .point_agent_at(
                AgentSlot::Main,
                &choice,
                &ModelSelection {
                    id: "some/model".to_string(),
                    variant: "default".to_string(),
                },
            )
            .await
            .unwrap();
        ModelSelectionState::load(booted, store, roster, "gpt-5.6-sol")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_openai_compatible_provider_takes_any_non_empty_model_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = router_state(&dir, ProviderMode::Codex).await;

        let snapshot = state.snapshot();
        assert!(snapshot.free_text_model);
        assert!(snapshot.available.is_empty());
        assert_eq!(snapshot.provider_id.as_deref(), Some("router"));
        assert_eq!(snapshot.current.id, "some/model");
        assert_eq!(snapshot.current.variant, "default");

        // Any id the endpoint might offer is accepted, and the variant is the
        // provider's own: the host has no effort ladder to promise.
        let accepted = state.validate("vendor/brand-new-model", "high").unwrap();
        assert_eq!(accepted.id, "vendor/brand-new-model");
        assert_eq!(accepted.variant, "default");
        // Shape is the only thing left to check.
        for rejected in ["", "  ", " model", "model "] {
            assert!(
                state.validate(rejected, "default").is_err(),
                "accepted {rejected:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_codex_registry_stays_curated_when_it_is_the_selected_provider() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let roster = roster(&dir, &store, ProviderMode::OpenRouter);
        let choice = roster.selection_for("codex").unwrap();
        roster
            .point_agent_at(
                AgentSlot::Main,
                &choice,
                &ModelSelection {
                    id: "gpt-5.6-sol".to_string(),
                    variant: "medium".to_string(),
                },
            )
            .await
            .unwrap();
        let state = ModelSelectionState::load(
            ProviderMode::OpenRouter,
            store,
            roster,
            "google/gemini-3.7-flash",
        )
        .await
        .unwrap();

        let snapshot = state.snapshot();
        assert!(!snapshot.free_text_model);
        assert_eq!(snapshot.provider_id.as_deref(), Some("codex"));
        assert_eq!(
            snapshot
                .available
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.6-sol"]
        );
        assert!(state.validate("gpt-5.6-luna", "max").is_err());
        assert!(state.validate("gpt-5.6-sol", "impossible").is_err());
        assert!(state.validate("gpt-5.6-sol", "xhigh").is_ok());
    }

    #[tokio::test]
    async fn a_main_provider_the_host_did_not_boot_on_leaves_the_live_spec_alone() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = router_state(&dir, ProviderMode::Codex).await;
        // Stored and reported...
        assert_eq!(elsewhere.snapshot().provider_id.as_deref(), Some("router"));
        assert_eq!(elsewhere.current().id, "some/model");
        // ...but the session was built on the booted provider's handle, so the
        // spec it runs stays the booted provider's own.
        let spec = elsewhere.model_spec().unwrap();
        assert_eq!(spec.id, "gpt-5.6-sol");
        assert_eq!(spec.variant.effort(), Some("medium"));

        // An agent still on the booted provider runs exactly what it selected.
        let booted_dir = tempfile::tempdir().unwrap();
        let booted = state(&booted_dir, ProviderMode::Codex, "gpt-5.6-sol").await;
        booted
            .persist_and_select(ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "xhigh".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(booted.model_spec().unwrap().variant.effort(), Some("xhigh"));
    }

    #[test]
    fn model_spec_carries_the_selected_effort_and_capability() {
        let spec = model_spec(
            ProviderMode::Codex,
            &ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "high".to_string(),
            },
        )
        .unwrap();
        assert_eq!(spec.id, "gpt-5.6-sol");
        assert_eq!(spec.variant.effort(), Some("high"));
        assert_eq!(spec.limits.context_window_tokens.get(), 200_000);
        assert!(
            spec.capability
                .reasoning
                .expect("reasoning capability")
                .efforts
                .contains(&"high".to_string())
        );
    }

    #[test]
    fn openrouter_model_spec_defers_reasoning_to_the_provider() {
        let spec = model_spec(
            ProviderMode::OpenRouter,
            &ModelSelection {
                id: "google/gemini-3.7-flash".to_string(),
                variant: "default".to_string(),
            },
        )
        .unwrap();
        assert_eq!(spec.id, "google/gemini-3.7-flash");
        assert_eq!(spec.variant, ReasoningSelection::ProviderDefault);
        assert_eq!(spec.limits.context_window_tokens.get(), 1_000_000);
        assert!(spec.capability.reasoning.is_none());
    }
}
