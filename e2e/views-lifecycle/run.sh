#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3310)"
DATA_DIR="/tmp/hirsel-e2e-views-lifecycle"
HOST_LOG="/tmp/hirsel-e2e-views-lifecycle-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

views() { get_json debug/views; }
events() { get_json debug/events; }
hello_views() {
  node "$ROOT/e2e/views-lifecycle/hello-views.cjs" \
    "ws://127.0.0.1:$PORT/ws" "$HIRSEL_TOKEN"
}

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null

# ---- Gate 1: show creates the instance, debug state, and view_upsert ----
SHOWN="$(post_json debug/show-view \
  '{"template_id":"task-progress","params":{"title":"E2E release","value":0.25,"progress_label":"One of four","current_step":"Preparing","state_label":"In progress","state":"running"},"placement":"canvas"}')"
VIEW_ID="$(printf '%s' "$SHOWN" | jq -r '.instance_id')"
printf '%s' "$SHOWN" | jq -e \
  '.instance_id | type=="string" and length>0' >/dev/null
views | jq -e \
  '.views[] | select(.instance_id=="'"$VIEW_ID"'" and .placement=="canvas" and .spec.type=="card" and .spec.children[0].type=="progress" and .spec.children[0].value==0.25)' >/dev/null
wait_jq debug/broadcasts \
  '.events[] | select(.type=="view_upsert" and .instance_id=="'"$VIEW_ID"'" and .placement=="canvas" and .spec.children[0].value==0.25)' 10 >/dev/null
pass_gate "Gate 1: show-view created $VIEW_ID with resolved placement/spec and broadcast view_upsert"

# ---- Gate 2: the real tool path updates the same id and re-broadcasts ----
UPDATE_PROMPT="Call views.update exactly once for instance_id $VIEW_ID with params {\"value\":0.75,\"progress_label\":\"Three of four\",\"current_step\":\"Verifying\"}. Do not call any other tool. After the tool succeeds, reply exactly VIEW_UPDATED."
post_json debug/owner-message "$(jq -nc --arg body "$UPDATE_PROMPT" \
  '{client_id:"views-update",body:$body,ref:null}')" >/dev/null
wait_jq debug/views \
  '.views[] | select(.instance_id=="'"$VIEW_ID"'" and .spec.children[0].value==0.75 and .spec.children[0].label=="Three of four" and .spec.children[1].children[1].text=="Verifying")' 180 >/dev/null
wait_jq debug/broadcasts \
  '[.events[] | select(.type=="view_upsert" and .instance_id=="'"$VIEW_ID"'")] as $matches | ($matches | length) >= 2 and ($matches | last | .spec.children[0].value)==0.75' 20 >/dev/null
wait_jq debug/chat \
  '.messages[] | select(.author=="agent" and any(.tool_calls[]?; .name=="views_update" and .ok==true))' 180 >/dev/null
pass_gate "Gate 2: views.update mutated $VIEW_ID in place and re-broadcast its resolved spec"

# ---- Gate 3: a canvas interaction routes through normal Owner ingress ----
INTERACTION="$(post_json debug/view-event \
  '{"instance_id":"'"$VIEW_ID"'","action":"advance","data":{"step":4}}')"
INTERACTION_ID="$(printf '%s' "$INTERACTION" | jq -r '.message.id')"
printf '%s' "$INTERACTION" | jq -e \
  '.message.author=="owner" and .message.ref==null and (.message.body | contains("emitted action `advance`")) and (.message.body | contains("\"step\":4"))' >/dev/null
wait_jq debug/chat '.messages[] | select(.id=='"$INTERACTION_ID"' and .author=="owner")' 10 >/dev/null
wait_agent_message_after "$INTERACTION_ID" 'true' 180
pass_gate "Gate 3: view_event routed canvas interaction as Owner message $INTERACTION_ID and completed its turn"

# ---- Gate 4: ping placement resolves to the Event anchor ----
SUMMARY="$(post_json debug/trigger-digest \
  '{"job_id":"views-anchor-probe","text":"Anchor routing probe.","status":"ready"}')"
PING_ID="$(printf '%s' "$SUMMARY" | jq -r '.id')"
ANCHOR_ID="$(printf '%s' "$SUMMARY" | jq -r '.anchor')"
PING_VIEW="$(post_json debug/show-view \
  '{"template_id":"task-progress","params":{"title":"Anchored view","value":1,"progress_label":"Complete","current_step":"Done","state_label":"Done","state":"success"},"placement":"ping:'"$PING_ID"'"}')"
PING_VIEW_ID="$(printf '%s' "$PING_VIEW" | jq -r '.instance_id')"
ANCHORED_EVENT="$(post_json debug/view-event \
  '{"instance_id":"'"$PING_VIEW_ID"'","action":"acknowledge","data":{"ok":true}}')"
ANCHORED_MESSAGE_ID="$(printf '%s' "$ANCHORED_EVENT" | jq -r '.message.id')"
printf '%s' "$ANCHORED_EVENT" | jq -e \
  '.message.author=="owner" and .message.ref=='"$ANCHOR_ID"'' >/dev/null
wait_jq debug/events '.events[] | select(.id=='"$PING_ID"' and .status=="done")' 15 >/dev/null
wait_agent_message_after "$ANCHORED_MESSAGE_ID" 'true' 180
pass_gate "Gate 4: ping:$PING_ID view event resolved anchor $ANCHOR_ID and completed its Event/turn"

# ---- Gate 5: active views replay in a fresh hello_ok.views ----
HELLO="$(hello_views)"
printf '%s' "$HELLO" | jq -e \
  '.views[] | select(.instance_id=="'"$VIEW_ID"'" and .placement=="canvas" and .spec.children[0].value==0.75)' >/dev/null
printf '%s' "$HELLO" | jq -e \
  '.views[] | select(.instance_id=="'"$PING_VIEW_ID"'" and .placement=="ping:'"$PING_ID"'")' >/dev/null
pass_gate "Gate 5: fresh hello_ok.views replayed updated $VIEW_ID and anchored $PING_VIEW_ID"

# ---- Gate 6: the real tool path clears and emits view_removed ----
CLEAR_PROMPT="Call views.clear exactly once for instance_id $VIEW_ID. Do not call any other tool. After the tool succeeds, reply exactly VIEW_CLEARED."
post_json debug/owner-message "$(jq -nc --arg body "$CLEAR_PROMPT" \
  '{client_id:"views-clear",body:$body,ref:null}')" >/dev/null
wait_jq debug/broadcasts \
  '.events[] | select(.type=="view_removed" and .instance_id=="'"$VIEW_ID"'")' 180 >/dev/null
wait_jq debug/views '[.views[] | select(.instance_id=="'"$VIEW_ID"'")] | length==0' 20 >/dev/null
wait_jq debug/chat \
  '.messages[] | select(.author=="agent" and any(.tool_calls[]?; .name=="views_clear" and .ok==true))' 180 >/dev/null
pass_gate "Gate 6: views.clear removed $VIEW_ID from /debug/views and broadcast view_removed"

debug_snapshot /tmp/hirsel-e2e-views-lifecycle-final
views >/tmp/hirsel-e2e-views-lifecycle-final.views.json
events >/tmp/hirsel-e2e-views-lifecycle-final.events.json
printf '%s\n' "$HELLO" >/tmp/hirsel-e2e-views-lifecycle-final.hello.json
pass_gate "views-lifecycle: all gates passed"
