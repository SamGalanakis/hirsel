// Hand-written mirror of ../PROTOCOL.md (canonical). If that file changes, this
// file and the Rust `hirsel-proto` crate both need to change to match.
//
// Transport: WebSocket, JSON text frames, one message per frame.

export type Author = "owner" | "agent";

export interface ChatMessage {
  id: number; // u64, monotonic, host-assigned
  author: Author;
  body: string; // markdown
  ref: number | null; // id of the chat message this replies to
  ts: string; // RFC3339
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

export interface SendMessageMsg {
  type: "send_message";
  client_id: string; // client-generated idempotency key (uuid)
  body: string;
  ref: number | null;
}

export interface ArchiveItemMsg {
  type: "archive_item";
  item_id: number;
}

export type ClientMessage = HelloMsg | SendMessageMsg | ArchiveItemMsg;

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

export interface ErrorMsg {
  type: "error";
  detail: string;
}

export type ServerMessage =
  | HelloOkMsg
  | MsgMsg
  | AgentActivityMsg
  | InboxUpsertMsg
  | ErrorMsg;
