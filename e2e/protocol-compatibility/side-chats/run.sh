#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/protocol-compatibility/lib/runbook-lib.sh"

PORT="$(choose_port 3210)"
DATA_DIR="/tmp/hirsel-e2e-side-chats-scripted"
HOST_LOG="/tmp/hirsel-e2e-side-chats-scripted-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
HIRSEL_COMPAT_SIDE_SESSIONS=1
export HIRSEL_COMPAT_SIDE_SESSIONS
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
post_json debug/owner-message '{"client_id":"side-seed","body":"Please delegate a trivial repo fix, then ask me before applying the result.","ref":null}' >/dev/null
PINGS_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and (.name | length > 0) and (.description | length > 0))' 60)"
PING_ID="$(printf '%s' "$PINGS_JSON" | jq -r '.pings[] | select(.status == "open") | .id' | tail -1)"
ANCHOR_ID="$(printf '%s' "$PINGS_JSON" | jq -r '.pings[] | select(.id == '"$PING_ID"') | .anchor')"

OPEN_JSON="$(post_json debug/open-side-chat "$(jq -nc --argjson ping_id "$PING_ID" '{ping_id:$ping_id}')")"
SC="$(printf '%s' "$OPEN_JSON" | jq -r '.sc')"
printf '%s' "$OPEN_JSON" | jq -e '.resumed == false and (.messages | length == 0)' >/dev/null
pass_gate "Side Chat $SC opened empty for Ping $PING_ID"

MAIN_BEFORE="$(max_chat_id)"
post_json debug/side-message "$(jq -nc --arg sc "$SC" '{sc:$sc,body:"Ship after the final check."}')" >/dev/null
wait_jq debug/side-chats '.side_chats[] | select(.sc == "'"$SC"'") | .messages | length >= 2' 10 >/dev/null
test "$(max_chat_id)" = "$MAIN_BEFORE"
wait_jq debug/broadcasts '.events[] | select(.type == "turn_event" and .sc == "'"$SC"'")' 10 >/dev/null
pass_gate "Side conversation stayed scoped and emitted turn events"

RESUME_JSON="$(post_json debug/open-side-chat "$(jq -nc --argjson ping_id "$PING_ID" '{ping_id:$ping_id}')")"
printf '%s' "$RESUME_JSON" | jq -e '.resumed == true and .sc == "'"$SC"'" and (.messages | length >= 2)' >/dev/null
pass_gate "Side Chat $SC resumed with its transcript"

BEFORE_COUNT="$(get_json debug/side-chats | jq '.side_chats[] | select(.sc == "'"$SC"'") | .messages | length')"
DRAFT_JSON="$(post_json debug/conclude "$(jq -nc --arg sc "$SC" '{sc:$sc}')")"
printf '%s' "$DRAFT_JSON" | jq -e '.text | contains("Ship after the final check.")' >/dev/null
test "$(get_json debug/side-chats | jq '.side_chats[] | select(.sc == "'"$SC"'") | .messages | length')" = "$BEFORE_COUNT"
pass_gate "Conclusion draft was not persisted in the transcript"

CONFIRM_BEFORE="$(max_chat_id)"
post_json debug/confirm-conclusion "$(jq -nc --arg sc "$SC" '{sc:$sc,text:"Ship it after the final check."}')" >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "owner" and .body == "Ship it after the final check." and .ref == '"$ANCHOR_ID"')' 10 >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$PING_ID"' and .status == "done")' 10 >/dev/null
wait_jq debug/side-chats 'all(.side_chats[]; .sc != "'"$SC"'")' 10 >/dev/null
wait_agent_message_after "$CONFIRM_BEFORE" 'true' 30
pass_gate "Confirmed Conclusion auto-resolved Ping $PING_ID, closed $SC, and woke the main Agent"

post_json debug/owner-message '{"client_id":"side-seed-2","body":"Please delegate another trivial repo fix, then ask me before applying the result.","ref":null}' >/dev/null
PING2_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .id != '"$PING_ID"')' 60)"
PING2_ID="$(printf '%s' "$PING2_JSON" | jq -r '.pings[] | select(.status == "open" and .id != '"$PING_ID"') | .id' | tail -1)"
ANCHOR2_ID="$(printf '%s' "$PING2_JSON" | jq -r '.pings[] | select(.id == '"$PING2_ID"') | .anchor')"
SC2="$(post_json debug/open-side-chat "$(jq -nc --argjson ping_id "$PING2_ID" '{ping_id:$ping_id}')" | jq -r '.sc')"
post_json debug/resolve-ping "$(jq -nc --argjson ping_id "$PING2_ID" '{ping_id:$ping_id}')" >/dev/null
post_json debug/conclude "$(jq -nc --arg sc "$SC2" '{sc:$sc}')" | jq -e '.text | length > 0' >/dev/null
post_json debug/confirm-conclusion "$(jq -nc --arg sc "$SC2" '{sc:$sc,text:"Already done, still concluded."}')" >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "owner" and .body == "Already done, still concluded." and .ref == '"$ANCHOR2_ID"')' 10 >/dev/null
wait_jq debug/pings '.pings[] | select(.id == '"$PING2_ID"' and .status == "done")' 10 >/dev/null
pass_gate "Conclusion remained idempotent after Ping $PING2_ID was already done"
debug_snapshot /tmp/hirsel-e2e-side-chats-scripted-final
