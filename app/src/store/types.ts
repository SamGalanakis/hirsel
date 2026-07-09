import type {
  AgentActivityState,
  Blob,
  ChatMessage,
  HelloOkMsg,
  InboxItem,
  InboxUpsertMsg,
  MsgMsg,
  ProcessInfo,
  ProcessUpsertMsg,
  SendMode,
  TurnEvent,
  TurnEventMsg,
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

/** A single ordered timeline event from the running turn (v1.5), carrying its
 * wire `seq` alongside the tagged event body. Accumulated in `seq` order while
 * the Agent is thinking; folded into a rendered timeline by `buildTimeline`. */
export interface TimelineEvent {
  seq: number;
  event: TurnEvent;
}

export interface AppState {
  messages: DisplayMessage[];
  inbox: InboxItem[];
  agentActivity: AgentActivity;
  connection: ConnectionStatus;
  lastSeenMsgId: number | null;
  pendingSends: PendingSend[];
  uploads: Upload[];
  /** v1.4: host-tracked background processes (sub-agents, monitors). Seeded by
   * hello_ok.processes and kept current by process_upsert. */
  processes: ProcessInfo[];
  /** v1.5: ordered events for the turn in progress, kept sorted by `seq`.
   * Ephemeral: cleared on turn commit (agent msg / agent_activity idle) and
   * frozen into `turnDetails` on an agent commit. */
  turnEvents: TimelineEvent[];
  /** v1.5: finished-turn timelines kept in session memory, keyed by the id of
   * the agent message that committed the turn (the "turn details" affordance).
   * Not persisted — gone after reload. Bounded to the most recent turns. */
  turnDetails: Record<number, TimelineEvent[]>;
  /** Host ids tombstoned by msg_removed. A cancelled queued message can have
   * its removal race its own echo; keeping the id here means a late echo is
   * dropped instead of re-materializing the bubble. Bounded. */
  removedIds: number[];
  /** v1.3: Inbox item ids the Owner has manually "Marked unread". There is no
   * wire unread op, so this is a purely client-side override layered on top of
   * the wire `read` flag: an item is effectively unread if `!read` OR its id is
   * here. Auto-read/"Mark read" removes the id (and sends read_item). Bounded. */
  unreadOverrides: number[];
}

export type Action =
  | { type: "hello_ok"; payload: HelloOkMsg }
  | { type: "msg"; payload: MsgMsg }
  | { type: "msg_removed"; id: number }
  | { type: "agent_activity"; payload: { state: AgentActivityState; text: string | null } }
  | { type: "process_upsert"; payload: ProcessUpsertMsg }
  | { type: "turn_event"; payload: TurnEventMsg }
  | { type: "inbox_upsert"; payload: InboxUpsertMsg }
  | { type: "read_local"; itemId: number }
  | { type: "mark_unread_local"; itemId: number }
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
    processes: [],
    turnEvents: [],
    turnDetails: {},
    removedIds: [],
    unreadOverrides: [],
  };
}
