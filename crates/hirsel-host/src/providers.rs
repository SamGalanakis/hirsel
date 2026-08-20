//! The provider roster: which provider instances exist, what each one offers a
//! resident agent, and which one each agent is pointed at.
//!
//! Two instances are built in and synthesised here rather than stored: `codex`
//! (the local Codex OAuth login) and `claude` (the local Claude CLI login).
//! Neither carries a key in `hirsel.toml`; both carry a detection status
//! instead. `claude` is Sub-agents only — ADR-0015 records why a resident agent
//! may not ride the Claude subscription through this harness — so it is never
//! `agent_selectable`. Everything else in the roster is an OpenAI-compatible
//! endpoint the Owner added.
//!
//! API keys live in `hirsel.toml` and stop there. Nothing in this module puts a
//! key on the wire, in a log line, or in an error message: the roster masks
//! every key to presence plus a short tail before it is broadcast, and warnings
//! name the instance id and the reason only.

use std::path::PathBuf;

use anyhow::anyhow;
use hirsel_proto::{
    AgentSlot, DetectionStatus, MaskedSecret, ModelSelection, ProviderInstance, ProviderKind,
    ProviderRoster,
};

use crate::{
    boot_provider::BootProvider,
    config::ProviderMode,
    host_config::{ConfigStore, EnvBootstrap, StoredProvider},
    provider_detect,
};

/// The built-in Codex instance's id — reserved, never stored, never removed.
pub const CODEX_ID: &str = "codex";
/// The built-in Claude instance's id, under the same rules.
pub const CLAUDE_ID: &str = "claude";
/// The curated model the Codex instance seeds an agent with.
const CODEX_DEFAULT_MODEL: &str = "gpt-5.6-sol";
/// The longest instance id the roster accepts.
const MAX_ID_LEN: usize = 32;
/// Below this length a key has no tail that can be shown without effectively
/// showing the key.
const MIN_KEY_LEN_FOR_TAIL: usize = 8;

/// Why `claude` cannot be a resident agent's provider. Quoted to the Owner
/// verbatim when a command tries.
pub const CLAUDE_NOT_SELECTABLE: &str = "claude is available to Sub-agents only: Anthropic's subscription terms do not permit routing \
     the Claude login through a resident agent (ADR-0015)";

/// The provider roster, backed by the hot-reloaded config store.
///
/// Cloneable and cheap, exactly like `PromptConfig`: every read goes through
/// the store, so the clone the model layer holds and the clone `AppState` holds
/// never disagree.
#[derive(Clone)]
pub struct ProviderRosterState {
    config_store: ConfigStore,
    /// The provider the host actually booted on, as a roster id. The legacy
    /// `anthropic` boot mode is not a roster instance, so it reports `None`.
    booted_provider_id: Option<String>,
    /// Set when a stored provider choice could not be honoured at boot. Carries
    /// no key material — an instance id and a reason.
    boot_notice: Option<String>,
    /// The home directory detection probes. Explicit so tests can point it at a
    /// fixture instead of the real login files.
    home: Option<PathBuf>,
}

/// What a resident agent's selected provider offers its model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProviderChoice {
    pub id: String,
    pub kind: ProviderKind,
    /// The model this provider seeds an agent with when it is newly selected.
    pub default_model: String,
}

impl AgentProviderChoice {
    /// True when the provider takes any model id its endpoint offers, so the
    /// host validates shape only and the picker is a text field.
    pub fn is_free_text(&self) -> bool {
        matches!(self.kind, ProviderKind::OpenAiCompatible)
    }
}

impl ProviderRosterState {
    /// `booted` is the host's single boot resolution — see
    /// [`crate::boot_provider`]. The roster reports what actually booted, so
    /// the client's "differs from booted" copy stays truthful.
    pub fn new(config_store: ConfigStore, booted: &BootProvider, home: Option<PathBuf>) -> Self {
        Self {
            config_store,
            booted_provider_id: booted.id.clone(),
            boot_notice: booted.notice.clone(),
            home,
        }
    }

    /// The provider the resident main-agent session actually booted on. A
    /// main-agent provider change is stored immediately but only takes effect
    /// at the next host start, and this is what lets a client say so.
    pub fn booted_provider_id(&self) -> Option<&str> {
        self.booted_provider_id.as_deref()
    }

    /// The whole roster, with fresh detection for the two built-ins. Detection
    /// is two small file reads and is never cached, so a `redetect_provider`
    /// cannot return a stale answer.
    pub async fn snapshot(&self) -> ProviderRoster {
        let mut instances = vec![self.codex_instance().await, self.claude_instance().await];
        instances.extend(
            self.config_store
                .providers()
                .iter()
                .map(instance_from_stored),
        );
        ProviderRoster {
            instances,
            booted_provider_id: self.booted_provider_id.clone(),
            boot_notice: self.boot_notice.clone(),
        }
    }

    /// The provider a resident agent is pointed at, or `None` when the stored
    /// key is absent or no longer usable — in which case the caller falls back
    /// to the booted provider's own curated selection.
    pub fn agent_provider(&self, agent: AgentSlot) -> Option<AgentProviderChoice> {
        let section = section_for(agent);
        let id = self.config_store.agent_provider(section)?;
        match self.choice(&id) {
            Some(choice) if choice.kind == ProviderKind::Claude => {
                tracing::warn!(
                    section,
                    provider = id,
                    "configured provider is Sub-agents only; falling back to the booted provider"
                );
                None
            }
            Some(choice) => Some(choice),
            None => {
                tracing::warn!(
                    section,
                    provider = id,
                    "configured provider is not in the roster; falling back to the booted provider"
                );
                None
            }
        }
    }

    /// Resolve one instance id to what it offers an agent, built-ins included.
    pub fn choice(&self, id: &str) -> Option<AgentProviderChoice> {
        match id {
            CODEX_ID => Some(AgentProviderChoice {
                id: CODEX_ID.to_string(),
                kind: ProviderKind::Codex,
                default_model: CODEX_DEFAULT_MODEL.to_string(),
            }),
            CLAUDE_ID => Some(AgentProviderChoice {
                id: CLAUDE_ID.to_string(),
                kind: ProviderKind::Claude,
                default_model: String::new(),
            }),
            _ => self.stored(id).map(|stored| AgentProviderChoice {
                id: stored.id.clone(),
                kind: ProviderKind::OpenAiCompatible,
                default_model: stored.default_model.clone(),
            }),
        }
    }

    /// Point a resident agent at an instance. Returns the model + variant the
    /// agent is seeded with, which the caller persists in the same write.
    pub fn selection_for(&self, id: &str) -> anyhow::Result<AgentProviderChoice> {
        let choice = self
            .choice(id)
            .ok_or_else(|| anyhow!("unknown provider instance `{id}`"))?;
        if choice.kind == ProviderKind::Claude {
            return Err(anyhow!("{CLAUDE_NOT_SELECTABLE}"));
        }
        Ok(choice)
    }

    /// Point a resident agent at an instance and seed its model in the same
    /// write, so no reader ever sees a provider without a model to run on.
    pub async fn point_agent_at(
        &self,
        agent: AgentSlot,
        choice: &AgentProviderChoice,
        seed: &ModelSelection,
    ) -> anyhow::Result<()> {
        self.config_store
            .set_agent_provider_and_model(
                section_for(agent),
                &choice.id,
                model_key_for(agent),
                &seed.id,
                &seed.variant,
            )
            .await
    }

    /// Add an OpenAI-compatible instance. Reserved and malformed ids, and ids
    /// that are already taken, are rejected commands.
    pub async fn add(
        &self,
        id: &str,
        label: &str,
        base_url: &str,
        api_key: &str,
        default_model: &str,
    ) -> anyhow::Result<()> {
        validate_id(id)?;
        if self.stored(id).is_some() {
            return Err(anyhow!("provider instance `{id}` already exists"));
        }
        let provider = StoredProvider {
            id: id.to_string(),
            label: non_empty(label, "label")?.to_string(),
            base_url: non_empty(base_url, "base_url")?.to_string(),
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
            default_model: non_empty(default_model, "default_model")?.to_string(),
        };
        self.config_store.upsert_provider(&provider).await
    }

    /// Edit one instance. An omitted field is unchanged; an `api_key` of `""`
    /// clears the stored key, and any other value replaces it.
    pub async fn update(
        &self,
        id: &str,
        label: Option<&str>,
        base_url: Option<&str>,
        api_key: Option<&str>,
        default_model: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut provider = self
            .stored(id)
            .ok_or_else(|| built_in_or_unknown(id, "edited"))?;
        if let Some(label) = label {
            provider.label = non_empty(label, "label")?.to_string();
        }
        if let Some(base_url) = base_url {
            provider.base_url = non_empty(base_url, "base_url")?.to_string();
        }
        if let Some(api_key) = api_key {
            provider.api_key = (!api_key.is_empty()).then(|| api_key.to_string());
        }
        if let Some(default_model) = default_model {
            provider.default_model = non_empty(default_model, "default_model")?.to_string();
        }
        self.config_store.upsert_provider(&provider).await
    }

    /// Remove an instance the Owner added. The built-ins are configured, never
    /// removed.
    pub async fn remove(&self, id: &str) -> anyhow::Result<()> {
        if self.stored(id).is_none() {
            return Err(built_in_or_unknown(id, "removed"));
        }
        self.config_store.remove_provider(id).await
    }

    /// Re-probe one built-in's local credentials. Detection is uncached, so
    /// this is really "the id must be one that has credentials to probe".
    pub fn redetect(&self, id: &str) -> anyhow::Result<()> {
        match id {
            CODEX_ID | CLAUDE_ID => Ok(()),
            _ => Err(anyhow!(
                "provider instance `{id}` has no local credentials to detect"
            )),
        }
    }

    fn stored(&self, id: &str) -> Option<StoredProvider> {
        self.config_store
            .providers()
            .into_iter()
            .find(|provider| provider.id == id)
    }

    async fn codex_instance(&self) -> ProviderInstance {
        ProviderInstance {
            id: CODEX_ID.to_string(),
            kind: ProviderKind::Codex,
            label: "Codex (ChatGPT login)".to_string(),
            base_url: None,
            api_key: MaskedSecret::default(),
            default_model: CODEX_DEFAULT_MODEL.to_string(),
            detection: Some(self.detect(ProviderKind::Codex).await),
            agent_selectable: true,
            removable: false,
        }
    }

    async fn claude_instance(&self) -> ProviderInstance {
        ProviderInstance {
            id: CLAUDE_ID.to_string(),
            kind: ProviderKind::Claude,
            label: "Claude (CLI login)".to_string(),
            base_url: None,
            api_key: MaskedSecret::default(),
            default_model: String::new(),
            detection: Some(self.detect(ProviderKind::Claude).await),
            agent_selectable: false,
            removable: false,
        }
    }

    async fn detect(&self, kind: ProviderKind) -> DetectionStatus {
        let hint = match kind {
            ProviderKind::Claude => provider_detect::claude_path_hint(),
            _ => provider_detect::codex_path_hint(),
        };
        let Some(home) = self.home.as_deref() else {
            return provider_detect::home_unset(hint);
        };
        match kind {
            ProviderKind::Claude => provider_detect::detect_claude(home).await,
            _ => provider_detect::detect_codex(home).await,
        }
    }
}

/// The `hirsel.toml` section that holds one agent's provider and model.
fn section_for(agent: AgentSlot) -> &'static str {
    match agent {
        AgentSlot::Main => "model",
        AgentSlot::Fork => "fork",
    }
}

/// The key that section stores the model id under. The two agents disagree for
/// historical reasons: `[model] id` predates `[fork] model`.
fn model_key_for(agent: AgentSlot) -> &'static str {
    match agent {
        AgentSlot::Main => "id",
        AgentSlot::Fork => "model",
    }
}

/// The boot environment the config store seeds a fresh file from.
pub fn env_bootstrap(
    provider: ProviderMode,
    model: &str,
    openrouter_api_key: Option<&str>,
) -> EnvBootstrap {
    EnvBootstrap {
        provider: booted_provider_id(provider),
        model: (!model.trim().is_empty()).then(|| model.to_string()),
        openrouter_api_key: openrouter_api_key.map(str::to_string),
    }
}

/// The roster id a boot mode corresponds to. Anthropic is a legacy boot path,
/// not a roster instance.
pub(crate) fn booted_provider_id(provider: ProviderMode) -> Option<String> {
    match provider {
        ProviderMode::Codex => Some(CODEX_ID.to_string()),
        ProviderMode::OpenRouter => Some("openrouter".to_string()),
        ProviderMode::Anthropic => None,
    }
}

fn instance_from_stored(stored: &StoredProvider) -> ProviderInstance {
    ProviderInstance {
        id: stored.id.clone(),
        kind: ProviderKind::OpenAiCompatible,
        label: stored.label.clone(),
        base_url: Some(stored.base_url.clone()),
        api_key: mask(stored.api_key.as_deref()),
        default_model: stored.default_model.clone(),
        detection: None,
        agent_selectable: true,
        removable: true,
    }
}

/// All the wire is allowed to know about a stored key: that there is one, and
/// its last four characters — and not even those when the key is short enough
/// that a tail would give most of it away.
fn mask(api_key: Option<&str>) -> MaskedSecret {
    let Some(key) = api_key.filter(|key| !key.is_empty()) else {
        return MaskedSecret::default();
    };
    let characters: Vec<char> = key.chars().collect();
    let tail = if characters.len() >= MIN_KEY_LEN_FOR_TAIL {
        characters[characters.len() - 4..].iter().collect()
    } else {
        String::new()
    };
    MaskedSecret {
        present: true,
        tail,
    }
}

fn built_in_or_unknown(id: &str, verb: &str) -> anyhow::Error {
    if id == CODEX_ID || id == CLAUDE_ID {
        anyhow!("`{id}` is a built-in provider and cannot be {verb}")
    } else {
        anyhow!("unknown provider instance `{id}`")
    }
}

fn non_empty<'a>(value: &'a str, field: &str) -> anyhow::Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("provider {field} must not be empty"));
    }
    Ok(trimmed)
}

/// Instance ids are `[a-z0-9][a-z0-9_-]{0,31}`, and `codex`/`claude` are the
/// host's own. A rejected id says exactly what is allowed.
fn validate_id(id: &str) -> anyhow::Result<()> {
    if id == CODEX_ID || id == CLAUDE_ID {
        return Err(anyhow!(
            "`{id}` is a built-in provider id; choose another name"
        ));
    }
    let valid = (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !valid {
        return Err(anyhow!(
            "provider id `{id}` is not valid: use 1-{MAX_ID_LEN} characters of a-z, 0-9, `_` or \
             `-`, starting with a letter or digit"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_KEY: &str = "sk-or-v1-fake-key-tail";

    async fn roster(dir: &tempfile::TempDir, booted: ProviderMode) -> ProviderRosterState {
        let store = ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            std::path::Path::new("/docs/hirsel-config.md"),
            &EnvBootstrap::default(),
        )
        .await
        .unwrap();
        ProviderRosterState::new(
            store,
            &BootProvider::env_default(booted),
            Some(dir.path().to_path_buf()),
        )
    }

    fn instance<'a>(roster: &'a ProviderRoster, id: &str) -> &'a ProviderInstance {
        roster
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .unwrap_or_else(|| panic!("roster has no instance `{id}`"))
    }

    #[tokio::test]
    async fn an_added_instance_round_trips_while_the_wire_sees_only_a_tail() {
        let dir = tempfile::tempdir().unwrap();
        let state = roster(&dir, ProviderMode::Codex).await;
        state
            .add(
                "my-router",
                "My Router",
                "https://example.invalid/v1",
                FAKE_KEY,
                "some/model",
            )
            .await
            .unwrap();

        // The key survives a reload from disk...
        let reloaded = roster(&dir, ProviderMode::Codex).await;
        let stored = reloaded.stored("my-router").unwrap();
        assert_eq!(stored.api_key.as_deref(), Some(FAKE_KEY));
        assert_eq!(stored.base_url, "https://example.invalid/v1");
        assert_eq!(stored.default_model, "some/model");

        // ...and never reaches the snapshot the host broadcasts.
        let snapshot = reloaded.snapshot().await;
        let added = instance(&snapshot, "my-router");
        assert!(added.api_key.present);
        assert_eq!(added.api_key.tail, "tail");
        assert!(added.agent_selectable && added.removable);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains(FAKE_KEY), "{encoded}");
        assert!(!encoded.contains("fake-key"), "{encoded}");
    }

    #[test]
    fn a_short_key_masks_to_an_empty_tail() {
        assert_eq!(
            mask(Some("sk-1234")),
            MaskedSecret {
                present: true,
                tail: String::new(),
            }
        );
        assert_eq!(
            mask(Some("sk-12345")),
            MaskedSecret {
                present: true,
                tail: "2345".to_string(),
            }
        );
        assert_eq!(mask(None), MaskedSecret::default());
        assert_eq!(mask(Some("")), MaskedSecret::default());
    }

    #[tokio::test]
    async fn update_keeps_clears_or_replaces_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let state = roster(&dir, ProviderMode::Codex).await;
        state
            .add("router", "Router", "https://a.invalid/v1", FAKE_KEY, "m")
            .await
            .unwrap();

        state
            .update("router", Some("Renamed"), None, None, None)
            .await
            .unwrap();
        let stored = state.stored("router").unwrap();
        assert_eq!(stored.label, "Renamed");
        assert_eq!(stored.api_key.as_deref(), Some(FAKE_KEY));

        state
            .update("router", None, None, Some("sk-replacement"), None)
            .await
            .unwrap();
        assert_eq!(
            state.stored("router").unwrap().api_key.as_deref(),
            Some("sk-replacement")
        );

        state
            .update("router", None, None, Some(""), None)
            .await
            .unwrap();
        assert_eq!(state.stored("router").unwrap().api_key, None);
        assert!(
            !state
                .snapshot()
                .await
                .instances
                .last()
                .unwrap()
                .api_key
                .present
        );
    }

    #[tokio::test]
    async fn reserved_and_malformed_ids_are_rejected_and_built_ins_stay() {
        let dir = tempfile::tempdir().unwrap();
        let state = roster(&dir, ProviderMode::Codex).await;
        for id in [CODEX_ID, CLAUDE_ID] {
            let error = state
                .add(id, "Label", "https://a.invalid/v1", "k", "m")
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("built-in"), "{error}");
            assert!(
                state
                    .remove(id)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("built-in"),
                "built-ins must not be removable"
            );
        }
        for id in [
            "Router",
            "-router",
            "ro uter",
            "rou.ter",
            "",
            &"r".repeat(33),
        ] {
            assert!(
                state
                    .add(id, "Label", "https://a.invalid/v1", "k", "m")
                    .await
                    .is_err(),
                "accepted malformed id {id:?}"
            );
        }
        // A rejected add leaves the roster exactly as it was: the two built-ins
        // and nothing else, because no boot environment seeded an instance.
        let snapshot = state.snapshot().await;
        assert_eq!(
            snapshot
                .instances
                .iter()
                .map(|instance| instance.id.as_str())
                .collect::<Vec<_>>(),
            [CODEX_ID, CLAUDE_ID]
        );
        assert!(!instance(&snapshot, CLAUDE_ID).agent_selectable);
        assert!(instance(&snapshot, CODEX_ID).agent_selectable);

        // An empty required field is a rejected command too.
        assert!(
            state
                .add("router", "Label", "https://a.invalid/v1", "k", "  ")
                .await
                .is_err()
        );
        assert!(
            state
                .update("nope", Some("x"), None, None, None)
                .await
                .is_err()
        );
        assert!(state.redetect("nope").is_err());
        assert!(state.redetect(CLAUDE_ID).is_ok());
    }

    #[tokio::test]
    async fn claude_is_never_a_resident_agents_provider() {
        let dir = tempfile::tempdir().unwrap();
        let state = roster(&dir, ProviderMode::Codex).await;
        let error = state.selection_for(CLAUDE_ID).unwrap_err().to_string();
        assert!(error.contains("Sub-agents only"), "{error}");
        assert!(error.contains("ADR-0015"), "{error}");
        assert!(state.selection_for("missing").is_err());
        assert_eq!(
            state.selection_for(CODEX_ID).unwrap().default_model,
            "gpt-5.6-sol"
        );
    }

    #[tokio::test]
    async fn a_stale_or_claude_agent_provider_degrades_to_the_booted_one() {
        let dir = tempfile::tempdir().unwrap();
        let state = roster(&dir, ProviderMode::Codex).await;
        assert_eq!(state.agent_provider(AgentSlot::Main), None);

        for id in [CLAUDE_ID, "retired"] {
            state
                .config_store
                .set_agent_provider_and_model("model", id, "id", "whatever", "default")
                .await
                .unwrap();
            assert_eq!(state.agent_provider(AgentSlot::Main), None);
        }

        state
            .add("router", "Router", "https://a.invalid/v1", FAKE_KEY, "m")
            .await
            .unwrap();
        state
            .config_store
            .set_agent_provider_and_model("fork", "router", "model", "m", "default")
            .await
            .unwrap();
        let choice = state.agent_provider(AgentSlot::Fork).unwrap();
        assert_eq!(choice.id, "router");
        assert!(choice.is_free_text());
    }

    #[tokio::test]
    async fn the_booted_provider_is_reported_per_boot_mode() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            roster(&dir, ProviderMode::Codex)
                .await
                .snapshot()
                .await
                .booted_provider_id,
            Some(CODEX_ID.to_string())
        );
        assert_eq!(
            roster(&dir, ProviderMode::OpenRouter)
                .await
                .snapshot()
                .await
                .booted_provider_id,
            Some("openrouter".to_string())
        );
        // Anthropic is a legacy boot path, not a roster instance.
        assert_eq!(
            roster(&dir, ProviderMode::Anthropic)
                .await
                .snapshot()
                .await
                .booted_provider_id,
            None
        );
    }

    #[tokio::test]
    async fn detection_runs_against_the_configured_home() {
        let dir = tempfile::tempdir().unwrap();
        let state = roster(&dir, ProviderMode::Codex).await;
        let snapshot = state.snapshot().await;
        // The fixture home has neither login, so both report undetected with a
        // probed path the Owner can check.
        for id in [CODEX_ID, CLAUDE_ID] {
            let detection = instance(&snapshot, id).detection.clone().unwrap();
            assert!(!detection.detected);
            assert!(detection.path.starts_with(dir.path().to_str().unwrap()));
        }

        let homeless = ProviderRosterState::new(
            state.config_store.clone(),
            &BootProvider::env_default(ProviderMode::Codex),
            None,
        );
        let detection = instance(&homeless.snapshot().await, CODEX_ID)
            .detection
            .clone()
            .unwrap();
        assert!(!detection.detected);
        assert!(detection.detail.unwrap().contains("HOME"));
    }
}
