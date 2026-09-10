use anyhow::anyhow;
use hirsel_drivers::AgentKind;
use hirsel_proto::{SubagentModel, SubagentModelCatalog, SubagentProviderModels};
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
    /// the same refreshed catalog used by Settings and execution validation.
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

pub(crate) fn registry_catalog() -> SubagentModelCatalog {
    SubagentModelCatalog {
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
    let mut branches = vec![
        json!({"required":["agent"],"properties":{"agent":{"const":"host"}},"not":{"anyOf":[{"required":["model"]},{"required":["variant"]},{"required":["cwd"]}]}}),
    ];
    // An existing child with no new selectors keeps its accepted backend.
    branches.push(json!({"required":["child_thread_id"],"not":{"anyOf":[{"required":["agent"]},{"required":["model"]},{"required":["variant"]},{"required":["cwd"]}]}}));
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
        let explicit = json!({"required":["agent"],"properties":{"agent":{"const":provider.provider}},"oneOf":models});
        branches.push(explicit);
        if provider.provider == "claude" {
            branches.push(json!({"not":{"required":["agent"]},"anyOf":[{"not":{"required":["child_thread_id"]}},{"required":["model"]},{"required":["variant"]},{"required":["cwd"]}],"oneOf":models}));
        }
    }
    json!({"type":"object","additionalProperties":false,"required":["title","brief","artifact_ids"],"properties":{"title":{"type":"string","minLength":1},"brief":{"type":"string","minLength":1},"artifact_ids":{"type":"array","maxItems":100,"items":{"type":"integer","minimum":1}},"child_thread_id":{"type":"integer","minimum":0},"agent":{"type":"string","enum":["host","claude","codex"]},"model":{"type":"string"},"variant":{"type":"string"},"cwd":{"type":"string"}},"oneOf":branches})
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
            std::path::Path::new("/docs/hirsel-config.md"),
            &crate::host_config::EnvBootstrap::default(),
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
                assert!(!model.variants.is_empty());
                assert!(model.variants.contains(&model.default_variant));
            }
        }
    }

    /// New choices do not reorder the existing CLI defaults or retune lanes.
    #[test]
    fn registry_preserves_existing_lanes_and_adds_supported_models() {
        let catalog = registry_catalog();
        let lanes = catalog
            .providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .map(|model| {
                (
                    model.id.as_str(),
                    model.enabled,
                    model.enabled_variants.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lanes,
            [
                ("gpt-5.6-sol", true, vec!["high".to_string()]),
                ("gpt-5.6-luna", true, vec!["max".to_string()]),
                (
                    "gpt-6-astra",
                    true,
                    vec!["low", "medium", "high", "xhigh", "max", "ultra"]
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                ),
                ("claude-opus-5", true, vec!["high".to_string()]),
                (
                    "claude-fable-5-1",
                    true,
                    vec!["low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                ),
            ]
        );
    }

    /// An explicit hirsel.toml override still wins over the shipped defaults.
    #[tokio::test]
    async fn overrides_win_over_default_enablement() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let is_enabled = |catalog: &SubagentModelCatalog, id: &str| {
            catalog
                .providers
                .iter()
                .flat_map(|provider| provider.models.iter())
                .find(|model| model.id == id)
                .unwrap()
                .enabled
        };

        let catalog = state.snapshot();
        assert!(is_enabled(&catalog, "gpt-5.6-luna"));

        let catalog = state
            .set("codex", "gpt-5.6-luna", false, &["max".to_string()])
            .await
            .unwrap();
        assert!(!is_enabled(&catalog, "gpt-5.6-luna"));

        let catalog = state
            .set("codex", "gpt-5.6-luna", true, &["max".to_string()])
            .await
            .unwrap();
        assert!(is_enabled(&catalog, "gpt-5.6-luna"));
    }

    #[tokio::test]
    async fn persistence_round_trips_via_config_store() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        state
            .set("claude", "claude-opus-5", false, &["high".to_string()])
            .await
            .unwrap();

        let reloaded = test_state(&dir).await;
        let catalog = reloaded.snapshot();
        let opus = catalog.providers[1]
            .models
            .iter()
            .find(|model| model.id == "claude-opus-5")
            .unwrap();
        assert!(!opus.enabled);
        assert_eq!(opus.enabled_variants, ["high"]);
    }

    #[tokio::test]
    async fn stale_persisted_model_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let mut text = std::fs::read_to_string(dir.path().join("hirsel.toml")).unwrap();
        // Both a never-known model and a retired lane are ignored, not fatal.
        text.push_str(
            "\n[subagent_models.codex.retired-model]\nenabled = false\nenabled_variants = [\"high\"]\n\
             \n[subagent_models.codex.\"gpt-5.6-terra\"]\nenabled = true\nenabled_variants = [\"medium\"]\n\
             \n[subagent_models.claude.claude-sonnet-5]\nenabled = true\nenabled_variants = [\"medium\"]\n",
        );
        std::fs::write(dir.path().join("hirsel.toml"), text).unwrap();
        let catalog = state.snapshot();
        assert_eq!(
            catalog.providers[0]
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.6-sol", "gpt-5.6-luna", "gpt-6-astra"]
        );
        assert_eq!(
            catalog.providers[1]
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["claude-opus-5", "claude-fable-5-1"]
        );
    }

    #[tokio::test]
    async fn resolve_defaults_and_rejects_disabled_unknown_and_bad_variants() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        assert_eq!(
            state.resolve(AgentKind::Claude, None, None).unwrap(),
            ResolvedSubagentModel {
                model_id: "claude-opus-5".to_string(),
                variant: "high".to_string(),
            }
        );
        assert_eq!(
            state.resolve(AgentKind::Codex, None, None).unwrap(),
            ResolvedSubagentModel {
                model_id: "gpt-5.6-sol".to_string(),
                variant: "high".to_string(),
            }
        );
        assert_eq!(
            state
                .resolve(AgentKind::Codex, Some("gpt-5.6-luna"), None)
                .unwrap()
                .variant,
            "max"
        );
        state
            .set("claude", "claude-opus-5", false, &["high".to_string()])
            .await
            .unwrap();
        assert!(
            state
                .resolve(AgentKind::Claude, Some("claude-opus-5"), None)
                .unwrap_err()
                .to_string()
                .contains("enabled models: claude-fable-5-1")
        );
        assert!(
            state
                .resolve(AgentKind::Codex, Some("unknown"), None)
                .is_err()
        );
        // Efforts outside the lane are rejected: there is no per-task tuning.
        assert!(
            state
                .resolve(AgentKind::Codex, None, Some("xhigh"))
                .is_err()
        );
        assert!(state.resolve(AgentKind::Codex, None, Some("high")).is_ok());
    }

    #[tokio::test]
    async fn delegation_schema_tracks_enabled_models_and_variants() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let schema = state.delegation_input_schema();
        let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
        let spawn = |agent: &str, model: &str, effort: &str| {
            json!({
                "agent": agent,
                "model": model,
                "variant": effort,
                "title":"Research", "brief": "Research Linear triage.", "artifact_ids":[]
            })
        };

        assert!(
            validator
                .validate(&spawn("claude", "claude-opus-5", "high"))
                .is_ok()
        );
        assert!(
            validator
                .validate(&spawn("codex", "gpt-5.6-luna", "max"))
                .is_ok()
        );
        assert!(
            validator
                .validate(&spawn("codex", "gpt-5.6-luna", "high"))
                .is_err()
        );
        assert!(
            validator
                .validate(&spawn("claude", "claude-sonnet-5", "high"))
                .is_err()
        );

        state
            .set("claude", "claude-opus-5", false, &["high".to_string()])
            .await
            .unwrap();
        let validator = jsonschema::JSONSchema::compile(&state.delegation_input_schema()).unwrap();
        assert!(
            validator
                .validate(&spawn("claude", "claude-opus-5", "high"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn new_models_resolve_persist_and_refresh_the_delegation_schema() {
        for (agent, provider, id, default, efforts) in [
            (
                AgentKind::Codex,
                "codex",
                "gpt-6-astra",
                "medium",
                &["low", "medium", "high", "xhigh", "max", "ultra"][..],
            ),
            (
                AgentKind::Claude,
                "claude",
                "claude-fable-5-1",
                "high",
                &["low", "medium", "high", "xhigh", "max"][..],
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let state = test_state(&dir).await;
            assert_eq!(
                state.resolve(agent, Some(id), None).unwrap().variant,
                default
            );
            let input = |effort: &str| json!({"agent":provider,"model":id,"variant":effort,"title":"Work","brief":"Do the work","artifact_ids":[]});
            let validator =
                jsonschema::JSONSchema::compile(&state.delegation_input_schema()).unwrap();
            for effort in efforts {
                assert_eq!(
                    state
                        .resolve(agent, Some(id), Some(effort))
                        .unwrap()
                        .variant,
                    *effort
                );
                assert!(validator.is_valid(&input(effort)));
            }
            assert!(state.resolve(agent, Some(id), Some("impossible")).is_err());
            assert!(!validator.is_valid(&input("impossible")));
            assert!(
                state
                    .set(provider, id, true, &["impossible".into()])
                    .await
                    .is_err()
            );

            state
                .set(provider, id, true, &["high".into()])
                .await
                .unwrap();
            let restricted = test_state(&dir).await;
            assert_eq!(
                restricted.resolve(agent, Some(id), None).unwrap().variant,
                "high"
            );
            assert!(restricted.resolve(agent, Some(id), Some("low")).is_err());
            let validator =
                jsonschema::JSONSchema::compile(&restricted.delegation_input_schema()).unwrap();
            assert!(validator.is_valid(&input("high")));
            assert!(!validator.is_valid(&input("low")));

            restricted
                .set(provider, id, false, &["high".into()])
                .await
                .unwrap();
            let disabled = test_state(&dir).await;
            assert!(disabled.resolve(agent, Some(id), None).is_err());
            assert!(
                !jsonschema::JSONSchema::compile(&disabled.delegation_input_schema())
                    .unwrap()
                    .is_valid(&input("high"))
            );
        }
    }

    #[tokio::test]
    async fn set_rejects_empty_and_unknown_variant_sets() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        assert!(
            state
                .set("codex", "gpt-5.6-sol", true, &[])
                .await
                .unwrap_err()
                .to_string()
                .contains("at least one")
        );
        assert!(
            state
                .set("codex", "gpt-5.6-sol", true, &["impossible".to_string()],)
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown variants")
        );
    }
}
