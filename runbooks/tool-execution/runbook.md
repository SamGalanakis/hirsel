# Product scenario: visible and honest tool execution

> Read [../RULES.md](../RULES.md) first.

**Purpose.** Prove that real tool attempts appear inline, update in place with
their actual result, and support an honest Agent answer for both success and
failure.

**Real model calls:** two.

## Golden rules

1. Prompt text and Agent claims are not tool evidence. Require matched
   `tool_start`/`tool_done` frames with one stable call ID.
2. The tool row is visible inline without opening a turn-wide disclosure.
   Expanding that individual row to inspect its payload is allowed.
3. Success requires the noncoincidental stdout marker in the tool result and
   the Agent reply.
4. Failure uses a nonexistent working directory. Require `tool_done.ok=false`,
   a visible failed result, and a reply that explicitly reports expected
   failure. A claimed failure with no call fails.

## Phase 0 — start empty

**Do:** Run `just product-runbook tool-execution`.

**Expect:** `00-empty.png` and the baseline extracts show an empty selected
Thread on DOM, wire, and disk.

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
| Success attempt | matched visible start/done row, `ok=true` | | `10-*` |
| Success content | exact marker in tool result and reply | | `10-success.png`, frames |
| Failure attempt | matched visible start/done row, `ok=false` | | `20-*` |
| Honest failure | reply reports the failed attempt and does not invent success | | `20-failure.png`, snapshot |
| Inline ordering | each call/result appears before its turn reply without whole-turn collapse | | DOM extract |
| Durable agreement | tool IDs/outcomes and message/turn counts agree across wire and disk | | `result.json`, store extract |

**Aggregate:** did the Owner see what was really invoked, what it returned,
and a reply consistent with that result in both success and failure?
