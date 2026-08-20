//! Owner-editable prompts: the Agent's system prompt and the wake-triage
//! fork's model + prompt.
//!
//! Both live in `hirsel.toml` beside the model selection and follow exactly the
//! same rules: the store is hot-reloaded, so a hand edit and a Settings edit are
//! the same event; an absent or empty override means the bundled default; an
//! unusable value degrades to the default with a warning instead of bricking
//! the boot.
//!
//! Prompt bodies are Owner data. Nothing here logs one — warnings name the key
//! and the reason, never the text.

use hirsel_proto::{ForkAgentConfig, ModelSelection, PromptDoc, PromptSnapshot};

use crate::{
    config::ProviderMode,
    host_config::ConfigStore,
    model_selection::{available_fork_models, default_fork_selection, validate_fork},
};

/// The bundled Agent prompt. The Owner's override replaces this body; the host
/// configuration section is appended to whichever body is in force.
pub const AGENT_PROMPT: &str = include_str!("../../../prompts/agent.md");
/// The bundled wake-triage fork prompt.
pub const FORK_PROMPT: &str = include_str!("../../../prompts/fork.md");

/// The Owner-editable prompt surface, backed by the hot-reloaded config store.
///
/// Cloneable and cheap: every read goes through the store, so a clone held by
/// the agent runtime and a clone held by `AppState` never disagree.
#[derive(Clone)]
pub struct PromptConfig {
    provider: ProviderMode,
    config_store: ConfigStore,
    /// Host-generated text appended after the editable body — the runtime
    /// configuration paths and the enabled plugins' skills. Not editable, and
    /// not part of what Settings shows as the prompt.
    host_section: std::sync::Arc<str>,
}

impl PromptConfig {
    pub fn new(provider: ProviderMode, config_store: ConfigStore, host_section: String) -> Self {
        Self {
            provider,
            config_store,
            host_section: host_section.into(),
        }
    }

    /// The Agent's effective system prompt body, without the host section.
    pub fn agent_prompt(&self) -> PromptDoc {
        doc(self.config_store.agent_prompt_override(), AGENT_PROMPT)
    }

    /// What the Agent's Lash session is actually prompted with: the effective
    /// body plus the host-generated configuration section.
    pub fn agent_guidance(&self) -> String {
        format!("{}{}", self.agent_prompt().text, self.host_section)
    }

    /// The fork's model selection, or `None` in a provider mode with no
    /// runtime-selectable registry.
    pub fn fork_model(&self) -> Option<ModelSelection> {
        let fallback = default_fork_selection(self.provider)?;
        let Some((id, variant)) = self.config_store.fork_model_selection() else {
            return Some(fallback);
        };
        match validate_fork(self.provider, &id, &variant) {
            Ok(selection) => Some(selection),
            Err(error) => {
                tracing::warn!(
                    path = %self.config_store.path().display(),
                    %error,
                    "persisted fork model is not available for this provider; falling back to the default fork model"
                );
                Some(fallback)
            }
        }
    }

    pub fn fork(&self) -> Option<ForkAgentConfig> {
        Some(ForkAgentConfig {
            current: self.fork_model()?,
            available: available_fork_models(self.provider),
            prompt: doc(self.config_store.fork_prompt_override(), FORK_PROMPT),
        })
    }

    pub fn snapshot(&self) -> PromptSnapshot {
        PromptSnapshot {
            agent: self.agent_prompt(),
            fork: self.fork(),
        }
    }

    /// Persist an Agent prompt body. Empty or whitespace-only text clears the
    /// override, so "Reset to default" is one op and no client ever has to send
    /// the bundled text back.
    pub async fn set_agent_prompt(&self, text: &str) -> anyhow::Result<()> {
        self.config_store
            .set_agent_prompt(override_text(text))
            .await
    }

    pub async fn set_fork_prompt(&self, text: &str) -> anyhow::Result<()> {
        self.config_store.set_fork_prompt(override_text(text)).await
    }

    /// Validate a fork model against the booted provider's registry and store
    /// it. An id the provider does not offer is a rejected command, not a
    /// silent fallback: the Owner picked it deliberately.
    pub async fn set_fork_model(
        &self,
        model_id: &str,
        variant: &str,
    ) -> anyhow::Result<ModelSelection> {
        if default_fork_selection(self.provider).is_none() {
            anyhow::bail!(
                "fork model selection requires HIRSEL_PROVIDER=codex or HIRSEL_PROVIDER=openrouter"
            );
        }
        let selection = validate_fork(self.provider, model_id, variant)?;
        self.config_store
            .set_fork_model(&selection.id, &selection.variant)
            .await?;
        Ok(selection)
    }
}

/// `None` when the text carries no override — the caller stores the absence.
fn override_text(text: &str) -> Option<&str> {
    (!text.trim().is_empty()).then_some(text)
}

fn doc(stored: Option<String>, bundled: &str) -> PromptDoc {
    match stored {
        Some(text) => PromptDoc {
            text,
            is_default: false,
        },
        None => PromptDoc {
            text: bundled.to_string(),
            is_default: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn config(dir: &tempfile::TempDir, provider: ProviderMode) -> PromptConfig {
        let store = ConfigStore::load(
            dir.path().join("hirsel.toml"),
            dir.path(),
            std::path::Path::new("/docs/hirsel-config.md"),
        )
        .await
        .unwrap();
        PromptConfig::new(provider, store, "\n\n## Host configuration\n".to_string())
    }

    #[tokio::test]
    async fn an_override_round_trips_and_an_empty_body_restores_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = config(&dir, ProviderMode::Codex).await;
        assert_eq!(prompts.agent_prompt().text, AGENT_PROMPT);
        assert!(prompts.agent_prompt().is_default);

        prompts
            .set_agent_prompt("You are a quieter hirsel.\n")
            .await
            .unwrap();
        assert_eq!(prompts.agent_prompt().text, "You are a quieter hirsel.\n");
        assert!(!prompts.agent_prompt().is_default);
        // The guidance the session is prompted with is the body plus the
        // host-generated section, never the bundled body once overridden.
        let guidance = prompts.agent_guidance();
        assert!(guidance.starts_with("You are a quieter hirsel."));
        assert!(guidance.ends_with("## Host configuration\n"));

        // Reload from disk: the override survives a fresh store.
        let reloaded = config(&dir, ProviderMode::Codex).await;
        assert_eq!(reloaded.agent_prompt().text, "You are a quieter hirsel.\n");

        for empty in ["", "   \n\t "] {
            prompts.set_agent_prompt(empty).await.unwrap();
            assert_eq!(prompts.agent_prompt().text, AGENT_PROMPT);
            assert!(prompts.agent_prompt().is_default);
        }
    }

    #[tokio::test]
    async fn a_multi_line_prompt_stays_hand_editable_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = config(&dir, ProviderMode::Codex).await;
        let body = "# hirsel\n\nLine one.\n\n- a bullet\n- another\n";
        prompts.set_agent_prompt(body).await.unwrap();

        let contents = tokio::fs::read_to_string(dir.path().join("hirsel.toml"))
            .await
            .unwrap();
        assert!(contents.contains("prompt = \"\"\""), "{contents}");
        assert!(contents.contains("\n- a bullet\n"), "{contents}");
        assert_eq!(prompts.agent_prompt().text, body);

        // Text the multi-line form cannot carry verbatim still round-trips.
        let awkward = "a \"\"\" fence and a \\ backslash\n";
        prompts.set_fork_prompt(awkward).await.unwrap();
        let reloaded = config(&dir, ProviderMode::Codex).await;
        assert_eq!(reloaded.fork().unwrap().prompt.text, awkward);
    }

    #[tokio::test]
    async fn a_hand_edit_is_picked_up_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirsel.toml");
        let prompts = config(&dir, ProviderMode::Codex).await;
        assert!(prompts.agent_prompt().is_default);

        let edited = std::fs::read_to_string(&path).unwrap().replacen(
            "[agent]\n",
            "[agent]\nprompt = \"\"\"\nhand-edited body\n\"\"\"\n",
            1,
        );
        std::fs::write(&path, edited).unwrap();

        assert_eq!(prompts.agent_prompt().text, "hand-edited body\n");
        assert!(!prompts.agent_prompt().is_default);
        assert!(prompts.agent_guidance().starts_with("hand-edited body"));
    }

    #[tokio::test]
    async fn the_fork_model_is_validated_against_the_booted_provider() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = config(&dir, ProviderMode::Codex).await;
        // With no stored pair, use this provider's cheap lane.
        assert_eq!(
            prompts.fork_model(),
            Some(ModelSelection {
                id: "gpt-5.6-luna".to_string(),
                variant: "max".to_string(),
            })
        );

        assert!(
            prompts
                .set_fork_model("gpt-5.6-luna", "xhigh")
                .await
                .is_ok()
        );
        assert_eq!(prompts.fork_model().unwrap().variant, "xhigh");
        // Off-registry ids and variants are rejected commands, and the stored
        // selection is untouched.
        assert!(
            prompts
                .set_fork_model("gpt-5.6-terra", "high")
                .await
                .is_err()
        );
        assert!(
            prompts
                .set_fork_model("google/gemini-3.7-flash", "default")
                .await
                .is_err()
        );
        assert!(
            prompts
                .set_fork_model("gpt-5.6-luna", "ultra")
                .await
                .is_err()
        );
        assert_eq!(prompts.fork_model().unwrap().id, "gpt-5.6-luna");
    }

    #[tokio::test]
    async fn anthropic_mode_offers_no_fork_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = config(&dir, ProviderMode::Anthropic).await;
        assert!(prompts.fork().is_none());
        assert!(prompts.snapshot().fork.is_none());
        assert!(
            prompts
                .set_fork_model("gpt-5.6-luna", "high")
                .await
                .is_err()
        );
        // The Agent prompt is editable in every provider mode.
        assert!(prompts.snapshot().agent.is_default);
    }

    #[tokio::test]
    async fn the_fork_prompt_defaults_to_the_bundled_body_and_overrides_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = config(&dir, ProviderMode::OpenRouter).await;
        let fork = prompts.fork().unwrap();
        assert_eq!(fork.prompt.text, FORK_PROMPT);
        assert!(fork.prompt.is_default);
        assert_eq!(fork.current.id, "google/gemini-3.7-flash");
        assert!(
            fork.available
                .iter()
                .any(|model| model.id == fork.current.id)
        );

        prompts.set_fork_prompt("Triage quietly.").await.unwrap();
        let fork = prompts.fork().unwrap();
        assert_eq!(fork.prompt.text, "Triage quietly.");
        assert!(!fork.prompt.is_default);
        // The fork prompt is a separate key: the Agent prompt is untouched.
        assert!(prompts.agent_prompt().is_default);
    }
}
