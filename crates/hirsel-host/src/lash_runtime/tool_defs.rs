use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentToolSurface {
    pub(super) fingerprint: String,
    pub(super) tool_names: Vec<String>,
}

pub(super) fn agent_tool_surface(
    definitions: &[ToolDefinition],
) -> anyhow::Result<AgentToolSurface> {
    agent_tool_surface_for_dialect(definitions, AGENT_RLM_DIALECT)
}

pub(super) fn agent_tool_surface_for_dialect(
    definitions: &[ToolDefinition],
    dialect: RlmDialect,
) -> anyhow::Result<AgentToolSurface> {
    let mut named_bindings = definitions
        .iter()
        .map(|definition| {
            let binding = ToolBinding::required_for_remote(&definition.manifest)
                .map_err(anyhow::Error::msg)?;
            Ok((
                format!(
                    "{}|{}|{}",
                    binding.authority_type,
                    binding.call_path(),
                    definition.manifest.name
                ),
                binding.call_path(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    named_bindings.sort();
    named_bindings.dedup();
    // The RLM dialect is part of the session's durable identity, not just the
    // tool surface: a dialect pin is recorded at first commit and cannot be
    // changed on a live session. Hashing it here makes a dialect switch rotate
    // the agent session through the same handoff-seed path a tool-surface
    // change already takes, by construction rather than by a second mechanism.
    let fingerprint_material = std::iter::once(format!("dialect:{}", dialect.language_id()))
        .chain(
            named_bindings
                .iter()
                .map(|(identity, _)| identity.to_string()),
        )
        .collect::<Vec<_>>()
        .join("\n");
    let fingerprint = format!("{:x}", Sha256::digest(fingerprint_material.as_bytes()));
    let mut tool_names = named_bindings
        .into_iter()
        .map(|(_, tool_name)| tool_name)
        .collect::<Vec<_>>();
    tool_names.sort();
    tool_names.dedup();
    Ok(AgentToolSurface {
        fingerprint,
        tool_names,
    })
}

pub(super) fn hirsel_tool_definitions(
    subagent_models: &SubagentModelCatalog,
) -> Vec<ToolDefinition> {
    vec![
        tool_definition(
            "hirsel.events_judgment",
            "events_judgment",
            "Emit a judgment Event when work reaches a taste boundary and needs Sam's decision. Valid example: events.judgment({ question: \"Where should canvas view state persist?\", context: \"Local persistence keeps the reopen path available after a host restart.\", options: [{ label: \"SQLite\", detail: \"Durable with the existing host store.\", recommended: true }, { label: \"Memory\", detail: \"Simpler, but state disappears on restart.\" }], unblocks: 2 }). Rules: context must add information beyond the question or be omitted; context \"Choose where canvas view state should persist.\" is rejected for question \"Where should canvas view state persist?\" because it only paraphrases it. Supply 2–4 options. Keys are optional and become A, B, C… in order; mark one recommendation explicitly, or the host recommends the first option.",
            events_judgment_input_schema(),
            event_send_output_schema("judgment"),
            ["events"],
            "judgment",
        ),
        tool_definition(
            "hirsel.events_notify",
            "events_notify",
            "Emit a quiet info Event for an FYI that belongs outside a warm Chat exchange.",
            events_notify_input_schema(),
            event_send_output_schema("info"),
            ["events"],
            "notify",
        ),
        tool_definition(
            "hirsel.events_summary",
            "events_summary",
            "Emit a summary Event for a digest, using either markdown content or a validated constrained-JSON UI tree.",
            events_summary_input_schema(),
            event_send_output_schema("summary"),
            ["events"],
            "summary",
        ),
        tool_definition(
            "hirsel.events_recompose",
            "events_recompose",
            "Recompose the exact open Task whose generated action woke this Agent turn. The Host preserves its identity and Anchor and validates the constrained UI. Never create a nested Task for the next stage.",
            events_recompose_input_schema(),
            events_recompose_output_schema(),
            ["events"],
            "recompose",
        ),
        tool_definition(
            "hirsel.events_archive",
            "events_archive",
            "Archive one finished Event so Sam's feed hides it. Archiving an open or snoozed Event also resolves it as dismissed.",
            event_archive_input_schema(),
            event_archive_output_schema(),
            ["events"],
            "archive",
        ),
        tool_definition(
            "hirsel.events_clear",
            "events_clear",
            "Clear Sam's feed by archiving every finished Event. Use events.clear when Sam asks to clear out or clear my feed; open judgments, including snoozed judgments, are kept until they receive a response.",
            empty_object_input_schema(),
            events_clear_output_schema(),
            ["events"],
            "clear",
        ),
        tool_definition(
            "hirsel.pings_send",
            "pings_send",
            "deprecated: use events.judgment / events.notify. Compatibility alias for emitting a judgment or info Event from the current Agent turn.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "description"],
                "properties": {
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 32,
                        "description": "Short event handle."
                    },
                    "description": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The judgment question shown as the card heading."
                    },
                    "content_md": {
                        "type": "string",
                        "description": "Optional judgment context. It must add stakes or constraints beyond the heading; omit it or pass an empty string when no context is needed."
                    },
                    "requires_response": { "type": "boolean", "default": true },
                    "view": { "type": ["object", "array", "null"] },
                    "unblocks": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Optional count of agents unblocked by this judgment."
                    },
                    "quick_replies": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 4,
                        "description": "Legacy judgment choices; the first is recommended. Prefer options.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["value", "label"],
                            "properties": {
                                "value": { "type": "string" },
                                "label": { "type": "string" }
                            }
                        }
                    },
                    "options": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 4,
                        "description": "Judgment choices. Keys are optional; when no recommendation is marked, the first option is recommended.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["label", "detail"],
                            "properties": {
                                "key": { "type": "string", "pattern": "^[A-Z]$" },
                                "label": { "type": "string", "minLength": 1 },
                                "detail": { "type": "string", "minLength": 1 },
                                "recommended": { "type": "boolean", "default": false }
                            }
                        }
                    }
                }
            }),
            pings_send_output_schema(),
            ["pings"],
            "send",
        ),
        tool_definition(
            "hirsel.pings_resolve",
            "pings_resolve",
            "Resolve a Ping that was overtaken by events.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["ping_id"],
                "properties": {
                    "ping_id": { "type": "integer", "minimum": 1 }
                }
            }),
            pings_resolve_output_schema(),
            ["pings"],
            "resolve",
        ),
        tool_definition(
            "hirsel.views_show",
            "views_show",
            "Resolve and show a validated component view in canvas, chat, or a Ping.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["placement"],
                "properties": {
                    "template_id": { "type": "string", "minLength": 1 },
                    "spec": { "type": "object" },
                    "params": { "type": "object" },
                    "instance_id": { "type": "string", "minLength": 1 },
                    "placement": {
                        "type": "string",
                        "pattern": "^(canvas|chat|ping:[1-9][0-9]*)$"
                    }
                },
                "oneOf": [
                    { "required": ["template_id"], "not": { "required": ["spec"] } },
                    { "required": ["spec"], "not": { "required": ["template_id"] } }
                ]
            }),
            view_instance_output_schema(),
            ["views"],
            "show",
        ),
        tool_definition(
            "hirsel.views_update",
            "views_update",
            "Update an active view by merging params and/or applying RFC 6902 JSON Patch.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["instance_id"],
                "properties": {
                    "instance_id": { "type": "string", "minLength": 1 },
                    "params": { "type": "object" },
                    "patch": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["op", "path"],
                            "properties": {
                                "op": {
                                    "type": "string",
                                    "enum": ["add", "remove", "replace", "move", "copy", "test"]
                                },
                                "path": { "type": "string" },
                                "from": { "type": "string" },
                                "value": true
                            }
                        }
                    }
                },
                "anyOf": [
                    { "required": ["params"] },
                    { "required": ["patch"] }
                ]
            }),
            view_instance_output_schema(),
            ["views"],
            "update",
        ),
        tool_definition(
            "hirsel.views_clear",
            "views_clear",
            "Remove an active component view.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["instance_id"],
                "properties": {
                    "instance_id": { "type": "string", "minLength": 1 }
                }
            }),
            view_clear_output_schema(),
            ["views"],
            "clear",
        ),
        tool_definition(
            "hirsel.views_list_templates",
            "views_list_templates",
            "List the file-based view templates currently available to the Agent.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            views_list_templates_output_schema(),
            ["views"],
            "list_templates",
        ),
        tool_definition(
            "hirsel.subagents_spawn",
            "subagents_spawn",
            "Start a Claude or Codex Sub-agent as a Lash Runtime Process. Call this after any required subagents.list check, then make the turn's Chat output a concise hand-off note. Do not wait or poll for completion in the same turn; the terminal event will wake you later.",
            SubagentModelState::spawn_input_schema_for(subagent_models),
            subagents_spawn_output_schema(),
            ["subagents"],
            "spawn",
        ),
        tool_definition(
            "hirsel.subagents_prompt",
            "subagents_prompt",
            "Send follow-up input to a running Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id", "text"],
                "properties": {
                    "process_id": { "type": "string" },
                    "text": { "type": "string" }
                }
            }),
            acknowledgement_output_schema(),
            ["subagents"],
            "prompt",
        ),
        tool_definition(
            "hirsel.subagents_interrupt",
            "subagents_interrupt",
            "Request interruption of a running Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            acknowledgement_output_schema(),
            ["subagents"],
            "interrupt",
        ),
        tool_definition(
            "hirsel.subagents_list",
            "subagents_list",
            "List known Sub-agent processes.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            subagents_list_output_schema(),
            ["subagents"],
            "list",
        ),
        tool_definition(
            "hirsel.subagents_progress",
            "subagents_progress",
            "Read recent progress events for a Sub-agent process.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            subagents_progress_output_schema(),
            ["subagents"],
            "progress",
        ),
        tool_definition(
            "hirsel.subagents_wait",
            "subagents_wait",
            "Wait for a Sub-agent process to reach a terminal outcome — for short waits only; for anything longer, end your turn and let the terminal event wake you.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["process_id"],
                "properties": {
                    "process_id": { "type": "string" }
                }
            }),
            subagents_wait_output_schema(),
            ["subagents"],
            "wait",
        ),
        tool_definition(
            "hirsel.monitors_create",
            "monitors_create",
            "Create a persisted host monitor that wakes the Agent when its condition fires. Monitors and timers are the way to watch for a condition instead of polling in-turn.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["cmd", "wake_on", "label"],
                "properties": {
                    "cmd": { "type": "string" },
                    "every_secs": { "type": "integer", "minimum": 30 },
                    "wake_on": {
                        "type": "string",
                        "enum": ["changed", "exit_zero", "exit_nonzero", "regex"]
                    },
                    "pattern": { "type": "string" },
                    "label": { "type": "string" }
                }
            }),
            monitors_create_output_schema(),
            ["monitors"],
            "create",
        ),
        tool_definition(
            "hirsel.monitors_list",
            "monitors_list",
            "List persisted host monitors.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            }),
            monitors_list_output_schema(),
            ["monitors"],
            "list",
        ),
        tool_definition(
            "hirsel.monitors_cancel",
            "monitors_cancel",
            "Cancel a persisted host monitor.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["monitor_id"],
                "properties": {
                    "monitor_id": { "type": "string" }
                }
            }),
            monitors_cancel_output_schema(),
            ["monitors"],
            "cancel",
        ),
        tool_definition(
            "hirsel.shell_run",
            "shell_run",
            "Run a bounded shell command and return stdout, stderr, status, and timeout state. For quick commands only (seconds); anything slow or watch-like goes to a Sub-agent or monitor with a wake — do not wait in-turn.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["cmd"],
                "properties": {
                    "cmd": { "type": "string" },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 600 }
                }
            }),
            shell_run_output_schema(),
            ["shell"],
            "run",
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
pub(super) fn tool_definition(
    id: &str,
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
    module_path: impl IntoIterator<Item = &'static str>,
    operation: &str,
) -> ToolDefinition {
    ToolDefinition::raw(id, name, description, input_schema, output_schema)
        .with_tool_binding(ToolBinding::new(module_path, operation))
}
