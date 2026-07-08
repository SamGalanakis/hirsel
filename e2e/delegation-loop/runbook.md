# Delegation Loop Runbook

## Purpose

Prove slice 1's loop with the debug HTTP surface: Owner message -> Agent delegates to a Sub-agent -> Sub-agent terminal event is observable -> Agent files a requires-response Inbox Item with a Quick Reply -> Owner sends the Quick Reply as an Anchor-refed Chat message -> Agent acknowledges in Chat.

## Host Setup

Start the Hirsel Host with the fake Sub-agent Driver:

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
export HIRSEL_TOKEN=dev-token
export HIRSEL_DEBUG=1
export HIRSEL_DRIVER=fake
export HIRSEL_DATA_DIR=/tmp/hirsel-e2e-data
export HIRSEL_LISTEN=127.0.0.1:8420
cargo run -p hirsel-host
```

The Agent LLM is expected to be real for the full product scenario. In this scaffold, `HIRSEL_DRIVER=fake` exercises the Sub-agent leg deterministically and the `lash_runtime` facade files the Inbox Item when the fake terminal event arrives.

## Scenario

1. Reset:

```bash
curl -sS -X POST http://127.0.0.1:8420/debug/reset
```

2. Inject the Owner delegation request:

```bash
curl -sS -X POST http://127.0.0.1:8420/debug/owner-message \
  -H 'content-type: application/json' \
  -d '{"body":"Please delegate a trivial repo fix to a Sub-agent, then ask me before applying the result.","ref":null}'
```

3. Poll `/debug/processes` until at least one process exists. Record `processes[0].id`.

4. Poll `/debug/processes` until that process has `status: "done"` and an event with `type: "terminal"`.

5. Poll `/debug/inbox` until there is an item with:

- `requires_response: true`
- `status: "open"`
- at least one `quick_replies` entry

Record the item `anchor` and the first Quick Reply `value`.

6. Send the Quick Reply as an Anchor-refed Owner message:

```bash
curl -sS -X POST http://127.0.0.1:8420/debug/owner-message \
  -H 'content-type: application/json' \
  -d '{"body":"ship it","ref":ANCHOR_ID}'
```

Replace `ANCHOR_ID` with the Inbox Item's `anchor`.

7. Poll `/debug/chat` until an Agent-authored message appears after the Quick Reply saying it acknowledged the Inbox reply.

## Success Gates

- `/debug/processes` shows a Sub-agent process.
- The process reaches a terminal done event through the fake driver.
- `/debug/inbox` contains a requires-response Inbox Item with a Quick Reply.
- The Quick Reply is sent as a normal Owner Chat message with `ref` equal to the Inbox Item anchor.
- `/debug/chat` contains a later Agent acknowledgement.

The run is void if the tester posts the Inbox Item or acknowledgement manually.
