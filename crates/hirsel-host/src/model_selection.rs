use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::{Context, anyhow};
use hirsel_proto::{AvailableModel, ModelSelection, ModelSnapshot};
use lash::provider::{ModelCapability, ReasoningCapability, ReasoningEncoding, ReasoningSelection};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const MODEL_SELECTION_FILE: &str = "model-selection.json";
const MAIN_MODEL_CONTEXT_TOKENS: usize = 200_000;

struct RegistryEntry {
    id: &'static str,
    label: &'static str,
    variants: &'static [&'static str],
    default_variant: &'static str,
}

// Current ChatGPT-account model metadata confirms these IDs and effort tokens.
// Keep this deliberately curated until the host has a provider-backed catalog.
const REGISTRY: &[RegistryEntry] = &[
    RegistryEntry {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        variants: &["low", "medium", "high", "xhigh", "max", "ultra"],
        default_variant: "medium",
    },
    RegistryEntry {
        id: "gpt-5.5",
        label: "GPT-5.5",
        variants: &["low", "medium", "high", "xhigh"],
        default_variant: "medium",
    },
];

#[derive(Clone)]
pub struct ModelSelectionState {
    current: Arc<RwLock<ModelSelection>>,
    path: Arc<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedSelection {
    model_id: String,
    variant: String,
}

impl ModelSelectionState {
    pub async fn load(data_dir: &Path, configured_model: &str) -> anyhow::Result<Self> {
        let path = data_dir.join(MODEL_SELECTION_FILE);
        let current = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let persisted: PersistedSelection =
                    serde_json::from_slice(&bytes).with_context(|| {
                        format!("parse persisted model selection at {}", path.display())
                    })?;
                validate_selection(&persisted.model_id, &persisted.variant).with_context(|| {
                    format!("validate persisted model selection at {}", path.display())
                })?
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                selection_for_configured_model(configured_model)?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read model selection at {}", path.display()));
            }
        };
        Ok(Self {
            current: Arc::new(RwLock::new(current)),
            path: Arc::new(path),
        })
    }

    pub fn current(&self) -> ModelSelection {
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
        persist_atomic(&self.path, &selection).await?;
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

async fn persist_atomic(path: &Path, selection: &ModelSelection) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("model selection path has no parent: {}", path.display()))?;
    tokio::fs::create_dir_all(parent).await?;
    let persisted = PersistedSelection {
        model_id: selection.id.clone(),
        variant: selection.variant.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&persisted)?;
    bytes.push(b'\n');
    let temp_path = parent.join(format!(
        ".{MODEL_SELECTION_FILE}.{}.tmp",
        Uuid::new_v4().simple()
    ));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp_path, path).await?;
        Ok::<_, std::io::Error>(())
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error)
            .with_context(|| format!("atomically persist model selection at {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let state = ModelSelectionState::load(dir.path(), "gpt-5.5")
            .await
            .unwrap();
        state
            .persist_and_select(ModelSelection {
                id: "gpt-5.6-sol".to_string(),
                variant: "max".to_string(),
            })
            .await
            .unwrap();

        let reloaded = ModelSelectionState::load(dir.path(), "gpt-5.5")
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
