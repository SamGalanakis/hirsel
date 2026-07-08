import type {
  AgentActivityState,
  Blob,
  ChatMessage,
  HelloOkMsg,
  InboxItem,
  InboxUpsertMsg,
  MsgMsg,
  SendMode,
} from "../protocol";

/** A chat message as rendered locally. `pending`/`clientId`/`mode`/`failed`
 * only exist on (or are carried through from) optimistic entries created by
 * this client. `clientId` and `mode` are preserved across reconciliation so a
 * next_turn bubble can still be cancelled (cancel_queued) after the host echo. */
export type DisplayMessage = ChatMessage & {
  pending?: boolean;
  failed?: boolean;
  clientId?: string;
  mode?: SendMode;
};

/** An outgoing `send_message` this client has emitted but not yet seen
 * reconciled via a server `msg` echo. Kept so reconnect can resend with the
 * same `client_id` (host dedupes) and so we know what to reconcile against. */
export interface PendingSend {
  clientId: string;
  body: string;
  ref: number | null;
  // Omitted (not just defaulted) when empty/"send" so the common case keeps the
  // original {clientId, body, ref} shape the store snapshots and tests encode.
  attachments?: string[];
  mode?: SendMode;
}

export type ConnectionStatus = "connecting" | "connected" | "reconnecting";

export interface AgentActivity {
  state: AgentActivityState;
  text: string | null;
}

/** Per-file upload state driving the composer attachment chips (v1.1). Purely
 * the state machine + resolved blob; the raw File/preview object-URL live in
 * the Composer, keyed by the same `clientId`. */
export type UploadState = "uploading" | "done" | "error";

export interface Upload {
  clientId: string;
  name: string;
  size: number;
  mime: string;
  state: UploadState;
  blobId?: string; // set once blob_ok correlates
}

export interface AppState {
  messages: DisplayMessage[];
  inbox: InboxItem[];
  agentActivity: AgentActivity;
  connection: ConnectionStatus;
  lastSeenMsgId: number | null;
  pendingSends: PendingSend[];
  uploads: Upload[];
  /** Host ids tombstoned by msg_removed. A cancelled queued message can have
   * its removal race its own echo; keeping the id here means a late echo is
   * dropped instead of re-materializing the bubble. Bounded. */
  removedIds: number[];
}

export type Action =
  | { type: "hello_ok"; payload: HelloOkMsg }
  | { type: "msg"; payload: MsgMsg }
  | { type: "msg_removed"; id: number }
  | { type: "agent_activity"; payload: { state: AgentActivityState; text: string | null } }
  | { type: "inbox_upsert"; payload: InboxUpsertMsg }
  | {
      type: "send_local";
      localId: number;
      clientId: string;
      body: string;
      ref: number | null;
      ts: string;
      attachments?: Blob[];
      mode?: SendMode;
    }
  | { type: "send_failed"; clientId: string }
  | { type: "send_retry"; clientId: string }
  | { type: "upload_start"; clientId: string; name: string; size: number; mime: string }
  | { type: "blob_ok"; clientId: string; blob: Blob }
  | { type: "upload_error"; clientId: string }
  | { type: "upload_retry"; clientId: string }
  | { type: "upload_remove"; clientId: string }
  | { type: "uploads_clear" }
  | { type: "connection_status"; status: ConnectionStatus };

export function initialState(): AppState {
  return {
    messages: [],
    inbox: [],
    agentActivity: { state: "idle", text: null },
    connection: "connecting",
    lastSeenMsgId: null,
    pendingSends: [],
    uploads: [],
    removedIds: [],
  };
}
