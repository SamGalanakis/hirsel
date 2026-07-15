#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3300)"
DATA_DIR="/tmp/hirsel-e2e-event-archive-undo"
HOST_LOG="/tmp/hirsel-e2e-event-archive-undo-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

events() { get_json debug/events; }

create_judgment() {
  local client_id="$1"
  local excluded_id="${2:-0}"
  post_json debug/owner-message "$(jq -nc \
    --arg client_id "$client_id" \
    --arg body "Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-event-archive-undo-work (create it if needed), then ask me before applying the result." \
    '{client_id:$client_id,body:$body,ref:null}')" >/dev/null
  wait_jq debug/events '.events[] | select(.id != '"$excluded_id"' and .kind=="judgment" and .status=="open" and .requires_response==true and .archived==false)' 60 \
    | jq -r '[.events[] | select(.id != '"$excluded_id"' and .kind=="judgment" and .status=="open" and .requires_response==true and .archived==false)] | last | .id'
}

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null

# ---- Gate 1: archive an OPEN judgment and observe its authoritative upsert ----
EVENT_ID="$(create_judgment archive-first)"
ARCHIVED="$(post_json debug/event-action \
  '{"event_id":'"$EVENT_ID"',"action":"archive","data":{}}')"
printf '%s' "$ARCHIVED" | jq -e \
  '.id=='"$EVENT_ID"' and .status=="done" and .archived==true and .archived_at != null' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type=="event_upsert" and .event.id=='"$EVENT_ID"' and .event.status=="done" and .event.archived==true and .event.archived_at != null)' 10 >/dev/null
pass_gate "Gate 1: archive auto-dismissed open judgment $EVENT_ID, stamped archived_at, and broadcast event_upsert"

# ---- Gate 2: archived rows leave the live feed but remain in durable event history ----
events | jq -e '.events[] | select(.id=='"$EVENT_ID"' and .archived==true)' >/dev/null
events | jq -e '[.events[] | select(.archived==false) | .id] | index('"$EVENT_ID"') == null' >/dev/null
pass_gate "Gate 2: archived judgment $EVENT_ID left the unarchived live feed and remained in /debug/events"

# ---- Gate 3: unarchive is deliberately only half of undo ----
UNARCHIVED="$(post_json debug/event-action \
  '{"event_id":'"$EVENT_ID"',"action":"unarchive","data":{}}')"
printf '%s' "$UNARCHIVED" | jq -e \
  '.id=='"$EVENT_ID"' and .archived==false and .archived_at==null and .status=="done"' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type=="event_upsert" and .event.id=='"$EVENT_ID"' and .event.archived==false and .event.archived_at==null and .event.status=="done")' 10 >/dev/null
pass_gate "Gate 3: unarchive cleared archived fields for $EVENT_ID but honestly left status=done"

# ---- Gate 4: reopen completes the honest undo pair ----
post_json debug/reopen-ping '{"ping_id":'"$EVENT_ID"'}' \
  | jq -e '.id=='"$EVENT_ID"' and .status=="open" and .archived==false and .archived_at==null and .requires_response==true' >/dev/null
wait_jq debug/events '.events[] | select(.id=='"$EVENT_ID"' and .status=="open" and .archived==false and .requires_response==true)' 10 >/dev/null
pass_gate "Gate 4: reopen-ping completed honest undo; judgment $EVENT_ID is open and requires response"

# ---- Gate 5: a second archived judgment survives SIGTERM and reboot ----
PERSIST_ID="$(create_judgment archive-persist "$EVENT_ID")"
post_json debug/event-action '{"event_id":'"$PERSIST_ID"',"action":"archive","data":{}}' \
  | jq -e '.id=='"$PERSIST_ID"' and .status=="done" and .archived==true and .archived_at != null' >/dev/null
ARCHIVED_AT="$(events | jq -r '.events[] | select(.id=='"$PERSIST_ID"') | .archived_at')"
restart_hirsel_host
wait_jq debug/events '.events[] | select(.id=='"$PERSIST_ID"' and .status=="done" and .archived==true and .archived_at=="'"$ARCHIVED_AT"'")' 15 >/dev/null
pass_gate "Gate 5: archived judgment $PERSIST_ID and archived_at=$ARCHIVED_AT survived SIGTERM+reboot"

debug_snapshot /tmp/hirsel-e2e-event-archive-undo-final
events >/tmp/hirsel-e2e-event-archive-undo-final.events.json
pass_gate "event-archive-undo: all gates passed"
