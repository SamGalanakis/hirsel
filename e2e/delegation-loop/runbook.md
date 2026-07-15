# Delegation Loop Runbook

## Purpose

Prove slice 1's loop with the debug HTTP surface: Owner message -> Agent delegates to a Sub-agent -> Sub-agent terminal event is observable -> Agent sends a named requires-response Ping with a Quick Reply -> Owner sends the Quick Reply as an Anchor-refed Chat message -> the host resolves the Ping -> Agent acknowledges in Chat.

## Scenario A: Scripted Test Double

Use this deterministic mode when no LLM credentials are available. It exercises Hirsel storage, debug HTTP, WebSocket broadcasts, Sub-agent Driver lifecycle, Pings, Quick Replies, and Anchor-refed Chat. It does not prove RLM behavior.

Execute the scripted scenario directly with:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
e2e/delegation-loop/run.sh
```

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
ROOT=/workspace/code/hirsel-rbcov
source "$ROOT/e2e/lib/runbook-lib.sh"
PORT="$(choose_port 3260)"
BASE="http://127.0.0.1:$PORT"
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-scripted
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

Run the gates:

1. Reset:

```bash
curl -sS -X POST "$BASE/debug/reset"
```

2. Inject the Owner delegation request:

```bash
curl -sS -X POST "$BASE/debug/owner-message" \
  -H 'content-type: application/json' \
  -d '{"body":"Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-delegation-work (create the directory if needed), then ask me before applying the result.","ref":null}'
```

3. Poll `/debug/processes` until at least one `kind: "subagent"` process exists. Record `processes[0].id`.

4. Poll `/debug/processes` until that process has `state: "done"` and a terminal summary.

5. Poll `/debug/pings` until there is an open Ping with non-empty `name` and `description`, `requires_response: true`, and at least one `quick_replies` entry. Record its `id`, `anchor`, and the first Quick Reply `value`.

6. Send the Quick Reply as an Anchor-refed Owner message:

```bash
curl -sS -X POST "$BASE/debug/owner-message" \
  -H 'content-type: application/json' \
  -d '{"body":"ship it","ref":ANCHOR_ID}'
```

Replace `ANCHOR_ID` with the Ping's `anchor`.

7. Poll `/debug/pings` until the Ping is `status: "done"` and `/debug/broadcasts` contains its done `ping_upsert`; the host performs this transition before the Agent turn, with no `pings.resolve` call.

8. Poll `/debug/chat` until an Agent-authored message appears after the Quick Reply acknowledging the Ping reply.

## Scenario B: Real RLM Agent With Codex

Use this mode to prove a real Lash RLM turn, real provider call, Lashlang-bound Hirsel tools, and fake Sub-agent Driver integration. It requires existing Codex OAuth credentials in `~/.codex/auth.json`.

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
ROOT=/workspace/code/hirsel-rbcov
source "$ROOT/e2e/lib/runbook-lib.sh"
PORT="$(choose_port 3260)"
BASE="http://127.0.0.1:$PORT"
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-codex
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

First prove the tool-call smoke:

```bash
curl -sS -X POST "$BASE/debug/reset"
curl -sS -X POST "$BASE/debug/owner-message" \
  -H 'content-type: application/json' \
  -d '{"body":"reply with exactly the word pong","ref":null}'
```

Poll `/debug/chat` until an Agent-authored message contains `pong`.

Then run the same delegation gates from Scenario A. If the Agent does not send a Ping or does not choose a Quick Reply for prompt-quality reasons, preserve the `/debug/chat`, `/debug/processes`, and `/debug/pings` transcript and treat it as prompt tuning work rather than manually forcing success.

After the real delegation turn uses tools, also prove tool-call visibility:

```bash
curl -sS "$BASE/debug/chat" | jq '.messages[] | select(.author == "agent" and (.tool_calls | length > 0))'
curl -sS "$BASE/debug/broadcasts" | jq '.events[] | select(.type == "turn_event" and (.event.kind == "tool_start" or .event.kind == "tool_done"))'
```

Both commands must match at least one row/event from the real turn.

## Success Gates

- `/debug/health` returns `ok: true`.
- `/debug/processes` shows a Sub-agent process.
- The process reaches terminal `state: "done"` through the fake driver.
- Real Lash runs show persisted Agent `tool_calls` and live `turn_event` tool broadcasts after a tool-using turn.
- `/debug/pings` contains a named, described, requires-response Ping with a Quick Reply.
- The Quick Reply is sent as a normal Owner Chat message with `ref` equal to the Ping Anchor.
- That reply auto-resolves the Ping to done without an Agent resolve call.
- `/debug/chat` contains a later Agent acknowledgement.

The run is void if the tester posts the Ping or acknowledgement manually.
