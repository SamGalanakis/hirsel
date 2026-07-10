//! Shared transport and state foundation for Hirsel clients.
//!
//! This first slice deliberately excludes attachments and blobs, send modes and
//! cancellation, running-turn timelines, and side chats. Those protocol
//! features remain in `hirsel-proto` and can be layered onto this foundation in
//! later slices.

mod client;
mod config;
mod observer;
mod store;
mod transport;

pub use client::{Client, ClientError, SendMessageRequest, SendReceipt};
pub use config::{ClientConfig, ConfigError, ReconnectPolicy};
pub use observer::{ClientObserver, LifecycleEvent};
pub use store::{AgentActivity, ChatEntry, ClientSnapshot, ConnectionState};

pub use hirsel_proto::{
    AgentActivityState, Blob, ChatAuthor, ChatMessage, HelloAuth, Ping, PingStatus, ProcessInfo,
    ProcessKind, ProcessState, QuickReply, ToolCallSummary,
};
