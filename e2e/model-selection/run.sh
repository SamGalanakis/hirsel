#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3330)"
DATA_DIR="/tmp/hirsel-e2e-model-selection"
HOST_LOG="/tmp/hirsel-e2e-model-selection-host.log"
HIRSEL_AGENT=scripted
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.6-sol
trap 'stop_hirsel_host TERM' EXIT

hello_snapshot() {
  node "$ROOT/e2e/model-selection/hello-snapshot.cjs" \
    "ws://127.0.0.1:$PORT/ws" "$HIRSEL_TOKEN"
}

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null

# ---- Gate 1: main model selection broadcasts and appears on a fresh hello ----
post_json debug/set-model '{"model_id":"gpt-5.6-sol","variant":"high"}' \
  | jq -e '.id=="gpt-5.6-sol" and .variant=="high"' >/dev/null
wait_jq debug/broadcasts \
  '.events[] | select(.type=="model_changed" and .current.id=="gpt-5.6-sol" and .current.variant=="high")' 10 >/dev/null
HELLO="$(hello_snapshot)"
printf '%s' "$HELLO" | jq -e \
  '.model.current.id=="gpt-5.6-sol" and .model.current.variant=="high"' >/dev/null
pass_gate "Gate 1: model_changed selected gpt-5.6-sol/high and fresh hello_ok.model reflected it"

# ---- Gate 2: a Sub-agent catalog row toggles and broadcasts wholesale ----
post_json debug/subagent-models \
  '{"provider":"claude","model_id":"claude-sonnet-5","enabled":false,"default_variant":"low"}' \
  | jq -e '.providers[] | select(.provider=="claude") | .models[] | select(.id=="claude-sonnet-5" and .enabled==false and .default_variant=="low")' >/dev/null
wait_jq debug/broadcasts \
  '.events[] | select(.type=="subagent_models_changed") | .catalog.providers[] | select(.provider=="claude") | .models[] | select(.id=="claude-sonnet-5" and .enabled==false and .default_variant=="low")' 10 >/dev/null
pass_gate "Gate 2: subagent_models_changed disabled claude-sonnet-5 and changed its default variant to low"

# ---- Gate 3: one invalid main-model id returns an objective error and changes nothing ----
INVALID_BODY="/tmp/hirsel-e2e-model-selection-invalid.json"
INVALID_STATUS="$(curl -sS -o "$INVALID_BODY" -w '%{http_code}' -X POST \
  "$BASE/debug/set-model" -H "authorization: Bearer $HIRSEL_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"model_id":"not-a-model","variant":"high"}')"
[[ "$INVALID_STATUS" == 500 ]] \
  || { fail_gate "Gate 3: invalid model returned HTTP $INVALID_STATUS"; exit 1; }
jq -e '.error | contains("unknown model: not-a-model")' "$INVALID_BODY" >/dev/null
get_json debug/health | jq -e \
  '.model.id=="gpt-5.6-sol" and .model.variant=="high"' >/dev/null
pass_gate "Gate 3: invalid model id returned an error and left gpt-5.6-sol/high selected"

# ---- Gate 4: both selections survive SIGTERM and reboot ----
restart_hirsel_host
get_json debug/health | jq -e \
  '.model.id=="gpt-5.6-sol" and .model.variant=="high"' >/dev/null
get_json debug/subagent-models | jq -e \
  '.providers[] | select(.provider=="claude") | .models[] | select(.id=="claude-sonnet-5" and .enabled==false and .default_variant=="low")' >/dev/null
HELLO_AFTER="$(hello_snapshot)"
printf '%s' "$HELLO_AFTER" | jq -e \
  '.model.current.id=="gpt-5.6-sol" and .model.current.variant=="high"' >/dev/null
printf '%s' "$HELLO_AFTER" | jq -e \
  '.subagent_models.providers[] | select(.provider=="claude") | .models[] | select(.id=="claude-sonnet-5" and .enabled==false and .default_variant=="low")' >/dev/null
pass_gate "Gate 4: main and Sub-agent selections survived SIGTERM+reboot and replayed in hello_ok"

debug_snapshot /tmp/hirsel-e2e-model-selection-final
printf '%s\n' "$HELLO_AFTER" >/tmp/hirsel-e2e-model-selection-final.hello.json
pass_gate "model-selection: all gates passed"
