//! Side chats (event forks): a scoped agent conversation seeded from an event.
//!
//! The subsystem is split into focused submodules; this root re-exports the
//! surface every other module in the crate uses.

mod backend;
mod fork_tool;
mod manager;
mod projection;
mod session;
mod sink;

#[cfg(test)]
mod tests;

pub(crate) use backend::{SideChatBackend, SideSessionCompatibility};
pub use manager::{SideChatManager, SideChatOpenResult, SideChatView};
