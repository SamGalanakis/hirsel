#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3290)"
DATA_DIR="/tmp/hirsel-e2e-event-snooze-sweep"
HOST_LOG="/tmp/hirsel-e2e-event-snooze-sweep-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

events() { get_json debug/events; }
event_field() { events | jq -r '.events[] | select(.id=='"$1"') | '"$2"''; }
sweep_over_ws() {
  node "$ROOT/e2e/event-snooze-sweep/clear-finished.cjs" "ws://127.0.0.1:$PORT/ws" "$HIRSEL_TOKEN"
}

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
post_json debug/register-push-token '{"platform":"android","token":"e2e-snooze-probe"}' >/dev/null

# ---- Gate 1: a judgment (scripted host flow) + a summary (digest producer) ----
post_json debug/owner-message '{"client_id":"ess-delegate","body":"Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-event-snooze-sweep-work (create it if needed), then ask me before applying the result.","ref":null}' >/dev/null
wait_jq debug/events '.events[] | select(.status=="open" and .requires_response==true)' 60 >/dev/null
EV=$(events | jq -r '[.events[] | select(.status=="open" and .requires_response==true)] | last | .id')
wait_jq debug/pushes '[.pushes[] | select(.payload.data.event_id=='"$EV"')] | length >= 1' 15 >/dev/null
SUMMARY=$(post_json debug/trigger-digest '{}' | jq -r '.id')
events | jq -e '.events[] | select(.id=='"$SUMMARY"' and .kind=="summary" and .requires_response==false)' >/dev/null
pass_gate "Gate 1: judgment $EV (pushed) via scripted flow; summary $SUMMARY via digest producer"

# ---- Gate 2: snooze without/with-invalid until is a retryable error naming presets ----
for BAD in '{}' '{"until":"not-a-timestamp"}' '{"until":"2020-01-01T00:00:00Z"}'; do
  BODY=$(curl -sS -X POST "$BASE/debug/event-action" -H "authorization: Bearer $HIRSEL_TOKEN" \
    -H 'content-type: application/json' -d '{"event_id":'"$EV"',"action":"snooze","data":'"$BAD"'}')
  printf '%s' "$BODY" | jq -e '.error | test("preset") and test("This evening")' >/dev/null \
    || { fail_gate "Gate 2: snooze data=$BAD not rejected with preset-naming error: $BODY"; exit 1; }
done
[[ "$(event_field "$EV" '.snoozed_until')" == "null" ]] \
  || { fail_gate "Gate 2: rejected snooze mutated snoozed_until"; exit 1; }
pass_gate "Gate 2: no-until / malformed / past snooze all rejected naming the presets; event untouched"

# ---- Gate 3: a valid snooze parks; the host timer returns it and re-pushes ----
UNTIL=$(date -u -d '+4 seconds' +%Y-%m-%dT%H:%M:%SZ)
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{"until":"'"$UNTIL"'"}}' \
  | jq -e '.snoozed_until != null' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type=="event_upsert" and .event.id=='"$EV"' and .event.snoozed_until != null)' 10 >/dev/null
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .snoozed_until == null)' 20 >/dev/null
wait_jq debug/pushes '[.pushes[] | select(.payload.data.event_id=='"$EV"')] | length >= 2' 15 >/dev/null
pass_gate "Gate 3: snooze until $UNTIL parked $EV; host timer returned it and re-pushed"

# ---- Gate 4: a pending return survives a host restart ----
UNTIL=$(date -u -d '+10 seconds' +%Y-%m-%dT%H:%M:%SZ)
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{"until":"'"$UNTIL"'"}}' \
  | jq -e '.snoozed_until != null' >/dev/null
restart_hirsel_host
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .snoozed_until == null)' 30 >/dev/null
pass_gate "Gate 4: return pending at SIGTERM completed after reboot on the same data dir"

# ---- Gate 5: unsnooze returns the event immediately ----
UNTIL=$(date -u -d '+1 hour' +%Y-%m-%dT%H:%M:%SZ)
post_json debug/event-action '{"event_id":'"$EV"',"action":"snooze","data":{"until":"'"$UNTIL"'"}}' \
  | jq -e '.snoozed_until != null' >/dev/null
post_json debug/event-action '{"event_id":'"$EV"',"action":"unsnooze","data":{}}' \
  | jq -e '.snoozed_until == null' >/dev/null
pass_gate "Gate 5: unsnooze cleared snoozed_until immediately"

# ---- Gate 6: the ws sweep archives read awareness with archived_at, keeps the open judgment ----
post_json debug/read-ping '{"ping_id":'"$SUMMARY"'}' >/dev/null
sweep_over_ws
wait_jq debug/events '.events[] | select(.id=='"$SUMMARY"' and .archived==true and .status=="done" and .archived_at != null)' 15 >/dev/null
events | jq -e '.events[] | select(.id=='"$SUMMARY"') | (now - (.archived_at | sub("\\.[0-9]+"; "") | sub("\\+00:00$"; "Z") | fromdateiso8601)) | (. >= -5 and . < 120)' >/dev/null
events | jq -e '.events[] | select(.id=='"$EV"') | (.archived==false and .status=="open")' >/dev/null
pass_gate "Gate 6: clear_finished_events over /ws archived summary $SUMMARY (archived_at stamped); open judgment $EV survived"

# ---- Gate 7: a decided judgment falls to the next sweep ----
post_json debug/resolve-ping '{"ping_id":'"$EV"'}' >/dev/null
sweep_over_ws
wait_jq debug/events '.events[] | select(.id=='"$EV"' and .archived==true and .archived_at != null)' 15 >/dev/null
pass_gate "Gate 7: decided judgment $EV archived with archived_at on the next sweep"

debug_snapshot /tmp/hirsel-e2e-event-snooze-sweep-final
pass_gate "event-snooze-sweep: all gates passed"
