//! The fork's context pack.
//!
//! A triage fork never sees the main Agent's transcript. It sees exactly four
//! things, and this module is the single authority on what they are:
//!
//! 1. **The triggering message, verbatim.** Whatever woke the host — a
//!    Sub-agent terminal line, a monitor's wake text, an external
//!    notification — reproduced without paraphrase. Triage that reads a
//!    summary of the thing it is triaging is triage of the summary.
//! 2. **The live event/task inventory.** Just enough per row to recognise
//!    "already known / already handled": id, kind, name, one-line description,
//!    status. Never the UI payload, never archived rows.
//! 3. **A short conversation tail.** The last few Owner/Agent messages, each
//!    truncated to one line, so the fork can tell "the Owner already knows"
//!    from "this is new". Not a transcript, and never enough to be one.
//! 4. **Recorded rules.** The taste-store decisions
//!    (`docs/product-direction.md` §11) so a ruled decision auto-resolves at
//!    the triage layer instead of paging the main Agent again.
//!
//! Everything here is a pure function over owned snapshots: the builder takes
//! data, returns a string, and touches no storage, clock, or session. That is
//! deliberate — the pack is the product of this feature, so it has to be
//! assertable in a unit test without a provider or a lash session.

use hirsel_proto::{ChatAuthor, ChatMessage, Event, EventStatus};

use crate::storage::TasteDecision;

/// How many live events the pack lists. Beyond this the inventory stops being
/// a recognition aid and starts being a context dump.
pub(crate) const PACK_EVENT_LIMIT: usize = 20;
/// How many recent chat messages the pack carries.
pub(crate) const PACK_CHAT_LIMIT: usize = 10;
/// How many recorded rules the pack carries.
pub(crate) const PACK_RULE_LIMIT: usize = 20;
/// Per-line truncation for anything quoted out of chat or an event.
const PACK_LINE_CHARS: usize = 240;
/// The triggering message is the one thing that is never summarised, but it is
/// still bounded: a runaway monitor tail must not blow the fork's window.
const TRIGGER_CHARS: usize = 8 * 1024;

/// Where a non-owner message came from. The fork is told this verbatim because
/// "a Sub-agent finished" and "a monitor fired" call for different triage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeSource {
    /// A Sub-agent reached a terminal state.
    Subagent { process_id: String },
    /// A monitor's probe fired its wake condition.
    Monitor { monitor_id: String, label: String },
    /// Anything else the host routed in (debug injection, external notifier).
    External { origin: String },
}

impl WakeSource {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Subagent { process_id } => format!("sub-agent {process_id}"),
            Self::Monitor { monitor_id, label } => format!("monitor {label} ({monitor_id})"),
            Self::External { origin } => format!("external {origin}"),
        }
    }

    /// A stable prefix for log fields and enqueue source keys.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Subagent { .. } => "subagent",
            Self::Monitor { .. } => "monitor",
            Self::External { .. } => "external",
        }
    }
}

/// One incoming non-owner message: exactly one fork is spawned per value of
/// this type (ADR-0015 ruling 1 — no batching, no debouncing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeMessage {
    pub source: WakeSource,
    /// The triggering text, verbatim.
    pub text: String,
    /// A per-message key: dispatch logs it, and the escalated brief carries it
    /// as its enqueue source key so a brief is traceable back to its trigger.
    pub key: String,
}

impl WakeMessage {
    pub fn new(source: WakeSource, text: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            source,
            text: text.into(),
            key: key.into(),
        }
    }
}

/// The owned snapshot the pack is rendered from. Read once, off the hot path,
/// so the builder itself stays pure.
#[derive(Debug, Clone, Default)]
pub struct PackContext {
    pub events: Vec<Event>,
    pub recent_chat: Vec<ChatMessage>,
    pub rules: Vec<TasteDecision>,
}

/// Render the fork's context pack. Pure: same inputs, same string.
pub fn build_pack(message: &WakeMessage, context: &PackContext) -> String {
    let mut pack = String::new();

    pack.push_str("## Incoming event\n\n");
    pack.push_str(&format!("Source: {}\n\n", message.source.label()));
    pack.push_str("Verbatim:\n\n```\n");
    pack.push_str(&truncate(message.text.trim(), TRIGGER_CHARS));
    pack.push_str("\n```\n");

    pack.push_str("\n## Live Tasks and events\n\n");
    let events = context
        .events
        .iter()
        .take(PACK_EVENT_LIMIT)
        .collect::<Vec<_>>();
    if events.is_empty() {
        pack.push_str("(none open)\n");
    } else {
        for event in events {
            pack.push_str(&format!(
                "- [{}] #{} {} — {} ({})\n",
                kind_name(event),
                event.id,
                event.name,
                truncate(&one_line(&event.description), PACK_LINE_CHARS),
                status_name(event.status),
            ));
        }
    }

    pack.push_str("\n## Recent conversation\n\n");
    let chat = context
        .recent_chat
        .iter()
        .rev()
        .take(PACK_CHAT_LIMIT)
        .collect::<Vec<_>>();
    if chat.is_empty() {
        pack.push_str("(none)\n");
    } else {
        for message in chat.into_iter().rev() {
            pack.push_str(&format!(
                "- {}: {}\n",
                author_name(message.author),
                truncate(&one_line(&message.body), PACK_LINE_CHARS),
            ));
        }
    }

    pack.push_str("\n## Recorded rules\n\n");
    let rules = context
        .rules
        .iter()
        .rev()
        .take(PACK_RULE_LIMIT)
        .collect::<Vec<_>>();
    if rules.is_empty() {
        pack.push_str("(none)\n");
    } else {
        for rule in rules.into_iter().rev() {
            pack.push_str(&format!(
                "- (from event #{}) {}\n",
                rule.event_id,
                truncate(&one_line(&rule.rule), PACK_LINE_CHARS),
            ));
        }
    }

    pack
}

fn kind_name(event: &Event) -> &'static str {
    match event.kind {
        hirsel_proto::EventKind::Judgment => "judgment",
        hirsel_proto::EventKind::Summary => "summary",
        hirsel_proto::EventKind::Info => "info",
    }
}

fn status_name(status: EventStatus) -> &'static str {
    match status {
        EventStatus::Open => "open",
        EventStatus::Done => "done",
    }
}

fn author_name(author: ChatAuthor) -> &'static str {
    match author {
        ChatAuthor::Owner => "owner",
        ChatAuthor::Agent => "agent",
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept = text.chars().take(limit).collect::<String>();
    format!("{kept}…")
}
