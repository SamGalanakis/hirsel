//! What the host actually boots the main agent on.
//!
//! Two things used to decide this independently and could disagree: the
//! `ProviderHandle` was built from the environment alone, while the roster
//! reported a `booted_provider_id` derived from the same environment — so a
//! stored `[model].provider` was quietly discarded at every restart even
//! though Settings promised it would be honoured at the next one.
//!
//! This module is the single resolution. It reconciles the stored roster choice
//! with the environment default once, at startup, and hands the same answer to
//! the provider builder and to the roster. When a stored choice cannot boot,
//! the host says so instead of pretending: one warning, and a notice the
//! Providers tab shows standing.
//!
//! Nothing here logs, formats, or returns key material. A [`BootPlan`] carries
//! the key the handle needs and is never rendered; every notice and warning
//! names an instance id and a reason only.

use std::{fmt, path::Path};

use crate::{
    config::ProviderMode,
    host_config::ConfigStore,
    provider_detect,
    providers::{CLAUDE_ID, CLAUDE_NOT_SELECTABLE, CODEX_ID, booted_provider_id},
};

/// What the host booted the main agent on, after reconciling the stored roster
/// choice with the environment default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootProvider {
    /// The roster id that won, or `None` in legacy anthropic mode.
    pub id: Option<String>,
    pub plan: BootPlan,
    /// Set only when a stored choice was asked for and could not be honoured.
    pub notice: Option<String>,
}

/// How the main agent's provider handle is built.
#[derive(Clone, PartialEq, Eq)]
pub enum BootPlan {
    /// Legacy env mode, unchanged behaviour: the keys come from
    /// `ANTHROPIC_API_KEY` / `OPENROUTER_API_KEY` as they always have.
    Env(ProviderMode),
    /// The built-in Codex login at `~/.codex/auth.json`.
    Codex,
    /// A stored OpenAI-compatible instance: its own base URL and its own key,
    /// never the environment's.
    OpenAiCompatible {
        id: String,
        base_url: String,
        api_key: String,
    },
}

impl BootPlan {
    pub fn label(&self) -> &str {
        match self {
            Self::Env(ProviderMode::Anthropic) => "anthropic",
            Self::Env(ProviderMode::Codex) | Self::Codex => "codex",
            Self::Env(ProviderMode::OpenRouter) => "openrouter",
            Self::OpenAiCompatible { id, .. } => id,
        }
    }
}

impl fmt::Debug for BootPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(mode) => formatter.debug_tuple("Env").field(mode).finish(),
            Self::Codex => formatter.write_str("Codex"),
            Self::OpenAiCompatible { id, base_url, .. } => formatter
                .debug_struct("OpenAiCompatible")
                .field("id", id)
                .field("base_url", base_url)
                .field("api_key", &"<redacted>")
                .finish(),
        }
    }
}

impl BootProvider {
    /// The environment default, exactly as the host booted before the roster
    /// existed.
    pub fn env_default(mode: ProviderMode) -> Self {
        Self {
            id: booted_provider_id(mode),
            plan: BootPlan::Env(mode),
            notice: None,
        }
    }

    fn fell_back(mode: ProviderMode, id: &str, reason: &str) -> Self {
        let notice = format!(
            "configured provider \"{id}\" is unavailable at boot: {reason} — running on {}",
            fallback_label(mode)
        );
        tracing::warn!(
            provider = id,
            reason,
            "configured main-agent provider cannot boot; falling back to the environment default"
        );
        Self {
            notice: Some(notice),
            ..Self::env_default(mode)
        }
    }
}

/// How a fallback names the provider it landed on, in the Owner's vocabulary.
fn fallback_label(mode: ProviderMode) -> &'static str {
    match mode {
        ProviderMode::Anthropic => "Anthropic",
        ProviderMode::Codex => "Codex",
        ProviderMode::OpenRouter => "OpenRouter",
    }
}

/// Resolve the main agent's provider: the stored `[model].provider` when it is
/// set and can actually boot, the environment default otherwise.
///
/// Usability is decided by what the host can see, never by a network call: an
/// OpenAI-compatible instance needs a stored key, and `codex` needs a readable
/// login. Both checks are the same ones the Providers tab reports.
pub async fn resolve(store: &ConfigStore, mode: ProviderMode, home: Option<&Path>) -> BootProvider {
    let Some(id) = store.agent_provider("model") else {
        return BootProvider::env_default(mode);
    };
    // The legacy anthropic boot path has no roster and no model-selection
    // machinery behind it, so it cannot honour a roster instance — that is why
    // `set_agent_provider` refuses in this mode. A hand-edited choice gets the
    // same honest notice as any other one the host cannot boot.
    if mode == ProviderMode::Anthropic {
        return BootProvider::fell_back(
            mode,
            &id,
            "HIRSEL_PROVIDER=anthropic is a legacy boot mode with no provider roster",
        );
    }
    match id.as_str() {
        CODEX_ID => {
            // Booting on Codex is already what the environment does, so there
            // is nothing to reconcile and nothing to re-probe.
            if mode == ProviderMode::Codex {
                return BootProvider::env_default(mode);
            }
            let Some(home) = home else {
                return BootProvider::fell_back(
                    mode,
                    &id,
                    "HOME is not set, so the Codex login cannot be located",
                );
            };
            if provider_detect::detect_codex(home).await.detected {
                BootProvider {
                    id: Some(CODEX_ID.to_string()),
                    plan: BootPlan::Codex,
                    notice: None,
                }
            } else {
                BootProvider::fell_back(mode, &id, "no Codex login was detected")
            }
        }
        CLAUDE_ID => BootProvider::fell_back(mode, &id, CLAUDE_NOT_SELECTABLE),
        _ => {
            let Some(stored) = store
                .providers()
                .into_iter()
                .find(|provider| provider.id == id)
            else {
                return BootProvider::fell_back(mode, &id, "it is not in the provider roster");
            };
            let Some(api_key) = stored.api_key.filter(|key| !key.is_empty()) else {
                return BootProvider::fell_back(mode, &id, "no API key is stored");
            };
            BootProvider {
                id: Some(stored.id.clone()),
                plan: BootPlan::OpenAiCompatible {
                    id: stored.id,
                    base_url: stored.base_url,
                    api_key,
                },
                notice: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_config::{EnvBootstrap, StoredProvider};

    const FAKE_KEY: &str = "sk-boot-fake-key-tail";

    #[test]
    fn boot_plan_debug_redacts_openai_compatible_api_key() {
        let plan = BootPlan::OpenAiCompatible {
            id: "acme".to_string(),
            base_url: "https://acme.invalid/v1".to_string(),
            api_key: "test-key-do-not-log".to_string(),
        };

        let debug = format!("{plan:?}");

        assert!(!debug.contains("test-key-do-not-log"), "{debug}");
        assert!(debug.contains("acme"), "{debug}");
    }

    async fn store(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            Path::new("/docs/hirsel-config.md"),
            &EnvBootstrap::default(),
        )
        .await
        .unwrap()
    }

    async fn with_router(dir: &tempfile::TempDir, api_key: Option<&str>) -> ConfigStore {
        let store = store(dir).await;
        store
            .upsert_provider(&StoredProvider {
                id: "acme".to_string(),
                label: "Acme".to_string(),
                base_url: "https://acme.invalid/v1".to_string(),
                api_key: api_key.map(str::to_string),
                default_model: "acme/model".to_string(),
            })
            .await
            .unwrap();
        store
            .set_agent_provider_and_model("model", "acme", "id", "acme/model", "default")
            .await
            .unwrap();
        store
    }

    #[tokio::test]
    async fn a_stored_instance_beats_the_environment_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = with_router(&dir, Some(FAKE_KEY)).await;

        let boot = resolve(&store, ProviderMode::Codex, Some(dir.path())).await;
        assert_eq!(boot.id.as_deref(), Some("acme"));
        assert_eq!(boot.notice, None);
        assert_eq!(
            boot.plan,
            BootPlan::OpenAiCompatible {
                id: "acme".to_string(),
                base_url: "https://acme.invalid/v1".to_string(),
                api_key: FAKE_KEY.to_string(),
            }
        );
    }

    #[tokio::test]
    async fn a_keyless_instance_falls_back_with_a_notice_that_carries_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = with_router(&dir, None).await;

        let boot = resolve(&store, ProviderMode::Codex, Some(dir.path())).await;
        assert_eq!(boot.id.as_deref(), Some("codex"));
        assert_eq!(boot.plan, BootPlan::Env(ProviderMode::Codex));
        let notice = boot.notice.unwrap();
        assert_eq!(
            notice,
            "configured provider \"acme\" is unavailable at boot: no API key is stored — running on Codex"
        );
        assert!(!notice.contains(FAKE_KEY), "{notice}");
    }

    #[tokio::test]
    async fn an_unknown_id_and_a_hand_edited_claude_both_fall_back_with_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        for (id, reason) in [
            ("retired", "it is not in the provider roster"),
            (CLAUDE_ID, "Sub-agents only"),
        ] {
            store
                .set_agent_provider_and_model("model", id, "id", "whatever", "default")
                .await
                .unwrap();
            let boot = resolve(&store, ProviderMode::OpenRouter, Some(dir.path())).await;
            assert_eq!(boot.id.as_deref(), Some("openrouter"));
            assert_eq!(boot.plan, BootPlan::Env(ProviderMode::OpenRouter));
            let notice = boot.notice.unwrap();
            assert!(notice.contains(id), "{notice}");
            assert!(notice.contains(reason), "{notice}");
            assert!(notice.ends_with("running on OpenRouter"), "{notice}");
        }
    }

    #[tokio::test]
    async fn a_stored_codex_choice_needs_a_readable_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        store
            .set_agent_provider_and_model("model", CODEX_ID, "id", "gpt-5.6-sol", "high")
            .await
            .unwrap();

        // The fixture home has no login, so an OpenRouter-booted host cannot
        // switch to it.
        let boot = resolve(&store, ProviderMode::OpenRouter, Some(dir.path())).await;
        assert_eq!(boot.plan, BootPlan::Env(ProviderMode::OpenRouter));
        assert!(
            boot.notice
                .as_deref()
                .is_some_and(|notice| notice.contains("no Codex login was detected")),
            "{:?}",
            boot.notice
        );

        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/auth.json"),
            r#"{"tokens":{"access_token":"a","refresh_token":"r"}}"#,
        )
        .unwrap();
        let boot = resolve(&store, ProviderMode::OpenRouter, Some(dir.path())).await;
        assert_eq!(boot.id.as_deref(), Some(CODEX_ID));
        assert_eq!(boot.plan, BootPlan::Codex);
        assert_eq!(boot.notice, None);

        // Already booting on Codex is a no-op: no re-probe, no notice.
        let boot = resolve(&store, ProviderMode::Codex, None).await;
        assert_eq!(boot.id.as_deref(), Some(CODEX_ID));
        assert_eq!(boot.plan, BootPlan::Env(ProviderMode::Codex));
        assert_eq!(boot.notice, None);
    }

    #[tokio::test]
    async fn legacy_anthropic_mode_stays_on_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let boot = resolve(&store, ProviderMode::Anthropic, Some(dir.path())).await;
        assert_eq!(boot.id, None);
        assert_eq!(boot.plan, BootPlan::Env(ProviderMode::Anthropic));
        assert_eq!(boot.notice, None);

        // A hand-edited choice in this mode is refused honestly, not silently.
        let store = with_router(&dir, Some(FAKE_KEY)).await;
        let boot = resolve(&store, ProviderMode::Anthropic, Some(dir.path())).await;
        assert_eq!(boot.id, None);
        assert_eq!(boot.plan, BootPlan::Env(ProviderMode::Anthropic));
        assert!(
            boot.notice
                .as_deref()
                .is_some_and(|notice| notice.contains("legacy boot mode")),
            "{:?}",
            boot.notice
        );
    }
}
