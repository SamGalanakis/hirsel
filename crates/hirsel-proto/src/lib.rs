//! The Hirsel owner-link wire protocol.
//!
//! Every type below is part of the wire contract: serde attributes and field
//! order are the format. The modules are an organisational seam only — all
//! types stay re-exported at the crate root (`hirsel_proto::TypeName`), which
//! is the path every consumer uses.

mod artifact;
pub use artifact::{Artifact, ArtifactKind, ArtifactSummary};

mod chat;
mod client;
mod host;
mod models;
mod process;
mod providers;
mod thread;
mod turn;
mod view;

pub use chat::{Blob, ChatAuthor, ChatMessage, ToolCallSummary};
pub use client::{AgentSlot, ClientToHost, HelloAuth, PushPlatform, SendMode};
pub use host::HostToClient;
pub use models::{
    AvailableModel, ForkAgentConfig, ModelSelection, ModelSnapshot, PromptDoc, PromptSnapshot,
    SubagentModel, SubagentModelCatalog, SubagentNativeWorker, SubagentProviderModels,
};
pub use process::{ProcessInfo, ProcessKind, ProcessState};
pub use providers::{
    DetectionStatus, MaskedSecret, ProviderInstance, ProviderKind, ProviderRoster,
    ProviderSelection,
};
pub use turn::{
    AgentActivityState, ThreadTurnTimeline, TurnEvent, TurnEventKind, TurnEventPayload,
};
pub use view::ViewInstance;

pub const IROH_OWNER_ALPN: &[u8] = b"hirsel/owner/1";

#[cfg(test)]
mod tests;

pub use thread::{
    Thread, ThreadActivity, ThreadAttention, ThreadBrief, ThreadDetail, ThreadKind,
    ThreadRelatedItem, ThreadRelatedTarget, ThreadTurn, ThreadTurnState,
};
