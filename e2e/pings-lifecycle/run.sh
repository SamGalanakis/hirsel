#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3170)"
DATA_DIR="/tmp/hirsel-e2e-pings-lifecycle-scripted"
HOST_LOG="/tmp/hirsel-e2e-pings-lifecycle-scripted-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "scripted debug reset"

post_json debug/owner-message '{"client_id":"pings-life-delegate-1","body":"Please delegate a trivial repo fix to a Sub-agent, then ask me before applying the result.","ref":null}' >/dev/null
PING_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and (.quick_replies | length > 0) and (.name | length > 0) and (.description | length > 0))' 60)"
PING_ID="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.status == "open" and .requires_response == true) | .id' | tail -1)"
ANCHOR_ID="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.id == '"$PING_ID"') | .anchor')"
PING_NAME="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.id == '"$PING_ID"') | .name')"
pass_gate "scripted Ping filed: ping=$PING_ID @$PING_NAME anchor=$ANCHOR_ID with description"

REPLY_JSON="$(post_json debug/owner-message "$(jq -nc --argjson ref "$ANCHOR_ID" '{client_id:"pings-life-reply-1",body:"ship it",ref:$ref}')")"
REPLY_ID="$(printf '%s' "$REPLY_JSON" | jq -r '.message.id')"
wait_jq debug/chat '.messages[] | select(.id == '"$REPLY_ID"' and .author == "owner" and .ref == '"$ANCHOR_ID"')' 10 >/dev/null
pass_gate "Owner reply is anchor-refed to $ANCHOR_ID"
wait_jq debug/pings '.pings[] | select(.id == '"$PING_ID"' and .status == "done")' 10 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "ping_upsert" and .ping.id == '"$PING_ID"' and .ping.status == "done")' 10 >/dev/null
pass_gate "anchor-refed Owner reply auto-resolved Ping $PING_ID without an Agent resolve call"
wait_agent_message_after "$REPLY_ID" '(.body | contains("Acknowledged"))' 30
pass_gate "Agent acknowledged anchor-refed Ping reply"

post_json debug/owner-message '{"client_id":"pings-life-delegate-2","body":"Please delegate another trivial repo fix to a Sub-agent, then ask me before applying the result.","ref":null}' >/dev/null
PING2_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and .id != '"$PING_ID"')' 60)"
PING2_ID="$(printf '%s' "$PING2_JSON" | jq -r '.pings[] | select(.status == "open" and .id != '"$PING_ID"') | .id' | tail -1)"
MENTION_REPLY="$(post_json debug/owner-message "$(jq -nc --argjson ping_id "$PING2_ID" '{client_id:"pings-life-mention",body:"What is the status of this?",ref:null,mentions:[$ping_id]}')")"
MENTION_REPLY_ID="$(printf '%s' "$MENTION_REPLY" | jq -r '.message.id')"
wait_jq debug/pings '.pings[] | select(.id == '"$PING2_ID"' and .status == "open")' 10 >/dev/null
pass_gate "mention in Owner message $MENTION_REPLY_ID left Ping $PING2_ID open"

post_json debug/resolve-ping "$(jq -nc --argjson ping_id "$PING2_ID" '{ping_id:$ping_id}')" | jq -e '.status == "done"' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "ping_upsert" and .ping.id == '"$PING2_ID"' and .ping.status == "done")' 10 >/dev/null
pass_gate "Owner resolve route moved Ping $PING2_ID to done"

debug_snapshot /tmp/hirsel-e2e-pings-lifecycle-scripted-final
stop_hirsel_host TERM

PORT="$(choose_port 3180)"
DATA_DIR="/tmp/hirsel-e2e-pings-lifecycle-real"
HOST_LOG="/tmp/hirsel-e2e-pings-lifecycle-real-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "real Agent debug reset"

BODY='Use pings.send to send one requires_response Ping with name "moot-question", description "Decide whether to continue", content_md exactly "Moot question: continue?", and one quick reply with value "stop" and label "Stop". End the turn with no chat text.'
post_json debug/owner-message "$(jq -nc --arg body "$BODY" '{client_id:"pings-life-real-send",body:$body,ref:null}')" >/dev/null
REAL_PING_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and (.content | contains("Moot question: continue?")) and (.name | length > 0) and (.description | length > 0))' 180)"
REAL_PING_ID="$(printf '%s' "$REAL_PING_JSON" | jq -r '.pings[] | select(.status == "open" and (.content | contains("Moot question: continue?"))) | .id' | tail -1)"
REAL_PING_NAME="$(printf '%s' "$REAL_PING_JSON" | jq -r '.pings[] | select(.id == '"$REAL_PING_ID"') | .name')"
pass_gate "real Agent sent named Ping: $REAL_PING_ID @$REAL_PING_NAME with description"
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "idle")' 180 >/dev/null
pass_gate "real Agent completed the pings.send turn"

before="$(max_chat_id)"
STOP_BODY="The Ping with ping_id $REAL_PING_ID is moot. Resolve it using pings.resolve, then reply exactly MOOT_RESOLVED."
post_json debug/owner-message "$(jq -nc --arg body "$STOP_BODY" '{client_id:"pings-life-real-moot",body:$body,ref:null}')" >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$REAL_PING_ID"' and .status == "done")' 180 >/dev/null
pass_gate "real Agent resolved moot Ping $REAL_PING_ID"
wait_agent_message_after "$before" '(.body | contains("MOOT_RESOLVED"))' 60
pass_gate "real Agent replied MOOT_RESOLVED"

if get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and any(.tool_calls[]?; .name == "pings_resolve"))' >/dev/null; then
  pass_gate "committed tool summary includes pings_resolve"
else
  debug_snapshot /tmp/hirsel-e2e-pings-lifecycle-real-tool-miss
  fail_gate "missing pings_resolve tool summary"
  exit 1
fi

debug_snapshot /tmp/hirsel-e2e-pings-lifecycle-real-final
