//! The provider half of `hirsel.toml`: the `[providers]` table, the boot-time
//! seeding of it, and the reads and writes the roster performs against it.
//!
//! Split out of the parent module so neither half crowds the file budget. It is
//! a file split and nothing more: every type and method here is re-exported
//! from `host_config`, so call sites still say `crate::host_config::…`.
//!
//! API keys live in this file's TOML and stop there. Nothing here logs a stored
//! value: a malformed entry is reported by instance id and reason only.

use anyhow::Result;
use toml_edit::{DocumentMut, Item, value};

use super::{ConfigStore, ensure_child_table, ensure_table};

/// The `kind` every stored provider instance carries. `codex` and `claude` are
/// synthesised by the host and never stored, so this is the only kind the file
/// can hold.
pub const OPENAI_COMPATIBLE_KIND: &str = "openai_compatible";

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

/// The OpenAI-compatible instance a fresh config file is seeded with, when the
/// boot environment gives it a reason to exist.
const SEED_PROVIDER_ID: &str = "openrouter";
const SEED_PROVIDER_LABEL: &str = "OpenRouter";
const SEED_PROVIDER_DEFAULT_MODEL: &str = "google/gemini-3.7-flash";

impl ConfigStore {
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
    pub async fn upsert_provider(&self, provider: &StoredProvider) -> Result<()> {
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
    pub async fn remove_provider(&self, id: &str) -> Result<()> {
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
    ) -> Result<()> {
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
}

/// Seed the `[providers]` table (and the agents' provider/model keys) from the
/// boot environment. Runs exactly once per file — see the caller's marker.
///
/// The `[providers]` table itself is always materialised, because its presence
/// is that once-only marker. The OpenRouter instance inside it is not: a host
/// booting on Codex with no `OPENROUTER_API_KEY` has no use for a keyless
/// endpoint it cannot call, so the roster stays honest and shows only what the
/// environment actually configured.
pub(super) fn seed_bootstrap(document: &mut DocumentMut, bootstrap: &EnvBootstrap) {
    ensure_table(document, "providers");
    let api_key = bootstrap
        .openrouter_api_key
        .as_deref()
        .filter(|key| !key.is_empty());
    let booting_on_openrouter = bootstrap.provider.as_deref() == Some(SEED_PROVIDER_ID);
    if api_key.is_some() || booting_on_openrouter {
        ensure_child_table(&mut document["providers"], SEED_PROVIDER_ID);
        let entry = &mut document["providers"][SEED_PROVIDER_ID];
        entry["kind"] = value(OPENAI_COMPATIBLE_KIND);
        entry["label"] = value(SEED_PROVIDER_LABEL);
        entry["base_url"] = value(lash_provider_openai::OPENROUTER_BASE_URL);
        if let Some(api_key) = api_key {
            entry["api_key"] = value(api_key);
        }
        entry["default_model"] = value(SEED_PROVIDER_DEFAULT_MODEL);
    }

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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
    async fn a_keyless_openrouter_instance_is_seeded_only_when_the_boot_mode_needs_it() {
        // Booting on Codex with no key: nothing to offer, so nothing is seeded.
        let codex = tempfile::tempdir().unwrap();
        let store = ConfigStore::load(
            codex.path().join("hirsel.toml"),
            codex.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap {
                provider: Some("codex".to_string()),
                model: None,
                openrouter_api_key: None,
            },
        )
        .await
        .unwrap();
        assert!(store.providers().is_empty());
        assert_eq!(store.agent_provider("model").as_deref(), Some("codex"));
        // The `[providers]` marker is still written, so a later key in the
        // environment cannot re-seed over a file the Owner has since edited.
        let contents = tokio::fs::read_to_string(codex.path().join("hirsel.toml"))
            .await
            .unwrap();
        assert!(contents.contains("[providers]"), "{contents}");

        // Booting on OpenRouter needs the instance to exist even before a key
        // is stored, so the Owner has a row to paste one into.
        let openrouter = tempfile::tempdir().unwrap();
        let store = ConfigStore::load(
            openrouter.path().join("hirsel.toml"),
            openrouter.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap {
                provider: Some("openrouter".to_string()),
                model: None,
                openrouter_api_key: None,
            },
        )
        .await
        .unwrap();
        let seeded = store.providers();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, "openrouter");
        assert_eq!(seeded[0].api_key, None);
        assert_eq!(
            seeded[0].base_url,
            lash_provider_openai::OPENROUTER_BASE_URL
        );
    }

    #[tokio::test]
    async fn no_boot_environment_seeds_no_instance_and_points_no_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            Path::new("/docs/config.md"),
            &EnvBootstrap::default(),
        )
        .await
        .unwrap();
        assert!(store.providers().is_empty());
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
        assert_eq!(ids, vec!["router".to_string()]);

        store.remove_provider("router").await.unwrap();
        assert!(!store.providers().iter().any(|p| p.id == "router"));
    }
}
