#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/workspace/.cargo-target-scaffold}"
export HIRSEL_TOKEN="${HIRSEL_TOKEN:-dev-token}"

HOST_PID=""
HOST_LOG="${HOST_LOG:-/tmp/hirsel-e2e-host.log}"
BASE=""

repo_root() {
  local lib_dir
  lib_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  cd -- "$lib_dir/../../.." && pwd
}

build_host() {
  if [[ -z "${HOST_BIN:-}" ]]; then
    cargo build -p hirsel-host
  elif [[ ! -x "$HOST_BIN" ]]; then
    printf 'HOST_BIN is not executable: %s\n' "$HOST_BIN" >&2
    return 1
  fi
}

host_bin() {
  printf '%s\n' "${HOST_BIN:-$CARGO_TARGET_DIR/debug/hirsel-host}"
}

port_is_busy() {
  local port="$1"
  ss -tlnp | awk '{print $4}' | grep -Eq "(:|])${port}$"
}

choose_port() {
  local start="${1:-3090}"
  local port
  ss -tlnp >&2 || true
  for ((port = start; port < start + 80; port++)); do
    if ! port_is_busy "$port"; then
      printf '%s\n' "$port"
      return 0
    fi
  done
  printf 'no free port found from %s\n' "$start" >&2
  return 1
}

wait_health() {
  local timeout="${1:-30}"
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if curl -fsS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/debug/health" | jq -e '.ok == true and .debug == true' >/dev/null 2>&1; then
      return 0
    fi
    if [[ -n "${HOST_PID:-}" ]] && ! kill -0 "$HOST_PID" 2>/dev/null; then
      printf 'host exited early; log follows\n' >&2
      tail -200 "$HOST_LOG" >&2 || true
      return 1
    fi
    sleep 0.25
  done
  printf 'timed out waiting for host health at %s\n' "$BASE" >&2
  tail -200 "$HOST_LOG" >&2 || true
  return 1
}

start_hirsel_host() {
  local mode="${1:-fresh}"
  : "${DATA_DIR:?DATA_DIR must be set}"
  : "${PORT:?PORT must be set}"
  : "${HIRSEL_AGENT:?HIRSEL_AGENT must be set}"
  : "${HIRSEL_DRIVER:?HIRSEL_DRIVER must be set}"
  : "${HIRSEL_PROVIDER:=codex}"
  : "${HIRSEL_MODEL:=gpt-5.6-sol}"

  if port_is_busy "$PORT"; then
    printf 'port %s is busy; refusing to start host\n' "$PORT" >&2
    ss -tlnp >&2 || true
    return 1
  fi

  build_host
  if [[ "$mode" == "fresh" ]]; then
    rm -rf "$DATA_DIR"
  fi
  mkdir -p "$DATA_DIR" "$(dirname "$HOST_LOG")"
  BASE="http://127.0.0.1:$PORT"

  (
    export CARGO_TARGET_DIR
    export HIRSEL_AGENT HIRSEL_DRIVER HIRSEL_PROVIDER HIRSEL_MODEL
    export HIRSEL_TOKEN
    export HIRSEL_DEBUG=1
    export HIRSEL_DATA_DIR="$DATA_DIR"
    export HIRSEL_TEMPLATES_DIR="${HIRSEL_TEMPLATES_DIR:-$(repo_root)/templates}"
    export HIRSEL_LISTEN="127.0.0.1:$PORT"
    export RUST_LOG="${RUST_LOG:-hirsel_host=info,hirsel_drivers=info,warn}"
    if [[ -n "${HIRSEL_FAKE_FIXTURE:-}" ]]; then
      export HIRSEL_FAKE_FIXTURE
    fi
    exec "$(host_bin)"
  ) >"$HOST_LOG" 2>&1 &
  HOST_PID=$!
  wait_health 45
}

stop_hirsel_host() {
  local sig="${1:-TERM}"
  if [[ -z "${HOST_PID:-}" ]]; then
    return 0
  fi
  if kill -0 "$HOST_PID" 2>/dev/null; then
    kill "-$sig" "$HOST_PID" 2>/dev/null || true
    for _ in {1..40}; do
      if ! kill -0 "$HOST_PID" 2>/dev/null; then
        wait "$HOST_PID" 2>/dev/null || true
        HOST_PID=""
        return 0
      fi
      sleep 0.25
    done
    kill -KILL "$HOST_PID" 2>/dev/null || true
    wait "$HOST_PID" 2>/dev/null || true
  fi
  HOST_PID=""
}

restart_hirsel_host() {
  stop_hirsel_host TERM
  start_hirsel_host preserve
}

post_json() {
  local path="$1"
  local body="$2"
  curl -fsS -X POST "$BASE/$path" -H "authorization: Bearer $HIRSEL_TOKEN" -H 'content-type: application/json' -d "$body"
}

get_json() {
  local path="$1"
  curl -fsS -H "authorization: Bearer $HIRSEL_TOKEN" "$BASE/$path"
}

wait_jq() {
  local path="$1"
  local filter="$2"
  local timeout="${3:-60}"
  local deadline=$((SECONDS + timeout))
  local json=""
  while (( SECONDS < deadline )); do
    json="$(get_json "$path")"
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf '%s\n' "$json"
      return 0
    fi
    if [[ -n "${HOST_PID:-}" ]] && ! kill -0 "$HOST_PID" 2>/dev/null; then
      printf 'host exited while waiting for %s filter %s\n' "$path" "$filter" >&2
      tail -200 "$HOST_LOG" >&2 || true
      return 1
    fi
    sleep 0.5
  done
  printf 'timed out waiting for %s filter %s\n' "$path" "$filter" >&2
  if [[ -n "$json" ]]; then
    printf '%s\n' "$json" | jq . >&2 || printf '%s\n' "$json" >&2
  fi
  tail -120 "$HOST_LOG" >&2 || true
  return 1
}

assert_no_jq_for() {
  local path="$1"
  local filter="$2"
  local seconds="$3"
  local deadline=$((SECONDS + seconds))
  local json
  while (( SECONDS < deadline )); do
    json="$(get_json "$path")"
    if printf '%s' "$json" | jq -e "$filter" >/dev/null; then
      printf 'unexpected match for %s filter %s\n' "$path" "$filter" >&2
      printf '%s\n' "$json" | jq . >&2 || printf '%s\n' "$json" >&2
      return 1
    fi
    sleep 0.5
  done
}

max_chat_id() {
  get_json debug/chat | jq '[.messages[].id] | max // 0'
}

wait_agent_message_after() {
  local after_id="$1"
  local filter="$2"
  local timeout="${3:-120}"
  wait_jq debug/chat '.messages[] | select(.author == "agent" and .id > ('"$after_id"') and ('"$filter"'))' "$timeout" >/dev/null
}

debug_snapshot() {
  local prefix="$1"
  mkdir -p "$(dirname "$prefix")"
  get_json debug/health >"$prefix.health.json" || true
  get_json debug/chat >"$prefix.chat.json" || true
  get_json debug/pings >"$prefix.pings.json" || true
  get_json debug/processes >"$prefix.processes.json" || true
  get_json debug/broadcasts >"$prefix.broadcasts.json" || true
  cp "$HOST_LOG" "$prefix.host.log" 2>/dev/null || true
}

pass_gate() {
  printf 'PASS %s\n' "$*"
}

fail_gate() {
  printf 'FAIL %s\n' "$*" >&2
}
