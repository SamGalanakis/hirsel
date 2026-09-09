# Independent driver verification

2026-09-09. Verifier read the exclusion map and all three finder briefs, then re-derived the claims from the actual dirty `/workspace/code/hirsel` tree. Reference revisions independently checked: bb `4ed2743219e8a6ecd7d2c2535c68865dcf821b20`; T3 Code `e16b8b059c9f5ff6dfed1addecffb831c6aee043`. No main-tree changes, real provider sessions, or existing process operations were performed. Fixtures spawned for verification were retired/killed by their own handles or recorded process-group IDs.

## Verdict

| Candidate | Verdict | Evidence strength |
|---|---|---|
| Commands acknowledged before provider acceptance; missing ordered handshake | Accept | Actual driver probe plus source; references independently checked |
| Running follow-up versus queued next turn | Refine | Scripted queued-ID failure reproduced; installed Codex's behavior remains untested |
| Startup ownership and delayed stderr drain | Accept | Malformed-startup child survival reproduced; further failure points established from source |
| Exit zero without terminal leaves work unsettled | Accept | Actual Claude driver probe; shared branch applies to both native drivers |
| Terminal consumed before durable append | Accept | Actual extracted receiver probe plus bridge/engine source; no database fault-injection integration test |
| Native child notification can settle root | Accept with deployment condition | Actual captured Codex traffic replayed through Hirsel driver; native-child availability in installed configuration untested |

The first four candidates independently converge across the Hirsel finder and both reference readers. The bridge candidate came from the Hirsel finder. The native-child candidate came from the T3 reader follow-up. The verifier did not search for additional findings.

## Executed bounded probes

An isolated crate lives at `/tmp/hirsel-driver-verification-20260909`. All `src/*.rs` were copied from the current driver crate and compared byte-for-byte afterward. Public `CodexDriver` and `ClaudeCodeDriver` execute fake Python `codex`/`claude` peers from a PATH override in the probe subprocess only. The standalone manifest resolves available compatible dependencies offline; this is not a full-host or live-provider test.

Probe sources, fake peers, reproduction instructions, and hashes of the tested production source are retained in [verification-probes](verification-probes/README.md). Its preparation script creates a fresh isolated copy and extracts the bus definitions from a supplied checkout; production sources and build outputs are not vendored. The packaging workflow was added after these successful runs and was not rerun.

`cargo build --offline --example verify` succeeded. Each mode ran with an eight-second outer deadline:

- `startup`: fake Codex emits malformed JSON and remains alive. `spawn` returns an error while the recorded fixture PID still exists. The probe explicitly kills that fixture group. This reproduces an actual ownership gap, not merely hypothetical descendant behavior.
- `reject`: fake Codex accepts A, then rejects the follow-up `turn/start` with JSON-RPC error `-1`. Hirsel `prompt` returns success. A following progress barrier establishes the rejection passed through the reader. `interrupt` also returns success without waiting for a response.
- `queue`: fake Codex starts A, returns B's ID for a follow-up without starting B, then accepts another command. The actual `interrupt` request targets B. Progress barriers make the ordering deterministic. This proves Hirsel's handling of the queued protocol sequence, not the installed provider's queuing policy.
- `exit0`: fake Claude reads initial input, emits init, then exits zero. The fixture PID is confirmed gone; the driver event stream neither emits Terminal nor closes within two seconds while the driver remains owned. The session is then retired explicitly.
- `child`: fake Codex opens the same root ID as T3's captured fixture and replays its notifications, excluding the already-handled `thread/started`. The fixture's child `turn/completed` makes the actual Hirsel driver emit Done. The captured sequence contains no root completion.

`verify_bus` compiles the actual `TerminalEventBus` and `TerminalEventReceiver` definitions extracted from current `tools.rs`, with only a minimal stand-in terminal payload. After receiving `proc-a`, republishing it does not reach that receiver within 200 ms; a newly created subscriber can replay it. This proves receiver semantics, not a database failure by itself.

## 1. Correlate acceptance and perform the handshake in order

**Verified local paths:** Codex sends `initialize` and `thread/start` back-to-back at `crates/hirsel-drivers/src/codex.rs:92-103`. Startup only recognizes successful external IDs at `:296-316`; JSON-RPC errors are not surfaced. The normal reader at `:328-361` extracts turn IDs/events without routing error responses. `prompt` and `interrupt` return `write_json_line` at `:136-166`; that helper only serializes/writes/flushes at `shared.rs:128-133`. Claude control requests contain IDs at `claude.rs:111-123`, but its dispatcher ignores `control_response` through the default branch at `:186-214`. The host converts these returns into acknowledgements at `hirsel-host/src/lash_runtime/executor.rs:249-265`; the control bridge records successful delivery at `bridges.rs:272-283`.

**Verified reference mechanisms:** bb's `plugins/provider-codex/src/bridge/app-server-connection.ts:234-257,330-373` correlates response IDs, rejects error responses, and optionally times requests out. `bridge.ts:856-865` awaits initialize. T3's `packages/effect-codex-app-server/src/protocol.ts:278-295,450-467` correlates errors/results; `:186-217` fails pending requests at termination. `CodexSessionRuntime.ts:2261-2264` awaits initialize then notifies initialized before thread creation.

**Refinement:** source proves the ordering and error visibility gaps. Do not claim every Codex version rejects the current startup sequence. Do not describe Claude text input as having the same RPC receipt as Codex controls. bb uses the Claude SDK and T3 queues SDK input; neither establishes a native Claude input receipt for Hirsel. Recommend correlated bounded controls and precise input-acceptance language.

## 2. Choose steering or queued turns explicitly

**Verified local paths:** `codex.rs:142` always sends `turn/start`. Every successful response with `/result/turn/id` overwrites `active_turn_id` at `:333-336`; `turn/started` also updates it at `:338-348`. Host `tools/subagents.rs:89-98` retires the driver and stops its event consumer on the first terminal. The tool describes a running process at `tool_defs.rs:252-264`.

**Verified reference distinction:** bb `bridge.ts:1500-1538` implements `turn/steer` with `expectedTurnId`, emits correlated `input.accepted` only after success, and includes a request timeout. T3 `CodexSessionRuntime.ts:2356-2383` deliberately uses `turn/start` and preserves the active ID with `session.activeTurnId ?? turnId`; its source comment explicitly identifies the returned follow-up ID as queued. Its regression `CodexCollabRuntime.integration.test.ts:607-653` scripts A active/B accepted and checks interruption targets A.

**Refinement:** reject the characterization that T3 uses `turn/steer`, or that Hirsel is proven to encounter a busy rejection on every current installation. What is proved is that Hirsel mishandles the queued sequence T3 explicitly supports; if B is queued, the first-terminal host policy can kill its remaining work. The proposed simpler decision—steer the current delegated run—fits Hirsel's existing one-terminal lifetime. It remains a product/protocol choice to approve before implementation. Preserve explicit unsupported behavior where a provider cannot implement that contract.

## 3. Own startup from spawn through host handoff

**Verified local paths:** Codex spawns at `codex.rs:75`; its group guard first appears at `:115`, after fallible writes and the 30-second external-ID wait. `start_in_process_group` at `shared.rs:136-146` only installs `setsid`; it does not configure `kill_on_drop`. Stderr is taken at `codex.rs:85-88` but drained only at `:131`. A child that fills that pipe before answering can therefore stall the handshake. The reader is not concurrently draining it.

Codex registry insertion `:121` precedes the initial turn write `:123-129`, and Claude registry insertion `claude.rs:88` precedes its initial write `:92-97`. A failed write can leave a registry-owned session while returning no handle. At the host boundary, `tools/subagents.rs:47-62` has fallible process insertion/persistence/subscription after provider spawn and before the retiring event task is installed. No rollback appears on those paths. The engine startup-error branch (`lash_runtime/process_engines.rs:33-48`) cannot retire a handle it never receives.

**Reference mechanism:** T3 binds child spawning to `runtimeScope` at `CodexSessionRuntime.ts:1216-1233`, attaches stderr/exit consumers before startup at `:2199-2257`, and then performs the handshake. bb attaches stderr and direct-child exit handling in `app-server-connection.ts:290-323` during connection construction.

**Refinement:** the probe proves the malformed-startup child survives error return. It does not prove every real provider or descendant survives EOF, nor execute each persistence/cancellation/stderr-flood case. An immediate guard plus transactional handoff is the supported outcome; preserve the existing process-group retirement rather than rediscovering cleanup as absent.

## 4. Settle missing-terminal exits, including zero

**Verified local paths:** actual current `shared.rs:179-190` has `Ok(status) if terminal_sent || status.success() => {}`. Both readers call it only after leaving their stdout loops (`codex.rs:369-390`, `claude.rs:168-183`). Registry ownership retains the session's EventHub sender (`shared.rs:25-51,55-65`), so direct-child reader completion does not close the host's event stream. Host settlement depends on a terminal event at `tools/subagents.rs:71-100`; `subagents_wait` has no local timeout at `executor.rs:289-302`.

**Reference mechanism:** bb listens for the direct child's `exit` and gives pipe close a one-second grace before finalization (`app-server-connection.ts:7,313-323`); `bridge.ts:791-813` fails all open turns on unexpected exit, including zero. T3 emits session-exited on zero at `CodexSessionRuntime.ts:2232-2257`; Claude's normal stream-end branch completes an outstanding turn as interrupted at `ClaudeAdapter.ts:4006-4036`.

**Refinement:** the direct-child-exits/descendant-retains-stdout case is supported by reader ordering and OS pipe semantics, but was not executed here. Avoid a claim that nonzero-exit failure handling is absent. Preserve terminal results already observed and ensure exactly one settlement when transport ends without one.

## 5. Retry result delivery after durable append failure

**Verified local paths:** `tools.rs:125-143` puts a process into `seen` before returning it and excludes it from lag recovery. `bridges.rs:161-194` uses a stable replay key, appends once, and only logs an append error. The provider has already been retired at `tools/subagents.rs:89-98`. Its engine awaits this process's registry terminal at `process_engines.rs:50-56`. The live bridge neither acknowledges successful handling nor requeues failed handling. Recovery at `lifecycle.rs:433-472` abandons nonterminal Hirsel runtime rows rather than replaying the completed host outcome.

The receiver probe confirms republishing cannot fix this within the existing receiver. A new subscription can recover retained events, so "the result is permanently destroyed" would be false. Correct impact: failed append can leave the runtime unsettled for that bridge's lifetime despite a completed host process. Retry the same replay key or acknowledge only after durable handling. This is retrying result delivery, not restarting abandoned delegated work; ADR 0004 does not exclude it.

## 6. Scope native notifications to the root execution

The late candidate is stronger than an invented JSON shape: T3's captured `apps/server/src/provider/testFixtures/codexMultiAgentWire.json:2-10` identifies Codex CLI 0.145.0 and distinct root/child IDs. Lines `367-380` contain an actual child `turn/completed`. Its integration test also adds synthetic child cases, but those are not the basis of this verification.

Hirsel stores `session.thread_id`, yet `codex.rs:328-361` never checks incoming `params.threadId`. Its terminal parser at `:430-453` treats any `turn/completed` as the delegated terminal. Child started events can also overwrite the root active turn and reset its last message. The replay probe with a matching root demonstrates Done from the captured child completion. Host retirement on that Done follows directly from `tools/subagents.rs:89-98`.

T3 explicitly filters root started/completed updates against the current provider thread at `CodexSessionRuntime.ts:1896-1925` and separately maintains child state. Hirsel needs only the smaller invariant that another thread cannot settle or replace the root's control identity.

**Deployment condition:** no live installed-provider run was performed, and the probe does not establish that Hirsel's chosen model/configuration currently exposes native child spawning. Report this as a verified protocol-isolation defect triggered when native child notifications occur, not as a definite everyday production failure. Root filtering with a captured-wire regression is adoptable without copying T3's child-agent product model.

## Corrections and exclusions carried forward

- Some finder line numbers drifted. Use the current ranges above, especially `shared.rs:179-190` and `claude.rs:186-214`.
- bb and T3 use the Claude SDK; their guarantees do not imply Hirsel should adopt it. Keep ADR 0003's native Rust seam.
- T3's base RPC await is not universally deadline-bounded (`protocol.ts:465`); bb request timeouts are optional at the transport layer. Copy correlation with explicit deadlines, not an assertion of universal bounded waits.
- No claim that native tests, fake-driver tests, model/schema validation, process-group retirement, or replay retention are absent. The verified test delta is actual native-protocol peers and failure ordering.
- No new task-tracker or architecture issue duplicates were discovered; exclusions remain in force. Driver fixes are recommendations requiring the stated design choice, not implemented changes.
