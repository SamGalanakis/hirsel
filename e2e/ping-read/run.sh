#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3190)"
DATA_DIR="/tmp/hirsel-e2e-ping-read"
HOST_LOG="/tmp/hirsel-e2e-ping-read-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
post_json debug/owner-message '{"client_id":"ping-read-delegate","body":"Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-ping-read-work, then ask me before applying the result.","ref":null}' >/dev/null
PING_JSON="$(wait_jq debug/pings '.pings[] | select(.status == "open" and .requires_response == true and .read == false and (.name | length > 0) and (.description | length > 0))' 60)"
PING_ID="$(printf '%s' "$PING_JSON" | jq -r '.pings[] | select(.status == "open" and .read == false) | .id' | tail -1)"
pass_gate "Ping $PING_ID arrived unread with name and description"

post_json debug/read-ping "$(jq -nc --argjson ping_id "$PING_ID" '{ping_id:$ping_id}')" | jq -e '.read == true' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "ping_upsert" and .ping.id == '"$PING_ID"' and .ping.read == true)' 10 >/dev/null
pass_gate "read-ping marked Ping $PING_ID read and broadcast ping_upsert"

restart_hirsel_host
wait_jq debug/pings '.pings[] | select(.id == '"$PING_ID"' and .read == true)' 10 >/dev/null
pass_gate "Ping $PING_ID remained read after host restart"
debug_snapshot /tmp/hirsel-e2e-ping-read-final
