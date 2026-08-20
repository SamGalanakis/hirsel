# Hirsel Context

Hirsel is a single-player personal orchestration system: one Owner, one globally aware Agent, one Host. The interface should feel effortless even while the Agent coordinates many processes. Permission products and multi-tenancy are out of scope.

## Current product language

**Hirsel Host:** The long-running Rust process that embeds lash, owns durable state, terminates transport connections, and supervises processes.
_Avoid_: server, backend, daemon

**Agent:** The Owner's one long-lived intelligence and global orchestrator. It knows the global conversation, every Task, and work happening in each Task. Focus changes the subject, never the interlocutor.
_Avoid_: assistant, router, meta-agent

**Owner:** The single human Hirsel serves.
_Avoid_: user when it could mean a Sub-agent tool user

**Task:** The only durable visible work object. A Task has a stable identity, an Anchor, related conversation, an open/done lifecycle, and a constrained generated instrument. Opening a Task is a dive into work, not a new conversation destination. Removing its composer scope keeps the Task open while the Owner speaks globally.
_Avoid_: card, inbox item, notification, queue item, thread

**Global conversation:** The standing exchange between Owner and Agent across everything. It is the resting state and remains aware of every Task and Task-scoped exchange. There is one composer; Task scope adds an Anchor and Task mention to an ordinary message, and can be removed without navigating away.
_Avoid_: chat destination, session list, global thread

**Task ref:** A Task's citation form, `#<id>` — the wire id with a `#`. One spelling everywhere: on the Task chip, on the focused Task card (where one click copies it), typed in the composer to cite a Task, rendered as an inline tag in conversation, and as the tail of the Task's `/t/<id>` address. Typing `#` in the composer opens the Task picker; the composed text is the only record, re-parsed into `send_message.mentions` at send time. Chip focus decides where a message lives (its Anchor); a typed ref decides what it cites.
_Avoid_: handle, tag, hashtag, @mention

**Task margin:** The Task-related slice of the same conversation, selected by durable Anchor and Task mentions. It provides local context beside the Task instrument without creating a nested thread or separate Agent.
_Avoid_: side chat, sub-conversation, fork

**Generated instrument:** A Task's primary interface: a validated, semantic JSON component tree rendered through the shared catalog. It may recompose in place after an action—for example, a deploy choice becoming a canary checkpoint—while Task identity and context remain stable. The renderer owns layout, type, color, accessibility, and fallback behavior.
_Avoid_: arbitrary generated app, HTML blob, card template

**Temporary utility:**
Processes, Settings, and Canvas. A utility overlays or docks beside the current Task world, never becomes a destination, and returns to the same Task, scope, draft, and focus when closed.

**Process:**
A visible Sub-agent or monitor run. Processes are observable background work, not places the Owner enters. Steering routes through the Agent.

**Sub-agent:** An external coding agent driven by the Agent through a native Sub-agent Driver. Sub-agents do work; they never talk to the Owner directly. Their sessions are disposable; the Agent's transcript and Task state are durable.
_Avoid_: worker, child agent, ACP session

**Sub-agent Driver:** The Host adapter that spawns, prompts, interrupts, and normalizes one Sub-agent CLI's native headless protocol.
_Avoid_: ACP client, connector

**Anchor:** The durable conversation message a Task points to. Task-scoped Owner messages reference that Anchor and mention the Task, preserving attribution without implicitly settling it.
_Avoid_: thread

## Wire compatibility language

The current product vocabulary above is authoritative. Older protocol and storage spellings remain while compatibility is retired:

- `EventItem`, `event_upsert`, and `event_action` carry Tasks.
- `ping`, `ping_id`, `ping:<id>`, `pings`, and `resolve_ping` are legacy wire/storage names only. They are not visible Pings, a Tray, or an Inbox.
- `ChatMessage`, `chat`, and `ref` are wire/code spellings for conversation messages and Anchors. They do not define a Chat destination.
- Side-chat frames and debug routes are legacy protocol coverage only. There is no Side Chat in the current client. The compatibility runtime is default-off, requires `HIRSEL_COMPAT_SIDE_SESSIONS=1`, and starts its session/reaper lazily on the first legacy open frame.
- Quick Replies are a compatibility field. Current Task interaction is the generated instrument (`optionList`, fields, submit, status, and later catalog nodes).

Historical ADRs retain the terms that were true when written. `PRODUCT.md`, `DESIGN.md`, this file, and `docs/product-direction.md` define the current visible product.
