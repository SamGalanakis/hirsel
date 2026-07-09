# Inbox Read State Runbook

## Purpose

Prove protocol v1.3 inbox read state across the host debug surface and persisted SQLite storage:
the Agent files an unread Inbox Item, `POST /debug/read-item` marks it read, and the read state
survives a host restart on the same data dir.

## Start Host

Use the scripted delegation path so the Inbox Item is created by the host/Agent flow, not by the
tester.

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-inbox-email
export HIRSEL_AGENT=scripted
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-inbox-read
export HIRSEL_LISTEN=127.0.0.1:3097
cargo run -p hirsel-host
```

## Scenario

Reset:

```bash
curl -sS -X POST http://127.0.0.1:3097/debug/reset
```

Inject a delegation request that makes scripted mode spawn the fake Sub-agent and file the terminal
result in the Inbox:

```bash
curl -sS -X POST http://127.0.0.1:3097/debug/owner-message \
  -H 'content-type: application/json' \
  -d '{"body":"Please delegate a trivial repo fix to a Sub-agent, then ask me before applying the result.","ref":null}'
```

Poll `/debug/inbox` until an open item with `requires_response: true` appears. Record its `id`.
Gate: the item has `"read": false`.

Mark it read:

```bash
curl -sS -X POST http://127.0.0.1:3097/debug/read-item \
  -H 'content-type: application/json' \
  -d '{"item_id":ITEM_ID}'
```

Replace `ITEM_ID` with the recorded Inbox Item id.

Poll `/debug/inbox` until that same item has `"read": true`.

Stop the host with SIGTERM, then boot it again with the same `HIRSEL_DATA_DIR` and `HIRSEL_LISTEN`.
Gate: `/debug/health` returns `ok: true`, and `/debug/inbox` still shows the same item id with
`"read": true`.

## Success Gates

- `/debug/health` returns `ok: true` before and after restart.
- The scripted delegation creates an Inbox Item through the Agent flow.
- The new Inbox Item arrives with `read: false`.
- `POST /debug/read-item { "item_id": id }` returns that item with `read: true`.
- `/debug/inbox` shows `read: true` before restart.
- After host restart on the same data dir, `/debug/inbox` still shows the same item id with
  `read: true`.

The run is void if the tester directly inserts the Inbox Item or edits the SQLite database.
