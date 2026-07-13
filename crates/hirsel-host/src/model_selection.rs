use std::sync::{Arc, RwLock};

use anyhow::anyhow;
use hirsel_proto::{AvailableModel, ModelSelection, ModelSnapshot};
use lash::provider::{ModelCapability, ReasoningCapability, ReasoningEncoding, ReasoningSelection};

use crate::host_config::ConfigStore;

const MAIN_MODEL_CONTEXT_TOKENS: usize = 200_000;

struct RegistryEntry {
    id: &'static str,
    label: &'static str,
    variants: &'static [&'static str],
    default_variant: &'static str,
}

// Current ChatGPT-account model metadata confirms this ID and its effort tokens.
// The main agent is deliberately pinned to GPT-5.6 Sol; keep this curated until
// the host has a provider-backed catalog.
const REGISTRY: &[RegistryEntry] = &[RegistryEntry {
    id: "gpt-5.6-sol",
    label: "GPT-5.6 Sol",
    variants: &["low", "medium", "high", "xhigh", "max"],
    default_variant: "medium",
}];

#[derive(Clone)]
pub struct ModelSelectionState {
    current: Arc<RwLock<ModelSelection>>,
    fallback: ModelSelection,
    config_store: ConfigStore,
}

impl ModelSelectionState {
    pub async fn load(config_store: ConfigStore, configured_model: &str) -> anyhow::Result<Self> {
        let fallback = selection_for_configured_model(configured_model)?;
        let current = selection_from_store(&config_store, &fallback);
        Ok(Self {
            current: Arc::new(RwLock::new(current)),
            fallback,
            config_store,
        })
    }

    pub fn current(&self) -> ModelSelection {
        let selection = selection_from_store(&self.config_store, &self.fallback);
        *self
            .current
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = selection.clone();
        self.current
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn snapshot(&self) -> ModelSnapshot {
        ModelSnapshot {
            current: self.current(),
            available: available_models(),
        }
    }

    pub fn validate(&self, model_id: &str, variant: &str) -> anyhow::Result<ModelSelection> {
        validate_selection(model_id, variant)
    }

    pub async fn persist_and_select(&self, selection: ModelSelection) -> anyhow::Result<()> {
        self.config_store
            .set_model_selection(&selection.id, &selection.variant)
            .await?;
        *self
            .current
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = selection;
        Ok(())
    }

    pub fn model_spec(&self) -> anyhow::Result<lash::ModelSpec> {
        model_spec(&self.current())
    }
}

fn selection_from_store(config_store: &ConfigStore, fallback: &ModelSelection) -> ModelSelection {
    let Some((model_id, variant)) = config_store.model_selection() else {
        tracing::warn!(
            path = %config_store.path().display(),
            "host config [model] section is missing or malformed; falling back to configured model"
        );
        return fallback.clone();
    };
    match validate_selection(&model_id, &variant) {
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

pub fn available_models() -> Vec<AvailableModel> {
    REGISTRY
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

pub fn model_spec(selection: &ModelSelection) -> anyhow::Result<lash::ModelSpec> {
    let entry =
        registry_entry(&selection.id).ok_or_else(|| anyhow!("unknown model: {}", selection.id))?;
    let capability = ModelCapability {
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
    };
    lash::ModelSpec::from_token_limits(
        selection.id.clone(),
        ReasoningSelection::Effort(selection.variant.clone()),
        MAIN_MODEL_CONTEXT_TOKENS,
        None,
    )
    .map(|spec| spec.with_capability(capability))
    .map_err(anyhow::Error::msg)
}

fn selection_for_configured_model(configured_model: &str) -> anyhow::Result<ModelSelection> {
    let model_id = configured_model.trim();
    let entry = registry_entry(model_id).ok_or_else(|| {
        anyhow!("HIRSEL_MODEL `{configured_model}` is not available for runtime selection")
    })?;
    Ok(ModelSelection {
        id: entry.id.to_string(),
        variant: entry.default_variant.to_string(),
    })
}

fn validate_selection(model_id: &str, variant: &str) -> anyhow::Result<ModelSelection> {
    let entry = registry_entry(model_id).ok_or_else(|| anyhow!("unknown model: {model_id}"))?;
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

fn registry_entry(model_id: &str) -> Option<&'static RegistryEntry> {
    REGISTRY.iter().find(|entry| entry.id == model_id)
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
        let selected = validate_selection("gpt-5.6-sol", "high").unwrap();
        assert_eq!(selected.id, "gpt-5.6-sol");
        assert_eq!(selected.variant, "high");
        assert!(validate_selection("gpt-5", "high").is_err());
        assert!(validate_selection("gpt-5.6-sol", "impossible").is_err());
    }

    #[tokio::test]
    async fn persistence_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = ModelSelectionState::load(store(&dir).await, "gpt-5.6-sol")
            .await
            .unwrap();
        state
            .persist_and_select(ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "max".to_string(),
            })
            .await
            .unwrap();

        let reloaded = ModelSelectionState::load(store(&dir).await, "gpt-5.6-sol")
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
        let state = ModelSelectionState::load(store, "gpt-5.6-sol")
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

    #[test]
    fn model_spec_carries_the_selected_effort_and_capability() {
        let spec = model_spec(&ModelSelection {
            id: "gpt-5.6-sol".to_string(),
            variant: "high".to_string(),
        })
        .unwrap();
        assert_eq!(spec.id, "gpt-5.6-sol");
        assert_eq!(spec.variant.effort(), Some("high"));
        assert!(
            spec.capability
                .reasoning
                .expect("reasoning capability")
                .efforts
                .contains(&"high".to_string())
        );
    }
}
