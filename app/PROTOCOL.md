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

## v1.1 — attachments (2026-07-09)

```
Blob { id: string (uuid), name: string, mime: string, size: u64 }
```

Client → server additions:
```
{ "type": "upload_blob", "client_id": string, "name": string, "mime": string, "data_b64": string }
  // decoded size cap 15 MB; server stores under data_dir/blobs/ and replies blob_ok (or error)
{ "type": "send_message", ..., "attachments": [blob_id] }   // optional, default []
```

Server → client additions:
```
{ "type": "blob_ok", "client_id": string, "blob": Blob }
ChatMessage gains "attachments": [Blob]   // default []
```

Blob CONTENT is fetched as an asset (like the static app shell, outside the message protocol):
`GET /blob/{id}?token=<HIRSEL_TOKEN>` — same-origin from the PWA; image/* renders inline, others download.

Agent-side semantics (host): image/* attachments are fed to the model turn via lash `TurnInput::with_image_blob`
(vision); all attachments are also written to disk and the turn text notes each stored path so the Agent
can read documents with its own tools.

Queueing: no protocol change — client keeps optimistic/pending sends with stable client_ids; host enqueue
is already durable+idempotent mid-turn.

## v1.2 — CLI-grade send/queue/cancel semantics (2026-07-09)

Modeled on lash-tui (Early Injection vs Next Full Turn) and codex/claude CLI conventions.

Client → server changes:
```
send_message gains "mode": "send" | "next_turn"   (optional, default "send")
  // "send": if an agent turn is active, deliver as Early Injection (earliest safe boundary
  //          in the active turn); otherwise normal next-turn ingress. This is plain Enter.
  // "next_turn": always hold until the current turn commits (lash Next Full Turn). This is Tab.
{ "type": "cancel_turn" }                  // cooperatively interrupt the ACTIVE agent turn (Esc).
                                           // No-op if idle. Host answers with agent_activity idle.
{ "type": "cancel_queued", "client_id" }   // cancel a not-yet-claimed queued message (host maps
                                           // client_id -> lash pending-input id). If already claimed,
                                           // host replies error "already claimed".
```

Chat storage/UX: cancelled queued messages are removed from chat history (they never reached the
Agent); host broadcasts a tombstone `{ "type": "msg_removed", "id": u64 }` so clients drop the bubble.

Keyboard map (client, fine-pointer): Enter = send/inject · Shift+Enter = newline · Tab (composer
non-empty) = queue next-turn · Esc = cancel active turn · ArrowUp (empty composer) = recall last
owner message into the composer for editing/resend. Phone: send button; long-press send = queue
next-turn; a stop control is visible while agent_activity=thinking.
Queue affordance: bubbles for messages with mode=next_turn show a "queued" chip while a turn is
active; chip clears when agent_activity returns to idle or the message's turn starts.

## v1.3 — email-like inbox read state (2026-07-09)

```
InboxItem gains "read": bool   (optional, default false)
```

Client → server addition:
```
{ "type": "read_item", "item_id": u64 }   // idempotent; host sets read=true, broadcasts inbox_upsert
```

Semantics: `read` is Owner-side "seen" state, set automatically by the client when an item is
viewed (email-like: visible in the viewport ~1.5s, or on first interaction with the card). It is
orthogonal to *replied* (derived from anchor-refed owner messages) and to *status* (open|archived).
The client flips `read=true` optimistically on send and the `inbox_upsert` reconciles it.

There is deliberately NO wire "unread" op: "Mark unread" is a client-only override the PWA keeps
locally (it does not round-trip). A subsequent auto-read/"Mark read" clears the override and sends
`read_item`.

UI language: `archived` is presented as **Deleted** (a trash section); the wire and storage keep
`archived`. **Delete** (the destructive action) lives only in each card's ⋯ context menu.

Badge = count of **open + unread** items (was open + requires_response). `document.title` mirrors it.
`requires_response` no longer drives the badge; it keeps its visual accent and remains the only
(future) push trigger.

## v1.4 — process visibility, tool-call visibility, monitors (2026-07-09)

```
ProcessInfo {
  id: string, kind: "subagent" | "monitor", label: string,
  agent: string | null, model: string | null,          // subagent kind only
  state: "running" | "done" | "failed" | "cancelled" | "abandoned",
  started_ts: string, last_event_ts: string, summary: string | null
}
hello_ok gains "processes": [ProcessInfo]              // all non-terminal + last 10 terminal
{ "type": "process_upsert", "process": ProcessInfo }   // broadcast on any state/summary change
// NOTE: v1.4 also shipped { "type": "agent_tool_call", ... } for live tool rows. It was REMOVED
// in v1.5 and superseded by the richer `turn_event` stream — see the v1.5 section below.
ChatMessage gains "tool_calls": [ { "name": string, "ok": bool } ]   // default []; stamped on
  // committed agent messages from lash's per-turn RemoteToolCallSummary
```

Client-side semantics:

- The **Processes tab** badge counts `state=="running"` processes only, independent of the Inbox
  unread badge; `document.title` keeps Inbox semantics. Rows are grouped Running / Finished (terminal
  states), newest `last_event_ts` first. Sub-agent rows show agent+model chips and expand to a full
  view with an **"Ask to stop"** action — this switches to Chat and pre-fills the composer with
  `stop process <id> (<label snippet>)`. Interrupts route through the Agent by design; there is no
  direct client-side kill frame.
- Live tool visibility under the "Thinking…" marker while a turn runs is now driven by the v1.5
  `turn_event` stream (see below), which replaced v1.4's `agent_tool_call`. The live timeline is
  cleared the moment the turn commits (an agent `msg` arrives or `agent_activity` goes idle) and is
  never stored past the running turn.
- A committed agent message with a non-empty `tool_calls` renders a collapsed **"⚙ N tools"** chip in
  its footer, expanding inline to the per-tool name + ok (check/cross) list. No chip when empty.

Monitors are Agent-created host-run probes (new lashlang tools, no client protocol surface beyond
ProcessInfo): monitors.create { cmd, every_secs (floor 30), wake_on:
"changed"|"exit_zero"|"exit_nonzero"|"regex", pattern?, label } / monitors.list / monitors.cancel.
Persisted across restarts (like timer schedules); each is a lash Runtime Process that runs the probe
on its interval and appends a wake event ONLY when the condition fires (payload: label + probe output
tail).

> **Superseded by v1.5:** `agent_tool_call` is removed from the wire and replaced by the richer
> `turn_event` stream (below). The committed-message `tool_calls` summary is unchanged.

## v1.5 — running-turn timeline (2026-07-09)

Replaces v1.4 `agent_tool_call` with an ordered `turn_event` stream so the client renders the
running turn as a lash-CLI-style timeline — streaming prose interleaved with tool rows and collapsed
reasoning, in exact `seq` order — instead of a bare list of tool rows. Intermediate prose (invisible
in v1.4) is now surfaced.

Server → client:
```
{ "type": "turn_event", "seq": u64, "event": TurnEvent }

TurnEvent  (tagged by "kind"):
  { "kind": "prose",      "text": string }                       // markdown delta → current prose block
  { "kind": "reasoning",  "text": string }                       // reasoning delta → current reasoning run
  { "kind": "tool_start", "id": string, "name": string, "summary": string | null }             // opens a tool row
  { "kind": "tool_done",  "id": string, "name": string, "ok": bool, "summary": string | null }  // resolves the row by "id"
```

- `seq` strictly orders events within the turn. Clients render in seq order, tolerate gaps (a missing
  seq is skipped, never buffered or reordered), and treat a redelivered seq idempotently (replace).
- Block model: consecutive same-kind deltas accumulate into one block/run; any change of kind
  (including a `tool_start`) closes the current prose/reasoning block, and later prose opens a new one.
  `tool_start` inserts a tool row at its seq position; `tool_done` updates the matching-`id` row in
  place with a spinner→check/cross result and its own condensed `summary` (it is not a separate row).
  `tool_done` also carries the tool `name`, so a `tool_done` with no matching `tool_start` (e.g. the
  start was lost across a reconnect mid-turn) is not dropped — it renders as an already-completed row
  labelled from that `name`.
- `summary` fields are clean one-liners produced host-side (no raw JSON); the client renders them as-is.
- Ephemeral like `agent_activity`: never stored or replayed. The client clears the live timeline on
  turn commit (an agent `msg`) or `agent_activity` idle. On commit the client MAY retain the finished
  timeline in session memory keyed to the committed message (a "turn details" affordance); client-only,
  not persisted, gone after reload.

## v2.0 — side chats: session-scoped frames (ADR-0008) (2026-07-09)

Breaks the "exactly one session" assumption. `send_message` and `cancel_turn` MAY carry
`"sc": string` (side chat id) to target a side session; `msg`, `turn_event`, and `agent_activity`
carry `"sc"` when side-scoped. When `sc` is absent the frame belongs to the main conversation and
its wire shape is byte-identical to v1.5 (the key is omitted, never null).

Client → server:
```
{ "type": "open_side_chat", "client_id": string, "item_id": u64 }
  → { "type": "side_chat_open", "sc": string, "item_id": u64, "messages": [ChatMessage] }
  // idempotent per item: if the item already has a live side chat the host answers with the SAME
  // sc and the transcript so far; otherwise a fresh scope ("side:<uuid>") with messages: [] —
  // the seed lives in the side session's prompt layer, not as transcript rows.
send_message { ..., "sc" } / cancel_turn { "sc" }  → routed to that side session
{ "type": "conclude_side_chat", "sc" }             // side agent drafts the Owner's reply (a real side turn)
  → { "type": "conclusion_draft", "sc", "text" }   // NOT appended to the side transcript
{ "type": "confirm_conclusion", "sc", "text" }     // owner-edited final text
  → host posts the Owner's anchor-refed reply in MAIN chat (normal msg flow, normal agent enqueue,
    idempotency client_id "side-conclude:<sc>"), archives the item (IDEMPOTENT — a no-op if the
    Agent already archived it), discards the side session + transcript
  → { "type": "side_chat_closed", "sc" }
{ "type": "discard_side_chat", "sc" } → side_chat_closed (no conclusion, item stays open)
```

Server → client:
```
hello_ok gains "side_chats": [ { "sc": string, "item_id": u64 } ]   // default []; live side chats
                                                                    // for reconnect + scoped replay
{ "type": "side_chat_open", "sc", "item_id", "messages": [ChatMessage] }
{ "type": "conclusion_draft", "sc", "text" }
{ "type": "side_chat_closed", "sc" }
```

Side transcripts persist only while the side chat lives (they survive host-side across reconnects;
message ids are a separate sequence from main chat) and are deleted on close (conclude/discard/TTL);
they never survive a host restart. Side sessions: one ephemeral lash session per side chat
(in-memory store, never the durable session store), seeded at open with the agent prompt + a
host-rendered context block (the Inbox Item, its Anchor message, and the last ~20 main-chat
messages), full tool access, NO process/timer wakes routed to them (wakes stay main-session only).
Stale side chats are silently closed (`side_chat_closed`) after `HIRSEL_SIDECHAT_TTL_SECS`
(default 86400) without activity.
