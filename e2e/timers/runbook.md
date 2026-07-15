# Timers Runbook

## Purpose

Prove the host-owned `timer.Schedule` trigger source: the Lash Agent registers a one-shot timer from lashlang, the host emits a `timer.Tick`, the trigger process wakes the `agent` session through queued work, and the Agent replies from the timer wake.

## Shared Helpers

```bash
ROOT=/workspace/code/hirsel-rbcov
source "$ROOT/e2e/lib/runbook-lib.sh"
PORT="$(choose_port 3220)"
BASE="http://127.0.0.1:$PORT"

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
    sleep 0.5
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
    sleep 0.5
  done
}
```

## Scenario A: Real Lash Agent With Codex Provider

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-timers-codex
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

Reset:

```bash
post_json debug/reset '{}'
```

Inject the timer request:

```bash
REQUEST_JSON="$(post_json debug/owner-message '{"client_id":"timer-real-1","body":"Wake yourself in 90 seconds and then say ping. Use the timer trigger source; do not say ping until the timer wake arrives.","ref":null}')"
REQUEST_ID="$(printf '%s' "$REQUEST_JSON" | jq -r '.message.id')"
```

Gates:

```bash
wait_jq debug/health '.ok == true' 5 >/dev/null
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and .id > ('$REQUEST_ID') and (.body | ascii_downcase | gsub("[^a-z]"; "") == "ping"))' 30
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > ('$REQUEST_ID') and (.body | ascii_downcase | gsub("[^a-z]"; "") == "ping"))' 180 >/dev/null
```

The first gate catches an immediate false-positive reply. The final gate proves the timer wake eventually produced the Agent-authored `ping` chat message.

## Report

Record the Owner request id and the Agent `ping` message id. On failure, preserve `/debug/chat`, `/debug/broadcasts`, and host logs; distinguish timer plumbing failures from prompt-behavior misses.
