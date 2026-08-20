//! The Hirsel owner-link wire protocol.
//!
//! Every type below is part of the wire contract: serde attributes and field
//! order are the format. The modules are an organisational seam only — all
//! types stay re-exported at the crate root (`hirsel_proto::TypeName`), which
//! is the path every consumer uses.

mod chat;
mod client;
mod event;
mod host;
mod models;
mod process;
mod turn;
mod view;

pub use chat::{Blob, ChatAuthor, ChatMessage, ToolCallSummary};
pub use client::{ClientToHost, HelloAuth, PushPlatform, SendMode};
pub use event::{
    Event, EventKind, EventLifecycle, EventSource, EventSourceKind, EventStatus, Ping, PingStatus,
    QuickReply,
};
pub use host::HostToClient;
pub use models::{
    AvailableModel, ForkAgentConfig, ModelSelection, ModelSnapshot, PromptDoc, PromptSnapshot,
    SubagentModel, SubagentModelCatalog, SubagentProviderModels,
};
pub use process::{ProcessInfo, ProcessKind, ProcessState, SideChatSummary};
pub use turn::{AgentActivityState, TurnEvent, TurnEventKind};
pub use view::ViewInstance;

pub const IROH_OWNER_ALPN: &[u8] = b"hirsel/owner/1";

#[cfg(test)]
mod tests;
