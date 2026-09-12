use anyhow::anyhow;
use hirsel_drivers::AgentKind;
use hirsel_proto::{
    SubagentModel, SubagentModelCatalog, SubagentNativeWorker, SubagentProviderModels,
};
use serde_json::{Value, json};

use crate::host_config::ConfigStore;

/// Curated CLI models and their selectable reasoning efforts. Existing house
/// lanes retain their fixed effort; additional models expose supported efforts.
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

const CODEX_MODELS: &[RegistryModel] = &[
    // Workhorse: judgment-heavy implementation and review-expensive verification.
    RegistryModel {
        id: "gpt-5.6-sol",
        label: "Sol",
        variants: &["high"],
        default_variant: "high",
    },
    // Economy: mechanically verifiable work.
    RegistryModel {
        id: "gpt-5.6-luna",
        label: "Luna",
        variants: &["max"],
        default_variant: "max",
    },
    RegistryModel {
        id: "gpt-6-astra",
        label: "Astra",
        variants: &["low", "medium", "high", "xhigh", "max", "ultra"],
        default_variant: "medium",
    },
];

const CLAUDE_MODELS: &[RegistryModel] = &[
    // Workhorse: taste-critical work (UI, API shape, copy) and fresh review.
    RegistryModel {
        id: "claude-opus-5",
        label: "Opus 5",
        variants: &["high"],
        default_variant: "high",
    },
    RegistryModel {
        id: "claude-fable-5-1",
        label: "Fable 5.1",
        variants: &["low", "medium", "high", "xhigh", "max"],
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
}

impl SubagentModelState {
    pub fn load(config_store: ConfigStore) -> Self {
        Self { config_store }
    }

    pub fn snapshot(&self) -> SubagentModelCatalog {
        catalog_from_store(&self.config_store)
    }

    /// The model-facing `threads.delegate` input contract. This is derived from
    /// the same refreshed catalog used by Settings and execution validation —
    /// including the native worker row, so the Owner's enable switch and the
    /// configured provider roster reach the tool contract by one path.
    pub fn delegation_input_schema(&self) -> Value {
        Self::delegation_input_schema_for(&self.snapshot())
    }

    pub(crate) fn delegation_input_schema_for(catalog: &SubagentModelCatalog) -> Value {
        delegation_input_schema(catalog)
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
            Some(variant)
                if model
                    .enabled_variants
                    .iter()
                    .any(|allowed| allowed == variant) =>
            {
                variant.to_string()
            }
            Some(variant) => {
                return Err(anyhow!(
                    "variant `{variant}` is unknown or disabled for Sub-agent model `{}`; enabled variants: {}",
                    model.id,
                    model.enabled_variants.join(", ")
                ));
            }
            None => {
                let registry_model = provider_entry
                    .models
                    .iter()
                    .find(|entry| entry.id == model.id)
                    .expect("catalog model mirrors static registry");
                if model
                    .enabled_variants
                    .iter()
                    .any(|variant| variant == registry_model.default_variant)
                {
                    registry_model.default_variant.to_string()
                } else {
                    model
                        .enabled_variants
                        .first()
                        .expect("enabled models always have an enabled variant")
                        .clone()
                }
            }
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
        enabled_variants: &[String],
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
        if enabled_variants.is_empty() {
            return Err(anyhow!(
                "Sub-agent model `{model_id}` must have at least one enabled variant"
            ));
        }
        let unknown = enabled_variants
            .iter()
            .filter(|variant| !model.variants.contains(&variant.as_str()))
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(anyhow!(
                "unknown variants for Sub-agent model `{model_id}`: {}; available variants: {}",
                unknown
                    .iter()
                    .map(|variant| variant.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                model.variants.join(", ")
            ));
        }
        let canonical_variants = model
            .variants
            .iter()
            .filter(|variant| enabled_variants.iter().any(|enabled| enabled == **variant))
            .map(|variant| (*variant).to_string())
            .collect::<Vec<_>>();
        self.config_store
            .set_subagent_model(provider, model_id, enabled, &canonical_variants)
            .await?;
        Ok(self.snapshot())
    }

    /// Update the native worker row. The model is free text — there is no
    /// curated registry behind this route — so it is validated the same way an
    /// explicit delegate `model` is, and a blank one clears the override.
    pub async fn set_native_worker(
        &self,
        enabled: bool,
        model: Option<&str>,
    ) -> anyhow::Result<SubagentModelCatalog> {
        let model = match model.map(str::trim).filter(|model| !model.is_empty()) {
            Some(model) => Some(crate::model_selection::validate_free_text(model)?.id),
            None => None,
        };
        self.config_store
            .set_native_worker(enabled, model.as_deref())
            .await?;
        Ok(self.snapshot())
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
    let mut catalog = registry_catalog();
    catalog.native_worker = native_worker_from_store(config_store);
    for provider in &mut catalog.providers {
        let registry = registry_provider(&provider.provider)
            .expect("catalog provider mirrors static registry");
        for model in &mut provider.models {
            let Some(value) = overrides
                .get(&provider.provider)
                .and_then(|models| models.get(&model.id))
            else {
                continue;
            };
            let registry_model = registry
                .models
                .iter()
                .find(|entry| entry.id == model.id)
                .expect("catalog model mirrors static registry");
            if value.enabled_variants.is_empty()
                || !value
                    .enabled_variants
                    .iter()
                    .all(|variant| registry_model.variants.contains(&variant.as_str()))
            {
                tracing::warn!(
                    provider = provider.provider,
                    model_id = model.id,
                    variants = ?value.enabled_variants,
                    "invalid Sub-agent model override; using built-in defaults"
                );
                continue;
            }
            model.enabled = value.enabled;
            model.enabled_variants = registry_model
                .variants
                .iter()
                .filter(|variant| {
                    value
                        .enabled_variants
                        .iter()
                        .any(|enabled| enabled == **variant)
                })
                .map(|variant| (*variant).to_string())
                .collect();
        }
    }
    catalog
}

/// The native worker row as the Owner configured it: the stored enable switch
/// and model override, resolved against the provider instances that can
/// actually host the worker right now.
fn native_worker_from_store(config_store: &ConfigStore) -> SubagentNativeWorker {
    let stored = config_store.native_worker_override();
    let eligible = crate::providers::native_worker_provider_ids(config_store);
    let provider_id = eligible
        .iter()
        .find(|id| id.as_str() == crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID)
        .cloned();
    // Unavailable means exactly one thing: nothing can host the worker. A
    // roster without the default instance is still usable — a delegation names
    // one of the eligible instances explicitly — so it is not a refusal.
    let unavailable_reason = eligible.is_empty().then(|| {
        "No configured provider has an API key, so there is nothing to run the worker on."
            .to_string()
    });
    SubagentNativeWorker {
        label: "Native worker".to_string(),
        enabled: stored.enabled,
        provider_id,
        eligible_provider_ids: eligible,
        model: stored
            .model
            .clone()
            .unwrap_or_else(|| crate::providers::NATIVE_WORKER_DEFAULT_MODEL.to_string()),
        default_model: crate::providers::NATIVE_WORKER_DEFAULT_MODEL.to_string(),
        model_override: stored.model,
        unavailable_reason,
    }
}

pub(crate) fn registry_catalog() -> SubagentModelCatalog {
    SubagentModelCatalog {
        native_worker: SubagentNativeWorker {
            label: "Native worker".to_string(),
            enabled: true,
            provider_id: None,
            eligible_provider_ids: Vec::new(),
            model: crate::providers::NATIVE_WORKER_DEFAULT_MODEL.to_string(),
            default_model: crate::providers::NATIVE_WORKER_DEFAULT_MODEL.to_string(),
            model_override: None,
            unavailable_reason: Some(
                "No configured provider has an API key, so there is nothing to run the worker on."
                    .to_string(),
            ),
        },
        providers: REGISTRY
            .iter()
            .map(|provider| SubagentProviderModels {
                provider: provider.id.to_string(),
                label: provider.label.to_string(),
                models: provider
                    .models
                    .iter()
                    .map(|model| SubagentModel {
                        id: model.id.to_string(),
                        label: model.label.to_string(),
                        variants: model
                            .variants
                            .iter()
                            .map(|variant| (*variant).to_string())
                            .collect(),
                        enabled_variants: model
                            .variants
                            .iter()
                            .map(|variant| (*variant).to_string())
                            .collect(),
                        enabled: true,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn delegation_input_schema(catalog: &SubagentModelCatalog) -> Value {
    let native_worker_providers: &[String] = if catalog.native_worker.enabled {
        &catalog.native_worker.eligible_provider_ids
    } else {
        &[]
    };
    let mut agents = vec!["host", "claude", "codex"];
    let mut branches = vec![
        json!({"required":["agent"],"properties":{"agent":{"const":"host"}},"not":{"anyOf":[{"required":["provider_id"]},{"required":["model"]},{"required":["variant"]},{"required":["cwd"]}]}}),
    ];
    // An existing child with no new selectors keeps its accepted backend.
    branches.push(json!({"required":["child_thread_id"],"not":{"anyOf":[{"required":["agent"]},{"required":["provider_id"]},{"required":["model"]},{"required":["variant"]},{"required":["cwd"]}]}}));
    for provider in &catalog.providers {
        let enabled = provider
            .models
            .iter()
            .filter(|m| m.enabled)
            .collect::<Vec<_>>();
        let Some(default) = enabled.first() else {
            continue;
        };
        let mut models = vec![
            json!({"not":{"required":["model"]},"properties":{"variant":{"enum":default.enabled_variants}}}),
        ];
        for model in enabled {
            models.push(json!({"required":["model"],"properties":{"model":{"const":model.id},"variant":{"enum":model.enabled_variants}}}));
        }
        let explicit = json!({"required":["agent"],"properties":{"agent":{"const":provider.provider}},"not":{"required":["provider_id"]},"oneOf":models});
        branches.push(explicit);
        if provider.provider == "claude" {
            branches.push(json!({"not":{"anyOf":[{"required":["agent"]},{"required":["provider_id"]}]},"anyOf":[{"not":{"required":["child_thread_id"]}},{"required":["model"]},{"required":["variant"]},{"required":["cwd"]}],"oneOf":models}));
        }
    }
    if !native_worker_providers.is_empty() {
        agents.push("lash");
        let mut provider_branches = native_worker_providers
            .iter()
            .map(|provider| {
                let requires_model =
                    provider != crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID;
                let mut branch = json!({
                    "required":["provider_id"],
                    "properties":{"provider_id":{"const":provider}}
                });
                if requires_model {
                    branch["required"] = json!(["provider_id", "model"]);
                }
                branch
            })
            .collect::<Vec<_>>();
        if native_worker_providers
            .iter()
            .any(|provider| provider == crate::providers::NATIVE_WORKER_DEFAULT_PROVIDER_ID)
        {
            provider_branches.push(json!({"not":{"required":["provider_id"]}}));
        }
        branches.push(json!({
            "required":["agent"],
            "properties":{"agent":{"const":"lash"},"variant":{"const":"default"}},
            "oneOf":provider_branches
        }));
    }
    json!({"type":"object","additionalProperties":false,"required":["title","brief","artifact_ids"],"properties":{"title":{"type":"string","minLength":1},"brief":{"type":"string","minLength":1},"artifact_ids":{"type":"array","maxItems":100,"items":{"type":"integer","minimum":1}},"child_thread_id":{"type":"integer","minimum":0},"agent":{"type":"string","enum":agents},"provider_id":{"type":"string"},"model":{"type":"string","minLength":1},"variant":{"type":"string"},"cwd":{"type":"string"}},"oneOf":branches})
}

fn registry_provider(provider: &str) -> Option<&'static RegistryProvider> {
    REGISTRY.iter().find(|entry| entry.id == provider)
}

#[cfg(test)]
mod tests;
