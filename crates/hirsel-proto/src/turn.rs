//! Streamed turn events: the agent's prose, reasoning, tool calls, and code cells.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentActivityState {
    Thinking,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEvent {
    pub seq: u64,
    pub event: TurnEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTurnTimeline {
    pub turn_id: u64,
    pub events: Vec<TurnEvent>,
}

/// A bounded verbatim JSON payload for an expandable timeline row. `truncated`
/// is explicit so a client never presents clipped content as complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEventPayload {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
#[serde(rename_all = "snake_case")]
pub enum TurnEventKind {
    Prose {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolStart {
        /// Correlates start/done pairs so clients resolve the right row.
        id: String,
        name: String,
        summary: Option<String>,
        input: Option<TurnEventPayload>,
    },
    ToolDone {
        id: String,
        name: String,
        ok: bool,
        summary: Option<String>,
        result: Option<TurnEventPayload>,
    },
    /// The Agent's own program for one cell, verbatim. Unlike tool summaries
    /// this carries the FULL source (clients render it as code, not a
    /// one-liner); `truncated` marks the rare block clipped at the host's
    /// safety cap.
    CodeStart {
        /// Correlates start/done pairs so clients resolve the right cell.
        id: String,
        language: String,
        code: String,
        truncated: bool,
    },
    CodeDone {
        id: String,
        ok: bool,
        summary: Option<String>,
    },
}
