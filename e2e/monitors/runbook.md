# Monitors Runbook

## Purpose

Prove v1.4 host monitors: persisted specs, `ProcessInfo` visibility, `process_upsert` broadcasts, changed-output wake behavior, Agent wake delivery, and restart survival.

## Shared Helpers

```bash
ROOT=/workspace/code/hirsel-rbcov
source "$ROOT/e2e/lib/runbook-lib.sh"
PORT="$(choose_port 3230)"
BASE="http://127.0.0.1:$PORT"
WATCH=/tmp/hirsel-monitor-watch.txt

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

## Scenario A: Scripted Debug Monitor

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-monitors-scripted
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

Run:

```bash
post_json debug/reset '{}'
printf 'before\n' > "$WATCH"
CREATE_JSON="$(post_json debug/create-monitor '{"cmd":"stat -c %Y /tmp/hirsel-monitor-watch.txt","every_secs":30,"wake_on":"changed","label":"watch test file"}')"
MONITOR_ID="$(printf '%s' "$CREATE_JSON" | jq -r '.monitor.id')"
wait_jq debug/processes '.processes[] | select(.id == "'$MONITOR_ID'" and .kind == "monitor" and .state == "running")' 5 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "process_upsert" and .process.id == "'$MONITOR_ID'")' 5 >/dev/null
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and (.body | contains("watch test file")))' 35
touch "$WATCH"
wait_jq debug/chat '.messages[] | select(.author == "agent" and (.body | contains("watch test file")))' 45 >/dev/null
```

Restart the host with the same env and data dir, then run:

```bash
wait_jq debug/processes '.processes[] | select(.id == "'$MONITOR_ID'" and .kind == "monitor" and .state == "running")' 10 >/dev/null
LATEST="$(curl -sS "$BASE/debug/chat" | jq '[.messages[].id] | max // 0')"
touch "$WATCH"
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > ('$LATEST') and (.body | contains("watch test file")))' 45 >/dev/null
```

## Scenario B: Real Lash Agent Creates Monitor

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-monitors-codex
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

Run:

```bash
post_json debug/reset '{}'
printf 'before\n' > "$WATCH"
post_json debug/owner-message '{"client_id":"monitor-real-1","body":"Create a monitor named watch test file. It should run `stat -c %Y /tmp/hirsel-monitor-watch.txt` every 30 seconds, wake on changed output, and tell me when it fires. Use monitors.create. After creating it, reply exactly: monitor armed","ref":null}'
MONITOR_ID="$(wait_jq debug/processes '.processes[] | select(.kind == "monitor" and .label == "watch test file")' 90 | jq -r '.processes[] | select(.kind == "monitor" and .label == "watch test file") | .id' | tail -1)"
wait_jq debug/broadcasts '.events[] | select(.type == "turn_event" and .event.kind == "tool_start" and .event.name == "monitors_create")' 15 >/dev/null
wait_jq debug/broadcasts '.events[] | select(.type == "process_upsert" and .process.id == "'$MONITOR_ID'")' 15 >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "agent" and .body == "monitor armed")' 30 >/dev/null
LATEST="$(curl -sS "$BASE/debug/chat" | jq '[.messages[].id] | max // 0')"
assert_no_jq_for debug/chat '.messages[] | select(.author == "agent" and .id > ('$LATEST'))' 35
touch "$WATCH"
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > ('$LATEST'))' 90 >/dev/null
```

Restart the host with the same env and data dir, then run:

```bash
wait_jq debug/processes '.processes[] | select(.id == "'$MONITOR_ID'" and .kind == "monitor" and .state == "running")' 20 >/dev/null
LATEST="$(curl -sS "$BASE/debug/chat" | jq '[.messages[].id] | max // 0')"
touch "$WATCH"
wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > ('$LATEST'))' 90 >/dev/null
```

## Success Gates

- No Agent wake is observed before the watched file changes.
- `/debug/processes` shows the monitor as `kind: "monitor"` and `state: "running"`.
- `/debug/broadcasts` contains `process_upsert` for the monitor.
- After touching the file, an Agent-authored Chat message appears from the monitor wake.
- After restarting with the same data dir, the same monitor id is visible and fires again after another file change.
- Real Lash variant also shows a `turn_event` `tool_start` for `monitors_create`.
