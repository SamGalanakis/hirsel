#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3130)"
DATA_DIR="/tmp/hirsel-e2e-abandoned-recovery-data"
HOST_LOG="/tmp/hirsel-e2e-abandoned-recovery-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5
FIXTURE="/tmp/hirsel-e2e-abandoned-recovery-fixture.json"
HIRSEL_FAKE_FIXTURE="$FIXTURE"
trap 'stop_hirsel_host TERM' EXIT

cat >"$FIXTURE" <<'JSON'
{
  "external_id": "fake-abandoned-long",
  "progress": ["long fake work still running"],
  "delay_ms": 60000,
  "terminal": { "status": "done", "summary": "should not complete before kill" }
}
JSON

WORK="/tmp/hirsel-e2e-abandoned-recovery-work"
rm -rf "$WORK"
mkdir -p "$WORK"

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "debug reset"

BODY="$(cat <<EOF
Start one Sub-agent now using subagents.spawn with agent "codex", explicit model "gpt-5", and cwd "$WORK". The task may be long. Do not start any sibling Sub-agent. Reply after spawning; do not wait for terminal completion.
EOF
)"
REQ="$(jq -nc --arg body "$BODY" '{client_id:"abandoned-start",body:$body,ref:null}')"
post_json debug/owner-message "$REQ" >/dev/null

PROC_JSON="$(wait_jq debug/processes '.processes[] | select(.kind == "subagent" and .state == "running")' 180)"
PROC_ID="$(printf '%s' "$PROC_JSON" | jq -r '.processes[] | select(.kind == "subagent" and .state == "running") | .id' | tail -1)"
pass_gate "fake-driver subagent running before SIGKILL: $PROC_ID"

debug_snapshot /tmp/hirsel-e2e-abandoned-recovery-before-kill
kill -KILL "$HOST_PID"
wait "$HOST_PID" 2>/dev/null || true
HOST_PID=""
pass_gate "host killed with SIGKILL while $PROC_ID was running"

start_hirsel_host preserve
pass_gate "host rebooted on same data dir"

assert_no_jq_for debug/processes '.processes[] | select(.kind == "subagent" and .state == "running")' 20
pass_gate "no running subagent auto-respawned after reboot"

wait_jq debug/processes '.processes[] | select(.id == "'"$PROC_ID"'" and .state == "abandoned")' 45 >/dev/null
pass_gate "abandoned process surfaced after reboot: $PROC_ID"

wait_jq debug/chat '.messages[] | select(.author == "agent" and (.body | ascii_downcase | test("abandon|orphan|recover|sub-agent|subagent")))' 90 >/dev/null
pass_gate "Agent visibly judged orphaned work after reboot"

debug_snapshot /tmp/hirsel-e2e-abandoned-recovery-final
