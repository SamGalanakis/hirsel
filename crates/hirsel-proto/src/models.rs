//! Runtime-selectable model state for the main agent and sub-agents.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub id: String,
    pub variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableModel {
    pub id: String,
    pub label: String,
    pub variants: Vec<String>,
    pub default_variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub current: ModelSelection,
    pub available: Vec<AvailableModel>,
    /// The provider instance this agent runs on. Absent on older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// True when the selected provider takes a free-text model id: `available`
    /// is then empty and `current.id` is whatever the Owner typed.
    #[serde(default)]
    pub free_text_model: bool,
}

/// One Owner-editable prompt: the text the Agent actually gets, plus whether
/// that text is still the bundled default (no override stored in `hirsel.toml`).
/// `text` is always the EFFECTIVE prompt, so a client renders one field and a
/// reset is "clear the override", never "paste the default back".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptDoc {
    pub text: String,
    pub is_default: bool,
}

/// The wake-triage fork agent's configuration: which model it runs as and the
/// prompt it runs with. Persisted and edited like every other setting; no
/// runtime consumes it yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkAgentConfig {
    pub current: ModelSelection,
    pub available: Vec<AvailableModel>,
    pub prompt: PromptDoc,
    /// The provider instance the fork runs on. Absent on older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// True when the selected provider takes a free-text model id: `available`
    /// is then empty and `current.id` is whatever the Owner typed.
    #[serde(default)]
    pub free_text_model: bool,
}

/// The Owner-editable prompt surface carried on `hello_ok` and replaced
/// wholesale by `prompts_changed`. `fork` is absent when the booted provider
/// offers no runtime-selectable models (Anthropic mode pins its model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSnapshot {
    pub agent: PromptDoc,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<ForkAgentConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentModel {
    pub id: String,
    pub label: String,
    pub variants: Vec<String>,
    pub enabled_variants: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderModels {
    pub provider: String,
    pub label: String,
    pub models: Vec<SubagentModel>,
}

/// The native in-process Lash coding worker as a delegation target. It is not
/// a CLI lane: there is no curated model list and no reasoning variant, only
/// the OpenAI-compatible provider instance it is routed through and the model
/// that route opens on.
///
/// `unavailable_reason` is the single availability signal — `Some` means no
/// configured provider can host the worker, so the row explains itself instead
/// of offering a control that cannot work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentNativeWorker {
    pub label: String,
    pub enabled: bool,
    /// The provider instance a delegation that names none is routed through,
    /// or `None` when that default instance is not configured.
    pub provider_id: Option<String>,
    /// Every configured provider instance eligible to host the worker; a
    /// delegation may name any of them explicitly.
    pub eligible_provider_ids: Vec<String>,
    /// The model the default route opens on: the Owner's override when set,
    /// otherwise the shipped default.
    pub model: String,
    pub default_model: String,
    pub model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentModelCatalog {
    pub providers: Vec<SubagentProviderModels>,
    pub native_worker: SubagentNativeWorker,
}
