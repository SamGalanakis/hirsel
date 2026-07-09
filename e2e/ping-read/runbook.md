# Ping Read State Runbook

## Purpose

Prove Ping read state across the host debug surface and persisted SQLite storage:
the Agent sends an unread Ping, `POST /debug/read-ping` marks it read, and the read state
survives a host restart on the same data dir.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/ping-read/run.sh
```

## Start Host

Use the scripted delegation path so the Ping is created by the host/Agent flow, not by the
tester.

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-ping-read
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-ping-read
export HIRSEL_LISTEN=127.0.0.1:3097
cargo run -p hirsel-host
```

## Scenario

Reset:

```bash
curl -sS -X POST http://127.0.0.1:3097/debug/reset
```

Inject a delegation request that makes scripted mode spawn the fake Sub-agent and file the terminal
result in a Ping:

```bash
curl -sS -X POST http://127.0.0.1:3097/debug/owner-message \
  -H 'content-type: application/json' \
  -d '{"body":"Please delegate a trivial task to a Sub-agent working in /tmp/hirsel-e2e-ping-read-work (create the directory if needed), then ask me before applying the result.","ref":null}'
```

Poll `/debug/pings` until an open Ping with `requires_response: true` appears. Record its `id`.
Gate: the Ping has non-empty `name` and `description`, plus `"read": false`.

Mark it read:

```bash
curl -sS -X POST http://127.0.0.1:3097/debug/read-ping \
  -H 'content-type: application/json' \
  -d '{"ping_id":PING_ID}'
```

Replace `PING_ID` with the recorded Ping id.

Poll `/debug/pings` until that same Ping has `"read": true`.

Stop the host with SIGTERM, then boot it again with the same `HIRSEL_DATA_DIR` and `HIRSEL_LISTEN`.
Gate: `/debug/health` returns `ok: true`, and `/debug/pings` still shows the same Ping id with
`"read": true`.

## Success Gates

- `/debug/health` returns `ok: true` before and after restart.
- The scripted delegation creates a named, described Ping through the Agent flow.
- The new Ping arrives with `read: false`.
- `POST /debug/read-ping { "ping_id": id }` returns that Ping with `read: true`.
- `/debug/pings` shows `read: true` before restart.
- After host restart on the same data dir, `/debug/pings` still shows the same Ping id with
  `read: true`.

The run is void if the tester directly inserts the Ping or edits the SQLite database.
