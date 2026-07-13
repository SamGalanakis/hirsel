use std::sync::{Arc, RwLock};

use anyhow::anyhow;
use hirsel_drivers::AgentKind;
use hirsel_proto::{SubagentModel, SubagentModelCatalog, SubagentProviderModels};

use crate::host_config::ConfigStore;

struct RegistryModel {
    id: &'static str,
    label: &'static str,
    variants: &'static [&'static str],
    default_variant: &'static str,
}

struct RegistryProvider {
    id: &'static str,
    label: &'static str,
    agent: AgentKind,
    models: &'static [RegistryModel],
}

const CODEX_MODELS: &[RegistryModel] = &[RegistryModel {
    id: "gpt-5.5",
    label: "GPT-5.5",
    variants: &["low", "medium", "high"],
    default_variant: "high",
}];

const CLAUDE_MODELS: &[RegistryModel] = &[
    RegistryModel {
        id: "claude-opus-4-8",
        label: "Opus 4.8",
        variants: &["low", "medium", "high"],
        default_variant: "high",
    },
    RegistryModel {
        id: "claude-sonnet-5",
        label: "Sonnet 5",
        variants: &["low", "medium", "high"],
        default_variant: "medium",
    },
    RegistryModel {
        id: "claude-fable-5",
        label: "Fable 5",
        variants: &["low", "medium", "high"],
        default_variant: "high",
    },
];

const REGISTRY: &[RegistryProvider] = &[
    RegistryProvider {
        id: "codex",
        label: "Codex CLI",
        agent: AgentKind::Codex,
        models: CODEX_MODELS,
    },
    RegistryProvider {
        id: "claude",
        label: "Claude Code CLI",
        agent: AgentKind::Claude,
        models: CLAUDE_MODELS,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSubagentModel {
    pub model_id: String,
    pub variant: String,
}

#[derive(Clone)]
pub struct SubagentModelState {
    config_store: ConfigStore,
    catalog: Arc<RwLock<SubagentModelCatalog>>,
}

impl SubagentModelState {
    pub fn load(config_store: ConfigStore) -> Self {
        let catalog = catalog_from_store(&config_store);
        Self {
            config_store,
            catalog: Arc::new(RwLock::new(catalog)),
        }
    }

    pub fn snapshot(&self) -> SubagentModelCatalog {
        self.refresh();
        self.catalog
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn resolve(
        &self,
        provider: AgentKind,
        requested_model: Option<&str>,
        requested_variant: Option<&str>,
    ) -> anyhow::Result<ResolvedSubagentModel> {
        let catalog = self.snapshot();
        let provider_entry = REGISTRY
            .iter()
            .find(|entry| entry.agent == provider)
            .expect("every AgentKind has a Sub-agent model registry");
        let configured = catalog
            .providers
            .iter()
            .find(|entry| entry.provider == provider_entry.id)
            .expect("catalog mirrors the static provider registry");
        let enabled = configured
            .models
            .iter()
            .filter(|model| model.enabled)
            .collect::<Vec<_>>();
        let model = if let Some(model_id) = requested_model {
            enabled
                .iter()
                .copied()
                .find(|model| model.id == model_id)
                .ok_or_else(|| unavailable_model_error(provider_entry, model_id, &enabled))?
        } else {
            enabled.first().copied().ok_or_else(|| {
                anyhow!(
                    "no enabled Sub-agent models for provider `{}`; enabled models: none",
                    provider_entry.id
                )
            })?
        };
        let variant = match requested_variant {
            Some(variant) if model.variants.iter().any(|allowed| allowed == variant) => {
                variant.to_string()
            }
            Some(variant) => {
                return Err(anyhow!(
                    "unknown variant `{variant}` for Sub-agent model `{}`; available variants: {}",
                    model.id,
                    model.variants.join(", ")
                ));
            }
            None => model.default_variant.clone(),
        };
        Ok(ResolvedSubagentModel {
            model_id: model.id.clone(),
            variant,
        })
    }

    pub async fn set(
        &self,
        provider: &str,
        model_id: &str,
        enabled: bool,
        default_variant: &str,
    ) -> anyhow::Result<SubagentModelCatalog> {
        let provider_entry = registry_provider(provider)
            .ok_or_else(|| anyhow!("unknown Sub-agent provider: {provider}"))?;
        let model = provider_entry
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or_else(|| {
                anyhow!("unknown Sub-agent model `{model_id}` for provider `{provider}`")
            })?;
        if !model.variants.contains(&default_variant) {
            return Err(anyhow!(
                "unknown default variant `{default_variant}` for Sub-agent model `{model_id}`; available variants: {}",
                model.variants.join(", ")
            ));
        }
        self.config_store
            .set_subagent_model(provider, model_id, enabled, default_variant)
            .await?;
        Ok(self.snapshot())
    }

    fn refresh(&self) {
        let refreshed = catalog_from_store(&self.config_store);
        *self
            .catalog
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = refreshed;
    }
}

fn unavailable_model_error(
    provider: &RegistryProvider,
    requested: &str,
    enabled: &[&SubagentModel],
) -> anyhow::Error {
    let enabled = enabled
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    anyhow!(
        "Sub-agent model `{requested}` is unknown or disabled for provider `{}`; enabled models: {}",
        provider.id,
        if enabled.is_empty() {
            "none".to_string()
        } else {
            enabled.join(", ")
        }
    )
}

fn catalog_from_store(config_store: &ConfigStore) -> SubagentModelCatalog {
    let overrides = config_store.subagent_model_overrides();
    for (provider, models) in &overrides {
        let Some(provider_entry) = registry_provider(provider) else {
            tracing::warn!(provider, "ignoring stale Sub-agent provider config");
            continue;
        };
        for model_id in models.keys() {
            if !provider_entry
                .models
                .iter()
                .any(|model| model.id == model_id)
            {
                tracing::warn!(provider, model_id, "ignoring stale Sub-agent model config");
            }
        }
    }
    SubagentModelCatalog {
        providers: REGISTRY
            .iter()
            .map(|provider| SubagentProviderModels {
                provider: provider.id.to_string(),
                label: provider.label.to_string(),
                models: provider
                    .models
                    .iter()
                    .map(|model| {
                        let override_value = overrides
                            .get(provider.id)
                            .and_then(|models| models.get(model.id));
                        let (enabled, default_variant) = match override_value {
                            Some(value)
                                if model.variants.contains(&value.default_variant.as_str()) =>
                            {
                                (value.enabled, value.default_variant.clone())
                            }
                            Some(value) => {
                                tracing::warn!(
                                    provider = provider.id,
                                    model_id = model.id,
                                    variant = value.default_variant,
                                    "invalid Sub-agent model override; using built-in defaults"
                                );
                                (true, model.default_variant.to_string())
                            }
                            None => (true, model.default_variant.to_string()),
                        };
                        SubagentModel {
                            id: model.id.to_string(),
                            label: model.label.to_string(),
                            variants: model
                                .variants
                                .iter()
                                .map(|variant| (*variant).to_string())
                                .collect(),
                            default_variant,
                            enabled,
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn registry_provider(provider: &str) -> Option<&'static RegistryProvider> {
    REGISTRY.iter().find(|entry| entry.id == provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_state(dir: &tempfile::TempDir) -> SubagentModelState {
        let store = ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            std::path::Path::new("/docs/hirsel-config.md"),
        )
        .await
        .unwrap();
        SubagentModelState::load(store)
    }

    #[test]
    fn registry_defaults_and_variants_are_valid() {
        for provider in REGISTRY {
            assert!(!provider.models.is_empty());
            for model in provider.models {
                assert!(model.variants.contains(&model.default_variant));
            }
        }
    }

    #[tokio::test]
    async fn persistence_round_trips_via_config_store() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        state
            .set("claude", "claude-opus-4-8", false, "low")
            .await
            .unwrap();

        let reloaded = test_state(&dir).await;
        let opus = &reloaded.snapshot().providers[1].models[0];
        assert!(!opus.enabled);
        assert_eq!(opus.default_variant, "low");
    }

    #[tokio::test]
    async fn stale_persisted_model_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let mut text = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
        text.push_str(
            "\n[subagent_models.codex.retired-model]\nenabled = false\ndefault_variant = \"high\"\n",
        );
        std::fs::write(dir.path().join("hirsel.toml"), text).unwrap();
        let catalog = state.snapshot();
        assert_eq!(catalog.providers[0].models.len(), 1);
        assert_eq!(catalog.providers[0].models[0].id, "gpt-5.5");
    }

    #[tokio::test]
    async fn resolve_defaults_and_rejects_disabled_unknown_and_bad_variants() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        assert_eq!(
            state.resolve(AgentKind::Claude, None, None).unwrap(),
            ResolvedSubagentModel {
                model_id: "claude-opus-4-8".to_string(),
                variant: "high".to_string(),
            }
        );
        assert_eq!(
            state
                .resolve(AgentKind::Claude, Some("claude-sonnet-5"), Some("low"))
                .unwrap()
                .variant,
            "low"
        );
        state
            .set("claude", "claude-opus-4-8", false, "high")
            .await
            .unwrap();
        assert!(
            state
                .resolve(AgentKind::Claude, Some("claude-opus-4-8"), None)
                .unwrap_err()
                .to_string()
                .contains("claude-sonnet-5")
        );
        assert!(
            state
                .resolve(AgentKind::Codex, Some("unknown"), None)
                .is_err()
        );
        assert!(
            state
                .resolve(AgentKind::Codex, None, Some("xhigh"))
                .is_err()
        );
    }
}
