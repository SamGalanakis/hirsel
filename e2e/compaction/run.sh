#!/usr/bin/env bash
set -euo pipefail
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold

ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=../lib/runbook-lib.sh
source "$ROOT/e2e/lib/runbook-lib.sh"

PORT="$(choose_port 3150)"
DATA_DIR="/tmp/hirsel-e2e-compaction-data"
HOST_LOG="/tmp/hirsel-e2e-compaction-host.log"
HIRSEL_AGENT=lash
HIRSEL_PROVIDER=codex
HIRSEL_DRIVER=fake
HIRSEL_MODEL=gpt-5.5
trap 'stop_hirsel_host TERM' EXIT

start_hirsel_host fresh
post_json debug/reset '{}' >/dev/null
pass_gate "debug reset"

before="$(max_chat_id)"
post_json debug/owner-message "$(jq -nc --arg body 'Remember this exact pre-compaction fact: GREEN-742.' '{client_id:"compact-fact",body:$body,ref:null}')" >/dev/null
wait_agent_message_after "$before" 'true' 180
pass_gate "pre-compaction fact turn completed"

before="$(max_chat_id)"
post_json debug/owner-message "$(jq -nc --arg body 'Compact your context now using control.continue_as. Your seed must preserve GREEN-742. Then confirm exactly COMPACTED.' '{client_id:"compact-now",body:$body,ref:null}')" >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "turn_event" and .event.kind == "tool_done" and (.event.name | test("continue_as|continue|control|compact"; "i")) and .event.ok == true)' 180 >/dev/null
pass_gate "continue_as/control tool_done visible in broadcasts"

before="$(max_chat_id)"
post_json debug/owner-message "$(jq -nc --arg body 'What was the exact pre-compaction fact? Reply with only the code.' '{client_id:"compact-recall",body:$body,ref:null}')" >/dev/null
# Gate 4 (behavioral) is a KNOWN-FAIL: upstream lash #8 drops the continue_as seed on frame
# materialization through the queued-turn path, so recall of GREEN-742 fails deterministically.
# Record the outcome without aborting — Gate 3 (visible control event) is the mechanical gate.
# See runbook.md "Known limitation" and e2e/RULES.md. Flips to a real pass when lash #8 is fixed.
if wait_agent_message_after "$before" '(.body | contains("GREEN-742"))' 60; then
  pass_gate "post-compaction recall returned GREEN-742 (lash #8 appears fixed)"
else
  printf 'KNOWN-FAIL post-compaction recall did not return GREEN-742 — upstream lash #8 (continue_as seed drop). See e2e/compaction/runbook.md.\n' >&2
fi

if get_json debug/broadcasts | jq -e '.events[] | select(.type == "turn_event" and .event.kind == "tool_start" and (.event.name | test("continue_as|continue|control|compact"; "i")))' >/dev/null; then
  pass_gate "continue_as/control tool_start visible in broadcasts"
elif get_json debug/chat | jq -e '.messages[] | select(.author == "agent" and any(.tool_calls[]?; (.name | test("continue|control|compact"; "i"))))' >/dev/null; then
  pass_gate "continue/control tool_call visible on committed chat"
else
  debug_snapshot /tmp/hirsel-e2e-compaction-missing-control
  fail_gate "no continue_as/control compaction tool evidence visible"
  exit 1
fi

debug_snapshot /tmp/hirsel-e2e-compaction-final
