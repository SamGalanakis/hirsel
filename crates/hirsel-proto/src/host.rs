//! Host → client frames.

use serde::{Deserialize, Serialize};

use crate::chat::{Blob, ChatMessage};
use crate::event::Event;
use crate::models::{ModelSelection, ModelSnapshot, SubagentModelCatalog};
use crate::process::{ProcessInfo, SideChatSummary};
use crate::turn::{AgentActivityState, TurnEventKind};
use crate::view::ViewInstance;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum HostToClient {
    Paired {
        device_token: String,
    },
    HelloOk {
        latest_msg_id: u64,
        messages: Vec<ChatMessage>,
        events: Vec<Event>,
        processes: Vec<ProcessInfo>,
        #[serde(default)]
        side_chats: Vec<SideChatSummary>,
        /// Host build identity (crate version + git sha), shown in Settings → About.
        /// `#[serde(default)]` keeps older hosts/snapshots that omit it parseable.
        #[serde(default)]
        host_version: String,
        /// Runtime-selectable main-agent model state. Older hosts omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<ModelSnapshot>,
        /// Runtime-selectable Sub-agent model catalog. Older hosts omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_models: Option<SubagentModelCatalog>,
        #[serde(default)]
        views: Vec<ViewInstance>,
    },
    Msg {
        message: ChatMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    ProcessUpsert {
        process: ProcessInfo,
    },
    TurnEvent {
        seq: u64,
        event: TurnEventKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    MsgRemoved {
        id: u64,
    },
    AgentActivity {
        state: AgentActivityState,
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sc: Option<String>,
    },
    EventUpsert {
        event: Event,
    },
    ModelChanged {
        current: ModelSelection,
    },
    SubagentModelsChanged {
        catalog: SubagentModelCatalog,
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
    SideChatOpen {
        sc: String,
        event_id: u64,
        /// Compatibility mirror for clients that still address side chats as Pings.
        ping_id: u64,
        event: Event,
        messages: Vec<ChatMessage>,
    },
    ConclusionDraft {
        sc: String,
        text: String,
    },
    SideChatClosed {
        sc: String,
    },
    ViewUpsert {
        instance_id: String,
        placement: String,
        spec: serde_json::Value,
    },
    ViewRemoved {
        instance_id: String,
    },
}
