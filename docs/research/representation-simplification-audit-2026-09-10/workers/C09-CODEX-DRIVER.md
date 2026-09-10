# C09-CODEX-DRIVER audit report

Snapshot: `HEAD 3ee0621a603659ab0168f565b99012b642415419`
Expected tree: `a4aac830c45398a66591f2c44b707aaf3cef281b`
Cluster: native Codex app-server session, request correlation, and process control.

Source integrity was checked before review with:

```text
git rev-parse HEAD HEAD^{tree}
git status --porcelain
```

The reported HEAD/tree matched and the source status was empty. The same checks
were repeated after review/report generation; the source checkout remains clean
and at the same HEAD/tree. No tests, builds, installs, application execution,
provider calls, or source edits were performed.

## Findings

### F-01 — Strictly classify inbound JSON-RPC frames before request correlation

Verdict: recommend. Priority: high for protocol robustness; confidence: high.

The inbound wire representation is an untyped `serde_json::Value`, and the
frame classifier uses field presence rather than a mutually exclusive protocol
shape:

```text
crates/hirsel-drivers/src/codex_io.rs:34-59
    let result = match serde_json::from_str::<Value>(&line) {
        Ok(value) if value.get("method").is_some() && value.get("id").is_some() => {
            ... write unsupported-server-request reply ...
        }
        Ok(value) => session.receive(value),
        Err(error) => Err(error.into()),
    };

crates/hirsel-drivers/src/codex.rs:50-58
struct CodexState {
    ...
    pending: HashMap<u64, (&'static str, PendingReply)>,
}

crates/hirsel-drivers/src/codex.rs:178-218
if value.get("method").is_none()
    && let Some(id) = value.get("id").and_then(Value::as_u64)
{
    if let Some((method, tx)) = state.pending.remove(&id) { ... }
    return Ok(());
}
```

Concrete invalid frames and results:

* `{"id":1,"method":null}` satisfies the first classifier and is treated as
  an unsupported server request; the pending client request with id `1` is
  never removed or completed.
* `{"id":"1","result":{}}` reaches `receive`, but `as_u64()` fails, so it
  is ignored and the pending request remains until its timeout.
* `{"id":1,"method":"x","result":{}}` is also treated as a server
  request rather than a response, leaving the pending request stranded.

The request is eventually cleaned by `PendingRequest::drop`
(`crates/hirsel-drivers/src/codex.rs:84-95`), but the caller waits for the
full control deadline (`:110-134`) and the malformed frame can cause a false
unsupported-request reply. This is a latent malformed-provider/peer path, not
a claim about a live Codex value: no live provider was queried. The supplied
fixture does exercise a valid native server request with a string id
(`crates/hirsel-drivers/fixtures/codex_app_server.py:144-160`), but no
ambiguous response shape. There is no duplicate source of truth here: the
pending map is the sole request waiter; the defect is that some wire values do
not map to any valid frame and therefore do not settle it.

Consumer/correlation blast radius was re-counted with this exact query:

```text
rg -n 'read_codex_stdout|receive\(|pending\.|value\.get\("method"\)|value\.get\("id"\)|from_str::<Value>' crates/hirsel-drivers/src/codex.rs crates/hirsel-drivers/src/codex_io.rs crates/hirsel-drivers/src/codex_native_tests.rs
```

Result: 19 matching lines across the ingress classifier, receiver, pending
state, and native-peer tests. The resulting `SubagentEvent`/`TerminalOutcome`
conversion is consumed by `crates/hirsel-host/src/lash_runtime/cli_turn.rs:208-240`;
there is no separate persisted wire shape in this cluster.

Target representation:

* Wire layer: parse each line into a strict `CodexFrame` enum with disjoint
  `Response { id: ClientRequestId, result/error }`, `ServerRequest { id:
  JsonRpcId, method: NonEmptyMethod, params }`, and `Notification { method:
  NonEmptyMethod, params }` variants. Preserve an explicit `null` result with
  presence-aware response parsing; do not classify by merely seeing a key.
* Driver layer: use a `ClientRequestId(u64)` newtype as the pending-map key and
  route only the `Response` variant to it. Invalid or ambiguous frames should
  return `DriverError::Protocol`, which reaches `read_codex_stdout`'s existing
  `session.fail` path and drains all pending waiters.
* Host/event layer: keep `SubagentEvent` and `TerminalOutcome` unchanged; they
  are the intentional conversion boundary, not a second request-correlation
  store.

Smallest credible scope: `codex_io.rs` frame parsing, `codex.rs` pending
correlation, and `codex_native_tests.rs` plus fixture modes for the three
ambiguous shapes. Preserve valid native server requests and their string IDs.
Existing validation covers syntax failure (`:152-173`), correlated control
errors (`:244-288`), out-of-order replies, and valid native requests
(`:375-389`), but not ambiguous frame shapes. Additional inspection-required
validation is to assert immediate protocol failure, exactly one terminal event,
all pending callers released, and process-group cleanup for each malformed
shape; no validation was run in this audit.

### F-02 — Reject empty provider thread/turn identities at the parse boundary

Verdict: recommend. Priority: medium; confidence: high.

Codex thread and turn identities are represented as unconstrained `Option<String>`
values and accepted whenever JSON contains a string, including `""`:

```text
crates/hirsel-drivers/src/codex.rs:50-58
    thread_id: Option<String>,
    active_turn_id: Option<String>,

crates/hirsel-drivers/src/codex.rs:195-210
if method == "thread/start" {
    state.thread_id = result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    ...
}
if method == "turn/start" && !self.events.is_terminal() {
    state.active_turn_id = result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .map(str::to_string);
}

crates/hirsel-drivers/src/codex.rs:439-455
let thread_id = opened.pointer("/thread/id").and_then(Value::as_str)
    .ok_or(DriverError::MissingExternalId)?;
...
if started.pointer("/turn/id").and_then(Value::as_str).is_none() {
    return Err(protocol_error("codex turn/start returned no turn id"));
}
```

The concrete malformed-but-representable sequence is:

1. `thread/start` returns `{"thread":{"id":""}}`; the state stores
   `Some(String::new())`, emits `Started { external_id: "" }`, and the local
   startup check accepts it because it tests only `None`.
2. `turn/start` returns `{"turn":{"id":""}}`; the state stores an empty
   active turn and returns a session handle for it.
3. `prompt` and `interrupt` serialize that empty value as `expectedTurnId` or
   `turnId` (`crates/hirsel-drivers/src/codex.rs:303-335`). Completion handling
   explicitly filters empty completion IDs (`:263-270`), so the session can
   become unsteerable and later fail as an apparent transport/protocol end.

This is latent input from the local provider peer, not an observed live value;
no provider/config/data rows were read. All supplied fixture IDs are nonempty
(`crates/hirsel-drivers/fixtures/codex_app_server.py:18-24,100-142`), and the
native tests cover mismatched nonempty IDs but have no empty-ID mode. No
duplicate write path was found: the startup response is the authoritative
assignment and `turn/started` only fills the turn when absent
(`crates/hirsel-drivers/src/codex.rs:238-242`). The issue is that the shared
`String` representation admits an identity that the later completion parser
already treats as invalid.

The identity consumer query was independently re-run as:

```text
rg -n 'threadId|expectedTurnId|turnId|external_id|thread_id|active_turn_id' crates/hirsel-drivers/src/codex.rs crates/hirsel-drivers/src/codex_config.rs crates/hirsel-drivers/src/codex_io.rs crates/hirsel-drivers/src/codex_native_tests.rs crates/hirsel-host/src/lash_runtime/cli_turn.rs crates/hirsel-drivers/src/types.rs
```

Result: 38 matching lines across state assignment, wire construction, root
filtering, tests, and host activity conversion. The host stores the validated
external id as ordinary activity JSON at
`crates/hirsel-host/src/lash_runtime/cli_turn.rs:213-223`; it does not need a
new persisted schema.

Target representation:

* Provider conversion: parse IDs through a `NonEmptyCodexId` constructor that
  accepts opaque UTF-8 strings but rejects missing and empty values.
* Driver state: use distinct `CodexThreadId` and `CodexTurnId` newtypes,
  retaining `Option<CodexThreadId>` before startup and
  `Option<CodexTurnId>` for the one active turn. Wire builders accept only the
  corresponding newtype and expose `as_str()`; this prevents an empty or
  thread/turn-swapped identity inside the driver.
* Host conversion: convert the validated thread id once to
  `SubagentEvent::Started { external_id: String }`; leave the shared public
  event contract and durable activity JSON unchanged.

Smallest credible scope: the response/notification identity extractors and
`CodexState` in `codex.rs`, with fixture modes and assertions in
`codex_app_server.py`/`codex_native_tests.rs`. Add pure extraction tests and
startup cleanup assertions for empty thread and turn IDs. Regression risk is
low: the target preserves all nonempty opaque IDs and rejects only malformed
provider output, but the exact provider's permitted ID alphabet should remain
opaque rather than being over-validated. No tests were run.

## Coverage contract and explicit skips

| Area | Exact owned definitions/files inspected | Result |
| --- | --- | --- |
| Session/request state | `codex.rs:40-288` (`CodexDriver`, `CodexState`, `CodexSession`, `StartupGuard`, `PendingRequest`, request/receive/fail paths) | F-01 and F-02 above; no third finding. |
| Driver API and process handoff | `codex.rs:294-464` (`SubagentDriver` methods, `spawn_command`, request builders) | Startup/retire/interrupt ownership is coherent in this snapshot; existing #2–#6 outcomes were not re-reported. `ProcessGroup` and direct `Child` are intentionally separate handles for group cleanup and direct-exit supervision. |
| Config/isolation/catalog | all `codex_config.rs:1-203` (`ISOLATION`, launch validation, inherited names, thread request, paged catalog verification) | Skip. The isolation keys are centralized, arbitrary MCP names are kept as object keys, and catalog tool sets are validated as unique exact sets. No independent duplicate-truth or invalid state cleared the materiality threshold. |
| Stdout/terminal conversion | all `codex_io.rs:1-156` (`read_codex_stdout`, progress/message/outcome decoders) | F-01 ingress shape; terminal status mapping and bounded summaries are otherwise explicit. |
| App-server peer fixture | all `codex_app_server.py:1-223` | Skip as a product owner: it is a hermetic protocol peer. Its modes cover valid correlation, child filtering, rejection, backpressure, EOF, malformed completion, catalog pagination, and process cleanup; no live values are implied. |
| MCP host fixture | all `codex_mcp_host.py:1-38` | Skip as a product owner: it only supplies the expected JSON-RPC bridge/catalog contract and has no independent production representation. |
| Native integration tests | all `codex_native_tests.rs:1-721` | Existing coverage recorded above; add only the F-01/F-02 regression shapes if either is implemented. The test file itself is not a finding. |

Read-only consumer/conversion context included:

* `crates/hirsel-drivers/src/types.rs:104-156`: `SpawnSpec`, `SessionHandle`,
  `SubagentEvent`, `TerminalOutcome`, and the stable `SubagentDriver` seam.
* `crates/hirsel-drivers/src/shared.rs:25-158,227-260`: session registry,
  retained event hub, terminal completion, and process-group helpers. The
  full assistant output versus bounded terminal summary is explicit contract
  (`types.rs:133-148`), so it is not reported as duplicate truth.
* `crates/hirsel-host/src/lash_runtime/cli_turn.rs:48-99,101-247`: terminal
  conversion, activity timeline writes, cancellation/interrupt recovery, and
  durable completion. Existing durable delivery behavior is excluded by #6;
  no distinct driver regression was found.

Explicitly skipped after the exclusion-map check: tracked #2–#14 outcomes,
tracked #16 interrupted FIFO head, tracked #18 stderr timing, #10's planned
native artifact viewer, independent F02–F05 wire/view findings, automatic
delegated-work restart, the intentional separate Thread dimensions, and the
shared EventHub retention/broadcast details. Those are either settled,
tracked, outside this owned boundary, or have no distinct current-source
regression.

## Independent coverage and audit log

1. Read the driver spec and `/tmp/hirsel-combined-audit/exclusions.md` before
   classifying findings.
2. Read all required repository guidance and both invoked audit skill
   instructions. Used `rg --files crates/hirsel-drivers`, line counts, and
   complete numbered reads of every owned file: 519 + 203 + 156 + 223 + 38 +
   721 = 1,860 owned lines.
3. Re-read the shared public types, event hub, host CLI-turn consumer, and
   driver call sites as consumer/conversion context only. No shared definition
   was reassigned to this cluster.
4. Ran the two exact consumer queries recorded under F-01 and F-02, then
   performed a second coverage pass over config, fixtures, tests, process
   control, event conversion, and all `crates/hirsel-drivers` file paths.
5. No database/table/cache layer belongs to this cluster. The only durable
   downstream shape inspected is host activity/terminal conversion; it is a
   consumer, not a co-owned Codex representation.
6. Rechecked `git rev-parse HEAD HEAD^{tree}` and `git status --porcelain` after
   writing this external report. Expected identity and clean source status
   remain intact.

Fix F-01 first: it prevents malformed provider frames from being mistaken for
valid server traffic and leaving correlated callers waiting on timeout.
