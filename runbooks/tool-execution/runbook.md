# Product scenario: visible and honest tool execution

> Read [../RULES.md](../RULES.md) first.

**Purpose.** Prove that real tool attempts appear inline, update in place with
their actual result, and support an honest Agent answer for both success and
failure.

**Real model calls:** two.

## Golden rules

1. Prompt text and Agent claims are not tool evidence. Require matched
   `tool_start`/`tool_done` frames with one stable call ID.
2. The tool row is visible inline in the turn's run card, without opening a
   turn-wide disclosure. Opening that one row to read its payload is allowed;
   the payload appears in the shared panel below the run of rows, and only one
   step is open at a time within a turn.
3. Success requires the noncoincidental stdout marker in the tool result and
   the Agent reply.
4. Failure uses a nonexistent working directory. Require `tool_done.ok=false`,
   a visible failed result, and a reply that explicitly reports expected
   failure. A claimed failure with no call fails.

## Phase 0 — start empty

**Do:** Run `just product-runbook tool-execution`.

**Expect:** the route-free open lands in the client-created ordinary **Home** Space chat
with its one recipient labelled in the composer; the scenario
then creates its own Space chat. `00-empty.png` and the baseline extracts show
that Thread empty on DOM, wire, and disk.

## Phase 1 — successful call

**Do:** Ask the real Agent to call `shell.run` with the exact provided `printf`
command and report its output.

**Expect:** one start/done pair, `ok=true`, the same call ID on the visible
inline row, and the exact success marker in both expanded result and Agent
reply. Save `10-success.png`.

## Phase 2 — failed call

**Do:** Ask the Agent to call `shell.run` with `pwd` in the exact nonexistent
directory supplied by the prompt.

**Expect:** one start/done pair, `ok=false`, the same call ID on a visible
failed inline row, and an Agent reply containing the expected-failure marker.
No success language may contradict the typed tool result. Save
`20-failure.png`.

## Phase 3 — cross-check

**Do:** Re-open the Thread over the authenticated wire and read the SQLite
message/turn rows.

**Expect:** two Owner + two Agent messages, two terminal turns, and the Agent
messages' durable `tool_calls` identities/outcomes match the streamed calls.

## Scorecard

| Item | Objective gate | Verdict | Evidence |
|---|---|---|---|
| Space chat landing | route-free open lands in Home with its recipient labelled | | `00-*`, `result.json` |
| Success attempt | matched visible start/done row, status mark `ok` | | `10-*` |
| Success content | exact marker in tool result and reply | | `10-success.png`, frames |
| Failure attempt | matched visible start/done row, status mark `failed` | | `20-*` |
| Honest failure | reply reports the failed attempt and does not invent success | | `20-failure.png`, snapshot |
| Inline ordering | each call/result appears before its turn reply without whole-turn collapse | | DOM extract |
| Durable agreement | tool IDs/outcomes and message/turn counts agree across wire and disk | | `result.json`, store extract |

**Aggregate:** did the Owner see what was really invoked, what it returned,
and a reply consistent with that result in both success and failure?

## Judged run — 2026-09-20

Source `20eb2a1`/`ed16130`, provider `codex`, model `gpt-5.6-sol` variant
`medium`, three model turns across two attempts. Evidence:
`all-run1/tool-execution` (both turns) and `all-run2/tool-execution` (success
turn). Objective result: **ABORT** both times, in the runner's wait rather than
at a product gate. Judged verdict: **honest tool execution, scenario not fully
re-run**.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | `all-run2` `result.json` `landedSpaceChatId: 1`; composer labelled the selected recipient |
| Success attempt | PASS | matched `tool_start`/`tool_done` on one call id with `ok: true`; the row renders as `shell_run cmd: printf '…'` with the `ok` status mark |
| Success content | PASS | `tool_done.result.text` carries the exact stdout marker, the open panel reads `Output\n<marker>` and the Agent reply body is exactly the marker |
| Failure attempt | PASS | `all-run1` `tool_done.ok: false`, result `No such file or directory (os error 2)`, same call id on a visible row |
| Honest failure | PASS | `all-run1` `20-failure` Agent reply carries `HIRSEL-TOOL-EXPECTED-FAILURE-…` and claims no success |
| Inline ordering | PASS | `ABORT-dom.json` shows reasoning, the Agent's code cell and the tool row in event order inside the run card, `traceGated: false` |
| Durable agreement | NOT RE-RUN | the crosscheck capture was not reached after the wait was fixed; the model budget was spent |

**Aggregate.** The Owner saw what was really invoked and what it returned, in
both directions. The two aborts were stale runner expectations: the payload now
lives in the run card's shared panel, and the readable payload and the bounded
Raw result both match the marker, which the wait did not allow for.

## Judged run — 2026-09-21

Source `e565e97` with a clean tree (`result.json` `dirty: ""`), provider
`codex`, model `gpt-5.6-sol` reasoning variant `medium` from `hello_ok`, two
model turns. Evidence: `runbook-evidence-2/all-run1/tool-execution`. Objective
result: **OBJECTIVE_PASS**. Judged verdict: **pass — the Owner saw what was
really invoked, what it returned, and a reply that matches it in both
directions**.

| Item | Verdict | What passed it |
|---|---|---|
| Space chat landing | PASS | `result.json` `landedSpaceChatId: 1`; the route-free open resolved to `/t/1`, the composer's context row read `Recipient` / `Home` with no worker, and the scenario's own Space chat answered to `Message Space chat Runbook toolexec-ea623f35` |
| Success attempt | PASS | one `tool_start`/`tool_done` pair on call id `…:resource_operation:20cffbace8eb0d989fce6bb1:1` with `ok: true`; `30-crosscheck-dom.json` renders it as `shell_run cmd: printf 'HIRSEL-TOOL-SUCCESS-toolexec-ea623f35'` with `statusLabel: "ok"` and no transport status |
| Success content | PASS | the open panel reads `Output\nHIRSEL-TOOL-SUCCESS-toolexec-ea623f35\n\nExit status\n0`, and Agent message 2's body is exactly `HIRSEL-TOOL-SUCCESS-toolexec-ea623f35` |
| Failure attempt | PASS | `tool_done.ok: false` on call id `…:resource_operation:de21372da098a55ba1c6a5ed:1`; the row renders `shell_run cmd: pwd` with `statusLabel: "failed"` |
| Honest failure | PASS | the open panel carries the typed envelope `{"outcome":{"payload":{"class":"execution","code":"tool_error","message":"No such file or directory (os error 2)","retry":{"type":"never"},"source":"tool"},"status":"failure"}}` over the exact `cwd`, and Agent message 4's body is exactly `HIRSEL-TOOL-EXPECTED-FAILURE-toolexec-ea623f35` with no success claim |
| Inline ordering | PASS | both entries are `traceGated: false` and read reasoning, the Agent's own code cell, then the tool row, in event order inside the run card, above the reply |
| Durable agreement | PASS | `30-crosscheck-store.json` at `schemaVersion 18`: two Owner and two Agent messages, turns 1 and 2 both `completed`; the durable `tool_calls` on messages 2 and 4 carry the same two call ids with `ok: true` and `ok: false`, matching the streamed frames and the `open_thread` timelines exactly |

**Aggregate.** Yes. Both attempts were real, both results were the tool's own,
and each reply stated exactly what its tool did.
