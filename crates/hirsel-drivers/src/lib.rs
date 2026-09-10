//! Sub-agent Drivers: one uniform trait over the coding-agent CLIs hirsel
//! supervises, plus a fixture-backed fake for tests.
//!
//! Every public item is re-exported at the crate root, so consumers keep
//! importing `hirsel_drivers::{SubagentDriver, SpawnSpec, ...}` directly.

mod claude;
mod codex;
mod fake;
mod shared;
mod types;

#[cfg(test)]
mod shared_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use claude::ClaudeCodeDriver;
pub use codex::CodexDriver;
pub use fake::FakeDriver;
pub use types::{
    AgentKind, DriverError, DriverResult, EventStream, ScopedMcpLaunch, SessionHandle, SpawnSpec,
    SubagentDriver, SubagentEvent, TerminalOutcome,
};
