# C11-DRIVER-SHARED

## Verdict

One materially useful fix is recommended. No second finding is promoted. The
source checkout was inspected read-only at the expected snapshot:

- `HEAD`: `3ee0621a603659ab0168f565b99012b642415419`
- `HEAD^{tree}`: `a4aac830c45398a66591f2c44b707aaf3cef281b`
- pre-report `git status --porcelain`: empty

## Finding 1 — timed-out shell executions discard real stderr

**Verdict:** Recommend. **Confidence:** high. **Reachability:** normal
`hirsel.shell_run` execution path; not a hypothetical or test-only state.

### Exact evidence across the affected layers

The owned projection overwrites raw stderr whenever the timeout bit is set:

```text
crates/hirsel-host/src/tools/shell.rs:21-31
pub(crate) fn shell_output(output: crate::process_run::BashCommandOutput) -> ShellRunOutput {
    ShellRunOutput {
        status: output.status,
        stdout: truncate_output(String::from_utf8_lossy(&output.stdout)),
        stderr: if output.timed_out {
            "command timed out".to_string()
        } else {
            truncate_output(String::from_utf8_lossy(&output.stderr))
        },
        timed_out: output.timed_out,
    }
}
```

The adjacent producer retains the bytes already read before killing the
process group, including stderr:

```text
crates/hirsel-host/src/process_run.rs:83-92
Err(_) => {
    kill_process_group(self.pgid);
    let _ = timeout(Duration::from_secs(5), self.child.wait()).await;
    self.pgid = 0;
    Ok(BashCommandOutput {
        status: None,
        stdout: out,
        stderr: err,
        timed_out: true,
    })
}
```

The actual scoped MCP path reaches the lossy conversion after the timeout
finish:

```text
crates/hirsel-host/src/lash_runtime/scoped_tools.rs:76-98
let running = crate::process_run::start_bash_command(...)?;
drop(guard);
let output = running.finish(Duration::from_secs(...)).await?;
let output = crate::tools::shell::shell_output(output);
...
shell_run_result(&output)
```

The public result representation and wire contract explicitly expose both
fields:

```text
crates/hirsel-host/src/tools.rs:50-56
pub struct ShellRunOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

crates/hirsel-host/src/lash_runtime/tool_schemas.rs:118-129
required: ["status", "stdout", "stderr", "timed_out"]
```

The tool description promises stdout, stderr, status, and timeout state
(`crates/hirsel-host/src/lash_runtime/tool_defs.rs:359-376`). The summary
consumer already treats `timed_out` as the timeout truth and does not need the
sentinel in stderr (`crates/hirsel-host/src/lash_runtime/condense.rs:84-98`):

```text
if payload
    .get("timed_out")
    .and_then(Value::as_bool)
    .unwrap_or(false)
{
    return Some("timed out".to_string());
}
```

### Concrete state, write path, and test evidence

A command that writes `partial-error` to stderr and then exceeds its timeout
produces a reachable raw state with `timed_out: true` and
`stderr: b"partial-error"`. The existing process-layer test establishes that
state without asserting any live external value:

```text
crates/hirsel-host/src/process_run.rs:121-139
printf partial-output; printf partial-error >&2; ... sleep 999 ...
assert!(output.timed_out);
assert_eq!(output.stderr, b"partial-error");
```

The owned conversion then changes that valid state to
`ShellRunOutput { stderr: "command timed out", timed_out: true }`, losing the
diagnostic bytes. This is a duplicate/overloaded truth problem rather than an
invalid `status` combination: timeout is represented once by `timed_out`, but
the projection invents a second timeout marker in `stderr` and discards the
producer's stderr. There is no second raw writer that needs reconciling.

The current host fixture uses the sentinel itself and therefore does not catch
the loss:

```text
crates/hirsel-host/src/lash_runtime/tests.rs:751-756
ShellRunOutput {
    status: None,
    stdout: String::new(),
    stderr: "timed out".to_string(),
    timed_out: true,
}
```

Reproducible consumer search and static result counts:

```text
$ rg -n -g '*.rs' 'crate::tools::shell::shell_output\s*\(' crates/hirsel-host/src
crates/hirsel-host/src/lash_runtime/scoped_tools.rs:91: let output = crate::tools::shell::shell_output(output);
```

That query has **1** cross-module consumer. The broader query
`rg -n -g '*.rs' 'shell_output\s*\(' crates/hirsel-host/src` has **3**
matches: the owned definition, the `ToolSuite::shell_run` call at
`tools/shell.rs:17`, and the scoped MCP call at `scoped_tools.rs:91`.

### Smallest target and affected representations

Keep the existing representations at every layer; no DDL or persistence
shape is involved:

1. `BashCommandOutput` (`process_run.rs`) remains raw bytes plus
   `timed_out`.
2. `ShellRunOutput` (`tools.rs`) keeps the same four fields.
3. Change only `shell_output` (`tools/shell.rs:21-31`) so stderr always uses
   the existing `truncate_output(String::from_utf8_lossy(&output.stderr))`;
   leave `timed_out` as the sole timeout owner.
4. Keep the JSON schema and tool result shape unchanged. Add a focused unit
   case beside `shell_output` (or its existing host test module) with raw
   stderr plus `timed_out: true`; assert stderr survives the normal 16 KiB
   cap and the timeout bit remains true.

This removes the duplicate timeout meaning without broadening output limits or
changing non-timeout behavior. The only cutover risk is callers or fixtures
that compare the exact `"command timed out"` stderr sentinel; those should
assert `timed_out` and, where applicable, the actual stderr instead. The
existing `condense` path shows the normal user-facing timeout summary remains
unchanged. The 16 KiB truncation still bounds the exposed stderr.

Validation identified but intentionally **not run** in this worker: the
existing `process_run::timeout_kills_the_spawned_process_group` regression
test, a new `shell_output` projection test for preserved timeout stderr, and
the host shell-result serialization/summary tests. No build, test, provider,
or live-data command was executed.

## Coverage and explicit skips

Every whole-file owner was read in full. The shared exact-definition ownership
list is empty; adjacent files below were read only as consumers or producers.

| Owned file | Definitions/behavior inspected | Result |
|---|---|---|
| `crates/hirsel-drivers/src/fake.rs` | `FakeDriver`, `FakeSession`, `FakeFixture`, defaults, spawn/prompt/interrupt/retire/events | No additional finding; terminal and fixture output are bounded at the shared conversion points. |
| `crates/hirsel-drivers/src/lib.rs` | Module declarations and public re-exports | No logic or representation to simplify. |
| `crates/hirsel-drivers/src/shared.rs` | `SessionRegistry`, `EventHub`/`EventState`, stream replay, completion race, formatting, environment sanitization, process groups, JSON lines, stderr drain | No promoted second finding. The retained `Vec<SubagentEvent>` is unbounded, but authoritative retained-log replay is explicitly deliberate (`shared.rs:128-143`); bounding it would change slow/late progress semantics without a stated retention contract. Process-group ownership and descendant cleanup have direct tests and no distinct source-backed regression. |
| `crates/hirsel-drivers/src/types.rs` | `DriverError`, `AgentKind`, `ScopedMcpLaunch` validation/args, `SpawnSpec`, `SessionHandle`, event/outcome enums, `SubagentDriver` | No material invalid state promoted. `SpawnSpec.agent`/`SessionHandle.agent` are not read by production registry operations; the driver instance owns provider routing. `expected_tools` is a validated `Vec` whose provider comparisons intentionally become sets. |
| `crates/hirsel-host/src/tools/shell.rs` | `ToolSuite::shell_run`, `shell_output`, byte-safe output truncation | Finding 1. |
| `crates/hirsel-drivers/fixtures/scoped_mcp.py` | Offline bridge argument parsing, sidecar config, request/response and pagination/call fixture behavior | Test fixture only; it does not read host capability contents or own authority. No production finding. |
| `crates/hirsel-drivers/src/shared_tests.rs` | launch validation/redaction, bridge args, serde, environment cleanup, lag/late replay, completion races, duplicate output, fake controls, offline MCP pagination | Tests demonstrate the intended exactly-once and full-output behavior; no test was changed or run. The timeout projection gap is outside these driver tests. |
| `crates/hirsel-drivers/src/test_support.rs` | Scoped MCP fixture and launch constructors | Structural test support only; no independent state owner. |
| `crates/hirsel-drivers/src/tests.rs` | fake lifecycle/interrupt/retire/replay, CLI stderr draining, ignored real-CLI smoke helpers | Existing coverage supports process-group and lifecycle conclusions; no additional source-backed defect. |

Read-only adjacent context included `claude.rs`, `claude_config.rs`,
`claude_events.rs`, `codex.rs`, `codex_config.rs`, `codex_io.rs`,
`hirsel-host/src/process_run.rs`, `scoped_tools.rs`, `tools.rs`,
`cli_turn.rs`, `tool_results.rs`, `tool_schemas.rs`, `tool_defs.rs`,
`condense.rs`, and relevant host tests. Full assistant output versus bounded
terminal summaries was not reported: `types.rs:133-135` explicitly makes
those independent, and the existing 30,000-character tests exercise both
representations. Existing tracked outcomes and excluded native artifact
viewer/thread dimensions were not re-reported.

## Handoff

Source remained unchanged during the audit. Pre-report snapshot and clean
status are recorded above; the post-report snapshot/status must match the
same `HEAD`, tree, and empty source status because this report is outside the
checkout. No tests or builds were run.

Fix first: preserve raw stderr in `shell_output` and use `timed_out` as the
sole timeout indicator.
