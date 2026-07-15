# Turn Timeline Runbook

## Purpose

Prove protocol v1.5 live turn timeline broadcasts through `/debug/broadcasts`: `turn_event` sequence numbers are ordered within a turn, prose can appear before a tool row, and tool summaries are condensed one-liners rather than raw JSON.

## Shared Helpers

```bash
ROOT=/workspace/code/hirsel-rbcov
source "$ROOT/e2e/lib/runbook-lib.sh"
PORT="$(choose_port 3250)"
BASE="http://127.0.0.1:$PORT"

post_json() {
  curl -sS -X POST "$BASE/$1" -H "authorization: Bearer $HIRSEL_TOKEN" -H 'content-type: application/json' -d "$2"
}

wait_jq() {
  path="$1"
  filter="$2"
  timeout="${3:-60}"
  end=$((SECONDS + timeout))
  while [ "$SECONDS" -lt "$end" ]; do
    json="$(curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/$path")" || return 1
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf '%s\n' "$json"
      return 0
    fi
    sleep 0.25
  done
  printf 'Timed out waiting for %s filter %s\n' "$path" "$filter" >&2
  curl -sS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/$path" >&2 || true
  return 1
}
```

## Scenario A: Scripted Deterministic

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-turn-timeline-scripted
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

Run:

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"timeline-scripted-1","body":"pong timeline check","ref":null}' >/dev/null
wait_jq debug/broadcasts '[.events[] | select(.type == "turn_event")] | length >= 4' 10 >/dev/null
```

Gates:

```bash
wait_jq debug/broadcasts '[.events[] | select(.type == "turn_event")] as $e | [$e[].seq] == ([$e[].seq] | sort)' 5 >/dev/null
wait_jq debug/broadcasts '[.events[] | select(.type == "turn_event")] as $e | any(range(0; $e|length) as $i | $e[$i].event.kind == "prose" and any(range($i + 1; $e|length) as $j | $e[$j].event.kind == "tool_start"))' 5 >/dev/null
wait_jq debug/broadcasts 'all(.events[] | select(.type == "turn_event" and (.event.kind == "tool_start" or .event.kind == "tool_done")); ((.event.summary // "") | contains("{\"") | not))' 5 >/dev/null
wait_jq debug/broadcasts 'all(.events[] | select(.type == "turn_event" and (.event.kind == "tool_start" or .event.kind == "tool_done")); ((.event.id // "") | length > 0))' 5 >/dev/null
```

## Scenario B: Real Lash Agent

Use this mode to prove the real lash observation bridge. It requires a working provider configuration. The Codex provider uses credentials from `~/.codex/auth.json`; Anthropic may be used instead by setting `HIRSEL_PROVIDER=anthropic` and `ANTHROPIC_API_KEY`.

Start the host:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-hirsel-rbcov
export HIRSEL_AGENT=lash
export HIRSEL_PROVIDER=codex
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-turn-timeline-real
export HIRSEL_LISTEN="127.0.0.1:$PORT"
cargo run -p hirsel-host
```

Run:

```bash
post_json debug/reset '{}'
post_json debug/owner-message '{"client_id":"timeline-real-1","body":"Briefly say what you are about to do, then call shell.run with cmd `true`, then reply exactly `done`.","ref":null}' >/dev/null
wait_jq debug/broadcasts '[.events[] | select(.type == "turn_event")] | any(.event.kind == "tool_done" and .event.name == "shell_run")' 120 >/dev/null
```

Gates:

```bash
wait_jq debug/broadcasts '[.events[] | select(.type == "turn_event")] as $e | [$e[].seq] == ([$e[].seq] | sort)' 5 >/dev/null
wait_jq debug/broadcasts '[.events[] | select(.type == "turn_event")] as $e | any(range(0; $e|length) as $i | $e[$i].event.kind == "prose" and any(range($i + 1; $e|length) as $j | $e[$j].event.kind == "tool_start"))' 5 >/dev/null
wait_jq debug/broadcasts 'all(.events[] | select(.type == "turn_event" and (.event.kind == "tool_start" or .event.kind == "tool_done")); ((.event.summary // "") | contains("{\"") | not))' 5 >/dev/null
wait_jq debug/broadcasts 'all(.events[] | select(.type == "turn_event" and (.event.kind == "tool_start" or .event.kind == "tool_done")); ((.event.id // "") | length > 0))' 5 >/dev/null
wait_jq debug/chat '.messages[] | select(.author == "agent" and (.body | contains("done")))' 30 >/dev/null
```

## Success Gates

- `/debug/broadcasts` contains `turn_event` entries with ascending `seq` values.
- In a talk-then-act turn, a `prose` event appears before a later `tool_start`.
- Tool summaries for `tool_start` and `tool_done` do not contain raw `{"` JSON dumps.
- Tool events for `tool_start` and `tool_done` carry a non-empty `id`.
- The real variant reaches a `shell_run` `tool_done` and commits an Agent chat reply.
