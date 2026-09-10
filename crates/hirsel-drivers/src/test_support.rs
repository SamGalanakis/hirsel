//! Structurally valid launch data for pure unit tests and FakeDriver only.
//! Real CLI fixtures must replace these with their own executable/IPC paths.

use crate::ScopedMcpLaunch;

pub(crate) const MCP_FIXTURE_SOURCE: &str = include_str!("../fixtures/scoped_mcp.py");

/// Installs an offline executable MCP peer in a caller-owned temporary directory.
pub(crate) fn scoped_mcp_fixture(dir: &std::path::Path, tools: &[&str]) -> ScopedMcpLaunch {
    use std::os::unix::fs::PermissionsExt;
    let executable = dir.join("scoped_mcp.py");
    std::fs::write(&executable, MCP_FIXTURE_SOURCE).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let schemas = tools
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name, "description": "Offline driver fixture tool",
                "inputSchema": {"type":"object", "properties":{}, "additionalProperties":false},
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        dir.join("scoped_mcp.json"),
        serde_json::to_vec(&serde_json::json!({
            "tools": schemas,
        }))
        .unwrap(),
    )
    .unwrap();
    ScopedMcpLaunch {
        host_executable: executable,
        socket_path: dir.join("fixture.sock"),
        capability_file: dir.join("unread-capability"),
        expected_tools: tools.iter().map(|name| (*name).to_owned()).collect(),
    }
}

pub(crate) fn scoped_launch() -> ScopedMcpLaunch {
    ScopedMcpLaunch {
        host_executable: "/hirsel-test-only/host".into(),
        socket_path: "/hirsel-test-only/bridge.sock".into(),
        capability_file: "/hirsel-test-only/capability".into(),
        expected_tools: vec!["threads_context".into(), "threads_delegate".into()],
    }
}
