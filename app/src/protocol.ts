// Hand-written mirror of ../PROTOCOL.md (canonical). If that file changes, this
// file and the Rust `hirsel-proto` crate both need to change to match.
//
// Transport: WebSocket, JSON text frames, one message per frame.

export type Author = "owner" | "agent";

/** A stored attachment (v1.1). CONTENT is fetched out-of-band from
 * `GET /blob/{id}?token=…`; this record is only the metadata carried on the
 * wire. */
export interface Blob {
  id: string; // uuid
  name: string;
  mime: string;
  size: number; // u64, decoded byte size
}

/** A single tool the Agent invoked during the turn that produced a committed
 * agent message (v1.4). Stamped from lash's per-turn RemoteToolCallSummary. */
export interface ToolCall {
  name: string;
  ok: boolean;
}

export interface ChatMessage {
  id: number; // u64, monotonic, host-assigned
  author: Author;
  body: string; // markdown
  ref: number | null; // id of the chat message this replies to
  ts: string; // RFC3339
  attachments?: Blob[]; // v1.1, default []
  /** v1.4: tools invoked in the turn that committed this (agent) message.
   * Optional on the wire; absent/empty renders no footer chip. */
  tool_calls?: ToolCall[];
}

/** A host-tracked background process the Agent has running (v1.4): a delegated
 * sub-agent, or a monitor probe. Surfaced in the Processes tab. */
export type ProcessKind = "subagent" | "monitor";

/** `running` is the only non-terminal state; the rest are terminal. `failed` /
 * `abandoned` get a warning tint in the UI. */
export type ProcessState = "running" | "done" | "failed" | "cancelled" | "abandoned";

export interface ProcessInfo {
  id: string;
  kind: ProcessKind;
  label: string;
  /** subagent kind only: the acting agent + model, shown as small chips. */
  agent: string | null;
  model: string | null;
  state: ProcessState;
  started_ts: string; // RFC3339
  last_event_ts: string; // RFC3339, drives newest-activity-first ordering
  summary: string | null; // latest progress line, single-line truncated in UI
}

export interface QuickReply {
  value: string;
  label: string;
}

/** A Ping has exactly two lifecycle states — `open` (needs the Owner's
 * attention) and `done` (dealt with, kept findable). The Owner replying to the
 * Ping's Anchor resolves it to `done` automatically, host-side. `archived` is
 * the pre-ADR-0009 persisted value for the same terminal state; the live wire
 * is open|done, but `isResolvedStatus` tolerates the legacy spelling for any
 * old persisted rows. No separate hard-delete state exists. */
export type PingStatus = "open" | "done";

/** v2.1 (ADR-0009 addendum): a Ping is a named, addressable async work item.
 * `name` is its short `@` handle (≤32 chars, mono in the UI) and `description`
 * is a one-line subtitle; both are required on the wire. `content` is the
 * markdown body. The Owner replying to `anchor` moves it open→done. */
export interface Ping {
  id: number;
  name: string; // short @-handle, <= 32 chars
  description: string; // one line
  content: string; // markdown
  anchor: number; // ChatMessage.id where pings.send was called
  requires_response: boolean;
  quick_replies: QuickReply[]; // may be empty
  status: PingStatus;
  ts: string;
  /** v1.3: Owner-side "seen" state, set automatically once a Ping has been
   * viewed (email-like). Optional on the wire; absent is treated as false. */
  read?: boolean;
}

/** Generative-UI tier (view templates): a resolved, concrete component tree the
 * host sends for the client to render natively. The client NEVER resolves
 * templates or bindings — `spec` is always the finished JSON tree of catalog
 * components (see templates/CATALOG.md). Typed loosely on the wire (a `type`
 * discriminator plus arbitrary resolved props/children); the renderer narrows
 * per catalog component and degrades gracefully on anything it doesn't know. */
export interface ViewSpec {
  type: string;
  [prop: string]: unknown;
}

/** Where a view is surfaced. `canvas` → the shared right context surface (a
 * full-screen sheet on phone); `chat` → an inline block in the transcript;
 * `ping:<id>` → inside the matching Ping card. A `view_event` from a `ping:<id>`
 * view becomes the anchor-refed owner reply that auto-resolves the Ping (the
 * host does that — the client just emits the event). */
export type ViewPlacement = "canvas" | "chat" | (string & {});

/** One active view instance. Keyed by `instance_id`; an update in place is just
 * a re-`view_upsert` of the same id. */
export interface ViewInstance {
  instance_id: string;
  placement: ViewPlacement;
  spec: ViewSpec;
}

// ---- Typed event queue (ADR-0012 / ADR-0013) ----
//
// `Event` (wire) generalizes `Ping`: a Ping is exactly a `judgment` Event. It
// is named `EventItem` here to avoid shadowing the DOM `Event` global (the wire
// tags stay `event_upsert` / `event_action`). The card body rides the wire as
// an OPAQUE JSON UI tree (`ui`) — the same pattern as `ViewInstance.spec` — and
// is validated/rendered client-side by the constrained catalog (ADR-0013), so a
// vocabulary skew degrades gracefully rather than breaking a card.

/** The interrupt-vs-accrue axis, baked into the type (ADR-0012). `judgment`
 * needs a decision (the hero — a Ping's `requires_response` maps here);
 * `summary` is a digest; `info` a quiet notification. Ordering (not a flag)
 * keeps info from drowning judgment. Open set on the wire — an unknown kind is
 * tolerated as awareness. */
export type EventKind = "judgment" | "summary" | "info";

/** Who produced an Event: the main Agent, a sub-agent at its own taste
 * boundary, a scheduled lash job (the "morning brief" is one), or a monitor.
 * `ref` is the producer handle (process_id / job_id / monitor label). */
export interface EventSource {
  kind: "agent" | "subagent" | "scheduled" | "monitor";
  ref: string | null;
}

/** Lifecycle mirrors a Ping (ADR-0009): `open` needs the Owner; `done` is
 * decided/dismissed and kept findable. A judgment resolves when decided;
 * summary/info auto-read on pass. */
export type EventStatus = "open" | "done";

/** One typed event in the home queue. `ui` is the constrained JSON-UI card body
 * (ADR-0013): the blessed judgment template (eyebrow · fork · optionList ·
 * viewSlot) for a judgment, a light tree for summary/info. Carried loosely
 * (opaque `ViewSpec`, or an array of nodes) so the renderer narrows per catalog
 * component and never trusts the shape. */
export interface EventItem {
  id: number;
  kind: EventKind;
  source: EventSource;
  /** Short `@`-handle (≤32 chars, mono in the UI). */
  name: string;
  /** One-line subtitle. */
  description: string;
  /** The constrained JSON-UI card body — a root node or an array of nodes. */
  ui: ViewSpec | ViewSpec[];
  /** True for a judgment; a judgment's options also live in `ui.optionList`. */
  requires_response: boolean;
  quick_replies: QuickReply[];
  status: EventStatus;
  read: boolean;
  anchor: number;
  ts: string;
  /** Host hint: this judgment stopped the fleet, so it leads the queue (ADR-0012
   * "blocking judgments first"). Optional on the wire; absent is treated as
   * false. Ordering is otherwise derived (kind + wait time). */
  blocking?: boolean;
}

// ---- Model configuration (main agent + sub-agent catalog) ----

/** The main agent's current model + reasoning variant. `variant` is one of the
 * chosen model's `variants` (e.g. "low" … "max"). */
export interface ModelSelection {
  id: string;
  variant: string;
}

/** A model the main agent may run as, plus its selectable reasoning variants and
 * the variant to fall back to when the model is chosen without one. */
export interface AvailableModel {
  id: string;
  label: string;
  variants: string[];
  default_variant: string;
}

/** The main-agent model snapshot carried on `hello_ok` and kept current by
 * `model_changed`: what's selected now, and everything selectable. */
export interface ModelSnapshot {
  current: ModelSelection;
  available: AvailableModel[];
}

/** One model in the sub-agent catalog: like an AvailableModel plus an `enabled`
 * flag (a disabled model can't be used to spawn a Sub-agent). `default_variant`
 * is the reasoning variant a spawned Sub-agent runs at. */
export interface SubagentModel {
  id: string;
  label: string;
  variants: string[];
  default_variant: string;
  enabled: boolean;
}

/** The sub-agent models offered by one provider (e.g. Codex CLI, Claude Code
 * CLI), grouped under its display `label`. */
export interface SubagentProviderModels {
  provider: string;
  label: string;
  models: SubagentModel[];
}

/** The full sub-agent model catalog carried on `hello_ok` and replaced wholesale
 * by `subagent_models_changed`. */
export interface SubagentModelCatalog {
  providers: SubagentProviderModels[];
}

// ---- Client -> server ----

export interface HelloMsg {
  type: "hello";
  token: string;
  last_seen_msg_id: number | null;
}

/** v1.2 send mode. "send" = plain Enter (Early Injection if a turn is active,
 * else normal ingress); "next_turn" = Tab (always held until the current turn
 * commits, lash Next Full Turn). Absent is treated as "send". */
export type SendMode = "send" | "next_turn";

export interface SendMessageMsg {
  type: "send_message";
  client_id: string; // client-generated idempotency key (uuid)
  body: string;
  ref: number | null;
  attachments?: string[]; // v1.1 blob ids, default []
  mode?: SendMode; // v1.2, default "send"
  /** v2.0: side chat scope. Absent = main conversation (byte-identical to
   * pre-v2.0 wire shape); present = routed to that side session. */
  sc?: string;
  /** v2.1 (ADR-0009 addendum): ping ids @-mentioned in the body. The host
   * validates every id and appends each Ping's context to the Agent turn.
   * Lifecycle-neutral — mentioning a Ping never resolves it. Default []/omitted. */
  mentions?: number[];
}

/** v2.1: resolve a Ping to `done` (⋯ "Mark done"). Idempotent; the host sets
 * status=done and broadcasts a ping_upsert. (Was `archive_item`.) */
export interface ResolvePingMsg {
  type: "resolve_ping";
  ping_id: number;
}

/** v2.2: reopen a resolved Ping (⋯ "Reopen", or the "Marked done" toast's
 * Undo). Idempotent; on success the host sets status=open and broadcasts the
 * same `ping_upsert` a resolve does — there is no bespoke inbound reply. The
 * recovery peer of `resolve_ping`, so a mis-tapped Done is never terminal. */
export interface ReopenPingMsg {
  type: "reopen_ping";
  ping_id: number;
}

/** v1.3: mark a Ping read (email-like "seen"). Idempotent; the host sets
 * read=true and broadcasts a ping_upsert. There is no "unread" op — that is a
 * client-only override (see store). (Was `read_item`.) */
export interface ReadPingMsg {
  type: "read_ping";
  ping_id: number;
}

/** v1.1: upload a file's bytes (base64) before referencing it from a
 * send_message. Correlated to a blob_ok by `client_id`. */
export interface UploadBlobMsg {
  type: "upload_blob";
  client_id: string;
  name: string;
  mime: string;
  data_b64: string;
}

/** D9: request a short-lived signed URL to fetch a blob's bytes. Replaces the
 * removed `GET /blob/{id}?token=<owner-token>` scheme (the host now rejects the
 * raw-token query param). Correlated to a `blob_url` by `client_id`. */
export interface GetBlobUrlMsg {
  type: "get_blob_url";
  client_id: string;
  blob_id: string;
}

/** v1.2: cooperatively interrupt the active agent turn (Esc). No-op if idle. */
export interface CancelTurnMsg {
  type: "cancel_turn";
  /** v2.0: side chat scope (see SendMessageMsg). */
  sc?: string;
}

/** v1.2: cancel a not-yet-claimed queued (next_turn) message. Host maps
 * `client_id` to its pending-input id; already-claimed → error. */
export interface CancelQueuedMsg {
  type: "cancel_queued";
  client_id: string;
}

/** v2.0 (ADR-0008): open (or resume) the side chat for a Ping. Idempotent per
 * Ping — if it already has a live side chat the host answers with the SAME sc
 * and its transcript so far; otherwise a fresh scope. */
export interface OpenSideChatMsg {
  type: "open_side_chat";
  client_id: string;
  ping_id: number;
}

/** v2.0: side agent drafts the Owner's reply (a real side turn). */
export interface ConcludeSideChatMsg {
  type: "conclude_side_chat";
  sc: string;
}

/** v2.0: the Owner's edited-or-not final text, confirming the conclusion. */
export interface ConfirmConclusionMsg {
  type: "confirm_conclusion";
  sc: string;
  text: string;
}

/** v2.0: end the side chat with no conclusion; the item stays open. */
export interface DiscardSideChatMsg {
  type: "discard_side_chat";
  sc: string;
}

/** Generative-UI tier: an owner-initiated event from an interactive view
 * component (`action` / `optionSet` / `form`). The host looks the instance up
 * and routes it through the normal owner-submission path — for a `ping:<id>`
 * view it becomes the anchor-refed reply that auto-resolves the Ping. The
 * client must NOT create Chat messages, resolve Pings, or open any side channel
 * directly; it only emits this frame. `data` is the component's declared payload
 * (`null` for a bare `action`, `{ value }` for `optionSet`, an object keyed by
 * field `name` for `form`). */
export interface ViewEventMsg {
  type: "view_event";
  instance_id: string;
  action: string;
  data: unknown;
}

/** Typed event queue (ADR-0012/0013): an owner-initiated action from an event
 * card's constrained UI. Generalizes the quick-reply resolution + `view_event`.
 * `action` is `choose` (data.choice = option key; resolves a judgment; optional
 * `data.record_rule` seeds the taste store), `submit` (data = field values),
 * `snooze`, `dismiss`, or `reopen` (Done-as-toggle recovery — the peer of a
 * choose, per the reopen machinery on main). The host looks the event up and
 * routes it through the normal owner-submission path; the client never resolves
 * the event itself, it only emits this frame. */
export interface EventActionMsg {
  type: "event_action";
  event_id: number;
  action: string;
  data: unknown;
}

/** Select the main agent's model + reasoning variant. The host applies it and
 * broadcasts the truth back as `model_changed`. */
export interface SetModelMsg {
  type: "set_model";
  model_id: string;
  variant: string;
}

/** Update one sub-agent catalog model's enabled state and/or default reasoning
 * variant. Carries the FULL row state (both fields) so it's a complete upsert,
 * not a diff; the host applies it and broadcasts `subagent_models_changed`. */
export interface SetSubagentModelMsg {
  type: "set_subagent_model";
  provider: string;
  model_id: string;
  enabled: boolean;
  default_variant: string;
}

export type ClientMessage =
  | HelloMsg
  | SendMessageMsg
  | ResolvePingMsg
  | ReopenPingMsg
  | ReadPingMsg
  | UploadBlobMsg
  | GetBlobUrlMsg
  | CancelTurnMsg
  | CancelQueuedMsg
  | OpenSideChatMsg
  | ConcludeSideChatMsg
  | ConfirmConclusionMsg
  | DiscardSideChatMsg
  | ViewEventMsg
  | EventActionMsg
  | SetModelMsg
  | SetSubagentModelMsg;

// ---- Server -> client ----

/** v2.0: a live side chat, as tracked by hello_ok for reconnect + resume. Just
 * the reference — no transcript; the client fetches that via `open_side_chat`
 * (idempotent) if/when the Owner resumes it. */
export interface SideChatRef {
  sc: string;
  ping_id: number;
}

export interface HelloOkMsg {
  type: "hello_ok";
  latest_msg_id: number;
  messages: ChatMessage[];
  pings: Ping[];
  /** Typed event queue (ADR-0012): the authoritative open + recent event set on
   * (re)connect, generalizing `pings`. Optional on the wire (serde-default []);
   * absent is treated as []. Pings continue to arrive on `pings` until the host
   * cutover; the client keeps both slices. */
  events?: EventItem[];
  /** v1.4: all non-terminal processes + the last 10 terminal ones. Optional on
   * the wire; absent is treated as []. */
  processes?: ProcessInfo[];
  /** v2.0: live side chats surviving reconnect. Optional on the wire; absent
   * is treated as []. */
  side_chats?: SideChatRef[];
  /** Generative-UI tier: the authoritative active view set on (re)connect.
   * Optional on the wire (serde-default []); absent is treated as []. */
  views?: ViewInstance[];
  /** Host build identity (crate version + git sha), shown in Settings → About.
   * Optional on the wire; older hosts omit it and About shows "Not reported". */
  host_version?: string;
  /** The main agent's model snapshot (selection + everything selectable).
   * Optional on the wire; older hosts omit it and the Models control is hidden. */
  model?: ModelSnapshot;
  /** The sub-agent model catalog, grouped by provider. Optional on the wire;
   * older hosts omit it and the Sub-agent models control is hidden. */
  subagent_models?: SubagentModelCatalog;
}

export interface MsgMsg {
  type: "msg";
  message: ChatMessage;
  /** v2.0: side chat scope (see SendMessageMsg). Absent = main conversation. */
  sc?: string;
}

export type AgentActivityState = "thinking" | "idle";

export interface AgentActivityMsg {
  type: "agent_activity";
  state: AgentActivityState;
  text: string | null;
  /** v2.0: side chat scope (see SendMessageMsg). Absent = main conversation. */
  sc?: string;
}

export interface PingUpsertMsg {
  type: "ping_upsert";
  ping: Ping;
}

/** Typed event queue (ADR-0012): seed or update one Event in place, keyed by
 * `id`. Generalizes `ping_upsert` (an update is a re-send of the same id). */
export interface EventUpsertMsg {
  type: "event_upsert";
  event: EventItem;
}

/** v1.1: ack for an upload_blob, correlated by `client_id`. */
export interface BlobOkMsg {
  type: "blob_ok";
  client_id: string;
  blob: Blob;
}

/** D9: a short-lived, HMAC-signed URL for fetching a blob's bytes, answering
 * `get_blob_url` (correlated by `client_id`). `url` is host-relative
 * (`/blob/{id}?exp=&sig=`) — prefix the blob origin; `expires_at` is unix
 * seconds (≈5 min out). Superseded the removed `?token=` scheme. */
export interface BlobUrlMsg {
  type: "blob_url";
  client_id: string;
  blob_id: string;
  url: string;
  expires_at: number;
}

/** v1.2: tombstone for a cancelled queued message; clients drop the bubble. */
export interface MsgRemovedMsg {
  type: "msg_removed";
  id: number;
}

/** v1.4: full-process upsert broadcast on any state/summary change. */
export interface ProcessUpsertMsg {
  type: "process_upsert";
  process: ProcessInfo;
}

/** v1.5: one ordered event in the running turn's timeline. Tagged by `kind`.
 * Prose/reasoning carry markdown deltas that accumulate into the current
 * block/run; tool_start opens a row that tool_done (matched by `id`) resolves.
 * `tool_done` carries its own `name` too, so an orphan done (no matching start,
 * e.g. a reconnect mid-turn) still renders a labelled row. Host `summary`
 * strings are clean one-liners (no raw JSON). */
export type TurnEvent =
  | { kind: "prose"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tool_start"; id: string; name: string; summary: string | null }
  | { kind: "tool_done"; id: string; name: string; ok: boolean; summary: string | null };

/** v1.5: ephemeral timeline event streamed while the Agent's turn runs (like
 * agent_activity); never stored or replayed. `seq` strictly orders events
 * within a turn (gaps tolerated, redelivery idempotent). Replaces v1.4's
 * `agent_tool_call`. */
export interface TurnEventMsg {
  type: "turn_event";
  seq: number;
  event: TurnEvent;
  /** v2.0: side chat scope (see SendMessageMsg). Absent = main conversation. */
  sc?: string;
}

export interface ErrorMsg {
  type: "error";
  detail: string;
  /** Optional correlation id echoed for upload_blob / cancel_queued failures,
   * so the client can mark the right chip/bubble. Not in the canonical doc's
   * minimal error shape, but hosts may include it; absent for global errors. */
  client_id?: string;
}

/** v2.0: answers `open_side_chat`. Idempotent per Ping: a fresh side chat
 * carries `messages: []` (the seed lives in the side session's prompt layer,
 * not as transcript rows); resuming a live one carries its transcript so far. */
export interface SideChatOpenMsg {
  type: "side_chat_open";
  sc: string;
  ping_id: number;
  messages: ChatMessage[];
}

/** v2.0: the side agent's drafted reply. NOT appended to the side transcript —
 * it only ever lives in the confirmation sheet. */
export interface ConclusionDraftMsg {
  type: "conclusion_draft";
  sc: string;
  text: string;
}

/** v2.0: the side chat has ended — via confirm_conclusion, discard_side_chat,
 * or a host-side TTL reap. The client cannot tell which from this frame alone;
 * it distinguishes by whether IT asked for the close (see the reducer). */
export interface SideChatClosedMsg {
  type: "side_chat_closed";
  sc: string;
}

/** Generative-UI tier: seed or update a view in place, keyed by `instance_id`.
 * An update in place is a re-send of the same id with a new resolved `spec`. */
export interface ViewUpsertMsg {
  type: "view_upsert";
  instance_id: string;
  placement: ViewPlacement;
  spec: ViewSpec;
}

/** Generative-UI tier: drop the view with this `instance_id`. */
export interface ViewRemovedMsg {
  type: "view_removed";
  instance_id: string;
}

/** The main agent's model selection changed (in response to a `set_model`, or
 * a host-side change). Carries only the new `current`; the `available` list is
 * unchanged and stays as last seen in the `ModelSnapshot`. */
export interface ModelChangedMsg {
  type: "model_changed";
  current: ModelSelection;
}

/** The sub-agent model catalog changed (in response to a `set_subagent_model`,
 * or a host-side change). Carries the full replacement catalog. */
export interface SubagentModelsChangedMsg {
  type: "subagent_models_changed";
  catalog: SubagentModelCatalog;
}

export type ServerMessage =
  | HelloOkMsg
  | MsgMsg
  | AgentActivityMsg
  | PingUpsertMsg
  | EventUpsertMsg
  | BlobOkMsg
  | BlobUrlMsg
  | MsgRemovedMsg
  | ProcessUpsertMsg
  | TurnEventMsg
  | ErrorMsg
  | SideChatOpenMsg
  | ConclusionDraftMsg
  | SideChatClosedMsg
  | ViewUpsertMsg
  | ViewRemovedMsg
  | ModelChangedMsg
  | SubagentModelsChangedMsg;
