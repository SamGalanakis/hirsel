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
            "hirsel.artifacts_create",
            "artifacts_create",
            "Explicitly publish a reusable artifact and insert its card in the current conversation. Solid artifacts are self-contained JSX modules exporting default App; imports limited to solid-js and @solidjs/web (Solid 2). HTML is self-contained. Files are UTF-8 text. All interaction is local: no network, backend or Hirsel tool access. Never create artifacts automatically from every output.",
            json!({"type":"object","additionalProperties":false,"required":["title","kind","content"],"properties":{"title":{"type":"string","minLength":1,"maxLength":200},"kind":{"type":"string","enum":["solid","html","file"]},"content":{"type":"string","minLength":1,"maxLength":1048576},"mime":{"type":"string"},"filename":{"type":"string"}}}),
            json!({"type":"object"}),
            ["artifacts"],
            "create",
        ),
        tool_definition(
            "hirsel.artifacts_edit",
            "artifacts_edit",
            "Edit a saved artifact in place using exact-match replacements. Each old_string must occur exactly once. The ID stays stable and every earlier reference opens the latest content; no revision history. Publishes a card in the current Thread. Use show to read current source before editing.",
            json!({"type":"object","additionalProperties":false,"required":["artifact_id","edits"],"properties":{"artifact_id":{"type":"integer","minimum":1},"title":{"type":"string","minLength":1,"maxLength":200},"edits":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"object","additionalProperties":false,"required":["old_string","new_string"],"properties":{"old_string":{"type":"string","minLength":1},"new_string":{"type":"string"}}}}}}),
            json!({"type":"object"}),
            ["artifacts"],
            "edit",
        ),
        tool_definition(
            "hirsel.artifacts_list",
            "artifacts_list",
            "List artifact summaries referenced in your own Thread or an authorized descendant. Backlinks include only visible Threads.",
            json!({"type":"object","additionalProperties":false,"properties":{"thread":thread_ref_schema()}}),
            json!({"type":"object"}),
            ["artifacts"],
            "list",
        ),
        tool_definition(
            "hirsel.artifacts_show",
            "artifacts_show",
            "Read an artifact's latest source and publish its card in the current conversation. Only already-authorized artifact content is available; publication records a reference in your Thread.",
            json!({"type":"object","additionalProperties":false,"required":["artifact_id"],"properties":{"artifact_id":{"type":"integer","minimum":1}}}),
            json!({"type":"object"}),
            ["artifacts"],
            "show",
        ),
        tool_definition(
            "hirsel.threads_context",
            "threads_context",
            "Read your identity, bounded ancestor names and this accepted turn assignment. No parent or peer transcript.",
            empty_object_input_schema(),
            json!({"type":"object"}),
            ["threads"],
            "context",
        ),
        tool_definition(
            "hirsel.threads_delegate",
            "threads_delegate",
            "Assign focused work to a new or existing direct child. Real asynchronous execution; durable results return upward. Parent completion does not abandon children.",
            crate::subagent_models::SubagentModelState::delegation_input_schema_for(
                subagent_models,
            ),
            json!({"type":"object"}),
            ["threads"],
            "delegate",
        ),
        tool_definition(
            "hirsel.threads_send",
            "threads_send",
            "Send a follow-up only to a direct child, using its stored execution backend. Returns its accepted turn identity.",
            thread_send_schema(),
            json!({"type":"object"}),
            ["threads"],
            "send",
        ),
        tool_definition(
            "hirsel.threads_report",
            "threads_report",
            "Report meaningful progress to the actual requesting parent. No recipient choice; terminal results are reported automatically.",
            thread_report_schema(),
            json!({"type":"object"}),
            ["threads"],
            "report",
        ),
        tool_definition(
            "hirsel.threads_cancel",
            "threads_cancel",
            "Cancel running or queued work in your own subtree. Never cancels a peer or changes settlement.",
            json!({"type":"object","additionalProperties":false,"properties":{"thread":thread_ref_schema()}}),
            json!({"type":"object"}),
            ["threads"],
            "cancel",
        ),
        tool_definition(
            "hirsel.threads_create",
            "threads_create",
            "Create durable work with its own conversation. Ordinary work needs no choices or notification kind. Reuse client_id on retries; every created Thread is visible immediately.",
            thread_create_schema(),
            thread_result_schema(),
            ["threads"],
            "create",
        ),
        tool_definition(
            "hirsel.threads_update",
            "threads_update",
            "Update an existing Thread title, icon, showcased artifact, description, generated instrument or attention from any wake. Identity and conversation are preserved. Reading or updating never settles it.",
            thread_update_schema(),
            thread_result_schema(),
            ["threads"],
            "update",
        ),
        tool_definition(
            "hirsel.threads_list",
            "threads_list",
            "List authorized descendants, direct children by default. Lifecycle is independent of hierarchy.",
            thread_list_schema(),
            json!({"type":"object","required":["threads"],"properties":{"threads":{"type":"array","items":{"type":"object"}}}}),
            ["threads"],
            "list",
        ),
        tool_definition(
            "hirsel.threads_read",
            "threads_read",
            "Read bounded own/subtree history. Follow next_cursor verbatim for older messages, turns and activities; null collection boundaries are exhausted. Current brief and Related references are independent of pagination. Use reference_url for ordinary Markdown links [label](reference_url); URLs never grant access.",
            thread_read_schema(),
            json!({"type":"object"}),
            ["threads"],
            "read",
        ),
        tool_definition(
            "hirsel.threads_add_related",
            "threads_add_related",
            "Explicitly save an HTTP(S) URL or an authorized Thread reference to this Thread or a descendant's Related list. Does not fetch the URL, create an artifact, grant access or start work. Use kind=thread for Thread targets. Read references with threads.read or threads.context.",
            thread_add_related_schema(),
            json!({"type":"object"}),
            ["threads"],
            "add_related",
        ),
        tool_definition(
            "hirsel.threads_remove_related",
            "threads_remove_related",
            "Remove a saved reference from this Thread or a descendant's Related list. Removing an absent reference is harmless.",
            thread_remove_related_schema(),
            json!({"type":"object"}),
            ["threads"],
            "remove_related",
        ),
        tool_definition(
            "hirsel.threads_activity",
            "threads_activity",
            "Append a factual update to an existing Thread. Activity does not create work, request attention, or settle it. Use threads.update when attention or the instrument changes.",
            thread_activity_schema(),
            json!({"type":"object","required":["activity"],"properties":{"activity":{"type":"object"}}}),
            ["threads"],
            "activity",
        ),
        tool_definition(
            "hirsel.views_show",
            "views_show",
            "Resolve and show a validated component view on the Canvas.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "template_id": { "type": "string", "minLength": 1 },
                    "spec": { "type": "object" },
                    "params": { "type": "object" },
                    "instance_id": { "type": "string", "minLength": 1 }
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
            "hirsel.monitors_create",
            "monitors_create",
            "Create a persisted host monitor that wakes the Agent when its condition fires. Monitors and timers are the way to watch for a condition instead of polling in-turn.",
            monitors_create_input_schema(),
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
