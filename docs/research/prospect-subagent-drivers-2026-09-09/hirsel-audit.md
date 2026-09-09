# Hirsel native subagent audit

Read-only finder pass against `/workspace/code/hirsel` dirty working tree, 2026-09-09. Exclusion map read first. These are candidates for independent verification, not claims of executed reproductions. No implementation edits, provider calls, or test execution.

## Current contract

- Tool definitions are `crates/hirsel-host/src/lash_runtime/tool_defs.rs:242-328`: spawn declares a process and returns a process ID; prompt sends follow-up input to a **running** process; interrupt requests interruption; wait awaits a terminal outcome. Prompt and interrupt return only an acknowledgement object.
- `lash_runtime/executor.rs:201-246` records a `StartProcess` intent before starting a driver, using the current captured execution environment and parent-end abandonment. `process_engines.rs:263-297` declares `RecoveryContract::OwnerBound`, preventing a second host owner from rerunning a non-idempotent spawn. Engine `run` (`:23-57`) starts the driver then waits for the same runtime process terminal.
- `tools/subagents.rs:31-62` validates model/variant, starts the provider, inserts an in-memory process record, persists SQLite state, and subscribes to driver events. Its event consumer (`:71-100`) updates/persists every event and broadcasts throttled summaries. On the **first** terminal it retires the provider process group, publishes `ProcessTerminal`, and stops consuming.
- Codex runs `codex app-server --stdio`, sends `initialize` then `thread/start`, waits up to 30 seconds for an external thread ID, sends `turn/start`, and starts background stdout/stderr readers (`hirsel-drivers/src/codex.rs:47-133`). Follow-up input always sends another `turn/start`; interrupt sends `turn/interrupt` with the most recently observed active turn ID (`:136-166`). Claude runs `claude -p --input-format stream-json --output-format stream-json --dangerously-skip-permissions --verbose`; model/effort become flags. It writes user messages and an interrupt control request (`claude.rs:37-130`). Both use local CLI credentials and full-auto permissions.
- Driver events expose only Started, compact Progress, and Terminal. Progress strings cap at 240 characters; final messages cap explicitly at 24,000 characters (`shared.rs:97-125`). Codex derives its result from the last completed agent message; Claude uses the result envelope. Runtime process terminal conversion preserves done/failed/interrupted status and the result text (`process_engines.rs:447-488`).
- Host terminal bus retains terminal outcomes and replays to late/lagged subscribers (`tools.rs:71-148`). `lash_runtime/bridges.rs:143-201` appends a terminal event to Lash with a stable replay key, then dispatches a triage fork or enqueues the direct wake. The runtime terminal event resolves wait.
- SQLite restoration changes previously Running rows to Abandoned (`storage/subagents.rs:76-117`); `lash_runtime/lifecycle.rs:407-531` reconciles runtime rows. This is deliberate no-retry recovery under ADR 0004, not a missing resume feature.

## Four strongest candidates

### 1. Startup ownership begins too late; startup errors can leave a provider tree unretired

**Trigger:** Codex starts successfully but never emits its thread ID, emits malformed initialization JSON, or encounters a write failure. A stderr burst larger than the pipe capacity before the thread ID is another concrete startup trigger.

**Evidence:** `codex.rs:75-103` spawns and waits with fallible returns; the `ProcessGroup` RAII guard is only constructed at `:115`, and stderr is only drained at `:131`. `shared.rs:136-169` creates a separate session with setsid and kills it only through that guard. There is no startup rollback or `kill_on_drop` configuration in this path. Even after guard creation, the session enters the registry at `codex.rs:121` before the fallible first-turn write at `:129`; a write error returns no handle to the host while leaving the registry entry. Claude likewise inserts at `claude.rs:89` before the initial fallible write at `:95-100`.

The host has another ownership-transfer gap: `tools/subagents.rs:47-62` can fail persistence/subscription after driver spawn but before starting the consumer that retires it. Engine error handling reports startup failure (`process_engines.rs:44-49`) without receiving a handle to retire.

**Impact:** failure reporting does not imply cleanup; process descendants can survive startup failure, or remain registry-owned until driver teardown. Startup stderr can force a false timeout. Actual descendant survival depends on provider behavior after pipe closure; no live reproduction claimed.

**Adoptable outcome:** create an owned startup guard immediately after spawn; start readers immediately; retain rollback until the session and host consumer are successfully installed. Require failure injection at handshake timeout, malformed init, initial write, and persistence handoff. Existing `tests.rs:325-343` proves the drain helper works when started promptly, not that Codex starts it promptly.

### 2. Clean exit without a terminal message leaves the process Running indefinitely

**Trigger:** provider exits with status zero before emitting Claude `result` / Codex `turn/completed` (including missing or unrecognized terminal envelopes).

**Evidence:** `shared.rs:179-190` explicitly does nothing for `terminal_sent || status.success()`. Both stdout loops break on EOF and call this helper (`codex.rs:369-390`; `claude.rs:174-193`). SessionRegistry retains the session/EventHub; EventHub owns its sender (`shared.rs:55-65`), so the host consumer's `events.next()` does not close merely because the child reader exits. Host retirement occurs only inside the terminal branch (`tools/subagents.rs:89-98`). `subagents_wait` has no independent timeout (`lash_runtime/executor.rs:289-302`).

**Impact:** the operating-system child has exited, but host state and runtime wait can remain running until explicit abandonment/restart. This is a direct branch/ownership derivation; no process reproduction executed.

**Adoptable outcome:** any process exit without the expected terminal envelope becomes an explicit failed terminal, including exit 0; malformed/unsupported terminal protocol shapes must fail closed. Test clean EOF, malformed JSON followed by EOF, and nonzero EOF in native protocol fixtures.

### 3. Provider commands are acknowledged on pipe flush; request rejection is discarded

**Trigger:** provider returns a JSON-RPC error for turn/start or turn/interrupt, or Claude returns a control error. A follow-up arriving during a running Codex turn exercises the ambiguous control contract.

**Evidence:** `codex.rs:136-166` and `claude.rs:103-130` return the result of `write_json_line`; `shared.rs:128-133` only serializes, writes, and flushes. `codex.rs:328-361` extracts turn IDs and notifications but has no error-response branch or pending-request correlation. `claude.rs:196-228` maps known event types and discards all others; control responses are not acknowledged. `lash_runtime/executor.rs:249-266` converts the successful write to acknowledgement. The control bridge records interruption acknowledgement after this call returns (`bridges.rs:263-284`), so rejection can also suppress its retry.

The exposed prompt contract specifically targets running work (`tool_defs.rs:254`), but Codex prompt always sends `turn/start` (`codex.rs:142`) rather than identifying a steer of the active turn. Meanwhile host supervision kills the process on the first turn terminal (`tools/subagents.rs:89-98`). This code proves inconsistent lifetime assumptions; whether this provider version rejects, steers, or queues turn/start requires protocol/reference verification. Do not claim a specific provider reaction from this local audit alone.

**Impact:** the Agent can receive success for input the provider rejected; interruption can be considered delivered without provider acceptance. Accepted queued follow-ups could be killed at first-terminal retirement, depending on provider semantics.

**Adoptable outcome:** correlate response IDs, surface typed provider errors, bound request acknowledgement time, reject pending requests on transport close, and explicitly choose running-turn steering versus queued follow-up semantics. Keep native drivers and current full-auto policy.

### 4. Terminal delivery is consumed before durable append succeeds

**Trigger:** a transient runtime registry append failure when the provider has already completed.

**Evidence:** `tools.rs:125-141` inserts the process ID into `seen` before returning a terminal to its consumer. `lash_runtime/bridges.rs:165-194` attempts append once and only logs errors. A lag recovery excludes seen process IDs; no retry/requeue/acknowledgement path appears in this bridge. The upstream provider was already retired and consumer ended (`tools/subagents.rs:89-98`). `HirselSubagentEngine::run` is awaiting terminal registry settlement (`process_engines.rs:50-56`).

**Impact:** the in-memory/SQLite host process can be Done while the Lash process and wait remain unsettled for the lifetime of this bridge. Retention solves late subscription and broadcast lag, not failed handling. A newly created subscriber could replay retained state, but the running bridge does not recreate itself. Startup recovery abandons unmatched running runtime rows rather than replaying this original outcome.

**Adoptable outcome:** acknowledge delivery only after durable append, retry the same replay key on transient failure, and reconcile persisted final host outcomes into unsettled runtime rows. This retries result delivery, not delegated work, and does not violate ADR 0004. Test one injected append failure followed by success and assert a single final outcome and wake.

## Coverage caveats / considered but lower priority

- Native real-CLI smoke tests are present and ignored by default (`hirsel-drivers/src/tests.rs:345-390`); they return on any Terminal, without asserting Done, and have no event-loop timeout. Parser tests preserve final text, cap Unicode safely, and exercise fake-driver lifecycle. Do not report all transport coverage absent: this pass inspected those tests only; repository-wide E2E needs a separate check.
- Driver EventHub retains every event in an unbounded Vec (`shared.rs:57,70`); ProcessStore also retains every event (`processes.rs:127`) and SQLite rewrites the whole event list per update (`storage/subagents.rs:16`). Recent-progress reads are bounded to 20 and broadcasts throttled; those do not bound retention. This is an additional performance candidate, below the four correctness outcomes.
- EventHub subscription is not an atomic append+broadcast boundary: emit drops the events lock before send (`shared.rs:69-71`), while stream snapshots under that lock (`:75-80`). A concurrent new subscriber can see a just-appended item both in backlog and broadcast. This is lower impact for the one-shot terminal consumer and should not distract from the four candidates.
- No provider SDK migration, permission UI, task-model redesign, automatic restart, or removal of existing schema validation proposed.
