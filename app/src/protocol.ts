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

export interface ChatMessage {
  id: number; // u64, monotonic, host-assigned
  author: Author;
  body: string; // markdown
  ref: number | null; // id of the chat message this replies to
  ts: string; // RFC3339
  attachments?: Blob[]; // v1.1, default []
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
  | UploadBlobMsg
  | CancelTurnMsg
  | CancelQueuedMsg;

// ---- Server -> client ----

export interface HelloOkMsg {
  type: "hello_ok";
  latest_msg_id: number;
  messages: ChatMessage[];
  inbox: InboxItem[];
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
  | ErrorMsg;
