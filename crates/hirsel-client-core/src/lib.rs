//! Shared transport and durable Thread state for Hirsel clients.

mod client;
mod config;
mod identity;
mod observer;
mod store;
mod timeline;
mod transport;

pub use client::{Client, ClientError, SendReceipt, SendThreadMessageRequest};
pub use config::{ClientConfig, ConfigError, ReconnectPolicy};
pub use identity::generate_iroh_identity;
pub use observer::{ClientObserver, LifecycleEvent};
pub(crate) use store::ConnectionState;
pub use store::{
    AgentActivity, ChatEntry, ClientSnapshot, ConfirmedMessage, CreatedThread, PendingSend,
    ThreadBrief, ThreadStream,
};
pub use timeline::{TimelineTextKind, timeline_text};

pub use hirsel_proto::{
    AgentActivityState, Blob, ChatAuthor, ChatMessage, EffectAction, HelloAuth, ProcessInfo,
    ProcessState, Thread, ThreadActivity, ThreadAttention, ThreadEffect, ThreadEffectKind,
    ThreadEffectReceipt, ThreadEffectRefusal, ThreadEffectTarget, ThreadIcon, ThreadKind,
    ThreadRelatedItem, ThreadRelatedTarget, ThreadTint, ThreadTurn, ThreadTurnState,
    ToolCallSummary,
};

#[cfg(test)]
mod thread_tests;
