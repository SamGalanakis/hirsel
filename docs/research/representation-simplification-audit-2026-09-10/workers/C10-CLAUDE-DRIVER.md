# C10-CLAUDE-DRIVER — Native Claude Code session, event correlation and process control

## Ranked findings

1. **C10-F1 — session identity is not an invariant for every Claude frame that can settle or publish state.** `claude.rs` classifies user frames as request receipts before `claude_events.rs` parses them, while the user-tool-result and rate-limit branches emit `SubagentEvent::Progress` without `check_session`. A frame from a foreign session, or a session-bound frame before initialization, can therefore acknowledge a pending input or write timeline activity for the wrong run. **Recommend; high confidence in the invalid state, medium materiality because the normal trusted CLI path does not currently emit foreign frames.**
2. **C10-F2 — the second scoped MCP catalog check loses duplicate-tool information.** Preflight rejects duplicate raw tool names, but the Claude `system/init` conversion collects prefixed names into a `BTreeSet` and accepts a duplicate inventory. **Recommend; high confidence in the representation inconsistency, medium materiality because valid bridge output is already unique and the defect is latent at the provider boundary.**

## C10-F1 — split event classification permits foreign or pre-init progress/receipts

### Verdict and exact evidence

**Verdict: recommend a small private parser/state-machine cutover.** The owned parser stores one accepted `session_id` and has a validator:

> `crates/hirsel-drivers/src/claude_events.rs:6-10`
> ```rust
> pub(super) struct ClaudeOutput {
>     expected: BTreeSet<String>,
>     session_id: Option<String>,
>     assistant: Option<String>,
> }
> ```

> `crates/hirsel-drivers/src/claude_events.rs:110-147`
> ```rust
> Some("assistant") => {
>     self.check_session(value)?;
>     ...
> }
> Some("stream_event") => {
>     self.check_session(value)?;
>     ...
> }
> Some("user") => {
>     if let Some(summary) = claude_tool_result(value) {
>         events.emit(SubagentEvent::Progress { summary })?;
>     }
> }
> Some("result") => {
>     self.check_session(value)?;
>     ...
> }
> ```

> `crates/hirsel-drivers/src/claude_events.rs:158-177`
> ```rust
> Some("rate_limit_event") => events.emit(SubagentEvent::Progress {
>     summary: "claude rate limit status updated".into(),
> })?,
> ...
> fn check_session(&self, value: &Value) -> DriverResult<()> {
>     let Some(id) = self.session_id.as_deref() else {
>         return Err(DriverError::Protocol(
>             "Claude output before scoped initialization".into(),
>         ));
>     };
>     if value.get("session_id").and_then(Value::as_str) != Some(id) {
>         return Err(DriverError::Protocol(
>             "Claude output belongs to another session".into(),
>         ));
>     }
>     Ok(())
> }
> ```

The validator is correct where called, but the `user` and `rate_limit_event` branches bypass it. The outer driver independently interprets the same raw frame as a receipt:

> `crates/hirsel-drivers/src/claude.rs:158-195`
> ```rust
> let receipt = if kind == Some("control_response") {
>     ...
> } else if kind == Some("user") && value.get("parent_tool_use_id").is_none_or(Value::is_null)
> {
>     value
>         .get("uuid")
>         .and_then(Value::as_str)
>         .map(|id| (id, true, Ok(())))
> } else {
>     None
> };
> ...
> if let Some((id, input, result)) = receipt
>     && let Ok(mut pending) = self.pending.lock()
>     && pending
>         .get(id)
>         .is_some_and(|request| request.input == input)
>     && let Some(request) = pending.remove(id)
> {
>     let _ = request.sender.send(result);
> }
> let result = lock(&self.output).and_then(|mut output| output.handle(value, &self.events));
> ```

The downstream normalization is intentionally small but durable:

> `crates/hirsel-drivers/src/types.rs:126-139`
> ```rust
> pub enum SubagentEvent {
>     Started { external_id: String },
>     Progress { summary: String },
>     AssistantOutput { text: String },
>     Terminal { outcome: TerminalOutcome },
> }
> ```

> `crates/hirsel-host/src/lash_runtime/cli_turn.rs:208-235`
> ```rust
> Some(SubagentEvent::AssistantOutput { text }) => {
>     anyhow::ensure!(output.is_none(), "duplicate final CLI output");
>     *output = Some(text);
> }
> Some(SubagentEvent::Terminal { outcome }) => return Ok(outcome),
> ...
> Some(SubagentEvent::Progress { summary }) => {
>     let activity = tools.storage().append_thread_activity(
>         request.thread_id,
>         Some(self.turn_id),
>         "execution_progress",
>         &json!({"summary":summary}),
>     ).await?;
>     tools.publish_thread_activity(activity).await;
> }
> ```

The initial user echo is necessarily allowed before initialization, so an unconditional `check_session` on every user frame would be the wrong fix. The missing representation is an explicit distinction between that bootstrap receipt and a session-bound user/tool-result frame.

### Concrete invalid states and reachability

After `ClaudeOutput` accepts a system init for session `A` at `claude_events.rs:103-108`, either of these frames is representable and currently accepted:

```json
{"type":"user","session_id":"B","parent_tool_use_id":null,
 "message":{"content":[{"type":"tool_result","is_error":false}]}}
```

This reaches `claude_tool_result` at `claude_events.rs:200-211` and emits `Progress` without checking `B`. The same can occur before initialization, so a tool-result activity can precede `Started`. A `rate_limit_event` with a foreign or absent session identifier is likewise emitted without checking the active phase. Separately, a foreign `user` frame carrying the UUID of a pending follow-up input satisfies the receipt branch in `claude.rs:178-194`; that branch checks only `type`, nullable `parent_tool_use_id`, UUID and the boolean request kind, not session identity.

These are reachable from one provider stdout line, but no current source fixture writes a foreign-session frame or an early tool/rate-limit frame. No live values were inspected and no tests were executed. The ordinary trusted bridge/Claude path is therefore not evidence that the state occurs in production; the defect is latent protocol misrouting/CLI-boundary risk.

There is no database or durable duplicate writer in this finding. The duplicate truth is interpretive: `claude.rs::handle` and `ClaudeOutput::handle` independently classify the same `Value`, and only one of those interpretations enforces session ownership. No existing write path was found that updates two persisted copies inconsistently.

### Consumer blast radius

Reproducible queries at the fixed snapshot:

```text
rg -n 'Some\("user"\)|Some\("rate_limit_event"\)|check_session|claude_tool_result' \
  crates/hirsel-drivers/src/claude_events.rs
```

Result: **8 lines**, including the two unchecked branches and the three current `check_session` call sites. The normalized events are consumed by:

```text
rg -n 'SubagentEvent::(Started|Progress|AssistantOutput|Terminal)' \
  crates/hirsel-host/src/lash_runtime/cli_turn.rs
```

Result: **6 lines**. `cli_turn.rs:225-235` persists every `Progress` as `execution_progress`; `cli_turn.rs:208-212` accepts final output/terminal, so a foreign progress frame becomes visible activity even if final settlement is later correct. `TerminalOutcome` has **7** matching lines in the same consumer, controlling completed/cancelled/failed durable state at `cli_turn.rs:50-55`.

### Target representation and smallest scope

Keep the external wire format and `SubagentEvent`/`TerminalOutcome` types unchanged. Inside the owned driver, replace the two independent `Value` interpretations with one private normalization boundary, for example:

```rust
enum ClaudePhase {
    AwaitingInit { initial_input_id: RequestId },
    Active { session_id: SessionId },
    Terminal,
}

enum ClaudeFrame {
    BootstrapInputReceipt { uuid: RequestId },
    Session { session_id: SessionId, body: SessionFrame },
}
```

`SessionFrame` carries assistant, stream, tool-result, rate-limit and result variants. The parser accepts `BootstrapInputReceipt` only for the exact startup UUID while awaiting init; all other frames require the active `SessionId` before either settling `PendingRequest` or emitting a normalized event. The host conversion remains `ClaudeFrame -> SubagentEvent` and needs no schema/table change. This removes the invalid “foreign/pre-init frame is progress or receipt” state and makes ownership checking happen once.

Smallest credible affected files: `crates/hirsel-drivers/src/claude.rs`, `crates/hirsel-drivers/src/claude_events.rs`, and `crates/hirsel-drivers/src/claude_tests.rs`. No host interface change is required.

Regression/cutover risk is concentrated in the legitimate startup echo: preserve it as the explicit bootstrap variant. Validate the provider’s rate-limit envelope while implementing; if that event is intentionally sessionless, model it as an explicitly sessionless frame rather than leaving it in the session-bound wildcard. No migration or durable data cutover is needed.

Existing tests demonstrate only adjacent protections: `claude_tests.rs:444-471` rejects a foreign connector/tool inventory, and `claude_tests.rs:158-195` checks an unrelated UUID plus a correlated interrupt error. They do not send a foreign `user`/tool-result frame, a foreign follow-up receipt, a rate-limit frame, or a pre-init progress frame. Add those fixture cases and assert that the driver emits a protocol failure, never emits `Progress`, and never resolves the pending request for a foreign session. Required post-fix validation (not run here): targeted `cargo test -p hirsel-drivers claude` plus the full `cargo test -p hirsel-drivers --lib`.

**Confidence: high for the source-level invariant violation; medium for production reachability/materiality.**

## C10-F2 — Claude init catalog collapses duplicate MCP tools

### Verdict and exact evidence

**Verdict: recommend preserving multiplicity until uniqueness is checked in the second validator.** The preflight conversion correctly treats duplicate bridge names as invalid:

> `crates/hirsel-drivers/src/claude_config.rs:112-129`
> ```rust
> let mut actual = BTreeSet::new();
> ...
> for tool in tools {
>     let name = tool["name"].as_str().filter(|name| !name.is_empty()).ok_or_else(|| DriverError::Protocol("invalid scoped tool name".into()))?;
>     if !actual.insert(name.to_string()) || !tool["inputSchema"].is_object() {
>         return Err(DriverError::Protocol("invalid or duplicate scoped tool".into()));
>     }
> }
> ...
> let expected: BTreeSet<_> = launch.expected_tools.iter().cloned().collect();
> return if actual == expected { Ok(()) } else { Err(DriverError::Protocol("scoped bridge catalog differs from host expectation".into())) };
> ```

The later provider inventory conversion discards that invariant:

> `crates/hirsel-drivers/src/claude_events.rs:62-79`
> ```rust
> let tools = value
>     .get("tools")
>     .and_then(Value::as_array)
>     .ok_or_else(|| DriverError::Protocol("Claude init missing tool inventory".into()))?;
> let names = tools
>     .iter()
>     .map(|v| {
>         v.as_str()
>             .ok_or_else(|| DriverError::Protocol("invalid Claude tool name".into()))
>     })
>     .collect::<DriverResult<Vec<_>>>()?;
> let actual: BTreeSet<_> = names
>     .iter()
>     .filter(|n| n.starts_with("mcp__"))
>     .map(|n| n.to_string())
>     .collect();
> ```

> `crates/hirsel-drivers/src/claude_events.rs:88-105`
> ```rust
> if actual != self.expected
>     || !valid_servers
>     || !no_plugins
>     || names.iter().any(|name| config::FORBIDDEN_TOOLS.contains(name))
> {
>     return Err(DriverError::Protocol(
>         "Claude scoped tool inventory does not match host configuration".into(),
>     ));
> }
> if self.session_id.is_none() {
>     self.session_id = Some(id.into());
>     events.emit(SubagentEvent::Started { external_id: id.into() })?;
> }
> ```

There is no duplicate check on `names` or on the MCP-filtered sequence before it becomes a set. The two owned layers therefore disagree about the same exact-catalog invariant.

### Concrete invalid state and reachability

With a valid unique host expectation, an init frame such as:

```text
tools = ["Read", "Bash", "mcp__hirsel__threads_context",
         "mcp__hirsel__threads_context", "mcp__hirsel__threads_delegate"]
```

produces the same `actual` set as the unique inventory and passes `actual == self.expected`, provided the other init fields are valid. The provider has advertised one MCP tool twice, but the driver emits `Started` and accepts the session. This is a reachable malformed provider frame after preflight; the current fixture derives names from the unique bridge catalog, so no existing fixture reaches it. The normal bridge path is protected by `claude_config.rs:121-123`; no live values were inspected and no tests were run.

No persistent duplicate writer was found. The defect is layer drift: preflight validates a `Vec` with multiplicity, while init converts another `Vec` to a lossy `BTreeSet` and treats it as the authority. The same tool inventory is therefore validated twice with different state spaces.

### Consumer blast radius

Reproducible query:

```text
rg -n 'BTreeSet|expected_tools|tools/list|actual|mcp__hirsel__' \
  crates/hirsel-drivers/src/claude_config.rs \
  crates/hirsel-drivers/src/claude_events.rs \
  crates/hirsel-drivers/src/claude_tests.rs
```

Result: **18 lines**. The affected conversion has one direct downstream event consumer: `cli_turn.rs:213-223` records the accepted `Started` event, after which the session can produce scoped progress/terminal events. No database or client wire shape changes.

### Target representation and smallest scope

Keep `ScopedMcpLaunch.expected_tools` and the provider wire list as ordered `Vec`/JSON arrays, because the wire shape carries multiplicity. In `ClaudeOutput::handle`, insert every MCP name into a fresh set while comparing the count (or reject when `insert` returns false) before `actual` can be used for equality:

```rust
let mut actual = BTreeSet::new();
for name in names.iter().filter(|name| name.starts_with("mcp__")).copied() {
    if !actual.insert(name.to_owned()) {
        return Err(DriverError::Protocol("duplicate Claude MCP tool".into()));
    }
}
```

The target representation at the normalized layer remains the same unique `BTreeSet<String>` used for equality, but only after the raw list has passed a uniqueness constraint. `SubagentEvent`, `TerminalOutcome`, storage and client wire types remain unchanged. This deletes the invalid duplicate inventory rather than silently normalizing it away and aligns the second check with preflight.

Smallest credible files: `crates/hirsel-drivers/src/claude_events.rs` and `crates/hirsel-drivers/src/claude_tests.rs`; `claude_config.rs` needs no behavior change. Regression risk is low: valid unique inventories are unchanged; a CLI that intentionally repeats a tool would now fail closed, which matches the existing preflight contract. No migration or public interface cutover is needed.

Existing tests cover foreign catalogs and connector names (`claude_tests.rs:443-471`) and assert the number of preflight `tools/list` requests (`claude_tests.rs:405-423`), but do not cover duplicate names in the Claude init frame. Add a fixture with a duplicated `mcp__hirsel__...` entry and assert spawn fails with the inventory protocol error and never publishes `Started`. Required post-fix validation (not run here): targeted `cargo test -p hirsel-drivers claude` plus the full `cargo test -p hirsel-drivers --lib`.

**Confidence: high for the representation mismatch; medium for production reachability/materiality.**

## Coverage contract and explicit skips

| Owned area | Exact files/definitions inspected | Result |
|---|---|---|
| Native session/process lifecycle, request correlation, startup and stop | `crates/hirsel-drivers/src/claude.rs:1-412` (`ClaudeCodeDriver`, `PendingRequest`, `ProcessSession`, `RequestGuard`, `request`, `handle`, `spawn_command`, driver trait implementation, stdout supervisor) | F1 covers the event/receipt boundary. Startup rollback, process-group ownership, bounded EOF/exit drain, pending rejection, terminal replay and double retirement were inspected and not separately reported; existing tests cover them at `claude_tests.rs:31-91`, `197-346`, `536-655`. |
| Claude launch isolation and MCP preflight | `crates/hirsel-drivers/src/claude_config.rs:1-166` (`FORBIDDEN_TOOLS`, `configure`, `preflight`, `response`) | F2 covers the only representation drift found. Strict config, environment removal, bridge pagination, exact expected tool comparison and cleanup were inspected; no additional finding. |
| Claude event parser and terminal conversion | `crates/hirsel-drivers/src/claude_events.rs:1-262` (`ClaudeOutput`, `handle`, `check_session`, progress helpers, `claude_terminal_outcome`) | F1/F2 cover the two accepted state-space gaps. Final-output retention, malformed result handling, session checks on assistant/stream/result, bounded terminal summaries and interrupted/error mapping were inspected; no additional finding. |
| Native fixture peer | `crates/hirsel-drivers/src/claude_fixture.py:1-88` | Test-only MCP/stream-json peer; it exercises strict launch, bridge pagination, tool calls, input echo, assistant/result output and unique tool inventories. No production representation or separate finding. |
| Claude lifecycle regression tests | `crates/hirsel-drivers/src/claude_tests.rs:1-685` | All tests inspected. Existing cases cover startup cancellation/timeout, process descendants, receipt correlation by UUID, control rejection, EOF/late output, terminal replay, scoped bridge behavior, foreign catalogs, malformed/failure output and interrupt cleanup. Neither F1 nor F2 is demonstrated. Tests were not run. |
| Cross-layer consumers (read-only) | `crates/hirsel-drivers/src/types.rs:104-156`; `crates/hirsel-host/src/lash_runtime/cli_turn.rs:7-249`; `crates/hirsel-host/src/tools.rs:97-112` | `SpawnSpec -> driver -> SubagentEvent -> timeline/durable turn` and `TerminalOutcome -> ThreadTurnState` were traced. These are consumer reads; C10 does not claim ownership of shared types, EventHub, process-group helpers, storage or timeline. |
| SQL/schema/generated layers | No C10-owned definitions; no DDL or generated conversion is involved in this cluster | Explicit skip: no table, durable schema, or generated wire representation is owned here. |

Intentional/superseded behavior not reported: native Rust CLI drivers and strict scoped MCP startup are settled; one-shot Claude `result` termination and terminal output policy are demonstrated by the existing tests; tracked #2–#6 lifecycle outcomes and #18 stderr fixture work were not duplicated. `SubagentEvent` dimension/state choices, process-group/EventHub semantics and durable turn settlement remain adjacent-owner or consumer concerns.

## Audit log and final state

- Read `/tmp/hirsel-combined-audit/exclusions.md`, the worker prelude, repository `CLAUDE.md`/`CONTRIBUTING.md`, and both invoked read-only audit skill instructions.
- Read every line of all five owned files and the relevant shared/consumer conversion sites listed above. Re-ran targeted `rg` queries and independently re-derived both invalid states from the current source.
- No application code, tests, builds, installs, provider calls, live data/config, session/process actions, migrations, commits or pushes were performed.
- Expected snapshot before and after: `HEAD=3ee0621a603659ab0168f565b99012b642415419`; `HEAD^{tree}=a4aac830c45398a66591f2c44b707aaf3cef281b`.
- Source checkout status was empty before report creation and was rechecked empty after report creation. The only write is this report outside the repository; source is unchanged.

**Fix C10-F1 first:** it is the broader ownership failure and can incorrectly publish or settle a run before the narrower catalog duplicate check matters.
