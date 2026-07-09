# Inbox Lifecycle Runbook

## Purpose

Prove Inbox lifecycle behavior beyond read/unread: a requires-response item is filed by the Agent, the Owner replies through an anchor-refed message, owner archive/delete is visible to the host, and the Agent can archive a moot item itself.

## Execute

```bash
export CARGO_TARGET_DIR=/workspace/.cargo-target-scaffold
e2e/inbox-lifecycle/run.sh
```

Scenario A uses `HIRSEL_AGENT=scripted HIRSEL_DRIVER=fake` for deterministic delegation, reply, and owner archive. Because this branch has no debug HTTP archive route, owner archive is sent over the canonical WebSocket `archive_item` frame and gated through debug state.

Scenario B uses `HIRSEL_AGENT=lash HIRSEL_PROVIDER=codex HIRSEL_DRIVER=fake` to ask the real Agent to file and then archive a moot item.

## Gates

Scenario A:

- Scripted delegation files an open `requires_response: true` item with a Quick Reply.
- The Owner sends an anchor-refed Chat reply using the Inbox item's `anchor`.
- The Agent acknowledges that reply in Chat.
- A second item is archived by an owner WebSocket `archive_item` frame.
- `/debug/inbox` and `/debug/broadcasts` show that item as `status: "archived"`.

Scenario B:

- The real Agent files a `requires_response` item via `inbox.file`.
- After the Owner says the item is moot, the Agent calls `inbox.archive`.
- The item becomes `status: "archived"` and the Agent replies with `MOOT_ARCHIVED`.

Real-Agent failure to choose `inbox.file` or `inbox.archive` is a prompt-behavior miss. Missing archive state after the WebSocket frame is mechanical.
