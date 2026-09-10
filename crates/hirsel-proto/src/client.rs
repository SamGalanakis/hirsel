//! Client → host frames and the auth/mode enums they carry.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    #[default]
    Send,
    NextTurn,
}

impl SendMode {
    pub fn is_send(&self) -> bool {
        matches!(self, Self::Send)
    }
}

/// Which resident agent a provider/model op addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSlot {
    Main,
    Fork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushPlatform {
    Android,
    Web,
    Ios,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelloAuth {
    StaticToken(String),
    DeviceToken(String),
    PairingCode { code: String, device_label: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub enum ClientToHost {
    Hello {
        auth: HelloAuth,
    },

    ListArtifacts {
        client_id: String,
        thread_id: Option<u64>,
    },
    OpenArtifact {
        client_id: String,
        artifact_id: u64,
    },
    CreateThread {
        #[serde(deserialize_with = "required_nullable_parent")]
        parent_thread_id: Option<u64>,
        client_id: String,
        title: String,
    },
    OpenThread {
        client_id: String,
        thread_id: u64,
        before_id: Option<u64>,
    },
    AddThreadRelated {
        client_id: String,
        history_id: String,
        thread_id: u64,
        target: crate::ThreadRelatedTarget,
        title: Option<String>,
    },
    RemoveThreadRelated {
        client_id: String,
        history_id: String,
        thread_id: u64,
        item_id: u64,
    },
    SendThreadMessage {
        client_id: String,
        thread_id: u64,
        body: String,
        #[serde(default)]
        attachments: Vec<String>,
        #[serde(default)]
        mentions: Vec<u64>,
        #[serde(default)]
        mode: SendMode,
        artifact_ids: Vec<u64>,
    },
    ThreadAction {
        thread_id: u64,
        action: String,
        #[serde(default)]
        data: serde_json::Value,
        #[serde(default)]
        expected_revision: Option<u64>,
    },

    CancelTurn {
        thread_id: u64,
    },
    CancelQueued {
        client_id: String,
    },
    SetModel {
        provider_id: String,
        model_id: String,
        variant: String,
    },
    SetSubagentModel {
        provider: String,
        model_id: String,
        enabled: bool,
        enabled_variants: Vec<String>,
    },
    /// Replace the Agent's system prompt body. An empty or whitespace-only
    /// `text` clears the override and restores the bundled default.
    SetAgentPrompt {
        text: String,
    },
    /// Replace the fork agent's prompt body; empty clears the override.
    SetForkPrompt {
        text: String,
    },
    /// Select the fork agent's model + reasoning variant for the named
    /// provider. Naming it makes concurrent provider/model changes race-free.
    SetForkModel {
        provider_id: String,
        model_id: String,
        variant: String,
    },
    /// Point one resident agent at a provider instance, seeding that provider's
    /// default model + variant.
    SetAgentProvider {
        agent: AgentSlot,
        provider_id: String,
    },
    /// Add an OpenAI-compatible provider instance.
    AddProvider {
        id: String,
        label: String,
        base_url: String,
        api_key: String,
        default_model: String,
    },
    /// Edit one instance. Omitted fields are unchanged; an `api_key` of `""`
    /// clears the stored key.
    UpdateProvider {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_model: Option<String>,
    },
    RemoveProvider {
        id: String,
    },
    /// Re-probe an OAuth provider's local credentials.
    RedetectProvider {
        id: String,
    },
    UploadBlob {
        client_id: String,
        name: String,
        mime: String,
        data_b64: String,
    },
    GetBlobUrl {
        client_id: String,
        blob_id: String,
    },

    RegisterPushToken {
        platform: PushPlatform,
        token: String,
    },
    UnregisterPushToken {
        token: String,
    },

    ViewEvent {
        instance_id: String,
        action: String,
        #[serde(default)]
        data: serde_json::Value,
    },
}

// A missing destination is distinct from an explicitly requested root.
fn required_nullable_parent<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(d)
}
