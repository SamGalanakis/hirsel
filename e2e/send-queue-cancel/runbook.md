# Send, Queue, Cancel Runbook

## Purpose

Prove protocol v1.2 send modes and cancellation semantics through the debug HTTP surface: `mode=send` can inject into an active lash turn, `mode=next_turn` waits for the next full turn, `cancel_queued` removes unclaimed queued Owner messages, and `cancel_turn` interrupts active work without killing the pump.

## Shared Helpers

Use these shell helpers in each scenario:

```bash
BASE=http://127.0.0.1:3089

post_json() {
  curl -sS -X POST "$BASE/$1" -H 'content-type: application/json' -d "$2"
}

wait_jq() {
  path="$1"
  filter="$2"
  timeout="${3:-60}"
  end=$((SECONDS + timeout))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(curl -sS "$BASE/$path")" || return 1
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf '%s\n' "$json"
      return 0
    fi
    sleep 0.25
  done
  printf 'Timed out waiting for %s filter %s\n' "$path" "$filter" >&2
  curl -sS "$BASE/$path" >&2 || true
  return 1
}

assert_no_jq_for() {
  path="$1"
  filter="$2"
  seconds="$3"
  end=$((SECONDS + seconds))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(curl -sS "$BASE/$path")" || return 1
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf 'Unexpected match for %s filter %s\n%s\n' "$path" "$filter" "$json" >&2
      return 1
    fi
    sleep 0.25
  done
}
```

## Scenario A: Scripted Deterministic

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-niceties-host
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-send-queue-cancel-scripted
export HIRSEL_LISTEN=127.0.0.1:3089
cargo run -p hirsel-host
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
wait_jq debug/chat '.messages[] | select(.author == "agent" and .ref == ('$ACTIVE_ID'))' 30 >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "agent" and .body == "pong")' 10 >/dev/null
```

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

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-niceties-host
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-send-queue-cancel-codex
export HIRSEL_LISTEN=127.0.0.1:3089
cargo run -p hirsel-host
```

Reset:

```bash
post_json debug/reset '{}'
```

### Gate 1: `send` mid-turn reaches the same turn as Early Injection

Start a turn that creates a checkpoint via `shell_run`, then inject a same-turn marker:

```bash
post_json debug/owner-message '{"client_id":"lash-inject-active","body":"Use shell_run to run exactly `sleep 25`, then use chat_send to reply. If you receive any same-turn owner input containing INJECTED_MARKER, include that exact marker in your reply.","ref":null}' >/dev/null
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
post_json debug/owner-message '{"client_id":"lash-next-active","body":"Use shell_run to run exactly `sleep 25`, then use chat_send to reply exactly `initial done`.","ref":null}' >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 15 >/dev/null
post_json debug/owner-message '{"client_id":"lash-next-queued","body":"Use chat_send to reply exactly `NEXT_TURN_DONE`.","ref":null,"mode":"next_turn"}' >/dev/null
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
CANCEL_REAL_JSON="$(post_json debug/owner-message '{"client_id":"lash-cancel-active","body":"Use shell_run to run exactly `sleep 25`, then use chat_send to reply exactly `SHOULD_NOT_APPEAR`.","ref":null}')"
CANCEL_REAL_ID="$(printf '%s' "$CANCEL_REAL_JSON" | jq -r '.message.id')"
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "thinking")' 15 >/dev/null
post_json debug/cancel-turn '{}'
```

Gates:

```bash
wait_jq debug/broadcasts '.events[] | select(.type == "agent_activity" and .state == "idle")' 10 >/dev/null
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and ((.body | contains("SHOULD_NOT_APPEAR")) or .ref == ('$CANCEL_REAL_ID')))' 5
```

## Report

For each gate, record the message id or broadcast event that proved it. If the real LLM variant fails because the model ignores a prompt instruction, preserve `/debug/chat`, `/debug/broadcasts`, and the host logs and report it as a prompt-behavior miss rather than manually forcing a pass.
