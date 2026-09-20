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
mod thread_icon;
mod turn;
mod view;

pub use chat::{
    Blob, ChatAuthor, ChatMessage, MessageOrigin, ProcessOutcome, ToolCallSummary, TriggerLabel,
};
pub use client::{AgentSlot, ClientToHost, HelloAuth, PushPlatform, SendMode};
pub use host::HostToClient;
pub use models::{
    AgentModelConfig, AvailableModel, ForkAgentConfig, ModelSelection, ModelSnapshot, PromptDoc,
    PromptSnapshot, SubagentModel, SubagentModelCatalog, SubagentProviderModels,
};
pub use process::{ProcessInfo, ProcessState};
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
    ReachTarget, Thread, ThreadActivity, ThreadAttention, ThreadBrief, ThreadDetail,
    ThreadExecutionTarget, ThreadGrant, ThreadGrantSource, ThreadGrantTarget, ThreadKind,
    ThreadRelatedItem, ThreadRelatedTarget, ThreadStatus, ThreadStatusKind, ThreadTurn,
    ThreadTurnState,
};
pub use thread_icon::{
    THREAD_SYMBOL_GROUPS, THREAD_SYMBOLS, ThreadIcon, ThreadTint, is_thread_symbol,
};
