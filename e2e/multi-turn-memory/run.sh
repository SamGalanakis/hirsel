#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3140)"
DATA_DIR="/tmp/hirsel-e2e-multi-turn-memory-data"
HOST_LOG="/tmp/hirsel-e2e-multi-turn-memory-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "debug reset"

send_and_wait() {
  local client_id="$1"
  local body="$2"
  local before
  before="$(max_chat_id)"
  post_json debug/owner-message "$(jq -nc --arg id "$client_id" --arg body "$body" '{client_id:$id,body:$body,ref:null}')" >/dev/null
  wait_agent_message_after "$before" 'true' 180
}

send_and_wait memory-1 'Remember this exact project codename for later: ORCHID-17.'
pass_gate "message 1 completed"
send_and_wait memory-2 'Also remember this exact delivery window for later: THURSDAY-MORNING.'
pass_gate "message 2 completed"

before="$(max_chat_id)"
post_json debug/owner-message "$(jq -nc --arg body 'Using only the earlier conversation, reply exactly: CODENAME=<codename>; WINDOW=<delivery window>.' '{client_id:"memory-3",body:$body,ref:null}')" >/dev/null
wait_agent_message_after "$before" '(.body | contains("ORCHID-17") and contains("THURSDAY-MORNING"))' 180
pass_gate "message 3 recalled both earlier facts"

if get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and (.body | contains("Agent turn failed")))' >/dev/null; then
  fail_gate "Agent turn failed before restart"
  exit 1
fi

restart_hirsel_host
pass_gate "host restarted on same data dir"

before="$(max_chat_id)"
post_json debug/owner-message "$(jq -nc --arg body 'After the restart, use the earlier conversation and reply exactly: CODENAME=<codename>; WINDOW=<delivery window>.' '{client_id:"memory-4",body:$body,ref:null}')" >/dev/null
wait_agent_message_after "$before" '(.body | contains("ORCHID-17") and contains("THURSDAY-MORNING"))' 180
pass_gate "message 4 recalled both facts after restart"

if get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and (.body | contains("Agent turn failed")))' >/dev/null; then
  fail_gate "Agent turn failed after restart"
  exit 1
fi

debug_snapshot /tmp/hirsel-e2e-multi-turn-memory-final
