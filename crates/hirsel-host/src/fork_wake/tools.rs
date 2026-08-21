//! The fork's tool surface: its exits, and nothing else.
//!
//! ADR-0015 gives a triage fork exactly three exits — drop, record, escalate —
//! and the host enforces that as *capability*, not as instruction. A fork is
//! opened against this provider alone, so `subagents_spawn`, `pings_send`,
//! `events_judgment`, `shell_run`, `views_*`, `monitors_*` and every plugin
//! tool are not "forbidden by the prompt": they do not exist in the fork's
//! catalog, and naming one is an invalid tool call.
//!
//! The five tools here map onto the three exits:
//!
//! | tool                   | exit     |
//! |------------------------|----------|
//! | `fork_record_info`     | record   |
//! | `fork_record_summary`  | record   |
//! | `fork_close_task`      | record (Task status) |
//! | `fork_escalate`        | escalate |
//! | `fork_drop`            | drop     |
//!
//! Every one is prefixed `fork_`. That is not decoration: the fork opens on
//! the same core as the main Agent, whose catalog already publishes
//! `events_notify` and friends, and lash rejects a session whose tool names
//! collide. The prefix also makes the fork's transcript unmistakable.
//!
//! `fork_drop` exists because Drop has to be *affirmative*. A fork that ends
//! its turn without calling anything is indistinguishable from a fork that
//! silently failed, and ruling 2 says a non-owner message is never lost — so
//! the dispatcher treats an exitless turn as a failure and escalates a
//! fallback brief. Calling `fork_drop` is how a fork says "I decided nothing
//! is needed" rather than "I fell over".

use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use lash::tools::{
    ToolBinding, ToolCall, ToolContract, ToolDefinition, ToolDefinitionBindingExt, ToolManifest,
    ToolOutcome, ToolProvider,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::pack::WakeMessage;
use crate::tools::ToolSuite;

/// Which exit a fork took. Recorded by the tools, read by the dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkExit {
    Dropped { reason: String },
    Recorded { what: String },
    Escalated { brief: String },
}

/// Where an escalated brief goes: the main Agent's queue, through the same
/// enqueue path an Owner queued turn uses.
#[async_trait]
pub trait BriefSink: Send + Sync {
    /// Inject exactly one distilled brief. `message` is the trigger it came
    /// from, so the sink can carry a traceable source key and marker.
    async fn inject(&self, message: &WakeMessage, brief: &str) -> anyhow::Result<()>;
}

/// The concrete exit implementations, shared by the lash tool provider and by
/// tests (which call these typed methods directly instead of round-tripping
/// through a model).
pub struct ForkTools {
    tools: ToolSuite,
    sink: Arc<dyn BriefSink>,
    message: WakeMessage,
    /// The chat message a recorded event anchors to, when the transcript has
    /// one. `None` means the transcript is empty: see [`ForkTools::record`] —
    /// the fork records nothing and escalates instead, because minting an
    /// anchor line would be the fork speaking to the Owner.
    anchor: Option<u64>,
    exit: Mutex<ExitSlot>,
}

/// The exit slot's three states.
///
/// `Claimed` is the load-bearing one. An exit is only *taken* once its side
/// effect has landed, so the window while a storage write or a queue injection
/// is in flight needs its own state: it must refuse a second exit (a fork gets
/// one) without yet counting as an exit to the dispatcher (whose fail-open path
/// keys on "no exit taken"). Committing after the effect is what makes ruling 2
/// hold — a failed effect rolls the slot back to `Open`, the dispatcher sees no
/// exit, and the message is escalated undistilled rather than lost.
#[derive(Debug, Default)]
enum ExitSlot {
    #[default]
    Open,
    Claimed,
    Taken(ForkExit),
}

impl ForkTools {
    pub fn new(
        tools: ToolSuite,
        sink: Arc<dyn BriefSink>,
        message: WakeMessage,
        anchor: Option<u64>,
    ) -> Self {
        Self {
            tools,
            sink,
            message,
            anchor,
            exit: Mutex::new(ExitSlot::Open),
        }
    }

    /// The exit this fork took, if it took one. An exit whose side effect
    /// failed is deliberately not one.
    pub async fn exit(&self) -> Option<ForkExit> {
        match &*self.exit.lock().await {
            ExitSlot::Taken(exit) => Some(exit.clone()),
            ExitSlot::Open | ExitSlot::Claimed => None,
        }
    }

    /// Claim the right to take an exit, before performing its side effect.
    /// A fork gets one exit: a second claim is a tool error, not a silent
    /// overwrite.
    async fn claim(&self) -> anyhow::Result<()> {
        let mut slot = self.exit.lock().await;
        match &*slot {
            ExitSlot::Open => {
                *slot = ExitSlot::Claimed;
                Ok(())
            }
            ExitSlot::Claimed => {
                anyhow::bail!("this fork is already taking an exit; end the turn now")
            }
            ExitSlot::Taken(existing) => anyhow::bail!(
                "this fork already took its one exit ({}); end the turn now",
                exit_name(existing)
            ),
        }
    }

    /// The side effect landed: the exit is now taken.
    async fn commit(&self, exit: ForkExit) {
        *self.exit.lock().await = ExitSlot::Taken(exit);
    }

    /// The side effect failed. Release the claim so the dispatcher sees an
    /// exitless fork and escalates the original message (ruling 2).
    async fn release(&self) {
        *self.exit.lock().await = ExitSlot::Open;
    }

    pub async fn record_info(
        &self,
        name: &str,
        description: &str,
        content_md: Option<String>,
    ) -> anyhow::Result<Value> {
        let Some(anchor) = self.anchor else {
            return self
                .escalate_unanchored_record("info", name, description, content_md.as_deref())
                .await;
        };
        self.claim().await?;
        match self
            .tools
            .events_notify(name, description, content_md, anchor)
            .await
        {
            Ok(event) => {
                self.commit(ForkExit::Recorded {
                    what: format!("info {name}"),
                })
                .await;
                Ok(json!({ "event_id": event.id, "status": "recorded" }))
            }
            Err(error) => {
                self.release().await;
                Err(error)
            }
        }
    }

    pub async fn record_summary(
        &self,
        name: &str,
        description: &str,
        content_md: Option<String>,
    ) -> anyhow::Result<Value> {
        let Some(anchor) = self.anchor else {
            return self
                .escalate_unanchored_record("summary", name, description, content_md.as_deref())
                .await;
        };
        self.claim().await?;
        match self
            .tools
            .events_summary(name, description, content_md, None, anchor)
            .await
        {
            Ok(event) => {
                self.commit(ForkExit::Recorded {
                    what: format!("summary {name}"),
                })
                .await;
                Ok(json!({ "event_id": event.id, "status": "recorded" }))
            }
            Err(error) => {
                self.release().await;
                Err(error)
            }
        }
    }

    pub async fn record_task_status(&self, event_id: u64) -> anyhow::Result<Value> {
        self.claim().await?;
        match self.tools.events_archive(event_id).await {
            Ok(Some(event)) => {
                self.commit(ForkExit::Recorded {
                    what: format!("archived task {event_id}"),
                })
                .await;
                Ok(json!({ "event_id": event.id, "status": "archived" }))
            }
            Ok(None) => {
                self.release().await;
                anyhow::bail!("event not found: {event_id}")
            }
            Err(error) => {
                self.release().await;
                Err(error)
            }
        }
    }

    pub async fn escalate(&self, brief: &str) -> anyhow::Result<Value> {
        let brief = brief.trim();
        if brief.is_empty() {
            anyhow::bail!("fork_escalate requires a non-empty brief");
        }
        self.claim().await?;
        self.inject_claimed(brief).await
    }

    pub async fn drop_message(&self, reason: &str) -> anyhow::Result<Value> {
        self.claim().await?;
        self.commit(ForkExit::Dropped {
            reason: reason.trim().to_string(),
        })
        .await;
        Ok(json!({ "status": "dropped" }))
    }

    /// Inject an already-claimed brief, committing only once the main Agent's
    /// queue has actually accepted it.
    async fn inject_claimed(&self, brief: &str) -> anyhow::Result<Value> {
        match self.sink.inject(&self.message, brief).await {
            Ok(()) => {
                self.commit(ForkExit::Escalated {
                    brief: brief.to_string(),
                })
                .await;
                Ok(json!({ "status": "escalated" }))
            }
            Err(error) => {
                // The brief never reached the queue, so no exit was taken and
                // the dispatcher will escalate the original message itself.
                self.release().await;
                Err(error)
            }
        }
    }

    /// A Record exit with nothing to anchor to.
    ///
    /// Events need an anchor chat message, and on a brand-new host the
    /// transcript is empty. The host does not mint one: a fork writing a chat
    /// line is a fork speaking to the Owner, which ADR-0015 forbids outright.
    /// So the record fails toward the main queue instead — the would-be record
    /// is escalated as a brief, and the Agent (who may speak to the Owner)
    /// decides what to write.
    async fn escalate_unanchored_record(
        &self,
        kind: &str,
        name: &str,
        description: &str,
        content_md: Option<&str>,
    ) -> anyhow::Result<Value> {
        self.claim().await?;
        let mut brief = format!(
            "A triage fork wanted to record this as {kind} but the transcript has no message to \
             anchor it to, so it is yours to write.\n\n{name}: {description}"
        );
        if let Some(content) = content_md.map(str::trim).filter(|text| !text.is_empty()) {
            brief.push_str("\n\n");
            brief.push_str(content);
        }
        self.inject_claimed(&brief).await
    }
}

fn exit_name(exit: &ForkExit) -> &'static str {
    match exit {
        ForkExit::Dropped { .. } => "drop",
        ForkExit::Recorded { .. } => "record",
        ForkExit::Escalated { .. } => "escalate",
    }
}

/// The lash-facing wrapper. Holds the definitions; delegates every call to
/// [`ForkTools`].
pub struct ForkToolProvider {
    tools: Arc<ForkTools>,
}

impl ForkToolProvider {
    pub fn new(tools: Arc<ForkTools>) -> Self {
        Self { tools }
    }
}

/// The fork's complete tool catalog. Kept as a free function so a test can
/// assert the surface without building a session.
pub fn fork_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::raw(
            "hirsel.fork_events_notify",
            "fork_record_info",
            "RECORD exit. Write one quiet info event stating the outcome in the Owner's terms. Never paste raw logs.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "description"],
                "properties": {
                    "name": { "type": "string", "minLength": 1 },
                    "description": { "type": "string", "minLength": 1 },
                    "content_md": { "type": ["string", "null"] }
                }
            }),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["event_id", "status"],
                "properties": {
                    "event_id": { "type": "integer" },
                    "status": { "const": "recorded" }
                }
            }),
        )
        .with_tool_binding(ToolBinding::new(["fork"], "record_info")),
        ToolDefinition::raw(
            "hirsel.fork_events_summary",
            "fork_record_summary",
            "RECORD exit. Write one digest summarising a settled outcome.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "description"],
                "properties": {
                    "name": { "type": "string", "minLength": 1 },
                    "description": { "type": "string", "minLength": 1 },
                    "content_md": { "type": ["string", "null"] }
                }
            }),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["event_id", "status"],
                "properties": {
                    "event_id": { "type": "integer" },
                    "status": { "const": "recorded" }
                }
            }),
        )
        .with_tool_binding(ToolBinding::new(["fork"], "record_summary")),
        ToolDefinition::raw(
            "hirsel.fork_events_archive",
            "fork_close_task",
            "RECORD exit. Close out a live Task or event this message settles. Pass an id from the live inventory in your context pack.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["event_id"],
                "properties": { "event_id": { "type": "integer" } }
            }),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["event_id", "status"],
                "properties": {
                    "event_id": { "type": "integer" },
                    "status": { "const": "archived" }
                }
            }),
        )
        .with_tool_binding(ToolBinding::new(["fork"], "close_task")),
        ToolDefinition::raw(
            "hirsel.fork_escalate",
            "fork_escalate",
            "ESCALATE exit. Inject one distilled brief into the main agent: what happened, the resulting state, and the open question. The main agent must not need to reread the event. Use this whenever you are unsure.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["brief"],
                "properties": { "brief": { "type": "string", "minLength": 1 } }
            }),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["status"],
                "properties": { "status": { "const": "escalated" } }
            }),
        )
        .with_tool_binding(ToolBinding::new(["fork"], "escalate")),
        ToolDefinition::raw(
            "hirsel.fork_drop",
            "fork_drop",
            "DROP exit. The message is already known, already handled, or only progress. Say why in one line and stop. Call this explicitly — ending the turn without an exit is treated as a failure and escalated.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["reason"],
                "properties": { "reason": { "type": "string", "minLength": 1 } }
            }),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["status"],
                "properties": { "status": { "const": "dropped" } }
            }),
        )
        .with_tool_binding(ToolBinding::new(["fork"], "drop")),
    ]
}

#[async_trait]
impl ToolProvider for ForkToolProvider {
    fn tool_manifests(&self) -> Vec<ToolManifest> {
        fork_tool_definitions()
            .iter()
            .map(ToolDefinition::manifest)
            .collect()
    }

    fn resolve_contract(&self, name: &str) -> Option<Arc<ToolContract>> {
        fork_tool_definitions()
            .into_iter()
            .find(|definition| definition.name() == name)
            .map(|definition| Arc::new(definition.contract()))
    }

    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        match self.dispatch(call).await {
            Ok(value) => ToolOutcome::ok(value),
            Err(error) => ToolOutcome::err_fmt(error),
        }
    }
}

impl ForkToolProvider {
    async fn dispatch(&self, call: ToolCall<'_>) -> anyhow::Result<Value> {
        let args = call.args;
        match call.name {
            "fork_record_info" => {
                self.tools
                    .record_info(
                        &required_str(args, "name")?,
                        &required_str(args, "description")?,
                        optional_str(args, "content_md"),
                    )
                    .await
            }
            "fork_record_summary" => {
                self.tools
                    .record_summary(
                        &required_str(args, "name")?,
                        &required_str(args, "description")?,
                        optional_str(args, "content_md"),
                    )
                    .await
            }
            "fork_close_task" => {
                let event_id = args
                    .get("event_id")
                    .and_then(Value::as_u64)
                    .context("fork_close_task requires an integer `event_id`")?;
                self.tools.record_task_status(event_id).await
            }
            "fork_escalate" => self.tools.escalate(&required_str(args, "brief")?).await,
            "fork_drop" => {
                self.tools
                    .drop_message(&required_str(args, "reason")?)
                    .await
            }
            other => anyhow::bail!(
                "a triage fork has no tool `{other}`; its only exits are fork_record_info, \
                 fork_record_summary, fork_close_task, fork_escalate and fork_drop"
            ),
        }
    }
}

fn required_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string field `{key}`"))
}

fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}
