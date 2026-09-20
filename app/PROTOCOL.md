# Hirsel client protocol

WebSocket JSON text frames connect an authenticated Owner to one Host. The current contract is mirrored by `src/protocol.ts`, `src/threads/types.ts`, `src/artifacts/types.ts`, and `hirsel-proto`. Clients implement this contract directly; unsupported frames fail explicitly.

## Authentication and reconnect

Send `{"type":"hello","auth":{"static_token":"…"}}`. Native clients can instead send `auth:{device_token:string}` or the Owner-minted `auth:{pairing_code:string}`. A pairing label belongs to that one-time code and is never supplied by the redeeming client. Never place bearer credentials in URLs.

The required `hello_ok` snapshot contains `history_id`, `threads`, `processes`, `host_version`, `views`, `model`, `subagent_models`, `prompts`, and `providers`. Arrays are present even when empty. Model/provider/prompt snapshots are explicitly null when unavailable for the configured runtime. `history_id` is a stable UUID for the current history store.

After hello, fetch the focused conversation with `open_thread`. A reconnect to the same history can retry unacknowledged Thread messages using their original `client_id`. A different history ID discards conversation/artifact caches, pending requests/uploads, queued actions and old focus before any replay. Plain unsent draft text remains available for explicit recovery. Drafts are keyed by history and Thread; text never silently follows a reused numeric ID into a different history.

## Spaces, Tasks and messages

A Thread is explicitly either `kind:"space"` or `kind:"task"`. Both kinds may be roots. Spaces may contain Spaces and Tasks, while Tasks may contain Tasks only. A Thread also has an ID, title, description, mutable constrained instrument, revision, timestamps, independent attention/read/visibility, running turn, queue count, latest finished turn and factual activity recency. Its nullable typed icon is `{"kind":"symbol","name":"…","tint":"…"}` or `{"kind":"image","blob_id":"…"}`; `null` selects the title monogram. `name` is one of 45 curated symbol names and `tint` one of `neutral|red|orange|amber|green|teal|blue|violet|pink` (defaulting to `neutral` when omitted); anything else is refused. Tasks alone can be settled; completing execution or reading a Task never settles it. Required `parent_thread_id:number|null` is immutable; `pinned_at:string|null` gives independent pin state and stable ordering. All IDs are ordinary, including retained 0. An empty Hello has no Threads.

| Client frame | Fields and result |
| --- | --- |
| `create_thread` | `client_id,history_id,title,kind:"space"|"task",parent_thread_id:number|null` → `thread_created {client_id,thread}` |
| `open_thread` | `client_id,thread_id,before_id:number|null` → `thread_opened {client_id,detail}` |
| `send_thread_message` | `client_id,history_id,thread_id,body,attachments:string[],mentions:number[],artifact_ids:number[],mode:"send"|"next_turn"` → owning `msg` and turn updates |
| `thread_action` | `client_id,history_id,thread_id,action,data,expected_revision?` → `thread_action_applied {client_id,history_id,thread_id}`; displayed instrument controls require their revision |
| `cancel_turn` | required `history_id,thread_id` |
| `cancel_thread_turn` | Owner-only `client_id,history_id,thread_id,turn_id,expected_state` → `thread_turn_cancellation_applied {client_id,history_id,thread_id,turn_id}` |
| `cancel_queued` | accepted outgoing `client_id` |

`thread_action` uses `action:"set_icon"`, required `expected_revision`, and `data:{icon}` for an Owner icon edit. Image icons must reference an uploaded PNG, JPEG or WebP blob of at most 256 KiB, with real bytes matching the MIME and square dimensions no larger than 256 px; SVG is rejected. The web picker center-crops and encodes uploads before sending the action. `action:"set_kind"` uses the same revision rule and `data:{kind}` for Owner conversion. A settled Task must reopen before conversion. Space to Task is invalid while it has a Space child, and Task to Space is invalid below a Task. Same-kind requests are revision-validated no-ops. `settle` and `reopen` apply only to Tasks. Generated instrument controls whose `settles` field is true or omitted are completion controls and are hidden on Spaces; controls with `settles:false` remain available.

`action:"set_title"` (`data:{title}`) and `action:"set_description"` (`data:{description}`) are the Owner's Info-pane text edits. Both require `expected_revision`, bump the revision, and broadcast the updated Thread. A title is trimmed and must be non-empty and at most 200 characters; a description may be empty and is at most 20,000 characters. The same bounds apply to the Agent's own `threads.update`. Neither edit marks the Thread unread or touches `last_activity_at`.

`action:"set_execution"` (`data:{execution}`) is the Owner's choice of backend for a Thread. `execution` is `null` to inherit the configured default Native provider and model, or one of `{kind:"native",provider_id,model}`, `{kind:"cli",agent,model,variant}`; unknown keys are rejected. It requires `expected_revision` and is validated against the live model catalog and provider roster by exactly the code that validates `threads.delegate`, so both refuse the same agents, models and variants. A `native` target names the provider and model this Thread's own session runs on: `provider_id` is any agent-selectable instance in the provider roster (`claude` is Sub-agents only and refused), and `model` is one of that instance's curated models, or free text where the instance takes free text. The Settings Native route remains the default for every Thread that names none. The choice takes effect on the **next** turn: a turn already running keeps the backend it captured when it started. `Thread.execution` mirrors the stored choice on every listing and broadcast, and is absent on older hosts.

Thread detail carries required `brief:{text:string,artifact_ids:number[]}`, its Thread, a bounded message page, turns, activities and `has_more`. Every `ChatMessage` requires `thread_id,id,author,body,ref,ts`. Each tool summary requires the canonical call `id`, `name`, and `ok`; live and durable tool data join only by that ID. Optional client correlation, attachments, tool summaries, mentions and artifact references carry their current meaning. `ref` and `mentions` are citations, never message ownership. `msg_removed {id}` is authoritative even if its echo arrives later.

Thread detail also carries required `effects`, the durable effects for the
turns represented by that bounded page. A receipt is
`{id,turn_id,operation_id,effect_index,tool,effect,target,target_turn_id,request_client_id,refusal,created_at}`.
`effect` is `created | sent_to | delegated | read | edited | refused`; `target`
is exactly `{kind:"thread",thread_id}`, `{kind:"artifact",artifact_id}` or
`{kind:"root"}`. `refusal` is null except for a refused effect, where it is
`{reason,grant_summary,detail}`. One operation may deliberately emit multiple
indexed effects, while replay of that operation preserves their receipt
identities.

Each receipt is projected as `{receipt,actions}`. Actions are only currently
true Host facts: `{kind:"open",target}`, `{kind:"archive",thread_id}`,
`{kind:"cancel_queued",thread_id,turn_id}` or
`{kind:"stop",thread_id,turn_id}`. They may change when a target starts,
finishes or is archived; they are not permanent receipt fields.
`thread_effects_changed {history_id,thread_id,turn_id,effects}` replaces the
complete current projection for that source turn, including an empty list.
The exact-turn cancellation operation is revision-like: the Host checks the
named turn still has `expected_state`, so a queued/running race cannot cancel
different work.

## Execution and factual activity

`thread_turn {turn}` carries required `id,thread_id,owner_message_id,agent_message_id,requester_thread_id,requester_turn_id,state,started_at,finished_at`. Nullable message IDs represent background/no-final execution. Requester IDs identify actual delegation; direct human input in a child has its parent requester with null requester turn. Root input has both null. States are queued, running, completed, failed, cancelled and interrupted.

`turn_event {thread_id,turn_id,seq,event}` and `agent_activity {thread_id,turn_id,state,text}` always name an actual turn. `event` is prose, reasoning, tool_start/tool_done or code_start/code_done with the exact fields in `src/protocol.ts`. Sequence ordering is per turn. Delayed frames cannot revive an earlier completed turn.

`thread_activity {activity}` carries `id,thread_id,turn_id:number|null,kind,data,artifact_ids:number[],ts`. Kind/data are extensible current diagnostics. Plugin kinds have a deliberate `{plugin,label,payload}` envelope. Info/summary records use description and content_md; process_completed uses summary. Unknown diagnostics remain visible as structured data.

The final assistant message joins its execution disclosure using the exact `agent_message_id`; artifact publication messages do not duplicate it. Unfinished/no-final execution joins its exact Owner message or, for a background turn, appears at its real start time. Page bounds exclude unloaded historical details. Timestamps order independent facts and never guess ownership.

## Artifacts, Views and operations

Artifacts are explicitly published global results without owners or revisions. `kind` is the single render discriminator and travels flat in the summary: `solid`, `html`, `markdown`, `openui`, `image` (with `mime`) or `file` (with `mime` and optional `filename`). Only those two variants carry a MIME type and only `file` carries a filename; no other field decides how a result is drawn. The publishing tool maps its ergonomic inputs onto a kind once, at the tool boundary, so a Markdown or image MIME/filename published as a file becomes that kind at rest. Cards resolve current content through `artifact_upsert`, `list_artifacts`/`artifacts_listed`, and `open_artifact`/`artifact_opened`, correlated by client ID. Each kind has exactly one surface — compiled Solid, HTML document, Markdown document, natively drawn OpenUI Lang, image, preformatted text — and the openers a card offers (Preview, Source, Download, Showcase) are derived from the kind alone. Rendered/Source is a view state; it never rewrites the artifact. Source downloads preserve original bytes. Preview documents are isolated with no network or backend bridge. `openui` is the one kind with no preview document at all: its body is parsed, never executed, and drawn in the host page, so its controls address the Thread through the ordinary message path instead of a frame bridge.

Thread turns carry immutable `accepted_at:string` separately from `started_at:string|null`. Queued turns have no start; admission to execution sets it once. A turn cancelled while queued retains a null start and has a terminal `finished_at`; clients use acceptance for its chronology and show no execution duration. Direct execution records both timestamps. Running turns always have a start.

`Thread.instrument` is `null` or a validated nonempty component object or nonempty component array. Arrays remain supported. Empty `{}` and `[]` are invalid; absence is SQL NULL, projected as JSON `null`. Agent `threads_update` omits `instrument` to preserve it and sends `instrument:null` to clear it. Instrument controls remain revision-fenced.

Current host-authored Canvas Views retain `view_upsert {instance_id,thread_id,spec}`, `view_removed {instance_id}`, and `view_event {instance_id,action,data}`. Thread instruments have their own constrained JSON controls and revision validation.

ProcessInfo requires `thread_id,id,name,trigger_recurring,cancellable,state,started_ts,last_event_ts` and carries optional `active_process_id`, nullable trigger subscription metadata, `last_fired_ts`, and `last_outcome`. Its `id` is a stable identity for one process name within its owning Thread, not an execution ID. Subscriptions and all runs of that name fold into one row; `process_upsert {process}` replaces it and `process_removed {thread_id,id}` removes a row absent from the authoritative projection. `hello_ok` replaces the full list on reconnect.

An in-flight incarnation is `running` (including a suspended Lash process). With no active incarnation, the row is `waiting` only if an enabled subscription can fire again; otherwise it shows the latest execution's terminal state. Disabled, never-fired subscriptions are `cancelled`. A consumed `in_secs` or `at` one-shot is tombstoned through Lash Delete, preserving its delivery history and label. Recurring schedules remain waiting between executions. `last_fired_ts` is the latest durable trigger occurrence time; `last_outcome` remains the most recently completed terminal result while a later incarnation runs. If same-name runs overlap, `active_process_id` selects the newest active run by creation time, then ID; Cancel targets that incarnation. Row start time is the earliest retained registration/run.

A registered trigger is visible before its first run with `cancellable=false`. Cancel is offered only with state `running` and `active_process_id`; Disable only with a live recurring subscription (`trigger_recurring`, `trigger_enabled`, key and revision). When several recurring registrations share a name, Disable selects the most recently updated enabled registration, then ID; subsequent updates expose the next remaining registration. `cancel_process` and `disable_process_trigger` are history- and Thread-addressed writes acknowledged by `process_action_applied {client_id}`; the Host validates the active process belongs to that Thread and fences recurring-trigger disable by subscription revision.

Views are Canvas-only and the conversation Canvas filters them by selected Thread. Settings use set_model, set_subagent_model, set_native_worker, set_agent_prompt, set_fork_prompt, set_fork_model, set_agent_provider, add_provider, update_provider, remove_provider and redetect_provider. The sub-agent catalog carries a `native_worker` row alongside its CLI providers; `set_native_worker` updates it and a provider edit that changes which instances can host it republishes the catalog too. Authoritative model_changed, subagent_models_changed, prompts_changed and providers_changed snapshots acknowledge updates. Provider capability nullability is distinct from compatibility support.

`upload_blob {client_id,name,mime,data_b64}` returns `blob_ok {client_id,blob}`. The general attachment limit remains 15 MiB; choosing a blob as a Thread icon applies the stricter raster contract above. `get_blob_url {client_id,blob_id}` returns `blob_url {client_id,blob_id,url,expires_at}` with a short-lived signed relative URL. Upload and retrieval requests are bounded and fail visibly. Authenticated plugin HTTP and `plugin_push {plugin,topic,data}` remain current operational contracts. `error {detail,client_id?}` echoes the request ID for action and other request failures; errors without a client ID remain global. Action clients retain the captured history and Thread until the matching success, error or timeout, and discard late or duplicate results after settlement or reset. A pre-auth failure returns to authentication.

## Nested coordination

Human Hello contains the full flat forest; agents receive a separate trusted caller-scoped host tool interface. Human create requires an explicit nullable parent; missing is invalid. `pin`/`unpin` are human Thread actions with empty data and the current revision. They do not change meaningful activity, read state, attention or settlement. Parentage cannot be moved in this cutover.

`delegation_received` activity lives in the child and has data `{requester_thread_id,requester_turn_id,brief}` with its child turn ID. `child_report` lives in the parent with local `turn_id:null` and data `{child_thread_id,child_turn_id,requester_turn_id,report_seq,status,summary}`. Status is progress/completed/failed/cancelled/interrupted. Artifact references are solely the activity's required `artifact_ids`, derived from normalized links. They appear once at the activity's chronological position; source child turns never join a parent inspector. Queued/running status comes from the actual child's Thread summary.

Selection uses an explicit route first. An unavailable explicit route remains unavailable rather than selecting another Thread. Route-free `/` restores the active top-level Space in `hirsel.last-project.<history_id>` or sends `ensure_home_project {client_id,history_id}` and opens the correlated ordinary Home Space returned as `thread_created`; Home receives no automatic root grant. `/t/0?history={uuid}` remains an ordinary route. Portable links require their history UUID; unqualified, malformed, wrong-history and missing destinations have no recipient. The incoming URL survives a history reset until validated by the current hello. Cached IDs do not authorize navigation or sending before that handshake. Native push destinations carry history and Thread identity and reject obsolete histories.

The composer labels `projectRecipientId`, `taskFocus` and `workerPairingId` independently; `focusedId` is navigation only. `TaskFocus {task_thread_id,snapshot}` is optional on `send_thread_message` and `ChatMessage`. The Host accepts only an object snapshot of at most 16 KiB for a reachable Task, stores it in `message_task_focus` with the Owner message, and preserves it on client-ID retry. Focus never changes reach. Pending sends and retries retain the exact snapshot.

Agent tools are fenced, not scoped away: every tool accepts any numeric Thread or artifact ID as well as caller-relative child paths, and a target outside the caller's reach returns a typed refusal result rather than an error (see Reach and grants). `threads_create` and `threads_update` accept typed symbol icons (a vocabulary name plus an optional tint) plus image `blob_id` or accessible base64 file `artifact_id` sources; the Host applies the same raster validation and stores a center-cropped 256 px WebP, with a bounded JPEG fallback for high-entropy images. Dispatch and messaging reach anything inside the grant, but never the caller's own ancestors; reports have a captured upward route. Human UI remains full-tree. Explicit message/activity/brief artifact references authorize current-content access and edits without disclosing other conversations. No client filtering substitutes for host enforcement.

## Human artifact references

Opening an existing artifact while an actual Thread is selected stages one visible, removable About context in that history/Thread draft. A new preview replaces it. Closing/returning from the preview and switching views preserve it; switching Threads does not transfer it. The overview stages no addressed context. Use in message explicitly re-stages the visible result after removal or choosing a Thread. Preview and refresh are read-only; no server reference grant happens during browsing.

`send_thread_message.artifact_ids` is required, including `[]`. The client snapshots it separately from blobs and Thread mentions, canonicalizes/deduplicates the bounded list (maximum16 distinct IDs), and retains exactly that set through pending failure, retry and reconnect. Sending clears the submitted draft context; history reset discards reference IDs while retaining recoverable plain text. Opening another preview during a file upload does not change the accepted submission snapshot.

Human acceptance validates existing IDs using human-global authority and atomically inserts message_artifacts links with the Owner message and queued turn/request. Invalid IDs reject the entire acceptance; replay cannot widen references under the same client_id. Both host and CLI accepted input identify these exact current-message references. Existing ArtifactCards render accepted Owner references. This grants scoped access to that artifact content without granting another Thread's conversation or adding artifact ownership, revisions or a JavaScript host bridge.

## Related web links

`ThreadDetail.related_items` is a required complete list of `{id, thread_id, target, title, created_at}`, independent of message pagination. `target` is `{kind:"url", url}` or `{kind:"thread", history_id, thread_id}`. Titles are optional for URLs; Thread references resolve current titles from the authoritative inventory. Human `add_thread_related {client_id, history_id, thread_id, target, title}` and `remove_thread_related {client_id, history_id, thread_id, item_id}` explicitly edit associations without messages, artifacts or execution. HTTP(S) URLs are canonicalized, credentials/control characters rejected, and exact canonical duplicates within one Thread preserve their original row/title. Query and fragment remain part of URL identity. Thread references never reparent or grant access.

`thread_related_changed {client_id: string|null, history_id, thread_id, revision, items}` carries the complete current list. Durable human mutation receipts acknowledge retries with the current list, so replaying an old add after removal cannot resurrect an item. Clients reject a different history and revisions older than the last applied **Related snapshot**, independently of newer unrelated Thread metadata revisions. Equal revisions are accepted. Read responses are correlated to their original history/Thread so delayed pre-reset results cannot populate reused IDs. Reconnect/open reloads this snapshot.

## Reach and grants

A Thread's reach is itself and its descendants, widened by explicit grants. `ThreadDetail.grants` is a required complete list of `{thread_id, target, granted_by, granted_at, note}`; `target` is `{kind:"thread", thread_id, title}` or `{kind:"root"}`, and `granted_by` is `{kind:"owner"}` or `{kind:"thread", thread_id}`. A Thread grant makes the named Thread **and its whole subtree** addressable, exactly like the default subtree; a root grant makes every Thread in the history addressable, including Threads created after the grant, and a Thread holds at most one. Reach is one-way, a grant restating the default reach is rejected rather than stored, and reach never reparents a Thread or changes human visibility.

Human `grant_thread_reach {client_id, history_id, thread_id, target, note}` and `revoke_thread_reach {client_id, history_id, thread_id, target}` are the Owner's edits, where `target` is a Thread ID or the literal `"root"`. Agents use the `threads.grant`/`threads.revoke` tools, which name the target the same way (plus caller-relative paths) and are themselves fenced: only a strict ancestor may widen a Thread, only with reach it already holds — root included — and never itself. Narrowing needs no reach of its own, so an ancestor may remove a grant the Owner made.

`thread_grants_changed {client_id: string|null, history_id, thread_id, revision, grants}` carries the complete current list. Durable mutation receipts acknowledge retries with the current list. Clients reject a different history and revisions older than the last applied **reach snapshot**, independently of newer unrelated Thread metadata revisions; equal revisions are accepted. Reconnect/open reloads this snapshot from `thread_opened`.

Naming an unreachable Thread or artifact is never an error and never a lie. The tool result is `{refused: true, reason, target, tool, grant_summary, detail}`, where `reason` is `outside_grant` or `owner_fence` and `target` is `{kind:"thread", thread_id}`, `{kind:"artifact", artifact_id}` or `{kind:"root"}`. `grant_summary` reads `everything (root)` for a root holder. Each actual refusal probe atomically writes one durable `refusal` Thread activity and one refused effect receipt; replaying the same operation does not duplicate either, while two distinct probes remain two facts. Conversation keeps the activity note and the reply carries a refused effect pill. `owner_fence` covers the one fence an ordinary subtree grant cannot open: a Thread never addresses its own ancestors with work, it reports to its requester.

Copy thread link emits an absolute same-origin `/t/{id}?history={uuid}` HTTP(S) URL. Copy reference emits ordinary Markdown `[Thread #id](URL)`. Local `#id` shorthand resolves only in its message's current history. Conversation Markdown links, including reference-style links, use one native anchor renderer with adjacent Open, Copy and explicit Add to Related actions. Local Thread URLs resolve only at the app origin; lookalikes are external links. Related combines saved references with the canonical artifact inventory; artifact preview Markdown remains inert inside its isolated frame.

The Host uses one canonical storage schema 16: material Task state and artifact revisions, plus symbol/image Thread icons whose
`threads.icon_symbol` and `threads.icon_tint` are CHECK-constrained to the
vocabulary and palette and exclusive with the `threads.icon_blob_id` foreign key, Lash process delivery receipts, durable accepted-turn effect receipts and Thread
authority, durable `thread_grants` reach whose NULL `target_thread_id` is the
root, and no `monitors` table. Only the exact
layout or an empty store is accepted; older and branch-specific layouts are
refused without modification.

### Process conversation delivery

`ChatMessage.origin` is optional and omitted for ordinary conversation messages.
A delivery has `{kind:"process",process_id,name,trigger,subscription_key?,outcome,result,error?}`.
`outcome` is `completed | failed | cancelled | woke` (the last represents an explicit process wake).
`result` retains its JSON type. The separate debug-only subscription key is never a display label.
`trigger` is one of `{kind:"timer",label,in_secs?,every_secs?,at?}`, `{kind:"cron",expr,tz?}`,
`{kind:"thread",event,thread_id,title}`, or `{kind:"other",key}`. Thread events name
`thread.Report`, `thread.Complete`, `thread.Message`, or `thread.Turn`; titles are captured at delivery.
Trigger metadata comes from the registered source descriptor retained in the durable trigger delivery snapshot, including after a one-shot subscription is deleted, with a neutral label when unavailable.
The message body is a bare JSON string, scalar text, a fenced JSON object/array, or the failure error text.
Process notes retain `author:"agent"` for compatibility but are rendered as ConversationNotes.
The owning Thread receives one durable normal turn with this message as context, bypassing fork triage.
Native clients preserve the optional origin losslessly as `ChatMessage.originJson` in the generated Kotlin binding.
Schema 7's existing delivery receipt stores the origin JSON in `result`; the historical
`triage_dispatched` column now records durable normal-turn acceptance. No store schema is changed.
