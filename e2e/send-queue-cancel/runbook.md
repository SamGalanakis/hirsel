# Send, Queue, Cancel Runbook

## Purpose

Prove protocol v1.2 send modes and cancellation semantics through the debug HTTP surface: `mode=send` can inject into an active lash turn, `mode=next_turn` waits for the next full turn, `cancel_queued` removes unclaimed queued Owner messages, and `cancel_turn` interrupts active work without killing the pump.

## Shared Helpers

Use these shell helpers in each scenario:

```bash
BASE=http://127.0.0.1:<verified-free-port> # never 3089
HIRSEL_TOKEN=dev-token

post_json() {
  curl -fsS -X POST "$BASE/$1" -H "authorization: Bearer $HIRSEL_TOKEN" \
    -H 'content-type: application/json' -d "$2"
}

get_json() { curl -fsS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/$1"; }

wait_jq() {
  path="$1"
  filter="$2"
  timeout="${3:-60}"
  end=$((SECONDS + timeout))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(get_json "$path")" || return 1
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf '%s\n' "$json"
      return 0
    fi
    sleep 0.25
  done
  printf 'Timed out waiting for %s filter %s\n' "$path" "$filter" >&2
  get_json "$path" >&2 || true
  return 1
}

assert_no_jq_for() {
  path="$1"
  filter="$2"
  seconds="$3"
  end=$((SECONDS + seconds))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(get_json "$path")" || return 1
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf 'Unexpected match for %s filter %s\n%s\n' "$path" "$filter" "$json" >&2
      return 1
    fi
    sleep 0.25
  done
}
```

## Scenario A: Scripted Deterministic

Build once in the checkout, then start the absolute binary from a neutral `/tmp` cwd on a
verified-free port. Never send a preflight Owner message to this host: `/debug/reset` clears
persisted debug state but is not a substitute for cancelling an already active turn.

```bash
export REPO=/absolute/path/to/hirsel
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-e2e
(cd "$REPO" && cargo build -p hirsel-host)
export HOST_BIN="$CARGO_TARGET_DIR/debug/hirsel-host"
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-send-queue-cancel-scripted
rm -rf "$HIRSEL_DATA_DIR"
export HIRSEL_TEMPLATES_DIR="$REPO/templates"
export HIRSEL_LISTEN=127.0.0.1:<verified-free-port>
mkdir -p /tmp/hirsel-e2e-send-queue-cancel-scripted-work
cd /tmp/hirsel-e2e-send-queue-cancel-scripted-work
exec "$HOST_BIN"
```

Reset:

```bash
post_json debug/reset '{}'
```

### Gate 1: `next_turn` waits behind an active slow turn

Start a deterministic active turn, then queue a next full turn:

```bash
ACTIVE_JSON="$(post_json debug/owner-message '{"client_id":"scripted-active-1","body":"slow:20","ref":null}')"
ACTIVE_ID="$(printf '%s' "$ACTIVE_JSON" | jq -r '.message.id')"
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 5 >/dev/null

NEXT_JSON="$(post_json debug/owner-message '{"client_id":"scripted-next-1","body":"pong","ref":null,"mode":"next_turn"}')"
NEXT_ID="$(printf '%s' "$NEXT_JSON" | jq -r '.message.id')"
```

Gate before the slow turn finishes:

```bash
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and .body == "pong")' 5
```

Gate after the slow turn finishes:

```bash
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > ('$NEXT_ID') and .ref == ('$ACTIVE_ID'))' 30 >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "agent" and .body == "pong")' 10 >/dev/null
```

The active `slow:20` turn intentionally ends with the scripted double's generic reply; that does
not mean the real Agent ran. Requiring its Agent row to be newer than `NEXT_ID` prevents an old
in-flight/preflight reply with a reused Chat id/ref from satisfying the active-turn gate.

### Gate 2: `cancel_queued` deletes row and emits `msg_removed`

Hold the queue open with another slow turn, queue a cancellable next turn, then cancel it:

```bash
post_json debug/owner-message '{"client_id":"scripted-active-2","body":"slow:20","ref":null}' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 5 >/dev/null

CANCEL_JSON="$(post_json debug/owner-message '{"client_id":"scripted-cancel-queued","body":"pong","ref":null,"mode":"next_turn"}')"
CANCEL_ID="$(printf '%s' "$CANCEL_JSON" | jq -r '.message.id')"
post_json debug/cancel-queued '{"client_id":"scripted-cancel-queued"}'
```

Gates:

```bash
wait_jq debug/chat 'all(.messages[]; .id != ('$CANCEL_ID'))' 5 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "msg_removed" and .id == ('$CANCEL_ID'))' 5 >/dev/null
post_json debug/cancel-turn '{}'
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "idle")' 5 >/dev/null
```

### Gate 3: `cancel_turn` interrupts fake thinking

Start another slow turn and cancel it:

```bash
CANCEL_TURN_JSON="$(post_json debug/owner-message '{"client_id":"scripted-cancel-turn","body":"slow:20","ref":null}')"
CANCEL_TURN_ID="$(printf '%s' "$CANCEL_TURN_JSON" | jq -r '.message.id')"
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 5 >/dev/null
post_json debug/cancel-turn '{}'
```

Gates:

```bash
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "idle")' 5 >/dev/null
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and .ref == ('$CANCEL_TURN_ID'))' 3
```

## Scenario B: Real Lash Agent With Codex Provider And Fake Driver

Use this mode to prove the real lash facade integration. It requires valid Codex OAuth credentials in `~/.codex/auth.json`. Prompt behavior is part of the finding: if the model misses an instruction even though the debug state shows the transport behavior, report it honestly.

Stop the scripted host, choose another verified-free non-3089 port, and start the same absolute
binary from a fresh neutral cwd:

```bash
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-send-queue-cancel-codex
rm -rf "$HIRSEL_DATA_DIR"
export HIRSEL_TEMPLATES_DIR="$REPO/templates"
export HIRSEL_LISTEN=127.0.0.1:<another-verified-free-port>
mkdir -p /tmp/hirsel-e2e-send-queue-cancel-codex-work
cd /tmp/hirsel-e2e-send-queue-cancel-codex-work
exec "$HOST_BIN"
```

Reset:

```bash
post_json debug/reset '{}'
```

### Gate 1: `send` mid-turn reaches the same turn as Early Injection

Start a turn that creates a checkpoint via `shell_run`, then inject a same-turn marker:

```bash
post_json debug/owner-message '{"client_id":"lash-inject-active","body":"Use shell_run to run exactly `sleep 25`, then reply in chat. If you receive any same-turn owner input containing INJECTED_MARKER, include that exact marker in your reply.","ref":null}' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 15 >/dev/null
post_json debug/owner-message '{"client_id":"lash-inject-marker","body":"INJECTED_MARKER=same-turn-42","ref":null,"mode":"send"}' >/dev/null
```

Gate:

```bash
wait_jq debug/chat '.messages[] | select(.author == "agent" and (.body | contains("same-turn-42")))' 60 >/dev/null
```

### Gate 2: `next_turn` waits behind an active real turn

Start a slow real turn, then queue a next-turn message:

```bash
post_json debug/owner-message '{"client_id":"lash-next-active","body":"Use shell_run to run exactly `sleep 25`, then reply exactly `initial done`.","ref":null}' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 15 >/dev/null
post_json debug/owner-message '{"client_id":"lash-next-queued","body":"Reply exactly `NEXT_TURN_DONE`.","ref":null,"mode":"next_turn"}' >/dev/null
```

Gate before the first turn finishes:

```bash
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and (.body | contains("NEXT_TURN_DONE")))' 5
```

Gate after the first turn finishes:

```bash
wait_jq debug/chat '.messages[] | select(.author == "agent" and (.body | contains("initial done")))' 60 >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "agent" and (.body | contains("NEXT_TURN_DONE")))' 60 >/dev/null
```

### Gate 3: `cancel_turn` interrupts active lash work

Start a slow real turn and interrupt it:

```bash
SHELL_STARTS_BEFORE="$(get_json debug/broadcasts | jq '[.events[] | select(.type=="turn_event" and .event.kind=="tool_start" and .event.name=="shell_run")] | length')"
SHELL_DONES_BEFORE="$(get_json debug/broadcasts | jq '[.events[] | select(.type=="turn_event" and .event.kind=="tool_done" and .event.name=="shell_run" and .event.ok==true)] | length')"
CANCEL_REAL_JSON="$(post_json debug/owner-message '{"client_id":"lash-cancel-active","body":"Use shell_run first to run exactly `true`. After it completes, use shell_run again to run exactly `sleep 25`. Then reply exactly `SHOULD_NOT_APPEAR`.","ref":null}')"
CANCEL_REAL_ID="$(printf '%s' "$CANCEL_REAL_JSON" | jq -r '.message.id')"
wait_jq debug/broadcasts '[.events[] | select(.type=="turn_event" and .event.kind=="tool_done" and .event.name=="shell_run" and .event.ok==true)] | length > '"$SHELL_DONES_BEFORE" 30 >/dev/null
wait_jq debug/broadcasts '[.events[] | select(.type=="turn_event" and .event.kind=="tool_start" and .event.name=="shell_run")] | length > ('"$SHELL_STARTS_BEFORE"' + 1)' 15 >/dev/null
post_json debug/cancel-turn '{}'
```

Gates:

```bash
wait_jq debug/broadcasts '[.events[] | select(.type == "agent_activity")] | last | .state == "idle"' 10 >/dev/null
# Cooperative cancellation may keep a tool that completes while cancellation is propagating. At
# every poll, persisted ok:true shell summaries must be backed by already-observed successful
# tool_done broadcasts; no unfinished/fabricated result may appear as successful.
end=$((SECONDS + 40))
while [ "$SECONDS" -lt "$end" ]; do
  BROADCASTS="$(get_json debug/broadcasts)"
  CHAT="$(get_json debug/chat)"
  DONE_COUNT="$(printf '%s' "$BROADCASTS" | jq '[.events[] | select(.type=="turn_event" and .event.kind=="tool_done" and .event.name=="shell_run" and .event.ok==true)] | length - '"$SHELL_DONES_BEFORE"')"
  KEPT_COUNT="$(printf '%s' "$CHAT" | jq '[.messages[] | select(.author=="agent" and (.body | endswith("— interrupted"))) | .tool_calls[]? | select(.name=="shell_run" and .ok==true)] | length')"
  test "$KEPT_COUNT" -le "$DONE_COUNT" || { echo "fabricated successful tool result" >&2; exit 1; }
  printf '%s' "$CHAT" | jq -e '.messages[] | select(.author=="agent" and (.body | endswith("— interrupted")))' >/dev/null && break
  sleep 0.25
done
printf '%s' "$CHAT" | jq -e '.messages[] | select(.author=="agent" and (.body | endswith("— interrupted")))' >/dev/null
test "$KEPT_COUNT" -eq "$DONE_COUNT" # every completed shell call was preserved
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and (.body | contains("SHOULD_NOT_APPEAR")))' 5
```

## Report

For each gate, record the message id or broadcast event that proved it. If the real LLM variant fails because the model ignores a prompt instruction, preserve `/debug/chat`, `/debug/broadcasts`, and the host logs and report it as a prompt-behavior miss rather than manually forcing a pass.
