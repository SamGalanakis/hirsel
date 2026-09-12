# Hirsel client protocol

WebSocket JSON text frames connect an authenticated Owner to one Host. The current contract is mirrored by `src/protocol.ts`, `src/threads/types.ts`, `src/artifacts/types.ts`, and `hirsel-proto`. Clients implement this contract directly; unsupported frames fail explicitly.

## Authentication and reconnect

Send `{"type":"hello","auth":{"static_token":"…"}}`. Native clients can instead send `auth:{device_token:string}` or `auth:{pairing_code:{code:string,device_label:string}}`. Never place bearer credentials in URLs.

The required `hello_ok` snapshot contains `history_id`, `threads`, `processes`, `host_version`, `views`, `model`, `subagent_models`, `prompts`, and `providers`. Arrays are present even when empty. Model/provider/prompt snapshots are explicitly null when unavailable for the configured runtime. `history_id` is a stable UUID for the current history store.

After hello, fetch the focused conversation with `open_thread`. A reconnect to the same history can retry unacknowledged Thread messages using their original `client_id`. A different history ID discards conversation/artifact caches, pending requests/uploads, queued actions and old focus before any replay. Plain unsent draft text remains available for explicit recovery. Drafts are keyed by history and Thread; text never silently follows a reused numeric ID into a different history.

## Spaces, Tasks and messages

A Thread is explicitly either `kind:"space"` or `kind:"task"`. Both kinds may be roots. Spaces may contain Spaces and Tasks, while Tasks may contain Tasks only. A Thread also has an ID, title, description, mutable constrained instrument, revision, timestamps, independent attention/read/visibility, running turn, queue count, latest finished turn and factual activity recency. Tasks alone can be settled; completing execution or reading a Task never settles it. Required `parent_thread_id:number|null` is immutable; `pinned_at:string|null` gives independent pin state and stable ordering. All IDs are ordinary, including retained 0. An empty Hello has no Threads.

| Client frame | Fields and result |
| --- | --- |
| `create_thread` | `client_id,history_id,title,kind:"space"|"task",parent_thread_id:number|null` → `thread_created {client_id,thread}` |
| `open_thread` | `client_id,thread_id,before_id:number|null` → `thread_opened {client_id,detail}` |
| `send_thread_message` | `client_id,history_id,thread_id,body,attachments:string[],mentions:number[],artifact_ids:number[],mode:"send"|"next_turn"` → owning `msg` and turn updates |
| `thread_action` | `client_id,history_id,thread_id,action,data,expected_revision?` → `thread_action_applied {client_id,history_id,thread_id}`; displayed instrument controls require their revision |
| `cancel_turn` | required `history_id,thread_id` |
| `cancel_queued` | accepted outgoing `client_id` |

`thread_action` uses `action:"set_kind"`, required `expected_revision`, and `data:{kind}` for Owner conversion. A settled Task must reopen before conversion. Space to Task is invalid while it has a Space child, and Task to Space is invalid below a Task. Same-kind requests are revision-validated no-ops. `settle` and `reopen` apply only to Tasks. Generated instrument controls whose `settles` field is true or omitted are completion controls and are hidden on Spaces; controls with `settles:false` remain available.

Thread detail carries required `brief:{text:string,artifact_ids:number[]}`, its Thread, a bounded message page, turns, activities and `has_more`. Every `ChatMessage` requires `thread_id,id,author,body,ref,ts`. Each tool summary requires the canonical call `id`, `name`, and `ok`; live and durable tool data join only by that ID. Optional client correlation, attachments, tool summaries, mentions and artifact references carry their current meaning. `ref` and `mentions` are citations, never message ownership. `msg_removed {id}` is authoritative even if its echo arrives later.

## Execution and factual activity

`thread_turn {turn}` carries required `id,thread_id,owner_message_id,agent_message_id,requester_thread_id,requester_turn_id,state,started_at,finished_at`. Nullable message IDs represent background/no-final execution. Requester IDs identify actual delegation; direct human input in a child has its parent requester with null requester turn. Root input has both null. States are queued, running, completed, failed, cancelled and interrupted.

`turn_event {thread_id,turn_id,seq,event}` and `agent_activity {thread_id,turn_id,state,text}` always name an actual turn. `event` is prose, reasoning, tool_start/tool_done or code_start/code_done with the exact fields in `src/protocol.ts`. Sequence ordering is per turn. Delayed frames cannot revive an earlier completed turn.

`thread_activity {activity}` carries `id,thread_id,turn_id:number|null,kind,data,artifact_ids:number[],ts`. Kind/data are extensible current diagnostics. Plugin kinds have a deliberate `{plugin,label,payload}` envelope. Info/summary records use description and content_md; process_completed uses summary. Unknown diagnostics remain visible as structured data.

The final assistant message joins its execution disclosure using the exact `agent_message_id`; artifact publication messages do not duplicate it. Unfinished/no-final execution joins its exact Owner message or, for a background turn, appears at its real start time. Page bounds exclude unloaded historical details. Timestamps order independent facts and never guess ownership.

## Artifacts, Views and operations

Artifacts are explicitly published global results without owners or revisions. Current kinds are solid, html and file. Cards resolve current content through `artifact_upsert`, `list_artifacts`/`artifacts_listed`, and `open_artifact`/`artifact_opened`, correlated by client ID. Only explicit Markdown MIME/filename file artifacts render Markdown; ordinary text remains preformatted. Source downloads preserve original bytes. Preview documents are isolated with no network or backend bridge.

Current host-authored Canvas Views retain `view_upsert {instance_id,thread_id,spec}`, `view_removed {instance_id}`, and `view_event {instance_id,action,data}`. Thread instruments have their own constrained JSON controls and revision validation.

ProcessInfo requires `thread_id`; processes use `process_upsert {process}`. Views are Canvas-only and the conversation Canvas filters them by selected Thread. Settings use set_model, set_subagent_model, set_native_worker, set_agent_prompt, set_fork_prompt, set_fork_model, set_agent_provider, add_provider, update_provider, remove_provider and redetect_provider. The sub-agent catalog carries a `native_worker` row alongside its CLI providers; `set_native_worker` updates it and a provider edit that changes which instances can host it republishes the catalog too. Authoritative model_changed, subagent_models_changed, prompts_changed and providers_changed snapshots acknowledge updates. Provider capability nullability is distinct from compatibility support.

`upload_blob {client_id,name,mime,data_b64}` returns `blob_ok {client_id,blob}`. `get_blob_url {client_id,blob_id}` returns `blob_url {client_id,blob_id,url,expires_at}` with a short-lived signed relative URL. Upload and retrieval requests are bounded and fail visibly. Authenticated plugin HTTP and `plugin_push {plugin,topic,data}` remain current operational contracts. `error {detail,client_id?}` echoes the request ID for action and other request failures; errors without a client ID remain global. Action clients retain the captured history and Thread until the matching success, error or timeout, and discard late or duplicate results after settlement or reset. A pre-auth failure returns to authentication.

## Nested coordination

Human Hello contains the full flat forest; agents receive a separate trusted caller-scoped host tool interface. Human create requires an explicit nullable parent; missing is invalid. `pin`/`unpin` are human Thread actions with empty data and the current revision. They do not change meaningful activity, read state, attention or settlement. Parentage cannot be moved in this cutover.

`delegation_received` activity lives in the child and has data `{requester_thread_id,requester_turn_id,brief}` with its child turn ID. `child_report` lives in the parent with local `turn_id:null` and data `{child_thread_id,child_turn_id,requester_turn_id,report_seq,status,summary}`. Status is progress/completed/failed/cancelled/interrupted. Artifact references are solely the activity's required `artifact_ids`, derived from normalized links. They appear once at the activity's chronological position; source child turns never join a parent inspector. Queued/running status comes from the actual child's Thread summary.

Selection uses an explicit route, then a valid saved selection from this history, otherwise no recipient. An unavailable explicit route remains unavailable rather than selecting another Thread. `/t/0?history={uuid}` is an ordinary route. Portable links require their history UUID; unqualified, malformed, wrong-history and missing destinations have no recipient. The incoming URL survives a history reset until validated by the current hello. Cached IDs do not authorize navigation or sending before that handshake. The overview `/` has no implicit composer; reset clears old focus before replay. Native push destinations carry history and Thread identity and reject obsolete histories.

Agent tools enforce self/subtree access after resolving numeric or caller-relative child paths. Direct dispatch is restricted to direct children; reports have a captured upward route. Human UI remains full-tree. Explicit message/activity/brief artifact references authorize current-content access and edits without disclosing other conversations. No client filtering substitutes for host enforcement.

## Human artifact references

Opening an existing artifact while an actual Thread is selected stages one visible, removable About context in that history/Thread draft. A new preview replaces it. Closing/returning from the preview and switching views preserve it; switching Threads does not transfer it. The overview stages no addressed context. Use in message explicitly re-stages the visible result after removal or choosing a Thread. Preview and refresh are read-only; no server reference grant happens during browsing.

`send_thread_message.artifact_ids` is required, including `[]`. The client snapshots it separately from blobs and Thread mentions, canonicalizes/deduplicates the bounded list (maximum16 distinct IDs), and retains exactly that set through pending failure, retry and reconnect. Sending clears the submitted draft context; history reset discards reference IDs while retaining recoverable plain text. Opening another preview during a file upload does not change the accepted submission snapshot.

Human acceptance validates existing IDs using human-global authority and atomically inserts message_artifacts links with the Owner message and queued turn/request. Invalid IDs reject the entire acceptance; replay cannot widen references under the same client_id. Both host and CLI accepted input identify these exact current-message references. Existing ArtifactCards render accepted Owner references. This grants scoped access to that artifact content without granting another Thread's conversation or adding artifact ownership, revisions or a JavaScript host bridge.

## Related web links

`ThreadDetail.related_items` is a required complete list of `{id, thread_id, target, title, created_at}`, independent of message pagination. `target` is `{kind:"url", url}` or `{kind:"thread", history_id, thread_id}`. Titles are optional for URLs; Thread references resolve current titles from the authoritative inventory. Human `add_thread_related {client_id, history_id, thread_id, target, title}` and `remove_thread_related {client_id, history_id, thread_id, item_id}` explicitly edit associations without messages, artifacts or execution. HTTP(S) URLs are canonicalized, credentials/control characters rejected, and exact canonical duplicates within one Thread preserve their original row/title. Query and fragment remain part of URL identity. Thread references never reparent or grant access.

`thread_related_changed {client_id: string|null, history_id, thread_id, revision, items}` carries the complete current list. Durable human mutation receipts acknowledge retries with the current list, so replaying an old add after removal cannot resurrect an item. Clients reject a different history and revisions older than the last applied **Related snapshot**, independently of newer unrelated Thread metadata revisions. Equal revisions are accepted. Read responses are correlated to their original history/Thread so delayed pre-reset results cannot populate reused IDs. Reconnect/open reloads this snapshot.

Copy thread link emits an absolute same-origin `/t/{id}?history={uuid}` HTTP(S) URL. Copy reference emits ordinary Markdown `[Thread #id](URL)`. Local `#id` shorthand resolves only in its message's current history. Conversation Markdown links, including reference-style links, use one native anchor renderer with adjacent Open, Copy and explicit Add to Related actions. Local Thread URLs resolve only at the app origin; lookalikes are external links. Related combines saved references with the canonical artifact inventory; artifact preview Markdown remains inert inside its isolated frame.
