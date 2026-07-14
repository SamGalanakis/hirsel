# Interactive Orchestration Runbook

## Purpose

Prove the three "keep main chat interactive" guarantees shipped together in July 2026:

1. **Wake bias** — a delegation turn ENDS while the Sub-agent is still running (no in-turn
   awaiting); the terminal event wakes the Agent, which then reports.
2. **Chat stays interactive** — while a Sub-agent runs, a fresh warm question gets a Chat answer
   before the delegated work finishes.
3. **Full terminal payloads** — a Sub-agent's long final report reaches the Agent UNTRUNCATED (the
   old bug cut it at 240 chars mid-sentence and the Agent re-ran the work).

Real-Agent (Codex OAuth in `~/.codex/auth.json`); `HIRSEL_DRIVER=fake` where terminal timing must be
deterministic (gates 1–2), REAL driver for gate 3 (the payload must come from a real Sub-agent).

## Shared Helpers

Use the standard `post_json` / `wait_jq` / `assert_no_jq_for` / `max_chat_id` helpers from
`e2e/channel-discipline/runbook.md`.

Host env (verified-free port, never 3089):

```bash
export HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_TOKEN=dev-token HIRSEL_DEBUG=1
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-interactive
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
# gates 1-2: HIRSEL_DRIVER=fake ; gate 3: real driver (unset HIRSEL_DRIVER)
cargo run -p hirsel-host
```

## Gate 1: the delegation turn ends while the work is still running

```bash
post_json debug/reset '{}'
BEFORE="$(max_chat_id)"
post_json debug/owner-message '{"client_id":"io-deleg","body":"Delegate a LONG task to a Sub-agent in /tmp/hirsel-e2e-io-work (create it) — a slow full-repo audit. Hand it off and let me know when it finishes; do not wait for it.","ref":null}' >/dev/null

# A Sub-agent comes up...
PROC="$(wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 120 | jq -r '[.processes[] | select(.kind=="subagent")] | last | .id')"
# ...and the Agent's turn ENDS while that process is still running: agent_activity goes idle
# with the process not yet terminal. (The wake-bias mechanical gate.)
wait_jq debug/broadcasts '[.events[] | select(.type=="agent_activity")] | last | .state == "idle"' 120 >/dev/null
curl -sS "$BASE/debug/processes" | jq -e '.processes[] | select(.id=="'"$PROC"'" and .state=="running")'
```

If the fake driver completes too fast to observe `idle`-while-`running`, configure the fixture with a
longer delay (the fake driver supports `delay_ms`) or gate on the ordering in `/debug/broadcasts`
(the idle `agent_activity` precedes the terminal `process_upsert`).

## Gate 2: chat answers while the fleet works

```bash
BEFORE="$(max_chat_id)"
post_json debug/owner-message '{"client_id":"io-warm","body":"Quick one while that runs: in one word, is 23 prime?","ref":null}' >/dev/null
# The warm answer lands BEFORE the delegated process reaches terminal state.
wait_jq debug/chat '.messages[] | select(.author=="agent" and .id > '"$BEFORE"')' 60 >/dev/null
curl -sS "$BASE/debug/processes" | jq -e '.processes[] | select(.id=="'"$PROC"'" and .state=="running")' \
  || echo "note: process finished first — rerun with a slower fixture; the gate is answer-before-terminal"
# Then the terminal wake still closes the loop (report lands after done).
wait_jq debug/processes '.processes[] | select(.id=="'"$PROC"'" and .state=="done")' 180 >/dev/null
wait_jq debug/chat '.messages[] | select(.author=="agent") | select(.id > '"$BEFORE"')' 120 >/dev/null
```

## Gate 3: long payloads survive the hand-off (REAL driver)

Restart the host with the real driver (same data dir is fine after a reset). The Sub-agent is
instructed to end its report with a sentinel that sits far beyond the old 240-char cut.

```bash
post_json debug/reset '{}'
BEFORE="$(max_chat_id)"
post_json debug/owner-message '{"client_id":"io-payload","body":"Delegate to a Sub-agent in /tmp/hirsel-e2e-io-payload (create it): write a ~500 word summary of what a message queue is, and END the report with the exact token PAYLOAD-INTACT-9471 on its own line. When it completes, relay the full report to me here in chat, including that final token.","ref":null}' >/dev/null

PROC="$(wait_jq debug/processes '.processes[] | select(.kind=="subagent")' 180 | jq -r '[.processes[] | select(.kind=="subagent")] | last | .id')"
wait_jq debug/processes '.processes[] | select(.id=="'"$PROC"'" and .state=="done")' 600 >/dev/null
# Mechanical: the sentinel from beyond char 240 reached the Agent and its relay.
wait_jq debug/chat '.messages[] | select(.author=="agent" and (.body | contains("PAYLOAD-INTACT-9471")))' 180 >/dev/null
# And nothing the Agent consumed shows the old silent-cut ellipsis on the report path.
curl -sS "$BASE/debug/chat" | jq -e '[.messages[] | select(.author=="agent" and (.body | contains("truncated by hirsel")))] | length == 0'
```

## Success Gates

- Gate 1: `agent_activity` idle while the delegated process is still `running` (turn ended, no
  in-turn await).
- Gate 2: a warm question answered in Chat before the delegated work reached terminal; the terminal
  wake still produced the close-the-loop report.
- Gate 3: a real Sub-agent's sentinel beyond char 240 reached the Agent's relay intact; no
  hirsel-truncation marker on a normal-sized report.

## Report

Per `e2e/RULES.md`: record the process ids, the idle-before-terminal broadcast ordering, the warm
answer's Chat id vs the terminal timestamp, and the sentinel-bearing Chat id. Wording is a
model-behavior finding; the ordering and the intact sentinel are the mechanical gates.
