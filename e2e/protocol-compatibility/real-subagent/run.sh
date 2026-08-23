#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/protocol-compatibility/lib/runbook-lib.sh"

PORT="$(choose_port 3120)"
DATA_DIR="/tmp/hirsel-e2e-real-subagent-data"
HOST_LOG="/tmp/hirsel-e2e-real-subagent-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=real
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

FIX_REPO="/tmp/hirsel-e2e-real-subagent-fix-repo"
rm -rf "$FIX_REPO"
mkdir -p "$FIX_REPO"
(
  cd "$FIX_REPO"
  git init -q
  git config user.email e2e@example.com
  git config user.name "E2E"
  printf 'def add(a, b):\n    return a - b\n' > calc.py
  printf 'import unittest\nfrom calc import add\n\nclass CalcTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == "__main__":\n    unittest.main()\n' > test_calc.py
  git add calc.py test_calc.py
  git commit -q -m initial
)
if (cd "$FIX_REPO" && python3 -m unittest -v >/tmp/hirsel-e2e-real-subagent-initial-test.log 2>&1); then
  fail_gate "fixture test should start failing"
  exit 1
fi
pass_gate "fixture repo starts with failing unittest"

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "debug reset"

BODY="$(cat <<EOF
Delegate this repo fix to a Codex Sub-agent now. Use subagents.spawn with agent "codex", explicit model "gpt-5.6-sol", and cwd "$FIX_REPO". The Sub-agent task is: run python3 -m unittest -v, fix the implementation, and leave the tests passing. Finish this turn immediately after spawning; do not wait or poll. The terminal event will wake you to report the outcome in Chat or a Ping according to your conventions.
EOF
)"
REQ="$(jq -nc --arg body "$BODY" '{client_id:"real-subagent-fix",body:$body,ref:null}')"
OWNER_JSON="$(post_json debug/owner-message "$REQ")"
OWNER_ID="$(printf '%s' "$OWNER_JSON" | jq -r '.message.id')"

PROC_JSON="$(wait_jq debug/processes '.processes[] | select(.kind == "subagent" and .agent == "codex" and .model == "gpt-5.6-sol")' 240)"
PROC_ID="$(printf '%s' "$PROC_JSON" | jq -r '.processes[] | select(.kind == "subagent" and .agent == "codex" and .model == "gpt-5.6-sol") | .id' | tail -1)"
pass_gate "real codex process visible with explicit model: $PROC_ID"

wait_jq debug/broadcasts '[.events[] | select(.type == "process_upsert" and .process.id == "'"$PROC_ID"'")] | length >= 2' 360 >/dev/null
pass_gate "process_upsert progress accumulated for $PROC_ID"

BEFORE_TERMINAL_CHAT="$(max_chat_id)"
BEFORE_TERMINAL_PING_COUNT="$(get_json debug/pings | jq '.pings | length')"
wait_jq debug/processes '.processes[] | select(.id == "'"$PROC_ID"'" and .state == "done")' 900 >/dev/null
pass_gate "real codex process reached done: $PROC_ID"

deadline=$((SECONDS + 180))
REPORTED=0
while (( SECONDS < deadline )); do
  CHAT_MATCH="$(get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and .id > ('"$BEFORE_TERMINAL_CHAT"'))' 2>/dev/null || true)"
  PING_COUNT="$(get_json debug/pings | jq '.pings | length')"
  if [[ -n "$CHAT_MATCH" ]] || (( PING_COUNT > BEFORE_TERMINAL_PING_COUNT )); then
    REPORTED=1
    break
  fi
  sleep 0.5
done
if (( REPORTED == 0 )); then
  debug_snapshot /tmp/hirsel-e2e-real-subagent-report-miss
  fail_gate "Agent did not report after terminal wake"
  exit 2
fi
pass_gate "Agent reported after terminal wake"

(cd "$FIX_REPO" && python3 -m unittest -v)
pass_gate "repo unittest passes after Sub-agent completion"

LONG_REPO="/tmp/hirsel-e2e-real-subagent-long-repo"
rm -rf "$LONG_REPO"
mkdir -p "$LONG_REPO"
(
  cd "$LONG_REPO"
  git init -q
  git config user.email e2e@example.com
  git config user.name "E2E"
  printf 'long task fixture\n' > README.md
  git add README.md
  git commit -q -m initial
)

BODY="$(cat <<EOF
Start a long-running Codex Sub-agent now and do not wait for it. Use subagents.spawn with agent "codex", explicit model "gpt-5.6-sol", and cwd "$LONG_REPO". The Sub-agent task is: run python3 -c 'import time; time.sleep(180)' and do not finish until that command returns. Reply in Chat after spawning with the process id.
EOF
)"
REQ="$(jq -nc --arg body "$BODY" '{client_id:"real-subagent-long",body:$body,ref:null}')"
post_json debug/owner-message "$REQ" >/dev/null

LONG_JSON="$(wait_jq debug/processes '.processes[] | select(.kind == "subagent" and .agent == "codex" and .model == "gpt-5.6-sol" and .state == "running" and .id != "'"$PROC_ID"'")' 240)"
LONG_PROC_ID="$(printf '%s' "$LONG_JSON" | jq -r '.processes[] | select(.kind == "subagent" and .agent == "codex" and .model == "gpt-5.6-sol" and .state == "running" and .id != "'"$PROC_ID"'") | .id' | tail -1)"
pass_gate "long-running real codex process visible: $LONG_PROC_ID"

STOP_BODY="Stop process $LONG_PROC_ID now. Use subagents.interrupt on exactly this process id, then reply after the interrupt request."
STOP_REQ="$(jq -nc --arg body "$STOP_BODY" '{client_id:"real-subagent-stop",body:$body,ref:null}')"
post_json debug/owner-message "$STOP_REQ" >/dev/null

wait_jq debug/processes '.processes[] | select(.id == "'"$LONG_PROC_ID"'" and (.state == "cancelled" or .state == "failed"))' 240 >/dev/null
pass_gate "interrupt produced terminal non-done state for $LONG_PROC_ID"

debug_snapshot /tmp/hirsel-e2e-real-subagent-final
