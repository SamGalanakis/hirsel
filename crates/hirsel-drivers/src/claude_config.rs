//! Explicit Claude execution configuration and a model-free scoped MCP preflight.
use super::*;
use crate::ScopedMcpLaunch;
use std::collections::BTreeSet;

// Native delegation/work inventory would create a second execution namespace.
// Keep the default coding tool surface; exclude only independent coordination.
pub(super) const FORBIDDEN_TOOLS: &[&str] = &[
    "Agent",
    "Task",
    "TaskCreate",
    "TaskList",
    "TaskGet",
    "TaskUpdate",
    "TaskStop",
    "TaskOutput",
    "SendMessage",
    "TeamCreate",
    "TeamDelete",
    "CronCreate",
    "CronDelete",
    "CronList",
];

pub(super) fn configure(command: &mut Command, task: &SpawnSpec) -> DriverResult<()> {
    task.scoped_mcp.validate()?;
    let args: Vec<_> = task
        .scoped_mcp
        .bridge_args()
        .into_iter()
        .map(|s| {
            s.into_string()
                .map_err(|_| DriverError::Protocol("Claude bridge path is not UTF-8".into()))
        })
        .collect::<DriverResult<_>>()?;
    let executable = task
        .scoped_mcp
        .host_executable
        .to_str()
        .ok_or_else(|| DriverError::Protocol("Claude bridge executable is not UTF-8".into()))?;
    let mcp = json!({"mcpServers":{"hirsel":{"type":"stdio","command":executable,"args":args}}});
    command
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(mcp.to_string())
        .arg("--setting-sources")
        .arg("")
        .arg("--settings")
        .arg(json!({"disableAllHooks":true,"autoMemoryEnabled":false}).to_string())
        .arg("--tools")
        .arg("default")
        .arg("--disallowedTools")
        .arg(FORBIDDEN_TOOLS.join(","))
        .arg("--allowedTools")
        .arg("mcp__hirsel__*")
        .arg("--disable-slash-commands")
        .arg("--no-chrome")
        .arg("--no-session-persistence")
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1")
        .env("CLAUDE_CODE_DISABLE_CRON", "1")
        .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
        .env("ENABLE_TOOL_SEARCH", "false")
        // Do not let a parent Claude/IDE launch inject its integration context.
        .env_remove("CLAUDE_CODE_SIMPLE")
        .env_remove("CLAUDE_CODE_SAFE_MODE")
        .env_remove("CLAUDE_CODE_SHELL_PREFIX")
        .env_remove("CLAUDE_CODE_IDE_HOST_OVERRIDE")
        .env_remove("CLAUDE_CODE_SSE_PORT")
        .env_remove("CLAUDE_CODE_SSE_TOKEN")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("CLAUDECODE");
    Ok(())
}

/// Inspect only the supplied bridge, never any ambient MCP registry or credential.
/// This precedes the initial Claude prompt. Claude's later init is a second check,
/// not a claim that its system/init message gates model admission.
pub(super) async fn preflight(launch: &ScopedMcpLaunch, limit: Duration) -> DriverResult<()> {
    launch.validate()?;
    let mut command = Command::new(&launch.host_executable);
    command
        .args(launch.bridge_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    start_in_process_group(&mut command);
    let mut child = command.spawn()?;
    let group = ProcessGroup::new(child.id().map(|id| id as i32).unwrap_or_default());
    let mut stdin = child
        .stdin
        .take()
        .ok_or(DriverError::MissingPipe("bridge stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or(DriverError::MissingPipe("bridge stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(DriverError::MissingPipe("bridge stderr"))?;
    let drain = tokio::spawn(drain_stderr(stderr));
    let mut lines = BufReader::new(stdout).lines();
    let result = timeout(limit, async {
        write_json_line(&mut stdin, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"hirsel-driver-preflight","version":"1"}}})).await?;
        let initialized = response(&mut lines, 1).await?;
        if !matches!(initialized.get("protocolVersion").and_then(Value::as_str), Some("2024-11-05" | "2025-03-26" | "2025-06-18")) || !initialized["capabilities"]["tools"].is_object() {
            return Err(DriverError::Protocol("scoped bridge did not negotiate MCP tools".into()));
        }
        write_json_line(&mut stdin, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await?;
        let mut actual = BTreeSet::new();
        let mut cursor: Option<String> = None;
        for page in 0..100 {
            let id = page + 2;
            let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            write_json_line(&mut stdin, &json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":params})).await?;
            let result = response(&mut lines, id).await?;
            let tools = result["tools"].as_array().ok_or_else(|| DriverError::Protocol("scoped bridge omitted tools".into()))?;
            for tool in tools {
                let name = tool["name"].as_str().filter(|name| !name.is_empty()).ok_or_else(|| DriverError::Protocol("invalid scoped tool name".into()))?;
                if !actual.insert(name.to_string()) || !tool["inputSchema"].is_object() {
                    return Err(DriverError::Protocol("invalid or duplicate scoped tool".into()));
                }
            }
            match result.get("nextCursor") {
                None | Some(Value::Null) => {
                    let expected: BTreeSet<_> = launch.expected_tools.iter().cloned().collect();
                    return if actual == expected { Ok(()) } else { Err(DriverError::Protocol("scoped bridge catalog differs from host expectation".into())) };
                }
                Some(Value::String(next)) if !next.is_empty() && cursor.as_ref() != Some(next) => cursor = Some(next.clone()),
                _ => return Err(DriverError::Protocol("invalid scoped bridge pagination".into())),
            }
        }
        Err(DriverError::Protocol("scoped bridge catalog exceeds pagination bound".into()))
    }).await.unwrap_or_else(|_| Err(DriverError::RequestTimeout("scoped Claude MCP preflight".into())));
    group.kill_group();
    drop(stdin);
    let _ = timeout(DRAIN_GRACE, child.wait()).await;
    drain.abort();
    result
}

async fn response(
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    id: u64,
) -> DriverResult<Value> {
    loop {
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| DriverError::Protocol("scoped bridge closed during preflight".into()))?;
        let value: Value = serde_json::from_str(&line)?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| DriverError::Protocol("scoped bridge rejected preflight".into()));
        }
        if value.get("id").is_some() || value.get("method").is_none() {
            return Err(DriverError::Protocol(
                "uncorrelated scoped bridge response".into(),
            ));
        }
    }
}
