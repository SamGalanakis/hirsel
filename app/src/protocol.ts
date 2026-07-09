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

export type InboxStatus = "open" | "archived";

export interface InboxItem {
  id: number;
  content: string; // markdown
  anchor: number; // ChatMessage.id where the inbox tool was called
  requires_response: boolean;
  quick_replies: QuickReply[]; // may be empty
  status: InboxStatus;
  ts: string;
  /** v1.3: Owner-side "seen" state, set automatically once an item has been
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
}

export interface ArchiveItemMsg {
  type: "archive_item";
  item_id: number;
}

/** v1.3: mark an Inbox item read (email-like "seen"). Idempotent; the host sets
 * read=true and broadcasts an inbox_upsert. There is no "unread" op — that is a
 * client-only override (see store). */
export interface ReadItemMsg {
  type: "read_item";
  item_id: number;
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
}

/** v1.2: cancel a not-yet-claimed queued (next_turn) message. Host maps
 * `client_id` to its pending-input id; already-claimed → error. */
export interface CancelQueuedMsg {
  type: "cancel_queued";
  client_id: string;
}

export type ClientMessage =
  | HelloMsg
  | SendMessageMsg
  | ArchiveItemMsg
  | ReadItemMsg
  | UploadBlobMsg
  | CancelTurnMsg
  | CancelQueuedMsg;

// ---- Server -> client ----

export interface HelloOkMsg {
  type: "hello_ok";
  latest_msg_id: number;
  messages: ChatMessage[];
  inbox: InboxItem[];
  /** v1.4: all non-terminal processes + the last 10 terminal ones. Optional on
   * the wire; absent is treated as []. */
  processes?: ProcessInfo[];
}

export interface MsgMsg {
  type: "msg";
  message: ChatMessage;
}

export type AgentActivityState = "thinking" | "idle";

export interface AgentActivityMsg {
  type: "agent_activity";
  state: AgentActivityState;
  text: string | null;
}

export interface InboxUpsertMsg {
  type: "inbox_upsert";
  item: InboxItem;
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
}

export interface ErrorMsg {
  type: "error";
  detail: string;
  /** Optional correlation id echoed for upload_blob / cancel_queued failures,
   * so the client can mark the right chip/bubble. Not in the canonical doc's
   * minimal error shape, but hosts may include it; absent for global errors. */
  client_id?: string;
}

export type ServerMessage =
  | HelloOkMsg
  | MsgMsg
  | AgentActivityMsg
  | InboxUpsertMsg
  | BlobOkMsg
  | MsgRemovedMsg
  | ProcessUpsertMsg
  | TurnEventMsg
  | ErrorMsg;
