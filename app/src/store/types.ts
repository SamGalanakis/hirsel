import type {
  AgentActivityState,
  Blob,
  ChatMessage,
  HelloOkMsg,
  MsgMsg,
  Ping,
  PingUpsertMsg,
  ProcessInfo,
  ProcessUpsertMsg,
  SendMode,
  SideChatRef,
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
  /** v2.1 (ADR-0009): @-mentioned ping ids, so a reconnect resend carries them
   * too. Omitted when empty (keeps the common-case shape). */
  mentions?: number[];
}

export type ConnectionStatus = "connecting" | "connected" | "reconnecting";

export interface AgentActivity {
  state: AgentActivityState;
  text: string | null;
}

/** v2.0 (ADR-0008): the hydrated, scoped state of one live side chat — created
 * on `side_chat_open` (fresh Discuss or a deliberate Resume) and torn down on
 * `side_chat_closed`. Deliberately mirrors the shape of the top-level chat
 * slice (messages/agentActivity/turnEvents) 1:1 so the same Timeline/Bubble
 * components render both, per the "side chat is just a fancy reply composer"
 * design (ADR-0008, critique "what's working" #2) — but it's a wholly separate
 * bucket so sc-scoped routing can never leak into (or read from) main state. */
export interface SideChatState {
  sc: string;
  pingId: number;
  messages: DisplayMessage[];
  agentActivity: AgentActivity;
  turnEvents: TimelineEvent[];
  /** True from the moment "Conclude" is tapped until the draft arrives (drives
   * the sc-scoped "Drafting your reply…" shimmer). */
  drafting: boolean;
  /** The side agent's drafted reply once `conclusion_draft` arrives; null
   * before, and cleared again by "Keep editing" (back to the side chat, no
   * discard) or once `confirm_conclusion` is sent. */
  draft: string | null;
  /** True once `confirm_conclusion` is sent, awaiting `side_chat_closed` (and
   * the resulting owner reply landing in main chat). Distinguishes an
   * expected close (drop the record) from a host-initiated one (below). */
  confirming: boolean;
  /** True once `discard_side_chat` is sent, awaiting `side_chat_closed`. Same
   * purpose as `confirming` for the other user-initiated close path. */
  discarding: boolean;
  /** Non-blocking banner: the Agent resolved this Ping while its side chat was
   * still open. Conclude/Discard both remain available (critique edge case). */
  pingResolved: boolean;
  /** Terminal: `side_chat_closed` arrived without this client having asked for
   * it (TTL reap, or gone after a reconnect) while the record was still kept
   * (i.e. the sheet was open on it). The sheet renders "This side chat ended";
   * a discarded/concluded record is instead deleted outright, never marked
   * `ended`. */
  ended: boolean;
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
  /** Client-side arrival timestamp (`Date.now()` at reduce time), stamped when
   * the event is first admitted. Lets the timeline show a per-tool duration
   * (tool_done.at − tool_start.at) and a running-turn elapsed reading without
   * any wire timing field. Optional so hand-constructed events (tests) and any
   * pre-existing replayed data render fine with no duration. */
  at?: number;
}

export interface AppState {
  messages: DisplayMessage[];
  pings: Ping[];
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
  /** v1.3: Ping ids the Owner has manually "Marked unread". There is no wire
   * unread op, so this is a purely client-side override layered on top of the
   * wire `read` flag: a Ping is effectively unread if `!read` OR its id is
   * here. Auto-read/"Mark read" removes the id (and sends read_ping). Bounded. */
  unreadOverrides: number[];
  /** v2.0: live side chats, keyed by `sc` — mirrors `hello_ok.side_chats`
   * (seeded there, kept in sync by side_chat_open/side_chat_closed) so a Ping
   * card can derive "in progress · resume" for its Ping without ever hydrating
   * the full transcript. Deliberately separate from `sideChats` below: a
   * background Ping can be "in progress" all session without its scoped state
   * ever being created (that only happens when the sheet is actually
   * opened/resumed). */
  sideChatRefs: SideChatRef[];
  /** v2.0: hydrated per-side-chat state, created by `side_chat_open` (Discuss
   * or Resume) and removed by `side_chat_closed`. See SideChatState. */
  sideChats: Record<string, SideChatState>;
  /** v2.0: outgoing side-chat sends not yet echoed, flat (not per-sc) like
   * `pendingSends` — flushed the same way on reconnect so "sends queued"
   * during a socket drop applies to side chats too (critique, reconnect
   * section). Side sends are text-only (no attachments/mode), so this is a
   * deliberately smaller shape than the main-chat PendingSend. */
  pendingSideSends: { sc: string; clientId: string; body: string; ref: number | null }[];
  /** v2.0 provenance (client-derived; the wire has no marker): while a
   * `confirm_conclusion` is in flight, the anchor message id it targets maps
   * to the side chat that produced it, so the plain `msg` that lands in main
   * chat can be recognized as a conclusion when it arrives. Cleared on match. */
  awaitingConclusions: Record<number, string>;
  /** v2.0 provenance: main-chat message ids recognized as side-chat
   * conclusions (renders the "worked out in a side chat" footer chip).
   * Client-only session memory, like `turnDetails` — not persisted, so
   * replayed history after a reload shows no chip (documented, accepted). */
  conclusionChips: number[];
  /** v2.0: one-shot "a conclusion just landed" signal consumed by ChatView to
   * close the sheet (if it's the active one) and land-and-highlight the new
   * owner bubble — the same pattern as `scrollToMessageId` et al. in UiState. */
  lastConclusion: { sc: string; messageId: number } | null;
}

export type Action =
  | { type: "hello_ok"; payload: HelloOkMsg }
  | { type: "msg"; payload: MsgMsg }
  | { type: "msg_removed"; id: number }
  | { type: "agent_activity"; payload: { state: AgentActivityState; text: string | null } }
  | { type: "process_upsert"; payload: ProcessUpsertMsg }
  | { type: "turn_event"; payload: TurnEventMsg }
  | { type: "ping_upsert"; payload: PingUpsertMsg }
  | { type: "read_local"; pingId: number }
  | { type: "mark_unread_local"; pingId: number }
  | {
      type: "send_local";
      localId: number;
      clientId: string;
      body: string;
      ref: number | null;
      ts: string;
      attachments?: Blob[];
      mode?: SendMode;
      mentions?: number[];
    }
  | { type: "send_failed"; clientId: string }
  | { type: "send_retry"; clientId: string }
  | { type: "upload_start"; clientId: string; name: string; size: number; mime: string }
  | { type: "blob_ok"; clientId: string; blob: Blob }
  | { type: "upload_error"; clientId: string }
  | { type: "upload_retry"; clientId: string }
  | { type: "upload_remove"; clientId: string }
  | { type: "uploads_clear" }
  | { type: "connection_status"; status: ConnectionStatus }
  // ---- v2.0 side chats (ADR-0008) ----
  | { type: "side_chat_open"; sc: string; pingId: number; messages: ChatMessage[] }
  | { type: "side_chat_msg"; sc: string; message: ChatMessage }
  | {
      type: "side_chat_send_local";
      sc: string;
      localId: number;
      clientId: string;
      body: string;
      ref: number | null;
      ts: string;
    }
  | { type: "side_chat_agent_activity"; sc: string; state: AgentActivityState; text: string | null }
  | { type: "side_chat_turn_event"; sc: string; seq: number; event: TurnEvent }
  | { type: "side_chat_conclude_requested"; sc: string }
  | { type: "side_chat_conclusion_draft"; sc: string; text: string }
  | { type: "side_chat_keep_editing"; sc: string }
  | { type: "side_chat_confirm_sent"; sc: string; anchor: number }
  | { type: "side_chat_discard_sent"; sc: string }
  | { type: "side_chat_closed"; sc: string }
  | { type: "clear_last_conclusion" };

export function initialState(): AppState {
  return {
    messages: [],
    pings: [],
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
    sideChatRefs: [],
    sideChats: {},
    pendingSideSends: [],
    awaitingConclusions: {},
    conclusionChips: [],
    lastConclusion: null,
  };
}
