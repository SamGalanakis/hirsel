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

const LEGACY_MODEL_SELECTION_FILE: &str = "model-selection.json";

/// The `kind` every stored provider instance carries. `codex` and `claude` are
/// synthesised by the host and never stored, so this is the only kind the file
/// can hold.
pub const OPENAI_COMPATIBLE_KIND: &str = "openai_compatible";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentModelOverride {
    pub enabled: bool,
    pub enabled_variants: Vec<String>,
}

/// One OpenAI-compatible provider instance exactly as `hirsel.toml` holds it —
/// API key included. This shape never reaches the wire: the roster masks the
/// key before anything is broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_model: String,
}

/// The environment values that seed a config file the first time the host
/// materialises a `[providers]` table for it. Passed in explicitly so the store
/// stays testable: nothing here reads `std::env`.
#[derive(Debug, Clone, Default)]
pub struct EnvBootstrap {
    /// `HIRSEL_PROVIDER`, when it names a roster instance (`codex`,
    /// `openrouter`). The legacy `anthropic` boot mode is not one, so it seeds
    /// nothing.
    pub provider: Option<String>,
    /// `HIRSEL_MODEL`.
    pub model: Option<String>,
    /// `OPENROUTER_API_KEY`. Written only when non-empty, and only once — a
    /// later change to the variable never overwrites a stored key.
    pub openrouter_api_key: Option<String>,
}

/// The OpenAI-compatible instance a fresh config file is seeded with.
const SEED_PROVIDER_ID: &str = "openrouter";
const SEED_PROVIDER_LABEL: &str = "OpenRouter";
const SEED_PROVIDER_DEFAULT_MODEL: &str = "google/gemini-3.7-flash";

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

    /// Every OpenAI-compatible instance the `[providers]` table holds, in file
    /// order. A malformed entry is skipped with a warning naming the instance
    /// id and the reason — never any of its values.
    pub fn providers(&self) -> Vec<StoredProvider> {
        self.reload_if_changed();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(table) = inner.document.get("providers").and_then(Item::as_table) else {
            return Vec::new();
        };
        let mut providers = Vec::new();
        for (id, item) in table {
            let Some(section) = item.as_table() else {
                tracing::warn!(provider = id, "provider entry is not a table; ignoring it");
                continue;
            };
            let kind = section
                .get("kind")
                .and_then(Item::as_str)
                .unwrap_or_default();
            if kind != OPENAI_COMPATIBLE_KIND {
                tracing::warn!(
                    provider = id,
                    "provider entry has an unknown kind; ignoring it"
                );
                continue;
            }
            let Some(base_url) = section.get("base_url").and_then(Item::as_str) else {
                tracing::warn!(provider = id, "provider entry has no base_url; ignoring it");
                continue;
            };
            providers.push(StoredProvider {
                id: id.to_string(),
                label: section
                    .get("label")
                    .and_then(Item::as_str)
                    .unwrap_or(id)
                    .to_string(),
                base_url: base_url.to_string(),
                api_key: section
                    .get("api_key")
                    .and_then(Item::as_str)
                    .filter(|key| !key.is_empty())
                    .map(str::to_string),
                default_model: section
                    .get("default_model")
                    .and_then(Item::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        providers
    }

    /// The provider instance id a resident agent is pointed at. `section` is
    /// `model` for the main Agent and `fork` for the wake-triage fork.
    pub fn agent_provider(&self, section: &str) -> Option<String> {
        self.reload_if_changed();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let id = inner
            .document
            .get(section)?
            .as_table()?
            .get("provider")?
            .as_str()?;
        (!id.trim().is_empty()).then(|| id.to_string())
    }

    /// Create or replace one OpenAI-compatible instance.
    pub async fn upsert_provider(&self, provider: &StoredProvider) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            ensure_table(&mut document, "providers");
            ensure_child_table(&mut document["providers"], &provider.id);
            let entry = &mut document["providers"][&provider.id];
            entry["kind"] = value(OPENAI_COMPATIBLE_KIND);
            entry["label"] = value(&provider.label);
            entry["base_url"] = value(&provider.base_url);
            match &provider.api_key {
                Some(key) => entry["api_key"] = value(key),
                None => {
                    if let Some(table) = entry.as_table_mut() {
                        table.remove("api_key");
                    }
                }
            }
            entry["default_model"] = value(&provider.default_model);
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
    }

    /// Remove one instance. Removing an id that is not there is not an error —
    /// the caller has already decided what is removable.
    pub async fn remove_provider(&self, id: &str) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            if let Some(table) = document.get_mut("providers").and_then(Item::as_table_mut) {
                table.remove(id);
            }
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
    }

    /// Point a resident agent at a provider instance and seed its model in the
    /// same write, so a reader never sees a provider without its model.
    pub async fn set_agent_provider_and_model(
        &self,
        section: &str,
        provider_id: &str,
        model_key: &str,
        model_id: &str,
        variant: &str,
    ) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        self.reload_if_changed();
        let (document, contents) = {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut document = inner.document.clone();
            ensure_table(&mut document, section);
            document[section]["provider"] = value(provider_id);
            document[section][model_key] = value(model_id);
            document[section]["variant"] = value(variant);
            let contents = document.to_string();
            (document, contents)
        };
        self.persist_and_replace(document, contents).await
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

/// Seed the `[providers]` table (and the agents' provider/model keys) from the
/// boot environment. Runs exactly once per file — see the caller's marker.
fn seed_bootstrap(document: &mut DocumentMut, bootstrap: &EnvBootstrap) {
    ensure_table(document, "providers");
    ensure_child_table(&mut document["providers"], SEED_PROVIDER_ID);
    let entry = &mut document["providers"][SEED_PROVIDER_ID];
    entry["kind"] = value(OPENAI_COMPATIBLE_KIND);
    entry["label"] = value(SEED_PROVIDER_LABEL);
    entry["base_url"] = value(lash_provider_openai::OPENROUTER_BASE_URL);
    if let Some(api_key) = bootstrap
        .openrouter_api_key
        .as_deref()
        .filter(|key| !key.is_empty())
    {
        entry["api_key"] = value(api_key);
    }
    entry["default_model"] = value(SEED_PROVIDER_DEFAULT_MODEL);

    ensure_table(document, "model");
    if let Some(provider) = bootstrap.provider.as_deref()
        && document["model"].get("provider").is_none()
    {
        document["model"]["provider"] = value(provider);
    }
    if let Some(model) = bootstrap.model.as_deref()
        && document["model"].get("id").is_none()
    {
        document["model"]["id"] = value(model);
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
    async fn the_provider_roster_is_seeded_from_the_environment_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.toml");
        let first = EnvBootstrap {
            provider: Some("codex".to_string()),
            model: None,
            openrouter_api_key: Some("sk-first-key".to_string()),
        };
        let store = ConfigStore::load(
            path.clone(),
            dir.path(),
            Path::new("/docs/config.md"),
            &first,
        )
        .await
        .unwrap();
        let seeded = store.providers();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, "openrouter");
        assert_eq!(seeded[0].label, "OpenRouter");
        assert_eq!(
            seeded[0].base_url,
            lash_provider_openai::OPENROUTER_BASE_URL
        );
        assert_eq!(seeded[0].default_model, "google/gemini-3.7-flash");
        assert_eq!(seeded[0].api_key.as_deref(), Some("sk-first-key"));
        assert_eq!(store.agent_provider("model").as_deref(), Some("codex"));

        // A second load with a different key leaves the stored one alone: the
        // `[providers]` table is the once-only marker.
        let second = EnvBootstrap {
            provider: Some("openrouter".to_string()),
            model: None,
            openrouter_api_key: Some("sk-second-key".to_string()),
        };
        let reloaded = ConfigStore::load(
            path.clone(),
            dir.path(),
            Path::new("/docs/config.md"),
            &second,
        )
        .await
        .unwrap();
        assert_eq!(
            reloaded.providers()[0].api_key.as_deref(),
            Some("sk-first-key")
        );
        assert_eq!(reloaded.agent_provider("model").as_deref(), Some("codex"));
    }

    #[tokio::test]
    async fn seeding_without_a_key_still_seeds_the_instance() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap::default(),
        )
        .await
        .unwrap();
        let seeded = store.providers();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].api_key, None);
        assert_eq!(
            seeded[0].base_url,
            lash_provider_openai::OPENROUTER_BASE_URL
        );
        assert_eq!(seeded[0].default_model, "google/gemini-3.7-flash");
        // No HIRSEL_PROVIDER to seed from means no agent is pointed anywhere:
        // both stay on whatever the host booted with.
        assert_eq!(store.agent_provider("model"), None);
        assert_eq!(store.agent_provider("fork"), None);
    }

    #[tokio::test]
    async fn a_file_with_an_empty_providers_table_is_never_reseeded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.toml");
        std::fs::write(
            &path,
            "[providers]\n[model]\nid = \"m\"\nvariant = \"default\"\n",
        )
        .unwrap();
        let store = ConfigStore::load(
            path,
            dir.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap {
                provider: Some("codex".to_string()),
                model: Some("gpt-5.6-sol".to_string()),
                openrouter_api_key: Some("sk-key".to_string()),
            },
        )
        .await
        .unwrap();
        assert!(store.providers().is_empty());
        assert_eq!(store.agent_provider("model"), None);
    }

    #[tokio::test]
    async fn provider_entries_round_trip_and_malformed_ones_are_ignored() {
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
        store
            .upsert_provider(&StoredProvider {
                id: "router".to_string(),
                label: "Router".to_string(),
                base_url: "https://example.invalid/v1".to_string(),
                api_key: Some("sk-router-key".to_string()),
                default_model: "some/model".to_string(),
            })
            .await
            .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[providers.router]"), "{contents}");
        // A hand-written entry missing its base_url is ignored, not fatal.
        std::fs::write(
            &path,
            format!("{contents}\n[providers.broken]\nkind = \"openai_compatible\"\n"),
        )
        .unwrap();
        let ids: Vec<String> = store.providers().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["openrouter".to_string(), "router".to_string()]);

        store.remove_provider("router").await.unwrap();
        assert!(!store.providers().iter().any(|p| p.id == "router"));
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
