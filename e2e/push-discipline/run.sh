#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3320)"
DATA_DIR="/tmp/hirsel-e2e-push-discipline"
HOST_LOG="/tmp/hirsel-e2e-push-discipline-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
PUSH_TOKEN="e2e-push-discipline-token"
trap 'stop_hirsel_host TERM' EXIT

events() { get_json debug/events; }

create_judgment() {
  local client_id="$1"
  local excluded_id="${2:-0}"
  post_json debug/owner-message "$(jq -nc \
    --arg client_id "$client_id" \
    --arg body "Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-push-discipline-work (create it if needed), then ask me before applying the result." \
    '{client_id:$client_id,body:$body,ref:null}')" >/dev/null
  wait_jq debug/events '.events[] | select(.id != '"$excluded_id"' and .kind=="judgment" and .status=="open" and .requires_response==true)' 60 \
    | jq -r '[.events[] | select(.id != '"$excluded_id"' and .kind=="judgment" and .status=="open" and .requires_response==true)] | last | .id'
}

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null

# ---- Gate 1: registration is an idempotent refresh, not an append ----
REGISTERED="$(post_json debug/register-push-token \
  '{"platform":"android","token":"'"$PUSH_TOKEN"'"}')"
REFRESHED="$(post_json debug/register-push-token \
  '{"platform":"web","token":"'"$PUSH_TOKEN"'"}')"
jq -ne --argjson first "$REGISTERED" --argjson second "$REFRESHED" \
  '$first.token==$second.token and $first.created_ts==$second.created_ts and
   $second.platform=="web" and $second.last_seen_ts >= $first.last_seen_ts' >/dev/null
pass_gate "Gate 1: re-register refreshed $PUSH_TOKEN in place (created_ts stable, platform web, last_seen advanced)"

# ---- Gate 2: one requires_response judgment produces exactly one one-recipient push ----
EVENT_ID="$(create_judgment push-first)"
wait_jq debug/pushes \
  '[.pushes[] | select(.payload.data.event_id=='"$EVENT_ID"')] | length == 1' 15 >/dev/null
get_json debug/pushes | jq -e \
  '.pushes | length==1 and .[0].payload.data.event_id=='"$EVENT_ID"' and .[0].tokens==["'"$PUSH_TOKEN"'"]' >/dev/null
pass_gate "Gate 2: judgment $EVENT_ID produced exactly one push to one idempotently registered token"

# ---- Gate 3: Chat, summary awareness, read, and resolve never add pushes ----
BEFORE_CHAT="$(max_chat_id)"
post_json debug/owner-message \
  '{"client_id":"push-negative-chat","body":"pong push negative check","ref":null}' >/dev/null
wait_agent_message_after "$BEFORE_CHAT" '(.body | ascii_downcase | contains("pong"))' 15
SUMMARY_ID="$(post_json debug/trigger-digest \
  '{"job_id":"push-discipline-digest","text":"Quiet test summary.","status":"ready"}' | jq -r '.id')"
post_json debug/read-ping '{"ping_id":'"$EVENT_ID"'}' >/dev/null
post_json debug/resolve-ping '{"ping_id":'"$EVENT_ID"'}' >/dev/null
assert_no_jq_for debug/pushes '.pushes | length != 1' 3
get_json debug/pushes | jq -e \
  '[.pushes[] | select(.payload.data.event_id=='"$SUMMARY_ID"')] | length==0' >/dev/null
pass_gate "Gate 3: Chat, summary $SUMMARY_ID, read, and resolve produced zero new pushes"

# ---- Gate 4: unregister is idempotent and prevents later judgment delivery ----
post_json debug/unregister-push-token '{"token":"'"$PUSH_TOKEN"'"}' \
  | jq -e '.removed==true' >/dev/null
post_json debug/unregister-push-token '{"token":"'"$PUSH_TOKEN"'"}' \
  | jq -e '.removed==false' >/dev/null
SECOND_ID="$(create_judgment push-after-unregister "$EVENT_ID")"
assert_no_jq_for debug/pushes \
  '.pushes[] | select(.payload.data.event_id=='"$SECOND_ID"')' 4
get_json debug/pushes | jq -e '.pushes | length==1' >/dev/null
pass_gate "Gate 4: unregister returned true then false and stopped judgment $SECOND_ID from pushing"

debug_snapshot /tmp/hirsel-e2e-push-discipline-final
events >/tmp/hirsel-e2e-push-discipline-final.events.json
get_json debug/pushes >/tmp/hirsel-e2e-push-discipline-final.pushes.json
pass_gate "push-discipline: all gates passed"
