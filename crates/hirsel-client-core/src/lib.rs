//! Shared transport and durable Thread state for Hirsel clients.

mod client;
mod config;
mod identity;
mod observer;
mod store;
mod transport;

pub use client::{Client, ClientError, SendReceipt, SendThreadMessageRequest};
pub use config::{ClientConfig, ConfigError, ReconnectPolicy};
pub use identity::generate_iroh_identity;
pub use observer::{ClientObserver, LifecycleEvent};
pub use store::{
    AgentActivity, ChatEntry, ClientSnapshot, ConfirmedMessage, ConnectionState, CreatedThread,
    PendingSend, ThreadBrief, ThreadStream,
};

pub use hirsel_proto::{
    AgentActivityState, Blob, ChatAuthor, ChatMessage, HelloAuth, ProcessInfo, ProcessKind,
    ProcessState, Thread, ThreadActivity, ThreadAttention, ThreadKind, ThreadRelatedItem,
    ThreadRelatedTarget, ThreadTurn, ThreadTurnState, ToolCallSummary,
};

#[cfg(test)]
mod thread_tests;
