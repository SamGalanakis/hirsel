#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3200)"
DATA_DIR="/tmp/hirsel-e2e-delegation-loop-scripted"
HOST_LOG="/tmp/hirsel-e2e-delegation-loop-scripted-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
OWNER_JSON="$(post_json debug/owner-message '{"client_id":"delegation-scripted","body":"Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-delegation-loop-work, then ask me before applying the result.","ref":null}')"
OWNER_ID="$(printf '%s' "$OWNER_JSON" | jq -r '.message.id')"
pass_gate "Owner delegation message persisted as $OWNER_ID"

PROCESS_JSON="$(wait_jq debug/processes '.processes[] | select(.kind == "subagent" and .state == "done")' 60)"
PROCESS_ID="$(printf '%s' "$PROCESS_JSON" | jq -r '.processes[] | select(.kind == "subagent" and .state == "done") | .id' | tail -1)"
pass_gate "Sub-agent process $PROCESS_ID reached done"

PING_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and (.quick_replies | length > 0) and (.name | length > 0) and (.description | length > 0))' 60)"
PING_ID="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.status == "open") | .id' | tail -1)"
ANCHOR_ID="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.id == '"$PING_ID"') | .anchor')"
QUICK_REPLY="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.id == '"$PING_ID"') | .quick_replies[0].value')"
pass_gate "Ping $PING_ID has name, description, Quick Reply, and Anchor $ANCHOR_ID"

REPLY_JSON="$(post_json debug/owner-message "$(jq -nc --arg body "$QUICK_REPLY" --argjson ref "$ANCHOR_ID" '{client_id:"delegation-quick-reply",body:$body,ref:$ref}')")"
REPLY_ID="$(printf '%s' "$REPLY_JSON" | jq -r '.message.id')"
wait_jq debug/pings '.pings[] | select(.id == '"$PING_ID"' and .status == "done")' 10 >/dev/null
pass_gate "Quick Reply $REPLY_ID auto-resolved Ping $PING_ID"
wait_agent_message_after "$REPLY_ID" '(.body | contains("Acknowledged"))' 30
pass_gate "Agent acknowledged the Ping reply"
debug_snapshot /tmp/hirsel-e2e-delegation-loop-scripted-final
