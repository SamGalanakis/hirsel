use std::sync::Weak;

use anyhow::Context;
use async_trait::async_trait;
use lash::tools::{
    StaticToolExecute, StaticToolProvider, ToolBinding, ToolCall, ToolDefinition,
    ToolDefinitionBindingExt, ToolOutcome,
};
use serde::Deserialize;
use serde_json::json;

use super::manager::SideChatManager;

#[derive(Clone)]
pub(super) struct ForkDecideExecutor {
    manager: Weak<SideChatManager>,
    sc: String,
    event_id: u64,
}

#[derive(Deserialize)]
struct ForkDecideArgs {
    choice: String,
}

pub(super) fn fork_decide_provider(
    manager: Weak<SideChatManager>,
    sc: String,
    event_id: u64,
) -> StaticToolProvider<ForkDecideExecutor> {
    let definition = ToolDefinition::raw(
        "hirsel.fork_decide",
        "fork_decide",
        "Decide the judgment seeded into this fork. Pass exactly one choice key from the event's optionList; the host resolves the event and closes this fork.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["choice"],
            "properties": {
                "choice": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The optionList choice key, such as A or B."
                }
            }
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["event_id", "choice", "status"],
            "properties": {
                "event_id": { "type": "integer" },
                "choice": { "type": "string" },
                "status": { "const": "done" }
            }
        }),
    )
    .with_tool_binding(ToolBinding::new(["fork"], "decide"));
    StaticToolProvider::new(
        vec![definition],
        ForkDecideExecutor {
            manager,
            sc,
            event_id,
        },
    )
}

#[async_trait]
impl StaticToolExecute for ForkDecideExecutor {
    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        let result = async {
            if call.name != "fork_decide" {
                anyhow::bail!("unknown fork tool: {}", call.name);
            }
            let args: ForkDecideArgs = serde_json::from_value(call.args.clone())
                .context("fork.decide requires {choice}")?;
            let manager = self
                .manager
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("fork manager is unavailable"))?;
            let event = manager
                .decide_event(self.event_id, &json!({ "choice": args.choice }))
                .await?
                .ok_or_else(|| anyhow::anyhow!("fork is no longer open: {}", self.sc))?;
            Ok::<_, anyhow::Error>(json!({
                "event_id": event.id,
                "choice": args.choice,
                "status": "done"
            }))
        }
        .await;
        match result {
            Ok(value) => ToolOutcome::ok(value),
            Err(error) => ToolOutcome::err_fmt(error),
        }
    }
}
