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

export type ClientMessage =
  | HelloMsg
  | SendMessageMsg
  | ResolvePingMsg
  | ReadPingMsg
  | UploadBlobMsg
  | CancelTurnMsg
  | CancelQueuedMsg
  | OpenSideChatMsg
  | ConcludeSideChatMsg
  | ConfirmConclusionMsg
  | DiscardSideChatMsg;

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
  /** v1.4: all non-terminal processes + the last 10 terminal ones. Optional on
   * the wire; absent is treated as []. */
  processes?: ProcessInfo[];
  /** v2.0: live side chats surviving reconnect. Optional on the wire; absent
   * is treated as []. */
  side_chats?: SideChatRef[];
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

/** v1.1: ack for an upload_blob, correlated by `client_id`. */
export interface BlobOkMsg {
  type: "blob_ok";
  client_id: string;
  blob: Blob;
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

export type ServerMessage =
  | HelloOkMsg
  | MsgMsg
  | AgentActivityMsg
  | PingUpsertMsg
  | BlobOkMsg
  | MsgRemovedMsg
  | ProcessUpsertMsg
  | TurnEventMsg
  | ErrorMsg
  | SideChatOpenMsg
  | ConclusionDraftMsg
  | SideChatClosedMsg;
