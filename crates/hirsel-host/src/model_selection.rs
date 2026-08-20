use anyhow::anyhow;
use hirsel_proto::{AvailableModel, ModelSelection, ModelSnapshot};
use lash::provider::{ModelCapability, ReasoningCapability, ReasoningEncoding, ReasoningSelection};

use crate::{config::ProviderMode, host_config::ConfigStore};

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
const CODEX_REGISTRY: &[RegistryEntry] = &[
    RegistryEntry {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        variants: &["low", "medium", "high", "xhigh", "max"],
        default_variant: "medium",
        context_window_tokens: 200_000,
        variant_kind: VariantKind::Effort,
    },
    // The cheap lane. It is here so a fork — a triage read that must not cost
    // what the main session costs — has a real option under this provider.
    RegistryEntry {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        variants: &["low", "medium", "high"],
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
    let entry = registry_entry(provider, default_fork_model_id(provider)?)?;
    Some(ModelSelection {
        id: entry.id.to_string(),
        variant: entry.default_variant.to_string(),
    })
}

/// Validate a model id + variant against the booted provider's registry.
pub fn validate(
    provider: ProviderMode,
    model_id: &str,
    variant: &str,
) -> anyhow::Result<ModelSelection> {
    validate_selection(provider, model_id, variant)
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

#[derive(Clone)]
pub struct ModelSelectionState {
    provider: ProviderMode,
    fallback: ModelSelection,
    config_store: ConfigStore,
}

impl ModelSelectionState {
    pub async fn load(
        provider: ProviderMode,
        config_store: ConfigStore,
        configured_model: &str,
    ) -> anyhow::Result<Self> {
        let fallback = selection_for_configured_model(provider, configured_model)?;
        Ok(Self {
            provider,
            fallback,
            config_store,
        })
    }

    pub fn current(&self) -> ModelSelection {
        selection_from_store(self.provider, &self.config_store, &self.fallback)
    }

    pub fn snapshot(&self) -> ModelSnapshot {
        ModelSnapshot {
            current: self.current(),
            available: available_models(self.provider),
        }
    }

    pub fn validate(&self, model_id: &str, variant: &str) -> anyhow::Result<ModelSelection> {
        validate_selection(self.provider, model_id, variant)
    }

    pub async fn persist_and_select(&self, selection: ModelSelection) -> anyhow::Result<()> {
        self.config_store
            .set_model_selection(&selection.id, &selection.variant)
            .await?;
        Ok(())
    }

    pub fn model_spec(&self) -> anyhow::Result<lash::ModelSpec> {
        model_spec(self.provider, &self.current())
    }
}

fn selection_from_store(
    provider: ProviderMode,
    config_store: &ConfigStore,
    fallback: &ModelSelection,
) -> ModelSelection {
    let Some((model_id, variant)) = config_store.model_selection() else {
        tracing::warn!(
            path = %config_store.path().display(),
            "host config [model] section is missing or malformed; falling back to configured model"
        );
        return fallback.clone();
    };
    match validate_selection(provider, &model_id, &variant) {
        Ok(selection) => selection,
        Err(error) => {
            tracing::warn!(
                path = %config_store.path().display(),
                %error,
                "persisted model selection is no longer available; falling back to configured model"
            );
            fallback.clone()
        }
    }
}

pub fn available_models(provider: ProviderMode) -> Vec<AvailableModel> {
    registry(provider)
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

fn registry_entry(provider: ProviderMode, model_id: &str) -> Option<&'static RegistryEntry> {
    registry(provider).iter().find(|entry| entry.id == model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            std::path::Path::new("/docs/hirsel-config.md"),
        )
        .await
        .unwrap()
    }

    #[test]
    fn registry_validates_models_and_variants() {
        let selected = validate_selection(ProviderMode::Codex, "gpt-5.6-sol", "high").unwrap();
        assert_eq!(selected.id, "gpt-5.6-sol");
        assert_eq!(selected.variant, "high");
        assert!(validate_selection(ProviderMode::Codex, "gpt-5", "high").is_err());
        assert!(validate_selection(ProviderMode::Codex, "gpt-5.6-sol", "impossible").is_err());
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
        let state =
            ModelSelectionState::load(ProviderMode::Codex, store(&dir).await, "gpt-5.6-sol")
                .await
                .unwrap();
        state
            .persist_and_select(ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "max".to_string(),
            })
            .await
            .unwrap();

        let reloaded =
            ModelSelectionState::load(ProviderMode::Codex, store(&dir).await, "gpt-5.6-sol")
                .await
                .unwrap();
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
        let state = ModelSelectionState::load(ProviderMode::Codex, store, "gpt-5.6-sol")
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
        let state =
            ModelSelectionState::load(ProviderMode::OpenRouter, store, "google/gemini-3.7-flash")
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
