//! The provider roster: the configured instances a resident agent can run on.

use serde::{Deserialize, Serialize};

/// How a provider instance authenticates, and what shape its model choice takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Local `~/.codex/auth.json` OAuth. Curated model select.
    Codex,
    /// Local Claude CLI credentials. Sub-agents only (ADR-0015 terms ruling):
    /// never selectable as the main or fork provider.
    Claude,
    /// An OpenAI-compatible endpoint: base URL + API key, free-text model id.
    ///
    /// Spelled out explicitly because `rename_all = "snake_case"` would break
    /// this identifier at the capital A — `open_ai_compatible` — while the
    /// config store, the docs and the client all say `openai_compatible`. The
    /// literal is pinned by a test.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
}

/// A stored secret as the wire is allowed to describe it: whether one is set,
/// and its last few characters. The full key NEVER leaves the host.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MaskedSecret {
    pub present: bool,
    /// Up to the last 4 characters of the stored key; empty when absent or
    /// when the key is too short to reveal any tail safely (< 8 chars).
    #[serde(default)]
    pub tail: String,
}

/// Whether the host can see the local credentials an OAuth-detected provider
/// needs. Refreshed on demand by `redetect_provider`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionStatus {
    pub detected: bool,
    /// The path the host probed, so the Owner can see what was checked.
    pub path: String,
    /// A non-secret identity hint when detected (e.g. a Codex account id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_hint: Option<String>,
    /// Why detection failed, when it did. Never carries credential material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One configured provider instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInstance {
    pub id: String,
    pub kind: ProviderKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: MaskedSecret,
    /// The model id an agent is seeded with when it selects this instance.
    #[serde(default)]
    pub default_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection: Option<DetectionStatus>,
    /// Whether the main Agent and the fork may select it. Claude is `false`.
    pub agent_selectable: bool,
    /// Built-in instances (`codex`, `claude`) are configured, never removed.
    pub removable: bool,
}

/// The whole roster, carried on `hello_ok` and replaced wholesale by
/// `providers_changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRoster {
    pub instances: Vec<ProviderInstance>,
    /// The provider the resident main-agent session actually booted on. A
    /// main-agent provider change is stored at once but only takes effect on
    /// the next host start, so the client can say so plainly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booted_provider_id: Option<String>,
}
