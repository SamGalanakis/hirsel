#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3170)"
DATA_DIR="/tmp/hirsel-e2e-inbox-lifecycle-scripted"
HOST_LOG="/tmp/hirsel-e2e-inbox-lifecycle-scripted-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "scripted debug reset"

post_json debug/owner-message '{"client_id":"inbox-life-delegate-1","body":"Please delegate a trivial repo fix to a Sub-agent, then ask me before applying the result.","ref":null}' >/dev/null
ITEM_JSON="$(wait_jq debug/inbox '.items[] | select(.status == "open" and .requires_response == true and (.quick_replies | length > 0))' 60)"
ITEM_ID="$(printf '%s' "$ITEM_JSON" | jq -r '.items[] | select(.status == "open" and .requires_response == true and (.quick_replies | length > 0)) | .id' | tail -1)"
ANCHOR_ID="$(printf '%s' "$ITEM_JSON" | jq -r '.items[] | select(.id == '"$ITEM_ID"') | .anchor')"
pass_gate "scripted requires-response item filed: item=$ITEM_ID anchor=$ANCHOR_ID"

REPLY_JSON="$(post_json debug/owner-message "$(jq -nc --argjson ref "$ANCHOR_ID" '{client_id:"inbox-life-reply-1",body:"ship it",ref:$ref}')")"
REPLY_ID="$(printf '%s' "$REPLY_JSON" | jq -r '.message.id')"
wait_jq debug/chat '.messages[] | select(.id == '"$REPLY_ID"' and .author == "owner" and .ref == '"$ANCHOR_ID"')' 10 >/dev/null
pass_gate "Owner reply is anchor-refed to $ANCHOR_ID"
wait_agent_message_after "$REPLY_ID" '(.body | contains("Acknowledged"))' 30
pass_gate "Agent acknowledged anchor-refed Inbox reply"

post_json debug/owner-message '{"client_id":"inbox-life-delegate-2","body":"Please delegate another trivial repo fix to a Sub-agent, then ask me before applying the result.","ref":null}' >/dev/null
ITEM2_JSON="$(wait_jq debug/inbox '.items[] | select(.status == "open" and .requires_response == true and .id != '"$ITEM_ID"')' 60)"
ITEM2_ID="$(printf '%s' "$ITEM2_JSON" | jq -r '.items[] | select(.status == "open" and .requires_response == true and .id != '"$ITEM_ID"') | .id' | tail -1)"
python3 "$ROOT/e2e/lib/ws_frame.py" 127.0.0.1 "$PORT" "$HIRSEL_TOKEN" "$(jq -nc --argjson id "$ITEM2_ID" '{type:"archive_item",item_id:$id}')" inbox_upsert >/tmp/hirsel-e2e-inbox-lifecycle-ws.log
wait_jq debug/inbox '.items[] | select(.id == '"$ITEM2_ID"' and .status == "archived")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "inbox_upsert" and .item.id == '"$ITEM2_ID"' and .item.status == "archived")' 10 >/dev/null
pass_gate "Owner WebSocket archive moved item $ITEM2_ID to archived"

debug_snapshot /tmp/hirsel-e2e-inbox-lifecycle-scripted-final
stop_hirsel_host TERM

PORT="$(choose_port 3180)"
DATA_DIR="/tmp/hirsel-e2e-inbox-lifecycle-real"
HOST_LOG="/tmp/hirsel-e2e-inbox-lifecycle-real-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "real Agent debug reset"

BODY='Use inbox.file to file one requires_response Inbox item with content exactly "Moot question: continue?" and one quick reply with value "stop" and label "Stop". End the turn with no chat text.'
post_json debug/owner-message "$(jq -nc --arg body "$BODY" '{client_id:"inbox-life-real-file",body:$body,ref:null}')" >/dev/null
REAL_ITEM_JSON="$(wait_jq debug/inbox '.items[] | select(.status == "open" and .requires_response == true and (.content | contains("Moot question: continue?")))' 180)"
REAL_ITEM_ID="$(printf '%s' "$REAL_ITEM_JSON" | jq -r '.items[] | select(.status == "open" and .requires_response == true and (.content | contains("Moot question: continue?"))) | .id' | tail -1)"
REAL_ANCHOR="$(printf '%s' "$REAL_ITEM_JSON" | jq -r '.items[] | select(.id == '"$REAL_ITEM_ID"') | .anchor')"
pass_gate "real Agent filed requires-response item: $REAL_ITEM_ID"

before="$(max_chat_id)"
STOP_BODY="Stop. Archive inbox item id $REAL_ITEM_ID as moot using inbox.archive, then reply exactly MOOT_ARCHIVED."
post_json debug/owner-message "$(jq -nc --arg body "$STOP_BODY" --argjson ref "$REAL_ANCHOR" '{client_id:"inbox-life-real-moot",body:$body,ref:$ref}')" >/dev/null
wait_jq debug/inbox '.items[] | select(.id == '"$REAL_ITEM_ID"' and .status == "archived")' 180 >/dev/null
pass_gate "real Agent archived moot item $REAL_ITEM_ID"
wait_agent_message_after "$before" '(.body | contains("MOOT_ARCHIVED"))' 60
pass_gate "real Agent replied MOOT_ARCHIVED"

if get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and any(.tool_calls[]?; .name == "inbox_archive"))' >/dev/null; then
  pass_gate "committed tool summary includes inbox_archive"
else
  debug_snapshot /tmp/hirsel-e2e-inbox-lifecycle-real-tool-miss
  fail_gate "missing inbox_archive tool summary"
  exit 1
fi

debug_snapshot /tmp/hirsel-e2e-inbox-lifecycle-real-final
