//! Invocation-local connector exclusion and exact scoped MCP catalog verification.
use super::*;
use crate::types::ScopedMcpLaunch;

const ISOLATION: &[&str] = &[
    "features.apps",
    "features.plugins",
    "features.multi_agent",
    "features.multi_agent_v2",
    "agents.enabled",
    "features.memories",
    "memories.use_memories",
    "memories.generate_memories",
];

pub(super) fn configure_command(command: &mut Command, task: &SpawnSpec) {
    command.arg("app-server").arg("--stdio");
    for key in ISOLATION {
        command.arg("-c").arg(format!("{key}=false"));
    }
    if let Some(model) = &task.model {
        command.arg("-c").arg(format!("model={}", json!(model)));
    }
    if let Some(variant) = &task.variant {
        command
            .arg("-c")
            .arg(format!("model_reasoning_effort={}", json!(variant)));
    }
}

pub(super) fn validate_launch(task: &SpawnSpec) -> DriverResult<()> {
    task.scoped_mcp.validate()?;
    let launch = &task.scoped_mcp;
    if !task.cwd.is_absolute()
        || [
            &task.cwd,
            &launch.host_executable,
            &launch.socket_path,
            &launch.capability_file,
        ]
        .iter()
        .any(|path| path.to_str().is_none())
    {
        return Err(protocol_error(
            "Codex scoped execution requires absolute UTF-8 paths",
        ));
    }
    Ok(())
}

fn names_in_config(value: &Value, names: &mut BTreeSet<String>) -> DriverResult<()> {
    let config = value
        .as_object()
        .ok_or_else(|| protocol_error("invalid Codex config object"))?;
    if let Some(servers) = config.get("mcp_servers") {
        let servers = servers
            .as_object()
            .ok_or_else(|| protocol_error("invalid Codex MCP config"))?;
        for name in servers.keys() {
            if name.is_empty() || name.chars().any(char::is_control) {
                return Err(protocol_error("invalid Codex MCP server name"));
            }
            names.insert(name.clone());
        }
    }
    if names.len() > 256 {
        return Err(protocol_error(
            "Codex MCP inventory exceeds scoped launch bound",
        ));
    }
    Ok(())
}

pub(super) fn inherited_names(response: &Value) -> DriverResult<BTreeSet<String>> {
    let config = response
        .get("config")
        .ok_or_else(|| protocol_error("missing Codex effective config"))?;
    for key in ISOLATION {
        let pointer = format!("/{}", key.replace('.', "/"));
        let value = config.pointer(&pointer);
        let disabled = value.and_then(Value::as_bool) == Some(false)
            || (key.starts_with("features.")
                && value
                    .and_then(|v| v.get("enabled"))
                    .and_then(Value::as_bool)
                    == Some(false));
        if !disabled {
            return Err(protocol_error(format!(
                "Codex scoped launch could not disable {key}"
            )));
        }
    }
    let mut names = BTreeSet::new();
    names_in_config(config, &mut names)?;
    let layers = response
        .get("layers")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_error("Codex scoped launch requires all config layers"))?;
    if layers.len() > 256 {
        return Err(protocol_error(
            "Codex config layers exceed scoped launch bound",
        ));
    }
    for layer in layers {
        // Disabled project layers can become trusted at thread/start.
        names_in_config(
            layer
                .get("config")
                .ok_or_else(|| protocol_error("missing Codex layer config"))?,
            &mut names,
        )?;
    }
    Ok(names)
}

pub(super) fn thread_start_request(
    task: &SpawnSpec,
    inherited: &BTreeSet<String>,
    bridge: &str,
) -> Value {
    let mut config = serde_json::Map::new();
    for key in ISOLATION {
        config.insert((*key).into(), json!(false));
    }
    // Codex splits dotted override keys literally; preserve arbitrary server
    // names as object keys instead of interpolating them into a key path.
    let mut servers = inherited
        .iter()
        .map(|name| (name.clone(), json!({"enabled":false})))
        .collect::<serde_json::Map<_, _>>();
    let launch = &task.scoped_mcp;
    servers.insert(bridge.to_owned(), json!({
        "command":launch.host_executable,
        "args":launch.bridge_args().iter().map(|arg| arg.to_str().expect("validated UTF-8 path")).collect::<Vec<_>>(),
        "enabled":true, "required":true, "default_tools_approval_mode":"auto"
    }));
    config.insert("mcp_servers".into(), Value::Object(servers));
    json!({"method":"thread/start", "params":{
        "cwd":task.cwd, "runtimeWorkspaceRoots":[task.cwd], "approvalPolicy":"never",
        "sandbox":"danger-full-access", "threadSource":"hirsel", "config":config
    }})
}

pub(super) async fn verify_catalog(
    session: &CodexSession,
    thread: &str,
    bridge: &str,
    launch: &ScopedMcpLaunch,
) -> DriverResult<()> {
    timeout(session.control_timeout, async {
        loop {
            let mut cursor: Option<String> = None;
            let mut cursors = BTreeSet::new();
            let mut servers = BTreeSet::new();
            let mut ready = false;
            let mut bridge_seen = false;
            loop {
                let response = session.request("mcpServerStatus/list", json!({"method":"mcpServerStatus/list", "params":{
                    "threadId":thread, "detail":"toolsAndAuthOnly", "limit":100, "cursor":cursor
                }})).await?;
                let rows = response.get("data").and_then(Value::as_array)
                    .ok_or_else(|| protocol_error("missing Codex MCP status inventory"))?;
                for row in rows {
                    let name = row.get("name").and_then(Value::as_str).ok_or_else(|| protocol_error("invalid Codex MCP status name"))?;
                    if !servers.insert(name.to_owned()) || servers.len() > 257 {
                        return Err(protocol_error("duplicate or oversized Codex MCP inventory"));
                    }
                    let tools = row.get("tools").and_then(Value::as_object).ok_or_else(|| protocol_error("invalid Codex MCP tool inventory"))?;
                    let status = row.get("runtimeStatus").and_then(Value::as_str);
                    if name != bridge {
                        if status != Some("disabled") || !tools.is_empty() {
                            return Err(protocol_error("Codex exposed an inherited connector"));
                        }
                        continue;
                    }
                    bridge_seen = true;
                    match status {
                        Some("starting" | "notStarted") if tools.is_empty() => continue,
                        Some("connected") => {},
                        _ => return Err(protocol_error("Codex scoped MCP bridge is unavailable")),
                    }
                    let names = tools.values().map(|tool| tool.get("name").and_then(Value::as_str)
                        .ok_or_else(|| protocol_error("invalid scoped MCP tool name")))
                        .collect::<DriverResult<Vec<_>>>()?;
                    let actual = names.iter().copied().collect::<BTreeSet<_>>();
                    let expected = launch.expected_tools.iter().map(String::as_str).collect::<BTreeSet<_>>();
                    if actual.len() != names.len() || actual != expected {
                        return Err(protocol_error("Codex scoped MCP catalog differs from host catalog"));
                    }
                    ready = true;
                }
                match response.get("nextCursor") {
                    None | Some(Value::Null) => break,
                    Some(Value::String(next)) if !next.is_empty() && cursors.insert(next.clone()) && cursors.len() <= 256 => cursor = Some(next.clone()),
                    _ => return Err(protocol_error("invalid Codex MCP pagination cursor")),
                }
            }
            if ready { return Ok(()); }
            if !bridge_seen { return Err(protocol_error("Codex scoped MCP bridge missing from inventory")); }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.map_err(|_| DriverError::RequestTimeout("Codex scoped MCP startup".into()))?
}
