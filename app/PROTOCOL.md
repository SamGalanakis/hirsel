# hirsel client protocol v1 (canonical)

Transport-agnostic bidirectional message stream (ADR-0006). v1 carrier: WebSocket, JSON text frames, one message per frame. No HTTP-isms in the protocol. This file is the single source of truth for both the Rust `hirsel-proto` crate and the PWA client; if it changes, both change.

## Types

```
ChatMessage {
  id: u64,                 // monotonic, host-assigned
  author: "owner" | "agent",
  body: string,            // markdown
  ref: u64 | null,         // id of the chat message this replies to (WhatsApp-quote style)
  ts: string               // RFC3339
}

InboxItem {
  id: u64,
  content: string,         // markdown
  anchor: u64,             // ChatMessage.id where the inbox tool was called
  requires_response: bool,
  quick_replies: [ { value: string, label: string } ],   // may be empty
  status: "open" | "archived",
  ts: string
}
```

## Client → server

```
{ "type": "hello", "token": string, "last_seen_msg_id": u64 | null }
{ "type": "send_message", "client_id": string, "body": string, "ref": u64 | null }
{ "type": "archive_item", "item_id": u64 }
```

- `hello` must be the first frame; anything else before it → close.
- `client_id` is a client-generated idempotency key (uuid); host dedupes resends after reconnect.
- Quick-reply tap = `send_message { body: value, ref: item.anchor }`. No separate message kind.

## Server → client

```
{ "type": "hello_ok", "latest_msg_id": u64, "messages": [ChatMessage], "inbox": [InboxItem] }
{ "type": "msg", "message": ChatMessage }
{ "type": "agent_activity", "state": "thinking" | "idle", "text": string | null }
{ "type": "inbox_upsert", "item": InboxItem }
{ "type": "error", "detail": string }
```

- `hello_ok.messages` = replay of everything after `last_seen_msg_id` (or last 200 if null). `inbox` = all open items + last 20 archived.
- `msg` includes the owner's own messages echoed back with host-assigned id (client reconciles via `client_id`? No — v1 keeps it simple: client renders optimistically, replaces optimistic entry with the first `msg` whose author=owner and body matches; good enough single-player).
- `agent_activity` is ephemeral (live turn preview from lash session observation); never stored, never replayed.
- `inbox_upsert` is full-item upsert; archiving arrives as an upsert with status=archived.

## Auth

Single static bearer token (env `HIRSEL_TOKEN`) carried in `hello`. Wrong/missing → `error` + close.
