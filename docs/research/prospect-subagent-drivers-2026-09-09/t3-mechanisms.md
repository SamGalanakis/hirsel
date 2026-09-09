# T3 Code provider mechanisms and Hirsel candidates

Reader findings only; independent verification remains required. Inspected local reference `/tmp/ref-t3code` at `e16b8b059c9f5ff6dfed1addecffb831c6aee043` and the actual dirty Hirsel checkout `/workspace/code/hirsel`. Read `exclusions.md` first. No implementation edits or live provider sessions.

## Doctrine read first, then applied

T3's [provider constraints](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/docs/internals/providers.md#L3-L10) put native protocol and capability normalization at the adapter boundary. Its [verification rules](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/AGENTS.md#L104-L110) require observable backend tests and waiting on receipts/worker drains rather than timing guesses.

Applied to Hirsel: the native Rust `SubagentDriver` seam already matches this doctrine and should stay. The gaps are behavioral contracts inside that seam: a successful pipe write is currently returned as command acknowledgement, active and queued Codex turns share one mutable ID, and process exit can leave the host waiting forever. The appropriate test delta is scripted native-protocol peers exercising the actual Codex/Claude drivers; Hirsel already has fake-driver and real CLI smoke tests, so this is not a proposal to add generic test coverage or replace the runtime.

## How T3 actually calls providers

### Codex: owned app-server process plus correlated RPC

- Spawns the configured Codex binary as app-server with the process attached to an Effect scope and a two-second force-kill escalation. It then attaches the app-server client to that process: [CodexSessionRuntime.ts:1180-1245](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexSessionRuntime.ts#L1180-L1245); constant at line 55.
- The transport assigns request IDs, stores an ID-to-deferred-and-method entry, writes, and awaits the correlated response. Provider error responses fail that exact request. Termination fails all pending requests and closes outgoing admission: [protocol.ts:186-244](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/packages/effect-codex-app-server/src/protocol.ts#L186-L244), [278-295](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/packages/effect-codex-app-server/src/protocol.ts#L278-L295), [450-467](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/packages/effect-codex-app-server/src/protocol.ts#L450-L467).
- Starts stderr and exit-code consumers before `start`; startup awaits `initialize`, sends `initialized`, then opens the thread. An unexpected zero exit still closes the session: [CodexSessionRuntime.ts:2199-2293](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexSessionRuntime.ts#L2199-L2293).
- Follow-up input uses `turn/start`, **not** `turn/steer`. Its returned queued-turn ID is distinct from the active ID; `activeTurnId: session.activeTurnId ?? turnId` preserves the currently interruptible turn: [2356-2383](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexSessionRuntime.ts#L2356-L2383).
- Stop sends interrupts to native child threads, bounded to three seconds each and ten seconds overall, then interrupts the root turn. Scope close shuts the process down: [2385-2418](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexSessionRuntime.ts#L2385-L2418), [2304-2323](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexSessionRuntime.ts#L2304-L2323).

### Claude: SDK-managed CLI, same-turn input, hard stop

- T3 imports `query` from `@anthropic-ai/claude-agent-sdk`: [ClaudeAdapter.ts:1-25](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L1-L25). This is **not** a second hand-written stream-json client like Hirsel. Transfer the observable guarantees, not the excluded SDK migration.
- A prompt during a real running turn enters `promptQueue` and retains that turn's ID; it does not fabricate another turn boundary: [4892-4900](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L4892-L4900), [4942-4985](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L4942-L4985), [5005-5018](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L5005-L5018). Queue admission is not proof the model has consumed the text.
- A normally ending stream with an outstanding turn emits interrupted, then closes the session: [4006-4036](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L4006-L4036).
- `interruptTurn` closes the SDK query because interrupt acknowledgement can leave background work alive. Closure is initiated before potentially blocking cleanup; it settles pending questions, active tasks, and the current turn, then shuts the prompt queue: [4039-4116](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L4039-L4116), [5021-5029](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.ts#L5021-L5029). Hirsel already distinguishes interrupt from abandon/retire; do not copy this hard-stop product choice without deciding that contract.

## Ranked adoptable practices and four candidates

### 1. Make driver command acceptance a protocol result

**Hirsel evidence:** `crates/hirsel-drivers/src/codex.rs:92-103` sends initialize and thread/start back-to-back; `read_codex_thread_id` at 294-318 only recognizes successful thread IDs and does not return RPC errors. Prompt/interrupt at 136-175 return after `write_json_line`; stdout handling at 321-399 has no response-error routing. Claude interrupt at `claude.rs:111-129` writes a request ID, while `claude_events` at 190-225 ignores `control_response`. The host returns its acknowledgement at `crates/hirsel-host/src/lash_runtime/executor.rs:249-265` after these writes.

**Concrete failure:** a Codex `turn/start` or `turn/interrupt` error is successfully written, ignored on return, and reported as an acknowledged tool call. A startup error is obscured as missing external ID after waiting. Claude negative interrupt responses are likewise invisible.

**Outcome:** correlate native control request IDs with success/error, await the initialization handshake in order, and settle pending requests on transport closure. Add bounded control deadlines rather than copying T3's unbounded root RPC wait. Distinguish accepted/written input from acknowledged controls where the provider has no input receipt. Protocol peer tests should reject initialize/start/interrupt and verify the tool sees the failure. No approval UI, capability catalog, SDK migration, or recovery policy change.

### 2. Decide and enforce running follow-up semantics

**Hirsel evidence:** `codex.rs:136-144` sends a new turn/start. Every response containing `/result/turn/id` overwrites `active_turn_id` at 339-343, regardless of whether that turn is queued. Host event pump `crates/hirsel-host/src/tools/subagents.rs:89-98` retires and kills the driver at the first terminal notification. The tool promises follow-up input to a running process at `lash_runtime/tool_defs.rs:252-264`.

**Concrete sequence to reproduce:** A is running; prompt B returns B's queued ID; interrupt now targets B instead of A. Independently, A completes before queued B completes; the host consumes A's terminal and kills the process that owns B. The finding is conditional on native queuing behavior; T3 documents and explicitly accommodates that behavior in its current runtime.

**Decision:** recommend defining this tool as steering the current delegated run and mapping it to the native same-run mechanism where supported. If queued next turns are desired, track active versus queued IDs and do not settle the delegated run on a predecessor's terminal event. Either choice needs an explicit capability/unsupported response; preserve ADR 0016's separation of durable Thread and process ownership. T3 is evidence for the distinction, not evidence that T3 uses Codex turn/steer. Test A-running/B-accepted/A-completed and interrupt-before-B-started with a wire peer.

### 3. Own a spawned process before the first fallible handshake operation

**Hirsel evidence:** `codex.rs:75-103` spawns and awaits a handshake; the process-group RAII guard is constructed only at 108-115. Stderr starts draining at 131, after handshake and initial turn write. Shared `start_in_process_group` creates a session but does not set kill-on-drop (`shared.rs:141-153`). A parser error, timeout, or cancelled spawn future can leave the child without the existing group guard. Startup stderr can fill its pipe before the driver begins draining it. Existing `tests.rs:324-342` verifies the standalone drain helper, not its startup ordering. Session registration also precedes the fallible initial prompt write (`codex.rs:121-129`; Claude `claude.rs:88-101`).

**Outcome:** install owned process cleanup immediately after spawn, start both output readers immediately, and transfer ownership into the registry only after successful startup or roll registration back on failure. Add deterministic peer cases for malformed/negative/never-completing handshakes, stderr flooding before init response, cancellation, and initial-write failure. This extends already-existing process-group retirement, rather than claiming cleanup is absent.

### 4. Settle delegated work when the transport ends without a terminal result

**Hirsel evidence:** `shared.rs:195-203` explicitly does nothing when `child.wait()` returns success, even if `terminal_sent` is false. Both native readers call this helper. `EventHub` retains its sender (`shared.rs:56-60`) while registered session ownership persists, so the host loop at `tools/subagents.rs:71-100` need not see channel closure or a terminal event.

**Concrete failure:** a scripted provider emits init/progress, exits 0, and never emits result/turn-completed. The process remains running in host state and the terminal wake is not published. Nonzero exit already has a failure path and must not be rediscovered as absent.

**Outcome:** every ended provider stream with outstanding delegated work must produce one terminal outcome even on exit 0, then close owned resources. Preserve the actual result when it arrived first. Test clean EOF/no result, failed EOF/no result, and result-before-EOF without duplicate settlement. T3's Claude stream-exit behavior and Codex explicit session-exited event provide the reference guarantee.

## Test mechanisms to adopt with those fixes

- A real subprocess mock peer exercises the actual transport: [client.test.ts:32-123](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/packages/effect-codex-app-server/src/client.test.ts#L32-L123). Its stderr-pressure test begins at line 126.
- Protocol tests exercise pending-request failure before blocked cleanup, correlated error responses, fragmented frames, and EOF: [protocol.test.ts:613-725](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/packages/effect-codex-app-server/src/protocol.test.ts#L613-L725), [790-810](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/packages/effect-codex-app-server/src/protocol.test.ts#L790-L810).
- Captured native wire replay drives the actual CodexSessionRuntime, not merely a pure translator: [CodexCollabRuntime.integration.test.ts:1-9](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexCollabRuntime.integration.test.ts#L1-L9), [201-233](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/CodexCollabRuntime.integration.test.ts#L201-L233).
- Claude tests prove mid-turn input preserves turn identity and interruption closes the query/settles tasks: [ClaudeAdapter.test.ts:1403-1470](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.test.ts#L1403-L1470), [2933-3009](https://github.com/pingdotgg/t3code/blob/e16b8b059c9f5ff6dfed1addecffb831c6aee043/apps/server/src/provider/Layers/ClaudeAdapter.test.ts#L2933-L3009).

## Considered, not adopted / reference weaknesses

- T3's request deferred is not universally deadline-bounded (`protocol.ts:465`); its root interrupt still awaits a potentially unbounded RPC, despite bounding child interrupts. Copy acknowledgement correlation with deadlines and closure guarantees, not the entire implementation.
- SDK adoption, Effect/event sourcing, account-instance expansion, persisted provider auto-resume, and approval UX are excluded or unnecessary to this round.
- T3 uses unbounded runtime/outgoing queues (`CodexSessionRuntime.ts:1191`; `protocol.ts:160`) while retaining only a bounded sliding raw-notification history (`protocol.ts:161-164`). Do not advertise its buffering as globally bounded. Hirsel's driver event replay vector is also unbounded (`shared.rs:58,72`), despite bounded host progress; this deserves separate verification but is not included among the four lifecycle candidates.
- T3's hard Claude interrupt is a deliberate product choice. Hirsel already has native interrupt and hard abandonment; improving acknowledgement/deadline semantics is useful without collapsing those operations.
- The T3 documentation's broader account, update, attachment, and native sandbox concerns do not establish a current Hirsel gap. No findings raised for them.
