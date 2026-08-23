# Hirsel Task and conversation protocol

This is the canonical product-facing contract for the web app and Rust Host.
The shared wire schema is implemented in
[`crates/hirsel-proto/src/lib.rs`](../crates/hirsel-proto/src/lib.rs); the
browser's hand-written subset is
[`app/src/protocol.ts`](src/protocol.ts). Historical version-by-version wire
notes and the retained `Ping`, `Chat`, and side-session spellings live in the
[`wire compatibility appendix`](PROTOCOL-COMPATIBILITY.md).

Transport is a bidirectional stream. The current carrier is WebSocket JSON text
frames, one message per frame. The native client carries the same protocol over
iroh. Unless a section explicitly says otherwise, the Host owns durable truth
and the client reconciles to Host snapshots and upserts.

## Product model

Hirsel has one globally aware Agent, one global conversation, and a flat
collection of Tasks.

- A **Task** is one durable `Event` record. Its `id` and `anchor` are stable
  identity; its `ui`, lifecycle fields, and description may change in place.
- A **Task dive** changes the subject presented around the standing composer.
  It does not create another Agent, transcript, or nested navigation stack.
- **Global conversation** is the same conversation with no Task scope. Leaving
  a Task's scope does not close the visible Task.
- A Task-scoped Owner message carries the Task's `anchor` as `ref` and its
  `id` in `mentions`. That gives the global Agent exact Task context while the
  persisted conversation remains one stream.
- **Generated Task UI** is an instrument, not client-authored state. The
  client renders the Host-provided constrained tree and emits only actions.
- **Canvas, Processes, and Settings** are temporary utilities over the current
  Task world. They never become conversation destinations.

The compatibility wire still calls a stored conversation row `ChatMessage` and
retains some `ping_*` and `side_chat_*` frames. Those are serialization names,
not product destinations.

## Core records

### Conversation row

```text
ChatMessage {
  id: u64
  author: "owner" | "agent"
  body: string
  ref: u64 | null
  ts: RFC3339
  attachments: Blob[]
  tool_calls: ToolCallSummary[]
}
```

`ref` is an Anchor/reply edge. The browser derives a Task's visible
conversation margin from the Task Anchor, later reply edges, and the immediate
Agent response to a Task-scoped Owner message. Another Task's Anchor is a hard
boundary, so neighboring work does not leak into the margin.

The global field shows the recent global conversation. The Task field and
global field are projections of the same stored stream, not separate
transcripts.

### Task

The Rust wire type is `Event`; the browser calls it `EventItem` to avoid
shadowing the DOM `Event` global.

```text
Event {
  id: u64
  kind: "judgment" | "summary" | "info"
  source: { kind: "agent" | "subagent" | "scheduled" | "monitor", ref: string | null }
  name: string
  description: string
  ui: constrained JSON tree
  anchor: ChatMessage.id
  requires_response: bool
  quick_replies: QuickReply[]
  status: "open" | "done"
  read: bool
  archived: bool
  snoozed_until: RFC3339 | null
  archived_at: RFC3339 | null
  fork_sc: string | null
  ts: RFC3339
}
```

`hello_ok.events` is the authoritative Task snapshot on connect or resync.
`event_upsert` replaces one Task by `id`. Open, unarchived, unsnoozed Tasks form
the primary inventory; the client derives ordering and attention tone rather
than persisting another navigation structure.

`kind` controls attention:

- `judgment` asks the Owner to decide;
- `summary` is durable synthesized awareness;
- `info` is a quiet durable notification.

`status`, `archived`, and `snoozed_until` are distinct axes. A done Task remains
reopenable. Archive is reversible visibility, and snooze is a Host-timed
temporary absence.

## One conversation, two speaking scopes

The standing composer sends:

```json
{
  "type": "send_message",
  "client_id": "<uuid>",
  "body": "message",
  "ref": 42,
  "mentions": [7],
  "attachments": [],
  "mode": "send"
}
```

When Task 7 is scoped, `ref` is its Anchor (42) and `mentions` includes 7.
Removing the scope chip sends globally while leaving Task 7 open on screen.
Selecting another Task replaces the subject directly; there is no back-stack
of task sessions.

`client_id` is the idempotency key used across reconnects. `mode` is `send`
(normal ingress or early injection into an active turn) or `next_turn` (hold
until the current turn commits). `cancel_turn` cooperatively interrupts the
single active global turn; `cancel_queued` removes an unclaimed queued message.

Host conversation output arrives as:

- `msg` for a committed conversation row;
- `agent_activity` for ephemeral thinking/idle state;
- `turn_event` for ephemeral ordered prose, reasoning, tool activity, and the
  Agent's own program source (`code_start`/`code_done`, paired by `id`);
- `msg_removed` for a cancelled queued row.

`turn_event`'s `prose` deltas are the Agent's reply being written: the Host
forwards lash's per-chunk assistant text, coalesced into roughly 12 frames per
second so a reply streams visibly instead of landing as a finished paragraph.
The browser accumulates consecutive `prose` deltas and renders the trailing run
as the in-flight reply, in the same typography as the committed row it becomes
— so the commit replaces the draft in place, exactly once, with no duplicate
flash. No frame is invented for this: `prose` was always an append-chunk.

The live turn timeline is not replayed. A committed Agent row can retain
`tool_calls`; the browser may retain fuller turn details in session memory
until reload.

## Generated Task instruments

`Event.ui` is the Task's primary interface. The accepted Task catalog is closed
and Host-validated:

```text
card, inset, eyebrow, heading, text, keyValue, badge, status,
divider, optionList, field, submit, viewSlot
```

Unknown components, excess fields, invalid option keys, and over-large/deep
trees are rejected by the Host. A `viewSlot` may contain only a validated View
template. The browser renderer is total and defensive, but it is not an
authority that can make arbitrary JSON durable.

The current production bounds are 128 component nodes, depth 8, 16 declared
actions, 16 fields, and 16 choices per option list. Action names are at most 64
bytes. Action data must be a JSON object no larger than 8 KiB; field and option
strings are at most 1 KiB. Action names, field names, and option keys are
unique within one instrument. A field currently has `kind: "text"` (or omits
`kind`) and may declare `required: true`.

An interaction emits only:

```json
{
  "type": "event_action",
  "event_id": 7,
  "action": "advance",
  "data": { "choice": "A", "label": "Inspect details" }
}
```

There are two action paths.

### Host lifecycle actions

The Host owns settlement and reversible lifecycle transitions:

- `choose` validates a declared judgment option, posts the choice into the
  global conversation at the Task Anchor, and settles the Task;
- `submit` and `dismiss` settle;
- `reopen` returns a done Task to open without discarding its last generated
  instrument;
- `snooze` requires a future RFC3339 `data.until`; `unsnooze` reverses it;
- `archive` and `unarchive` change visibility without inventing a new Task.

The client may render a brief optimistic state, but `event_upsert` is final.

### Adaptive generated actions

A producer may declare a non-lifecycle action with `"settles": false`. For such
an action the Host:

1. loads the current open Task;
2. validates that the action and payload are declared by its current `ui`;
3. submits an exact Task-action context to the same global Agent session;
4. permits `events.recompose` only for that exact open Task; and
5. broadcasts the authoritative same-`id`, same-`anchor` `event_upsert`.

The context contains the current Task identity, source, status, Anchor, UI,
action, and data. `events.recompose { event_id, description?, ui }` can replace
only the presentation of the Task that woke the turn. It cannot create a
nested Agent or Task, change identity, settle lifecycle, or upsert arbitrary
client JSON.

This is how a simple instrument can advance through radically different stages
without turning the UI into a route labyrinth. A later Host lifecycle action
settles the recomposed Task. Reload and reopen preserve the meaningful latest
instrument.

Payload validation is closed rather than best-effort. Option actions accept
only their declared `choice` and matching optional `label`. Form actions
accept only declared string fields and require every field marked `required`.
Unknown keys, missing required values, type mismatches, over-limit data,
duplicate declarations, and actions against an inactive or different Task are
rejected before an Agent turn can start.

## Standalone generated Views

`ViewInstance { instance_id, placement, spec }` is the separate, temporary View
substrate. `hello_ok.views` is authoritative; `view_upsert` replaces an
instance by `instance_id`, and `view_removed` drops it. `view_event` sends an
interaction back through normal Owner-message ingress.

Current placements are:

- `canvas`: the temporary Canvas utility;
- `chat`: the global conversation field;
- `ping:<event-id>`: compatibility spelling for a View attached to a Task.

Task instruments use `Event.ui`. A standalone View does not become a Task and
does not create a conversation scope.

## Plugin pushes

An enabled plugin can broadcast to every connected client:

```json
{ "type": "plugin_push", "plugin": "<id>", "topic": "...", "data": <any JSON> }
```

`plugin` is the plugin id (lowercase kebab, matching its folder under
`plugins/`). `topic` and `data` are the plugin's own vocabulary — the host
neither interprets nor validates them, it fans them out. A client routes the
frame to the UI module registered for that plugin id and ignores frames whose
`plugin` it does not know.

Plugins are otherwise addressed over HTTP, not the socket, under `/api/plugins`
(same bearer-token gate as the rest of the API):

- `GET /api/plugins` →
  `{"plugins":[{"id","label","version","state":"running"|"disabled"|"errored","error"?,"settings":[{key,label,kind,default?}],"values":{...}}]}`.
  Secret values read back as `"<set>"` when stored and `null` when unset.
- `POST /api/plugins/<id>/enabled` with `{"enabled":bool}`.
- `POST /api/plugins/<id>/settings` with `{"values":{...}}`; a value of
  `"<set>"` or an absent key leaves a stored secret unchanged.
- `/api/plugins/<id>/...` — the plugin's own routes, served only while it is
  enabled (404 otherwise). `enabled` and `settings` are reserved names.

## Temporary utilities

The browser keeps one local exclusive `rightRegion`:
`none | canvas | processes | settings`.

- **Processes** projects Host-backed `ProcessInfo` rows seeded by
  `hello_ok.processes` and updated by `process_upsert`.
- **Canvas** projects current `canvas` View instances. A Host-created Canvas
  View may make the utility available, but it does not replace Task selection.
- **Settings** combines local browser preferences with Host-backed
  `model`/`subagent_models`/`prompts`/`providers` snapshots and their change
  broadcasts, presented as side tabs: Appearance, Agents, Providers,
  Connection & devices, Notifications, About & debug, Plugins.

On wide screens a utility is an in-flow inspector; on phone it is a modal
sheet. Only the active utility is mounted. Closing it returns to the same Task,
composer scope, and draft. Opening one utility replaces another in the single
region instead of stacking overlays.

## Connection, snapshot, and auth

The first frame must be `hello`. The browser currently sends the accepted
static-token compatibility shape:

```json
{ "type": "hello", "token": "<HIRSEL_TOKEN>", "last_seen_msg_id": 123 }
```

Rust clients use the `auth` union (`static_token`, `device_token`, or
`pairing_code`). Static-token auth is valid over WebSocket; device and pairing
auth are iroh-only. A successful pairing sends `paired` before `hello_ok`.

The current Host snapshot is:

```text
hello_ok {
  latest_msg_id
  messages
  events
  processes
  side_chats
  host_version
  model
  subagent_models
  prompts
  providers
  views
}
```

`prompts.agent` always carries the effective editable body and an `is_default`
flag. The Host appends its own runtime-configuration section after that body;
the generated section is never editable or returned in `prompts.agent.text`.
An accepted `set_agent_prompt` updates the live Lash session policy before the
operation returns and therefore applies from the next turn, never midway
through a running turn. Empty or whitespace-only text removes the override.

`prompts.fork`, when the active provider has a selectable registry, carries the
ephemeral incoming-event triage fork's model, available models, and effective
prompt. `set_fork_model` is rejected unless both the model and variant belong
to the active provider's registry. `set_fork_prompt` follows the same empty-is-
default rule. The configuration is persisted now; the fork runtime consumes it
in a follow-up. Any accepted prompt or fork edit broadcasts the full
`prompts_changed { prompts }` replacement snapshot.

`model` and `prompts.fork` each carry two further fields describing the provider
the agent runs on: `provider_id` (the instance id, absent on older hosts) and
`free_text_model` (true when that provider takes any model id it recognises, in
which case `available` is empty and `current.id` is whatever the Owner typed).

Both snapshots are derived from the agent's **stored** provider choice, not from
the provider the Host booted on, and both are served fresh on every `hello_ok`.
Because a provider move reshapes the model control itself — a curated registry
with a reasoning ladder is a different question from one free-text id —
`model_changed { model }` carries the whole `ModelSnapshot` as a replacement,
exactly as `prompts_changed` carries the whole prompt surface. It is emitted for
an accepted `set_model` and for an accepted `set_agent_provider` on the main
slot, and only when the snapshot actually changed.

When the stored choice is not the booted provider, an accepted `set_model` is
persisted and broadcast but never applied to the running session: that session's
provider handle was built at boot, so the new selection takes effect at the next
Host restart. A client can tell the two apart by comparing `model.provider_id`
with `providers.booted_provider_id`.

The persisted keys are `[agent].prompt` and `[fork].model`, `[fork].variant`,
`[fork].prompt` in `data/hirsel.toml`. The store re-reads the file before every
snapshot and before each Agent turn, so hand edits are live without a restart.

The browser treats `events`, `processes`, and `views` as authoritative snapshot
slices. Live updates then arrive as full-record upserts. Unknown or rejected
requests return `error { detail, client_id? }`.

`last_seen_msg_id` is an **attention cursor, not a history gate**. Every
`hello_ok` carries at least the newest 200 conversation rows, whatever the
client presents; the cursor only *widens* that replay, for a client further
behind than the window. A null cursor therefore means the same thing it always
did — exactly the window.

This is what makes a reload show the conversation. A caught-up client used to
present its stored cursor and receive nothing, so a refreshed browser rendered
an empty margin until the next message arrived; `last_seen` governs unseen and
attention semantics only. Re-sending rows the client already holds is safe
because the client merge is range-authoritative: the snapshot owns every id
from its lowest upward (so a full resync drops residue), and local history
below that range is preserved (so a windowed replay never truncates what the
client still has).

History before that replay window is paged just in time:

```json
{
  "type": "fetch_messages",
  "client_id": "<uuid>",
  "before_id": 401,
  "limit": 100
}
```

The Host clamps `limit` to `1..=100`, selects rows with `id < before_id`, and
returns them oldest-to-newest within the page:

```json
{
  "type": "messages",
  "client_id": "<same uuid>",
  "before_id": 401,
  "messages": [
    {
      "id": 400,
      "author": "agent",
      "body": "Earlier message",
      "ref": null,
      "ts": "2026-08-20T10:00:00Z",
      "attachments": []
    }
  ],
  "has_more": true
}
```

`client_id` correlates the response or an `error`. `has_more: false` is the
authoritative beginning of stored history; the browser renders no terminal
marker there. The browser keeps only one page request in flight. Its bounded
600-row committed range normally retains the newest edge; while prepending at
the cap it instead evicts from the newest committed edge so the row being read
cannot disappear. That creates an intentional newer gap, recorded locally;
“Jump to latest” reloads the newest Host page before pinning to the bottom.

The live turn timeline is still never replayed. A reconnect mid-turn therefore
shows committed rows only, and any half-streamed reply is dropped rather than
left orphaned on screen.

The local dev mock accepts any non-empty static token unless `MOCK_TOKEN` is
set to exercise exact-token rejection. The Rust Host still compares against
its configured `HIRSEL_TOKEN`. The protocol never puts bearer tokens in blob
URLs or query strings.

## Provider roster

`hello_ok.providers` carries the configured instances a resident agent can run
on, and any accepted roster edit broadcasts the whole thing back:

```json
{
  "type": "providers_changed",
  "roster": {
    "instances": [
      {
        "id": "codex",
        "kind": "codex",
        "label": "Codex",
        "default_model": "gpt-5.6-sol",
        "detection": {
          "detected": true,
          "path": "/home/owner/.codex/auth.json",
          "account_hint": "owner@example.com"
        },
        "agent_selectable": true,
        "selection": {
          "mode": "curated",
          "main": [
            {
              "id": "gpt-5.6-sol",
              "label": "GPT-5.6 Sol",
              "variants": ["low", "medium", "high", "xhigh", "max"],
              "default_variant": "medium"
            }
          ],
          "fork": [
            {
              "id": "gpt-5.6-luna",
              "label": "GPT-5.6 Luna",
              "variants": ["low", "medium", "high", "xhigh", "max"],
              "default_variant": "max"
            }
          ]
        },
        "removable": false
      },
      {
        "id": "openrouter",
        "kind": "openai_compatible",
        "label": "OpenRouter",
        "base_url": "https://openrouter.ai/api/v1",
        "api_key": { "present": true, "tail": "9f2c" },
        "default_model": "z-ai/glm-5",
        "agent_selectable": true,
        "selection": { "mode": "free_text" },
        "removable": true
      }
    ],
    "booted_provider_id": "codex",
    "boot_notice": "configured provider \"acme\" is unavailable at boot: no API key is stored — running on Codex"
  }
}
```

`kind` is `codex`, `claude`, or `openai_compatible`. The first two are local
OAuth credentials the Host either can see or cannot, described by `detection`
and re-probed on demand; there is no login flow on the wire, because the Owner
logs in with that CLI on the Host machine. Every other instance is an
OpenAI-compatible endpoint the Owner configures.

`agent_selectable` is the Host's answer to "may a resident agent run on this?".
Claude is `false` (ADR-0015: the Claude driver is a Sub-agent lane only), so it
never appears in the main-agent or fork provider select. `removable` is `false`
for the two built-ins.

`selection` lets Settings reshape model controls from the locally selected
provider without waiting for a Host round trip. `curated` carries separate
`main` and `fork` registries because Codex deliberately offers Sol only to the
main Agent and Luna plus Sol to the fork. `free_text` means the provider has an
open model-id namespace and offers no host-owned Reasoning select. The field is
absent on older hosts and on Claude, which resident agents cannot select.

`booted_provider_id` is the provider the resident main-agent session actually
booted on: the stored `[model].provider`, when the Host could honour it, and
the environment default otherwise. A main-agent provider change is stored
immediately, but the running session stays where it is until the Host restarts
— the client says so rather than implying a live switch.

`boot_notice` is present only when a stored provider choice could not boot (no
key stored, an unknown id, a Sub-agents-only provider, a missing Codex login)
and the Host fell back to its environment default. It names the instance and
the reason, carries no key material, and stands until the next restart; the
client shows it as a quiet standing line on the Providers surface, because the
Host is degraded but running.

**Masked secrets.** A stored API key NEVER leaves the Host. The wire describes
it as `{ "present": bool, "tail": string }`, where `tail` is at most the last
four characters and is empty when the key is absent or too short to reveal any
tail safely. The browser therefore never renders, stores, or logs key material:
the only key bytes client-side are the transient contents of a password field
during one edit.

The five roster ops are:

```text
set_agent_provider { agent: "main" | "fork", provider_id }
add_provider       { id, label, base_url, api_key, default_model }
update_provider    { id, label?, base_url?, api_key?, default_model? }
remove_provider    { id }
redetect_provider  { id }
```

`set_agent_provider` seeds that provider's default model and variant for the
named slot. `update_provider` is a patch: an omitted field is unchanged, and an
`api_key` of `""` clears the stored key — so a save that leaves the key field
empty omits `api_key` entirely. Instance ids match
`^[a-z0-9][a-z0-9_-]{0,31}$`; `codex` and `claude` are reserved. Rejections come
back as `error { detail }` and broadcast nothing.

Model writes name the provider whose controls produced them:

```text
set_model      { provider_id, model_id, variant }
set_fork_model { provider_id, model_id, variant }
```

The Host serializes these with provider changes and rejects a frame whose
`provider_id` is no longer the configured provider for that slot. It then
validates Codex ids and variants against the slot's curated registry, while an
OpenAI-compatible provider accepts any non-empty, whitespace-trimmed model id.

**Older-client and older-host tolerance.** `providers` is optional on
`hello_ok`, `provider_id` / `free_text_model` are optional on the model and
fork snapshots, and `selection` is optional on each provider instance. A host
without them leaves the client with no roster (the
Providers tab says so, and the agent provider controls do not render), and a
client without them ignores the extra fields. A host that sends
`providers_changed` to a client that does not know the frame is likewise
harmless — unknown frames are dropped.

## Attachments and blob access

The client uploads bytes with `upload_blob`, then references returned Blob ids
from `send_message.attachments`. Blob content is fetched outside the message
stream:

1. send `get_blob_url { client_id, blob_id }`;
2. receive a short-lived, blob-scoped `blob_url`; and
3. fetch that Host-relative signed URL before expiry.

The raw owner token is never a blob query parameter. Image attachments may be
fed to a vision turn; all stored attachments are also described to the Agent by
their Host path.

## Plugin tier

A plugin may push to its own browser-side UI over the same socket:

```json
{ "type": "plugin_push", "plugin": "github", "topic": "tick", "data": {} }
```

`data` is opaque: the client never interprets it and never stores it as app
state. The frame is routed to the handlers that plugin registered for `topic`
(`api.onPush`), and dropped when nobody is listening.

Everything else in the plugin tier is HTTP on the Host's origin, authenticated
with the same owner token as a `Bearer` header. The Host's own surface is `GET
/api/plugins` (the roster: state, settings descriptors, values), `POST
/api/plugins/<id>/enabled`, and `POST /api/plugins/<id>/settings`, all behind
Settings → Plugins. Each plugin additionally mounts its own router under
`/api/plugins/<id>/…`, which only that plugin's UI calls.

Plugin UI is not served over the protocol at all: it lives in the repo at
`plugins/<id>/ui/index.tsx` and is compiled into the app. Installing a plugin
means adding its folder and rebuilding. The client initialises a compiled-in UI
only when the roster reports that plugin is not `disabled`.

## Current frame index

The Rust `ClientToHost` union currently accepts:

```text
hello, send_message, cancel_turn, cancel_queued,
set_model, set_subagent_model,
set_agent_prompt, set_fork_prompt, set_fork_model,
set_agent_provider, add_provider, update_provider,
remove_provider, redetect_provider,
upload_blob, get_blob_url,
event_action, clear_finished_events,
register_push_token, unregister_push_token,
view_event,
resolve_ping, reopen_ping, read_ping,
open_side_chat, conclude_side_chat, confirm_conclusion, discard_side_chat
```

The Rust `HostToClient` union currently emits:

```text
paired, hello_ok, msg, msg_removed,
agent_activity, turn_event,
event_upsert, process_upsert,
model_changed, subagent_models_changed,
prompts_changed, providers_changed,
blob_ok, blob_url, error,
view_upsert, view_removed,
plugin_push,
side_chat_open, conclusion_draft, side_chat_closed
```

The final two groups of `ping` and `side_chat` frames are retained compatibility
surface. Side-session execution is disabled by default and is available only
with `HIRSEL_COMPAT_SIDE_SESSIONS=1`; even then its session and TTL reaper start
only on the first legacy open frame. Their historical semantics,
auto-resolution rules, and wire evolution are intentionally outside the
current product model; see the
[`compatibility appendix`](PROTOCOL-COMPATIBILITY.md).

## Change discipline

A protocol change is complete only when all affected surfaces agree:

1. update `hirsel-proto`;
2. update the Host ingress/broadcast/storage behavior;
3. update the browser subset, reducer, and renderer if the web app consumes it;
4. update this document;
5. add protocol-unit coverage and the appropriate Task-surface or
   [protocol-compatibility runbook](../e2e/protocol-compatibility/README.md).
