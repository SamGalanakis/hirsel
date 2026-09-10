//! Host → client frames.

use serde::{Deserialize, Serialize};

use crate::chat::{Blob, ChatMessage};
use crate::models::{ModelSnapshot, PromptSnapshot, SubagentModelCatalog};
use crate::process::ProcessInfo;
use crate::providers::ProviderRoster;
use crate::turn::{AgentActivityState, TurnEventKind};
use crate::view::ViewInstance;

// `hello_ok` is the whole-session snapshot: it is inherently far larger than
// the incremental frames beside it, and boxing its fields would put a pointer
// chase in the wire contract to save bytes on a frame that is sent once per
// connection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum HostToClient {
    Paired {
        device_token: String,
    },
    HelloOk {
        history_id: String,
        threads: Vec<crate::Thread>,
        processes: Vec<ProcessInfo>,
        host_version: String,
        model: Option<ModelSnapshot>,
        subagent_models: Option<SubagentModelCatalog>,
        prompts: Option<PromptSnapshot>,
        providers: Option<ProviderRoster>,
        views: Vec<ViewInstance>,
    },
    Msg {
        message: ChatMessage,
    },

    ProcessUpsert {
        process: ProcessInfo,
    },
    TurnEvent {
        turn_id: u64,
        thread_id: u64,
        seq: u64,
        event: TurnEventKind,
    },
    MsgRemoved {
        id: u64,
    },
    AgentActivity {
        turn_id: u64,
        thread_id: u64,
        state: AgentActivityState,
        text: Option<String>,
    },
    ArtifactsListed {
        client_id: String,
        artifacts: Vec<crate::ArtifactSummary>,
    },
    ArtifactOpened {
        client_id: String,
        artifact: crate::Artifact,
    },
    ArtifactUpsert {
        artifact: crate::ArtifactSummary,
    },
    ThreadUpsert {
        thread: crate::Thread,
    },
    ThreadOpened {
        client_id: String,
        detail: crate::ThreadDetail,
    },
    ThreadRelatedChanged {
        client_id: Option<String>,
        history_id: String,
        thread_id: u64,
        revision: u64,
        items: Vec<crate::ThreadRelatedItem>,
    },
    ThreadCreated {
        client_id: String,
        thread: crate::Thread,
    },
    ThreadActivity {
        activity: crate::ThreadActivity,
    },
    ThreadTurn {
        turn: crate::ThreadTurn,
    },

    /// The main agent's model surface after an accepted edit — the WHOLE
    /// snapshot, because a provider change reshapes it: a curated registry and
    /// a free-text id are two different controls, and the client cannot derive
    /// one from a bare selection.
    ModelChanged {
        model: ModelSnapshot,
    },
    SubagentModelsChanged {
        catalog: SubagentModelCatalog,
    },
    /// The Owner-editable prompt surface after an accepted edit — the whole
    /// snapshot, because a prompt edit can change the fork config's shape too.
    PromptsChanged {
        prompts: PromptSnapshot,
    },
    /// The provider roster after an accepted edit — the whole roster, because
    /// one edit can change another instance's derived state.
    ProvidersChanged {
        roster: ProviderRoster,
    },
    BlobOk {
        client_id: String,
        blob: Blob,
    },
    BlobUrl {
        client_id: String,
        blob_id: String,
        url: String,
        expires_at: u64,
    },
    Error {
        detail: String,
        /// Correlates the error to a specific client request (upload_blob,
        /// cancel_queued) so the client can mark the exact chip/bubble.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },

    ViewUpsert {
        thread_id: u64,
        instance_id: String,
        placement: String,
        spec: serde_json::Value,
    },
    ViewRemoved {
        instance_id: String,
    },
    /// A message an enabled plugin broadcast to every connected client via
    /// `PluginCtx::push`. `plugin` is the plugin id; `topic` and `data` are
    /// entirely the plugin's own vocabulary — the host neither interprets nor
    /// validates them, it fans them out.
    PluginPush {
        plugin: String,
        topic: String,
        data: serde_json::Value,
    },
}
