use std::{
    collections::BTreeMap,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, anyhow};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use toml_edit::{Array, DocumentMut, Item, Table, value};
use uuid::Uuid;

mod provider_store;

use provider_store::seed_bootstrap;
pub use provider_store::{EnvBootstrap, OPENAI_COMPATIBLE_KIND, StoredProvider};

const LEGACY_MODEL_SELECTION_FILE: &str = "model-selection.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentModelOverride {
    pub enabled: bool,
    pub enabled_variants: Vec<String>,
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
    source: String,
}

#[derive(Debug, Deserialize)]
struct LegacyModelSelection {
    model_id: String,
    variant: String,
}

impl ConfigStore {
    pub async fn load(
        path: PathBuf,
        data_dir: &Path,
        docs_path: &Path,
        bootstrap: &EnvBootstrap,
    ) -> anyhow::Result<Self> {
        let defaults = default_document(docs_path)?;
        // A file the host could not parse is left exactly as the Owner wrote
        // it: the in-memory defaults keep the host alive, but nothing is
        // seeded over a file that is one typo away from being correct.
        let mut writable = true;
        let (mut document, mut source) = match tokio::fs::read_to_string(&path).await {
            Ok(contents) => match contents.parse::<DocumentMut>() {
                Ok(document) => (document, contents),
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "host config is malformed; using built-in defaults"
                    );
                    writable = false;
                    (defaults.clone(), contents)
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let mut document = defaults.clone();
                migrate_legacy_model(data_dir, &mut document).await;
                let source = document.to_string();
                persist_atomic(&path, source.as_bytes()).await?;
                (document, source)
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read host config at {}", path.display()));
            }
        };
        // The presence of a `[providers]` table is the once-only marker: a file
        // that already has one — even an empty one — is never re-seeded, so a
        // later `OPENROUTER_API_KEY` change can never overwrite a stored key.
        if writable && !document.get("providers").is_some_and(Item::is_table) {
            seed_bootstrap(&mut document, bootstrap);
            source = document.to_string();
            persist_atomic(&path, source.as_bytes()).await?;
        }
        Ok(Self {
            path: Arc::new(path),
            defaults: Arc::new(defaults),
            inner: Arc::new(Mutex::new(StoreInner { document, source })),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reload_if_changed(&self) {
        let source = match std::fs::read_to_string(&*self.path) {
            Ok(contents) => contents,
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "failed to reload host config; keeping the last valid in-memory configuration"
                );
                return;
            }
        };
        if self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .source
            == source
        {
            return;
        }
        let document = parse_or_defaults(&self.path, &source, &self.defaults);
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.document = document;
        inner.source = source;
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
                let Some(enabled_variants) = section
                    .get("enabled_variants")
                    .and_then(Item::as_array)
                    .and_then(|variants| {
                        variants
                            .iter()
                            .map(|variant| variant.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                    })
                else {
                    tracing::warn!(
                        provider,
                        model_id,
                        "Sub-agent enabled variants are missing or invalid; using defaults"
                    );
                    continue;
                };
                parsed_models.insert(
                    model_id.to_string(),
                    SubagentModelOverride {
                        enabled,
                        enabled_variants,
                    },
                );
            }
            parsed.insert(provider.to_string(), parsed_models);
        }
        parsed
    }

    /// The Owner's Agent system-prompt override, or `None` when the key is
    /// absent, empty, or not a string (the bundled prompt then stands). The
    /// value is Owner data: it is never logged, only length-reported.
    pub fn agent_prompt_override(&self) -> Option<String> {
        self.prompt_override("agent")
    }

    /// The Owner's fork-agent prompt override, under the same rules.
    pub fn fork_prompt_override(&self) -> Option<String> {
        self.prompt_override("fork")
    }

    fn prompt_override(&self, section: &str) -> Option<String> {
        self.reload_if_changed();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let text = inner
            .document
            .get(section)?
            .as_table()?
            .get("prompt")?
            .as_str()?;
        (!text.trim().is_empty()).then(|| text.to_string())
    }

    /// The fork agent's persisted model selection, or `None` when the `[fork]`
    /// section carries no usable pair.
    pub fn fork_model_selection(&self) -> Option<(String, String)> {
        self.reload_if_changed();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let section = inner.document.get("fork")?.as_table()?;
        let id = section.get("model")?.as_str()?.to_string();
        let variant = section.get("variant")?.as_str()?.to_string();
        Some((id, variant))
    }

    /// Store an Agent prompt override, or remove it when `text` is `None`.
    pub async fn set_agent_prompt(&self, text: Option<&str>) -> anyhow::Result<()> {
        self.set_prompt("agent", text).await
    }

    /// Store a fork prompt override, or remove it when `text` is `None`.
    pub async fn set_fork_prompt(&self, text: Option<&str>) -> anyhow::Result<()> {
        self.set_prompt("fork", text).await
    }

    async fn set_prompt(&self, section: &str, text: Option<&str>) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            match text {
                // A prompt is many lines of prose; a multi-line literal string
                // keeps the file hand-editable instead of one escaped ribbon.
                Some(text) => {
                    ensure_table(&mut document, section);
                    document[section]["prompt"] = multiline_item(text);
                }
                None => {
                    if let Some(table) = document.get_mut(section).and_then(Item::as_table_mut) {
                        table.remove("prompt");
                    }
                }
            }
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
    }

    pub async fn set_fork_model(&self, model_id: &str, variant: &str) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            ensure_table(&mut document, "fork");
            document["fork"]["model"] = value(model_id);
            document["fork"]["variant"] = value(variant);
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
    }

    pub async fn set_model_selection(&self, model_id: &str, variant: &str) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            ensure_table(&mut document, "model");
            document["model"]["id"] = value(model_id);
            document["model"]["variant"] = value(variant);
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
    }

    pub async fn set_subagent_model(
        &self,
        provider: &str,
        model_id: &str,
        enabled: bool,
        enabled_variants: &[String],
    ) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            ensure_table(&mut document, "subagent_models");
            ensure_child_table(&mut document["subagent_models"], provider);
            ensure_child_table(&mut document["subagent_models"][provider], model_id);
            document["subagent_models"][provider][model_id]["enabled"] = value(enabled);
            let mut variants = Array::new();
            for variant in enabled_variants {
                variants.push(variant.as_str());
            }
            document["subagent_models"][provider][model_id]["enabled_variants"] =
                Item::Value(variants.into());
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
    }

    async fn persist_and_replace(
        &self,
        document: DocumentMut,
        contents: String,
    ) -> anyhow::Result<()> {
        persist_atomic(&self.path, contents.as_bytes()).await?;
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner.document = document;
        inner.source = contents;
        Ok(())
    }
}

/// A prompt rendered as a TOML multi-line literal so `hirsel.toml` stays
/// hand-editable — the default string repr would collapse a page of prose into
/// one escaped ribbon. Built by parsing the snippet and keeping it only when it
/// reads back byte-identical; anything the multi-line form cannot carry
/// verbatim (embedded `"""`, backslashes) falls back to the escaped repr.
fn multiline_item(text: &str) -> Item {
    let snippet = format!("prompt = \"\"\"\n{text}\"\"\"\n");
    let round_trips = snippet
        .parse::<DocumentMut>()
        .ok()
        .and_then(|document| document.get("prompt").cloned())
        .filter(|item| item.as_str() == Some(text));
    round_trips.unwrap_or_else(|| value(text))
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

# The provider instances the resident agents can run on. `codex` and `claude`
# are built in — they use the local CLI logins, store no key here, and cannot be
# removed (`claude` is Sub-agents only, never a resident agent's provider). Add
# any number of OpenAI-compatible endpoints alongside them:
#
# [providers.my-endpoint]
# kind = "openai_compatible"
# label = "My endpoint"
# base_url = "https://example.invalid/v1"
# api_key = "..."                  # stays in this file; never sent to a client
# default_model = "some/model"
#
# Instance ids are lowercase [a-z0-9][a-z0-9_-]*; `codex` and `claude` are
# reserved. API keys live here and nowhere else — Settings only ever shows
# whether one is set and its last four characters.

# Main Agent provider and model. `provider` names an instance above; leave it
# out to stay on whatever HIRSEL_PROVIDER booted. An OpenAI-compatible provider
# takes any model id its endpoint offers; codex offers gpt-5.6-sol (variants:
# low, medium, high, xhigh, max). A model change applies from the Agent's next
# turn; a provider change applies at the next host start.
[model]
id = "google/gemini-3.7-flash"
variant = "default"

# The Agent's system prompt. With no `prompt` key (or an empty one) the Agent
# runs on the bundled `prompts/agent.md`. Set one here or in Settings > Prompt
# to override it; the change applies from the Agent's next turn. The host
# appends its own configuration notes to whatever body is in force.
[agent]
# prompt = """
# You are ...
# """

# The wake-triage fork: which provider and model read an incoming wake, and the
# prompt they read it with. `provider` works exactly as it does under [model];
# with none set the fork stays on the booted provider's cheap lane. `prompt`
# defaults to the bundled `prompts/fork.md`. Stored but not yet consumed — the
# fork runtime lands later.
[fork]
# provider = "codex"
# model = "gpt-5.6-luna"
# variant = "medium"
# prompt = """
# You are a wake-triage fork ...
# """

# Sub-agent delegation lanes. The catalog is exactly these three rows, one
# effort each — there is no per-task effort tuning. Set `enabled = false` to
# take a lane out of service; entries for anything else are ignored.

# Workhorse lane: judgment-heavy implementation and review-expensive
# verification.
[subagent_models.codex."gpt-5.6-sol"]
enabled = true
enabled_variants = ["high"]

# Economy lane: mechanically verifiable work (checks, audits, bulk analysis,
# tightly specified edits, recon).
[subagent_models.codex."gpt-5.6-luna"]
enabled = true
enabled_variants = ["max"]

# Workhorse lane: taste-critical work (UI, API shape, copy) and fresh review.
[subagent_models.claude.claude-opus-5]
enabled = true
enabled_variants = ["high"]
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
    use std::fs::FileTimes;

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
        let store = ConfigStore::load(
            path.clone(),
            dir.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap::default(),
        )
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
            .set_subagent_model("codex", "gpt-5.6-luna", false, &["max".to_string()])
            .await
            .unwrap();
        let edited = tokio::fs::read_to_string(path).await.unwrap();
        assert!(edited.contains("# Economy lane: mechanically verifiable work"));
        assert!(edited.contains("enabled = false"));
        assert!(edited.contains("enabled_variants = [\"max\"]"));
    }

    #[tokio::test]
    async fn reloads_a_direct_edit_with_an_unchanged_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.toml");
        let store = ConfigStore::load(
            path.clone(),
            dir.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap::default(),
        )
        .await
        .unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("variant = \"default\"", "variant = \"max\"");
        std::fs::write(&path, edited).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(store.model_selection().unwrap().1, "max");
    }

    #[tokio::test]
    async fn malformed_edits_fall_back_and_a_repair_reloads_without_sleeping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.toml");
        let store = ConfigStore::load(
            path.clone(),
            dir.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap::default(),
        )
        .await
        .unwrap();
        std::fs::write(&path, "not = [valid").unwrap();
        assert_eq!(store.model_selection().unwrap().1, "default");

        let repaired = default_document(Path::new("/docs/config.md"))
            .unwrap()
            .to_string();
        std::fs::write(
            &path,
            repaired.replace("variant = \"default\"", "variant = \"high\""),
        )
        .unwrap();
        assert_eq!(store.model_selection().unwrap().1, "high");
    }
}
