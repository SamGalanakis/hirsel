use std::{
    collections::BTreeMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use anyhow::{Context, anyhow};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use toml_edit::{DocumentMut, Item, Table, value};
use uuid::Uuid;

const LEGACY_MODEL_SELECTION_FILE: &str = "model-selection.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentModelOverride {
    pub enabled: bool,
    pub default_variant: String,
}

#[derive(Clone)]
pub struct ConfigStore {
    path: Arc<PathBuf>,
    defaults: Arc<DocumentMut>,
    inner: Arc<Mutex<StoreInner>>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

struct StoreInner {
    document: DocumentMut,
    modified: Option<SystemTime>,
}

#[derive(Debug, Deserialize)]
struct LegacyModelSelection {
    model_id: String,
    variant: String,
}

impl ConfigStore {
    pub async fn load(path: PathBuf, data_dir: &Path, docs_path: &Path) -> anyhow::Result<Self> {
        let defaults = default_document(docs_path)?;
        let (document, modified) = match tokio::fs::read_to_string(&path).await {
            Ok(contents) => (
                parse_or_defaults(&path, &contents, &defaults),
                file_modified(&path),
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let mut document = defaults.clone();
                migrate_legacy_model(data_dir, &mut document).await;
                persist_atomic(&path, document.to_string().as_bytes()).await?;
                let modified = file_modified(&path);
                (document, modified)
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read host config at {}", path.display()));
            }
        };
        Ok(Self {
            path: Arc::new(path),
            defaults: Arc::new(defaults),
            inner: Arc::new(Mutex::new(StoreInner { document, modified })),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reload_if_changed(&self) {
        let observed = file_modified(&self.path);
        let previous = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .modified;
        if observed == previous {
            return;
        }
        let document = match std::fs::read_to_string(&*self.path) {
            Ok(contents) => parse_or_defaults(&self.path, &contents, &self.defaults),
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "failed to reload host config; keeping the last valid in-memory configuration"
                );
                return;
            }
        };
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.document = document;
        inner.modified = observed;
    }

    pub fn model_selection(&self) -> Option<(String, String)> {
        self.reload_if_changed();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let section = inner.document.get("model")?.as_table()?;
        let id = section.get("id")?.as_str()?.to_string();
        let variant = section.get("variant")?.as_str()?.to_string();
        Some((id, variant))
    }

    pub fn subagent_model_overrides(
        &self,
    ) -> BTreeMap<String, BTreeMap<String, SubagentModelOverride>> {
        self.reload_if_changed();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(providers) = inner
            .document
            .get("subagent_models")
            .and_then(Item::as_table)
        else {
            return BTreeMap::new();
        };
        let mut parsed = BTreeMap::new();
        for (provider, item) in providers {
            let Some(models) = item.as_table() else {
                tracing::warn!(
                    provider,
                    "invalid Sub-agent provider config; using defaults"
                );
                continue;
            };
            let mut parsed_models = BTreeMap::new();
            for (model_id, item) in models {
                let Some(section) = item.as_table() else {
                    tracing::warn!(
                        provider,
                        model_id,
                        "invalid Sub-agent model config; using defaults"
                    );
                    continue;
                };
                let Some(enabled) = section.get("enabled").and_then(Item::as_bool) else {
                    tracing::warn!(
                        provider,
                        model_id,
                        "Sub-agent model enabled flag is missing or invalid; using defaults"
                    );
                    continue;
                };
                let Some(default_variant) = section
                    .get("default_variant")
                    .and_then(Item::as_str)
                    .map(str::to_string)
                else {
                    tracing::warn!(
                        provider,
                        model_id,
                        "Sub-agent default variant is missing or invalid; using defaults"
                    );
                    continue;
                };
                parsed_models.insert(
                    model_id.to_string(),
                    SubagentModelOverride {
                        enabled,
                        default_variant,
                    },
                );
            }
            parsed.insert(provider.to_string(), parsed_models);
        }
        parsed
    }

    pub async fn set_model_selection(&self, model_id: &str, variant: &str) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let contents = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            ensure_table(&mut inner.document, "model");
            inner.document["model"]["id"] = value(model_id);
            inner.document["model"]["variant"] = value(variant);
            inner.document.to_string()
        };
        self.persist_and_mark(contents).await
    }

    pub async fn set_subagent_model(
        &self,
        provider: &str,
        model_id: &str,
        enabled: bool,
        default_variant: &str,
    ) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let contents = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            ensure_table(&mut inner.document, "subagent_models");
            ensure_child_table(&mut inner.document["subagent_models"], provider);
            ensure_child_table(&mut inner.document["subagent_models"][provider], model_id);
            inner.document["subagent_models"][provider][model_id]["enabled"] = value(enabled);
            inner.document["subagent_models"][provider][model_id]["default_variant"] =
                value(default_variant);
            inner.document.to_string()
        };
        self.persist_and_mark(contents).await
    }

    async fn persist_and_mark(&self, contents: String) -> anyhow::Result<()> {
        persist_atomic(&self.path, contents.as_bytes()).await?;
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .modified = file_modified(&self.path);
        Ok(())
    }
}

fn ensure_table(document: &mut DocumentMut, key: &str) {
    if !document.get(key).is_some_and(Item::is_table) {
        document[key] = Item::Table(Table::new());
    }
}

fn ensure_child_table(parent: &mut Item, key: &str) {
    if !parent.get(key).is_some_and(|item| item.is_table()) {
        parent[key] = Item::Table(Table::new());
    }
}

fn parse_or_defaults(path: &Path, contents: &str, defaults: &DocumentMut) -> DocumentMut {
    match contents.parse::<DocumentMut>() {
        Ok(document) => document,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "host config is malformed; using built-in defaults"
            );
            defaults.clone()
        }
    }
}

fn default_document(docs_path: &Path) -> anyhow::Result<DocumentMut> {
    format!(
        r#"# Hirsel host configuration.
# This file is safe to hand-edit (or edit with an Agent).
# The host watches it and reloads changes live; no restart is required.
# Documentation: {}

# Main Agent model. Models: gpt-5.6-sol. Variants: low, medium, high, xhigh, max.
[model]
id = "gpt-5.6-sol"
variant = "medium"

# Codex CLI model gpt-5.5. Variants: low, medium, high.
[subagent_models.codex."gpt-5.5"]
enabled = true
default_variant = "high"

# Claude Code CLI model claude-opus-4-8. Variants: low, medium, high.
[subagent_models.claude.claude-opus-4-8]
enabled = true
default_variant = "high"

# Claude Code CLI model claude-sonnet-5. Variants: low, medium, high.
[subagent_models.claude.claude-sonnet-5]
enabled = true
default_variant = "medium"

# Claude Code CLI model claude-fable-5. Variants: low, medium, high.
[subagent_models.claude.claude-fable-5]
enabled = true
default_variant = "high"
"#,
        docs_path.display()
    )
    .parse::<DocumentMut>()
    .context("parse built-in Hirsel host config")
}

async fn migrate_legacy_model(data_dir: &Path, document: &mut DocumentMut) {
    let path = data_dir.join(LEGACY_MODEL_SELECTION_FILE);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to read legacy model selection");
            return;
        }
    };
    match serde_json::from_slice::<LegacyModelSelection>(&bytes) {
        Ok(selection) => {
            document["model"]["id"] = value(selection.model_id);
            document["model"]["variant"] = value(selection.variant);
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "legacy model selection is malformed; using defaults");
        }
    }
}

fn file_modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

async fn persist_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("host config path has no parent: {}", path.display()))?;
    tokio::fs::create_dir_all(parent).await?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("hirsel.toml");
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp_path, path).await?;
        Ok::<_, std::io::Error>(())
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error)
            .with_context(|| format!("atomically persist host config at {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn seeds_comments_migrates_legacy_and_preserves_comments_on_write() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join(LEGACY_MODEL_SELECTION_FILE),
            br#"{"model_id":"gpt-5.6-sol","variant":"high"}"#,
        )
        .await
        .unwrap();
        let path = dir.path().join("hirsel.toml");
        let store = ConfigStore::load(path.clone(), dir.path(), Path::new("/docs/config.md"))
            .await
            .unwrap();
        assert_eq!(
            store.model_selection(),
            Some(("gpt-5.6-sol".to_string(), "high".to_string()))
        );
        let seeded = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(seeded.contains("safe to hand-edit"));
        assert!(seeded.contains("/docs/config.md"));

        store
            .set_subagent_model("codex", "gpt-5.5", false, "medium")
            .await
            .unwrap();
        let edited = tokio::fs::read_to_string(path).await.unwrap();
        assert!(edited.contains("# Codex CLI model gpt-5.5."));
        assert!(edited.contains("enabled = false"));
    }

    #[tokio::test]
    async fn reloads_direct_edits_and_survives_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.toml");
        let store = ConfigStore::load(path.clone(), dir.path(), Path::new("/docs/config.md"))
            .await
            .unwrap();
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("variant = \"medium\"", "variant = \"max\"");
        std::fs::write(&path, edited).unwrap();
        assert_eq!(store.model_selection().unwrap().1, "max");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        std::fs::write(&path, "not = [valid").unwrap();
        assert_eq!(store.model_selection().unwrap().1, "medium");
    }
}
