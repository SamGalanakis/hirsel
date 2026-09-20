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
5. Tool telemetry is not effect evidence. These two `shell.run` calls touch no
   Thread or artifact, so they must not fabricate a durable effect pill from
   `tool_done.ok` or the assistant's summary. Thread/artifact tools instead
   require their own transactional effect receipt, which remains visible while
   the source turn runs and after failure or reload.

## Phase 0 — start empty

**Do:** Run `just product-runbook tool-execution`.

**Expect:** the route-free open lands in the bootstrapped **Home** Space chat
with Space, Focus and Worker separately labelled in the composer; the scenario
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
The turn details have no effect receipts for these shell-only calls.

## Scorecard

| Item | Objective gate | Verdict | Evidence |
|---|---|---|---|
| Space chat landing | route-free open lands in Home with Space, Focus and Worker labelled | | `00-*`, `result.json` |
| Success attempt | matched visible start/done row, status mark `ok` | | `10-*` |
| Success content | exact marker in tool result and reply | | `10-success.png`, frames |
| Failure attempt | matched visible start/done row, status mark `failed` | | `20-*` |
| Honest failure | reply reports the failed attempt and does not invent success | | `20-failure.png`, snapshot |
| Inline ordering | each call/result appears before its turn reply without whole-turn collapse | | DOM extract |
| Durable agreement | tool IDs/outcomes and message/turn counts agree across wire and disk | | `result.json`, store extract |
| No invented effects | shell telemetry produces no Thread/artifact effect receipt or pill | | wire and store extracts |

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
| Space chat landing | PASS | `all-run2` `result.json` `landedSpaceChatId: 1`; composer labelled Space/Focus/Worker |
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
