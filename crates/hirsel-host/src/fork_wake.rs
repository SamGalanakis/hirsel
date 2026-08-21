//! Ephemeral fork triage for non-owner wakes (ADR-0015).
//!
//! Before this module, every incoming message turned the one resident Agent
//! session: an Owner sentence and "worker still running, nothing to do" cost
//! the same model and silted up the same durable transcript.
//!
//! Now the host has exactly one mechanical dispatch rule:
//!
//! * **Owner message** → the main Agent's queue, unchanged
//!   (`AgentRuntime::enqueue`). Owner messages never pass through a fork.
//! * **Anything else** → [`ForkWake::dispatch`], which spawns one ephemeral
//!   child session per message. It reads a curated [`pack`], takes exactly one
//!   exit from [`tools`], and is deleted. Only its Escalate exit puts a turn
//!   on the main Agent's queue.
//!
//! Everything discretionary — what to drop, what to record, what to escalate —
//! is `prompts/fork.md`, editable in Settings. ADR-0005's principle survives
//! intact: the host installs no wake *policy*, only this dispatch.
//!
//! Module map:
//!
//! * [`pack`] — the pure, unit-testable context-pack builder, and the
//!   [`WakeMessage`] type every wake site converts its event into.
//! * [`tools`] — the fork's three exits as its complete tool catalog.
//! * [`dispatch`] — one fork per message, bounded concurrency, fail-open.
//! * [`session`] — the lash child-session runner that actually executes a
//!   triage turn.

mod dispatch;
mod pack;
mod session;
mod tools;

#[cfg(test)]
mod tests;

pub use dispatch::{
    FORK_TURN_TIMEOUT, ForkWake, ForkWakeHandle, MAX_CONCURRENT_FORKS, TriageRequest, TriageRunner,
};
pub use pack::{PackContext, WakeMessage, WakeSource, build_pack};
pub use session::LashForkRunner;
pub use tools::{BriefSink, ForkExit, ForkToolProvider, ForkTools, fork_tool_definitions};
